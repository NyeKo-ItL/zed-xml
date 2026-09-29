//! Modèle, analyse et formatage XML partagés par le serveur LSP.

use std::collections::BTreeSet;

use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};

const MAX_XML_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_XML_DEPTH: usize = 512;

/// Document XML partiellement analysé.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct XmlDocument {
    /// Nom de l'élément racine lorsqu'il a pu être identifié.
    pub root: Option<String>,
    /// Nombre d'éléments rencontrés pendant l'analyse.
    pub element_count: usize,
}

/// Catégorie d'un diagnostic XML.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum XmlDiagnosticKind {
    Syntax,
    Structure,
}

/// Diagnostic d'analyse exprimé avec un offset UTF-8 dans la source.
#[derive(Debug, PartialEq, Eq)]
pub struct XmlDiagnostic {
    pub kind: XmlDiagnosticKind,
    pub message: String,
    pub offset: usize,
}

impl XmlDiagnostic {
    pub fn code(&self) -> &'static str {
        match self.kind {
            XmlDiagnosticKind::Syntax => "xml-syntax",
            XmlDiagnosticKind::Structure => "xml-structure",
        }
    }
}

/// Résultat d'analyse conservant le modèle partiel même si la source est invalide.
#[derive(Debug, PartialEq, Eq)]
pub struct XmlParseResult {
    pub document: XmlDocument,
    pub diagnostics: Vec<XmlDiagnostic>,
}

