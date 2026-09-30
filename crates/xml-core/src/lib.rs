//! XML model, parsing and formatting shared by the LSP server.

pub mod diff;
mod format;
pub mod tags;
pub mod wellformed;

pub use format::{
    EmptyElements, FormatOptions, FormattedRange, LineEnding, SplitAttributes, format_xml,
    format_xml_range, format_xml_with,
};

use std::collections::BTreeSet;

use quick_xml::{
    Reader,
    errors::{Error as QuickXmlError, IllFormedError, SyntaxError},
    events::Event,
};
use wellformed::{XmlProblemKind, check_well_formedness};

const MAX_XML_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_XML_DEPTH: usize = 512;

/// Partially parsed XML document.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct XmlDocument {
    /// Name of the root element when it could be identified.
    pub root: Option<String>,
    /// Number of elements encountered during parsing.
    pub element_count: usize,
}

/// Category of an XML diagnostic.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum XmlDiagnosticKind {
    Syntax,
    Structure,
}

/// Parse diagnostic expressed with UTF-8 offsets into the source.
#[derive(Debug, PartialEq, Eq)]
pub struct XmlDiagnostic {
    pub kind: XmlDiagnosticKind,
    pub message: String,
    pub offset: usize,
    /// End of the reported range (`offset` for a point diagnostic).
    pub end: usize,
    /// Stable identifier of the problem ([`wellformed::XmlProblemKind::id`]),
    /// published in `data.kind` for quick fixes.
    pub rule: Option<&'static str>,
}

impl XmlDiagnostic {
    fn at(kind: XmlDiagnosticKind, message: String, offset: usize) -> Self {
        Self {
            kind,
            message,
            offset,
            end: offset,
            rule: None,
        }
    }
}

impl XmlDiagnostic {
    pub fn code(&self) -> &'static str {
        match self.kind {
            XmlDiagnosticKind::Syntax => "xml-syntax",
            XmlDiagnosticKind::Structure => "xml-structure",
        }
    }
}

/// Parse result keeping the partial model even when the source is invalid.
#[derive(Debug, PartialEq, Eq)]
pub struct XmlParseResult {
    pub document: XmlDocument,
    pub diagnostics: Vec<XmlDiagnostic>,
}

/// Parses an XML source and returns the diagnostics recoverable while editing.
///
/// Tag matching, unterminated tag and unclosed reference errors reported by
/// `quick-xml` (which stops at the first error) are replaced by the located
/// problems of [`wellformed::check_well_formedness`] when it finds any;
/// duplicate attributes, unquoted values and unescaped characters always
/// come from the latter.
pub fn parse_xml(source: &str) -> XmlParseResult {
    if source.len() > MAX_XML_SOURCE_BYTES {
        return XmlParseResult {
            document: XmlDocument::default(),
            diagnostics: vec![XmlDiagnostic::at(
                XmlDiagnosticKind::Syntax,
                "XML document too large".to_owned(),
                0,
            )],
        };
    }
    let mut reader = Reader::from_str(source);
    let mut stack = Vec::new();
    let mut document = XmlDocument::default();
    // `quick-xml` diagnostics and a "covered by the tolerant check" flag.
    let mut diagnostics: Vec<(XmlDiagnostic, bool)> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                if stack.len() >= MAX_XML_DEPTH {
                    diagnostics.push((
                        XmlDiagnostic::at(
                            XmlDiagnosticKind::Structure,
                            "maximum XML depth exceeded".to_owned(),
                            reader.buffer_position() as usize,
                        ),
                        false,
                    ));
                    break;
                }
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                if stack.is_empty() {
                    if document.root.is_some() {
                        diagnostics.push((
                            XmlDiagnostic::at(
                                XmlDiagnosticKind::Structure,
                                "XML must contain a single root element".to_owned(),
                                reader.buffer_position() as usize,
                            ),
                            false,
                        ));
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
                        diagnostics.push((
                            XmlDiagnostic::at(
                                XmlDiagnosticKind::Structure,
                                "XML must contain a single root element".to_owned(),
                                reader.buffer_position() as usize,
                            ),
                            false,
                        ));
                    } else {
                        document.root = Some(name);
                    }
                }
                document.element_count += 1;
            }
            Ok(Event::End(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                let message = match stack.pop() {
                    Some(open_name) if open_name == name => continue,
                    Some(open_name) => format!("end tag </{name}> does not match </{open_name}>"),
                    None => format!("unexpected end tag </{name}>"),
                };
                diagnostics.push((
                    XmlDiagnostic::at(
                        XmlDiagnosticKind::Structure,
                        message,
                        reader.buffer_position() as usize,
                    ),
                    true,
                ));
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                let covered = matches!(
                    error,
                    QuickXmlError::IllFormed(
                        IllFormedError::MismatchedEndTag { .. }
                            | IllFormedError::UnmatchedEndTag(_)
                            | IllFormedError::UnclosedReference
                    ) | QuickXmlError::Syntax(SyntaxError::UnclosedTag)
                );
                diagnostics.push((
                    XmlDiagnostic::at(
                        XmlDiagnosticKind::Syntax,
                        format!("XML error: {error}"),
                        reader.buffer_position() as usize,
                    ),
                    covered,
                ));
                break;
            }
        }
    }

    if let Some(open_name) = stack.last() {
        diagnostics.push((
            XmlDiagnostic::at(
                XmlDiagnosticKind::Structure,
                format!("unclosed element <{open_name}>"),
                source.len(),
            ),
            true,
        ));
    }

    let problems = check_well_formedness(source);
    let replaces_covered = problems.iter().any(|problem| {
        !matches!(
            problem.kind,
            XmlProblemKind::DuplicateAttribute { .. } | XmlProblemKind::UnquotedAttributeValue
        )
    });
    let mut diagnostics = diagnostics
        .into_iter()
        .filter(|(_, covered)| !(replaces_covered && *covered))
        .map(|(diagnostic, _)| diagnostic)
        .collect::<Vec<_>>();
    diagnostics.extend(problems.into_iter().map(|problem| XmlDiagnostic {
        kind: if problem.kind.is_structural() {
            XmlDiagnosticKind::Structure
        } else {
            XmlDiagnosticKind::Syntax
        },
        rule: Some(problem.kind.id()),
        message: problem.message,
        offset: problem.range.start,
        end: problem.range.end,
    }));

    XmlParseResult {
        document,
        diagnostics,
    }
}

