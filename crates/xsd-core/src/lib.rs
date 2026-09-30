//! XSD model and parsing shared by the LSP server.

mod component_check;
pub(crate) mod content;
pub mod datatypes;
pub mod identity;
mod instance_check;
pub mod model;
pub mod pattern;
mod reference_check;
pub mod schema_check;
mod simple_type_check;

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Component, Path, PathBuf},
    rc::Rc,
    str,
    sync::Arc,
};

use quick_xml::{Reader, events::Event};

use crate::{
    datatypes::{BuiltinType, SimpleType},
    model::{XsdInstanceStep, XsdModel, XsdModelSet},
};

const MAX_XSD_SOURCE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum number of schema documents loaded for one document (through
/// `xsi:schemaLocation`, `xs:include`, `xs:import`...), to bound the work
/// done for a hostile or runaway schema graph.
pub const MAX_SCHEMA_DOCUMENTS: usize = 256;

/// Cardinality of an XSD element.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct XsdOccurs {
    pub min: usize,
    pub max: Option<usize>,
}

impl Default for XsdOccurs {
    fn default() -> Self {
        Self {
            min: 1,
            max: Some(1),
        }
    }
}

/// Element declared by an XSD schema.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct XsdElement {
    pub name: String,
    pub occurs: XsdOccurs,
    pub type_name: Option<String>,
    pub form: Option<String>,
    pub default: Option<String>,
    pub fixed: Option<String>,
    pub nillable: bool,
}

/// Simple restriction carried by an XSD type.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct XsdRestriction {
    pub length: Option<usize>,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub min_inclusive: Option<String>,
    pub max_inclusive: Option<String>,
    pub min_exclusive: Option<String>,
    pub max_exclusive: Option<String>,
    pub total_digits: Option<usize>,
    pub fraction_digits: Option<usize>,
    pub white_space: Option<String>,
    pub pattern: Option<String>,
}

/// Minimal XSD schema.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct XsdSchema {
    pub target_namespace: Option<String>,
    pub element_form_default: Option<String>,
    pub attribute_form_default: Option<String>,
    pub elements: Vec<XsdElement>,
    pub children: HashMap<String, Vec<String>>,
    pub choices: HashMap<String, Vec<String>>,
    pub alls: HashMap<String, Vec<String>>,
    pub attributes: HashMap<String, Vec<String>>,
    pub required_attributes: HashMap<String, Vec<String>>,
    pub attribute_defaults: HashMap<String, String>,
    pub attribute_fixed: HashMap<String, String>,
    pub enumerations: HashMap<String, Vec<String>>,
    pub restrictions: HashMap<String, XsdRestriction>,
    pub lists: HashMap<String, String>,
    pub unions: HashMap<String, Vec<String>>,
    pub attribute_groups: HashMap<String, Vec<String>>,
    pub model_groups: HashMap<String, Vec<String>>,
    pub substitution_groups: HashMap<String, Vec<String>>,
    pub any_children: HashMap<String, bool>,
    pub any_attributes: HashMap<String, bool>,
    pub complex_extensions: HashMap<String, String>,
    pub simple_extensions: HashMap<String, String>,
    pub includes: Vec<String>,
    pub imports: Vec<(Option<String>, String)>,
    /// Component models of the schema documents, used to check values
    /// against their simple types.
    pub models: Vec<Arc<XsdModel>>,
    /// Errors of the schema components that do not prevent using the schema
    /// (invalid identity constraints...): the schema is invalid, but
    /// instances are still validated against what could be read.
    pub problems: Vec<String>,
}

/// Merges several XSD schemas into a model usable by validation.
pub fn merge_schemas(schemas: impl IntoIterator<Item = XsdSchema>) -> XsdSchema {
    let mut merged = XsdSchema::default();
    for schema in schemas {
        if merged.target_namespace.is_none() {
            merged.target_namespace = schema.target_namespace;
        }
        if merged.element_form_default.is_none() {
            merged.element_form_default = schema.element_form_default;
        }
        if merged.attribute_form_default.is_none() {
            merged.attribute_form_default = schema.attribute_form_default;
        }
        merged.elements.extend(schema.elements);
        merge_string_lists(&mut merged.children, schema.children);
        merge_string_lists(&mut merged.choices, schema.choices);
        merge_string_lists(&mut merged.alls, schema.alls);
        merge_string_lists(&mut merged.attributes, schema.attributes);
        merge_string_lists(&mut merged.required_attributes, schema.required_attributes);
        merged.attribute_defaults.extend(schema.attribute_defaults);
        merged.attribute_fixed.extend(schema.attribute_fixed);
        merge_string_lists(&mut merged.enumerations, schema.enumerations);
        merged.restrictions.extend(schema.restrictions);
        merged.lists.extend(schema.lists);
        merged.unions.extend(schema.unions);
        merge_string_lists(&mut merged.attribute_groups, schema.attribute_groups);
        merge_string_lists(&mut merged.model_groups, schema.model_groups);
        merge_string_lists(&mut merged.substitution_groups, schema.substitution_groups);
        merged.any_children.extend(schema.any_children);
        merged.any_attributes.extend(schema.any_attributes);
        merged.complex_extensions.extend(schema.complex_extensions);
        merged.simple_extensions.extend(schema.simple_extensions);
        merged.includes.extend(schema.includes);
        merged.imports.extend(schema.imports);
        merged.models.extend(schema.models);
        for problem in schema.problems {
            if !merged.problems.contains(&problem) {
                merged.problems.push(problem);
            }
        }
    }
    // A document listed twice (by itself and through an include) is one.
    let mut unique: Vec<Arc<XsdModel>> = Vec::new();
    for model in &merged.models {
        if !unique.iter().any(|known| **known == **model) {
            unique.push(Arc::clone(model));
        }
    }
    merged.models = unique;
    let set = XsdModelSet::new(merged.models.clone());
    // The set is complete when every include, import and redefine with a
    // location was loaded (each loaded document is a model).
    let locations = merged
        .includes
        .iter()
        .chain(merged.imports.iter().map(|(_, location)| location))
        .collect::<HashSet<_>>();
    let complete = merged.models.len() > locations.len();
    let imported = merged
        .imports
        .iter()
        .filter_map(|(namespace, _)| namespace.clone())
        .collect::<HashSet<_>>();
    for problem in set
        .identity_problems()
        .into_iter()
        .chain(set.component_problems())
        .chain(set.reference_problems(complete, &imported))
    {
        if !merged.problems.contains(&problem) {
            merged.problems.push(problem);
        }
    }
    merged
}

fn merge_string_lists(
    target: &mut HashMap<String, Vec<String>>,
    source: HashMap<String, Vec<String>>,
) {
    for (key, values) in source {
        let entry = target.entry(key).or_default();
        for value in values {
            if !entry.contains(&value) {
                entry.push(value);
            }
        }
    }
}

/// XSD reference extracted from an XML document.
#[derive(Debug, PartialEq, Eq)]
pub struct SchemaReference {
    pub namespace: Option<String>,
    pub path: PathBuf,
    /// Which construct named the schema (`xs:include`, `xs:import`,
    /// `xsi:schemaLocation` ...).
    pub kind: SchemaLocationKind,
}

/// Item proposed by XSD completion.
#[derive(Debug, PartialEq, Eq)]
pub struct XsdCompletion {
    pub label: String,
    pub insert_text: String,
}

/// XSD validation rule behind a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XsdDiagnosticKind {
    MissingRoot,
    UnknownRoot,
    UnexpectedElement,
    UnexpectedOrder,
    MissingElement,
    TooManyElements,
    FixedValue,
    InvalidContent,
    NotNillable,
    UnexpectedAttribute,
    MissingAttribute,
    /// Value outside the `enumeration` of its type.
    InvalidEnumeration,
    /// Attribute value invalid for its simple type.
    InvalidAttributeValue,
    /// `xs:ID` value used twice in the document.
    DuplicateId,
    /// `xs:IDREF(S)` value naming no `xs:ID` of the document.
    UnknownIdref,
    /// Key sequence repeated for an `xs:unique` or `xs:key`.
    DuplicateKey,
    /// Field of an `xs:key` missing (or nil) on a selected element.
    MissingKeyField,
    /// Field selecting several nodes, or an element without simple value.
    InvalidKeyField,
    /// `xs:keyref` value matching no key in scope.
    UnknownKeyref,
    /// `xsi:type` unknown, not derived from the declared type, blocked or
    /// abstract; abstract declared type without `xsi:type`.
    InvalidXsiType,
}

impl XsdDiagnosticKind {
    /// Stable identifier of the rule (published in `data.rule` by the LSP).
    pub fn id(self) -> &'static str {
        match self {
            Self::MissingRoot => "missingRoot",
            Self::UnknownRoot => "unknownRoot",
            Self::UnexpectedElement => "unexpectedElement",
            Self::UnexpectedOrder => "unexpectedOrder",
            Self::MissingElement => "missingElement",
            Self::TooManyElements => "tooManyElements",
            Self::FixedValue => "fixedValue",
            Self::InvalidContent => "invalidContent",
            Self::NotNillable => "notNillable",
            Self::UnexpectedAttribute => "unexpectedAttribute",
            Self::MissingAttribute => "missingAttribute",
            Self::InvalidEnumeration => "invalidEnumeration",
            Self::InvalidAttributeValue => "invalidAttributeValue",
            Self::DuplicateId => "duplicateId",
            Self::UnknownIdref => "unknownIdref",
            Self::DuplicateKey => "duplicateKey",
            Self::MissingKeyField => "missingKeyField",
            Self::InvalidKeyField => "invalidKeyField",
            Self::UnknownKeyref => "unknownKeyref",
            Self::InvalidXsiType => "invalidXsiType",
        }
    }
}

/// Minimal XSD validation diagnostic.
#[derive(Debug, PartialEq, Eq)]
pub struct XsdDiagnostic {
    pub kind: XsdDiagnosticKind,
    pub message: String,
}

/// XSD diagnostic associated with a range of the XML document.
#[derive(Debug, PartialEq, Eq)]
pub struct LocatedXsdDiagnostic {
    pub kind: XsdDiagnosticKind,
    pub message: String,
    /// Start of the relevant start tag (`<`), or of the faulty attribute
    /// value.
    pub offset: usize,
    /// End of the name of that tag (`offset` if the element is unknown), or
    /// of the attribute value.
    pub end: usize,
}

