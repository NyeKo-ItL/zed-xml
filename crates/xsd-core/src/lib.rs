//! XSD model and parsing shared by the LSP server.

pub mod datatypes;
pub mod model;
pub mod pattern;

use std::{
    collections::HashMap,
    ops::Range,
    path::{Component, Path, PathBuf},
    str,
    sync::Arc,
};

use quick_xml::{Reader, events::Event};

use crate::{
    datatypes::{BuiltinType, SimpleType},
    model::{XsdInstanceStep, XsdModel, XsdModelSet},
};

const MAX_XSD_SOURCE_BYTES: usize = 16 * 1024 * 1024;

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

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
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
                if current_name == "include"
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
                    if sequence_depth > 0 {
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
                            min: attribute(&element, "minOccurs")
                                .as_deref()
                                .unwrap_or("1")
                                .parse()
                                .map_err(|_| "invalid minOccurs".to_owned())?,
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
                if current_name == "include"
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
                        if sequence_depth > 0 {
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
                            min: attribute(&element, "minOccurs")
                                .as_deref()
                                .unwrap_or("1")
                                .parse()
                                .map_err(|_| "invalid minOccurs".to_owned())?,
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
    if schema.elements.is_empty() {
        return Err("the XSD schema contains no xs:element".to_owned());
    }
    schema.models = model::parse_xsd_model(source)
        .ok()
        .map(Arc::new)
        .into_iter()
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
    if !location.contains('%') || raw.exists() {
        return raw;
    }
    resolve_path(base_directory, &percent_decode(location))
}

/// The path is actually a non-local URL (`http:`, `https:`, `urn:`…) kept
/// by [`resolve_location`].
pub fn is_remote_location(path: &Path) -> bool {
    path.to_str()
        .and_then(uri_scheme)
        .is_some_and(|scheme| !scheme.eq_ignore_ascii_case("file"))
}

/// Local path of a `file:` URI (`file:///a%20b.xsd` -> `/a b.xsd`); query
/// and fragment ignored.
pub fn file_uri_to_path(uri: &str) -> PathBuf {
    let raw = uri.get(5..).filter(|_| {
        uri.get(..5)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
    });
    let raw = raw.unwrap_or(uri);
    let raw = raw.strip_prefix("//localhost/").map_or_else(
        || raw.strip_prefix("//").unwrap_or(raw),
        |rest| &raw[raw.len() - rest.len() - 1..],
    );
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
                    "include" => SchemaLocationKind::Include,
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
                    references.push(SchemaReference { namespace, path });
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("XSD error: {error}")),
        }
    }
    Ok(references)
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
                } else if current_group.is_some()
                    && group_depth > 0
                    && name == "element"
                    && let Some(element_name) = attribute(&element, "name")
                {
                    schema
                        .model_groups
                        .entry(current_group.clone().unwrap())
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
                } else if current_group.is_some()
                    && group_depth > 0
                    && name == "element"
                    && let Some(element_name) = attribute(&element, "name")
                {
                    schema
                        .model_groups
                        .entry(current_group.clone().unwrap())
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
    step: XsdInstanceStep,
    value: ValueCheck<'s>,
}

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
    let models = XsdModelSet::new(schema.models.clone());
    let mut reader = Reader::from_str(source);
    let mut stack: Vec<XmlFrame<'_>> = Vec::new();
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
                        validate_sequence_frame(&frame, schema),
                        &frame.location,
                    ));
                    diagnostics.extend(located(
                        validate_text_content(&frame, &stack, schema),
                        &frame.location,
                    ));
                }
                continue;
            }
            Ok(Event::Text(text)) => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push_str(&String::from_utf8_lossy(text.as_ref()));
                }
                continue;
            }
            Ok(Event::CData(data)) => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push_str(&String::from_utf8_lossy(data.as_ref()));
                }
                continue;
            }
            Ok(Event::GeneralRef(reference)) => {
                if let Some(frame) = stack.last_mut() {
                    match resolve_reference(&reference) {
                        Some(character) => frame.text.push(character),
                        None => frame.unresolved_text = true,
                    }
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
        if !root_checked {
            root_checked = true;
            diagnostics.extend(located(
                validate_root_element(&name, &namespaces, &models, schema),
                &location,
            ));
        }
        let lookup = |prefix: &str| lookup_prefix(&stack, &namespaces, prefix);
        let step = instance_step(&name, &element, &lookup);
        let mut path = stack
            .iter()
            .map(|frame| frame.step.clone())
            .collect::<Vec<_>>();
        path.push(step.clone());
        let resolved = models
            .resolve_element_path(&path)
            .filter(|resolved| !resolved.skipped);
        let (attribute_diagnostics, attribute_types) = validate_attribute_values(
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
        let mut element_diagnostics =
            validate_attributes(schema, &name, &element, &same_value, &lookup);
        element_diagnostics.extend(validate_nil(schema, &name, &element));
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
            None => ValueCheck::Flat,
        };
        if let Some(parent) = stack.last_mut() {
            if !is_allowed_child(schema, &parent.name, &name) {
                element_diagnostics.push(XsdDiagnostic {
                    kind: XsdDiagnosticKind::UnexpectedElement,
                    message: format!("element <{name}> not allowed in <{}>", parent.name),
                });
            }
            parent.children.push(name.clone());
        }
        diagnostics.extend(located(element_diagnostics, &location));
        let frame = XmlFrame {
            name,
            children: Vec::new(),
            text: String::new(),
            unresolved_text: false,
            location,
            namespaces,
            step,
            value,
        };
        if empty {
            // Content model rules are only checked on elements with an end
            // tag; the (empty) value is checked here.
            diagnostics.extend(located(
                validate_text_content(&frame, &stack, schema),
                &frame.location,
            ));
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
    diagnostics
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
) -> Vec<XsdDiagnostic> {
    if models.models().is_empty() {
        return validate_root(name, schema);
    }
    let (prefix, local) = name.split_once(':').unwrap_or(("", name));
    let namespace = lookup_prefix(&[], namespaces, prefix);
    if models.global_elements().any(|declaration| {
        declaration.item.name == local && declaration.item.namespace == namespace
    }) {
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

/// Namespace bound to `prefix` (`""` for the default namespace) by the
/// start tag being read (`own`) or its ancestors.
fn lookup_prefix(stack: &[XmlFrame<'_>], own: &[(String, String)], prefix: &str) -> Option<String> {
    if prefix == "xml" {
        return Some(model::XML_NAMESPACE.to_owned());
    }
    own.iter()
        .chain(stack.iter().rev().flat_map(|frame| frame.namespaces.iter()))
        .find(|(declared, _)| declared == prefix)
        .map(|(_, namespace)| namespace.clone())
        .filter(|namespace| !namespace.is_empty())
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
/// returns the simple types found (by local name) for the `fixed` checks.
fn validate_attribute_values<'s>(
    source: &str,
    location: &Range<usize>,
    models: &'s XsdModelSet,
    resolved: Option<&model::ResolvedElement<'s>>,
    element: &quick_xml::events::BytesStart<'_>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> (Vec<LocatedXsdDiagnostic>, Vec<(String, SimpleType<'s>)>) {
    let mut diagnostics = Vec::new();
    let mut types = Vec::new();
    let Some(resolved) = resolved else {
        return (diagnostics, types);
    };
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
        let declaration = models
            .resolve_attribute(Some(resolved), namespace.as_deref(), local)
            .map(|found| found.declaration)
            .or_else(|| {
                // An unqualified attribute matched by `xs:anyAttribute` is
                // validated against a global attribute without namespace.
                let wildcard = resolved
                    .element_type
                    .and_then(|reference| reference.definition)
                    .is_some_and(|definition| definition.any_attribute);
                (wildcard && namespace.is_none())
                    .then(|| models.global_attribute(None, local))
                    .flatten()
                    .filter(|found| found.item.namespace.is_none())
            });
        let Some(value_type) = declaration
            .and_then(|declaration| models.attribute_type(declaration))
            .and_then(|reference| models.simple_type(reference))
        else {
            continue;
        };
        if let Some(value) = model::normalized_attribute_value(&attribute)
            && let Err(error) = value_type.validate(&value, Some(lookup))
        {
            let range = borrowed_range(source, attribute.value.as_ref())
                .unwrap_or_else(|| location.clone());
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
        types.push((local.to_owned(), value_type));
    }
    (diagnostics, types)
}

/// Range in `source` of a slice borrowed from it.
fn borrowed_range(source: &str, slice: &[u8]) -> Option<Range<usize>> {
    let base = source.as_ptr() as usize;
    let start = (slice.as_ptr() as usize).checked_sub(base)?;
    (start + slice.len() <= source.len()).then(|| start..start + slice.len())
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
    ancestors: &[XmlFrame<'_>],
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
    if nil || frame.unresolved_text {
        return Vec::new();
    }
    let lookup = |prefix: &str| lookup_prefix(ancestors, &frame.namespaces, prefix);
    let Some(value_type) = value_type else {
        // Complex content: only the `fixed` value of a text-only element.
        return match fixed {
            Some(fixed)
                if frame.children.is_empty() && !frame.text.is_empty() && frame.text != fixed =>
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
            && attribute
                .unescape_value()
                .is_ok_and(|value| !same_value(&name, value.as_ref(), fixed))
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

fn parse_max_occurs(value: Option<String>) -> Result<Option<usize>, String> {
    match value.as_deref() {
        None => Ok(Some(1)),
        Some("unbounded") => Ok(None),
        Some(value) => value
            .parse()
            .map(Some)
            .map_err(|_| "invalid maxOccurs".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:test">
            <xs:element name="root"/>
            <xs:element name="item" minOccurs="0" maxOccurs="unbounded"/>
        </xs:schema>
    "#;
    const SEQUENCE: &str = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="child"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>
    "#;
    const CHOICE: &str = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:choice>
                        <xs:element name="text"/>
                        <xs:element name="number"/>
                    </xs:choice>
                </xs:complexType>
            </xs:element>
        </xs:schema>
    "#;
    const ENUMERATION: &str = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:simpleType name="Color">
                <xs:restriction base="xs:string">
                    <xs:enumeration value="red"/>
                    <xs:enumeration value="blue"/>
                </xs:restriction>
            </xs:simpleType>
            <xs:element name="item" type="Color"/>
        </xs:schema>
    "#;

    #[test]
    fn parses_schema_namespace_elements_and_cardinalities() {
        let schema = parse_xsd(SIMPLE).unwrap();

        assert_eq!(schema.target_namespace.as_deref(), Some("urn:test"));
        assert_eq!(schema.elements[0].name, "root");
        assert_eq!(schema.elements[0].occurs, XsdOccurs::default());
        assert_eq!(schema.elements[1].occurs.min, 0);
        assert_eq!(schema.elements[1].occurs.max, None);
    }

    #[test]
    fn validates_declared_and_unknown_roots() {
        let schema = parse_xsd(SIMPLE).unwrap();

        assert!(validate_root("root", &schema).is_empty());
        assert_eq!(validate_root("unknown", &schema).len(), 1);
    }

    #[test]
    fn locates_validation_diagnostics_in_the_xml_source() {
        let schema = parse_xsd(SEQUENCE).unwrap();
        let source = "<root><magazine /></root>";
        let diagnostics = validate_document_located(source, &schema);

        assert_eq!(
            diagnostics[0].message,
            "element <magazine> not allowed in <root>"
        );
        assert_eq!(diagnostics[0].offset, source.find("<magazine").unwrap());
        assert_eq!(diagnostics[0].end, source.find(" />").unwrap());
        assert_eq!(diagnostics[0].kind, XsdDiagnosticKind::UnexpectedElement);
        assert_eq!(diagnostics[0].kind.id(), "unexpectedElement");
    }

    #[test]
    fn locates_diagnostics_on_the_offending_occurrence() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="root"><xs:complexType><xs:sequence>
    <xs:element name="item" maxOccurs="unbounded"><xs:complexType>
      <xs:attribute name="id" use="required"/>
    </xs:complexType></xs:element>
  </xs:sequence></xs:complexType></xs:element>
</xs:schema>"#,
        )
        .unwrap();
        let source = "<root>\n  <item id=\"1\"/>\n  <item/>\n</root>";
        let diagnostics = validate_document_located(source, &schema);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, XsdDiagnosticKind::MissingAttribute);
        assert_eq!(diagnostics[0].offset, source.rfind("<item").unwrap());
        assert_eq!(&source[diagnostics[0].offset..diagnostics[0].end], "<item");

        let unknown = validate_document_located("<?xml version=\"1.0\"?>\n<other/>", &schema);
        assert_eq!(unknown[0].kind, XsdDiagnosticKind::UnknownRoot);
        assert_eq!((unknown[0].offset, unknown[0].end), (22, 28));
        let empty = validate_document_located("", &schema);
        assert_eq!(empty[0].kind, XsdDiagnosticKind::MissingRoot);
    }

    #[test]
    fn validates_children_declared_by_a_sequence() {
        let schema = parse_xsd(SEQUENCE).unwrap();

        assert_eq!(schema.children["root"], vec!["child"]);
        assert!(validate_document("<root><child /></root>", &schema).is_empty());
        assert_eq!(
            validate_document("<root><other /></root>", &schema)[0].message,
            "element <other> not allowed in <root>"
        );
    }

    #[test]
    fn validates_required_attributes() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="item"><xs:complexType><xs:attribute name="id" use="required"/></xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.required_attributes["item"], vec!["id"]);
        assert!(
            validate_document("<item/>", &schema)[0]
                .message
                .contains("@id required")
        );
        assert!(validate_document("<item id=\"1\"/>", &schema).is_empty());
    }

    #[test]
    fn validates_declared_attributes() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="item"><xs:complexType>
                    <xs:attribute name="id"/>
                </xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.attributes["item"], vec!["id"]);
        assert!(validate_document("<item id=\"1\"/>", &schema).is_empty());
        assert!(
            validate_document("<item other=\"1\"/>", &schema)[0]
                .message
                .contains("@other not allowed")
        );
    }

    #[test]
    fn validates_sequence_order_and_cardinality() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:element name="first"/>
                    <xs:element name="second" minOccurs="0" maxOccurs="2"/>
                </xs:sequence></xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        let order = validate_document("<root><second/><first/></root>", &schema);
        assert!(
            order
                .iter()
                .any(|diagnostic| diagnostic.message.contains("unexpected order"))
        );
        assert!(
            validate_document("<root></root>", &schema)[0]
                .message
                .contains("<first> required")
        );
        assert!(
            validate_document("<root><first/><second/><second/><second/></root>", &schema)
                .iter()
                .any(|diagnostic| diagnostic.message.contains("too many <second> elements"))
        );
    }

    #[test]
    fn normalizes_whitespace_restrictions_before_validation() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="Code"><xs:restriction base="xs:string">
                    <xs:whiteSpace value="collapse"/><xs:length value="3"/>
                </xs:restriction></xs:simpleType>
                <xs:element name="code" type="Code"/>
            </xs:schema>"#,
        )
        .unwrap();

        assert!(validate_document("<code> A B </code>", &schema).is_empty());
        assert!(
            validate_document("<code>A B C</code>", &schema)[0]
                .message
                .contains("length")
        );
    }

    #[test]
    fn supports_any_elements_and_attributes() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:any/>
                </xs:sequence><xs:anyAttribute/></xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert!(schema.any_children["root"]);
        assert!(schema.any_attributes["root"]);
        assert!(validate_document("<root><unknown/><other/></root>", &schema).is_empty());
        assert!(validate_document("<root arbitrary=\"1\"/>", &schema).is_empty());
    }

    #[test]
    fn preserves_attribute_defaults_and_validates_fixed_values() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="item"><xs:complexType>
                    <xs:attribute name="kind" default="normal"/>
                    <xs:attribute name="version" fixed="1"/>
                </xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.attribute_defaults["item:kind"], "normal");
        assert_eq!(schema.attribute_fixed["item:version"], "1");
        assert!(
            validate_document("<item version=\"2\"/>", &schema)[0]
                .message
                .contains("fixed")
        );
        assert!(validate_document("<item version=\"1\"/>", &schema).is_empty());
    }

    #[test]
    fn validates_xsi_nil_for_nillable_elements() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="allowed" nillable="true"/>
                <xs:element name="forbidden" nillable="false"/>
            </xs:schema>"#,
        )
        .unwrap();

        assert!(validate_document(
            r#"<allowed xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:nil="true"/>"#,
            &schema
        )
        .is_empty());
        assert!(validate_document(
            r#"<forbidden xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:nil="true"/>"#,
            &schema
        )[0]
            .message
            .contains("not nillable"));
    }

    #[test]
    fn preserves_defaults_and_validates_fixed_values() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="status" default="new"/>
                <xs:element name="version" fixed="1"/>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.elements[0].default.as_deref(), Some("new"));
        assert_eq!(schema.elements[1].fixed.as_deref(), Some("1"));
        assert!(
            validate_document("<version>2</version>", &schema)[0]
                .message
                .contains("fixed")
        );
        assert!(validate_document("<version>1</version>", &schema).is_empty());
    }

    #[test]
    fn parses_complex_and_simple_content_extensions() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="complex"><xs:complexType><xs:complexContent>
                    <xs:extension base="Base"/>
                </xs:complexContent></xs:complexType></xs:element>
                <xs:element name="simple"><xs:complexType><xs:simpleContent>
                    <xs:extension base="xs:string"/>
                </xs:simpleContent></xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.complex_extensions["complex"], "Base");
        assert_eq!(schema.simple_extensions["simple"], "xs:string");
        assert_eq!(schema.elements[1].type_name.as_deref(), Some("xs:string"));
    }

    #[test]
    fn resolves_attribute_references() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attribute name="id"/>
                <xs:element name="item"><xs:complexType>
                    <xs:attribute ref="id"/>
                </xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.attributes["item"], vec!["id"]);
        assert!(validate_document("<item id=\"1\"/>", &schema).is_empty());
    }

    #[test]
    fn resolves_named_model_groups() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:group name="common"><xs:sequence><xs:element name="id"/></xs:sequence></xs:group>
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:group ref="common"/>
                </xs:sequence></xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.model_groups["common"], vec!["id"]);
        assert_eq!(schema.children["root"], vec!["id"]);
        assert!(validate_document("<root><id/></root>", &schema).is_empty());
    }

    #[test]
    fn resolves_named_attribute_groups() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attributeGroup name="common"><xs:attribute name="id"/></xs:attributeGroup>
                <xs:element name="item"><xs:complexType>
                    <xs:attributeGroup ref="common"/>
                </xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.attribute_groups["common"], vec!["id"]);
        assert_eq!(schema.attributes["item"], vec!["id"]);
        assert!(validate_document("<item id=\"1\"/>", &schema).is_empty());
    }

    #[test]
    fn preserves_schema_namespace_form_defaults() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                targetNamespace="urn:test" elementFormDefault="qualified"
                attributeFormDefault="unqualified">
                <xs:element name="root" form="qualified"/>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.target_namespace.as_deref(), Some("urn:test"));
        assert_eq!(schema.element_form_default.as_deref(), Some("qualified"));
        assert_eq!(
            schema.attribute_form_default.as_deref(),
            Some("unqualified")
        );
        assert_eq!(schema.elements[0].form.as_deref(), Some("qualified"));
    }

    #[test]
    fn resolves_element_references_in_model_groups() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="shared"/>
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:element ref="shared"/>
                </xs:sequence></xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.children["root"], vec!["shared"]);
        assert!(validate_document("<root><shared/></root>", &schema).is_empty());
        assert_eq!(complete_elements("<root><s", 9, &schema)[0].label, "shared");
    }

    #[test]
    fn supports_all_children_without_order_constraints() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root"><xs:complexType><xs:all>
                    <xs:element name="first"/><xs:element name="second" minOccurs="0"/>
                </xs:all></xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.alls["root"], vec!["first", "second"]);
        assert!(validate_document("<root><second/><first/></root>", &schema).is_empty());
        assert!(
            validate_document("<root></root>", &schema)[0]
                .message
                .contains("<first> required")
        );
        assert_eq!(complete_elements("<root><", 8, &schema).len(), 2);
    }

    #[test]
    fn supports_choice_children_for_validation_and_completion() {
        let schema = parse_xsd(CHOICE).unwrap();

        assert_eq!(schema.choices["root"], vec!["text", "number"]);
        assert!(validate_document("<root><number /></root>", &schema).is_empty());
        assert_eq!(complete_elements("<root><n", 9, &schema)[0].label, "number");
    }

    #[test]
    fn parses_anonymous_simple_types() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="item"><xs:simpleType><xs:restriction base="xs:string">
                    <xs:enumeration value="one"/>
                </xs:restriction></xs:simpleType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();
        assert_eq!(schema.enumerations["__anonymous:item"], vec!["one"]);
    }

    #[test]
    fn validates_simple_type_length_restrictions() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="Code"><xs:restriction base="xs:string">
                    <xs:minLength value="3"/><xs:maxLength value="5"/>
                </xs:restriction></xs:simpleType>
                <xs:element name="code" type="Code"/>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.restrictions["Code"].min_length, Some(3));
        assert!(
            validate_document("<code>ok</code>", &schema)[0]
                .message
                .contains("requires at least 3 (minLength)")
        );
        assert!(
            validate_document("<code>abcdef</code>", &schema)[0]
                .message
                .contains("requires at most 5 (maxLength)")
        );
        assert!(validate_document("<code>valid</code>", &schema).is_empty());
    }

    fn messages(source: &str, schema: &XsdSchema) -> Vec<String> {
        validate_document(source, schema)
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect()
    }

    const TYPED: &str = r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
            xmlns:t="urn:typed" targetNamespace="urn:typed" elementFormDefault="qualified">
        <xs:element name="root">
            <xs:complexType>
                <xs:sequence>
                    <xs:element name="date" type="xs:date" minOccurs="0" maxOccurs="unbounded"/>
                    <xs:element name="count" type="xs:unsignedByte" minOccurs="0" default="7"/>
                    <xs:element name="ratio" type="xs:decimal" minOccurs="0" fixed="1.5"/>
                    <xs:element name="name" type="xs:QName" minOccurs="0"/>
                    <xs:element name="any" minOccurs="0"/>
                    <xs:element name="box" type="t:Box" minOccurs="0"/>
                    <xs:element name="skip" minOccurs="0">
                        <xs:complexType><xs:sequence>
                            <xs:any processContents="skip" namespace="##any"/>
                        </xs:sequence></xs:complexType>
                    </xs:element>
                </xs:sequence>
                <xs:attribute name="level" type="t:Level"/>
                <xs:attribute name="stamp" type="xs:dateTime"/>
            </xs:complexType>
        </xs:element>
        <xs:element name="date" type="xs:date"/>
        <xs:complexType name="Box"><xs:sequence><xs:element name="date" type="xs:date"/></xs:sequence></xs:complexType>
        <xs:simpleType name="Level">
            <xs:restriction base="xs:int"><xs:minInclusive value="1"/><xs:maxInclusive value="5"/></xs:restriction>
        </xs:simpleType>
    </xs:schema>"###;

    fn typed(body: &str) -> String {
        format!(
            "<t:root xmlns:t=\"urn:typed\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">{body}</t:root>"
        )
    }

    #[test]
    fn validates_text_contents_against_builtin_types() {
        let schema = parse_xsd(TYPED).unwrap();
        assert_eq!(
            messages(&typed("<t:date>2026-09-30</t:date>"), &schema),
            Vec::<String>::new()
        );
        assert_eq!(
            messages(&typed("<t:date>2024-13-01</t:date>"), &schema),
            vec!["content of <t:date>: '2024-13-01' is not a valid xs:date: month must be 01-12"]
        );
        assert_eq!(
            messages(&typed("<t:count>256</t:count>"), &schema),
            vec![
                "content of <t:count>: '256' is not a valid xs:unsignedByte: the value must be at most 255"
            ]
        );
        // Whitespace is collapsed, entity and character references and CDATA
        // sections are part of the value.
        assert!(messages(&typed("<t:count>\n 2&#53; </t:count>"), &schema).is_empty());
        assert!(messages(&typed("<t:date><![CDATA[2026-09-30]]></t:date>"), &schema).is_empty());
        assert!(!messages(&typed("<t:date>2026-09-30&amp;</t:date>"), &schema).is_empty());
    }

    #[test]
    fn checks_empty_elements_defaults_and_fixed_values() {
        let schema = parse_xsd(TYPED).unwrap();
        assert_eq!(
            messages(&typed("<t:date/>"), &schema),
            vec![
                "content of <t:date>: '' is not a valid xs:date: expected the format YYYY-MM-DD with an optional time zone"
            ]
        );
        // An empty element takes its default or fixed value.
        assert!(messages(&typed("<t:count/><t:ratio></t:ratio>"), &schema).is_empty());
        // Fixed values are compared in the value space.
        assert!(messages(&typed("<t:ratio>1.50</t:ratio>"), &schema).is_empty());
        assert_eq!(
            messages(&typed("<t:ratio>2</t:ratio>"), &schema),
            vec!["content of <t:ratio> differs from the fixed value '1.5'"]
        );
        // xsi:nil elements are not checked.
        assert!(messages(&typed("<t:date xsi:nil=\"true\"/>"), &schema).len() == 1);
    }

    #[test]
    fn resolves_local_declarations_xsi_type_and_qname_prefixes() {
        let schema = parse_xsd(TYPED).unwrap();
        assert!(
            messages(
                &typed("<t:box><t:date>2026-01-01</t:date></t:box>"),
                &schema
            )
            .is_empty()
        );
        assert!(!messages(&typed("<t:box><t:date>soon</t:date></t:box>"), &schema).is_empty());
        assert!(messages(&typed("<t:name>t:root</t:name>"), &schema).is_empty());
        assert_eq!(
            messages(&typed("<t:name>u:root</t:name>"), &schema),
            vec![
                "content of <t:name>: 'u:root' is not a valid xs:QName: the prefix 'u' is not bound to a namespace"
            ]
        );
        assert!(messages(&typed("<t:name xmlns:u=\"urn:u\">u:root</t:name>"), &schema).is_empty());
        let xs = "xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"";
        assert!(
            messages(
                &typed(&format!("<t:any {xs} xsi:type=\"xs:int\">12</t:any>")),
                &schema
            )
            .is_empty()
        );
        assert_eq!(
            messages(
                &typed(&format!("<t:any {xs} xsi:type=\"xs:int\">twelve</t:any>")),
                &schema
            ),
            vec![
                "content of <t:any>: 'twelve' is not a valid xs:int: expected an integer such as '-12' (digits only)"
            ]
        );
    }

    #[test]
    fn validates_attribute_values_at_their_location() {
        let schema = parse_xsd(TYPED).unwrap();
        let source = "<t:root xmlns:t=\"urn:typed\"\r\n  level=\"9\" stamp=\"été\"/>";
        let diagnostics = validate_document_located(source, &schema);
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert_eq!(
            diagnostics[0].kind,
            XsdDiagnosticKind::InvalidAttributeValue
        );
        assert_eq!(&source[diagnostics[0].offset..diagnostics[0].end], "9");
        assert_eq!(
            diagnostics[0].message,
            "attribute @level of <t:root>: '9' must be at most 5 (maxInclusive of t:Level)"
        );
        assert_eq!(&source[diagnostics[1].offset..diagnostics[1].end], "été");
        assert!(
            diagnostics[1]
                .message
                .contains("is not a valid xs:dateTime")
        );
        assert!(
            validate_document("<t:root xmlns:t=\"urn:typed\" level=\" 3 \"/>", &schema).is_empty()
        );
    }

    #[test]
    fn reports_enumerations_with_their_own_kind() {
        let schema = parse_xsd(ENUMERATION).unwrap();
        let diagnostics = validate_document("<item>green</item>", &schema);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, XsdDiagnosticKind::InvalidEnumeration);
        assert_eq!(diagnostics[0].kind.id(), "invalidEnumeration");
        assert_eq!(
            XsdDiagnosticKind::InvalidAttributeValue.id(),
            "invalidAttributeValue"
        );
    }

    #[test]
    fn checks_content_kinds() {
        let schema = parse_xsd(TYPED).unwrap();
        assert_eq!(
            messages(
                &typed("<t:date><t:date>2026-01-01</t:date></t:date>"),
                &schema
            ),
            vec!["element <t:date> has the simple type xs:date and cannot contain elements"]
        );
        assert_eq!(
            messages(
                &typed("<t:box>text<t:date>2026-01-01</t:date></t:box>"),
                &schema
            ),
            vec!["element <t:box> cannot contain text (its type has element-only content)"]
        );
        // Elements matched by a processContents="skip" wildcard are not
        // checked.
        assert!(messages(&typed("<t:skip><t:date>nope</t:date></t:skip>"), &schema).is_empty());
    }

    #[test]
    fn checks_the_expanded_name_of_the_root() {
        let schema = parse_xsd(TYPED).unwrap();
        assert!(validate_document("<t:root xmlns:t=\"urn:typed\"/>", &schema).is_empty());
        assert!(validate_document("<root xmlns=\"urn:typed\"/>", &schema).is_empty());
        assert_eq!(
            messages("<t:root xmlns:t=\"urn:other\"/>", &schema),
            vec!["root element <t:root> not declared in the XSD schema"]
        );
    }

    #[test]
    fn validates_builtin_simple_types() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="enabled" type="xs:boolean"/>
                <xs:element name="count" type="xs:integer"/>
                <xs:element name="price" type="xs:decimal"/>
            </xs:schema>"#,
        )
        .unwrap();

        assert!(
            validate_document("<enabled>maybe</enabled>", &schema)[0]
                .message
                .contains("boolean")
        );
        assert!(
            validate_document("<count>12.5</count>", &schema)[0]
                .message
                .contains("integer")
        );
        assert!(validate_document("<price>12.50</price>", &schema).is_empty());
    }

    #[test]
    fn parses_and_validates_list_and_union_types() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="Numbers"><xs:list itemType="xs:integer"/></xs:simpleType>
                <xs:simpleType name="Value"><xs:union memberTypes="xs:integer xs:boolean"/></xs:simpleType>
                <xs:element name="numbers" type="Numbers"/>
                <xs:element name="value" type="Value"/>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.lists["Numbers"], "xs:integer");
        assert_eq!(schema.unions["Value"], vec!["xs:integer", "xs:boolean"]);
        assert!(validate_document("<numbers>1 2 3</numbers>", &schema).is_empty());
        assert!(
            validate_document("<numbers>1 nope</numbers>", &schema)
                .iter()
                .any(|diagnostic| diagnostic.message.contains("integer"))
        );
        assert!(validate_document("<value>false</value>", &schema).is_empty());
        assert!(
            validate_document("<value>nope</value>", &schema)[0]
                .message
                .contains("not valid for any member type of Value (xs:integer, xs:boolean)")
        );
    }

    #[test]
    fn validates_digit_restrictions() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="Amount"><xs:restriction base="xs:decimal">
                    <xs:totalDigits value="4"/><xs:fractionDigits value="2"/>
                </xs:restriction></xs:simpleType>
                <xs:element name="amount" type="Amount"/>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.restrictions["Amount"].total_digits, Some(4));
        assert!(
            validate_document("<amount>1.234</amount>", &schema)
                .iter()
                .any(|diagnostic| diagnostic.message.contains("fractionDigits"))
        );
        assert!(
            validate_document("<amount>12.345</amount>", &schema)
                .iter()
                .any(|diagnostic| diagnostic.message.contains("totalDigits"))
        );
        assert!(validate_document("<amount>123.4</amount>", &schema).is_empty());
    }

    #[test]
    fn validates_numeric_bound_restrictions() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="Score"><xs:restriction base="xs:integer">
                    <xs:minInclusive value="1"/><xs:maxExclusive value="10"/>
                </xs:restriction></xs:simpleType>
                <xs:element name="score" type="Score"/>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(
            schema.restrictions["Score"].min_inclusive.as_deref(),
            Some("1")
        );
        assert!(
            validate_document("<score>0</score>", &schema)[0]
                .message
                .contains("'0' must be at least 1 (minInclusive of Score)")
        );
        assert!(
            validate_document("<score>10</score>", &schema)[0]
                .message
                .contains("maxExclusive")
        );
        assert!(validate_document("<score>5</score>", &schema).is_empty());
    }

    #[test]
    fn validates_exact_length_restrictions() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="Code"><xs:restriction base="xs:string">
                    <xs:length value="3"/>
                </xs:restriction></xs:simpleType>
                <xs:element name="code" type="Code"/>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(schema.restrictions["Code"].length, Some(3));
        assert!(
            validate_document("<code>AB</code>", &schema)[0]
                .message
                .contains("requires exactly 3 (length)")
        );
        assert!(validate_document("<code>ABC</code>", &schema).is_empty());
    }

    #[test]
    fn validates_simple_type_patterns() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="Code"><xs:restriction base="xs:string">
                    <xs:pattern value="[A-Z]{3}"/>
                </xs:restriction></xs:simpleType>
                <xs:element name="code" type="Code"/>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(
            schema.restrictions["Code"].pattern.as_deref(),
            Some("[A-Z]{3}")
        );
        assert!(
            validate_document("<code>abc</code>", &schema)[0]
                .message
                .contains("pattern")
        );
        assert!(validate_document("<code>ABC</code>", &schema).is_empty());
    }

    #[test]
    fn validates_anonymous_simple_type_length_restrictions() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="code"><xs:simpleType><xs:restriction base="xs:string">
                    <xs:minLength value="2"/>
                </xs:restriction></xs:simpleType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert!(
            validate_document("<code>x</code>", &schema)
                .iter()
                .any(|diagnostic| diagnostic.message.contains("minLength"))
        );
    }

    #[test]
    fn completes_enumerated_attribute_values() {
        let schema = parse_xsd(ENUMERATION).unwrap();

        assert_eq!(schema.enumerations["Color"], vec!["red", "blue"]);
        assert_eq!(
            complete_attribute_values("<item color=\"b", 15, &schema)[0].label,
            "blue"
        );
    }

    #[test]
    fn completes_declared_attributes() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="item"><xs:complexType>
                    <xs:attribute name="id"/><xs:attribute name="name"/>
                </xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(complete_attributes("<item n", 7, &schema)[0].label, "name");
        assert_eq!(complete_attributes("<item ", 6, &schema).len(), 2);
    }

    #[test]
    fn completes_children_from_the_current_sequence() {
        let schema = parse_xsd(SEQUENCE).unwrap();

        assert_eq!(
            complete_elements("<root><", 7, &schema),
            vec![XsdCompletion {
                label: "child".to_owned(),
                insert_text: "child".to_owned(),
            }]
        );
    }

    #[test]
    fn supports_substitution_groups_in_validation_and_completion() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="head"/>
                <xs:element name="member" substitutionGroup="head"/>
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:element ref="head"/>
                </xs:sequence></xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();
        let source = "<root><";
        assert!(
            complete_elements(source, source.len(), &schema)
                .iter()
                .any(|completion| completion.label == "member")
        );
        assert!(validate_document("<root><member /></root>", &schema).is_empty());
    }

    #[test]
    fn completion_respects_sequence_order_and_max_occurs() {
        let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:element name="first"/>
                    <xs:element name="second"/>
                </xs:sequence></xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();
        let source = "<root><first /><";
        assert_eq!(
            complete_elements(source, source.len(), &schema),
            vec![XsdCompletion {
                label: "second".to_owned(),
                insert_text: "second".to_owned(),
            }]
        );
    }

    #[test]
    fn merges_components_from_multiple_schemas() {
        let first = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"/></xs:schema>"#,
        )
        .unwrap();
        let second = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="child"/></xs:schema>"#,
        )
        .unwrap();

        let merged = merge_schemas([first, second]);
        assert_eq!(
            merged
                .elements
                .iter()
                .map(|element| element.name.as_str())
                .collect::<Vec<_>>(),
            vec!["root", "child"]
        );
    }

    #[test]
    fn resolves_include_and_import_dependencies() {
        let references = resolve_schema_dependencies(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:include schemaLocation="common/base.xsd"/>
                <xs:import namespace="urn:other" schemaLocation="../other.xsd"/>
            </xs:schema>"#,
            "workspace/schema/root.xsd",
        )
        .unwrap();

        assert_eq!(references.len(), 2);
        assert_eq!(references[0].namespace, None);
        assert_eq!(
            references[0].path,
            PathBuf::from("workspace/schema/common/base.xsd")
        );
        assert_eq!(references[1].namespace.as_deref(), Some("urn:other"));
        assert_eq!(references[1].path, PathBuf::from("workspace/other.xsd"));
    }

    #[test]
    fn resolves_schema_location_pairs_and_no_namespace_location() {
        let source = r#"
            <root xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
                xsi:schemaLocation="urn:test schemas/test.xsd urn:other other.xsd"
                xsi:noNamespaceSchemaLocation="local.xsd" />
        "#;

        let references = resolve_schema_locations(source, "workspace/docs/document.xml").unwrap();
        assert_eq!(references.len(), 3);
        assert_eq!(references[0].namespace.as_deref(), Some("urn:test"));
        assert_eq!(
            references[0].path,
            PathBuf::from("workspace/docs/schemas/test.xsd")
        );
        assert_eq!(references[2].namespace, None);
        assert_eq!(
            references[2].path,
            PathBuf::from("workspace/docs/local.xsd")
        );
    }

    #[test]
    fn rejects_an_odd_schema_location_list() {
        assert!(
            resolve_schema_locations(
                r#"<root xsi:schemaLocation="urn:test only.xsd extra"/>"#,
                "document.xml"
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_oversized_xsd_sources() {
        let source = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">".to_owned()
            + &"x".repeat(16 * 1024 * 1024);
        assert_eq!(parse_xsd(&source), Err("XSD schema too large".to_owned()));
    }

    #[test]
    fn rejects_a_schema_without_elements() {
        assert!(parse_xsd("<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"/>").is_err());
    }

    #[test]
    fn decodes_percent_encoded_and_file_uri_schema_locations() {
        let directory =
            std::env::temp_dir().join(format!("xsd-core-locations {}", std::process::id()));
        std::fs::create_dir_all(directory.join("my schemas")).unwrap();
        std::fs::write(directory.join("my schemas/a b.xsd"), "<x/>").unwrap();
        let document = directory.join("doc.xml");
        let source = r#"<root xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
            xsi:noNamespaceSchemaLocation="my%20schemas/a%20b.xsd"/>"#;
        let references = resolve_schema_locations(source, &document).unwrap();
        assert_eq!(references[0].path, directory.join("my schemas/a b.xsd"));
        let _ = std::fs::remove_dir_all(&directory);

        assert_eq!(
            resolve_location(Path::new("/base"), "file:///tmp/a%20b.xsd#frag"),
            PathBuf::from("/tmp/a b.xsd")
        );
        assert_eq!(
            resolve_location(Path::new("/base"), "file://localhost/tmp/c.xsd"),
            PathBuf::from("/tmp/c.xsd")
        );
        assert_eq!(
            resolve_location(Path::new("/base"), "./sub/../missing%zz.xsd"),
            PathBuf::from("/base/missing%zz.xsd")
        );
        let remote = resolve_location(Path::new("/base"), "https://example.com/s.xsd");
        assert_eq!(remote, PathBuf::from("https://example.com/s.xsd"));
        assert!(is_remote_location(&remote));
        assert!(is_remote_location(Path::new("urn:x:y")));
        assert!(!is_remote_location(Path::new("/base/s.xsd")));
        assert!(!is_remote_location(Path::new("C:/base/s.xsd")));
    }

    #[test]
    fn consults_the_location_resolver_first() {
        let resolver = |request: &SchemaLocation<'_>| -> Option<PathBuf> {
            match (request.kind, request.namespace, request.location) {
                (SchemaLocationKind::SchemaLocation, Some("urn:a"), _) => {
                    Some(PathBuf::from("/catalog/a.xsd"))
                }
                (SchemaLocationKind::Import, Some("urn:b"), None) => {
                    Some(PathBuf::from("/catalog/b.xsd"))
                }
                (SchemaLocationKind::Include, None, Some("http://x/inc.xsd")) => {
                    Some(PathBuf::from("/catalog/inc.xsd"))
                }
                _ => None,
            }
        };
        let references = resolve_schema_locations_with(
            r#"<r xsi:schemaLocation="urn:a http://x/a.xsd urn:c c.xsd"/>"#,
            "/docs/d.xml",
            &resolver,
        )
        .unwrap();
        assert_eq!(
            references,
            vec![
                SchemaReference {
                    namespace: Some("urn:a".to_owned()),
                    path: PathBuf::from("/catalog/a.xsd"),
                },
                SchemaReference {
                    namespace: Some("urn:c".to_owned()),
                    path: PathBuf::from("/docs/c.xsd"),
                },
            ]
        );
        let dependencies = resolve_schema_dependencies_with(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:import namespace="urn:b"/>
                <xs:import namespace="urn:unknown"/>
                <xs:include schemaLocation="http://x/inc.xsd"/>
            </xs:schema>"#,
            "/schemas/s.xsd",
            &resolver,
        )
        .unwrap();
        assert_eq!(
            dependencies
                .iter()
                .map(|reference| reference.path.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("/catalog/b.xsd"),
                PathBuf::from("/catalog/inc.xsd")
            ]
        );
    }
}
