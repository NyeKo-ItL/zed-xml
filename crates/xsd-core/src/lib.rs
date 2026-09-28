//! Modèle et parsing XSD partagés par le serveur LSP.

use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    str,
};

use quick_xml::{Reader, events::Event};
use regex::Regex;

/// Cardinalité d'un élément XSD.
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

/// Élément déclaré par un schéma XSD.
#[derive(Debug, PartialEq, Eq)]
pub struct XsdElement {
    pub name: String,
    pub occurs: XsdOccurs,
    pub type_name: Option<String>,
}

/// Restriction simple portée par un type XSD.
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
    pub pattern: Option<String>,
}

/// Schéma XSD minimal.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct XsdSchema {
    pub target_namespace: Option<String>,
    pub elements: Vec<XsdElement>,
    pub children: HashMap<String, Vec<String>>,
    pub choices: HashMap<String, Vec<String>>,
    pub attributes: HashMap<String, Vec<String>>,
    pub required_attributes: HashMap<String, Vec<String>>,
    pub enumerations: HashMap<String, Vec<String>>,
    pub restrictions: HashMap<String, XsdRestriction>,
}

/// Référence XSD extraite d'un document XML.
#[derive(Debug, PartialEq, Eq)]
pub struct SchemaReference {
    pub namespace: Option<String>,
    pub path: PathBuf,
}

/// Élément proposé par l’autocomplétion XSD.
#[derive(Debug, PartialEq, Eq)]
pub struct XsdCompletion {
    pub label: String,
    pub insert_text: String,
}

/// Diagnostic de validation XSD minimal.
#[derive(Debug, PartialEq, Eq)]
pub struct XsdDiagnostic {
    pub message: String,
}

/// Parse un `xs:schema`, ses éléments et une première `xs:sequence`.
pub fn parse_xsd(source: &str) -> Result<XsdSchema, String> {
    let mut reader = Reader::from_str(source);
    let mut schema = XsdSchema::default();
    let mut element_stack = Vec::new();
    let mut model_stack: Vec<String> = Vec::new();
    let mut sequence_depth = 0usize;
    let mut choice_depth = 0usize;
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
                simple_type_stack.push(simple_name);
                if current_name == "schema" {
                    schema.target_namespace = attribute(&element, "targetNamespace");
                }
                if current_name == "sequence" {
                    sequence_depth += 1;
                }
                if current_name == "choice" {
                    choice_depth += 1;
                }
                let declared_name = if current_name == "element" {
                    attribute(&element, "name")
                } else {
                    None
                };
                if current_name == "attribute"
                    && let Some(parent) = model_stack.last()
                    && let Some(name) = attribute(&element, "name")
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
                            .push(name);
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
                }
                if let Some(name) = declared_name {
                    schema.elements.push(XsdElement {
                        name: name.clone(),
                        occurs: XsdOccurs {
                            min: attribute(&element, "minOccurs")
                                .as_deref()
                                .unwrap_or("1")
                                .parse()
                                .map_err(|_| "minOccurs invalide".to_owned())?,
                            max: parse_max_occurs(attribute(&element, "maxOccurs"))?,
                        },
                        type_name: attribute(&element, "type"),
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
                if current_name == "attribute"
                    && let Some(parent) = model_stack.last()
                    && let Some(name) = attribute(&element, "name")
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
                            .push(name);
                    }
                }
                if current_name == "element"
                    && let Some(name) = attribute(&element, "name")
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
                    }
                    schema.elements.push(XsdElement {
                        name,
                        occurs: XsdOccurs {
                            min: attribute(&element, "minOccurs")
                                .as_deref()
                                .unwrap_or("1")
                                .parse()
                                .map_err(|_| "minOccurs invalide".to_owned())?,
                            max: parse_max_occurs(attribute(&element, "maxOccurs"))?,
                        },
                        type_name: attribute(&element, "type"),
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
                if element_stack.pop().flatten().is_some() {
                    model_stack.pop();
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("erreur XSD : {error}")),
        }
    }

    if schema.elements.is_empty() {
        return Err("le schéma XSD ne contient aucun xs:element".to_owned());
    }

    Ok(schema)
}

