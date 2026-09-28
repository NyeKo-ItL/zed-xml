//! Modèle, analyse et formatage XML partagés par le serveur LSP.

use std::collections::BTreeSet;

use quick_xml::{Reader, Writer, events::Event};

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

/// Élément proposé par l'autocomplétion XML.
#[derive(Debug, PartialEq, Eq)]
pub struct XmlCompletion {
    pub label: String,
    pub insert_text: String,
}

/// Retourne des propositions locales à partir du document et du contexte courant.
pub fn complete_xml(source: &str, offset: usize) -> Vec<XmlCompletion> {
    let offset = offset.min(source.len());
    let prefix = &source[..offset];
    let Some(opening) = prefix.rfind('<') else {
        return Vec::new();
    };
    if prefix[opening..].contains('>') {
        return Vec::new();
    }

    let (elements, attributes) = collect_names(source);
    let fragment = &prefix[opening + 1..];
    let mut candidates = BTreeSet::new();
    let mut insert_prefix = String::new();

    if let Some(fragment) = fragment.strip_prefix('/') {
        let typed = fragment.trim();
        let stack = open_elements(prefix);
        insert_prefix.push_str("</");
        if let Some(name) = stack.last()
            && name.starts_with(typed)
        {
            candidates.insert(name.clone());
        }
    } else if fragment.chars().any(char::is_whitespace) {
        let typed = fragment
            .split(|character: char| character.is_whitespace())
            .last()
            .unwrap_or_default();
        for name in attributes {
            if name.starts_with(typed) {
                candidates.insert(name);
            }
        }
    } else {
        let typed = fragment.trim();
        for name in elements {
            if name.starts_with(typed) {
                candidates.insert(name);
            }
        }
    }

    candidates
        .into_iter()
        .map(|name| XmlCompletion {
            label: name.clone(),
            insert_text: format!("{insert_prefix}{name}"),
        })
        .collect()
}

fn collect_names(source: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut reader = Reader::from_str(source);
    let mut elements = BTreeSet::new();
    let mut attributes = BTreeSet::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                elements.insert(String::from_utf8_lossy(element.name().as_ref()).into_owned());
                for attribute in element.attributes().flatten() {
                    attributes.insert(String::from_utf8_lossy(attribute.key.as_ref()).into_owned());
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }

    (elements, attributes)
}

fn open_elements(source: &str) -> Vec<String> {
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
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }

    stack
}

/// Formate un document XML valide avec deux espaces par niveau.
pub fn format_xml(source: &str) -> Result<String, String> {
    if !parse_xml(source).diagnostics.is_empty() {
        return Err("le document XML est invalide".to_owned());
    }

    let mut reader = Reader::from_str(source);
    let mut writer = Writer::new(Vec::new());
    let mut depth = 0usize;
    let mut stack = Vec::new();
    let mut has_root = false;
    let mut output_started = false;

    loop {
        let event = reader
            .read_event()
            .map_err(|error| format!("erreur XML : {error}"))?;
        match event {
            Event::Eof => break,
            Event::Decl(_) | Event::DocType(_) | Event::PI(_) => {
                write_indent(&mut writer, depth, output_started)?;
                writer
                    .write_event(event.into_owned())
                    .map_err(|error| error.to_string())?;
                output_started = true;
            }
            Event::Start(element) => {
                if !stack.last().copied().unwrap_or(false) {
                    write_indent(&mut writer, depth, output_started)?;
                }
                if depth == 0 {
                    has_root = true;
                }
                stack.push(false);
                writer
                    .write_event(Event::Start(element.into_owned()))
                    .map_err(|error| error.to_string())?;
                depth += 1;
                output_started = true;
            }
            Event::Empty(element) => {
                if !stack.last().copied().unwrap_or(false) {
                    write_indent(&mut writer, depth, output_started)?;
                }
                if depth == 0 {
                    has_root = true;
                }
                writer
                    .write_event(Event::Empty(element.into_owned()))
                    .map_err(|error| error.to_string())?;
                output_started = true;
            }
            Event::End(element) => {
                depth = depth.saturating_sub(1);
                let has_text = stack.pop().unwrap_or(false);
                if !has_text {
                    write_indent(&mut writer, depth, output_started)?;
                }
                writer
                    .write_event(Event::End(element.into_owned()))
                    .map_err(|error| error.to_string())?;
                output_started = true;
            }
            Event::Text(text) => {
                if !text
                    .decode()
                    .map_err(|error| error.to_string())?
                    .trim()
                    .is_empty()
                {
                    if let Some(has_text) = stack.last_mut() {
                        *has_text = true;
                    }
                    writer
                        .write_event(Event::Text(text.into_owned()))
                        .map_err(|error| error.to_string())?;
                    output_started = true;
                }
            }
            Event::CData(data) => {
                if let Some(has_text) = stack.last_mut() {
                    *has_text = true;
                }
                writer
                    .write_event(Event::CData(data.into_owned()))
                    .map_err(|error| error.to_string())?;
                output_started = true;
            }
            Event::Comment(comment) => {
                if !stack.last().copied().unwrap_or(false) {
                    write_indent(&mut writer, depth, output_started)?;
                }
                writer
                    .write_event(Event::Comment(comment.into_owned()))
                    .map_err(|error| error.to_string())?;
                output_started = true;
            }
            Event::GeneralRef(reference) => {
                writer
                    .write_event(Event::GeneralRef(reference.into_owned()))
                    .map_err(|error| error.to_string())?;
                output_started = true;
            }
        }
    }

    if !has_root {
        return Err("le document XML ne contient aucun élément racine".to_owned());
    }

    let mut result = String::from_utf8(writer.into_inner()).map_err(|error| error.to_string())?;
    while result.ends_with('\n') {
        result.pop();
    }
    result.push('\n');
    Ok(result)
}

