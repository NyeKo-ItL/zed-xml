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
fn locates_validation_diagnostics_in_the_xml_source() {
    let schema = parse_xsd(SEQUENCE).unwrap();
    let source = "<root><magazine /></root>";
    let diagnostics = validate_document_located(source, &schema);

    assert_eq!(
        diagnostics[0].message,
        "element <magazine> not allowed in <root>"
    );
    assert_eq!(diagnostics[0].offset, source.find("<magazine").unwrap());
    assert_eq!(diagnostics[0].end, source.find(" />").unwrap());
    assert_eq!(diagnostics[0].kind, XsdDiagnosticKind::UnexpectedElement);
    assert_eq!(diagnostics[0].kind.id(), "unexpectedElement");
}

#[test]
fn locates_diagnostics_on_the_offending_occurrence() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="root"><xs:complexType><xs:sequence>
    <xs:element name="item" maxOccurs="unbounded"><xs:complexType>
      <xs:attribute name="id" use="required"/>
    </xs:complexType></xs:element>
  </xs:sequence></xs:complexType></xs:element>
</xs:schema>"#,
    )
    .unwrap();
    let source = "<root>\n  <item id=\"1\"/>\n  <item/>\n</root>";
    let diagnostics = validate_document_located(source, &schema);

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, XsdDiagnosticKind::MissingAttribute);
    assert_eq!(diagnostics[0].offset, source.rfind("<item").unwrap());
    assert_eq!(&source[diagnostics[0].offset..diagnostics[0].end], "<item");

    let unknown = validate_document_located("<?xml version=\"1.0\"?>\n<other/>", &schema);
    assert_eq!(unknown[0].kind, XsdDiagnosticKind::UnknownRoot);
    assert_eq!((unknown[0].offset, unknown[0].end), (22, 28));
    let empty = validate_document_located("", &schema);
    assert_eq!(empty[0].kind, XsdDiagnosticKind::MissingRoot);
}

#[test]
fn validates_children_declared_by_a_sequence() {
    let schema = parse_xsd(SEQUENCE).unwrap();

    assert_eq!(schema.children["root"], vec!["child"]);
    assert!(validate_document("<root><child /></root>", &schema).is_empty());
    assert_eq!(
        validate_document("<root><other /></root>", &schema)[0].message,
        "element <other> not allowed in <root>"
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
            .contains("@id required")
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
            .contains("@other not allowed")
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
            .any(|diagnostic| diagnostic.message.contains("unexpected order"))
    );
    assert!(
        validate_document("<root></root>", &schema)[0]
            .message
            .contains("<first> required")
    );
    assert!(
        validate_document("<root><first/><second/><second/><second/></root>", &schema)
            .iter()
            .any(|diagnostic| diagnostic.message.contains("too many <second> elements"))
    );
}

#[test]
fn normalizes_whitespace_restrictions_before_validation() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="Code"><xs:restriction base="xs:string">
                    <xs:whiteSpace value="collapse"/><xs:length value="3"/>
                </xs:restriction></xs:simpleType>
                <xs:element name="code" type="Code"/>
            </xs:schema>"#,
    )
    .unwrap();

    assert!(validate_document("<code> A B </code>", &schema).is_empty());
    assert!(
        validate_document("<code>A B C</code>", &schema)[0]
            .message
            .contains("length")
    );
}

#[test]
fn supports_any_elements_and_attributes() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:any minOccurs="0" maxOccurs="2" processContents="lax"/>
                </xs:sequence><xs:anyAttribute processContents="lax"/></xs:complexType></xs:element>
            </xs:schema>"#,
    )
    .unwrap();

    assert!(schema.any_children["root"]);
    assert!(schema.any_attributes["root"]);
    assert!(validate_document("<root><unknown/><other/></root>", &schema).is_empty());
    assert!(!validate_document("<root><a/><b/><c/></root>", &schema).is_empty());
    assert!(validate_document("<root arbitrary=\"1\"/>", &schema).is_empty());
}

#[test]
fn preserves_attribute_defaults_and_validates_fixed_values() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="item"><xs:complexType>
                    <xs:attribute name="kind" default="normal"/>
                    <xs:attribute name="version" fixed="1"/>
                </xs:complexType></xs:element>
            </xs:schema>"#,
    )
    .unwrap();

    assert_eq!(schema.attribute_defaults["item:kind"], "normal");
    assert_eq!(schema.attribute_fixed["item:version"], "1");
    assert!(
        validate_document("<item version=\"2\"/>", &schema)[0]
            .message
            .contains("fixed")
    );
    assert!(validate_document("<item version=\"1\"/>", &schema).is_empty());
}

#[test]
fn validates_xsi_nil_for_nillable_elements() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="allowed" nillable="true"/>
                <xs:element name="forbidden" nillable="false"/>
            </xs:schema>"#,
    )
    .unwrap();

    assert!(
        validate_document(
            r#"<allowed xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:nil="true"/>"#,
            &schema
        )
        .is_empty()
    );
    assert!(
        validate_document(
            r#"<forbidden xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:nil="true"/>"#,
            &schema
        )[0]
        .message
        .contains("not nillable")
    );
}

#[test]
fn preserves_defaults_and_validates_fixed_values() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="status" default="new"/>
                <xs:element name="version" fixed="1"/>
            </xs:schema>"#,
    )
    .unwrap();

    assert_eq!(schema.elements[0].default.as_deref(), Some("new"));
    assert_eq!(schema.elements[1].fixed.as_deref(), Some("1"));
    assert!(
        validate_document("<version>2</version>", &schema)[0]
            .message
            .contains("fixed")
    );
    assert!(validate_document("<version>1</version>", &schema).is_empty());
}

#[test]
fn parses_complex_and_simple_content_extensions() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="complex"><xs:complexType><xs:complexContent>
                    <xs:extension base="Base"/>
                </xs:complexContent></xs:complexType></xs:element>
                <xs:element name="simple"><xs:complexType><xs:simpleContent>
                    <xs:extension base="xs:string"/>
                </xs:simpleContent></xs:complexType></xs:element>
            </xs:schema>"#,
    )
    .unwrap();

    assert_eq!(schema.complex_extensions["complex"], "Base");
    assert_eq!(schema.simple_extensions["simple"], "xs:string");
    assert_eq!(schema.elements[1].type_name.as_deref(), Some("xs:string"));
}

