use super::*;

const MAIN_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:lib="urn:lib" targetNamespace="urn:lib" elementFormDefault="qualified">
  <xs:include schemaLocation="types.xsd"/>
  <xs:element name="library">
    <xs:annotation><xs:documentation>A library of *books*.</xs:documentation></xs:annotation>
    <xs:complexType><xs:sequence>
      <xs:element name="title" type="xs:string"><xs:annotation><xs:documentation>Library name.</xs:documentation></xs:annotation></xs:element>
      <xs:element name="book" type="lib:Book" minOccurs="0" maxOccurs="unbounded"/>
    </xs:sequence></xs:complexType>
  </xs:element>
  <xs:element name="title"><xs:annotation><xs:documentation>Global title.</xs:documentation></xs:annotation></xs:element>
</xs:schema>"#;

const TYPES_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:lib="urn:lib" targetNamespace="urn:lib" elementFormDefault="qualified">
  <xs:complexType name="Item">
    <xs:annotation><xs:documentation>Base item.</xs:documentation></xs:annotation>
    <xs:attribute name="id" type="xs:ID" use="required"><xs:annotation><xs:documentation>Unique identifier.</xs:documentation></xs:annotation></xs:attribute>
  </xs:complexType>
  <xs:complexType name="Book">
    <xs:annotation><xs:documentation xml:lang="fr">Un livre.</xs:documentation><xs:documentation xml:lang="en">A book.</xs:documentation></xs:annotation>
    <xs:complexContent><xs:extension base="lib:Item">
      <xs:sequence>
        <xs:element name="title" type="lib:Title"><xs:annotation><xs:documentation>Book title.</xs:documentation></xs:annotation></xs:element>
        <xs:element name="isbn" type="lib:Isbn"/>
      </xs:sequence>
      <xs:attribute name="format" type="lib:Format" default="paper"/>
    </xs:extension></xs:complexContent>
  </xs:complexType>
  <xs:simpleType name="Format">
    <xs:annotation><xs:documentation>Publication format.</xs:documentation></xs:annotation>
    <xs:restriction base="xs:string">
      <xs:enumeration value="paper"><xs:annotation><xs:documentation>Printed edition.</xs:documentation></xs:annotation></xs:enumeration>
      <xs:enumeration value="ebook"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="Isbn"><xs:restriction base="xs:string"><xs:pattern value="[0-9]{13}"/><xs:length value="13"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Title"><xs:restriction base="xs:string"><xs:minLength value="1"/><xs:maxLength value="200"/></xs:restriction></xs:simpleType>
</xs:schema>"#;

const INSTANCE: &str = "<l:library xmlns:l=\"urn:lib\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:schemaLocation=\"urn:lib main.xsd\">\r\n  <l:title>Città 📚</l:title>\r\n  <l:book id=\"b1\" format=\"paper\">\r\n    <l:title>Dune</l:title>\r\n    <l:isbn>9780441013593</l:isbn>\r\n  </l:book>\r\n</l:library>";