/// Parses an `xs:schema`, its elements and a first `xs:sequence`.
pub fn parse_xsd(source: &str) -> Result<XsdSchema, String> {
    if source.len() > MAX_XSD_SOURCE_BYTES {
        return Err("XSD schema too large".to_owned());
    }
    let mut reader = Reader::from_str(source);
    let mut schema = XsdSchema::default();
    let mut element_stack = Vec::new();
    let mut model_stack: Vec<String> = Vec::new();
    let mut sequence_depth = 0usize;
    let mut choice_depth = 0usize;
    let mut all_depth = 0usize;
    let mut complex_content_depth = 0usize;
    let mut simple_content_depth = 0usize;
    let mut simple_type_stack: Vec<Option<String>> = Vec::new();
    // `(parent, child)` pairs already in `schema.children`: a child is
    // listed once, and a schema repeating it (nested elements of the same
    // name) keeps the list and the validation linear.
    let mut listed_children: HashSet<(String, String)> = HashSet::new();
    let mut depth = 0usize;

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                depth += 1;
                if depth > model::MAX_SCHEMA_DEPTH {
                    return Err(format!(
                        "the schema is nested more than {} levels deep",
                        model::MAX_SCHEMA_DEPTH
                    ));
                }
                let element_name = element.name();
                let current_name = local_name(element_name.as_ref());
                let simple_name = if current_name == "simpleType" {
                    attribute(&element, "name").or_else(|| {
                        model_stack
                            .last()
                            .map(|parent| format!("__anonymous:{parent}"))
                    })
                } else {
                    None
                };
                if current_name == "enumeration"
                    && let Some(simple_type) = simple_type_stack
                        .iter()
                        .rev()
                        .find_map(|name| name.as_ref())
                    && let Some(value) = attribute(&element, "value")
                {
                    schema
                        .enumerations
                        .entry(simple_type.clone())
                        .or_default()
                        .push(value);
                }
                if current_name == "pattern"
                    && let Some(simple_type) = simple_type_stack
                        .iter()
                        .rev()
                        .find_map(|name| name.as_ref())
                    && let Some(pattern) = attribute(&element, "value")
                {
                    schema
                        .restrictions
                        .entry(simple_type.clone())
                        .or_default()
                        .pattern = Some(pattern);
                }
                if matches!(current_name, "length" | "minLength" | "maxLength")
                    && let Some(simple_type) = simple_type_stack
                        .iter()
                        .rev()
                        .find_map(|name| name.as_ref())
                    && let Some(value) = parse_optional_usize(attribute(&element, "value"))?
                {
                    let restriction = schema.restrictions.entry(simple_type.clone()).or_default();
                    match current_name {
                        "length" => restriction.length = Some(value),
                        "minLength" => restriction.min_length = Some(value),
                        _ => restriction.max_length = Some(value),
                    }
                }
                if let Some(facet) = digit_facet(current_name)
                    && let Some(simple_type) = simple_type_stack
                        .iter()
                        .rev()
                        .find_map(|name| name.as_ref())
                    && let Some(value) = parse_optional_usize(attribute(&element, "value"))?
                {
                    set_digit_facet(
                        schema.restrictions.entry(simple_type.clone()).or_default(),
                        facet,
                        value,
                    );
                }
                if let Some(facet) = numeric_facet(current_name)
                    && let Some(simple_type) = simple_type_stack
                        .iter()
                        .rev()
                        .find_map(|name| name.as_ref())
                    && let Some(value) = attribute(&element, "value")
                {
                    set_numeric_facet(
                        schema.restrictions.entry(simple_type.clone()).or_default(),
                        facet,
                        value,
                    );
                }
                if let Some(simple_type) = simple_type_stack
                    .iter()
                    .rev()
                    .find_map(|name| name.as_ref())
                {
                    if current_name == "list"
                        && let Some(item_type) = attribute(&element, "itemType")
                    {
                        schema.lists.insert(simple_type.clone(), item_type);
                    }
                    if current_name == "union"
                        && let Some(member_types) = attribute(&element, "memberTypes")
                    {
                        schema.unions.insert(
                            simple_type.clone(),
                            member_types.split_whitespace().map(str::to_owned).collect(),
                        );
                    }
                }
                simple_type_stack.push(simple_name);
                if current_name == "schema" {
                    schema.target_namespace = attribute(&element, "targetNamespace");
                    schema.element_form_default = attribute(&element, "elementFormDefault");
                    schema.attribute_form_default = attribute(&element, "attributeFormDefault");
                }
                if matches!(current_name, "include" | "redefine" | "override")
                    && let Some(location) = attribute(&element, "schemaLocation")
                {
                    schema.includes.push(location);
                }
                if current_name == "import"
                    && let Some(location) = attribute(&element, "schemaLocation")
                {
                    schema
                        .imports
                        .push((attribute(&element, "namespace"), location));
                }
                if current_name == "sequence" {
                    sequence_depth += 1;
                }
                if current_name == "choice" {
                    choice_depth += 1;
                }
                if current_name == "all" {
                    all_depth += 1;
                }
                if current_name == "any"
                    && let Some(parent) = model_stack.last()
                {
                    schema.any_children.insert(parent.clone(), true);
                }
                if current_name == "anyAttribute"
                    && let Some(parent) = model_stack.last()
                {
                    schema.any_attributes.insert(parent.clone(), true);
                }
                if current_name == "complexContent" {
                    complex_content_depth += 1;
                }
                if current_name == "simpleContent" {
                    simple_content_depth += 1;
                }
                if current_name == "extension"
                    && let Some(base) = attribute(&element, "base")
                    && let Some(element_name) = model_stack.last()
                {
                    if complex_content_depth > 0 {
                        schema
                            .complex_extensions
                            .insert(element_name.clone(), base.clone());
                    }
                    if simple_content_depth > 0 {
                        schema.simple_extensions.insert(element_name.clone(), base);
                    }
                }
                let declared_name = if current_name == "element" {
                    attribute(&element, "name").or_else(|| {
                        attribute(&element, "ref").map(|reference| {
                            reference
                                .rsplit(':')
                                .next()
                                .unwrap_or(&reference)
                                .to_owned()
                        })
                    })
                } else {
                    None
                };
                if current_name == "attribute"
                    && let Some(parent) = model_stack.last()
                    && let Some(name) = attribute(&element, "name").or_else(|| {
                        attribute(&element, "ref").map(|reference| {
                            reference
                                .rsplit(':')
                                .next()
                                .unwrap_or(&reference)
                                .to_owned()
                        })
                    })
                {
                    schema
                        .attributes
                        .entry(parent.clone())
                        .or_default()
                        .push(name.clone());
                    if attribute(&element, "use").as_deref() == Some("required") {
                        schema
                            .required_attributes
                            .entry(parent.clone())
                            .or_default()
                            .push(name.clone());
                    }
                    let key = format!("{parent}:{name}");
                    if let Some(value) = attribute(&element, "default") {
                        schema.attribute_defaults.insert(key.clone(), value);
                    }
                    if let Some(value) = attribute(&element, "fixed") {
                        schema.attribute_fixed.insert(key, value);
                    }
                }
                if let (Some(parent), Some(child)) = (model_stack.last(), declared_name.as_ref()) {
                    if sequence_depth > 0 && listed_children.insert((parent.clone(), child.clone()))
                    {
                        schema
                            .children
                            .entry(parent.clone())
                            .or_default()
                            .push(child.clone());
                    }
                    if choice_depth > 0 {
                        schema
                            .choices
                            .entry(parent.clone())
                            .or_default()
                            .push(child.clone());
                    }
                    if all_depth > 0 {
                        schema
                            .alls
                            .entry(parent.clone())
                            .or_default()
                            .push(child.clone());
                    }
                }
                if let Some(name) = declared_name {
                    if let Some(head) = attribute(&element, "substitutionGroup") {
                        schema
                            .substitution_groups
                            .entry(head)
                            .or_default()
                            .push(name.clone());
                    }
                    schema.elements.push(XsdElement {
                        name: name.clone(),
                        occurs: XsdOccurs {
                            min: match attribute(&element, "minOccurs") {
                                None => 1,
                                Some(value) => parse_count(&value)
                                    .ok_or_else(|| "invalid minOccurs".to_owned())?,
                            },
                            max: parse_max_occurs(attribute(&element, "maxOccurs"))?,
                        },
                        type_name: attribute(&element, "type"),
                        form: attribute(&element, "form"),
                        default: attribute(&element, "default"),
                        fixed: attribute(&element, "fixed"),
                        nillable: attribute(&element, "nillable").as_deref() == Some("true"),
                    });
                    model_stack.push(name.clone());
                    element_stack.push(Some(name));
                } else {
                    element_stack.push(None);
                }
            }
            Ok(Event::Empty(element)) => {
                let element_name = element.name();
                let current_name = local_name(element_name.as_ref());
                if current_name == "schema" {
                    schema.target_namespace = attribute(&element, "targetNamespace");
                    schema.element_form_default = attribute(&element, "elementFormDefault");
                    schema.attribute_form_default = attribute(&element, "attributeFormDefault");
                }
                if current_name == "any"
                    && let Some(parent) = model_stack.last()
                {
                    schema.any_children.insert(parent.clone(), true);
                }
                if current_name == "anyAttribute"
                    && let Some(parent) = model_stack.last()
                {
                    schema.any_attributes.insert(parent.clone(), true);
                }
                if current_name == "extension"
                    && let Some(base) = attribute(&element, "base")
                    && let Some(element_name) = model_stack.last()
                {
                    if complex_content_depth > 0 {
                        schema
                            .complex_extensions
                            .insert(element_name.clone(), base.clone());
                    }
                    if simple_content_depth > 0 {
                        schema.simple_extensions.insert(element_name.clone(), base);
                    }
                }
                if matches!(current_name, "include" | "redefine" | "override")
                    && let Some(location) = attribute(&element, "schemaLocation")
                {
                    schema.includes.push(location);
                }
                if current_name == "import"
                    && let Some(location) = attribute(&element, "schemaLocation")
                {
                    schema
                        .imports
                        .push((attribute(&element, "namespace"), location));
                }
                if current_name == "enumeration"
                    && let Some(simple_type) = simple_type_stack
                        .iter()
                        .rev()
                        .find_map(|name| name.as_ref())
                    && let Some(value) = attribute(&element, "value")
                {
                    schema
                        .enumerations
                        .entry(simple_type.clone())
                        .or_default()
                        .push(value);
                }
                if current_name == "pattern"
                    && let Some(simple_type) = simple_type_stack
                        .iter()
                        .rev()
                        .find_map(|name| name.as_ref())
                    && let Some(pattern) = attribute(&element, "value")
                {
                    schema
                        .restrictions
                        .entry(simple_type.clone())
                        .or_default()
                        .pattern = Some(pattern);
                }
                if matches!(current_name, "length" | "minLength" | "maxLength")
                    && let Some(simple_type) = simple_type_stack
                        .iter()
                        .rev()
                        .find_map(|name| name.as_ref())
                    && let Some(value) = parse_optional_usize(attribute(&element, "value"))?
                {
                    let restriction = schema.restrictions.entry(simple_type.clone()).or_default();
                    match current_name {
                        "length" => restriction.length = Some(value),
                        "minLength" => restriction.min_length = Some(value),
                        _ => restriction.max_length = Some(value),
                    }
                }
                if let Some(facet) = digit_facet(current_name)
                    && let Some(simple_type) = simple_type_stack
                        .iter()
                        .rev()
                        .find_map(|name| name.as_ref())
                    && let Some(value) = parse_optional_usize(attribute(&element, "value"))?
                {
                    set_digit_facet(
                        schema.restrictions.entry(simple_type.clone()).or_default(),
                        facet,
                        value,
                    );
                }
                if let Some(facet) = numeric_facet(current_name)
                    && let Some(simple_type) = simple_type_stack
                        .iter()
                        .rev()
                        .find_map(|name| name.as_ref())
                    && let Some(value) = attribute(&element, "value")
                {
                    set_numeric_facet(
                        schema.restrictions.entry(simple_type.clone()).or_default(),
                        facet,
                        value,
                    );
                }
                if let Some(simple_type) = simple_type_stack
                    .iter()
                    .rev()
                    .find_map(|name| name.as_ref())
                {
                    if current_name == "list"
                        && let Some(item_type) = attribute(&element, "itemType")
                    {
                        schema.lists.insert(simple_type.clone(), item_type);
                    }
                    if current_name == "union"
                        && let Some(member_types) = attribute(&element, "memberTypes")
                    {
                        schema.unions.insert(
                            simple_type.clone(),
                            member_types.split_whitespace().map(str::to_owned).collect(),
                        );
                    }
                }
                if current_name == "attribute"
                    && let Some(parent) = model_stack.last()
                    && let Some(name) = attribute(&element, "name").or_else(|| {
                        attribute(&element, "ref").map(|reference| {
                            reference
                                .rsplit(':')
                                .next()
                                .unwrap_or(&reference)
                                .to_owned()
                        })
                    })
                {
                    schema
                        .attributes
                        .entry(parent.clone())
                        .or_default()
                        .push(name.clone());
                    if attribute(&element, "use").as_deref() == Some("required") {
                        schema
                            .required_attributes
                            .entry(parent.clone())
                            .or_default()
                            .push(name.clone());
                    }
                    let key = format!("{parent}:{name}");
                    if let Some(value) = attribute(&element, "default") {
                        schema.attribute_defaults.insert(key.clone(), value);
                    }
                    if let Some(value) = attribute(&element, "fixed") {
                        schema.attribute_fixed.insert(key, value);
                    }
                }
                if current_name == "element"
                    && let Some(name) = attribute(&element, "name").or_else(|| {
                        attribute(&element, "ref").map(|reference| {
                            reference
                                .rsplit(':')
                                .next()
                                .unwrap_or(&reference)
                                .to_owned()
                        })
                    })
                {
                    if let Some(parent) = model_stack.last() {
                        if sequence_depth > 0
                            && listed_children.insert((parent.clone(), name.clone()))
                        {
                            schema
                                .children
                                .entry(parent.clone())
                                .or_default()
                                .push(name.clone());
                        }
                        if choice_depth > 0 {
                            schema
                                .choices
                                .entry(parent.clone())
                                .or_default()
                                .push(name.clone());
                        }
                        if all_depth > 0 {
                            schema
                                .alls
                                .entry(parent.clone())
                                .or_default()
                                .push(name.clone());
                        }
                    }
                    if let Some(head) = attribute(&element, "substitutionGroup") {
                        schema
                            .substitution_groups
                            .entry(head)
                            .or_default()
                            .push(name.clone());
                    }
                    schema.elements.push(XsdElement {
                        name,
                        occurs: XsdOccurs {
                            min: match attribute(&element, "minOccurs") {
                                None => 1,
                                Some(value) => parse_count(&value)
                                    .ok_or_else(|| "invalid minOccurs".to_owned())?,
                            },
                            max: parse_max_occurs(attribute(&element, "maxOccurs"))?,
                        },
                        type_name: attribute(&element, "type"),
                        form: attribute(&element, "form"),
                        default: attribute(&element, "default"),
                        fixed: attribute(&element, "fixed"),
                        nillable: attribute(&element, "nillable").as_deref() == Some("true"),
                    });
                }
            }
            Ok(Event::End(element)) => {
                depth = depth.saturating_sub(1);
                simple_type_stack.pop();
                let element_name = element.name();
                let current_name = local_name(element_name.as_ref());
                if current_name == "sequence" {
                    sequence_depth = sequence_depth.saturating_sub(1);
                }
                if current_name == "choice" {
                    choice_depth = choice_depth.saturating_sub(1);
                }
                if current_name == "all" {
                    all_depth = all_depth.saturating_sub(1);
                }
                if current_name == "complexContent" {
                    complex_content_depth = complex_content_depth.saturating_sub(1);
                }
                if current_name == "simpleContent" {
                    simple_content_depth = simple_content_depth.saturating_sub(1);
                }
                if element_stack.pop().flatten().is_some() {
                    model_stack.pop();
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("XSD error: {error}")),
        }
    }

    apply_attribute_group_references(source, &mut schema)?;
    apply_model_group_references(source, &mut schema)?;
    apply_content_extensions(&mut schema);
    let model = model::parse_xsd_model(source);
    // A schema of types, groups or attributes only (included by others) has
    // no element; a document that is not an `xs:schema` is no schema.
    if schema.elements.is_empty()
        && let Err(error) = &model
    {
        return Err(format!("the XSD schema contains no xs:element ({error})"));
    }
    schema.models = model.ok().map(Arc::new).into_iter().collect();
    schema.problems = schema
        .models
        .iter()
        .flat_map(|model| model.problems.iter().cloned())
        .chain(
            schema_check::check_schema_document(source)
                .into_iter()
                .map(|problem| problem.message),
        )
        .collect();

    Ok(schema)
}