#[test]
fn resolves_attribute_references() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attribute name="id"/>
                <xs:element name="item"><xs:complexType>
                    <xs:attribute ref="id"/>
                </xs:complexType></xs:element>
            </xs:schema>"#,
    )
    .unwrap();

    assert_eq!(schema.attributes["item"], vec!["id"]);
    assert!(validate_document("<item id=\"1\"/>", &schema).is_empty());
}

#[test]
fn resolves_named_model_groups() {
    let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:group name="common"><xs:sequence><xs:element name="id"/></xs:sequence></xs:group>
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:group ref="common"/>
                </xs:sequence></xs:complexType></xs:element>
            </xs:schema>"#,
        )
        .unwrap();

    assert_eq!(schema.model_groups["common"], vec!["id"]);
    assert_eq!(schema.children["root"], vec!["id"]);
    assert!(validate_document("<root><id/></root>", &schema).is_empty());
}

#[test]
fn resolves_named_attribute_groups() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attributeGroup name="common"><xs:attribute name="id"/></xs:attributeGroup>
                <xs:element name="item"><xs:complexType>
                    <xs:attributeGroup ref="common"/>
                </xs:complexType></xs:element>
            </xs:schema>"#,
    )
    .unwrap();

    assert_eq!(schema.attribute_groups["common"], vec!["id"]);
    assert_eq!(schema.attributes["item"], vec!["id"]);
    assert!(validate_document("<item id=\"1\"/>", &schema).is_empty());
}

#[test]
fn preserves_schema_namespace_form_defaults() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                targetNamespace="urn:test" elementFormDefault="qualified"
                attributeFormDefault="unqualified">
                <xs:element name="root" form="qualified"/>
            </xs:schema>"#,
    )
    .unwrap();

    assert_eq!(schema.target_namespace.as_deref(), Some("urn:test"));
    assert_eq!(schema.element_form_default.as_deref(), Some("qualified"));
    assert_eq!(
        schema.attribute_form_default.as_deref(),
        Some("unqualified")
    );
    assert_eq!(schema.elements[0].form.as_deref(), Some("qualified"));
}

#[test]
fn resolves_element_references_in_model_groups() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="shared"/>
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:element ref="shared"/>
                </xs:sequence></xs:complexType></xs:element>
            </xs:schema>"#,
    )
    .unwrap();

    assert_eq!(schema.children["root"], vec!["shared"]);
    assert!(validate_document("<root><shared/></root>", &schema).is_empty());
    assert_eq!(complete_elements("<root><s", 9, &schema)[0].label, "shared");
}

#[test]
fn supports_all_children_without_order_constraints() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root"><xs:complexType><xs:all>
                    <xs:element name="first"/><xs:element name="second" minOccurs="0"/>
                </xs:all></xs:complexType></xs:element>
            </xs:schema>"#,
    )
    .unwrap();

    assert_eq!(schema.alls["root"], vec!["first", "second"]);
    assert!(validate_document("<root><second/><first/></root>", &schema).is_empty());
    assert!(
        validate_document("<root></root>", &schema)[0]
            .message
            .contains("<first> required")
    );
    assert_eq!(complete_elements("<root><", 8, &schema).len(), 2);
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
            .contains("requires at least 3 (minLength)")
    );
    assert!(
        validate_document("<code>abcdef</code>", &schema)[0]
            .message
            .contains("requires at most 5 (maxLength)")
    );
    assert!(validate_document("<code>valid</code>", &schema).is_empty());
}

