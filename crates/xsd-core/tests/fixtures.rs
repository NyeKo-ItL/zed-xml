use std::path::PathBuf;

use xsd_core::{
    XsdOccurs, complete_attribute_values, parse_xsd, resolve_schema_locations, validate_document,
    validate_root,
};

const SIMPLE: &str = include_str!("../../../tests/fixtures/xsd/simple.xsd");
const SCHEMA_LOCATION_XML: &str = include_str!("../../../tests/fixtures/xml/schema-location.xml");
const SEQUENCE: &str = include_str!("../../../tests/fixtures/xsd/sequence.xsd");
const INVALID_CHILD: &str = include_str!("../../../tests/fixtures/xml/invalid-child.xml");
const CHOICE: &str = include_str!("../../../tests/fixtures/xsd/choice.xsd");
const ENUM: &str = include_str!("../../../tests/fixtures/xsd/enum.xsd");
const RESTRICTIONS: &str = include_str!("../../../tests/fixtures/xsd/restrictions.xsd");
const RESTRICTED: &str = include_str!("../../../tests/fixtures/xml/restricted.xml");
const PATTERN: &str = include_str!("../../../tests/fixtures/xsd/pattern.xsd");
const PATTERN_INVALID: &str = include_str!("../../../tests/fixtures/xml/pattern-invalid.xml");

#[test]
fn parses_the_shared_simple_schema() {
    let schema = parse_xsd(SIMPLE).expect("simple fixture should parse");

    assert_eq!(
        schema.target_namespace.as_deref(),
        Some("urn:example:catalog")
    );
    assert_eq!(schema.elements.len(), 2);
    assert_eq!(schema.elements[0].name, "catalog");
    assert_eq!(schema.elements[1].occurs.min, 0);
    assert_eq!(schema.elements[1].occurs.max, None);
}

#[test]
fn resolves_the_shared_schema_location_fixture() {
    let references = resolve_schema_locations(
        SCHEMA_LOCATION_XML,
        "tests/fixtures/xml/schema-location.xml",
    )
    .unwrap();

    assert_eq!(references.len(), 1);
    assert_eq!(references[0].namespace, None);
    assert_eq!(
        references[0].path,
        PathBuf::from("tests/fixtures/xsd/simple.xsd")
    );
}

#[test]
fn validates_restrictions_from_shared_fixtures() {
    let schema = parse_xsd(RESTRICTIONS).unwrap();

    assert!(
        validate_document(RESTRICTED, &schema)
            .iter()
            .any(|diagnostic| diagnostic.message.contains("minLength"))
    );
    assert!(validate_document("<code>valid</code>", &schema).is_empty());
}

#[test]
fn validates_pattern_from_shared_fixtures() {
    let schema = parse_xsd(PATTERN).unwrap();

    assert!(
        validate_document(PATTERN_INVALID, &schema)
            .iter()
            .any(|diagnostic| diagnostic.message.contains("pattern"))
    );
    assert!(validate_document("<code>ABC</code>", &schema).is_empty());
}

#[test]
fn completes_enum_values_from_the_shared_schema() {
    let schema = parse_xsd(ENUM).unwrap();
    let completions = complete_attribute_values("<item color=\"b", 14, &schema);

    assert_eq!(completions[0].label, "blue");
}

#[test]
fn validates_choice_children_against_the_shared_schema() {
    let schema = parse_xsd(CHOICE).unwrap();

    assert!(validate_document("<message><number /></message>", &schema).is_empty());
    assert_eq!(
        schema.choices["message"],
        vec!["text".to_owned(), "number".to_owned()]
    );
}

#[test]
fn validates_sequence_children_against_the_shared_schema() {
    let schema = parse_xsd(SEQUENCE).unwrap();

    assert!(validate_document("<catalog><book /></catalog>", &schema).is_empty());
    assert_eq!(
        validate_document(INVALID_CHILD, &schema)[0].message,
        "element <magazine> not allowed in <catalog>"
    );
}

#[test]
fn validates_roots_against_the_shared_schema() {
    let schema = parse_xsd(SIMPLE).unwrap();

    assert!(validate_root("catalog", &schema).is_empty());
    assert_eq!(
        validate_root("unknown", &schema)[0].message,
        "root element <unknown> not declared in the XSD schema"
    );
    assert_eq!(XsdOccurs::default().max, Some(1));
}