/// Kind of a schema location to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaLocationKind {
    /// Location of an `xsi:schemaLocation` pair.
    SchemaLocation,
    /// `xsi:noNamespaceSchemaLocation`.
    NoNamespaceSchemaLocation,
    /// `xs:include/@schemaLocation`.
    Include,
    /// `xs:import` (with or without `schemaLocation`).
    Import,
}

/// Schema location as written in the document, before resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaLocation<'a> {
    pub kind: SchemaLocationKind,
    /// Associated namespace (`xsi:schemaLocation`, `xs:import`).
    pub namespace: Option<&'a str>,
    /// Location value (`None`: `xs:import` without `schemaLocation`).
    pub location: Option<&'a str>,
    /// Directory of the document holding the reference.
    pub base_directory: &'a Path,
}

/// Location resolver (XML catalog…) consulted before the default
/// resolution; `None` lets the default resolution apply.
pub type LocationResolver<'r> = dyn Fn(&SchemaLocation<'_>) -> Option<PathBuf> + 'r;

/// Resolves `request` with `resolver`, then by default with
/// [`resolve_location`]. `None` for an `xs:import` without `schemaLocation`
/// unknown to the resolver.
pub fn resolve_schema_location(
    request: &SchemaLocation<'_>,
    resolver: &LocationResolver<'_>,
) -> Option<PathBuf> {
    resolver(request).or_else(|| {
        request
            .location
            .map(|location| resolve_location(request.base_directory, location))
    })
}

/// URI scheme of `value` (at least two characters, so that a Windows drive
/// letter `C:` is not mistaken for a scheme).
fn uri_scheme(value: &str) -> Option<&str> {
    let (scheme, _) = value.split_once(':')?;
    let mut chars = scheme.chars();
    (scheme.len() >= 2
        && chars.next()?.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

/// Default resolution of a schema location: `file:` URI, absolute path or
/// path relative to `base_directory` (percent-decoded if the raw path does
/// not exist). A remote URL is kept as is: it does not designate any
/// readable file (see [`is_remote_location`]).
pub fn resolve_location(base_directory: &Path, location: &str) -> PathBuf {
    let location = location.trim();
    if let Some(scheme) = uri_scheme(location) {
        if scheme.eq_ignore_ascii_case("file") {
            return file_uri_to_path(location);
        }
        return PathBuf::from(location);
    }
    let raw = resolve_path(base_directory, location);
    if !location.contains('%') || xml_core::resource::is_network_path(&raw) || raw.exists() {
        return raw;
    }
    resolve_path(base_directory, &percent_decode(location))
}

/// The path is actually a non-local URL (`http:`, `https:`, `urn:`…) kept
/// by [`resolve_location`], or a network share (`\\server\share`,
/// `file://server/share`): neither is ever read.
pub fn is_remote_location(path: &Path) -> bool {
    path.to_str()
        .and_then(uri_scheme)
        .is_some_and(|scheme| !scheme.eq_ignore_ascii_case("file"))
        || xml_core::resource::is_network_path(path)
}

/// Local path of a `file:` URI (`file:///a%20b.xsd` -> `/a b.xsd`); query
/// and fragment ignored. A URI naming another host
/// (`file://server/share/a.xsd`) gives the network path
/// `//server/share/a.xsd`, which [`is_remote_location`] recognizes.
pub fn file_uri_to_path(uri: &str) -> PathBuf {
    let raw = uri.get(5..).filter(|_| {
        uri.get(..5)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
    });
    let raw = raw.unwrap_or(uri);
    let raw = match raw.strip_prefix("//") {
        Some(rest) => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            let drive = authority.len() == 2
                && authority.as_bytes()[0].is_ascii_alphabetic()
                && authority.as_bytes()[1] == b':';
            if authority.is_empty() || authority.eq_ignore_ascii_case("localhost") {
                &rest[authority.len()..]
            } else if drive {
                // `file://C:/a.xsd` (sloppy form of `file:///C:/a.xsd`).
                rest
            } else {
                // Another host: keep the `//` of a network path.
                raw
            }
        }
        None => raw,
    };
    let raw = raw.split(['?', '#']).next().unwrap_or(raw);
    let raw = if cfg!(windows) && raw.starts_with('/') && raw.as_bytes().get(2) == Some(&b':') {
        &raw[1..]
    } else {
        raw
    };
    PathBuf::from(percent_decode(raw))
}

/// Decodes `%XX` sequences (invalid sequences are kept).
pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (
                (bytes[index + 1] as char).to_digit(16),
                (bytes[index + 2] as char).to_digit(16),
            )
        {
            decoded.push((high * 16 + low) as u8);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Resolves the XSD references of an XML document relative to its path.
pub fn resolve_schema_locations(
    source: &str,
    document_path: impl AsRef<Path>,
) -> Result<Vec<SchemaReference>, String> {
    resolve_schema_locations_with(source, document_path, &|_| None)
}

/// Like [`resolve_schema_locations`], consulting `resolver` (XML catalog)
/// first for each location.
pub fn resolve_schema_locations_with(
    source: &str,
    document_path: impl AsRef<Path>,
    resolver: &LocationResolver<'_>,
) -> Result<Vec<SchemaReference>, String> {
    let mut reader = Reader::from_str(source);
    let mut references = Vec::new();
    let base_directory = document_path
        .as_ref()
        .parent()
        .unwrap_or_else(|| Path::new(""));

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                if let Some(value) = attribute(&element, "schemaLocation") {
                    let values = value.split_whitespace().collect::<Vec<_>>();
                    if values.len() % 2 != 0 {
                        return Err(
                            "xsi:schemaLocation must contain namespace/path pairs".to_owned()
                        );
                    }
                    for [namespace, location] in values.as_chunks::<2>().0 {
                        let request = SchemaLocation {
                            kind: SchemaLocationKind::SchemaLocation,
                            namespace: Some(*namespace),
                            location: Some(*location),
                            base_directory,
                        };
                        if let Some(path) = resolve_schema_location(&request, resolver) {
                            references.push(SchemaReference {
                                namespace: Some((*namespace).to_owned()),
                                path,
                                kind: SchemaLocationKind::SchemaLocation,
                            });
                        }
                    }
                }
                if let Some(value) = attribute(&element, "noNamespaceSchemaLocation") {
                    let request = SchemaLocation {
                        kind: SchemaLocationKind::NoNamespaceSchemaLocation,
                        namespace: None,
                        location: Some(value.trim()),
                        base_directory,
                    };
                    if let Some(path) = resolve_schema_location(&request, resolver) {
                        references.push(SchemaReference {
                            namespace: None,
                            path,
                            kind: SchemaLocationKind::NoNamespaceSchemaLocation,
                        });
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("XML error: {error}")),
        }
    }

    Ok(references)
}

/// Resolves the `xs:include` and `xs:import` dependencies of an XSD schema.
pub fn resolve_schema_dependencies(
    source: &str,
    schema_path: impl AsRef<Path>,
) -> Result<Vec<SchemaReference>, String> {
    resolve_schema_dependencies_with(source, schema_path, &|_| None)
}

/// Like [`resolve_schema_dependencies`], consulting `resolver` first; an
/// `xs:import` without `schemaLocation` is only kept if `resolver` resolves
/// it (XML catalog by namespace).
pub fn resolve_schema_dependencies_with(
    source: &str,
    schema_path: impl AsRef<Path>,
    resolver: &LocationResolver<'_>,
) -> Result<Vec<SchemaReference>, String> {
    let mut reader = Reader::from_str(source);
    let base_directory = schema_path
        .as_ref()
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let mut references = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                let element_name = element.name();
                let kind = match local_name(element_name.as_ref()) {
                    "include" | "redefine" | "override" => SchemaLocationKind::Include,
                    "import" => SchemaLocationKind::Import,
                    _ => continue,
                };
                let location = attribute(&element, "schemaLocation");
                let namespace = match kind {
                    SchemaLocationKind::Import => attribute(&element, "namespace"),
                    _ => None,
                };
                if kind == SchemaLocationKind::Include && location.is_none() {
                    continue;
                }
                let request = SchemaLocation {
                    kind,
                    namespace: namespace.as_deref(),
                    location: location.as_deref().map(str::trim),
                    base_directory,
                };
                if let Some(path) = resolve_schema_location(&request, resolver) {
                    references.push(SchemaReference {
                        namespace,
                        path,
                        kind,
                    });
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("XSD error: {error}")),
        }
    }
    Ok(references)
}