fn messages(source: &str, schema: &XsdSchema) -> Vec<String> {
    validate_document(source, schema)
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

const TYPED: &str = r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
            xmlns:t="urn:typed" targetNamespace="urn:typed" elementFormDefault="qualified">
        <xs:element name="root">
            <xs:complexType>
                <xs:sequence>
                    <xs:element name="date" type="xs:date" minOccurs="0" maxOccurs="unbounded"/>
                    <xs:element name="count" type="xs:unsignedByte" minOccurs="0" default="7"/>
                    <xs:element name="ratio" type="xs:decimal" minOccurs="0" fixed="1.5"/>
                    <xs:element name="name" type="xs:QName" minOccurs="0"/>
                    <xs:element name="any" minOccurs="0"/>
                    <xs:element name="box" type="t:Box" minOccurs="0"/>
                    <xs:element name="skip" minOccurs="0">
                        <xs:complexType><xs:sequence>
                            <xs:any processContents="skip" namespace="##any"/>
                        </xs:sequence></xs:complexType>
                    </xs:element>
                </xs:sequence>
                <xs:attribute name="level" type="t:Level"/>
                <xs:attribute name="stamp" type="xs:dateTime"/>
            </xs:complexType>
        </xs:element>
        <xs:element name="date" type="xs:date"/>
        <xs:complexType name="Box"><xs:sequence><xs:element name="date" type="xs:date"/></xs:sequence></xs:complexType>
        <xs:simpleType name="Level">
            <xs:restriction base="xs:int"><xs:minInclusive value="1"/><xs:maxInclusive value="5"/></xs:restriction>
        </xs:simpleType>
    </xs:schema>"###;

fn typed(body: &str) -> String {
    format!(
        "<t:root xmlns:t=\"urn:typed\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">{body}</t:root>"
    )
}

#[test]
fn validates_text_contents_against_builtin_types() {
    let schema = parse_xsd(TYPED).unwrap();
    assert_eq!(
        messages(&typed("<t:date>2026-09-30</t:date>"), &schema),
        Vec::<String>::new()
    );
    assert_eq!(
        messages(&typed("<t:date>2024-13-01</t:date>"), &schema),
        vec!["content of <t:date>: '2024-13-01' is not a valid xs:date: month must be 01-12"]
    );
    assert_eq!(
        messages(&typed("<t:count>256</t:count>"), &schema),
        vec![
            "content of <t:count>: '256' is not a valid xs:unsignedByte: the value must be at most 255"
        ]
    );
    // Whitespace is collapsed, entity and character references and CDATA
    // sections are part of the value.
    assert!(messages(&typed("<t:count>\n 2&#53; </t:count>"), &schema).is_empty());
    assert!(messages(&typed("<t:date><![CDATA[2026-09-30]]></t:date>"), &schema).is_empty());
    assert!(!messages(&typed("<t:date>2026-09-30&amp;</t:date>"), &schema).is_empty());
}

#[test]
fn checks_empty_elements_defaults_and_fixed_values() {
    let schema = parse_xsd(TYPED).unwrap();
    assert_eq!(
        messages(&typed("<t:date/>"), &schema),
        vec![
            "content of <t:date>: '' is not a valid xs:date: expected the format YYYY-MM-DD with an optional time zone"
        ]
    );
    // An empty element takes its default or fixed value.
    assert!(messages(&typed("<t:count/><t:ratio></t:ratio>"), &schema).is_empty());
    // Fixed values are compared in the value space.
    assert!(messages(&typed("<t:ratio>1.50</t:ratio>"), &schema).is_empty());
    assert_eq!(
        messages(&typed("<t:ratio>2</t:ratio>"), &schema),
        vec!["content of <t:ratio> differs from the fixed value '1.5'"]
    );
    // xsi:nil elements are not checked.
    assert!(messages(&typed("<t:date xsi:nil=\"true\"/>"), &schema).len() == 1);
}

#[test]
fn resolves_local_declarations_xsi_type_and_qname_prefixes() {
    let schema = parse_xsd(TYPED).unwrap();
    assert!(
        messages(
            &typed("<t:box><t:date>2026-01-01</t:date></t:box>"),
            &schema
        )
        .is_empty()
    );
    assert!(!messages(&typed("<t:box><t:date>soon</t:date></t:box>"), &schema).is_empty());
    assert!(messages(&typed("<t:name>t:root</t:name>"), &schema).is_empty());
    assert_eq!(
        messages(&typed("<t:name>u:root</t:name>"), &schema),
        vec![
            "content of <t:name>: 'u:root' is not a valid xs:QName: the prefix 'u' is not bound to a namespace"
        ]
    );
    assert!(messages(&typed("<t:name xmlns:u=\"urn:u\">u:root</t:name>"), &schema).is_empty());
    let xs = "xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"";
    assert!(
        messages(
            &typed(&format!("<t:any {xs} xsi:type=\"xs:int\">12</t:any>")),
            &schema
        )
        .is_empty()
    );
    assert_eq!(
        messages(
            &typed(&format!("<t:any {xs} xsi:type=\"xs:int\">twelve</t:any>")),
            &schema
        ),
        vec![
            "content of <t:any>: 'twelve' is not a valid xs:int: expected an integer such as '-12' (digits only)"
        ]
    );
}

#[test]
fn checks_xsi_type_derivation_blocks_and_abstract_types() {
    let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="Base"><xs:sequence><xs:element name="a" type="xs:string"/></xs:sequence></xs:complexType>
  <xs:complexType name="Ext"><xs:complexContent><xs:extension base="Base"><xs:sequence><xs:element name="b" type="xs:string"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType>
  <xs:complexType name="Other"><xs:sequence><xs:element name="a" type="xs:string"/></xs:sequence></xs:complexType>
  <xs:complexType name="Shape" abstract="true"><xs:sequence><xs:element name="a" type="xs:string"/></xs:sequence></xs:complexType>
  <xs:complexType name="Circle"><xs:complexContent><xs:extension base="Shape"><xs:sequence/></xs:extension></xs:complexContent></xs:complexType>
  <xs:element name="open" type="Base"/>
  <xs:element name="closed" type="Base" block="extension"/>
  <xs:element name="shape" type="Shape"/>
</xs:schema>"#,
        )
        .unwrap();
    let x = r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#;
    let run = |element: &str, xsi: &str, content: &str| {
        let attribute = if xsi.is_empty() {
            String::new()
        } else {
            format!(r#" xsi:type="{xsi}""#)
        };
        validate_document(
            &format!("<{element} {x}{attribute}>{content}</{element}>"),
            &schema,
        )
    };
    assert!(run("open", "Ext", "<a>1</a><b>2</b>").is_empty());
    assert!(run("open", "Base", "<a>1</a>").is_empty());
    let kinds = |diagnostics: Vec<XsdDiagnostic>| {
        diagnostics
            .into_iter()
            .filter(|d| d.kind == XsdDiagnosticKind::InvalidXsiType)
            .map(|d| d.message)
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds(run("open", "Other", "<a>1</a>")).len(), 1);
    assert_eq!(kinds(run("open", "Missing", "<a>1</a>")).len(), 1);
    assert_eq!(kinds(run("closed", "Ext", "<a>1</a><b>2</b>")).len(), 1);
    assert_eq!(kinds(run("shape", "", "<a>1</a>")).len(), 1);
    assert!(kinds(run("shape", "Circle", "<a>1</a>")).is_empty());
    assert_eq!(kinds(run("shape", "Shape", "<a>1</a>")).len(), 1);
}

#[test]
fn validates_attribute_values_at_their_location() {
    let schema = parse_xsd(TYPED).unwrap();
    let source = "<t:root xmlns:t=\"urn:typed\"\r\n  level=\"9\" stamp=\"été\"/>";
    let diagnostics = validate_document_located(source, &schema);
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert_eq!(
        diagnostics[0].kind,
        XsdDiagnosticKind::InvalidAttributeValue
    );
    assert_eq!(&source[diagnostics[0].offset..diagnostics[0].end], "9");
    assert_eq!(
        diagnostics[0].message,
        "attribute @level of <t:root>: '9' must be at most 5 (maxInclusive of t:Level)"
    );
    assert_eq!(&source[diagnostics[1].offset..diagnostics[1].end], "été");
    assert!(
        diagnostics[1]
            .message
            .contains("is not a valid xs:dateTime")
    );
    assert!(validate_document("<t:root xmlns:t=\"urn:typed\" level=\" 3 \"/>", &schema).is_empty());
}

#[test]
fn reports_enumerations_with_their_own_kind() {
    let schema = parse_xsd(ENUMERATION).unwrap();
    let diagnostics = validate_document("<item>green</item>", &schema);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, XsdDiagnosticKind::InvalidEnumeration);
    assert_eq!(diagnostics[0].kind.id(), "invalidEnumeration");
    assert_eq!(
        XsdDiagnosticKind::InvalidAttributeValue.id(),
        "invalidAttributeValue"
    );
}

#[test]
fn checks_content_kinds() {
    let schema = parse_xsd(TYPED).unwrap();
    assert_eq!(
        messages(
            &typed("<t:date><t:date>2026-01-01</t:date></t:date>"),
            &schema
        ),
        vec!["element <t:date> has the simple type xs:date and cannot contain elements"]
    );
    assert_eq!(
        messages(
            &typed("<t:box>text<t:date>2026-01-01</t:date></t:box>"),
            &schema
        ),
        vec!["element <t:box> cannot contain text (its type has element-only content)"]
    );
    // Elements matched by a processContents="skip" wildcard are not
    // checked.
    assert!(messages(&typed("<t:skip><t:date>nope</t:date></t:skip>"), &schema).is_empty());
}