fn write_indent(
    writer: &mut Writer<Vec<u8>>,
    depth: usize,
    output_started: bool,
) -> Result<(), String> {
    if output_started {
        writer
            .write_event(Event::Text(quick_xml::events::BytesText::new("\n")))
            .map_err(|error| error.to_string())?;
    }
    if depth > 0 {
        let spaces = "  ".repeat(depth);
        writer
            .write_event(Event::Text(quick_xml::events::BytesText::new(&spaces)))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
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

    #[test]
    fn formats_nested_elements_and_is_idempotent() {
        let formatted = format_xml("<root>\n  <child id=\"1\" />\n</root>").unwrap();
        assert_eq!(formatted, "<root>\n  <child id=\"1\" />\n</root>\n");
        assert_eq!(format_xml(&formatted).unwrap(), formatted);
    }

    #[test]
    fn preserves_mixed_content_and_comments() {
        let formatted = format_xml("<root>Hello <b>world</b><!-- note --></root>").unwrap();
        assert_eq!(formatted, "<root>Hello <b>world</b><!-- note --></root>\n");
    }

    #[test]
    fn completes_element_names_and_closing_tags() {
        let elements = complete_xml("<root><item /></root><it", 24);
        assert_eq!(
            elements,
            vec![XmlCompletion {
                label: "item".to_owned(),
                insert_text: "item".to_owned(),
            }]
        );

        let closing = complete_xml("<root><item></", 14);
        assert_eq!(
            closing,
            vec![XmlCompletion {
                label: "item".to_owned(),
                insert_text: "</item".to_owned(),
            }]
        );
    }

    #[test]
    fn completes_known_attributes() {
        let completions = complete_xml("<root id=\"1\"><child name=\"x\" /></root><root ", 44);
        assert_eq!(
            completions,
            vec![
                XmlCompletion {
                    label: "id".to_owned(),
                    insert_text: "id".to_owned(),
                },
                XmlCompletion {
                    label: "name".to_owned(),
                    insert_text: "name".to_owned(),
                }
            ]
        );
    }

    #[test]
    fn preserves_declaration_and_cdata() {
        let formatted =
            format_xml("<?xml version=\"1.0\"?><root><![CDATA[a < b]]></root>").unwrap();
        assert_eq!(
            formatted,
            "<?xml version=\"1.0\"?>\n<root><![CDATA[a < b]]></root>\n"
        );
    }

    #[test]
    fn rejects_invalid_xml() {
        assert!(format_xml("<root>").is_err());
    }
}
