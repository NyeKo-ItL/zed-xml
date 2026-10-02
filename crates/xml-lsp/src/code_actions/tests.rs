use std::collections::HashMap;

use super::*;
use crate::{formatting::apply_edits, hover::ModelCache, path_to_uri};

const SCHEMA: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="catalog">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="book" maxOccurs="unbounded">
          <xs:complexType>
            <xs:sequence>
              <xs:element name="title" type="xs:string"/>
              <xs:element name="format" type="Format" minOccurs="0"/>
            </xs:sequence>
            <xs:attribute name="isbn" type="xs:string" use="required"/>
            <xs:attribute name="lang" type="xs:language" use="required" default="fr"/>
            <xs:attribute name="status" type="Status" use="required"/>
            <xs:attribute name="color" type="Color"/>
          </xs:complexType>
        </xs:element>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
  <xs:simpleType name="Format">
    <xs:restriction base="xs:token">
      <xs:enumeration value="hardcover"/>
      <xs:enumeration value="paperback"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="Status"><xs:restriction base="xs:string"><xs:enumeration value="draft"/><xs:enumeration value="published"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Color"><xs:restriction base="xs:string"><xs:enumeration value="red"/><xs:enumeration value="green"/><xs:enumeration value="blue"/></xs:restriction></xs:simpleType>
</xs:schema>"#;

const BOUND: &str = "<catalog xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"catalog.xsd\">";

struct Fixture {
    directory: std::path::PathBuf,
    documents: HashMap<String, String>,
    cache: ModelCache,
}

impl Fixture {
    fn new(name: &str, schema: Option<&str>) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "xml-lsp-code-actions-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("directory should be created");
        if let Some(schema) = schema {
            fs::write(directory.join("catalog.xsd"), schema).expect("schema should be written");
        }
        Self {
            directory,
            documents: HashMap::new(),
            cache: ModelCache::new(),
        }
    }

    fn uri(&self) -> String {
        path_to_uri(&self.directory.join("catalog.xml"))
    }

    fn actions(&mut self, source: &str, range: Range<usize>, context: Value) -> Vec<Value> {
        let uri = self.uri();
        self.documents.insert(uri.clone(), source.to_owned());
        let mut hover = HoverContext {
            documents: &self.documents,
            cache: &mut self.cache,
            associated_schemas: Vec::new(),
            catalogs: &crate::catalog::Catalogs::default(),
        };
        code_actions(&mut hover, &uri, source, range, &context)
    }

    fn diagnostics(&mut self, source: &str) -> Vec<Value> {
        let uri = self.uri();
        let mut hover = HoverContext {
            documents: &self.documents,
            cache: &mut self.cache,
            associated_schemas: Vec::new(),
            catalogs: &crate::catalog::Catalogs::default(),
        };
        enumeration_diagnostics(&mut hover, &uri, source)
    }

    /// Applies the action titled `title`.
    fn apply(&self, source: &str, actions: &[Value], title: &str) -> String {
        let action = actions
            .iter()
            .find(|action| action["title"] == title)
            .unwrap_or_else(|| panic!("action {title:?} missing in {actions:#?}"));
        apply_edits(source, &action["edit"]["changes"][self.uri()])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn at(source: &str, needle: &str) -> Range<usize> {
    let start = source
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} missing"));
    start..start
}

fn titles(actions: &[Value]) -> Vec<&str> {
    actions
        .iter()
        .map(|action| action["title"].as_str().unwrap())
        .collect()
}

fn quick_fixes() -> Value {
    json!({"only": ["quickfix"]})
}

#[test]
fn fixes_mismatched_and_unmatched_end_tags() {
    let mut fixture = Fixture::new("tags", None);
    let source = "<root>\n  <child></chidl>\n</root>";
    let actions = fixture.actions(source, at(source, "hidl"), quick_fixes());
    assert_eq!(
        titles(&actions),
        vec![
            "Replace </chidl> with </child>",
            "Rename <child> to <chidl>"
        ]
    );
    assert_eq!(actions[0]["isPreferred"], true);
    assert_eq!(actions[0]["kind"], "quickfix");
    assert_eq!(
        fixture.apply(source, &actions, "Replace </chidl> with </child>"),
        "<root>\n  <child></child>\n</root>"
    );
    assert_eq!(
        fixture.apply(source, &actions, "Rename <child> to <chidl>"),
        "<root>\n  <chidl></chidl>\n</root>"
    );
    // Outside the faulty tag: no fix.
    assert!(fixture.actions(source, 0..0, quick_fixes()).is_empty());

    let source = "<root></extra></root>";
    let actions = fixture.actions(source, at(source, "extra"), quick_fixes());
    assert_eq!(
        fixture.apply(source, &actions, "Remove end tag </extra>"),
        "<root></root>"
    );
}