#[test]
fn checks_the_expanded_name_of_the_root() {
    let schema = parse_xsd(TYPED).unwrap();
    assert!(validate_document("<t:root xmlns:t=\"urn:typed\"/>", &schema).is_empty());
    assert!(validate_document("<root xmlns=\"urn:typed\"/>", &schema).is_empty());
    assert_eq!(
        messages("<t:root xmlns:t=\"urn:other\"/>", &schema),
        vec!["root element <t:root> not declared in the XSD schema"]
    );
}

const IDENTITY: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
            xmlns:k="urn:k" targetNamespace="urn:k" elementFormDefault="qualified">
        <xs:element name="catalog">
            <xs:complexType>
                <xs:sequence>
                    <xs:element name="item" minOccurs="0" maxOccurs="unbounded">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="code" type="xs:decimal" minOccurs="0"/>
                            </xs:sequence>
                            <xs:attribute name="id" type="xs:ID"/>
                            <xs:attribute name="sku" type="xs:string"/>
                            <xs:attribute name="kind" type="xs:string" default="plain"/>
                        </xs:complexType>
                    </xs:element>
                    <xs:element name="link" minOccurs="0" maxOccurs="unbounded">
                        <xs:complexType>
                            <xs:attribute name="to" type="xs:IDREF"/>
                            <xs:attribute name="all" type="xs:IDREFS"/>
                            <xs:attribute name="code" type="xs:decimal"/>
                        </xs:complexType>
                    </xs:element>
                </xs:sequence>
            </xs:complexType>
            <xs:key name="itemCode">
                <xs:selector xpath="k:item"/>
                <xs:field xpath="k:code"/>
            </xs:key>
            <xs:unique name="itemSku">
                <xs:selector xpath=".//k:item"/>
                <xs:field xpath="@sku"/>
                <xs:field xpath="@kind"/>
            </xs:unique>
            <xs:keyref name="linkCode" refer="k:itemCode">
                <xs:selector xpath="k:link"/>
                <xs:field xpath="@code"/>
            </xs:keyref>
        </xs:element>
    </xs:schema>"#;

fn catalog(body: &str) -> String {
    format!("<k:catalog xmlns:k=\"urn:k\">{body}</k:catalog>")
}

#[test]
fn checks_ids_and_id_references() {
    let schema = parse_xsd(IDENTITY).unwrap();
    let source = catalog(
        "<k:item id=\"a\"><k:code>1</k:code></k:item>\r\n<k:item id=\"é\"><k:code>2</k:code></k:item>\
             <k:item id=\"a\"><k:code>3</k:code></k:item><k:link to=\"é\" all=\" a  b \"/>",
    );
    let diagnostics = validate_document_located(&source, &schema);
    let found = diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.kind, &source[diagnostic.offset..diagnostic.end]))
        .collect::<Vec<_>>();
    assert_eq!(
        found,
        [
            (XsdDiagnosticKind::DuplicateId, "a"),
            (XsdDiagnosticKind::UnknownIdref, "b"),
        ],
        "{diagnostics:?}"
    );
    assert_eq!(diagnostics[1].message, "no element has the ID 'b'");
    let links = identity_links(&source, &schema)
        .into_iter()
        .map(|link| (&source[link.reference.clone()], link.target.start))
        .collect::<Vec<_>>();
    let first = source.find("\"a\"").unwrap() + 1;
    let accented = source.find("\"é\"").unwrap() + 1;
    assert_eq!(links, [("é", accented), ("a", first)]);
}

#[test]
fn checks_keys_uniques_and_keyrefs_in_the_value_space() {
    let schema = parse_xsd(IDENTITY).unwrap();
    assert!(schema.problems.is_empty(), "{:?}", schema.problems);
    let valid = catalog(
        "<k:item sku=\"x\"><k:code>1</k:code></k:item><k:item sku=\"x\" kind=\"gift\"><k:code>2</k:code></k:item>\
             <k:link code=\"1.0\"/><k:link code=\"02\"/>",
    );
    assert_eq!(messages(&valid, &schema), Vec::<String>::new());
    // `1.0` designates the key `1`: the link targets its value.
    let links = identity_links(&valid, &schema);
    assert_eq!(links.len(), 2);
    assert_eq!(&valid[links[0].reference.clone()], "1.0");
    assert_eq!(&valid[links[0].target.clone()], "1");

    let source = catalog(
        "<k:item sku=\"x\"><k:code>1</k:code></k:item><k:item sku=\"x\"><k:code> 1.00 </k:code></k:item>\
             <k:item/><k:link code=\"7\"/>",
    );
    let diagnostics = validate_document_located(&source, &schema);
    let found = diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.kind, diagnostic.message.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        found,
        [
            (
                XsdDiagnosticKind::MissingKeyField,
                "the field 'k:code' of the key 'itemCode' is missing in <k:item>"
            ),
            (
                XsdDiagnosticKind::DuplicateKey,
                "duplicate value '1.00' of the key 'itemCode'"
            ),
            (
                XsdDiagnosticKind::DuplicateKey,
                "duplicate value ('x', 'plain') of the unique constraint 'itemSku'"
            ),
            (
                XsdDiagnosticKind::UnknownKeyref,
                "the value '7' of the keyref 'linkCode' matches no 'itemCode' in scope"
            ),
        ]
    );
    assert_eq!(&source[diagnostics[1].offset..diagnostics[1].end], " 1.00 ");
    assert_eq!(XsdDiagnosticKind::UnknownKeyref.id(), "unknownKeyref");
}