/// Target namespace of a schema document (`None` when it has none or is not
/// readable as a schema).
pub fn schema_target_namespace(source: &str) -> Option<Option<String>> {
    model::parse_xsd_model(source)
        .ok()
        .map(|model| model.target_namespace)
}

/// Namespace rules of the `xs:include`, `xs:redefine`, `xs:override` and
/// `xs:import` elements of a schema document (XML Schema 1.0 Part 1 §4.2.1,
/// §4.2.3): an included schema has no target namespace or the one of the
/// including schema; an import names the target namespace of the imported
/// schema, which is not the one of the importing schema, and is only without
/// `namespace` when the importing schema has a target namespace; a schema
/// does not redefine itself. `target_namespace_of` gives the target
/// namespace of a dependency (`None` when it is not loaded).
pub fn dependency_problems(
    source: &str,
    schema_path: &Path,
    resolver: &LocationResolver<'_>,
    target_namespace_of: &dyn Fn(&Path) -> Option<Option<String>>,
) -> Vec<String> {
    let mut reader = Reader::from_str(source);
    let base_directory = schema_path.parent().unwrap_or_else(|| Path::new(""));
    let mut including: Option<Option<String>> = None;
    let mut problems = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                let name = element.name();
                let local = local_name(name.as_ref());
                if local == "schema" && including.is_none() {
                    including =
                        Some(attribute(&element, "targetNamespace").filter(|v| !v.is_empty()));
                    continue;
                }
                let including_namespace = including.clone().flatten();
                let location = attribute(&element, "schemaLocation");
                match local {
                    "include" | "redefine" | "override" => {
                        let Some(location) = location else { continue };
                        let request = SchemaLocation {
                            kind: SchemaLocationKind::Include,
                            namespace: None,
                            location: Some(location.trim()),
                            base_directory,
                        };
                        let Some(path) = resolve_schema_location(&request, resolver) else {
                            continue;
                        };
                        if local == "redefine" && same_file(&path, schema_path) {
                            problems.push("a schema cannot redefine itself".to_owned());
                        }
                        if let Some(Some(included)) = target_namespace_of(&path)
                            && including_namespace.as_deref() != Some(included.as_str())
                        {
                            problems.push(format!(
                                "xs:{local} of '{}': its target namespace '{included}' is not the target namespace of the including schema",
                                location.trim()
                            ));
                        }
                    }
                    "import" => {
                        let namespace = attribute(&element, "namespace");
                        if let Some(namespace) = &namespace
                            && Some(namespace.as_str()) == including_namespace.as_deref()
                        {
                            problems.push(format!(
                                "xs:import cannot import the target namespace '{namespace}' of its own schema"
                            ));
                        }
                        if namespace.is_none() && including_namespace.is_none() {
                            problems.push(
                                "an xs:import without namespace needs a targetNamespace in the importing schema"
                                    .to_owned(),
                            );
                        }
                        let Some(location) = location else { continue };
                        let request = SchemaLocation {
                            kind: SchemaLocationKind::Import,
                            namespace: namespace.as_deref(),
                            location: Some(location.trim()),
                            base_directory,
                        };
                        let Some(path) = resolve_schema_location(&request, resolver) else {
                            continue;
                        };
                        if let Some(imported) = target_namespace_of(&path)
                            && imported != namespace
                        {
                            problems.push(format!(
                                "xs:import of '{}': the imported schema has {} but the import names {}",
                                location.trim(),
                                imported.as_deref().map_or_else(
                                    || "no target namespace".to_owned(),
                                    |namespace| format!("the target namespace '{namespace}'")
                                ),
                                namespace.as_deref().map_or_else(
                                    || "none".to_owned(),
                                    |namespace| format!("'{namespace}'")
                                )
                            ));
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    problems
}

fn same_file(left: &Path, right: &Path) -> bool {
    left == right
        || (!is_remote_location(left) && !is_remote_location(right))
            && left
                .canonicalize()
                .ok()
                .zip(right.canonicalize().ok())
                .is_some_and(|(left, right)| left == right)
}

fn apply_content_extensions(schema: &mut XsdSchema) {
    let complex_extensions = schema.complex_extensions.clone();
    for (element_name, base) in complex_extensions {
        if let Some(base_attributes) = schema.attributes.get(&base).cloned() {
            let attributes = schema.attributes.entry(element_name).or_default();
            for attribute in base_attributes {
                if !attributes.contains(&attribute) {
                    attributes.push(attribute);
                }
            }
        }
    }
    for element in &mut schema.elements {
        if element.type_name.is_none()
            && let Some(base) = schema.simple_extensions.get(&element.name)
        {
            element.type_name = Some(base.clone());
        }
    }
}

fn apply_model_group_references(source: &str, schema: &mut XsdSchema) -> Result<(), String> {
    let mut reader = Reader::from_str(source);
    let mut current_group: Option<String> = None;
    let mut group_depth = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if name == "group" {
                    if let Some(group) = attribute(&element, "name") {
                        current_group = Some(group.clone());
                        schema.model_groups.entry(group).or_default();
                    }
                } else if current_group.is_some() && name == "sequence" {
                    group_depth += 1;
                } else if let Some(group) = &current_group
                    && group_depth > 0
                    && name == "element"
                    && let Some(element_name) = attribute(&element, "name")
                {
                    schema
                        .model_groups
                        .entry(group.clone())
                        .or_default()
                        .push(element_name);
                }
            }
            Ok(Event::Empty(element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if name == "group"
                    && let Some(group) = attribute(&element, "name")
                {
                    schema.model_groups.entry(group).or_default();
                } else if let Some(group) = &current_group
                    && group_depth > 0
                    && name == "element"
                    && let Some(element_name) = attribute(&element, "name")
                {
                    schema
                        .model_groups
                        .entry(group.clone())
                        .or_default()
                        .push(element_name);
                }
            }
            Ok(Event::End(element)) => {
                let qname = element.name();
                match local_name(qname.as_ref()) {
                    "sequence" if current_group.is_some() => {
                        group_depth = group_depth.saturating_sub(1)
                    }
                    "group" => {
                        current_group = None;
                        group_depth = 0;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("XSD error: {error}")),
        }
    }

    let mut reader = Reader::from_str(source);
    let mut elements = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if name == "element"
                    && let Some(element_name) = attribute(&element, "name")
                {
                    elements.push(element_name);
                }
            }
            Ok(Event::Empty(element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if name == "group"
                    && let Some(group) = attribute(&element, "ref")
                    && let Some(parent) = elements.last()
                    && let Some(children) = schema.model_groups.get(&group)
                {
                    let target = schema.children.entry(parent.clone()).or_default();
                    for child in children {
                        if !target.contains(child) {
                            target.push(child.clone());
                        }
                    }
                }
                if name == "element"
                    && let Some(element_name) = attribute(&element, "name")
                {
                    elements.push(element_name);
                    elements.pop();
                }
            }
            Ok(Event::End(element)) => {
                if local_name(element.name().as_ref()) == "element" {
                    elements.pop();
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("XSD error: {error}")),
        }
    }
    Ok(())
}

fn apply_attribute_group_references(source: &str, schema: &mut XsdSchema) -> Result<(), String> {
    let mut reader = Reader::from_str(source);
    let mut current_group: Option<String> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if name == "attributeGroup" {
                    current_group = attribute(&element, "name");
                    if let Some(group) = &current_group {
                        schema.attribute_groups.entry(group.clone()).or_default();
                    }
                } else if name == "attribute"
                    && let Some(group) = &current_group
                    && let Some(attribute_name) = attribute(&element, "name")
                {
                    schema
                        .attribute_groups
                        .entry(group.clone())
                        .or_default()
                        .push(attribute_name);
                }
            }
            Ok(Event::Empty(element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if name == "attributeGroup"
                    && let Some(group) = attribute(&element, "name")
                {
                    schema.attribute_groups.entry(group).or_default();
                } else if name == "attribute"
                    && let Some(group) = &current_group
                    && let Some(attribute_name) = attribute(&element, "name")
                {
                    schema
                        .attribute_groups
                        .entry(group.clone())
                        .or_default()
                        .push(attribute_name);
                }
            }
            Ok(Event::End(element)) => {
                if local_name(element.name().as_ref()) == "attributeGroup" {
                    current_group = None;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("XSD error: {error}")),
        }
    }

    let mut reader = Reader::from_str(source);
    let mut elements = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if name == "element"
                    && let Some(element_name) = attribute(&element, "name")
                {
                    elements.push(element_name);
                }
            }
            Ok(Event::Empty(element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if name == "element"
                    && let Some(element_name) = attribute(&element, "name")
                {
                    elements.push(element_name);
                }
                if name == "attributeGroup"
                    && let Some(group) = attribute(&element, "ref")
                    && let Some(element_name) = elements.last()
                    && let Some(attributes) = schema.attribute_groups.get(&group)
                {
                    let target = schema.attributes.entry(element_name.clone()).or_default();
                    for attribute_name in attributes {
                        if !target.contains(attribute_name) {
                            target.push(attribute_name.clone());
                        }
                    }
                }
            }
            Ok(Event::End(element)) => {
                if local_name(element.name().as_ref()) == "element" {
                    elements.pop();
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("XSD error: {error}")),
        }
    }
    Ok(())
}

/// Resolves a schema path (`schemaLocation`) relative to `base_directory`
/// and normalizes the `.` and `..` components.
pub fn resolve_path(base_directory: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_owned()
    } else {
        normalize_path(&base_directory.join(path))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// Returns the XSD elements suited to the current XML context.
pub fn complete_attribute_values(
    source: &str,
    offset: usize,
    schema: &XsdSchema,
) -> Vec<XsdCompletion> {
    let prefix = &source[..offset.min(source.len())];
    let Some(opening) = prefix.rfind('<') else {
        return Vec::new();
    };
    let fragment = &prefix[opening + 1..];
    let Some(equals) = fragment.rfind('=') else {
        return Vec::new();
    };
    let attribute_name = fragment[..equals]
        .split_whitespace()
        .last()
        .unwrap_or_default();
    let typed = fragment[equals + 1..].trim_matches([' ', '\"', '\'']);
    let mut open_elements = open_xml_elements(&prefix[..opening]);
    if let Some(current_element) = fragment.split_whitespace().next()
        && !current_element.is_empty()
    {
        open_elements.push(current_element.to_owned());
    }
    let Some(parent) = open_elements.last() else {
        return Vec::new();
    };
    let Some(element) = schema
        .elements
        .iter()
        .find(|element| element.name == *parent)
    else {
        return Vec::new();
    };
    let type_name = element
        .type_name
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("__anonymous:{parent}"));
    if attribute_name.is_empty() {
        return Vec::new();
    }
    schema
        .enumerations
        .get(&type_name)
        .into_iter()
        .flatten()
        .filter(|value| value.starts_with(typed))
        .map(|value| XsdCompletion {
            label: value.clone(),
            insert_text: value.clone(),
        })
        .collect()
}