#[test]
fn closes_unclosed_elements_and_tags() {
    let mut fixture = Fixture::new("close", None);
    let source = "<root>\n  <item>\n  <other/>\n</root>";
    let actions = fixture.actions(source, at(source, "item"), quick_fixes());
    assert_eq!(
        fixture.apply(source, &actions, "Close <item> with </item>"),
        "<root>\n  <item>\n  <other/>\n</item></root>"
    );
    assert_eq!(
        fixture.apply(source, &actions, "Make <item> self-closing"),
        "<root>\n  <item/>\n  <other/>\n</root>"
    );

    let source = "<root><a x=\"1\" \n</root>";
    let actions = fixture.actions(source, at(source, "a x"), quick_fixes());
    assert_eq!(
        fixture.apply(source, &actions, "End the tag with `/>`"),
        "<root><a x=\"1\"/> \n</root>"
    );
    assert_eq!(
        fixture.apply(source, &actions, "End the tag with `>`"),
        "<root><a x=\"1\"> \n</root>"
    );
}

#[test]
fn fixes_attributes_and_unescaped_characters() {
    let mut fixture = Fixture::new("syntax", None);
    let source = "<a x=\"1\" y=2 x='3'>1 < 2 & 3</a>";
    let actions = fixture.actions(source, 0..source.len(), quick_fixes());
    assert_eq!(
        titles(&actions),
        vec![
            "Quote the value",
            "Remove duplicate attribute x",
            "Replace `<` with `&lt;`",
            "Replace `&` with `&amp;`",
        ]
    );
    let mut fixed = source.to_owned();
    for title in titles(&actions) {
        let actions = fixture.actions(&fixed, 0..fixed.len(), quick_fixes());
        fixed = fixture.apply(&fixed, &actions, title);
    }
    assert_eq!(fixed, "<a x=\"1\" y=\"2\">1 &lt; 2 &amp; 3</a>");
    assert!(check_well_formedness(&fixed).is_empty());
}

#[test]
fn declares_a_well_known_undeclared_prefix() {
    let mut fixture = Fixture::new("prefix", None);
    let source = "<root>\n  <a xsi:nil=\"true\"/>\n</root>";
    let actions = fixture.actions(source, 0..source.len(), quick_fixes());
    assert_eq!(
        fixture.apply(
            source,
            &actions,
            "Declare the prefix xsi on the root element"
        ),
        "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\n  <a xsi:nil=\"true\"/>\n</root>"
    );
    let unknown = "<root><a foo:x=\"1\"/></root>";
    assert!(
        fixture
            .actions(unknown, 0..unknown.len(), quick_fixes())
            .is_empty()
    );
}

#[test]
fn attaches_matching_context_diagnostics_only() {
    let mut fixture = Fixture::new("diagnostics", None);
    let source = "<a>é & b</a>";
    let range = |start: u32, end: u32| json!({"start": {"line": 0, "character": start}, "end": {"line": 0, "character": end}});
    let matching = json!({
        "range": range(5, 6),
        "code": "xml-syntax",
        "data": {"category": "xml", "kind": "unescapedCharacter"},
        "message": "unescaped `&` character",
    });
    let other = json!({"range": range(5, 6), "code": "xsd-validation", "message": "other"});
    // `&`: UTF-16 column 5, byte 6 (é takes two bytes).
    let actions = fixture.actions(
        source,
        6..6,
        json!({"only": ["quickfix"], "diagnostics": [matching.clone(), other]}),
    );
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["diagnostics"], json!([matching]));
}

