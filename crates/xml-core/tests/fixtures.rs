use xml_core::{XmlDiagnosticKind, format_xml, parse_xml};

const VALID: &str = include_str!("../../../tests/fixtures/xml/valid.xml");
const MALFORMED: &str = include_str!("../../../tests/fixtures/xml/malformed.xml");
const NAMESPACES: &str = include_str!("../../../tests/fixtures/xml/namespaces.xml");
const CDATA: &str = include_str!("../../../tests/fixtures/xml/cdata.xml");
const MIXED_CONTENT: &str = include_str!("../../../tests/fixtures/xml/mixed-content.xml");

#[test]
fn valid_fixture_builds_a_document_and_formats_idempotently() {
    let parsed = parse_xml(VALID);

    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.document.root.as_deref(), Some("catalog"));
    let formatted = format_xml(VALID).expect("valid fixture should format");
    assert_eq!(format_xml(&formatted).unwrap(), formatted);
}

#[test]
fn malformed_fixture_reports_syntax_and_structure() {
    let parsed = parse_xml(MALFORMED);

    // `</catalog>` closes the root: <book> and <title> stay unclosed, each
    // reported on the name of its start tag.
    let unclosed = parsed
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.kind == XmlDiagnosticKind::Structure)
        .map(|diagnostic| &MALFORMED[diagnostic.offset..diagnostic.end])
        .collect::<Vec<_>>();
    assert_eq!(unclosed, vec!["book", "title"]);

    let parsed = parse_xml(&MALFORMED.replace("<title>", "<title"));
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == XmlDiagnosticKind::Syntax)
    );
}

#[test]
fn namespace_fixture_keeps_qualified_element_names() {
    let parsed = parse_xml(NAMESPACES);

    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.document.root.as_deref(), Some("x:note"));
}

#[test]
fn cdata_and_mixed_content_fixtures_survive_formatting() {
    let cdata = format_xml(CDATA).expect("CDATA fixture should format");
    let mixed = format_xml(MIXED_CONTENT).expect("mixed fixture should format");

    assert!(cdata.contains("<![CDATA[if (a < b) { return true; }]]>"));
    assert!(mixed.contains("This is <b>mixed</b> content."));
}
