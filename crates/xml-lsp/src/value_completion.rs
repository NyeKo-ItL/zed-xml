//! Completion of enumerated values from the XSD model of the document:
//! attribute values (`<a status="|">`) and text contents (`<status>|</status>`)
//! whose simple type restricts its values with `xs:enumeration`.
//!
//! The document being typed is malformed, so a copy of the text before the
//! cursor is closed (the open value, or a probe element) before it is analyzed.

use serde_json::{Value, json};

use crate::{
    code_actions::{enumeration_values, prefix_for},
    hover::{self, Document, HoverContext},
    selection::LineIndex,
};

/// Enumerated values accepted at `offset`, as completion items.
pub(crate) fn completions(
    context: &mut HoverContext<'_>,
    uri: &str,
    source: &str,
    offset: usize,
) -> Vec<Value> {
    let Some(prefix) = source.get(..offset) else {
        return Vec::new();
    };
    let Some(site) = ValueSite::at(prefix) else {
        return Vec::new();
    };
    let patched = format!("{prefix}{}", site.closing());
    let document = Document::parse(&patched);
    let models = hover::instance_models(context, uri, &document);
    if models.set.models().is_empty() {
        return Vec::new();
    }
    let Some(probe) = document
        .tree
        .elements()
        .iter()
        .rposition(|element| element.start_tag.range.start == site.tag_start)
    else {
        return Vec::new();
    };
    let set = &models.set;
    let (value_type, typed_start) = match &site.kind {
        SiteKind::AttributeValue { name, start, .. } => {
            let (namespace, local) = hover::attribute_namespace(&document, probe, name);
            let resolved = set.resolve_element_path(&document.instance_path(probe));
            let Some(declaration) = set
                .resolve_attribute(resolved.as_ref(), namespace, local)
                .map(|attribute| attribute.declaration)
            else {
                return Vec::new();
            };
            let Some(value_type) = set.attribute_type(declaration) else {
                return Vec::new();
            };
            (value_type, *start)
        }
        SiteKind::Text { start } => {
            let Some(parent) = document.tree.elements()[probe].parent else {
                return Vec::new();
            };
            let Some(resolved) = set.resolve_element_path(&document.instance_path(parent)) else {
                return Vec::new();
            };
            let Some(element_type) = resolved.element_type else {
                return Vec::new();
            };
            if element_type
                .definition
                .is_some_and(|definition| definition.complex && !definition.simple_content)
            {
                return Vec::new();
            }
            (element_type, *start)
        }
    };
    let Some(values) = enumeration_values(set, value_type) else {
        return Vec::new();
    };
    let typed = source[typed_start..offset].trim_start();
    let typed_start = offset - typed.len();
    let lines = LineIndex::new(source);
    let range = json!({
        "start": lines.position(source, typed_start),
        "end": lines.position(source, offset),
    });
    values
        .into_iter()
        .filter(|value| value.starts_with(typed))
        .map(|value| {
            let documentation = set
                .enumeration(value_type, &value)
                .and_then(|enumeration| enumeration.documentation.clone());
            let mut item = json!({
                "label": value,
                "kind": 12,
                "insertText": value,
                "textEdit": {"range": range, "newText": value},
            });
            if let Some(documentation) = documentation {
                item["documentation"] = json!({"kind": "markdown", "value": documentation});
            }
            item
        })
        .collect()
}

/// Elements the XSD allows at `offset` while a start tag name is being typed
/// (`<Int|`): the children the content model of the parent accepts after
/// the preceding siblings, or the global elements for the root.
pub(crate) fn element_completions(
    context: &mut HoverContext<'_>,
    uri: &str,
    source: &str,
    offset: usize,
) -> Vec<Value> {
    let Some(prefix) = source.get(..offset) else {
        return Vec::new();
    };
    let Some(opening) = prefix.rfind('<') else {
        return Vec::new();
    };
    let typed = &prefix[opening + 1..];
    if typed.starts_with(['/', '!', '?'])
        || typed.contains(|c: char| c.is_whitespace() || matches!(c, '>' | '"' | '\'' | '='))
    {
        return Vec::new();
    }
    let patched = format!("{}<_/>", &prefix[..opening]);
    let document = Document::parse(&patched);
    let models = hover::instance_models(context, uri, &document);
    if models.set.models().is_empty() {
        return Vec::new();
    }
    let elements = document.tree.elements();
    let Some(probe) = elements
        .iter()
        .rposition(|element| element.start_tag.range.start == opening)
    else {
        return Vec::new();
    };
    let set = &models.set;
    let candidates: Vec<(Option<String>, String)> = match elements[probe].parent {
        None => set
            .global_elements()
            .filter(|candidate| !candidate.item.is_abstract)
            .map(|candidate| {
                (
                    candidate.item.namespace.clone(),
                    candidate.item.name.clone(),
                )
            })
            .collect(),
        Some(parent) => {
            let Some(parent_type) = set
                .resolve_element_path(&document.instance_path(parent))
                .and_then(|resolved| resolved.element_type)
            else {
                return Vec::new();
            };
            let preceding = elements
                .iter()
                .enumerate()
                .filter(|(index, element)| *index != probe && element.parent == Some(parent))
                .map(|(index, _)| {
                    let (namespace, local) = document.split(index, document.element_name(index));
                    (namespace.map(str::to_owned), local.to_owned())
                })
                .collect::<Vec<_>>();
            set.child_elements_after(parent_type, &preceding)
                .into_iter()
                .map(|candidate| {
                    (
                        candidate.item.namespace.clone(),
                        candidate.item.name.clone(),
                    )
                })
                .collect()
        }
    };
    let lines = LineIndex::new(source);
    let range = json!({
        "start": lines.position(source, opening + 1),
        "end": lines.position(source, offset),
    });
    let mut names = candidates
        .into_iter()
        .filter_map(|(namespace, local)| match namespace {
            None => Some(local),
            Some(namespace) if document.namespace(probe, None) == Some(namespace.as_str()) => {
                Some(local)
            }
            Some(namespace) => {
                prefix_for(&document, probe, &namespace).map(|prefix| format!("{prefix}:{local}"))
            }
        })
        .filter(|name| name.starts_with(typed))
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| {
            json!({
                "label": name,
                "kind": 7,
                "insertText": name,
                "textEdit": {"range": range, "newText": name},
            })
        })
        .collect()
}