#[test]
fn filters_actions_by_requested_kinds() {
    let mut fixture = Fixture::new("only", None);
    let source = "<root><empty></empty> & </root>";
    let everything = fixture.actions(source, at(source, "empty>"), json!({}));
    let kinds = everything
        .iter()
        .map(|action| action["kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec!["refactor.rewrite", "source", "source.copyXPath"]
    );
    let refactors = fixture.actions(source, at(source, "empty>"), json!({"only": ["refactor"]}));
    assert_eq!(refactors.len(), 1);
    assert!(
        fixture
            .actions(
                source,
                at(source, "empty>"),
                json!({"only": ["refactor.extract"]})
            )
            .is_empty()
    );
    assert!(
        fixture
            .actions(source, at(source, "empty>"), json!({"only": ["sourceX"]}))
            .is_empty()
    );
}

#[test]
fn converts_between_empty_and_self_closing_elements() {
    let mut fixture = Fixture::new("rewrite", None);
    let rewrite = json!({"only": ["refactor.rewrite"]});
    let source = "<root>\n  <item id=\"1\">\n  </item>\n</root>";
    let actions = fixture.actions(source, at(source, "/item"), rewrite.clone());
    let converted = fixture.apply(
        source,
        &actions,
        "Convert <item></item> to self-closing <item/>",
    );
    assert_eq!(converted, "<root>\n  <item id=\"1\"/>\n</root>");
    let actions = fixture.actions(&converted, at(&converted, "item"), rewrite.clone());
    assert_eq!(
        fixture.apply(&converted, &actions, "Expand <item/> to <item></item>"),
        "<root>\n  <item id=\"1\"></item>\n</root>"
    );
    let source = "<p:a xmlns:p=\"urn:p\" />";
    let actions = fixture.actions(source, 1..1, rewrite.clone());
    assert_eq!(
        fixture.apply(source, &actions, "Expand <p:a/> to <p:a></p:a>"),
        "<p:a xmlns:p=\"urn:p\"></p:a>"
    );
    // Non-empty element or cursor in the content: no rewrite.
    let source = "<a>text</a>";
    assert!(fixture.actions(source, 1..1, rewrite.clone()).is_empty());
    assert!(fixture.actions(source, 5..5, rewrite).is_empty());
}

#[test]
fn adds_missing_required_attributes() {
    let mut fixture = Fixture::new("required", Some(SCHEMA));
    let source =
        format!("{BOUND}\n  <book isbn=\"1\"><title>T</title></book>\n  <book />\n</catalog>");
    let first = source.find("<book").unwrap();
    let actions = fixture.actions(&source, first + 3..first + 3, quick_fixes());
    assert_eq!(
        titles(&actions),
        vec!["Add required attributes lang, status"]
    );
    let fixed = fixture.apply(&source, &actions, "Add required attributes lang, status");
    assert!(fixed.contains("<book isbn=\"1\" lang=\"fr\" status=\"draft\">"));

    let second = source.rfind("<book").unwrap();
    let actions = fixture.actions(&source, second + 7..second + 7, quick_fixes());
    let fixed = fixture.apply(
        &source,
        &actions,
        "Add required attributes isbn, lang, status",
    );
    assert!(fixed.contains("<book isbn=\"\" lang=\"fr\" status=\"draft\" />"));
}

#[test]
fn replaces_values_outside_enumerations() {
    let mut fixture = Fixture::new("enumeration", Some(SCHEMA));
    let source = format!(
        "{BOUND}\n  <book isbn=\"1\" lang=\"fr\" status=\"draft\" color=\"gren\"><title>T</title><format> hardcovers </format></book>\n</catalog>"
    );
    let diagnostics = fixture.diagnostics(&source);
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0]["code"], "xsd-validation");
    assert_eq!(diagnostics[0]["data"]["rule"], "invalidEnumeration");
    assert_eq!(
        diagnostics[0]["message"],
        "value `gren` is not in the enumeration of @color on <book> (expected: `red`, `green`, `blue`)"
    );
    assert_eq!(
        diagnostics[1]["message"],
        "value `hardcovers` is not in the enumeration of <format> (expected: `hardcover`, `paperback`)"
    );

    let color = source.find("gren").unwrap();
    let actions = fixture.actions(
        &source,
        color..color,
        json!({"only": ["quickfix"], "diagnostics": [diagnostics[0]]}),
    );
    assert_eq!(
        titles(&actions),
        vec![
            "Replace with `red`",
            "Replace with `green`",
            "Replace with `blue`"
        ]
    );
    assert_eq!(actions[1]["isPreferred"], true);
    assert!(actions[0].get("isPreferred").is_none());
    assert_eq!(actions[1]["diagnostics"], json!([diagnostics[0]]));
    let fixed = fixture.apply(&source, &actions, "Replace with `green`");
    assert!(fixed.contains("color=\"green\""));

    let format = source.find("hardcovers").unwrap();
    let actions = fixture.actions(&source, format + 2..format + 2, quick_fixes());
    let fixed = fixture.apply(&source, &actions, "Replace with `hardcover`");
    assert!(fixed.contains("<format> hardcover </format>"));
    assert!(fixture.diagnostics(&fixed).len() == 1);
}