/// Returns the XSD attributes suited to the current open element.
pub fn complete_attributes(source: &str, offset: usize, schema: &XsdSchema) -> Vec<XsdCompletion> {
    let prefix = &source[..offset.min(source.len())];
    let Some(opening) = prefix.rfind('<') else {
        return Vec::new();
    };
    if prefix[opening..].contains('>') {
        return Vec::new();
    }
    let fragment = &prefix[opening + 1..];
    if fragment.starts_with('/') {
        return Vec::new();
    }
    let mut tokens = fragment.split_whitespace();
    let Some(element_name) = tokens.next() else {
        return Vec::new();
    };
    let typed = if fragment
        .chars()
        .last()
        .is_some_and(|character| character.is_whitespace())
    {
        ""
    } else {
        tokens.last().unwrap_or("")
    };
    schema
        .attributes
        .get(element_name)
        .into_iter()
        .flatten()
        .filter(|name| name.starts_with(typed))
        .map(|name| XsdCompletion {
            label: name.clone(),
            insert_text: name.clone(),
        })
        .collect()
}

pub fn complete_elements(source: &str, offset: usize, schema: &XsdSchema) -> Vec<XsdCompletion> {
    let prefix = &source[..offset.min(source.len())];
    let Some(opening) = prefix.rfind('<') else {
        return Vec::new();
    };
    if prefix[opening..].contains('>') {
        return Vec::new();
    }
    let fragment = &prefix[opening + 1..];
    if fragment.starts_with('/') {
        return Vec::new();
    }
    let typed = fragment.trim();
    let stack = open_xml_elements(prefix);
    let names = stack
        .last()
        .map(|parent| {
            let mut names = schema.children.get(parent).cloned().unwrap_or_default();
            let existing = direct_children(&prefix[..opening]);
            if !names.is_empty() {
                names.retain(|name| {
                    let count = existing.iter().filter(|child| *child == name).count();
                    let max = schema
                        .elements
                        .iter()
                        .find(|element| element.name == *name)
                        .and_then(|element| element.occurs.max);
                    max.is_none_or(|maximum| count < maximum)
                });
                if let Some(last) = existing.last()
                    && let Some(index) = schema
                        .children
                        .get(parent)
                        .and_then(|children| children.iter().position(|child| child == last))
                {
                    names.retain(|name| {
                        schema
                            .children
                            .get(parent)
                            .and_then(|children| children.iter().position(|child| child == name))
                            .is_some_and(|candidate| candidate >= index)
                    });
                }
            }
            names.extend(schema.choices.get(parent).cloned().unwrap_or_default());
            names.extend(schema.alls.get(parent).cloned().unwrap_or_default());
            let heads = names.clone();
            for head in heads {
                names.extend(
                    schema
                        .substitution_groups
                        .get(&head)
                        .cloned()
                        .unwrap_or_default(),
                );
            }
            names.sort();
            names.dedup();
            names
        })
        .filter(|names| !names.is_empty())
        .unwrap_or_else(|| {
            schema
                .elements
                .iter()
                .map(|element| element.name.clone())
                .collect()
        });

    names
        .into_iter()
        .filter(|name| name.starts_with(typed))
        .map(|name| XsdCompletion {
            label: name.clone(),
            insert_text: name,
        })
        .collect()
}

fn direct_children(source: &str) -> Vec<String> {
    let mut reader = Reader::from_str(source);
    let mut stack = Vec::new();
    let mut children = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                if !stack.is_empty() && stack.len() == 1 {
                    children.push(name.clone());
                }
                stack.push(name);
            }
            Ok(Event::Empty(element)) => {
                if !stack.is_empty() && stack.len() == 1 {
                    children.push(String::from_utf8_lossy(element.name().as_ref()).into_owned());
                }
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    children
}

fn open_xml_elements(source: &str) -> Vec<String> {
    let mut reader = Reader::from_str(source);
    let mut stack = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                stack.push(String::from_utf8_lossy(element.name().as_ref()).into_owned())
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    stack
}

/// Returns the name of the first XML element encountered.
pub fn root_element_name(source: &str) -> Option<String> {
    let mut reader = Reader::from_str(source);
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                return Some(String::from_utf8_lossy(element.name().as_ref()).into_owned());
            }
            Ok(Event::Eof) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

/// Namespace of the `xsi:` attributes.
const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";

struct XmlFrame<'s> {
    name: String,
    children: Vec<String>,
    text: String,
    /// The text contains an entity reference that cannot be resolved.
    unresolved_text: bool,
    /// `<name` range of the start tag.
    location: Range<usize>,
    /// Namespace declarations of the start tag (`""` for the default).
    namespaces: Vec<(String, String)>,
    /// Declaration resolved for the element (`skipped` ones included), the
    /// starting point of its children.
    resolution: Option<model::ResolvedElement<'s>>,
    value: ValueCheck<'s>,
    /// Index of the element in the identity tree.
    node: usize,
    /// How the child elements are checked.
    content: ContentCheck,
    /// Range of the text content (first to last text, CDATA section or
    /// reference).
    text_range: Option<Range<usize>>,
}

/// How the child elements of an element are checked.
enum ContentCheck {
    /// Against the content model of its type.
    Run(content::ContentRun),
    /// No content model to check (undeclared element, `xs:anyType`,
    /// wildcard, simple content...).
    Unchecked,
    /// The schema has no component model: name based checks of the flat
    /// declarations.
    Flat,
}

/// Content models by type definition, built once per validation.
type ContentModels = HashMap<*const model::XsdTypeDef, Option<Rc<content::ContentModel>>>;

/// How the text content of an element is checked.
enum ValueCheck<'s> {
    /// Declaration resolved through the component model: value type (for
    /// simple content), `default` and `fixed`, whether `xsi:nil` is set.
    Model {
        value_type: Option<SimpleType<'s>>,
        /// Complex type with element-only (or empty) content: no text.
        element_only: bool,
        default: Option<&'s str>,
        fixed: Option<&'s str>,
        nil: bool,
    },
    /// Only the flat declaration is known: lexical `fixed` check.
    Flat,
}

/// Checks the XML document and associates each diagnostic with the start tag
/// of the relevant element (the faulty element, or the parent for content
/// model rules) or with the faulty attribute value.
///
/// Text contents and attribute values are checked against their simple
/// types ([`datatypes`]) when the component model resolves their
/// declarations.
pub fn validate_document_located(source: &str, schema: &XsdSchema) -> Vec<LocatedXsdDiagnostic> {
    validate(source, schema).0
}

/// Links of the `xs:IDREF(S)` and `xs:keyref` values of the document to the
/// `xs:ID` and key values they designate.
pub fn identity_links(source: &str, schema: &XsdSchema) -> Vec<identity::IdentityLink> {
    validate(source, schema).1
}

fn validate(
    source: &str,
    schema: &XsdSchema,
) -> (Vec<LocatedXsdDiagnostic>, Vec<identity::IdentityLink>) {
    let models = XsdModelSet::new(schema.models.clone());
    let mut content_models = ContentModels::new();
    let has_constraints = models
        .models()
        .iter()
        .any(|model| !model.identity_constraints.is_empty());
    let mut reader = Reader::from_str(source);
    let mut stack: Vec<XmlFrame<'_>> = Vec::new();
    let mut bindings = Bindings::default();
    let mut nodes: Vec<identity::InstanceNode<'_>> = Vec::new();
    let mut diagnostics = Vec::new();
    let mut root_checked = false;
    let located = |diagnostics: Vec<XsdDiagnostic>, location: &Range<usize>| {
        diagnostics
            .into_iter()
            .map(|diagnostic| LocatedXsdDiagnostic {
                kind: diagnostic.kind,
                message: diagnostic.message,
                offset: location.start,
                end: location.end,
            })
            .collect::<Vec<_>>()
    };

    loop {
        let event_start = reader.buffer_position() as usize;
        let event = reader.read_event();
        let (element, empty) = match event {
            Ok(Event::Start(element)) => (element, false),
            Ok(Event::Empty(element)) => (element, true),
            Ok(Event::End(_)) => {
                if let Some(frame) = stack.pop() {
                    diagnostics.extend(located(
                        validate_content_end(&frame, schema),
                        &frame.location,
                    ));
                    diagnostics.extend(located(
                        validate_text_content(&frame, &bindings, schema),
                        &frame.location,
                    ));
                    close_node(&mut nodes, &frame, &bindings, has_constraints);
                    bindings.unbind(&frame.namespaces);
                }
                continue;
            }
            Ok(Event::Text(text)) => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push_str(&String::from_utf8_lossy(text.as_ref()));
                    extend_text_range(frame, borrowed_range(source, text.as_ref()));
                }
                continue;
            }
            Ok(Event::CData(data)) => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push_str(&String::from_utf8_lossy(data.as_ref()));
                    extend_text_range(frame, borrowed_range(source, data.as_ref()));
                }
                continue;
            }
            Ok(Event::GeneralRef(reference)) => {
                if let Some(frame) = stack.last_mut() {
                    match resolve_reference(&reference) {
                        Some(character) => frame.text.push(character),
                        None => frame.unresolved_text = true,
                    }
                    // `&name;` around the borrowed name.
                    extend_text_range(
                        frame,
                        borrowed_range(source, reference.as_ref())
                            .map(|range| range.start.saturating_sub(1)..range.end + 1),
                    );
                }
                continue;
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => continue,
        };
        let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
        let start = source
            .get(event_start..)
            .and_then(|rest| rest.find('<'))
            .map_or(event_start, |offset| event_start + offset);
        let location = start..(start + 1 + element.name().as_ref().len()).min(source.len());
        let namespaces = namespace_declarations(&element);
        bindings.bind(&namespaces);
        let lookup = |prefix: &str| bindings.lookup(prefix);
        let step = instance_step(&name, &element, &lookup);
        if !root_checked {
            root_checked = true;
            diagnostics.extend(located(
                validate_root_element(&name, &namespaces, &models, schema, step.xsi_type.as_ref()),
                &location,
            ));
        }
        // Each open element keeps its own resolution: resolving a child from
        // its parent's is constant time, the whole path would be quadratic.
        let chain = match stack.last() {
            None => models.resolve_root_element(&step),
            Some(parent) => parent
                .resolution
                .as_ref()
                .and_then(|parent| models.resolve_child_element(parent, &step)),
        };
        let resolved = chain.filter(|resolved| !resolved.skipped);
        let (attribute_diagnostics, attribute_types, instance_attributes) =
            validate_attribute_values(
                source,
                &location,
                &models,
                resolved.as_ref(),
                &element,
                &lookup,
            );
        diagnostics.extend(attribute_diagnostics);
        let same_value = |attribute: &str, value: &str, fixed: &str| {
            attribute_types
                .iter()
                .find(|(name, _)| name == attribute)
                .map_or(value == fixed, |(_, value_type)| {
                    value_type.values_equal(value, fixed, Some(&lookup))
                })
        };
        let mut element_diagnostics = if models.models().is_empty() {
            let mut diagnostics =
                validate_attributes(schema, &name, &element, &same_value, &lookup);
            diagnostics.extend(validate_nil(schema, &name, &element));
            diagnostics
        } else {
            let mut diagnostics = instance_check::validate_attributes(
                &models,
                resolved.as_ref(),
                &name,
                &element,
                &same_value,
                &lookup,
            );
            diagnostics.extend(instance_check::validate_nil(
                resolved.as_ref(),
                &name,
                &element,
                &lookup,
            ));
            diagnostics.extend(instance_check::validate_xsi_type(
                &models,
                resolved.as_ref(),
                &name,
                step.xsi_type.as_ref(),
            ));
            diagnostics
        };
        let value = match &resolved {
            Some(resolved) => ValueCheck::Model {
                value_type: builtin_xsi_type(&step).or_else(|| {
                    resolved
                        .element_type
                        .and_then(|reference| models.simple_type(reference))
                }),
                element_only: resolved
                    .element_type
                    .is_some_and(|reference| is_element_only(&models, reference)),
                default: resolved.declaration.item.default.as_deref(),
                fixed: resolved.declaration.item.fixed.as_deref(),
                nil: is_nil(&element),
            },
            // No declaration: an `xsi:type` naming a simple type still
            // decides the value.
            None => match undeclared_xsi_simple_type(&models, &step) {
                Some(value_type) => ValueCheck::Model {
                    value_type: Some(value_type),
                    element_only: false,
                    default: None,
                    fixed: None,
                    nil: is_nil(&element),
                },
                None => ValueCheck::Flat,
            },
        };
        if let Some(parent) = stack.last_mut() {
            match &mut parent.content {
                ContentCheck::Run(run) => {
                    if let Err(error) = run.step(step.namespace.as_deref(), &step.local) {
                        element_diagnostics.push(unexpected_child(&error, &name, &parent.name));
                    } else if resolved.is_none()
                        && !parent
                            .resolution
                            .as_ref()
                            .is_some_and(|parent| parent.skipped)
                        && let Some(parent_type) = parent
                            .resolution
                            .as_ref()
                            .and_then(|parent| parent.element_type)
                        && models.strict_wildcard_applies(parent_type, step.namespace.as_deref())
                        && !step.xsi_type.as_ref().is_some_and(|(namespace, local)| {
                            models.global_type(namespace.as_deref(), local).is_some()
                                || namespace.as_deref() == Some(model::XSD_NAMESPACE)
                        })
                    {
                        // The wildcard is `processContents="strict"`: the
                        // element must be declared.
                        element_diagnostics.push(XsdDiagnostic {
                            kind: XsdDiagnosticKind::UnexpectedElement,
                            message: format!(
                                "element <{name}> matches a strict wildcard of <{}> but has no declaration",
                                parent.name
                            ),
                        });
                    }
                }
                ContentCheck::Unchecked => {}
                ContentCheck::Flat => {
                    if !is_allowed_child(schema, &parent.name, &name) {
                        element_diagnostics.push(XsdDiagnostic {
                            kind: XsdDiagnosticKind::UnexpectedElement,
                            message: format!("element <{name}> not allowed in <{}>", parent.name),
                        });
                    }
                }
            }
            parent.children.push(name.clone());
        }
        diagnostics.extend(located(element_diagnostics, &location));
        let node = nodes.len();
        if let Some(parent) = stack.last() {
            nodes[parent.node].children.push(node);
        }
        nodes.push(identity::InstanceNode {
            children: Vec::new(),
            last: node,
            namespace: step.namespace.clone(),
            local: step.local.clone(),
            name: name.clone(),
            location: location.clone(),
            constraints: resolved.as_ref().map_or(&[], |resolved| {
                resolved.declaration.item.identity_constraints.as_slice()
            }),
            attributes: instance_attributes,
            value: None,
            nil: matches!(value, ValueCheck::Model { nil: true, .. }),
        });
        let content = if models.models().is_empty() {
            ContentCheck::Flat
        } else {
            resolved
                .as_ref()
                .filter(|_| !is_nil(&element))
                .and_then(|resolved| resolved.element_type)
                .and_then(|reference| {
                    let definition = reference.definition?;
                    content_models
                        .entry(std::ptr::from_ref(definition))
                        .or_insert_with(|| models.content_model(reference).map(Rc::new))
                        .clone()
                })
                .map_or(ContentCheck::Unchecked, |model| {
                    ContentCheck::Run(content::ContentRun::new(model))
                })
        };
        let frame = XmlFrame {
            name,
            children: Vec::new(),
            text: String::new(),
            unresolved_text: false,
            location,
            namespaces,
            resolution: chain,
            value,
            node,
            content,
            text_range: None,
        };
        if empty {
            diagnostics.extend(located(
                validate_content_end(&frame, schema),
                &frame.location,
            ));
            diagnostics.extend(located(
                validate_text_content(&frame, &bindings, schema),
                &frame.location,
            ));
            close_node(&mut nodes, &frame, &bindings, has_constraints);
            bindings.unbind(&frame.namespaces);
        } else {
            stack.push(frame);
        }
    }

    if !root_checked {
        diagnostics.push(LocatedXsdDiagnostic {
            kind: XsdDiagnosticKind::MissingRoot,
            message: "XML document without a root element".to_owned(),
            offset: 0,
            end: 0,
        });
    }
    // Elements left open by a malformed document.
    let count = nodes.len();
    for frame in &stack {
        nodes[frame.node].last = count.saturating_sub(1).max(frame.node);
    }
    let (identity_diagnostics, links) = identity::check(source, &nodes);
    diagnostics.extend(identity_diagnostics);
    (diagnostics, links)
}

