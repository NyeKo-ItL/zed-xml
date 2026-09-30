//! Checks of a schema document itself: the schema representation
//! constraints of XML Schema 1.0 Part 1 (the structure the schema for
//! schemas requires and the constraints on attributes it cannot express),
//! reported with the range of the offending construct.
//!
//! The check reads the document with the tolerant tag scanner of
//! `xml-core`, so it works on a schema being edited; it does nothing when
//! the root is not an `xs:schema`.

use std::{collections::HashMap, ops::Range};

use xml_core::{
    names::{is_ncname, is_qname},
    tags::{
        XML_NAMESPACE, XmlAttribute, XmlTagTree, qualified_name_parts, resolve_namespace,
        scan_attributes,
    },
    text::decode_references,
};

use crate::model::XSD_NAMESPACE;

/// A violation of the schema representation constraints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaProblem {
    pub range: Range<usize>,
    /// Stable identifier (`data.kind` of the diagnostic).
    pub rule: &'static str,
    pub message: String,
}

pub(crate) const INVALID_ELEMENT: &str = "invalidSchemaElement";
pub(crate) const INVALID_ATTRIBUTE: &str = "invalidSchemaAttribute";
pub(crate) const INVALID_VALUE: &str = "invalidSchemaValue";
pub(crate) const MISSING_ATTRIBUTE: &str = "missingSchemaAttribute";
pub(crate) const MISSING_CONTENT: &str = "missingSchemaContent";
pub(crate) const INVALID_COMBINATION: &str = "invalidSchemaAttributes";

/// A schema document read for checking.
pub(crate) struct SchemaDocument<'a> {
    pub(crate) source: &'a str,
    pub(crate) tree: XmlTagTree,
    pub(crate) attributes: Vec<Vec<XmlAttribute>>,
    /// Local name of each element in the XML Schema namespace (`None` for
    /// other elements).
    pub(crate) names: Vec<Option<&'a str>>,
    pub(crate) children: Vec<Vec<usize>>,
    pub(crate) root: usize,
}

impl<'a> SchemaDocument<'a> {
    /// Reads `source`; `None` unless its root is an `xs:schema`.
    pub(crate) fn read(source: &'a str) -> Option<Self> {
        let tree = XmlTagTree::parse(source);
        let attributes = tree
            .elements()
            .iter()
            .map(|element| scan_attributes(source, &element.start_tag))
            .collect::<Vec<_>>();
        let mut names = Vec::with_capacity(attributes.len());
        for (index, element) in tree.elements().iter().enumerate() {
            let (prefix, local) = qualified_name_parts(source, element.start_tag.name.clone());
            let prefix = prefix.map(|range| &source[range]);
            let namespace = resolve_namespace(source, &tree, &attributes, index, prefix);
            names.push((namespace == Some(Some(XSD_NAMESPACE))).then_some(&source[local]));
        }
        let root = tree
            .elements()
            .iter()
            .position(|element| element.parent.is_none())?;
        if names[root] != Some("schema") {
            return None;
        }
        let mut children = vec![Vec::new(); names.len()];
        for (index, element) in tree.elements().iter().enumerate() {
            if let Some(parent) = element.parent {
                children[parent].push(index);
            }
        }
        Some(Self {
            source,
            tree,
            attributes,
            names,
            children,
            root,
        })
    }

    pub(crate) fn name_range(&self, element: usize) -> Range<usize> {
        self.tree.elements()[element].start_tag.name.clone()
    }

    pub(crate) fn local(&self, element: usize) -> &'a str {
        self.names[element].unwrap_or("")
    }

    pub(crate) fn parent(&self, element: usize) -> Option<usize> {
        self.tree.elements()[element].parent
    }

    /// Unprefixed attribute `name` of the element.
    pub(crate) fn attribute(&self, element: usize, name: &str) -> Option<&XmlAttribute> {
        self.attributes[element]
            .iter()
            .find(|attribute| attribute.name(self.source) == name)
    }

    /// Decoded value of the unprefixed attribute `name`.
    pub(crate) fn value(&self, element: usize, name: &str) -> Option<String> {
        self.attribute(element, name)
            .and_then(|attribute| attribute.value(self.source))
            .map(decode_references)
    }

    pub(crate) fn has_attribute(&self, element: usize, name: &str) -> bool {
        self.attribute(element, name).is_some()
    }

    /// Namespace of a prefix in scope of the element (`None`: undeclared).
    pub(crate) fn namespace(&self, element: usize, prefix: Option<&str>) -> Option<Option<&str>> {
        resolve_namespace(self.source, &self.tree, &self.attributes, element, prefix)
    }
}

// ---------------------------------------------------------------------------
// Attribute specifications
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Kind {
    Id,
    NcName,
    QName,
    QNameList,
    NonNegative,
    Positive,
    MaxOccurs,
    Boolean,
    AnyUri,
    Text,
    Enum(&'static [&'static str]),
    /// `#all` or a list of the tokens.
    Set(&'static [&'static str]),
    Namespace,
}

type Spec = &'static [(&'static str, Kind, bool)];

const FORM: Kind = Kind::Enum(&["qualified", "unqualified"]);
const DERIVATION: &[&str] = &["extension", "restriction", "substitution"];
const ELEMENT_FINAL: &[&str] = &["extension", "restriction"];
const TYPE_FINAL: &[&str] = &["extension", "restriction", "list", "union"];
const TYPE_BLOCK: &[&str] = &["extension", "restriction"];
const SIMPLE_FINAL: &[&str] = &["list", "union", "restriction"];
const PROCESS: Kind = Kind::Enum(&["skip", "lax", "strict"]);