/// Name of the open element while its end tag is typed (`</Cus|`), replacing
/// the typed name (the client would otherwise insert `</` a second time) and
/// adding the `>` when none follows.
pub(crate) fn end_tag_completions(source: &str, offset: usize) -> Vec<Value> {
    let Some(prefix) = source.get(..offset) else {
        return Vec::new();
    };
    let Some(opening) = prefix.rfind('<') else {
        return Vec::new();
    };
    let Some(typed) = prefix[opening..].strip_prefix("</") else {
        return Vec::new();
    };
    if typed.contains(|c: char| c.is_whitespace() || c == '>') {
        return Vec::new();
    }
    let Some(name) = xml_core::complete_xml(source, offset)
        .into_iter()
        .find_map(|completion| completion.insert_text.strip_prefix("</").map(str::to_owned))
    else {
        return Vec::new();
    };
    let new_text = if source[offset..].starts_with('>') {
        name.clone()
    } else {
        format!("{name}>")
    };
    let lines = LineIndex::new(source);
    vec![json!({
        "label": name,
        "kind": 7,
        "insertText": new_text,
        "textEdit": {
            "range": {
                "start": lines.position(source, opening + 2),
                "end": lines.position(source, offset),
            },
            "newText": new_text,
        },
    })]
}

struct ValueSite {
    /// Offset of the element of the probe: the start tag the cursor is in
    /// (attribute value) or the element appended at the cursor (text).
    tag_start: usize,
    kind: SiteKind,
}

enum SiteKind {
    /// Inside the unclosed quoted value of the attribute `name`, which starts
    /// at `start`.
    AttributeValue {
        name: String,
        quote: char,
        start: usize,
    },
    /// In the text starting at `start`.
    Text { start: usize },
}

impl ValueSite {
    fn at(prefix: &str) -> Option<Self> {
        let opening = prefix.rfind('<')?;
        let Some(closing) = prefix.rfind('>').filter(|closing| *closing > opening) else {
            return Self::attribute_value(prefix, opening);
        };
        Some(Self {
            tag_start: prefix.len(),
            kind: SiteKind::Text { start: closing + 1 },
        })
    }

    fn attribute_value(prefix: &str, opening: usize) -> Option<Self> {
        let tag = &prefix[opening + 1..];
        if tag.starts_with(['/', '!', '?']) {
            return None;
        }
        // Last attribute whose quoted value is still open.
        let mut quote = None;
        let mut value_start = 0;
        let mut name_end = 0;
        for (index, character) in tag.char_indices() {
            match quote {
                Some(open) if character == open => quote = None,
                Some(_) => {}
                None if character == '"' || character == '\'' => {
                    quote = Some(character);
                    value_start = index + 1;
                    name_end = index;
                }
                None => {}
            }
        }
        let quote = quote?;
        let name = tag[..name_end]
            .trim_end()
            .strip_suffix('=')?
            .split_whitespace()
            .last()?;
        Some(Self {
            tag_start: opening,
            kind: SiteKind::AttributeValue {
                name: name.to_owned(),
                quote,
                start: opening + 1 + value_start,
            },
        })
    }

    /// Text appended to the prefix so that its last tag is complete.
    fn closing(&self) -> String {
        match &self.kind {
            SiteKind::AttributeValue { quote, .. } => format!("{quote}/>"),
            SiteKind::Text { .. } => "<_/>".to_owned(),
        }
    }
}