/// Résout les références XSD d'un document XML par rapport à son chemin.
pub fn resolve_schema_locations(
    source: &str,
    document_path: impl AsRef<Path>,
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
                            "xsi:schemaLocation doit contenir des paires namespace/chemin"
                                .to_owned(),
                        );
                    }
                    for pair in values.chunks_exact(2) {
                        references.push(SchemaReference {
                            namespace: Some(pair[0].to_owned()),
                            path: resolve_path(base_directory, pair[1]),
                        });
                    }
                }
                if let Some(value) = attribute(&element, "noNamespaceSchemaLocation") {
                    references.push(SchemaReference {
                        namespace: None,
                        path: resolve_path(base_directory, value.trim()),
                    });
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("erreur XML : {error}")),
        }
    }

    Ok(references)
}

fn resolve_path(base_directory: &Path, value: &str) -> PathBuf {
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

/// Retourne les éléments XSD adaptés au contexte XML courant.
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

/// Retourne les attributs XSD adaptés à l’élément ouvert courant.
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
            names.extend(schema.choices.get(parent).cloned().unwrap_or_default());
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

/// Retourne le nom du premier élément XML rencontré.
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

struct XmlFrame {
    name: String,
    children: Vec<String>,
    text: String,
}

/// Vérifie le document XML contre les éléments déclarés par le schéma.
pub fn validate_document(source: &str, schema: &XsdSchema) -> Vec<XsdDiagnostic> {
    let mut diagnostics = match root_element_name(source) {
        Some(root) => validate_root(&root, schema),
        None => vec![XsdDiagnostic {
            message: "document XML sans élément racine".to_owned(),
        }],
    };
    let mut reader = Reader::from_str(source);
    let mut stack: Vec<XmlFrame> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                diagnostics.extend(validate_attributes(schema, &name, &element));
                if let Some(parent) = stack.last_mut() {
                    if !is_allowed_child(schema, &parent.name, &name) {
                        diagnostics.push(XsdDiagnostic {
                            message: format!("élément <{name}> interdit dans <{}>", parent.name),
                        });
                    }
                    parent.children.push(name.clone());
                }
                stack.push(XmlFrame {
                    name,
                    children: Vec::new(),
                    text: String::new(),
                });
            }
            Ok(Event::Empty(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                diagnostics.extend(validate_attributes(schema, &name, &element));
                if let Some(parent) = stack.last_mut() {
                    if !is_allowed_child(schema, &parent.name, &name) {
                        diagnostics.push(XsdDiagnostic {
                            message: format!("élément <{name}> interdit dans <{}>", parent.name),
                        });
                    }
                    parent.children.push(name);
                }
            }
            Ok(Event::End(_)) => {
                if let Some(frame) = stack.pop() {
                    diagnostics.extend(validate_sequence_frame(&frame, schema));
                    diagnostics.extend(validate_text_content(&frame, schema));
                }
            }
            Ok(Event::Text(text)) => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push_str(&String::from_utf8_lossy(text.as_ref()));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }

    diagnostics
}

fn validate_sequence_frame(frame: &XmlFrame, schema: &XsdSchema) -> Vec<XsdDiagnostic> {
    let Some(expected) = schema.children.get(&frame.name) else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    let mut previous_index = 0;
    for child in &frame.children {
        if let Some(index) = expected.iter().position(|name| name == child) {
            if index < previous_index {
                diagnostics.push(XsdDiagnostic {
                    message: format!("ordre inattendu de <{child}> dans <{}>", frame.name),
                });
            }
            previous_index = index;
        }
    }
    for child in expected {
        let count = frame.children.iter().filter(|name| *name == child).count();
        if let Some(element) = schema
            .elements
            .iter()
            .find(|element| element.name == *child)
        {
            if count < element.occurs.min {
                diagnostics.push(XsdDiagnostic {
                    message: format!("élément <{child}> requis dans <{}>", frame.name),
                });
            }
            if let Some(max) = element.occurs.max
                && count > max
            {
                diagnostics.push(XsdDiagnostic {
                    message: format!("trop d’éléments <{child}> dans <{}>", frame.name),
                });
            }
        }
    }
    diagnostics
}