#[test]
fn reports_invalid_identity_constraints_as_schema_problems() {
    let problems = |constraints: &str| {
        let source = format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="root">
                        <xs:complexType><xs:attribute name="a"/></xs:complexType>
                        {constraints}
                    </xs:element>
                </xs:schema>"#
        );
        merge_schemas([parse_xsd(&source).unwrap()]).problems
    };
    let key = r#"<xs:key name="k"><xs:selector xpath="."/><xs:field xpath="@a"/></xs:key>"#;
    assert!(problems(key).is_empty());
    assert_eq!(
        problems(r#"<xs:key name="k"><xs:selector xpath="a//b"/><xs:field xpath="@a"/></xs:key>"#),
        ["xs:key 'k': invalid xpath 'a//b': '//' is only allowed at the start, as './/'"]
    );
    assert_eq!(
        problems(&format!("{key}{key}")),
        ["the identity constraint name 'k' is declared twice"]
    );
    assert_eq!(
        problems(
            r#"<xs:keyref name="r" refer="missing"><xs:selector xpath="."/><xs:field xpath="@a"/></xs:keyref>"#
        ),
        ["the keyref 'r' refers to 'missing', which is not a declared key or unique constraint"]
    );
    assert_eq!(
        problems(&format!(
            r#"{key}<xs:keyref name="r" refer="k"><xs:selector xpath="."/><xs:field xpath="@a"/><xs:field xpath="@a"/></xs:keyref>"#
        )),
        ["the keyref 'r' has 2 field(s) but the key 'k' it refers to has 1"]
    );
    // A constraint without selector (structure checked by `schema_check`).
    assert!(
        problems(r#"<xs:unique name="u"><xs:field xpath="@a"/></xs:unique>"#)
            .iter()
            .any(|message| message.contains("xs:selector")),
    );
    // The schema stays usable.
    let source = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root"><xs:complexType><xs:attribute name="a" type="xs:int"/></xs:complexType>
            <xs:key name="k"><xs:selector xpath="/"/><xs:field xpath="@a"/></xs:key></xs:element>
        </xs:schema>"#;
    let schema = parse_xsd(source).unwrap();
    assert_eq!(schema.problems.len(), 1);
    assert_eq!(messages("<root a=\"x\"/>", &schema).len(), 1);
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
fn parses_and_validates_list_and_union_types() {
    let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="Numbers"><xs:list itemType="xs:integer"/></xs:simpleType>
                <xs:simpleType name="Value"><xs:union memberTypes="xs:integer xs:boolean"/></xs:simpleType>
                <xs:element name="numbers" type="Numbers"/>
                <xs:element name="value" type="Value"/>
            </xs:schema>"#,
        )
        .unwrap();

    assert_eq!(schema.lists["Numbers"], "xs:integer");
    assert_eq!(schema.unions["Value"], vec!["xs:integer", "xs:boolean"]);
    assert!(validate_document("<numbers>1 2 3</numbers>", &schema).is_empty());
    assert!(
        validate_document("<numbers>1 nope</numbers>", &schema)
            .iter()
            .any(|diagnostic| diagnostic.message.contains("integer"))
    );
    assert!(validate_document("<value>false</value>", &schema).is_empty());
    assert!(
        validate_document("<value>nope</value>", &schema)[0]
            .message
            .contains("not valid for any member type of Value (xs:integer, xs:boolean)")
    );
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
        validate_document("<amount>1.234</amount>", &schema)
            .iter()
            .any(|diagnostic| diagnostic.message.contains("fractionDigits"))
    );
    assert!(
        validate_document("<amount>12.345</amount>", &schema)
            .iter()
            .any(|diagnostic| diagnostic.message.contains("totalDigits"))
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
            .contains("'0' must be at least 1 (minInclusive of Score)")
    );
    assert!(
        validate_document("<score>10</score>", &schema)[0]
            .message
            .contains("maxExclusive")
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
            .contains("requires exactly 3 (length)")
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
            .contains("pattern")
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
            .any(|diagnostic| diagnostic.message.contains("minLength"))
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
fn supports_substitution_groups_in_validation_and_completion() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="head"/>
                <xs:element name="member" substitutionGroup="head"/>
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:element ref="head"/>
                </xs:sequence></xs:complexType></xs:element>
            </xs:schema>"#,
    )
    .unwrap();
    let source = "<root><";
    assert!(
        complete_elements(source, source.len(), &schema)
            .iter()
            .any(|completion| completion.label == "member")
    );
    assert!(validate_document("<root><member /></root>", &schema).is_empty());
}

#[test]
fn completion_respects_sequence_order_and_max_occurs() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:element name="first"/>
                    <xs:element name="second"/>
                </xs:sequence></xs:complexType></xs:element>
            </xs:schema>"#,
    )
    .unwrap();
    let source = "<root><first /><";
    assert_eq!(
        complete_elements(source, source.len(), &schema),
        vec![XsdCompletion {
            label: "second".to_owned(),
            insert_text: "second".to_owned(),
        }]
    );
}

#[test]
fn merges_components_from_multiple_schemas() {
    let first = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"/></xs:schema>"#,
        )
        .unwrap();
    let second = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="child"/></xs:schema>"#,
        )
        .unwrap();

    let merged = merge_schemas([first, second]);
    assert_eq!(
        merged
            .elements
            .iter()
            .map(|element| element.name.as_str())
            .collect::<Vec<_>>(),
        vec!["root", "child"]
    );
}