struct Fixture {
    directory: PathBuf,
    documents: HashMap<String, String>,
    cache: ModelCache,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-hover-{}-{name}", std::process::id()));
        fs::create_dir_all(&directory).expect("directory should be created");
        fs::write(directory.join("main.xsd"), MAIN_XSD).expect("schema should be written");
        fs::write(directory.join("types.xsd"), TYPES_XSD).expect("schema should be written");
        Self {
            directory,
            documents: HashMap::new(),
            cache: ModelCache::new(),
        }
    }

    fn uri(&self, file: &str) -> String {
        path_to_uri(&self.directory.join(file))
    }

    fn open(&mut self, file: &str, source: &str) -> String {
        let uri = self.uri(file);
        self.documents.insert(uri.clone(), source.to_owned());
        uri
    }

    fn hover(&mut self, uri: &str, offset: usize) -> Option<Value> {
        let source = self.documents[uri].clone();
        let mut context = HoverContext {
            documents: &self.documents,
            cache: &mut self.cache,
            associated_schemas: Vec::new(),
            catalogs: &crate::catalog::Catalogs::default(),
        };
        hover(&mut context, uri, &source, offset)
    }

    fn markdown(&mut self, uri: &str, offset: usize) -> String {
        let hover = self.hover(uri, offset).expect("hover expected");
        assert_eq!(hover["contents"]["kind"], "markdown");
        hover["contents"]["value"].as_str().unwrap().to_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn at(source: &str, needle: &str, nth: usize) -> usize {
    source
        .match_indices(needle)
        .nth(nth)
        .map(|(index, _)| index)
        .expect("needle should exist")
}

#[test]
fn documents_elements_resolved_in_their_context() {
    let mut fixture = Fixture::new("elements");
    let uri = fixture.open("doc.xml", INSTANCE);
    let main_uri = fixture.uri("main.xsd");
    let types_uri = fixture.uri("types.xsd");

    let hover = fixture.hover(&uri, at(INSTANCE, "library", 0) + 2).unwrap();
    assert_eq!(
        hover["range"],
        json!({"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 10}})
    );
    assert_eq!(
        hover["contents"]["value"],
        format!(
            "**Element** `<l:library>`\n\n- Namespace: `urn:lib`\n- Type: anonymous complex\n\nA library of \\*books\\*.\n\nSource: [main.xsd]({main_uri})"
        )
    );

    // Local declaration of <library>, not the homonymous global declaration.
    let local = fixture.markdown(&uri, at(INSTANCE, "l:title", 0));
    assert!(local.contains("- Type: `xs:string`"), "{local}");
    assert!(local.contains("- Cardinality: `1..1`"), "{local}");
    assert!(local.contains("Library name."), "{local}");
    assert!(!local.contains("Global title."), "{local}");

    // Local declaration of the inherited type, in the included schema.
    let nested = fixture.markdown(&uri, at(INSTANCE, "l:title", 2) + 4);
    assert!(
        nested.contains("- Type: `lib:Title` (restriction of `xs:string`)"),
        "{nested}"
    );
    assert!(nested.contains("Book title."), "{nested}");
    assert!(nested.contains(&format!("Source: [types.xsd]({types_uri})")));

    // End tag: documentation of the type when the element has none.
    let book = fixture
        .hover(&uri, at(INSTANCE, "</l:book", 0) + 3)
        .unwrap();
    assert_eq!(
        book["range"],
        json!({"start": {"line": 5, "character": 4}, "end": {"line": 5, "character": 10}})
    );
    let book = book["contents"]["value"].as_str().unwrap();
    assert!(
        book.contains("- Type: `lib:Book` (extension of `lib:Item`)"),
        "{book}"
    );
    assert!(book.contains("- Cardinality: `0..*`"), "{book}");
    assert!(book.contains("\n\nA book.\n\n"), "{book}");
    assert!(!book.contains("Un livre"), "{book}");
}

#[test]
fn documents_attributes_and_namespace_declarations() {
    let mut fixture = Fixture::new("attributes");
    let uri = fixture.open("doc.xml", INSTANCE);
    let types_uri = fixture.uri("types.xsd");

    let id = fixture.hover(&uri, at(INSTANCE, "id=", 0) + 1).unwrap();
    assert_eq!(
        id["range"],
        json!({"start": {"line": 2, "character": 10}, "end": {"line": 2, "character": 12}})
    );
    assert_eq!(
        id["contents"]["value"],
        format!(
            "**Attribute** `id`\n\n- Type: `xs:ID`\n- Use: required\n\nUnique identifier.\n\nSource: [types.xsd]({types_uri})"
        )
    );

    let format = fixture.markdown(&uri, at(INSTANCE, "format=", 0));
    assert!(format.contains("- Type: `lib:Format` (restriction of `xs:string`)"));
    assert!(format.contains("- Use: optional"), "{format}");
    assert!(format.contains("- Default value: `paper`"), "{format}");
    assert!(format.contains("- Allowed values: `paper`, `ebook`"));
    assert!(format.contains("Publication format."), "{format}");

    let namespace = fixture.markdown(&uri, at(INSTANCE, "xmlns:l", 0) + 6);
    assert_eq!(
        namespace,
        "**Namespace declaration** `xmlns:l`\n\n`urn:lib`"
    );
}

#[test]
fn documents_enumeration_values_and_simple_content_facets() {
    let mut fixture = Fixture::new("values");
    let uri = fixture.open("doc.xml", INSTANCE);

    let value = fixture.hover(&uri, at(INSTANCE, "paper", 0) + 1).unwrap();
    assert_eq!(
        value["range"],
        json!({"start": {"line": 2, "character": 26}, "end": {"line": 2, "character": 31}})
    );
    let value = value["contents"]["value"].as_str().unwrap();
    assert!(value.starts_with("**Value** `paper`\n\n"), "{value}");
    assert!(value.contains("- Allowed values: `paper`, `ebook`"));
    assert!(value.contains("Printed edition."), "{value}");

    let isbn = fixture.markdown(&uri, at(INSTANCE, "9780", 0) + 5);
    assert!(isbn.contains("- Type: `lib:Isbn` (restriction of `xs:string`)"));
    assert!(isbn.contains("- Pattern: `[0-9]{13}`"), "{isbn}");
    assert!(isbn.contains("- Length: `13`"), "{isbn}");
    assert!(isbn.contains("- Built-in type: `xs:string`"), "{isbn}");

    let title = fixture.markdown(&uri, at(INSTANCE, "Dune", 0));
    assert!(title.contains("- Minimum length: `1`"), "{title}");
    assert!(title.contains("- Maximum length: `200`"), "{title}");

    // UTF-16 positions (emoji outside the Basic Multilingual Plane) and CRLF.
    let text = fixture.hover(&uri, at(INSTANCE, "Città", 0) + 1).unwrap();
    assert_eq!(
        text["range"],
        json!({"start": {"line": 1, "character": 11}, "end": {"line": 1, "character": 19}})
    );
    assert!(
        text["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("- Type: `xs:string`")
    );

    // Whitespace between elements, complex content: no hover.
    assert!(
        fixture
            .hover(&uri, at(INSTANCE, "\r\n  <l:book", 0) + 2)
            .is_none()
    );
}

#[test]
fn documents_xsd_references_across_included_schemas() {
    let mut fixture = Fixture::new("schemas");
    let main_uri = fixture.open("main.xsd", MAIN_XSD);
    let types_uri = fixture.uri("types.xsd");

    let book = fixture
        .hover(&main_uri, at(MAIN_XSD, "lib:Book", 0) + 5)
        .unwrap();
    assert_eq!(
        book["contents"]["value"],
        format!(
            "**Complex type** `lib:Book`\n\n- Namespace: `urn:lib`\n- Derivation: extension of `lib:Item`\n\nA book.\n\nSource: [types.xsd]({types_uri})"
        )
    );
    assert_eq!(book["range"]["start"], json!({"line": 6, "character": 36}));

    let builtin = fixture.markdown(&main_uri, at(MAIN_XSD, "xs:string", 0));
    assert!(
        builtin.starts_with("**Built-in type** `xs:string`"),
        "{builtin}"
    );

    let global = fixture.markdown(&main_uri, at(MAIN_XSD, "\"title\"", 1) + 1);
    assert!(global.contains("Global title."), "{global}");
    // Local declaration: no global component to document.
    assert!(
        fixture
            .hover(&main_uri, at(MAIN_XSD, "\"title\"", 0) + 1)
            .is_none_or(|hover| !hover.to_string().contains("Library name"))
    );

    // An unsaved open schema takes precedence over the disk.
    let edited = TYPES_XSD.replace(">A book.<", ">An edited book.<");
    let types_uri = fixture.open("types.xsd", &edited);
    let book = fixture.markdown(&main_uri, at(MAIN_XSD, "lib:Book", 0));
    assert!(book.contains("An edited book."), "{book}");

    let item = fixture.markdown(&types_uri, at(&edited, "lib:Item", 0) + 4);
    assert!(item.contains("**Complex type** `lib:Item`"), "{item}");
    assert!(item.contains("Base item."), "{item}");
    let format = fixture.markdown(&types_uri, at(&edited, "\"Format\"", 0) + 2);
    assert!(format.starts_with("**Simple type** `Format`"), "{format}");
    assert!(format.contains("- Allowed values: `paper`, `ebook`"));
    assert!(format.contains("Publication format."), "{format}");
}

#[test]
fn resolves_xsd_reference_lists_prefixes_and_groups() {
    let mut fixture = Fixture::new("lists");
    let source = r#"<schema xmlns="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t" targetNamespace="urn:t">
  <simpleType name="A"><annotation><documentation>Type A</documentation></annotation><restriction base="string"/></simpleType>
  <simpleType name="B"><annotation><documentation>Type B</documentation></annotation><restriction base="int"/></simpleType>
  <simpleType name="AB"><union memberTypes="t:A  t:B"/></simpleType>
  <group name="G"><annotation><documentation>Group G</documentation></annotation><sequence/></group>
  <attributeGroup name="AG"><annotation><documentation>Group AG</documentation></annotation></attributeGroup>
  <attribute name="lang"><annotation><documentation>Lang attribute</documentation></annotation></attribute>
  <complexType name="C"><sequence><group ref="t:G"/></sequence><attributeGroup ref="t:AG"/><attribute ref="t:lang" use="required"/></complexType>
</schema>"#;
    let uri = fixture.open("lists.xsd", source);

    let b = fixture.hover(&uri, at(source, "t:B", 0) + 2).unwrap();
    assert!(b["contents"]["value"].as_str().unwrap().contains("Type B"));
    assert_eq!(
        b["range"],
        json!({"start": {"line": 3, "character": 49}, "end": {"line": 3, "character": 52}})
    );
    assert!(
        fixture
            .markdown(&uri, at(source, "t:A", 0))
            .contains("Type A")
    );
    assert!(
        fixture
            .markdown(&uri, at(source, "\"string\"", 0) + 1)
            .starts_with("**Built-in type** `string`")
    );
    let union = fixture.markdown(&uri, at(source, "\"AB\"", 0) + 1);
    assert!(union.contains("- Union of `t:A`, `t:B`"), "{union}");
    assert!(
        fixture
            .markdown(&uri, at(source, "t:G", 0))
            .contains("**Group** `t:G`")
    );
    assert!(
        fixture
            .markdown(&uri, at(source, "t:AG", 0))
            .contains("Group AG")
    );
    let lang = fixture.markdown(&uri, at(source, "t:lang", 0));
    assert!(lang.contains("**Attribute** `t:lang`"), "{lang}");
    assert!(lang.contains("Lang attribute"), "{lang}");
    // Outside a reference: minimal XML hover.
    assert_eq!(
        fixture.markdown(&uri, at(source, "sequence", 0)),
        "**Element** `<sequence>`\n\nNamespace: `http://www.w3.org/2001/XMLSchema`"
    );
}

#[test]
fn falls_back_without_schema() {
    let mut fixture = Fixture::new("fallback");
    let source = "<a:root xmlns:a=\"urn:x\" b=\"1\" a:c=\"2\">text<child/></a:root>";
    let uri = fixture.open("plain.xml", source);

    assert_eq!(
        fixture.markdown(&uri, 3),
        "**Element** `<a:root>`\n\nNamespace: `urn:x`"
    );
    assert_eq!(
        fixture.markdown(&uri, at(source, "child", 0)),
        "**Element** `<child>`"
    );
    assert_eq!(
        fixture.markdown(&uri, at(source, "b=", 0)),
        "**Attribute** `b`"
    );
    assert_eq!(
        fixture.markdown(&uri, at(source, "a:c", 0)),
        "**Attribute** `a:c`\n\nNamespace: `urn:x`"
    );
    assert!(fixture.hover(&uri, at(source, "\"1\"", 0) + 1).is_none());
    assert!(fixture.hover(&uri, at(source, "text", 0) + 1).is_none());
    assert!(fixture.hover(&uri, 0).is_none());
}

#[test]
fn resolves_default_namespace_xsi_type_and_malformed_documents() {
    let mut fixture = Fixture::new("xsi");
    let schema = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t" targetNamespace="urn:t" elementFormDefault="qualified">
  <xs:element name="shape" type="t:Shape"/>
  <xs:complexType name="Shape"><xs:attribute name="name"/></xs:complexType>
  <xs:complexType name="Circle"><xs:annotation><xs:documentation>A circle.</xs:documentation></xs:annotation><xs:complexContent><xs:extension base="t:Shape">
    <xs:attribute name="radius" type="xs:decimal"><xs:annotation><xs:documentation>Radius &lt;cm&gt;.</xs:documentation></xs:annotation></xs:attribute>
  </xs:extension></xs:complexContent></xs:complexType>
</xs:schema>"#;
    fs::write(fixture.directory.join("shape.xsd"), schema).unwrap();
    let source = "<shape xmlns=\"urn:t\" xmlns:i=\"http://www.w3.org/2001/XMLSchema-instance\" i:schemaLocation=\"urn:t shape.xsd\" i:type=\"Circle\" radius=\"2\"";
    let uri = fixture.open("shape.xml", source);

    let shape = fixture.markdown(&uri, 2);
    assert!(
        shape.contains("- Type: `Circle` (extension of `t:Shape`)"),
        "{shape}"
    );
    assert!(shape.contains("A circle."), "{shape}");
    let radius = fixture.markdown(&uri, at(source, "radius", 0));
    assert!(radius.contains("Radius \\<cm\\>."), "{radius}");
    let value = fixture.markdown(&uri, at(source, "\"2\"", 0) + 1);
    assert!(value.contains("- Type: `xs:decimal`"), "{value}");
}
