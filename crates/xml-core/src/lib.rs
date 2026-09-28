//! Modèle et analyse XML partagés par le serveur LSP.

use quick_xml::{Reader, events::Event};

/// Document XML partiellement analysé.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct XmlDocument {
    /// Nom de l'élément racine lorsqu'il a pu être identifié.
    pub root: Option<String>,
    /// Nombre d'éléments rencontrés pendant l'analyse.
    pub element_count: usize,
}

/// Diagnostic d'analyse exprimé avec un offset UTF-8 dans la source.
#[derive(Debug, PartialEq, Eq)]
pub struct XmlDiagnostic {
    pub message: String,
    pub offset: usize,
}

/// Résultat d'analyse conservant le modèle partiel même si la source est invalide.
#[derive(Debug, PartialEq, Eq)]
pub struct XmlParseResult {
    pub document: XmlDocument,
    pub diagnostics: Vec<XmlDiagnostic>,
}

/// Analyse une source XML et retourne les diagnostics récupérables pendant l'édition.
pub fn parse_xml(source: &str) -> XmlParseResult {
    let mut reader = Reader::from_str(source);
    let mut stack = Vec::new();
    let mut document = XmlDocument::default();
    let mut diagnostics = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                if stack.is_empty() {
                    if document.root.is_some() {
                        diagnostics.push(XmlDiagnostic {
                            message: "XML doit contenir un seul élément racine".to_owned(),
                            offset: reader.buffer_position() as usize,
                        });
                    } else {
                        document.root = Some(name.clone());
                    }
                }
                document.element_count += 1;
                stack.push(name);
            }
            Ok(Event::Empty(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                if stack.is_empty() {
                    if document.root.is_some() {
                        diagnostics.push(XmlDiagnostic {
                            message: "XML doit contenir un seul élément racine".to_owned(),
                            offset: reader.buffer_position() as usize,
                        });
                    } else {
                        document.root = Some(name);
                    }
                }
                document.element_count += 1;
            }
            Ok(Event::End(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                match stack.pop() {
                    Some(open_name) if open_name == name => {}
                    Some(open_name) => diagnostics.push(XmlDiagnostic {
                        message: format!("balise fermante </{name}> attend </{open_name}>"),
                        offset: reader.buffer_position() as usize,
                    }),
                    None => diagnostics.push(XmlDiagnostic {
                        message: format!("balise fermante inattendue </{name}>"),
                        offset: reader.buffer_position() as usize,
                    }),
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                diagnostics.push(XmlDiagnostic {
                    message: format!("erreur XML : {error}"),
                    offset: reader.buffer_position() as usize,
                });
                break;
            }
        }
    }

    if let Some(open_name) = stack.last() {
        diagnostics.push(XmlDiagnostic {
            message: format!("balise non fermée <{open_name}>"),
            offset: source.len(),
        });
    }

    XmlParseResult {
        document,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_valid_nested_document() {
        let result = parse_xml("<root><child /></root>");

        assert!(result.diagnostics.is_empty());
        assert_eq!(result.document.root.as_deref(), Some("root"));
        assert_eq!(result.document.element_count, 2);
    }

    #[test]
    fn reports_a_mismatched_closing_tag() {
        let result = parse_xml("<root><child></root>");

        assert_eq!(result.diagnostics.len(), 2);
        assert!(
            result.diagnostics[0]
                .message
                .contains("expected `</child>`")
        );
        assert!(result.diagnostics[1].message.contains("non fermée <child>"));
    }

    #[test]
    fn reports_an_unclosed_tag_at_end_of_source() {
        let result = parse_xml("<root><child>");

        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].offset, "<root><child>".len());
        assert!(result.diagnostics[0].message.contains("non fermée <child>"));
    }
}