#[test]
fn resolves_include_and_import_dependencies() {
    let references = resolve_schema_dependencies(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:include schemaLocation="common/base.xsd"/>
                <xs:import namespace="urn:other" schemaLocation="../other.xsd"/>
            </xs:schema>"#,
        "workspace/schema/root.xsd",
    )
    .unwrap();

    assert_eq!(references.len(), 2);
    assert_eq!(references[0].namespace, None);
    assert_eq!(
        references[0].path,
        PathBuf::from("workspace/schema/common/base.xsd")
    );
    assert_eq!(references[1].namespace.as_deref(), Some("urn:other"));
    assert_eq!(references[1].path, PathBuf::from("workspace/other.xsd"));
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
fn rejects_oversized_xsd_sources() {
    let source = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">".to_owned()
        + &"x".repeat(16 * 1024 * 1024);
    assert_eq!(parse_xsd(&source), Err("XSD schema too large".to_owned()));
}

#[test]
fn rejects_a_document_that_is_not_a_schema_but_accepts_one_without_elements() {
    assert!(parse_xsd("<html/>").is_err());
    // Types only, as included by other schemas.
    let types = parse_xsd(
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:simpleType name=\"t\"><xs:restriction base=\"xs:string\"/></xs:simpleType></xs:schema>",
        )
        .unwrap();
    assert!(types.elements.is_empty());
    assert_eq!(types.models.len(), 1);
}

#[test]
fn decodes_percent_encoded_and_file_uri_schema_locations() {
    let directory = std::env::temp_dir().join(format!("xsd-core-locations {}", std::process::id()));
    std::fs::create_dir_all(directory.join("my schemas")).unwrap();
    std::fs::write(directory.join("my schemas/a b.xsd"), "<x/>").unwrap();
    let document = directory.join("doc.xml");
    let source = r#"<root xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
            xsi:noNamespaceSchemaLocation="my%20schemas/a%20b.xsd"/>"#;
    let references = resolve_schema_locations(source, &document).unwrap();
    assert_eq!(references[0].path, directory.join("my schemas/a b.xsd"));
    let _ = std::fs::remove_dir_all(&directory);

    assert_eq!(
        resolve_location(Path::new("/base"), "file:///tmp/a%20b.xsd#frag"),
        PathBuf::from("/tmp/a b.xsd")
    );
    assert_eq!(
        resolve_location(Path::new("/base"), "file://localhost/tmp/c.xsd"),
        PathBuf::from("/tmp/c.xsd")
    );
    assert_eq!(
        resolve_location(Path::new("/base"), "./sub/../missing%zz.xsd"),
        PathBuf::from("/base/missing%zz.xsd")
    );
    let share = resolve_location(Path::new("/base"), "file://server/share/s.xsd");
    assert_eq!(share, PathBuf::from("//server/share/s.xsd"));
    assert!(is_remote_location(&share));
    assert!(is_remote_location(Path::new(r"\\server\share\s.xsd")));
    assert!(!is_remote_location(Path::new("/tmp/s.xsd")));
    assert_eq!(
        resolve_location(Path::new("/base"), "file://C:/s.xsd"),
        PathBuf::from("C:/s.xsd")
    );
    let remote = resolve_location(Path::new("/base"), "https://example.com/s.xsd");
    assert_eq!(remote, PathBuf::from("https://example.com/s.xsd"));
    assert!(is_remote_location(&remote));
    assert!(is_remote_location(Path::new("urn:x:y")));
    assert!(!is_remote_location(Path::new("/base/s.xsd")));
    assert!(!is_remote_location(Path::new("C:/base/s.xsd")));
}

#[test]
fn consults_the_location_resolver_first() {
    let resolver = |request: &SchemaLocation<'_>| -> Option<PathBuf> {
        match (request.kind, request.namespace, request.location) {
            (SchemaLocationKind::SchemaLocation, Some("urn:a"), _) => {
                Some(PathBuf::from("/catalog/a.xsd"))
            }
            (SchemaLocationKind::Import, Some("urn:b"), None) => {
                Some(PathBuf::from("/catalog/b.xsd"))
            }
            (SchemaLocationKind::Include, None, Some("http://x/inc.xsd")) => {
                Some(PathBuf::from("/catalog/inc.xsd"))
            }
            _ => None,
        }
    };
    let references = resolve_schema_locations_with(
        r#"<r xsi:schemaLocation="urn:a http://x/a.xsd urn:c c.xsd"/>"#,
        "/docs/d.xml",
        &resolver,
    )
    .unwrap();
    assert_eq!(
        references,
        vec![
            SchemaReference {
                namespace: Some("urn:a".to_owned()),
                path: PathBuf::from("/catalog/a.xsd"),
                kind: SchemaLocationKind::SchemaLocation,
            },
            SchemaReference {
                namespace: Some("urn:c".to_owned()),
                path: PathBuf::from("/docs/c.xsd"),
                kind: SchemaLocationKind::SchemaLocation,
            },
        ]
    );
    let dependencies = resolve_schema_dependencies_with(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:import namespace="urn:b"/>
                <xs:import namespace="urn:unknown"/>
                <xs:include schemaLocation="http://x/inc.xsd"/>
            </xs:schema>"#,
        "/schemas/s.xsd",
        &resolver,
    )
    .unwrap();
    assert_eq!(
        dependencies
            .iter()
            .map(|reference| reference.path.clone())
            .collect::<Vec<_>>(),
        vec![
            PathBuf::from("/catalog/b.xsd"),
            PathBuf::from("/catalog/inc.xsd")
        ]
    );
}

#[test]
fn handles_deeply_nested_schemas_and_documents() {
    const DEPTH: usize = 100_000;
    let schema = format!(
        "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:element name=\"a\">{}{}</xs:element></xs:schema>",
        "<xs:complexType><xs:sequence><xs:element name=\"a\">".repeat(DEPTH),
        "</xs:element></xs:sequence></xs:complexType>".repeat(DEPTH)
    );
    let error = parse_xsd(&schema).unwrap_err();
    assert!(error.contains("nested more than"), "{error}");
    let _ = resolve_schema_dependencies(&schema, "/tmp/deep.xsd");
    // A recursive schema validating a deeply nested instance.
    let schema = parse_xsd(
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:element name=\"a\"><xs:complexType><xs:sequence><xs:element ref=\"a\" minOccurs=\"0\"/></xs:sequence><xs:attribute name=\"x\"/></xs:complexType></xs:element></xs:schema>",
        )
        .unwrap();
    let document = format!("{}{}", "<a>".repeat(DEPTH), "</a>".repeat(DEPTH));
    validate_document_located(&document, &schema);
    let middle = DEPTH * 3;
    complete_elements(&document, middle, &schema);
    complete_attributes(&document, middle - 1, &schema);
    let _ = resolve_schema_locations(&document, "/tmp/deep.xml");
    let open = "<a x='1'>".repeat(DEPTH);
    validate_document_located(&open, &schema);
    complete_elements(&open, open.len(), &schema);
}

#[test]
fn checks_the_content_model_of_each_type_not_of_each_name() {
    // `note` is required in `a` and optional in `b`: the declarations
    // sharing a name must not be mixed up.
    let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="note" type="xs:string"/>
  <xs:element name="root"><xs:complexType><xs:sequence>
    <xs:element name="a"><xs:complexType><xs:sequence><xs:element ref="note"/></xs:sequence></xs:complexType></xs:element>
    <xs:element name="b"><xs:complexType><xs:sequence><xs:element ref="note" minOccurs="0"/><xs:element name="z"/></xs:sequence></xs:complexType></xs:element>
  </xs:sequence></xs:complexType></xs:element>
</xs:schema>"#,
        )
        .unwrap();
    assert!(validate_document("<root><a><note>x</note></a><b><z/></b></root>", &schema).is_empty());
    let missing = messages("<root><a/><b><z/></b></root>", &schema);
    assert_eq!(missing, ["element <note> required in <a>"]);
}

#[test]
fn reports_a_misplaced_or_repeated_child_once_on_the_child() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="root"><xs:complexType><xs:sequence>
    <xs:element name="a"/><xs:element name="b" maxOccurs="2"/>
  </xs:sequence></xs:complexType></xs:element>