/// Analyse une source XML et retourne les diagnostics récupérables pendant l'édition.
pub fn parse_xml(source: &str) -> XmlParseResult {
    if source.len() > MAX_XML_SOURCE_BYTES {
        return XmlParseResult {
            document: XmlDocument::default(),
            diagnostics: vec![XmlDiagnostic {
                kind: XmlDiagnosticKind::Syntax,
                message: "document XML trop volumineux".to_owned(),
                offset: 0,
            }],
        };
    }
    let mut reader = Reader::from_str(source);
    let mut stack = Vec::new();
    let mut document = XmlDocument::default();
    let mut diagnostics = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                if stack.len() >= MAX_XML_DEPTH {
                    diagnostics.push(XmlDiagnostic {
                        kind: XmlDiagnosticKind::Structure,
                        message: "profondeur XML maximale dépassée".to_owned(),
                        offset: reader.buffer_position() as usize,
                    });
                    break;
                }
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                if stack.is_empty() {
                    if document.root.is_some() {
                        diagnostics.push(XmlDiagnostic {
                            kind: XmlDiagnosticKind::Structure,
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
                            kind: XmlDiagnosticKind::Structure,
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
                        kind: XmlDiagnosticKind::Structure,
                        message: format!("balise fermante </{name}> attend </{open_name}>"),
                        offset: reader.buffer_position() as usize,
                    }),
                    None => diagnostics.push(XmlDiagnostic {
                        kind: XmlDiagnosticKind::Structure,
                        message: format!("balise fermante inattendue </{name}>"),
                        offset: reader.buffer_position() as usize,
                    }),
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                diagnostics.push(XmlDiagnostic {
                    kind: XmlDiagnosticKind::Syntax,
                    message: format!("erreur XML : {error}"),
                    offset: reader.buffer_position() as usize,
                });
                break;
            }
        }
    }

    if let Some(open_name) = stack.last() {
        diagnostics.push(XmlDiagnostic {
            kind: XmlDiagnosticKind::Structure,
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

/// Retourne une balise fermante lorsque le curseur suit immédiatement une balise ouvrante.
pub fn auto_close_tag(source: &str, offset: usize) -> Option<XmlCompletion> {
    let prefix = &source[..offset.min(source.len())];
    if !prefix.ends_with('>') {
        return None;
    }

    let mut reader = Reader::from_str(prefix);
    let mut stack = Vec::new();
    let mut opening_is_last = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                stack.push(String::from_utf8_lossy(element.name().as_ref()).into_owned());
                opening_is_last = true;
            }
            Ok(Event::Empty(_)) => opening_is_last = false,
            Ok(Event::End(_)) => {
                stack.pop();
                opening_is_last = false;
            }
            Ok(Event::Text(text)) => {
                opening_is_last = text
                    .decode()
                    .map(|value| value.trim().is_empty())
                    .unwrap_or(false);
            }
            Ok(
                Event::CData(_)
                | Event::Comment(_)
                | Event::Decl(_)
                | Event::DocType(_)
                | Event::PI(_)
                | Event::GeneralRef(_),
            ) => opening_is_last = false,
            Ok(Event::Eof) | Err(_) => break,
        }
    }

    if opening_is_last {
        stack.last().map(|name| XmlCompletion {
            label: format!("</{name}>"),
            insert_text: format!("</{name}>"),
        })
    } else {
        None
    }
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
    if let Some(processing_instruction) = fragment.strip_prefix('?') {
        return processing_instruction_templates(processing_instruction);
    }
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

fn processing_instruction_templates(typed: &str) -> Vec<XmlCompletion> {
    const TEMPLATES: [(&str, &str); 3] = [
        ("xml", "xml version=\"1.0\" encoding=\"UTF-8\"?>"),
        (
            "xml-model",
            "xml-model href=\"schema.xsd\" type=\"application/xml\" schematypens=\"http://www.w3.org/2001/XMLSchema\"?>",
        ),
        (
            "xml-stylesheet",
            "xml-stylesheet type=\"text/xsl\" href=\"stylesheet.xsl\"?>",
        ),
    ];

    if let Some((label, template)) = TEMPLATES.iter().find(|(label, _)| *label == typed) {
        return vec![XmlCompletion {
            label: (*label).to_owned(),
            insert_text: template[typed.len()..].to_owned(),
        }];
    }

    TEMPLATES
        .into_iter()
        .filter(|(label, _)| label.starts_with(typed))
        .map(|(label, template)| XmlCompletion {
            label: label.to_owned(),
            insert_text: template[typed.len()..].to_owned(),
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
    let mut pending_start: Option<BytesStart<'static>> = None;

    loop {
        let event = reader
            .read_event()
            .map_err(|error| format!("erreur XML : {error}"))?;
        match event {
            Event::Eof => break,
            Event::Decl(_) | Event::DocType(_) | Event::PI(_) => {
                flush_pending_start(
                    &mut writer,
                    &mut pending_start,
                    &mut stack,
                    &mut depth,
                    &mut output_started,
                )?;
                write_indent(&mut writer, depth, output_started)?;
                writer
                    .write_event(event.into_owned())
                    .map_err(|error| error.to_string())?;
                output_started = true;
            }
            Event::Start(element) => {
                flush_pending_start(
                    &mut writer,
                    &mut pending_start,
                    &mut stack,
                    &mut depth,
                    &mut output_started,
                )?;
                pending_start = Some(element.into_owned());
                if depth == 0 {
                    has_root = true;
                }
            }
            Event::Empty(element) => {
                flush_pending_start(
                    &mut writer,
                    &mut pending_start,
                    &mut stack,
                    &mut depth,
                    &mut output_started,
                )?;
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
                if let Some(start) = pending_start.take() {
                    flush_start_element(
                        &mut writer,
                        start,
                        &mut stack,
                        &mut depth,
                        &mut output_started,
                    )?;
                }
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
                if !String::from_utf8_lossy(text.as_ref()).trim().is_empty() {
                    flush_pending_start(
                        &mut writer,
                        &mut pending_start,
                        &mut stack,
                        &mut depth,
                        &mut output_started,
                    )?;
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
                flush_pending_start(
                    &mut writer,
                    &mut pending_start,
                    &mut stack,
                    &mut depth,
                    &mut output_started,
                )?;
                if let Some(has_text) = stack.last_mut() {
                    *has_text = true;
                }
                writer
                    .write_event(Event::CData(data.into_owned()))
                    .map_err(|error| error.to_string())?;
                output_started = true;
            }
            Event::Comment(comment) => {
                flush_pending_start(
                    &mut writer,
                    &mut pending_start,
                    &mut stack,
                    &mut depth,
                    &mut output_started,
                )?;
                if !stack.last().copied().unwrap_or(false) {
                    write_indent(&mut writer, depth, output_started)?;
                }
                writer
                    .write_event(Event::Comment(comment.into_owned()))
                    .map_err(|error| error.to_string())?;
                output_started = true;
            }
            Event::GeneralRef(reference) => {
                flush_pending_start(
                    &mut writer,
                    &mut pending_start,
                    &mut stack,
                    &mut depth,
                    &mut output_started,
                )?;
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

fn flush_pending_start(
    writer: &mut Writer<Vec<u8>>,
    pending_start: &mut Option<BytesStart<'static>>,
    stack: &mut Vec<bool>,
    depth: &mut usize,
    output_started: &mut bool,
) -> Result<(), String> {
    let Some(start) = pending_start.take() else {
        return Ok(());
    };
    flush_start_element(writer, start, stack, depth, output_started)
}

fn flush_start_element(
    writer: &mut Writer<Vec<u8>>,
    start: BytesStart<'static>,
    stack: &mut Vec<bool>,
    depth: &mut usize,
    output_started: &mut bool,
) -> Result<(), String> {
    if !stack.last().copied().unwrap_or(false) {
        write_indent(writer, *depth, *output_started)?;
    }
    stack.push(false);
    writer
        .write_event(Event::Start(start))
        .map_err(|error| error.to_string())?;
    *depth += 1;
    *output_started = true;
    Ok(())
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
    fn rejects_xml_sources_exceeding_safety_limits() {
        let source = "<root>".to_owned() + &"x".repeat(16 * 1024 * 1024);
        let result = parse_xml(&source);
        assert_eq!(
            result.diagnostics[0].message,
            "document XML trop volumineux"
        );

        let nested = (0..513).fold(String::new(), |mut source, _| {
            source.push_str("<node>");
            source
        });
        let result = parse_xml(&nested);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("profondeur XML"))
        );
    }

    #[test]
    fn formats_nested_elements_and_is_idempotent() {
        let formatted = format_xml("<root>\n  <child id=\"1\" />\n</root>").unwrap();
        assert_eq!(formatted, "<root>\n  <child id=\"1\" />\n</root>\n");
        assert_eq!(format_xml(&formatted).unwrap(), formatted);
    }

    #[test]
    fn preserves_explicit_empty_element_pairs() {
        let source = "<root>\n  <connexionId>\n  </connexionId>\n  <nested>\n    <value>\n    </value>\n  </nested>\n</root>";
        let formatted = format_xml(source).unwrap();

        assert_eq!(
            formatted,
            "<root>\n  <connexionId>\n  </connexionId>\n  <nested>\n    <value>\n    </value>\n  </nested>\n</root>\n"
        );
        assert_eq!(format_xml(&formatted).unwrap(), formatted);
    }

    #[test]
    fn indents_deeply_nested_documents_with_existing_whitespace() {
        let source = r#"<?xml version="1.0" encoding="iso-8859-1" ?>
<SampleEnvelope>
<Connection>
<EnvelopeNumber>EXAMPLE-001</EnvelopeNumber>
<Timestamp>
<Date>2099-01-02</Date>
<Time>03:04:05</Time>
</Timestamp>
</Connection>
<Payload>
<Header>
<TransportData>
<AccessId />
<Direction />
</TransportData>
</Header>
<Charges>
<Charge><Code>TEST</Code><Amount>123</Amount></Charge>
</Charges>
</Payload>
</SampleEnvelope>"#;
        let formatted = format_xml(source).expect("nested XML should be formatted");
        assert!(formatted.contains("\n  <Connection>\n    <EnvelopeNumber>"));
        assert!(formatted.contains("\n      <Date>2099-01-02</Date>"));
        assert!(formatted.contains("\n        <AccessId />"));
        assert!(formatted.contains("\n    <Charges>\n      <Charge>\n        <Code>"));
        assert_eq!(format_xml(&formatted).unwrap(), formatted);
    }

    #[test]
    fn preserves_mixed_content_and_comments() {
        let formatted = format_xml("<root>Hello <b>world</b><!-- note --></root>").unwrap();
        assert_eq!(formatted, "<root>Hello <b>world</b><!-- note --></root>\n");
    }

    #[test]
    fn auto_closes_the_most_recent_opening_tag() {
        assert_eq!(
            auto_close_tag("<root>", 6),
            Some(XmlCompletion {
                label: "</root>".to_owned(),
                insert_text: "</root>".to_owned(),
            })
        );
        assert_eq!(auto_close_tag("<root />", 8), None);
        assert_eq!(auto_close_tag("<root>text", 10), None);
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
    fn completes_processing_instruction_templates() {
        assert_eq!(
            complete_xml("<?", 2),
            vec![
                XmlCompletion {
                    label: "xml".to_owned(),
                    insert_text: "xml version=\"1.0\" encoding=\"UTF-8\"?>".to_owned(),
                },
                XmlCompletion {
                    label: "xml-model".to_owned(),
                    insert_text: "xml-model href=\"schema.xsd\" type=\"application/xml\" schematypens=\"http://www.w3.org/2001/XMLSchema\"?>".to_owned(),
                },
                XmlCompletion {
                    label: "xml-stylesheet".to_owned(),
                    insert_text: "xml-stylesheet type=\"text/xsl\" href=\"stylesheet.xsl\"?>".to_owned(),
                },
            ]
        );

        let xml = complete_xml("<?xml", 5);
        assert_eq!(xml.len(), 1);
        assert_eq!(xml[0].label, "xml");
        assert_eq!(xml[0].insert_text, " version=\"1.0\" encoding=\"UTF-8\"?>");

        let model = complete_xml("<?xml-mo", 8);
        assert_eq!(model.len(), 1);
        assert_eq!(model[0].label, "xml-model");
        assert!(model[0].insert_text.starts_with("del href=\"schema.xsd\""));
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
    fn handles_completion_and_auto_close_across_many_typing_contexts() {
        let cases = [
            ("<root><item /></root><", vec!["item", "root"]),
            ("<root><item /></root><ro", vec!["root"]),
            ("<root><item /></root><root><i", vec!["item"]),
            ("<root><item></", vec!["item"]),
            ("<known attr=\"x\"/><root attr", vec!["attr"]),
            ("<known attr=\"x\"/><root><item attr", vec!["attr"]),
        ];

        for (source, expected_labels) in cases {
            let completions = complete_xml(source, source.len());
            let labels = completions
                .iter()
                .map(|completion| completion.label.as_str())
                .collect::<Vec<_>>();
            assert_eq!(labels, expected_labels, "completion context: {source:?}");
        }

        for (source, expected) in [
            ("<root>", Some("</root>")),
            ("<root><item>", Some("</item>")),
            ("<root><item>\n", None),
            ("<root><item>text", None),
            ("<root><item></item>", None),
            ("<root />", None),
        ] {
            assert_eq!(
                auto_close_tag(source, source.len()).map(|completion| completion.insert_text),
                expected.map(str::to_owned),
                "auto-close context: {source:?}"
            );
        }
    }

    #[test]
    fn formatting_is_stable_for_many_realistic_xml_documents() {
        let documents = [
            "<root />",
            "<root><item /></root>",
            "<root><item>value</item><empty></empty></root>",
            "<root><item id=\"1\">one &amp; two</item><!-- note --></root>",
            "<?xml version=\"1.0\"?><root><item><![CDATA[a < b]]></item></root>",
            "<catalog><book><title>XML</title><author>Élodie</author></book></catalog>",
            "<Message><Header><Id>1</Id><Date>2026-09-29</Date></Header><Body /></Message>",
        ];

        for document in documents.iter().cycle().take(100) {
            let formatted = format_xml(document).expect("document should format");
            assert_eq!(
                format_xml(&formatted).unwrap(),
                formatted,
                "formatting should be idempotent for {document:?}"
            );
            assert!(formatted.ends_with('\n'));
            assert!(!formatted.contains("</root>\n</root>"));
            assert!(!formatted.contains("</Message>\n</Message>"));
        }
    }

    #[test]
    fn formats_documents_with_legacy_declared_encodings() {
        let source = "<?xml version=\"1.0\" encoding=\"iso-8859-1\" ?><root><label>Montant cautionné</label></root>";
        let formatted = format_xml(source).expect("legacy encoding declaration should be accepted");
        assert_eq!(
            formatted,
            "<?xml version=\"1.0\" encoding=\"iso-8859-1\" ?>\n<root>\n  <label>Montant cautionné</label>\n</root>\n"
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