fn extend_text_range(frame: &mut XmlFrame<'_>, range: Option<Range<usize>>) {
    if let Some(range) = range {
        frame.text_range = Some(match frame.text_range.take() {
            Some(current) => current.start.min(range.start)..current.end.max(range.end),
            None => range,
        });
    }
}

/// Records the value of a closed element in the identity tree (when
/// identity constraints or IDs need it) and the extent of its subtree.
fn close_node(
    nodes: &mut [identity::InstanceNode<'_>],
    frame: &XmlFrame<'_>,
    bindings: &Bindings,
    has_constraints: bool,
) {
    let last = nodes.len().saturating_sub(1).max(frame.node);
    let needed = has_constraints
        || matches!(
            &frame.value,
            ValueCheck::Model { value_type: Some(value_type), .. } if value_type.id_kind().is_some()
        );
    let value = if needed {
        element_value(frame, bindings)
    } else {
        None
    };
    if let Some(node) = nodes.get_mut(frame.node) {
        node.last = last;
        node.value = value;
    }
}

/// Simple value of a closed element (`None` for element content, nil or
/// unknown entities).
fn element_value(frame: &XmlFrame<'_>, bindings: &Bindings) -> Option<identity::InstanceValue> {
    if frame.unresolved_text || !frame.children.is_empty() {
        return None;
    }
    let range = frame
        .text_range
        .clone()
        .unwrap_or_else(|| frame.location.clone());
    match &frame.value {
        ValueCheck::Model {
            value_type,
            default,
            fixed,
            nil,
            ..
        } => {
            if *nil {
                return None;
            }
            let text = match (frame.text.is_empty(), default.or(*fixed)) {
                (true, Some(value)) => value,
                _ => frame.text.as_str(),
            };
            let lookup = |prefix: &str| bindings.lookup(prefix);
            // Complex content has no simple value (§3.11.4, fields).
            let value_type = value_type.as_ref()?;
            Some(instance_value(value_type, text, range, &lookup).0)
        }
        ValueCheck::Flat => Some(untyped_value(&frame.text, range)),
    }
}

fn untyped_value(text: &str, range: Range<usize>) -> identity::InstanceValue {
    identity::InstanceValue {
        value: datatypes::Value::String(text.to_owned()),
        text: text.to_owned(),
        range,
        valid: true,
        id: None,
    }
}

/// Value of `raw` for its simple type, and the validation error.
fn instance_value(
    value_type: &SimpleType<'_>,
    raw: &str,
    range: Range<usize>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> (identity::InstanceValue, Option<datatypes::ValueError>) {
    let text = value_type.white_space().apply(raw).into_owned();
    let id = value_type.id_kind();
    match value_type.validate(raw, Some(lookup)) {
        Ok(value) => (
            identity::InstanceValue {
                value,
                text,
                range,
                valid: true,
                id,
            },
            None,
        ),
        Err(error) => (
            identity::InstanceValue {
                value: datatypes::Value::String(text.clone()),
                text,
                range,
                valid: false,
                id,
            },
            Some(error),
        ),
    }
}

/// Checks the XML document against the elements declared by the schema.
pub fn validate_document(source: &str, schema: &XsdSchema) -> Vec<XsdDiagnostic> {
    validate_document_located(source, schema)
        .into_iter()
        .map(|diagnostic| XsdDiagnostic {
            kind: diagnostic.kind,
            message: diagnostic.message,
        })
        .collect()
}

/// Checks the root element: against the global declarations of the model
/// (expanded names), or of the flat schema when no model is available.
fn validate_root_element(
    name: &str,
    namespaces: &[(String, String)],
    models: &XsdModelSet,
    schema: &XsdSchema,
    xsi_type: Option<&(Option<String>, String)>,
) -> Vec<XsdDiagnostic> {
    if models.models().is_empty() {
        return validate_root(name, schema);
    }
    let (prefix, local) = name.split_once(':').unwrap_or(("", name));
    let namespace = namespaces
        .iter()
        .rev()
        .find(|(declared, _)| declared == prefix)
        .map(|(_, namespace)| namespace.clone())
        .filter(|namespace| !namespace.is_empty());
    // `global_element` also finds the elements of chameleon schemas, which
    // take the namespace of a schema in the set that includes them.
    let declared = models
        .global_element(namespace.as_deref(), local)
        .is_some_and(|found| {
            found.item.namespace == namespace
                || models
                    .models()
                    .iter()
                    .any(|model| model.target_namespace == namespace)
        });
    // Without declaration, an `xsi:type` naming a known type is enough (the
    // element is then assessed by that type).
    let typed = xsi_type.is_some_and(|(namespace, local)| {
        models.global_type(namespace.as_deref(), local).is_some()
            || namespace.as_deref() == Some(model::XSD_NAMESPACE)
    });
    if declared || typed {
        Vec::new()
    } else {
        vec![XsdDiagnostic {
            kind: XsdDiagnosticKind::UnknownRoot,
            message: format!("root element <{name}> not declared in the XSD schema"),
        }]
    }
}

/// Whitespace characters of XML.
const XML_WHITESPACE: [char; 4] = [' ', '\t', '\n', '\r'];

/// Whether a type is complex with element-only or empty content (neither
/// simple content nor mixed, including along its derivation).
fn is_element_only(models: &XsdModelSet, reference: model::XsdTypeRef<'_>) -> bool {
    let mut current = Some(reference);
    let mut first = true;
    for _ in 0..model::MAX_DEPTH {
        let Some(reference) = current else {
            return true;
        };
        let Some(definition) = reference.definition else {
            // Derived from `xs:anyType` without `mixed`: element-only.
            // Other built-in or unresolved bases are not judged.
            return !first
                && reference
                    .name
                    .is_some_and(|name| name.is_builtin() && name.local == "anyType");
        };
        if !definition.complex || definition.simple_content || definition.mixed {
            return false;
        }
        first = false;
        current = models.base_type(reference);
    }
    false
}

/// Character of a predefined entity or character reference.
fn resolve_reference(reference: &quick_xml::events::BytesRef<'_>) -> Option<char> {
    if let Ok(Some(character)) = reference.resolve_char_ref() {
        return Some(character);
    }
    match reference.as_ref() {
        b"lt" => Some('<'),
        b"gt" => Some('>'),
        b"amp" => Some('&'),
        b"apos" => Some('\''),
        b"quot" => Some('"'),
        _ => None,
    }
}

/// `xmlns` and `xmlns:prefix` declarations of a start tag.
fn namespace_declarations(element: &quick_xml::events::BytesStart<'_>) -> Vec<(String, String)> {
    element
        .attributes()
        .flatten()
        .filter_map(|attribute| {
            let key = std::str::from_utf8(attribute.key.as_ref()).ok()?;
            let prefix = if key == "xmlns" {
                ""
            } else {
                key.strip_prefix("xmlns:")?
            };
            let value = attribute.unescape_value().ok()?.into_owned();
            Some((prefix.to_owned(), value))
        })
        .collect()
}

/// Namespace bindings in scope while a document is walked: the declarations
/// of every open element (and of the start tag being read), innermost last,
/// so a lookup does not depend on the nesting depth.
#[derive(Default)]
struct Bindings(HashMap<String, Vec<String>>);

impl Bindings {
    fn bind(&mut self, declarations: &[(String, String)]) {
        for (prefix, namespace) in declarations {
            self.0
                .entry(prefix.clone())
                .or_default()
                .push(namespace.clone());
        }
    }

    fn unbind(&mut self, declarations: &[(String, String)]) {
        for (prefix, _) in declarations.iter().rev() {
            if let Some(namespaces) = self.0.get_mut(prefix) {
                namespaces.pop();
                if namespaces.is_empty() {
                    self.0.remove(prefix);
                }
            }
        }
    }

    /// Namespace bound to `prefix` (`""` for the default namespace).
    fn lookup(&self, prefix: &str) -> Option<String> {
        if prefix == "xml" {
            return Some(model::XML_NAMESPACE.to_owned());
        }
        self.0
            .get(prefix)
            .and_then(|namespaces| namespaces.last())
            .filter(|namespace| !namespace.is_empty())
            .cloned()
    }
}

/// Step of an instance element: expanded name and `xsi:type`.
fn instance_step(
    name: &str,
    element: &quick_xml::events::BytesStart<'_>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> XsdInstanceStep {
    let (prefix, local) = name.split_once(':').unwrap_or(("", name));
    let xsi_type = element
        .attributes()
        .flatten()
        .find(|attribute| {
            std::str::from_utf8(attribute.key.as_ref())
                .ok()
                .and_then(|key| key.split_once(':'))
                .is_some_and(|(prefix, local)| {
                    local == "type" && lookup(prefix).as_deref() == Some(XSI_NAMESPACE)
                })
        })
        .and_then(|attribute| attribute.unescape_value().ok())
        .map(|value| {
            let value = value.trim();
            let (prefix, local) = value.split_once(':').unwrap_or(("", value));
            (lookup(prefix), local.to_owned())
        });
    XsdInstanceStep {
        namespace: lookup(prefix),
        local: local.to_owned(),
        xsi_type,
    }
}

/// Simple type named by the `xsi:type` of an element without declaration.
fn undeclared_xsi_simple_type<'s>(
    models: &'s XsdModelSet,
    step: &XsdInstanceStep,
) -> Option<SimpleType<'s>> {
    let (namespace, local) = step.xsi_type.as_ref()?;
    if let Some(builtin) = builtin_xsi_type(step) {
        return Some(builtin);
    }
    let found = models.global_type(namespace.as_deref(), local)?;
    if found.item.complex {
        return None;
    }
    models.simple_type(model::XsdTypeRef {
        schema: found.schema,
        name: None,
        definition: Some(found.item),
    })
}