</xs:schema>"#,
    )
    .unwrap();
    let source = "<root><b/><a/><a/></root>";
    let diagnostics = validate_document_located(source, &schema);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].kind, XsdDiagnosticKind::UnexpectedOrder);
    assert_eq!(&source[diagnostics[0].offset..diagnostics[0].end], "<b");

    let source = "<root><a/><b/><b/><b/></root>";
    let diagnostics = validate_document_located(source, &schema);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].kind, XsdDiagnosticKind::TooManyElements);
    assert_eq!(diagnostics[0].offset, source.rfind("<b").unwrap());
}

#[test]
fn checks_the_content_of_empty_elements() {
    let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="root"><xs:complexType><xs:sequence><xs:element name="a"/></xs:sequence></xs:complexType></xs:element>
</xs:schema>"#,
        )
        .unwrap();
    assert_eq!(
        messages("<root/>", &schema),
        ["element <a> required in <root>"]
    );
    assert!(validate_document("<root><a/></root>", &schema).is_empty());
}

#[test]
fn matches_children_by_expanded_name() {
    // Qualified local elements: an unqualified child is a different name.
    let schema = parse_xsd(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:t" elementFormDefault="qualified">
  <xs:element name="root"><xs:complexType><xs:sequence><xs:element name="a"/></xs:sequence></xs:complexType></xs:element>
</xs:schema>"#,
        )
        .unwrap();
    assert!(validate_document(r#"<root xmlns="urn:t"><a/></root>"#, &schema).is_empty());
    let unqualified = messages(r#"<t:root xmlns:t="urn:t"><a/></t:root>"#, &schema);
    assert_eq!(
        unqualified[0],
        "element <a> not allowed in <t:root>: it is in the wrong namespace ({urn:t}a expected)"
    );
}

#[test]
fn a_redefined_type_extends_the_definition_it_replaces() {
    let directory = std::env::temp_dir().join(format!("xsd-redefine {}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
            directory.join("base.xsd"),
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:complexType name="t"><xs:sequence><xs:element name="r"/></xs:sequence></xs:complexType></xs:schema>"#,
        )
        .unwrap();
    let main = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:redefine schemaLocation="base.xsd"><xs:complexType name="t"><xs:complexContent><xs:extension base="t"><xs:sequence><xs:element name="c"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType></xs:redefine>
  <xs:element name="root" type="t"/>
</xs:schema>"#;
    std::fs::write(directory.join("main.xsd"), main).unwrap();
    let references = resolve_schema_dependencies(main, directory.join("main.xsd")).unwrap();
    assert_eq!(references.len(), 1);
    let base = std::fs::read_to_string(directory.join("base.xsd")).unwrap();
    let schema = merge_schemas([parse_xsd(main).unwrap(), parse_xsd(&base).unwrap()]);
    assert!(validate_document("<root><r/><c/></root>", &schema).is_empty());
    assert!(!validate_document("<root><c/></root>", &schema).is_empty());
    std::fs::remove_dir_all(directory).ok();
}

/// Schema problems of `body` wrapped in an `xs:schema`.
fn schema_problems(body: &str) -> Vec<String> {
    let source =
        format!(r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{body}</xs:schema>"#);
    merge_schemas([parse_xsd(&source).unwrap()]).problems
}

#[test]
fn checks_final_and_the_kind_of_base_of_derivations() {
    let derived = |base: &str, derivation: &str| {
        format!(
            r#"{base}<xs:complexType name="d"><xs:complexContent><xs:{derivation} base="b"><xs:sequence/></xs:{derivation}></xs:complexContent></xs:complexType>"#
        )
    };
    let plain = r#"<xs:complexType name="b"><xs:sequence/></xs:complexType>"#;
    assert!(schema_problems(&derived(plain, "extension")).is_empty());
    let final_extension =
        r#"<xs:complexType name="b" final="extension"><xs:sequence/></xs:complexType>"#;
    assert_eq!(
        schema_problems(&derived(final_extension, "extension")).len(),
        1
    );
    assert!(schema_problems(&derived(final_extension, "restriction")).is_empty());
    let source = format!(
        r##"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" finalDefault="#all">{}</xs:schema>"##,
        derived(plain, "restriction")
    );
    assert_eq!(
        merge_schemas([parse_xsd(&source).unwrap()]).problems.len(),
        1
    );
    // A complexContent derivation of a simple type.
    let simple = r#"<xs:simpleType name="b"><xs:restriction base="xs:string"/></xs:simpleType>"#;
    assert_eq!(schema_problems(&derived(simple, "extension")).len(), 1);
    // A simpleContent restriction of a simple type.
    let restriction = r#"<xs:complexType name="d"><xs:simpleContent><xs:restriction base="xs:string"/></xs:simpleContent></xs:complexType>"#;
    assert_eq!(schema_problems(restriction).len(), 1);
}

#[test]
fn checks_element_attribute_and_all_rules_of_restrictions_and_extensions() {
    let restriction = |base: &str, derived: &str| {
        schema_problems(&format!(
            r#"<xs:complexType name="b">{base}</xs:complexType>
               <xs:complexType name="d"><xs:complexContent><xs:restriction base="b">{derived}</xs:restriction></xs:complexContent></xs:complexType>"#
        ))
    };
    let element = |attributes: &str| {
        format!(
            r#"<xs:sequence><xs:element name="e" type="xs:string" {attributes}/></xs:sequence>"#
        )
    };
    assert!(restriction(&element(""), &element("")).is_empty());
    assert!(restriction(&element(r#"nillable="true""#), &element("")).is_empty());
    assert_eq!(
        restriction(&element(""), &element(r#"nillable="true""#)).len(),
        1
    );
    assert_eq!(restriction(&element(r#"fixed="a""#), &element("")).len(), 1);
    assert_eq!(
        restriction(&element(r##"block="#all""##), &element("")).len(),
        1
    );
    assert_eq!(
        restriction(
            r#"<xs:sequence><xs:element name="e" type="xs:string"/></xs:sequence>"#,
            r#"<xs:sequence><xs:element name="e" type="xs:int"/></xs:sequence>"#
        )
        .len(),
        1
    );
    // A choice cannot restrict a sequence.
    assert!(
        !restriction(
            r#"<xs:sequence><xs:element name="a"/><xs:element name="b"/></xs:sequence>"#,
            r#"<xs:choice><xs:element name="a"/><xs:element name="b"/></xs:choice>"#
        )
        .is_empty()
    );
    // Attributes: a required one stays required, a fixed one stays fixed.
    let attribute = |usage: &str| format!(r#"<xs:attribute name="a" use="{usage}" fixed="1"/>"#);
    assert!(restriction(&attribute("required"), &attribute("required")).is_empty());
    assert_eq!(
        restriction(&attribute("required"), &attribute("prohibited")).len(),
        1
    );
    assert_eq!(
        restriction(
            &attribute("optional"),
            r#"<xs:attribute name="a" fixed="2"/>"#
        )
        .len(),
        1
    );
    // An extension cannot add an xs:all to a type that has content.
    let extension = |content: &str| {
        schema_problems(&format!(
            r#"<xs:complexType name="b"><xs:sequence><xs:element name="x"/></xs:sequence></xs:complexType>
               <xs:complexType name="d"><xs:complexContent><xs:extension base="b">{content}</xs:extension></xs:complexContent></xs:complexType>"#
        ))
    };
    assert!(extension(r#"<xs:sequence><xs:element name="y"/></xs:sequence>"#).is_empty());
    assert_eq!(
        extension(r#"<xs:all><xs:element name="y"/></xs:all>"#).len(),
        1
    );
}

#[test]
fn checks_xsd_facets_and_occurrence_bounds_of_schema_documents() {
    let check = |body: &str| {
        let source =
            format!(r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{body}</xs:schema>"#);
        schema_check::check_schema_document(&source).len()
    };
    let all = |attributes: &str| {
        format!(
            r#"<xs:complexType name="t"><xs:all {attributes}><xs:element name="a"/></xs:all></xs:complexType>"#
        )
    };
    assert_eq!(check(&all("")), 0);
    assert_eq!(check(&all(r#"minOccurs="0""#)), 0);
    assert_eq!(check(&all(r#"maxOccurs="2""#)), 1);
    // Bounds beyond usize are allowed.
    assert_eq!(
        check(
            r#"<xs:complexType name="t"><xs:sequence minOccurs="79228162514244337593543950335" maxOccurs="unbounded"><xs:element name="a"/></xs:sequence></xs:complexType>"#
        ),
        0
    );
    assert_eq!(
        check(
            r#"<xs:simpleType name="s"><xs:restriction base="xs:string"><xs:length value="3"/><xs:maxLength value="5"/></xs:restriction></xs:simpleType>"#
        ),
        1
    );
}

#[test]
fn checks_xsi_type_of_simple_types_nil_and_undeclared_roots() {
    let schema = parse_xsd(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="Base"><xs:restriction base="xs:string"><xs:maxLength value="5"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Short"><xs:restriction base="Base"><xs:maxLength value="2"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Other"><xs:restriction base="xs:string"/></xs:simpleType>
  <xs:element name="open" type="Base"/>
  <xs:element name="closed" type="Base" block="restriction"/>
  <xs:element name="nil" nillable="true" type="xs:string"/>
  <xs:element name="plain" type="xs:string"/>
</xs:schema>"#,
    )
    .unwrap();
    let x = r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#;
    let messages = |source: String| -> Vec<String> {
        validate_document(&source, &schema)
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect()
    };
    assert!(messages(format!(r#"<open {x} xsi:type="Short">ab</open>"#)).is_empty());
    assert_eq!(
        messages(format!(r#"<open {x} xsi:type="Other">ab</open>"#)).len(),
        1
    );
    assert_eq!(
        messages(format!(r#"<closed {x} xsi:type="Short">ab</closed>"#)).len(),
        1
    );
    assert!(messages(format!(r#"<closed {x}>ab</closed>"#)).is_empty());
    // Any xsi:nil on a declaration that is not nillable, content in a nil element.
    assert_eq!(
        messages(format!(r#"<plain {x} xsi:nil="false">a</plain>"#)).len(),
        1
    );
    assert!(messages(format!(r#"<nil {x} xsi:nil="true"/>"#)).is_empty());
    assert_eq!(
        messages(format!(r#"<nil {x} xsi:nil="true">text</nil>"#)).len(),
        1
    );
    // A root without declaration is judged by its xsi:type.
    assert!(messages(format!(r#"<other {x} xsi:type="Short">ab</other>"#)).is_empty());
    assert_eq!(
        messages(format!(r#"<other {x} xsi:type="Short">abcdef</other>"#)).len(),
        1
    );
    assert_eq!(messages(r#"<other/>"#.to_owned()).len(), 1);
}

#[test]
fn checks_the_namespaces_of_includes_and_imports() {
    let problems = |source: &str, dependency: Option<Option<&str>>| {
        dependency_problems(source, Path::new("/schemas/a.xsd"), &|_| None, &|_| {
            dependency.map(|namespace| namespace.map(str::to_owned))
        })
    };
    let schema = |namespace: &str, body: &str| {
        format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"{namespace}>{body}</xs:schema>"#
        )
    };
    let own = r#" targetNamespace="urn:a""#;
    let include = r#"<xs:include schemaLocation="b.xsd"/>"#;
    // An included schema has no target namespace, or the same one.
    assert!(problems(&schema(own, include), Some(None)).is_empty());
    assert!(problems(&schema(own, include), Some(Some("urn:a"))).is_empty());
    assert_eq!(
        problems(&schema(own, include), Some(Some("urn:b"))).len(),
        1
    );
    assert_eq!(problems(&schema("", include), Some(Some("urn:b"))).len(), 1);
    assert!(problems(&schema(own, include), None).is_empty());
    // An import names the namespace of the imported schema, not its own.
    let import = r#"<xs:import namespace="urn:b" schemaLocation="b.xsd"/>"#;
    assert!(problems(&schema(own, import), Some(Some("urn:b"))).is_empty());
    assert_eq!(problems(&schema(own, import), Some(Some("urn:c"))).len(), 1);
    assert_eq!(problems(&schema(own, import), Some(None)).len(), 1);
    let own_import = r#"<xs:import namespace="urn:a"/>"#;
    assert_eq!(problems(&schema(own, own_import), None).len(), 1);
    // Without namespace, the importing schema needs a target namespace.
    let bare = r#"<xs:import schemaLocation="b.xsd"/>"#;
    assert!(problems(&schema(own, bare), Some(None)).is_empty());
    assert_eq!(problems(&schema("", bare), Some(None)).len(), 1);
    // A schema does not redefine itself.
    let redefine = r#"<xs:redefine schemaLocation="a.xsd"/>"#;
    assert_eq!(problems(&schema(own, redefine), None).len(), 1);
}