/// Item proposed by XML completion.
#[derive(Debug, PartialEq, Eq)]
pub struct XmlCompletion {
    pub label: String,
    pub insert_text: String,
}

/// Returns an end tag when the cursor immediately follows a start tag.
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

/// Returns local suggestions based on the document and the current context.
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
            .next_back()
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
        // `</root>` closes the root: the <child> element stays unclosed.
        let result = parse_xml("<root><child></root>");

        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message, "unclosed element <child>");
        assert_eq!(result.diagnostics[0].code(), "xml-structure");
        assert_eq!(result.diagnostics[0].rule, Some("unclosedElement"));
        assert_eq!(
            (result.diagnostics[0].offset, result.diagnostics[0].end),
            (7, 12)
        );

        let result = parse_xml("<root><child></chidl></root>");
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].message,
            "end tag </chidl> does not match </child>"
        );
        assert_eq!(result.diagnostics[0].rule, Some("mismatchedEndTag"));
    }

    #[test]
    fn reports_an_unclosed_tag_at_end_of_source() {
        let result = parse_xml("<root><child>");

        assert_eq!(result.diagnostics.len(), 2);
        assert_eq!(result.diagnostics[1].offset, "<root><".len());
        assert!(
            result.diagnostics[0]
                .message
                .contains("unclosed element <root>")
        );
        assert!(
            result.diagnostics[1]
                .message
                .contains("unclosed element <child>")
        );
    }

    #[test]
    fn keeps_quick_xml_diagnostics_not_covered_by_the_tolerant_checks() {
        let result = parse_xml("<a/><b/>");
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].rule, None);
        assert!(
            result.diagnostics[0]
                .message
                .contains("single root element")
        );

        // Duplicate attributes and unescaped characters: diagnostics added.
        let result = parse_xml("<a x=\"1\" x=\"2\">1 & 2</a>");
        let rules = result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.rule, diagnostic.code()))
            .collect::<Vec<_>>();
        assert_eq!(
            rules,
            vec![
                (Some("duplicateAttribute"), "xml-syntax"),
                (Some("unescapedCharacter"), "xml-syntax"),
            ]
        );
    }

    #[test]
    fn rejects_xml_sources_exceeding_safety_limits() {
        let source = "<root>".to_owned() + &"x".repeat(16 * 1024 * 1024);
        let result = parse_xml(&source);
        assert_eq!(result.diagnostics[0].message, "XML document too large");

        let nested = (0..513).fold(String::new(), |mut source, _| {
            source.push_str("<node>");
            source
        });
        let result = parse_xml(&nested);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("maximum XML depth"))
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