#[test]
fn suggests_close_element_names() {
    let mut fixture = Fixture::new("unknown", Some(SCHEMA));
    let source = format!(
        "{BOUND}\n  <book isbn=\"1\" lang=\"fr\" status=\"draft\"><titel>T</titel></book>\n</catalog>"
    );
    let unknown = source.find("titel").unwrap();
    let actions = fixture.actions(&source, unknown..unknown, quick_fixes());
    assert_eq!(titles(&actions), vec!["Did you mean <title>?"]);
    assert_eq!(actions[0]["isPreferred"], true);
    let fixed = fixture.apply(&source, &actions, "Did you mean <title>?");
    assert!(fixed.contains("<title>T</title>"));

    let source = source
        .replace("<catalog xmlns", "<catalgo xmlns")
        .replace("</catalog>", "</catalgo>");
    let actions = fixture.actions(&source, 3..3, quick_fixes());
    assert_eq!(titles(&actions), vec!["Did you mean <catalog>?"]);
    // No close name: no suggestion.
    let source = source.replace("catalgo", "zzz");
    assert!(fixture.actions(&source, 2..2, quick_fixes()).is_empty());
}

#[test]
fn binds_documents_to_sibling_or_placeholder_schemas() {
    let source_actions = json!({"only": ["source"]});
    let mut fixture = Fixture::new("bind", Some(SCHEMA));
    fs::write(fixture.directory.join("a other.xsd"), "<xs:schema/>").unwrap();
    let source = "<?xml version=\"1.0\"?>\n<catalog>\n  <book/>\n</catalog>";
    let actions = fixture.actions(source, 0..0, source_actions.clone());
    assert_eq!(
        titles(&actions),
        vec![
            "Bind the document to the XSD schema catalog.xsd",
            "Bind the document to the XSD schema a other.xsd",
        ]
    );
    assert_eq!(actions[0]["kind"], "source");
    assert_eq!(
        fixture.apply(
            source,
            &actions,
            "Bind the document to the XSD schema a other.xsd"
        ),
        "<?xml version=\"1.0\"?>\n<catalog xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"a%20other.xsd\">\n  <book/>\n</catalog>"
    );
    // Already bound: nothing to offer.
    assert!(
        fixture
            .actions(&format!("{BOUND}</catalog>"), 0..0, source_actions.clone())
            .iter()
            .all(|action| action["kind"] == "source.copyXPath")
    );

    let mut empty = Fixture::new("bind-placeholder", None);
    let source =
        "<t:root xmlns:t=\"urn:t\" xmlns:i=\"http://www.w3.org/2001/XMLSchema-instance\"/>";
    let actions = empty.actions(source, 0..0, source_actions);
    assert_eq!(
        titles(&actions),
        vec![
            "Bind the document to an XSD schema (placeholder schema.xsd)",
            "Copy XPath: /t:root"
        ]
    );
    assert_eq!(
        empty.apply(
            source,
            &actions,
            "Bind the document to an XSD schema (placeholder schema.xsd)"
        ),
        "<t:root xmlns:t=\"urn:t\" xmlns:i=\"http://www.w3.org/2001/XMLSchema-instance\" i:schemaLocation=\"urn:t schema.xsd\"/>"
    );
}

#[test]
fn computes_edit_distances_with_transpositions() {
    assert_eq!(edit_distance("chidl", "child"), 1);
    assert_eq!(edit_distance("titel", "title"), 1);
    assert_eq!(edit_distance("", "abc"), 3);
    assert_eq!(edit_distance("kitten", "sitting"), 3);
    assert_eq!(edit_distance("été", "ete"), 2);
    assert_eq!(
        closest("Titel", ["title", "format", "titles"].into_iter()),
        vec![("title", 1), ("titles", 2)]
    );
}