const SCHEMA: Spec = &[
    ("id", Kind::Id, false),
    ("targetNamespace", Kind::AnyUri, false),
    ("version", Kind::Text, false),
    ("attributeFormDefault", FORM, false),
    ("elementFormDefault", FORM, false),
    ("blockDefault", Kind::Set(DERIVATION), false),
    ("finalDefault", Kind::Set(TYPE_FINAL), false),
];
const INCLUDE: Spec = &[
    ("id", Kind::Id, false),
    ("schemaLocation", Kind::AnyUri, true),
];
const IMPORT: Spec = &[
    ("id", Kind::Id, false),
    ("namespace", Kind::AnyUri, false),
    ("schemaLocation", Kind::AnyUri, false),
];
const ID_ONLY: Spec = &[("id", Kind::Id, false)];
const APPINFO: Spec = &[("source", Kind::AnyUri, false)];
const DOCUMENTATION: Spec = &[("source", Kind::AnyUri, false)];
const GLOBAL_ELEMENT: Spec = &[
    ("id", Kind::Id, false),
    ("name", Kind::NcName, true),
    ("type", Kind::QName, false),
    ("default", Kind::Text, false),
    ("fixed", Kind::Text, false),
    ("nillable", Kind::Boolean, false),
    ("abstract", Kind::Boolean, false),
    ("substitutionGroup", Kind::QName, false),
    ("block", Kind::Set(DERIVATION), false),
    ("final", Kind::Set(ELEMENT_FINAL), false),
];
const LOCAL_ELEMENT: Spec = &[
    ("id", Kind::Id, false),
    ("name", Kind::NcName, false),
    ("ref", Kind::QName, false),
    ("type", Kind::QName, false),
    ("default", Kind::Text, false),
    ("fixed", Kind::Text, false),
    ("nillable", Kind::Boolean, false),
    ("block", Kind::Set(DERIVATION), false),
    ("form", FORM, false),
    ("minOccurs", Kind::NonNegative, false),
    ("maxOccurs", Kind::MaxOccurs, false),
];
const ELEMENT_REFERENCE: &[&str] = &["id", "ref", "minOccurs", "maxOccurs"];
const GLOBAL_ATTRIBUTE: Spec = &[
    ("id", Kind::Id, false),
    ("name", Kind::NcName, true),
    ("type", Kind::QName, false),
    ("default", Kind::Text, false),
    ("fixed", Kind::Text, false),
];
const LOCAL_ATTRIBUTE: Spec = &[
    ("id", Kind::Id, false),
    ("name", Kind::NcName, false),
    ("ref", Kind::QName, false),
    ("type", Kind::QName, false),
    (
        "use",
        Kind::Enum(&["optional", "prohibited", "required"]),
        false,
    ),
    ("default", Kind::Text, false),
    ("fixed", Kind::Text, false),
    ("form", FORM, false),
];
const ATTRIBUTE_REFERENCE: &[&str] = &["id", "ref", "use", "default", "fixed"];
const GLOBAL_COMPLEX_TYPE: Spec = &[
    ("id", Kind::Id, false),
    ("name", Kind::NcName, true),
    ("mixed", Kind::Boolean, false),
    ("abstract", Kind::Boolean, false),
    ("final", Kind::Set(TYPE_BLOCK), false),
    ("block", Kind::Set(TYPE_BLOCK), false),
];
const LOCAL_COMPLEX_TYPE: Spec = &[("id", Kind::Id, false), ("mixed", Kind::Boolean, false)];
const GLOBAL_SIMPLE_TYPE: Spec = &[
    ("id", Kind::Id, false),
    ("name", Kind::NcName, true),
    ("final", Kind::Set(SIMPLE_FINAL), false),
];
const GROUP_DEFINITION: Spec = &[("id", Kind::Id, false), ("name", Kind::NcName, true)];
const GROUP_REFERENCE: Spec = &[
    ("id", Kind::Id, false),
    ("ref", Kind::QName, true),
    ("minOccurs", Kind::NonNegative, false),
    ("maxOccurs", Kind::MaxOccurs, false),
];
const ATTRIBUTE_GROUP_DEFINITION: Spec = &[("id", Kind::Id, false), ("name", Kind::NcName, true)];
const ATTRIBUTE_GROUP_REFERENCE: Spec = &[("id", Kind::Id, false), ("ref", Kind::QName, true)];
const COMPOSITOR: Spec = &[
    ("id", Kind::Id, false),
    ("minOccurs", Kind::NonNegative, false),
    ("maxOccurs", Kind::MaxOccurs, false),
];
const ANY: Spec = &[
    ("id", Kind::Id, false),
    ("namespace", Kind::Namespace, false),
    ("notNamespace", Kind::Text, false),
    ("notQName", Kind::Text, false),
    ("processContents", PROCESS, false),
    ("minOccurs", Kind::NonNegative, false),
    ("maxOccurs", Kind::MaxOccurs, false),
];
const ANY_ATTRIBUTE: Spec = &[
    ("id", Kind::Id, false),
    ("namespace", Kind::Namespace, false),
    ("notNamespace", Kind::Text, false),
    ("notQName", Kind::Text, false),
    ("processContents", PROCESS, false),
];
const NOTATION: Spec = &[
    ("id", Kind::Id, false),
    ("name", Kind::NcName, true),
    ("public", Kind::Text, false),
    ("system", Kind::AnyUri, false),
];
const COMPLEX_CONTENT: Spec = &[("id", Kind::Id, false), ("mixed", Kind::Boolean, false)];
const DERIVATION_WITH_BASE: Spec = &[("id", Kind::Id, false), ("base", Kind::QName, true)];
const SIMPLE_RESTRICTION: Spec = &[("id", Kind::Id, false), ("base", Kind::QName, false)];
const LIST: Spec = &[("id", Kind::Id, false), ("itemType", Kind::QName, false)];
const UNION: Spec = &[
    ("id", Kind::Id, false),
    ("memberTypes", Kind::QNameList, false),
];
const FACET: Spec = &[
    ("id", Kind::Id, false),
    ("value", Kind::Text, true),
    ("fixed", Kind::Boolean, false),
];
const PATTERN: Spec = &[("id", Kind::Id, false), ("value", Kind::Text, true)];
const WHITE_SPACE: Spec = &[
    ("id", Kind::Id, false),
    (
        "value",
        Kind::Enum(&["preserve", "replace", "collapse"]),
        true,
    ),
    ("fixed", Kind::Boolean, false),
];
const LENGTH_FACET: Spec = &[
    ("id", Kind::Id, false),
    ("value", Kind::NonNegative, true),
    ("fixed", Kind::Boolean, false),
];
const TOTAL_DIGITS: Spec = &[
    ("id", Kind::Id, false),
    ("value", Kind::Positive, true),
    ("fixed", Kind::Boolean, false),
];
const IDENTITY: Spec = &[("id", Kind::Id, false), ("name", Kind::NcName, true)];
const KEYREF: Spec = &[
    ("id", Kind::Id, false),
    ("name", Kind::NcName, true),
    ("refer", Kind::QName, true),
];
const XPATH: Spec = &[("id", Kind::Id, false), ("xpath", Kind::Text, true)];

