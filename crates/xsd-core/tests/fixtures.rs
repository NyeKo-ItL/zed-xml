use std::path::PathBuf;

use xsd_core::{XsdOccurs, parse_xsd, resolve_schema_locations, validate_root};

const SIMPLE: &str = include_str!("../../../tests/fixtures/xsd/simple.xsd");
const SCHEMA_LOCATION_XML: &str = include_str!("../../../tests/fixtures/xml/schema-location.xml");

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
fn validates_roots_against_the_shared_schema() {
    let schema = parse_xsd(SIMPLE).unwrap();

    assert!(validate_root("catalog", &schema).is_empty());
    assert_eq!(
        validate_root("unknown", &schema)[0].message,
        "élément racine <unknown> absent du schéma XSD"
    );
    assert_eq!(XsdOccurs::default().max, Some(1));
}
