//! Modèle et parsing XSD partagés par le serveur LSP.

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
}

/// Diagnostic de validation XSD minimal.
#[derive(Debug, PartialEq, Eq)]
pub struct XsdDiagnostic {
    pub message: String,
}

/// Parse un `xs:schema` et ses déclarations `xs:element`.
pub fn parse_xsd(source: &str) -> Result<XsdSchema, String> {
    let mut reader = Reader::from_str(source);
    let mut schema = XsdSchema::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                let element_name = element.name();
                let local_name = local_name(element_name.as_ref());
                if local_name == "schema" {
                    schema.target_namespace = attribute(&element, "targetNamespace");
                } else if local_name == "element"
                    && let Some(name) = attribute(&element, "name")
                {
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
    fn rejects_a_schema_without_elements() {
        assert!(parse_xsd("<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"/>").is_err());
    }
}