/// The attributes an XSD element allows; `None` for an unknown element.
fn attribute_spec(document: &SchemaDocument<'_>, element: usize) -> Option<Spec> {
    let local = document.local(element);
    let parent = document
        .parent(element)
        .map(|parent| document.local(parent));
    let top_level = matches!(parent, Some("schema" | "redefine"));
    Some(match local {
        "schema" => SCHEMA,
        "include" | "redefine" => INCLUDE,
        "import" => IMPORT,
        "annotation" => ID_ONLY,
        "appinfo" => APPINFO,
        "documentation" => DOCUMENTATION,
        "element" if top_level => GLOBAL_ELEMENT,
        "element" => LOCAL_ELEMENT,
        "attribute" if top_level => GLOBAL_ATTRIBUTE,
        "attribute" => LOCAL_ATTRIBUTE,
        "complexType" if top_level => GLOBAL_COMPLEX_TYPE,
        "complexType" => LOCAL_COMPLEX_TYPE,
        "simpleType" if top_level => GLOBAL_SIMPLE_TYPE,
        "simpleType" => ID_ONLY,
        "group" if top_level => GROUP_DEFINITION,
        "group" => GROUP_REFERENCE,
        "attributeGroup" if top_level => ATTRIBUTE_GROUP_DEFINITION,
        "attributeGroup" => ATTRIBUTE_GROUP_REFERENCE,
        "all" | "choice" | "sequence" => COMPOSITOR,
        "any" => ANY,
        "anyAttribute" => ANY_ATTRIBUTE,
        "notation" => NOTATION,
        "simpleContent" => ID_ONLY,
        "complexContent" => COMPLEX_CONTENT,
        "extension" => DERIVATION_WITH_BASE,
        "restriction" if parent == Some("simpleType") => SIMPLE_RESTRICTION,
        "restriction" if parent == Some("simpleContent") => SIMPLE_RESTRICTION,
        "restriction" => DERIVATION_WITH_BASE,
        "list" => LIST,
        "union" => UNION,
        "minExclusive" | "minInclusive" | "maxExclusive" | "maxInclusive" | "fractionDigits" => {
            if local == "fractionDigits" {
                LENGTH_FACET
            } else {
                FACET
            }
        }
        "length" | "minLength" | "maxLength" => LENGTH_FACET,
        "totalDigits" => TOTAL_DIGITS,
        "whiteSpace" => WHITE_SPACE,
        "enumeration" | "pattern" => PATTERN,
        "unique" | "key" => IDENTITY,
        "keyref" => KEYREF,
        "selector" | "field" => XPATH,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Content specifications
// ---------------------------------------------------------------------------

struct Stage {
    names: &'static [&'static str],
    min: usize,
    max: Option<usize>,
}

const fn stage(names: &'static [&'static str], min: usize, max: Option<usize>) -> Stage {
    Stage { names, min, max }
}

const ANNOTATION: &[&str] = &["annotation"];
const COMPOSITORS: &[&str] = &["group", "all", "choice", "sequence"];
const ATTRIBUTE_USES: &[&str] = &["attribute", "attributeGroup"];
const FACETS: &[&str] = &[
    "minExclusive",
    "minInclusive",
    "maxExclusive",
    "maxInclusive",
    "totalDigits",
    "fractionDigits",
    "length",
    "minLength",
    "maxLength",
    "enumeration",
    "whiteSpace",
    "pattern",
];
const DECLARATIONS: &[&str] = &[
    "simpleType",
    "complexType",
    "group",
    "attributeGroup",
    "element",
    "attribute",
    "notation",
    "annotation",
];

/// The stages the children of an element must follow, in order.
fn content_spec(document: &SchemaDocument<'_>, element: usize) -> Option<Vec<Stage>> {
    let local = document.local(element);
    let parent = document
        .parent(element)
        .map(|parent| document.local(parent));
    Some(match local {
        "schema" => vec![
            stage(&["include", "import", "redefine", "annotation"], 0, None),
            stage(DECLARATIONS, 0, None),
        ],
        "redefine" => vec![stage(
            &[
                "annotation",
                "simpleType",
                "complexType",
                "group",
                "attributeGroup",
            ],
            0,
            None,
        )],
        "include" | "import" => vec![stage(ANNOTATION, 0, Some(1))],
        "annotation" => vec![stage(&["appinfo", "documentation"], 0, None)],
        "element" => vec![
            stage(ANNOTATION, 0, Some(1)),
            stage(&["simpleType", "complexType"], 0, Some(1)),
            stage(&["unique", "key", "keyref"], 0, None),
        ],
        "attribute" => vec![
            stage(ANNOTATION, 0, Some(1)),
            stage(&["simpleType"], 0, Some(1)),
        ],
        "complexType" => {
            let mut spec = vec![stage(ANNOTATION, 0, Some(1))];
            let first = document.children[element]
                .iter()
                .map(|child| document.local(*child))
                .find(|name| *name != "annotation");
            match first {
                Some("simpleContent") => spec.push(stage(&["simpleContent"], 1, Some(1))),
                Some("complexContent") => spec.push(stage(&["complexContent"], 1, Some(1))),
                _ => {
                    spec.push(stage(COMPOSITORS, 0, Some(1)));
                    spec.push(stage(ATTRIBUTE_USES, 0, None));
                    spec.push(stage(&["anyAttribute"], 0, Some(1)));
                }
            }
            spec
        }
        "simpleContent" | "complexContent" => vec![
            stage(ANNOTATION, 0, Some(1)),
            stage(&["restriction", "extension"], 1, Some(1)),
        ],
        "restriction" => match parent {
            Some("simpleType") => vec![
                stage(ANNOTATION, 0, Some(1)),
                stage(&["simpleType"], 0, Some(1)),
                stage(FACETS, 0, None),
            ],
            Some("simpleContent") => vec![
                stage(ANNOTATION, 0, Some(1)),
                stage(&["simpleType"], 0, Some(1)),
                stage(FACETS, 0, None),
                stage(ATTRIBUTE_USES, 0, None),
                stage(&["anyAttribute"], 0, Some(1)),
            ],
            _ => vec![
                stage(ANNOTATION, 0, Some(1)),
                stage(COMPOSITORS, 0, Some(1)),
                stage(ATTRIBUTE_USES, 0, None),
                stage(&["anyAttribute"], 0, Some(1)),
            ],
        },
        "extension" => match parent {
            Some("simpleContent") => vec![
                stage(ANNOTATION, 0, Some(1)),
                stage(ATTRIBUTE_USES, 0, None),
                stage(&["anyAttribute"], 0, Some(1)),
            ],
            _ => vec![
                stage(ANNOTATION, 0, Some(1)),
                stage(COMPOSITORS, 0, Some(1)),
                stage(ATTRIBUTE_USES, 0, None),
                stage(&["anyAttribute"], 0, Some(1)),
            ],
        },
        "simpleType" => vec![
            stage(ANNOTATION, 0, Some(1)),
            stage(&["restriction", "list", "union"], 1, Some(1)),
        ],
        "list" => vec![
            stage(ANNOTATION, 0, Some(1)),
            stage(&["simpleType"], 0, Some(1)),
        ],
        "union" => vec![
            stage(ANNOTATION, 0, Some(1)),
            stage(&["simpleType"], 0, None),
        ],
        "group" if matches!(parent, Some("schema" | "redefine")) => vec![
            stage(ANNOTATION, 0, Some(1)),
            stage(&["all", "choice", "sequence"], 1, Some(1)),
        ],
        "group" => vec![stage(ANNOTATION, 0, Some(1))],
        "attributeGroup" if matches!(parent, Some("schema" | "redefine")) => vec![
            stage(ANNOTATION, 0, Some(1)),
            stage(ATTRIBUTE_USES, 0, None),
            stage(&["anyAttribute"], 0, Some(1)),
        ],
        "attributeGroup" => vec![stage(ANNOTATION, 0, Some(1))],
        "all" => vec![stage(ANNOTATION, 0, Some(1)), stage(&["element"], 0, None)],
        "choice" | "sequence" => vec![
            stage(ANNOTATION, 0, Some(1)),
            stage(&["element", "group", "choice", "sequence", "any"], 0, None),
        ],
        "any" | "anyAttribute" | "notation" | "selector" | "field" => {
            vec![stage(ANNOTATION, 0, Some(1))]
        }
        "unique" | "key" | "keyref" => vec![
            stage(ANNOTATION, 0, Some(1)),
            stage(&["selector"], 1, Some(1)),
            stage(&["field"], 1, None),
        ],
        name if FACETS.contains(&name) => vec![stage(ANNOTATION, 0, Some(1))],
        _ => return None,
    })
}

/// Checks the children of `element` against its stages.
fn check_children(
    document: &SchemaDocument<'_>,
    element: usize,
    problems: &mut Vec<SchemaProblem>,
) {
    let Some(spec) = content_spec(document, element) else {
        return;
    };
    let local = document.local(element);
    let mut current = 0;
    let mut count = 0;
    'children: for &child in &document.children[element] {
        let Some(name) = document.names[child] else {
            // Elements outside the XML Schema namespace are not allowed in
            // the content of schema components.
            problems.push(SchemaProblem {
                range: document.name_range(child),
                rule: INVALID_ELEMENT,
                message: format!(
                    "the element <{}> is not allowed in xs:{local}",
                    &document.source[document.name_range(child)]
                ),
            });
            continue;
        };
        loop {
            let Some(stage) = spec.get(current) else {
                problems.push(SchemaProblem {
                    range: document.name_range(child),
                    rule: INVALID_ELEMENT,
                    message: format!("xs:{name} is not allowed here in xs:{local}"),
                });
                continue 'children;
            };
            if stage.names.contains(&name) && stage.max.is_none_or(|max| count < max) {
                count += 1;
                continue 'children;
            }
            if stage.names.contains(&name) {
                // The stage is full: this occurrence is one too many.
                problems.push(SchemaProblem {
                    range: document.name_range(child),
                    rule: INVALID_ELEMENT,
                    message: format!(
                        "xs:{local} allows at most {} of xs:{name}",
                        stage.max.unwrap_or(0)
                    ),
                });
                continue 'children;
            }
            if count < stage.min {
                problems.push(SchemaProblem {
                    range: document.name_range(child),
                    rule: INVALID_ELEMENT,
                    message: format!(
                        "xs:{name} is not allowed here: {} is required first in xs:{local}",
                        describe(stage.names)
                    ),
                });
                continue 'children;
            }
            current += 1;
            count = 0;
        }
    }
    // Required stages that were not reached.
    let mut missing = None;
    for (index, stage) in spec.iter().enumerate().skip(current) {
        let seen = if index == current { count } else { 0 };
        if seen < stage.min {
            missing = Some(stage);
            break;
        }
    }
    if let Some(stage) = missing {
        problems.push(SchemaProblem {
            range: document.name_range(element),
            rule: MISSING_CONTENT,
            message: format!("xs:{local} requires {}", describe(stage.names)),
        });
    }
}

