//! Attribute and `xsi:nil` rules of instance validation, resolved through
//! the component model (declarations found in the context of the element,
//! qualified and unqualified names kept apart) instead of name-keyed flat
//! lists.

use quick_xml::events::BytesStart;

use crate::{
    XSI_NAMESPACE, XsdDiagnostic, XsdDiagnosticKind,
    model::{ResolvedElement, XsdModelSet, XsdProcessContents, XsdUse},
};

/// Whether the type accepts any attribute (`xs:anyType`) or is not known.
fn unconstrained(resolved: &ResolvedElement<'_>) -> bool {
    match resolved.element_type {
        None => true,
        Some(reference) => match (reference.definition, reference.name) {
            (Some(_), _) => false,
            (None, Some(name)) => name.is_builtin() && name.local == "anyType",
            (None, None) => true,
        },
    }
}

/// Declared, prohibited, required and `fixed` attributes of an element.
/// `same_value(local name, value, fixed)` compares a value with its fixed
/// value; `lookup` resolves prefixes (`xmlns` and `xsi:` attributes are
/// always allowed).
pub(crate) fn validate_attributes(
    models: &XsdModelSet,
    resolved: Option<&ResolvedElement<'_>>,
    element_name: &str,
    element: &BytesStart<'_>,
    same_value: &dyn Fn(&str, &str, &str) -> bool,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<XsdDiagnostic> {
    let Some(resolved) = resolved else {
        return Vec::new();
    };
    if unconstrained(resolved) {
        return Vec::new();
    }
    let Some(element_type) = resolved.element_type else {
        return Vec::new();
    };
    // An element of a simple type has no attribute at all.
    let (uses, wildcards) = if element_type
        .definition
        .is_some_and(|definition| definition.complex)
    {
        (
            models.attribute_uses(element_type),
            models.attribute_wildcards(element_type),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let mut diagnostics = Vec::new();
    let mut present: Vec<(Option<String>, String)> = Vec::new();
    for attribute in element.attributes().flatten() {
        let Ok(key) = std::str::from_utf8(attribute.key.as_ref()) else {
            continue;
        };
        if key == "xmlns" || key.starts_with("xmlns:") {
            continue;
        }
        let (namespace, local) = match key.split_once(':') {
            Some((prefix, local)) => (lookup(prefix), local),
            None => (None, key),
        };
        if namespace.as_deref() == Some(XSI_NAMESPACE) {
            continue;
        }
        present.push((namespace.clone(), local.to_owned()));
        let usage = uses
            .iter()
            .find(|usage| usage.item.name == local && usage.item.namespace == namespace);
        match usage {
            Some(usage) if usage.item.usage == XsdUse::Prohibited => {
                diagnostics.push(XsdDiagnostic {
                    kind: XsdDiagnosticKind::UnexpectedAttribute,
                    message: format!("attribute @{key} is prohibited on <{element_name}>"),
                });
            }
            Some(usage) => {
                let declaration = models.attribute_declaration(*usage);
                let fixed = usage
                    .item
                    .fixed
                    .as_deref()
                    .or(declaration.item.fixed.as_deref());
                if let Some(fixed) = fixed
                    && attribute
                        .unescape_value()
                        .is_ok_and(|value| !same_value(local, value.as_ref(), fixed))
                {
                    diagnostics.push(XsdDiagnostic {
                        kind: XsdDiagnosticKind::FixedValue,
                        message: format!("attribute @{key} differs from the fixed value"),
                    });
                }
            }
            None => {
                let wildcard = wildcards
                    .iter()
                    .find(|wildcard| wildcard.namespaces.allows(namespace.as_deref()));
                match wildcard {
                    Some(wildcard)
                        if wildcard.process_contents == XsdProcessContents::Strict
                            // Only when the namespace's schema is loaded: its
                            // declarations may be in a schema not in the set.
                            && models
                                .models()
                                .iter()
                                .any(|model| model.target_namespace == namespace)
                            && models
                                .global_attribute(namespace.as_deref(), local)
                                .is_none_or(|found| found.item.namespace != namespace) =>
                    {
                        diagnostics.push(XsdDiagnostic {
                            kind: XsdDiagnosticKind::UnexpectedAttribute,
                            message: format!(
                                "attribute @{key} on <{element_name}> has no declaration (strict wildcard)"
                            ),
                        });
                    }
                    Some(_) => {}
                    None => diagnostics.push(XsdDiagnostic {
                        kind: XsdDiagnosticKind::UnexpectedAttribute,
                        message: format!("attribute @{key} not allowed on <{element_name}>"),
                    }),
                }
            }
        }
    }
    for usage in &uses {
        if usage.item.usage == XsdUse::Required
            && !present.iter().any(|(namespace, local)| {
                *local == usage.item.name && *namespace == usage.item.namespace
            })
        {
            diagnostics.push(XsdDiagnostic {
                kind: XsdDiagnosticKind::MissingAttribute,
                message: format!(
                    "attribute @{} required on <{element_name}>",
                    usage.item.name
                ),
            });
        }
    }
    diagnostics
}

/// `xsi:nil="true"` on an element whose declaration is not nillable.
pub(crate) fn validate_nil(
    resolved: Option<&ResolvedElement<'_>>,
    element_name: &str,
    element: &BytesStart<'_>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<XsdDiagnostic> {
    let is_nil = element.attributes().flatten().any(|attribute| {
        let Ok(key) = std::str::from_utf8(attribute.key.as_ref()) else {
            return false;
        };
        key.split_once(':').is_some_and(|(prefix, local)| {
            local == "nil" && lookup(prefix).as_deref() == Some(XSI_NAMESPACE)
        }) && attribute
            .unescape_value()
            .is_ok_and(|value| matches!(value.trim(), "true" | "1"))
    });
    match resolved {
        Some(resolved) if is_nil && !resolved.declaration.item.nillable => {
            vec![XsdDiagnostic {
                kind: XsdDiagnosticKind::NotNillable,
                message: format!("element <{element_name}> is not nillable but has xsi:nil"),
            }]
        }
        _ => Vec::new(),
    }
}
