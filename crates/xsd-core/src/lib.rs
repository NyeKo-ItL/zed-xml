//! Modèle et parsing XSD partagés par le serveur LSP.

use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    str,
};

use quick_xml::{Reader, events::Event};

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
}

/// Schéma XSD minimal.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct XsdSchema {
    pub target_namespace: Option<String>,
    pub elements: Vec<XsdElement>,
    pub children: HashMap<String, Vec<String>>,
}

/// Référence XSD extraite d'un document XML.
#[derive(Debug, PartialEq, Eq)]
pub struct SchemaReference {
    pub namespace: Option<String>,
    pub path: PathBuf,
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

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let element_name = element.name();
                let current_name = local_name(element_name.as_ref());
                if current_name == "schema" {
                    schema.target_namespace = attribute(&element, "targetNamespace");
                }
                if current_name == "sequence" {
                    sequence_depth += 1;
                }
                let declared_name = if current_name == "element" {
                    attribute(&element, "name")
                } else {
                    None
                };
                if let (Some(parent), Some(child)) = (model_stack.last(), declared_name.as_ref())
                    && sequence_depth > 0
                {
                    schema
                        .children
                        .entry(parent.clone())
                        .or_default()
                        .push(child.clone());
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
                if current_name == "element"
                    && let Some(name) = attribute(&element, "name")
                {
                    if let Some(parent) = model_stack.last()
                        && sequence_depth > 0
                    {
                        schema
                            .children
                            .entry(parent.clone())
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
                                .map_err(|_| "minOccurs invalide".to_owned())?,
                            max: parse_max_occurs(attribute(&element, "maxOccurs"))?,
                        },
                    });
                }
            }
            Ok(Event::End(element)) => {
                let element_name = element.name();
                let current_name = local_name(element_name.as_ref());
                if current_name == "sequence" {
                    sequence_depth = sequence_depth.saturating_sub(1);
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

/// Vérifie le document XML contre les éléments déclarés par le schéma.
pub fn validate_document(source: &str, schema: &XsdSchema) -> Vec<XsdDiagnostic> {
    let mut diagnostics = match root_element_name(source) {
        Some(root) => validate_root(&root, schema),
        None => vec![XsdDiagnostic {
            message: "document XML sans élément racine".to_owned(),
        }],
    };
    let mut reader = Reader::from_str(source);
    let mut stack = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                if let Some(parent) = stack.last()
                    && let Some(allowed) = schema.children.get(parent)
                    && !allowed.iter().any(|child| child == &name)
                {
                    diagnostics.push(XsdDiagnostic {
                        message: format!("élément <{name}> interdit dans <{parent}>"),
                    });
                }
                stack.push(name);
            }
            Ok(Event::Empty(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                if let Some(parent) = stack.last()
                    && let Some(allowed) = schema.children.get(parent)
                    && !allowed.iter().any(|child| child == &name)
                {
                    diagnostics.push(XsdDiagnostic {
                        message: format!("élément <{name}> interdit dans <{parent}>"),
                    });
                }
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }

    diagnostics
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