fn validate_text_content(frame: &XmlFrame, schema: &XsdSchema) -> Vec<XsdDiagnostic> {
    let Some(element) = schema
        .elements
        .iter()
        .find(|element| element.name == frame.name)
    else {
        return Vec::new();
    };
    let type_name = element
        .type_name
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("__anonymous:{}", frame.name));
    let value = frame.text.trim();
    let mut diagnostics = validate_builtin_type(&frame.name, &type_name, value);
    let Some(restriction) = schema.restrictions.get(&type_name) else {
        return diagnostics;
    };
    diagnostics.extend(validate_numeric_facets(
        &frame.name,
        &type_name,
        value,
        restriction,
    ));
    diagnostics.extend(validate_digit_facets(&frame.name, value, restriction));
    let length = value.chars().count();
    if let Some(expected) = restriction.length
        && length != expected
    {
        diagnostics.push(XsdDiagnostic {
            message: format!(
                "contenu de <{}> de longueur incorrecte (attendu {expected} caractères)",
                frame.name
            ),
        });
    }
    if let Some(min) = restriction.min_length
        && length < min
    {
        diagnostics.push(XsdDiagnostic {
            message: format!(
                "contenu de <{}> trop court (minimum {min} caractères)",
                frame.name
            ),
        });
    }
    if let Some(max) = restriction.max_length
        && length > max
    {
        diagnostics.push(XsdDiagnostic {
            message: format!(
                "contenu de <{}> trop long (maximum {max} caractères)",
                frame.name
            ),
        });
    }
    if let Some(pattern) = &restriction.pattern
        && Regex::new(pattern).is_ok_and(|regex| !regex.is_match(value))
    {
        diagnostics.push(XsdDiagnostic {
            message: format!("contenu de <{}> ne respecte pas le motif XSD", frame.name),
        });
    }
    diagnostics
}

fn validate_numeric_facets(
    element_name: &str,
    type_name: &str,
    value: &str,
    restriction: &XsdRestriction,
) -> Vec<XsdDiagnostic> {
    let Some(number) = value.parse::<f64>().ok() else {
        return Vec::new();
    };
    let checks = [
        (
            restriction.min_inclusive.as_deref(),
            0u8,
            "minimum inclusif",
        ),
        (
            restriction.max_inclusive.as_deref(),
            1u8,
            "maximum inclusif",
        ),
        (
            restriction.min_exclusive.as_deref(),
            2u8,
            "minimum exclusif",
        ),
        (
            restriction.max_exclusive.as_deref(),
            3u8,
            "maximum exclusif",
        ),
    ];
    checks
        .into_iter()
        .find_map(|(limit, operation, label)| {
            let limit = limit?.parse::<f64>().ok()?;
            let valid = match operation {
                0 => number >= limit,
                1 => number <= limit,
                2 => number > limit,
                _ => number < limit,
            };
            (!valid).then(|| XsdDiagnostic {
                message: format!("contenu de <{element_name}> hors {label} du type {type_name}"),
            })
        })
        .into_iter()
        .collect()
}