/// `xsi:type` naming a built-in simple type (`xsi:type="xs:int"`).
fn builtin_xsi_type<'s>(step: &XsdInstanceStep) -> Option<SimpleType<'s>> {
    let (namespace, local) = step.xsi_type.as_ref()?;
    (namespace.as_deref() == Some(model::XSD_NAMESPACE))
        .then(|| BuiltinType::from_local_name(local))
        .flatten()
        .map(SimpleType::builtin)
}

fn is_nil(element: &quick_xml::events::BytesStart<'_>) -> bool {
    element
        .attributes()
        .flatten()
        .find(|attribute| local_name(attribute.key.as_ref()) == "nil")
        .and_then(|attribute| attribute.unescape_value().ok())
        .is_some_and(|value| matches!(value.trim(), "true" | "1"))
}

/// Checks the attribute values whose declarations the model resolves, and
/// returns the simple types found (by local name) for the `fixed` checks and
/// the attributes of the element for the identity tree.
#[allow(clippy::type_complexity)]
fn validate_attribute_values<'s>(
    source: &str,
    location: &Range<usize>,
    models: &'s XsdModelSet,
    resolved: Option<&model::ResolvedElement<'s>>,
    element: &quick_xml::events::BytesStart<'_>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> (
    Vec<LocatedXsdDiagnostic>,
    Vec<(String, SimpleType<'s>)>,
    Vec<identity::InstanceAttribute>,
) {
    let mut diagnostics = Vec::new();
    let mut types = Vec::new();
    let mut attributes = Vec::new();
    let element_name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
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
        let Some(value) = model::normalized_attribute_value(&attribute) else {
            continue;
        };
        let range =
            borrowed_range(source, attribute.value.as_ref()).unwrap_or_else(|| location.clone());
        // An attribute matched only by a `processContents="skip"` wildcard
        // is not assessed, even when a global declaration exists.
        let skipped = resolved
            .and_then(|resolved| resolved.element_type)
            .is_some_and(|element_type| {
                !models.attribute_uses(element_type).iter().any(|usage| {
                    usage.item.usage != model::XsdUse::Prohibited
                        && usage.item.name == local
                        && usage.item.namespace == namespace
                }) && models
                    .attribute_wildcards(element_type)
                    .iter()
                    .find(|wildcard| wildcard.namespaces.allows(namespace.as_deref()))
                    .is_some_and(|wildcard| {
                        wildcard.process_contents == model::XsdProcessContents::Skip
                    })
            });
        let value_type = resolved.filter(|_| !skipped).and_then(|resolved| {
            let declaration = models
                .resolve_attribute(Some(resolved), namespace.as_deref(), local)
                .map(|found| found.declaration)
                .or_else(|| {
                    // An unqualified attribute matched by `xs:anyAttribute`
                    // is validated against a global attribute without
                    // namespace.
                    let wildcard = resolved
                        .element_type
                        .and_then(|reference| reference.definition)
                        .is_some_and(|definition| definition.any_attribute.is_some());
                    (wildcard && namespace.is_none())
                        .then(|| models.global_attribute(None, local))
                        .flatten()
                        .filter(|found| found.item.namespace.is_none())
                });
            declaration
                .and_then(|declaration| models.attribute_type(declaration))
                .and_then(|reference| models.simple_type(reference))
        });
        let instance = match &value_type {
            Some(value_type) => {
                let (instance, error) = instance_value(value_type, &value, range.clone(), lookup);
                if let Some(error) = error {
                    diagnostics.push(LocatedXsdDiagnostic {
                        kind: if error.enumeration {
                            XsdDiagnosticKind::InvalidEnumeration
                        } else {
                            XsdDiagnosticKind::InvalidAttributeValue
                        },
                        message: format!("attribute @{key} of <{element_name}>: {}", error.message),
                        offset: range.start,
                        end: range.end,
                    });
                }
                instance
            }
            None => untyped_value(&value, range),
        };
        attributes.push(identity::InstanceAttribute {
            namespace,
            local: local.to_owned(),
            value: instance,
        });
        if let Some(value_type) = value_type {
            types.push((local.to_owned(), value_type));
        }
    }
    // Absent attributes with a default or fixed value take it.
    if let Some(resolved) = resolved
        && let Some(element_type) = resolved.element_type
    {
        for usage in models.attribute_uses(element_type) {
            let (namespace, local) = (usage.item.namespace.as_deref(), usage.item.name.as_str());
            if attributes.iter().any(|attribute| {
                attribute.local == local && attribute.namespace.as_deref() == namespace
            }) {
                continue;
            }
            let Some(found) = models.resolve_attribute(Some(resolved), namespace, local) else {
                continue;
            };
            if found.usage.item.usage == model::XsdUse::Prohibited {
                continue;
            }
            let Some(value) = [
                &found.usage.item.default,
                &found.usage.item.fixed,
                &found.declaration.item.default,
                &found.declaration.item.fixed,
            ]
            .into_iter()
            .find_map(|value| value.as_deref()) else {
                continue;
            };
            let instance = match models
                .attribute_type(found.declaration)
                .and_then(|reference| models.simple_type(reference))
            {
                Some(value_type) => instance_value(&value_type, value, location.clone(), lookup).0,
                None => untyped_value(value, location.clone()),
            };
            attributes.push(identity::InstanceAttribute {
                namespace: namespace.map(str::to_owned),
                local: local.to_owned(),
                value: instance,
            });
        }
    }
    (diagnostics, types, attributes)
}

/// Range in `source` of a slice borrowed from it.
fn borrowed_range(source: &str, slice: &[u8]) -> Option<Range<usize>> {
    let base = source.as_ptr() as usize;
    let start = (slice.as_ptr() as usize).checked_sub(base)?;
    (start + slice.len() <= source.len()).then(|| start..start + slice.len())
}