fn describe(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("xs:{name}"))
        .collect::<Vec<_>>()
        .join(" or ")
}

// ---------------------------------------------------------------------------
// Attribute values
// ---------------------------------------------------------------------------

fn collapse(value: &str) -> String {
    value.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

fn check_value(
    document: &SchemaDocument<'_>,
    element: usize,
    attribute: &XmlAttribute,
    name: &str,
    kind: Kind,
    problems: &mut Vec<SchemaProblem>,
) {
    let Some(raw) = attribute.value(document.source) else {
        return;
    };
    let value = collapse(&decode_references(raw));
    let range = attribute.value.clone().unwrap_or(attribute.name.clone());
    let problem = |message: String| SchemaProblem {
        range: range.clone(),
        rule: INVALID_VALUE,
        message,
    };
    let valid = match kind {
        Kind::Id | Kind::NcName => is_ncname(&value),
        Kind::QName => is_qname(&value) && prefix_declared(document, element, &value),
        Kind::QNameList => {
            !value.is_empty()
                && value
                    .split(' ')
                    .all(|item| is_qname(item) && prefix_declared(document, element, item))
        }
        Kind::NonNegative => value.bytes().all(|byte| byte.is_ascii_digit()) && !value.is_empty(),
        Kind::Positive => {
            !value.is_empty()
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && value.bytes().any(|byte| byte != b'0')
        }
        Kind::MaxOccurs => {
            value == "unbounded"
                || (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        }
        Kind::Boolean => matches!(value.as_str(), "true" | "false" | "1" | "0"),
        Kind::AnyUri => true,
        Kind::Text => true,
        Kind::Enum(allowed) => allowed.contains(&value.as_str()),
        Kind::Set(allowed) => {
            value == "#all"
                || value
                    .split(' ')
                    .all(|token| token.is_empty() || allowed.contains(&token))
        }
        Kind::Namespace => {
            let tokens = value.split(' ').collect::<Vec<_>>();
            value.is_empty()
                || if tokens.len() == 1 {
                    !tokens[0].starts_with("##")
                        || matches!(
                            tokens[0],
                            "##any" | "##other" | "##local" | "##targetNamespace"
                        )
                } else {
                    tokens.iter().all(|token| {
                        !token.starts_with("##")
                            || matches!(*token, "##local" | "##targetNamespace")
                    })
                }
        }
    };
    if !valid {
        let expected = match kind {
            Kind::Id | Kind::NcName => "a name without colon (NCName)".to_owned(),
            Kind::QName | Kind::QNameList => "a qualified name with a declared prefix".to_owned(),
            Kind::NonNegative => "a non-negative integer".to_owned(),
            Kind::Positive => "a positive integer".to_owned(),
            Kind::MaxOccurs => "a non-negative integer or 'unbounded'".to_owned(),
            Kind::Boolean => "true, false, 1 or 0".to_owned(),
            Kind::AnyUri => "a valid URI reference".to_owned(),
            Kind::Enum(allowed) => format!("one of {}", allowed.join(", ")),
            Kind::Set(allowed) => format!("#all or a list of {}", allowed.join(", ")),
            Kind::Namespace => {
                "##any, ##other, or a list of URIs, ##local and ##targetNamespace".to_owned()
            }
            Kind::Text => "a valid value".to_owned(),
        };
        problems.push(problem(format!(
            "'{value}' is not a valid value for '{name}' (expected {expected})"
        )));
    }
}

fn prefix_declared(document: &SchemaDocument<'_>, element: usize, qname: &str) -> bool {
    match qname.split_once(':') {
        Some((prefix, _)) => prefix == "xml" || document.namespace(element, Some(prefix)).is_some(),
        None => true,
    }
}

fn check_attributes(
    document: &SchemaDocument<'_>,
    element: usize,
    ids: &mut HashMap<String, usize>,
    problems: &mut Vec<SchemaProblem>,
) {
    let Some(spec) = attribute_spec(document, element) else {
        problems.push(SchemaProblem {
            range: document.name_range(element),
            rule: INVALID_ELEMENT,
            message: format!("xs:{} is not a schema element", document.local(element)),
        });
        return;
    };
    let local = document.local(element);
    let mut seen = Vec::new();
    for attribute in &document.attributes[element] {
        let name = attribute.name(document.source);
        if name == "xmlns" || name.starts_with("xmlns:") {
            continue;
        }
        if let Some((prefix, _)) = name.split_once(':') {
            // Attributes of other namespaces are allowed (`xml:lang`, foreign
            // annotations), those of the XML Schema namespace are not.
            let namespace = if prefix == "xml" {
                Some(Some(XML_NAMESPACE))
            } else {
                document.namespace(element, Some(prefix))
            };
            if namespace == Some(Some(XSD_NAMESPACE)) {
                problems.push(SchemaProblem {
                    range: attribute.name.clone(),
                    rule: INVALID_ATTRIBUTE,
                    message: format!(
                        "attributes of the XML Schema namespace are not allowed on xs:{local}"
                    ),
                });
            }
            continue;
        }
        seen.push(name);
        match spec.iter().find(|(allowed, _, _)| *allowed == name) {
            Some((_, kind, _)) => {
                check_value(document, element, attribute, name, *kind, problems);
                if name == "id"
                    && let Some(id) = document.value(element, "id")
                {
                    let id = collapse(&id);
                    if let Some(first) = ids.get(&id).copied() {
                        let _ = first;
                        problems.push(SchemaProblem {
                            range: attribute.value.clone().unwrap_or(attribute.name.clone()),
                            rule: INVALID_VALUE,
                            message: format!("the id '{id}' is used twice in the schema"),
                        });
                    } else {
                        ids.insert(id, element);
                    }
                }
            }
            None => problems.push(SchemaProblem {
                range: attribute.name.clone(),
                rule: INVALID_ATTRIBUTE,
                message: format!("the attribute '{name}' is not allowed on xs:{local}"),
            }),
        }
    }
    for (name, _, required) in spec {
        if *required && !seen.contains(name) {
            problems.push(SchemaProblem {
                range: document.name_range(element),
                rule: MISSING_ATTRIBUTE,
                message: format!("xs:{local} requires the attribute '{name}'"),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Constraints across attributes and children
// ---------------------------------------------------------------------------

fn problem(
    document: &SchemaDocument<'_>,
    element: usize,
    rule: &'static str,
    message: impl Into<String>,
) -> SchemaProblem {
    SchemaProblem {
        range: document.name_range(element),
        rule,
        message: message.into(),
    }
}

fn occurs(document: &SchemaDocument<'_>, element: usize) -> (usize, Option<usize>) {
    let min = document
        .value(element, "minOccurs")
        .and_then(|value| collapse(&value).parse().ok())
        .unwrap_or(1);
    let max = match document
        .value(element, "maxOccurs")
        .map(|value| collapse(&value))
    {
        Some(value) if value == "unbounded" => None,
        Some(value) => value.parse().ok().or(Some(1)),
        None => Some(1),
    };
    (min, max)
}

fn check_constraints(
    document: &SchemaDocument<'_>,
    element: usize,
    problems: &mut Vec<SchemaProblem>,
) {
    let local = document.local(element);
    let parent = document
        .parent(element)
        .map(|parent| document.local(parent));
    let top_level = matches!(parent, Some("schema" | "redefine"));
    let child_names = document.children[element]
        .iter()
        .map(|child| document.local(*child))
        .collect::<Vec<_>>();
    match local {
        "element" if !top_level => {
            let reference = document.has_attribute(element, "ref");
            let named = document.has_attribute(element, "name");
            if reference == named {
                problems.push(problem(
                    document,
                    element,
                    INVALID_COMBINATION,
                    "a local xs:element has either 'name' or 'ref'",
                ));
            }
            if reference {
                for attribute in &document.attributes[element] {
                    let name = attribute.name(document.source);
                    if !name.contains(':')
                        && !name.starts_with("xmlns")
                        && !ELEMENT_REFERENCE.contains(&name)
                    {
                        problems.push(SchemaProblem {
                            range: attribute.name.clone(),
                            rule: INVALID_COMBINATION,
                            message: format!(
                                "the attribute '{name}' cannot be used with 'ref' on xs:element"
                            ),
                        });
                    }
                }
                if child_names.iter().any(|name| {
                    matches!(
                        *name,
                        "simpleType" | "complexType" | "unique" | "key" | "keyref"
                    )
                }) {
                    problems.push(problem(
                        document,
                        element,
                        INVALID_COMBINATION,
                        "an xs:element with 'ref' has no type definition or identity constraint",
                    ));
                }
            }
            if matches!(parent, Some("all")) {
                let (min, max) = occurs(document, element);
                if min > 1 || max.is_none_or(|max| max > 1) {
                    problems.push(problem(
                        document,
                        element,
                        INVALID_VALUE,
                        "an element of an xs:all group has minOccurs 0 or 1 and maxOccurs 1",
                    ));
                }
            }
        }
        "attribute" if !top_level => {
            let reference = document.has_attribute(element, "ref");
            let named = document.has_attribute(element, "name");
            if reference == named {
                problems.push(problem(
                    document,
                    element,
                    INVALID_COMBINATION,
                    "a local xs:attribute has either 'name' or 'ref'",
                ));
            }
            if reference {
                for attribute in &document.attributes[element] {
                    let name = attribute.name(document.source);
                    if !name.contains(':')
                        && !name.starts_with("xmlns")
                        && !ATTRIBUTE_REFERENCE.contains(&name)
                    {
                        problems.push(SchemaProblem {
                            range: attribute.name.clone(),
                            rule: INVALID_COMBINATION,
                            message: format!(
                                "the attribute '{name}' cannot be used with 'ref' on xs:attribute"
                            ),
                        });
                    }
                }
                if child_names.contains(&"simpleType") {
                    problems.push(problem(
                        document,
                        element,
                        INVALID_COMBINATION,
                        "an xs:attribute with 'ref' has no simpleType",
                    ));
                }
            }
        }
        _ => {}
    }
    match local {
        "element" | "attribute" => {
            if document.has_attribute(element, "default")
                && document.has_attribute(element, "fixed")
            {
                problems.push(problem(
                    document,
                    element,
                    INVALID_COMBINATION,
                    format!("xs:{local} cannot have both 'default' and 'fixed'"),
                ));
            }
            if document.has_attribute(element, "type")
                && child_names
                    .iter()
                    .any(|name| matches!(*name, "simpleType" | "complexType"))
            {
                problems.push(problem(
                    document,
                    element,
                    INVALID_COMBINATION,
                    format!("xs:{local} with 'type' has no anonymous type definition"),
                ));
            }
            if local == "attribute" {
                if document.value(element, "name").as_deref().map(str::trim) == Some("xmlns") {
                    problems.push(problem(
                        document,
                        element,
                        INVALID_VALUE,
                        "an attribute cannot be named 'xmlns'",
                    ));
                }
                if document.has_attribute(element, "default")
                    && document
                        .value(element, "use")
                        .is_some_and(|value| collapse(&value) != "optional")
                {
                    problems.push(problem(
                        document,
                        element,
                        INVALID_COMBINATION,
                        "an attribute with a 'default' value must have use='optional'",
                    ));
                }
            }
        }
        _ => {}
    }
    match local {
        "element" | "all" | "choice" | "sequence" | "any" | "group"
            if !top_level
                && (document.has_attribute(element, "minOccurs")
                    || document.has_attribute(element, "maxOccurs")) =>
        {
            let (min, max) = occurs(document, element);
            if let Some(max) = max {
                if max == 0 && min != 0 {
                    // `maxOccurs="0"` requires `minOccurs="0"`.
                    problems.push(problem(
                        document,
                        element,
                        INVALID_VALUE,
                        "maxOccurs 0 requires minOccurs 0",
                    ));
                } else if min > max {
                    problems.push(problem(
                        document,
                        element,
                        INVALID_VALUE,
                        format!("minOccurs ({min}) is greater than maxOccurs ({max})"),
                    ));
                }
            }
        }
        "all"
            if !matches!(
                parent,
                Some("complexType" | "restriction" | "extension" | "group")
            ) =>
        {
            problems.push(problem(
                document,
                element,
                INVALID_ELEMENT,
                "xs:all is only allowed as the content model of a type or of a group",
            ));
        }
        _ => {}
    }
    match local {
        "simpleType" => {
            // Exactly one derivation: checked by the content spec.
        }
        "restriction" if parent == Some("simpleType") || parent == Some("simpleContent") => {
            let inline = child_names.contains(&"simpleType");
            let based = document.has_attribute(element, "base");
            if parent == Some("simpleType") && inline == based {
                problems.push(problem(
                    document,
                    element,
                    INVALID_COMBINATION,
                    "a simple type restriction has either a 'base' or an anonymous simpleType",
                ));
            }
            if parent == Some("simpleContent") && !based {
                problems.push(problem(
                    document,
                    element,
                    MISSING_ATTRIBUTE,
                    "xs:restriction requires the attribute 'base'",
                ));
            }
        }
        "list" => {
            let inline = child_names.contains(&"simpleType");
            let based = document.has_attribute(element, "itemType");
            if inline == based {
                problems.push(problem(
                    document,
                    element,
                    INVALID_COMBINATION,
                    "xs:list has either 'itemType' or an anonymous simpleType",
                ));
            }
        }
        "union" => {
            let inline = child_names.contains(&"simpleType");
            let members = document.has_attribute(element, "memberTypes");
            if !inline && !members {
                problems.push(problem(
                    document,
                    element,
                    MISSING_ATTRIBUTE,
                    "xs:union requires 'memberTypes' or at least one simpleType",
                ));
            }
        }
        _ => {}
    }
}

/// Checks the schema representation constraints of a schema document.
pub fn check_schema_document(source: &str) -> Vec<SchemaProblem> {
    let Some(document) = SchemaDocument::read(source) else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    let mut ids = HashMap::new();
    let mut stack = vec![document.root];
    while let Some(element) = stack.pop() {
        if document.names[element].is_none() {
            continue;
        }
        let local = document.local(element);
        check_attributes(&document, element, &mut ids, &mut problems);
        // The content of annotations is free.
        if matches!(local, "appinfo" | "documentation") {
            continue;
        }
        check_children(&document, element, &mut problems);
        check_constraints(&document, element, &mut problems);
        stack.extend(document.children[element].iter().rev().copied());
    }
    problems.sort_by_key(|problem| (problem.range.start, problem.range.end));
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(body: &str) -> String {
        format!(r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{body}</xs:schema>"###)
    }

    fn rules(body: &str) -> Vec<(&'static str, String)> {
        let source = schema(body);
        check_schema_document(&source)
            .into_iter()
            .map(|problem| (problem.rule, source[problem.range].to_owned()))
            .collect()
    }

    #[test]
    fn accepts_valid_schemas() {
        for body in [
            r###"<xs:element name="a" type="xs:string"/>"###,
            r###"<xs:annotation><xs:documentation xml:lang="en">x <b>y</b></xs:documentation></xs:annotation>
               <xs:element name="a"><xs:complexType mixed="true"><xs:sequence>
                 <xs:element ref="a" minOccurs="0" maxOccurs="unbounded"/><xs:any namespace="##other" processContents="lax"/>
               </xs:sequence><xs:attribute name="x" type="xs:int" use="required"/><xs:anyAttribute/></xs:complexType>
               <xs:unique name="u"><xs:selector xpath="a"/><xs:field xpath="@x"/></xs:unique></xs:element>"###,
            r###"<xs:simpleType name="t"><xs:restriction base="xs:string"><xs:pattern value="a+"/><xs:enumeration value="a"/></xs:restriction></xs:simpleType>
               <xs:simpleType name="l"><xs:list itemType="t"/></xs:simpleType>
               <xs:simpleType name="u"><xs:union memberTypes="t l"><xs:simpleType><xs:restriction base="xs:int"/></xs:simpleType></xs:union></xs:simpleType>"###,
            r###"<xs:complexType name="c"><xs:simpleContent><xs:extension base="xs:string"><xs:attribute name="q"/></xs:extension></xs:simpleContent></xs:complexType>
               <xs:group name="g"><xs:choice><xs:element name="a"/></xs:choice></xs:group>
               <xs:attributeGroup name="ag"><xs:attribute name="p"/></xs:attributeGroup>
               <xs:notation name="n" public="x"/>"###,
        ] {
            assert_eq!(rules(body), vec![], "{body}");
        }
    }

    #[test]
    fn reports_unknown_and_misplaced_elements() {
        assert_eq!(
            rules(r###"<xs:element name="a"><xs:foo/></xs:element>"###)
                .first()
                .map(|r| r.0),
            Some(INVALID_ELEMENT)
        );
        // A second type definition and an annotation after the type.
        assert_eq!(
            rules(
                r###"<xs:element name="a"><xs:complexType/><xs:simpleType><xs:restriction base="xs:int"/></xs:simpleType></xs:element>"###
            )
            .len(),
            1
        );
        assert_eq!(
            rules(r###"<xs:element name="a"><xs:complexType/><xs:annotation/></xs:element>"###)
                .len(),
            1
        );
        // Content in xs:any, text-only elements, foreign elements.
        assert_eq!(rules(r###"<xs:complexType name="c"><xs:sequence><xs:any><xs:element name="x"/></xs:any></xs:sequence></xs:complexType>"###).len(), 1);
        assert_eq!(
            rules(r###"<xs:element name="a"><foo/></xs:element>"###).len(),
            1
        );
        assert_eq!(
            rules(r###"<xs:simpleType name="s"/>"###),
            vec![(MISSING_CONTENT, "xs:simpleType".to_owned())]
        );
    }

    #[test]
    fn reports_attribute_problems() {
        let bad = |body: &str, rule: &'static str| {
            let found = rules(body);
            assert!(
                found.iter().any(|(found, _)| *found == rule),
                "{body}: {found:?}"
            );
        };
        bad(r###"<xs:element name="a" foo="1"/>"###, INVALID_ATTRIBUTE);
        bad(r###"<xs:element/>"###, MISSING_ATTRIBUTE);
        bad(r###"<xs:element name="1a"/>"###, INVALID_VALUE);
        bad(
            r###"<xs:element name="a" nillable="maybe"/>"###,
            INVALID_VALUE,
        );
        bad(
            r###"<xs:complexType name="c"><xs:sequence><xs:element name="a" minOccurs="2" maxOccurs="1"/></xs:sequence></xs:complexType>"###,
            INVALID_VALUE,
        );
        bad(
            r###"<xs:complexType name="c"><xs:sequence><xs:element ref="a" name="b"/></xs:sequence></xs:complexType>"###,
            INVALID_COMBINATION,
        );
        bad(
            r###"<xs:complexType name="c"><xs:sequence><xs:element ref="a" type="xs:int"/></xs:sequence></xs:complexType>"###,
            INVALID_COMBINATION,
        );
        bad(
            r###"<xs:element name="a" default="1" fixed="1"/>"###,
            INVALID_COMBINATION,
        );
        bad(
            r###"<xs:element name="a" type="xs:int"><xs:simpleType><xs:restriction base="xs:int"/></xs:simpleType></xs:element>"###,
            INVALID_COMBINATION,
        );
        bad(r###"<xs:element name="a" type="q:t"/>"###, INVALID_VALUE);
        bad(
            r###"<xs:complexType name="c"><xs:attribute name="x" use="sometimes"/></xs:complexType>"###,
            INVALID_VALUE,
        );
        bad(
            r###"<xs:complexType name="c"><xs:attribute name="xmlns"/></xs:complexType>"###,
            INVALID_VALUE,
        );
        bad(
            r###"<xs:complexType name="c"><xs:attribute name="x" default="1" use="required"/></xs:complexType>"###,
            INVALID_COMBINATION,
        );
        bad(
            r###"<xs:element name="a" id="i"/><xs:element name="b" id="i"/>"###,
            INVALID_VALUE,
        );
        bad(
            r###"<xs:simpleType name="s"><xs:restriction base="xs:string"><xs:whiteSpace value="fold"/></xs:restriction></xs:simpleType>"###,
            INVALID_VALUE,
        );
        bad(
            r###"<xs:simpleType name="s"><xs:restriction base="xs:int"><xs:totalDigits value="0"/></xs:restriction></xs:simpleType>"###,
            INVALID_VALUE,
        );
        bad(
            r###"<xs:simpleType name="s"><xs:list/></xs:simpleType>"###,
            INVALID_COMBINATION,
        );
        bad(
            r###"<xs:simpleType name="s"><xs:union/></xs:simpleType>"###,
            MISSING_ATTRIBUTE,
        );
        bad(
            r###"<xs:complexType name="c"><xs:all><xs:element name="a" maxOccurs="2"/></xs:all></xs:complexType>"###,
            INVALID_VALUE,
        );
        bad(
            r###"<xs:complexType name="c"><xs:sequence><xs:any namespace="##foo"/></xs:sequence></xs:complexType>"###,
            INVALID_VALUE,
        );
    }

    #[test]
    fn ignores_documents_that_are_not_schemas() {
        assert_eq!(check_schema_document("<root><xs:element/></root>"), vec![]);
        assert_eq!(check_schema_document("<xs:schema/>"), vec![]);
    }
}