fn validate_digit_facets(
    element_name: &str,
    value: &str,
    restriction: &XsdRestriction,
) -> Vec<XsdDiagnostic> {
    let digits = value
        .chars()
        .filter(|character| character.is_ascii_digit())
        .count();
    let fraction = value
        .split_once('.')
        .map(|(_, fraction)| {
            fraction
                .chars()
                .filter(|character| character.is_ascii_digit())
                .count()
        })
        .unwrap_or(0);
    let mut diagnostics = Vec::new();
    if let Some(expected) = restriction.total_digits
        && digits != expected
    {
        diagnostics.push(XsdDiagnostic {
            message: format!("contenu de <{element_name}> invalide (totalDigits={expected})"),
        });
    }
    if let Some(expected) = restriction.fraction_digits
        && fraction > expected
    {
        diagnostics.push(XsdDiagnostic {
            message: format!("contenu de <{element_name}> invalide (fractionDigits={expected})"),
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
        "minInclusive" | "maxInclusive" | "minExclusive" | "maxExclusive"
    )
    .then_some(name)
}

fn set_numeric_facet(restriction: &mut XsdRestriction, facet: &str, value: String) {
    match facet {
        "minInclusive" => restriction.min_inclusive = Some(value),
        "maxInclusive" => restriction.max_inclusive = Some(value),
        "minExclusive" => restriction.min_exclusive = Some(value),
        "maxExclusive" => restriction.max_exclusive = Some(value),
        _ => {}
    }
}

fn validate_builtin_type(element_name: &str, type_name: &str, value: &str) -> Vec<XsdDiagnostic> {
    let valid = match type_name.rsplit(':').next().unwrap_or(type_name) {
        "boolean" => matches!(value, "true" | "false" | "0" | "1"),
        "integer" => value.parse::<i128>().is_ok(),
        "decimal" => Regex::new(r"^-?[0-9]+(\.[0-9]+)?$").is_ok_and(|regex| regex.is_match(value)),
        _ => true,
    };
    if valid {
        Vec::new()
    } else {
        vec![XsdDiagnostic {
            message: format!("contenu de <{element_name}> invalide pour le type {type_name}"),
        }]
    }
}

fn validate_attributes(
    schema: &XsdSchema,
    element_name: &str,
    element: &quick_xml::events::BytesStart<'_>,
) -> Vec<XsdDiagnostic> {
    let Some(allowed) = schema.attributes.get(element_name) else {
        return Vec::new();
    };
    let mut present = Vec::new();
    let mut diagnostics = Vec::new();
    for attribute in element.attributes().flatten() {
        let raw_name = String::from_utf8_lossy(attribute.key.as_ref());
        let name = local_name(attribute.key.as_ref()).to_owned();
        if raw_name == "xmlns" || raw_name.starts_with("xmlns:") {
            continue;
        }
        present.push(name.clone());
        if !allowed.iter().any(|item| item == &name) {
            diagnostics.push(XsdDiagnostic {
                message: format!("attribut @{name} interdit sur <{element_name}>"),
            });
        }
    }
    if let Some(required) = schema.required_attributes.get(element_name) {
        for name in required {
            if !present.iter().any(|item| item == name) {
                diagnostics.push(XsdDiagnostic {
                    message: format!("attribut @{name} requis sur <{element_name}>"),
                });
            }
        }
    }
    diagnostics
}

fn is_allowed_child(schema: &XsdSchema, parent: &str, child: &str) -> bool {
    let sequence = schema.children.get(parent);
    let choice = schema.choices.get(parent);
    if sequence.is_none() && choice.is_none() {
        return true;
    }
    sequence.is_some_and(|children| children.iter().any(|name| name == child))
        || choice.is_some_and(|children| children.iter().any(|name| name == child))
}

/// Vérifie que le nom de la racine XML est déclaré par le schéma.
pub fn validate_root(root_name: &str, schema: &XsdSchema) -> Vec<XsdDiagnostic> {
    if schema
        .elements
        .iter()
        .any(|element| element.name == root_name)
    {
        Vec::new()
    } else {
        vec![XsdDiagnostic {
            message: format!("élément racine <{root_name}> absent du schéma XSD"),
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
                .map_err(|_| "valeur numérique XSD invalide".to_owned())
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
            .map_err(|_| "maxOccurs invalide".to_owned()),
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
    fn validates_children_declared_by_a_sequence() {
        let schema = parse_xsd(SEQUENCE).unwrap();

        assert_eq!(schema.children["root"], vec!["child"]);
        assert!(validate_document("<root><child /></root>", &schema).is_empty());
        assert_eq!(
            validate_document("<root><other /></root>", &schema)[0].message,
            "élément <other> interdit dans <root>"
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
                .contains("@id requis")
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
                .contains("@other interdit")
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
                .any(|diagnostic| diagnostic.message.contains("ordre inattendu"))
        );
        assert!(
            validate_document("<root></root>", &schema)[0]
                .message
                .contains("<first> requis")
        );
        assert!(
            validate_document("<root><first/><second/><second/><second/></root>", &schema)
                .iter()
                .any(|diagnostic| diagnostic.message.contains("trop d’éléments <second>"))
        );
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
                .contains("trop court")
        );
        assert!(
            validate_document("<code>abcdef</code>", &schema)[0]
                .message
                .contains("trop long")
        );
        assert!(validate_document("<code>valid</code>", &schema).is_empty());
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
            validate_document("<amount>12.345</amount>", &schema)
                .iter()
                .any(|diagnostic| diagnostic.message.contains("fractionDigits"))
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
                .contains("minimum inclusif")
        );
        assert!(
            validate_document("<score>10</score>", &schema)[0]
                .message
                .contains("maximum exclusif")
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
                .contains("longueur incorrecte")
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
                .contains("motif")
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
                .any(|diagnostic| diagnostic.message.contains("trop court"))
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
    fn rejects_a_schema_without_elements() {
        assert!(parse_xsd("<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"/>").is_err());
    }
}