/// Diagnostic of a child element rejected by its parent's content model.
fn unexpected_child(error: &content::ContentError, child: &str, parent: &str) -> XsdDiagnostic {
    let content::ContentError::Unexpected {
        expected,
        known,
        repeated,
        wrong_namespace,
    } = error
    else {
        unreachable!("only unexpected children are reported per child");
    };
    let expected = if expected.is_empty() {
        String::new()
    } else {
        format!(
            ", expected {}",
            expected
                .iter()
                .map(|name| format!("<{name}>"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    if let Some(name) = wrong_namespace {
        XsdDiagnostic {
            kind: XsdDiagnosticKind::UnexpectedElement,
            message: format!(
                "element <{child}> not allowed in <{parent}>: it is in the wrong namespace ({name} expected)"
            ),
        }
    } else if !known {
        XsdDiagnostic {
            kind: XsdDiagnosticKind::UnexpectedElement,
            message: format!("element <{child}> not allowed in <{parent}>"),
        }
    } else if *repeated {
        XsdDiagnostic {
            kind: XsdDiagnosticKind::TooManyElements,
            message: format!("too many <{child}> elements in <{parent}>"),
        }
    } else {
        XsdDiagnostic {
            kind: XsdDiagnosticKind::UnexpectedOrder,
            message: format!("unexpected order of <{child}> in <{parent}>{expected}"),
        }
    }
}

/// Content model rules checked when an element ends (or is empty): required
/// children that are missing.
fn validate_content_end(frame: &XmlFrame<'_>, schema: &XsdSchema) -> Vec<XsdDiagnostic> {
    match &frame.content {
        ContentCheck::Flat => validate_sequence_frame(frame, schema),
        ContentCheck::Unchecked => Vec::new(),
        ContentCheck::Run(run) => match run.finish() {
            Ok(()) => Vec::new(),
            Err(content::ContentError::Incomplete { expected }) => vec![XsdDiagnostic {
                kind: XsdDiagnosticKind::MissingElement,
                message: match expected.as_slice() {
                    [only] => format!("element <{only}> required in <{}>", frame.name),
                    _ => format!(
                        "one of {} required in <{}>",
                        expected
                            .iter()
                            .map(|name| format!("<{name}>"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        frame.name
                    ),
                },
            }],
            Err(_) => Vec::new(),
        },
    }
}

fn validate_sequence_frame(frame: &XmlFrame<'_>, schema: &XsdSchema) -> Vec<XsdDiagnostic> {
    let Some(expected) = schema
        .children
        .get(&frame.name)
        .or_else(|| schema.alls.get(&frame.name))
    else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    let mut previous_index = 0;
    for child in &frame.children {
        if let Some(index) = expected.iter().position(|name| name == child) {
            if !schema.alls.contains_key(&frame.name) && index < previous_index {
                diagnostics.push(XsdDiagnostic {
                    kind: XsdDiagnosticKind::UnexpectedOrder,
                    message: format!("unexpected order of <{child}> in <{}>", frame.name),
                });
            }
            previous_index = index;
        }
    }
    for child in expected {
        let count = frame
            .children
            .iter()
            .filter(|name| {
                *name == child
                    || schema
                        .substitution_groups
                        .get(child)
                        .is_some_and(|members| members.iter().any(|member| member == *name))
            })
            .count();
        if let Some(element) = schema
            .elements
            .iter()
            .find(|element| element.name == *child)
        {
            if count < element.occurs.min {
                diagnostics.push(XsdDiagnostic {
                    kind: XsdDiagnosticKind::MissingElement,
                    message: format!("element <{child}> required in <{}>", frame.name),
                });
            }
            if let Some(max) = element.occurs.max
                && count > max
            {
                diagnostics.push(XsdDiagnostic {
                    kind: XsdDiagnosticKind::TooManyElements,
                    message: format!("too many <{child}> elements in <{}>", frame.name),
                });
            }
        }
    }
    diagnostics
}

/// Checks the text content of a closed element: value against its simple
/// type, `fixed` value. `ancestors` are the frames still open (for the
/// namespace prefixes of `QName` values).
fn validate_text_content(
    frame: &XmlFrame<'_>,
    bindings: &Bindings,
    schema: &XsdSchema,
) -> Vec<XsdDiagnostic> {
    let name = &frame.name;
    let (value_type, default, fixed, nil) = match &frame.value {
        ValueCheck::Model {
            value_type,
            element_only,
            default,
            fixed,
            nil,
        } => {
            if *element_only && !frame.text.trim_matches(XML_WHITESPACE).is_empty() {
                return vec![XsdDiagnostic {
                    kind: XsdDiagnosticKind::InvalidContent,
                    message: format!(
                        "element <{name}> cannot contain text (its type has element-only content)"
                    ),
                }];
            }
            (value_type.as_ref(), *default, *fixed, *nil)
        }
        ValueCheck::Flat => {
            let fixed = schema
                .elements
                .iter()
                .find(|element| element.name == *name)
                .and_then(|element| element.fixed.as_deref());
            return match fixed {
                Some(fixed) if frame.text.trim() != fixed => vec![XsdDiagnostic {
                    kind: XsdDiagnosticKind::FixedValue,
                    message: format!("content of <{name}> differs from the fixed value"),
                }],
                _ => Vec::new(),
            };
        }
    };
    if nil {
        if fixed.is_some() {
            return vec![XsdDiagnostic {
                kind: XsdDiagnosticKind::FixedValue,
                message: format!(
                    "element <{name}> has xsi:nil=\"true\" but a fixed value constraint"
                ),
            }];
        }
        // An element with `xsi:nil="true"` has no content (whitespace
        // tolerated, as editors reformat documents).
        return if frame.children.is_empty() && frame.text.trim_matches(XML_WHITESPACE).is_empty() {
            Vec::new()
        } else {
            vec![XsdDiagnostic {
                kind: XsdDiagnosticKind::InvalidContent,
                message: format!("element <{name}> has xsi:nil=\"true\" and must be empty"),
            }]
        };
    }
    if frame.unresolved_text {
        return Vec::new();
    }
    let lookup = |prefix: &str| bindings.lookup(prefix);
    let Some(value_type) = value_type else {
        // Complex content: only the `fixed` value of a text-only element.
        return match fixed {
            Some(fixed)
                if !frame.children.is_empty()
                    || (!frame.text.is_empty() && frame.text != fixed) =>
            {
                vec![XsdDiagnostic {
                    kind: XsdDiagnosticKind::FixedValue,
                    message: format!("content of <{name}> differs from the fixed value '{fixed}'"),
                }]
            }
            _ => Vec::new(),
        };
    };
    if !frame.children.is_empty() {
        return vec![XsdDiagnostic {
            kind: XsdDiagnosticKind::InvalidContent,
            message: format!(
                "element <{name}> has the simple type {} and cannot contain elements",
                value_type.name
            ),
        }];
    }
    // An empty element takes its default or fixed value.
    let text = match (frame.text.is_empty(), default.or(fixed)) {
        (true, Some(value)) => value,
        _ => frame.text.as_str(),
    };
    let mut diagnostics = Vec::new();
    if let Err(error) = value_type.validate(text, Some(&lookup)) {
        diagnostics.push(XsdDiagnostic {
            kind: if error.enumeration {
                XsdDiagnosticKind::InvalidEnumeration
            } else {
                XsdDiagnosticKind::InvalidContent
            },
            message: format!("content of <{name}>: {}", error.message),
        });
    } else if let Some(fixed) = fixed
        && !value_type.values_equal(text, fixed, Some(&lookup))
    {
        diagnostics.push(XsdDiagnostic {
            kind: XsdDiagnosticKind::FixedValue,
            message: format!("content of <{name}> differs from the fixed value '{fixed}'"),
        });
    }
    diagnostics
}

fn digit_facet(name: &str) -> Option<&str> {
    match name {
        "totalDigits" | "fractionDigits" => Some(name),
        _ => None,
    }
}

fn set_digit_facet(restriction: &mut XsdRestriction, facet: &str, value: usize) {
    match facet {
        "totalDigits" => restriction.total_digits = Some(value),
        "fractionDigits" => restriction.fraction_digits = Some(value),
        _ => {}
    }
}

fn numeric_facet(name: &str) -> Option<&str> {
    matches!(
        name,
        "minInclusive" | "maxInclusive" | "minExclusive" | "maxExclusive" | "whiteSpace"
    )
    .then_some(name)
}

fn set_numeric_facet(restriction: &mut XsdRestriction, facet: &str, value: String) {
    match facet {
        "minInclusive" => restriction.min_inclusive = Some(value),
        "maxInclusive" => restriction.max_inclusive = Some(value),
        "minExclusive" => restriction.min_exclusive = Some(value),
        "maxExclusive" => restriction.max_exclusive = Some(value),
        "whiteSpace" => restriction.white_space = Some(value),
        _ => {}
    }
}

fn validate_nil(
    schema: &XsdSchema,
    element_name: &str,
    element: &quick_xml::events::BytesStart<'_>,
) -> Vec<XsdDiagnostic> {
    let is_nil = element
        .attributes()
        .flatten()
        .find(|attribute| local_name(attribute.key.as_ref()) == "nil")
        .and_then(|attribute| attribute.unescape_value().ok())
        .is_some_and(|value| matches!(value.as_ref(), "true" | "1"));
    if !is_nil {
        return Vec::new();
    }
    if schema
        .elements
        .iter()
        .find(|element| element.name == element_name)
        .is_some_and(|element| element.nillable)
    {
        Vec::new()
    } else {
        vec![XsdDiagnostic {
            kind: XsdDiagnosticKind::NotNillable,
            message: format!("element <{element_name}> is not nillable but has xsi:nil"),
        }]
    }
}

/// Declared, required and `fixed` attributes of the flat model;
/// `same_value(local name, value, fixed)` compares a value with its fixed
/// value, `lookup` resolves prefixes (`xsi:` attributes are always
/// allowed).
fn validate_attributes(
    schema: &XsdSchema,
    element_name: &str,
    element: &quick_xml::events::BytesStart<'_>,
    same_value: &dyn Fn(&str, &str, &str) -> bool,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<XsdDiagnostic> {
    if schema
        .any_attributes
        .get(element_name)
        .copied()
        .unwrap_or(false)
    {
        return Vec::new();
    }
    let Some(allowed) = schema.attributes.get(element_name) else {
        return Vec::new();
    };
    let mut present = Vec::new();
    let mut diagnostics = Vec::new();
    for attribute in element.attributes().flatten() {
        let raw_name = String::from_utf8_lossy(attribute.key.as_ref());
        let name = local_name(attribute.key.as_ref()).to_owned();
        if raw_name == "xmlns"
            || raw_name.starts_with("xmlns:")
            || raw_name
                .split_once(':')
                .and_then(|(prefix, _)| lookup(prefix))
                .as_deref()
                == Some(XSI_NAMESPACE)
        {
            continue;
        }
        present.push(name.clone());
        if !allowed.iter().any(|item| item == &name) {
            diagnostics.push(XsdDiagnostic {
                kind: XsdDiagnosticKind::UnexpectedAttribute,
                message: format!("attribute @{name} not allowed on <{element_name}>"),
            });
        }
        if let Some(fixed) = schema
            .attribute_fixed
            .get(&format!("{element_name}:{name}"))
            && model::normalized_attribute_value(&attribute)
                .is_some_and(|value| !same_value(&name, &value, fixed))
        {
            diagnostics.push(XsdDiagnostic {
                kind: XsdDiagnosticKind::FixedValue,
                message: format!("attribute @{name} differs from the fixed value"),
            });
        }
    }
    if let Some(required) = schema.required_attributes.get(element_name) {
        for name in required {
            if !present.iter().any(|item| item == name) {
                diagnostics.push(XsdDiagnostic {
                    kind: XsdDiagnosticKind::MissingAttribute,
                    message: format!("attribute @{name} required on <{element_name}>"),
                });
            }
        }
    }
    diagnostics
}

fn child_allowed_by_substitution(schema: &XsdSchema, allowed: &[String], child: &str) -> bool {
    allowed.iter().any(|name| {
        name == child
            || schema
                .substitution_groups
                .get(name)
                .is_some_and(|members| members.iter().any(|member| member == child))
    })
}

fn is_allowed_child(schema: &XsdSchema, parent: &str, child: &str) -> bool {
    let sequence = schema.children.get(parent);
    let choice = schema.choices.get(parent);
    let all = schema.alls.get(parent);
    if schema.any_children.get(parent).copied().unwrap_or(false) {
        return true;
    }
    if sequence.is_none() && choice.is_none() && all.is_none() {
        return true;
    }
    sequence.is_some_and(|children| child_allowed_by_substitution(schema, children, child))
        || choice.is_some_and(|children| child_allowed_by_substitution(schema, children, child))
        || all.is_some_and(|children| child_allowed_by_substitution(schema, children, child))
}

/// Checks that the name of the XML root is declared by the schema.
pub fn validate_root(root_name: &str, schema: &XsdSchema) -> Vec<XsdDiagnostic> {
    if schema
        .elements
        .iter()
        .any(|element| element.name == root_name)
    {
        Vec::new()
    } else {
        vec![XsdDiagnostic {
            kind: XsdDiagnosticKind::UnknownRoot,
            message: format!("root element <{root_name}> not declared in the XSD schema"),
        }]
    }
}

fn local_name(name: &[u8]) -> &str {
    let name = std::str::from_utf8(name).unwrap_or_default();
    name.rsplit(':').next().unwrap_or(name)
}

fn attribute(element: &quick_xml::events::BytesStart<'_>, wanted: &str) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|attribute| local_name(attribute.key.as_ref()) == wanted)
        .and_then(|attribute| attribute.unescape_value().ok())
        .map(|value| value.into_owned())
}

fn parse_optional_usize(value: Option<String>) -> Result<Option<usize>, String> {
    value
        .map(|value| {
            value
                .parse()
                .map_err(|_| "invalid XSD numeric value".to_owned())
        })
        .transpose()
}

/// Value of an occurrence bound: a non-negative integer of any size (larger
/// than `usize` counts as `usize::MAX`).
pub(crate) fn parse_count(text: &str) -> Option<usize> {
    let digits = text.trim().strip_prefix('+').unwrap_or(text.trim());
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(digits.parse().unwrap_or(usize::MAX))
}

fn parse_max_occurs(value: Option<String>) -> Result<Option<usize>, String> {
    match value.as_deref() {
        None => Ok(Some(1)),
        Some("unbounded") => Ok(None),
        Some(value) => parse_count(value)
            .map(Some)
            .ok_or_else(|| "invalid maxOccurs".to_owned()),
    }
}

#[cfg(test)]
mod tests;
