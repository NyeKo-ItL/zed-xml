use super::*;

/// Test-specific temporary directory.
fn directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("xml-lsp-dtd {name} {}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("directory should be created");
    directory
}

struct Fixture {
    documents: HashMap<String, String>,
    catalogs: Catalogs,
    cache: DtdCache,
}

impl Fixture {
    fn new() -> Self {
        Self {
            documents: HashMap::new(),
            catalogs: Catalogs::default(),
            cache: DtdCache::new(),
        }
    }

    fn context(&mut self) -> DtdContext<'_> {
        DtdContext {
            documents: &self.documents,
            catalogs: &self.catalogs,
            cache: &mut self.cache,
        }
    }

    fn grammar(&mut self, uri: &str, source: &str) -> Option<Grammar> {
        load(&mut self.context(), uri, source)
    }

    fn diagnostics(&mut self, uri: &str, source: &str) -> Vec<Value> {
        self.diagnostics_with(uri, source, &ValidationSettings::default())
    }

    fn diagnostics_with(
        &mut self,
        uri: &str,
        source: &str,
        validation: &ValidationSettings,
    ) -> Vec<Value> {
        diagnostics(&mut self.context(), uri, source, validation)
    }
}

fn kinds(diagnostics: &[Value]) -> Vec<(String, String, u64)> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            (
                diagnostic["code"].as_str().unwrap().to_owned(),
                diagnostic["data"]["kind"].as_str().unwrap().to_owned(),
                diagnostic["severity"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn kind(code: &str, kind: &str, severity: u64) -> (String, String, u64) {
    (code.to_owned(), kind.to_owned(), severity)
}

fn labels(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .map(|item| item["label"].as_str().unwrap())
        .collect()
}

fn at(source: &str, marker: &str) -> usize {
    source.find(marker).expect("marker should exist")
}

const MEMO: &str = "<?xml version=\"1.0\"?>\n<!DOCTYPE memo [\n  <!-- A memo. -->\n  <!ELEMENT memo (to+, from, body)>\n  <!ELEMENT to (#PCDATA)>\n  <!ELEMENT from (#PCDATA)>\n  <!ELEMENT body (#PCDATA | ref)*>\n  <!ELEMENT ref EMPTY>\n  <!-- Memo priority. -->\n  <!ATTLIST memo priority (low | normal | high) \"normal\" id ID #REQUIRED>\n  <!ATTLIST ref target IDREF #REQUIRED>\n  <!ENTITY company \"ACME\">\n  <!ENTITY % shared \"x\">\n]>\n";

#[test]
fn parses_tag_contexts() {
    assert_eq!(tag_context("me"), Some(TagContext::ElementName));
    assert_eq!(tag_context(""), Some(TagContext::ElementName));
    assert_eq!(
        tag_context("memo id=\"a\" pri"),
        Some(TagContext::AttributeName {
            element: "memo",
            present: vec!["id"],
        })
    );
    assert_eq!(
        tag_context("memo id='a b' priority=\"lo"),
        Some(TagContext::AttributeValue {
            element: "memo",
            attribute: "priority",
        })
    );
    assert_eq!(tag_context("memo id="), None);
    assert_eq!(tag_context("/memo"), None);
    assert_eq!(tag_context("!DOCTYPE"), None);
}

#[test]
fn completes_from_the_document_type_declaration() {
    let mut fixture = Fixture::new();
    let uri = "file:///tmp/memo.xml";
    let complete = |fixture: &mut Fixture, source: &str| {
        let grammar = fixture.grammar(uri, source);
        completions(grammar.as_ref(), uri, source, source.len())
    };

    // Root: name of the DOCTYPE.
    let source = format!("{MEMO}<");
    assert_eq!(labels(&complete(&mut fixture, &source)), vec!["memo"]);
    // Children allowed after the ones already present.
    let source = format!("{MEMO}<memo id=\"m1\"><to>a</to><");
    let items = complete(&mut fixture, &source);
    assert_eq!(labels(&items), vec!["from", "to"]);
    assert_eq!(items[0]["detail"], "<!ELEMENT from (#PCDATA)>");
    let source = format!("{MEMO}<memo id=\"m1\"><body>text <");
    assert_eq!(labels(&complete(&mut fixture, &source)), vec!["ref"]);
    // Attributes not present yet, with documentation.
    let source = format!("{MEMO}<memo id=\"m1\" ");
    let items = complete(&mut fixture, &source);
    assert_eq!(labels(&items), vec!["priority"]);
    assert_eq!(items[0]["insertText"], "priority=\"$1\"");
    assert_eq!(items[0]["insertTextFormat"], 2);
    assert_eq!(items[0]["documentation"]["value"], "Memo priority.");
    // Enumerated values and existing IDs for IDREF.
    let source = format!("{MEMO}<memo id=\"m1\" priority=\"");
    assert_eq!(
        labels(&complete(&mut fixture, &source)),
        vec!["low", "normal", "high"]
    );
    let source = format!("{MEMO}<memo id=\"m1\"><body><ref target=\"");
    assert_eq!(labels(&complete(&mut fixture, &source)), vec!["m1"]);
    // Entities after `&`, in text and attribute values.
    let source = format!("{MEMO}<memo id=\"m1\"><to>&co");
    let items = complete(&mut fixture, &source);
    assert_eq!(
        labels(&items),
        vec!["amp", "lt", "gt", "quot", "apos", "company"]
    );
    assert_eq!(items[5]["insertText"], "company;");
    assert_eq!(items[5]["detail"], "<!ENTITY company \"ACME\">");
    // Without a DTD: predefined entities only.
    let source = "<a b=\"&";
    assert_eq!(complete(&mut fixture, source).len(), 5);
    // Nothing in a comment.
    let source = format!("{MEMO}<memo id=\"m1\"><!-- &");
    assert!(complete(&mut fixture, &source).is_empty());
}

#[test]
fn completes_inside_dtd_text() {
    let mut fixture = Fixture::new();
    let uri = "file:///tmp/grammar.dtd";
    let complete = |fixture: &mut Fixture, source: &str| {
        let grammar = fixture.grammar(uri, source);
        completions(grammar.as_ref(), uri, source, source.len())
    };
    let base = "<!ENTITY % inline \"b | i\">\n<!ELEMENT p (#PCDATA)>\n<!ELEMENT b (#PCDATA)>\n";
    let items = complete(&mut fixture, &format!("{base}<!"));
    assert_eq!(
        labels(&items),
        vec!["ELEMENT", "ATTLIST", "ENTITY", "NOTATION"]
    );
    assert_eq!(items[0]["insertTextFormat"], 2);
    assert_eq!(
        labels(&complete(&mut fixture, &format!("{base}<!ELEMENT q (#"))),
        vec!["PCDATA", "REQUIRED", "IMPLIED", "FIXED"]
    );
    assert_eq!(
        labels(&complete(
            &mut fixture,
            &format!("{base}<!ELEMENT q (#PCDATA | %")
        )),
        vec!["inline"]
    );
    assert_eq!(
        labels(&complete(&mut fixture, &format!("{base}<!ELEMENT q (p, "))),
        // `q` is already declared by the current declaration (recursion allowed).
        vec!["p", "b", "q"]
    );
    assert_eq!(
        labels(&complete(&mut fixture, &format!("{base}<!ELEMENT q "))),
        vec!["EMPTY", "ANY"]
    );
    assert_eq!(
        labels(&complete(&mut fixture, &format!("{base}<!ATTLIST "))),
        vec!["p", "b"]
    );
    assert!(labels(&complete(&mut fixture, &format!("{base}<!ATTLIST p a "))).contains(&"CDATA"));
    assert!(complete(&mut fixture, &format!("{base}<!ENTITY x \"<")).is_empty());

    // Internal subset of an instance document.
    let uri = "file:///tmp/doc.xml";
    let source = "<!DOCTYPE r [\n  <!ELEMENT r EMPTY>\n  <!";
    let grammar = fixture.grammar(uri, source);
    assert!(in_dtd_text(grammar.as_ref(), uri, source, source.len()));
    assert_eq!(
        completions(grammar.as_ref(), uri, source, source.len()).len(),
        4
    );
}

#[test]
fn hovers_and_navigates_to_external_declarations() {
    let directory = directory("hover");
    let dtd_path = directory.join("memo.dtd");
    let dtd_text = "<!-- Recipient\n     of the memo. -->\n<!ELEMENT to (#PCDATA)>\n<!ELEMENT memo (to)>\n<!-- Level. -->\n<!ATTLIST memo level NMTOKEN #IMPLIED>\n<!ENTITY sign \"— ACME\">\n";
    fs::write(&dtd_path, dtd_text).expect("dtd should be written");
    let uri = path_to_uri(&directory.join("memo.xml"));
    let source =
        "<!DOCTYPE memo SYSTEM \"memo.dtd\">\n<memo level=\"1\"><to>Bob &sign;</to></memo>";
    let mut fixture = Fixture::new();
    let grammar = fixture.grammar(&uri, source).expect("grammar");
    assert!(
        grammar.dtd.problems.is_empty(),
        "{:?}",
        grammar.dtd.problems
    );

    let hover_on = |marker: &str, delta: usize| {
        hover(&grammar, &uri, source, at(source, marker) + delta).expect("hover")
    };
    let element = hover_on("<to>", 1);
    let markdown = element["contents"]["value"].as_str().unwrap();
    assert!(
        markdown.starts_with("```xml\n<!ELEMENT to (#PCDATA)>\n```"),
        "{markdown}"
    );
    assert!(markdown.contains("Recipient\nof the memo."), "{markdown}");
    assert!(
        markdown.contains("Source: [memo.dtd](file://"),
        "{markdown}"
    );
    assert_eq!(
        element["range"]["start"],
        json!({"line": 1, "character": 17})
    );
    let memo = hover_on("<memo", 2);
    assert!(
        memo["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("<!ATTLIST memo level NMTOKEN #IMPLIED>")
    );
    let attribute = hover_on("level=", 1);
    assert!(
        attribute["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("Level.")
    );
    let entity = hover_on("&sign;", 2);
    assert!(
        entity["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("<!ENTITY sign \"— ACME\">")
    );
    assert!(hover(&grammar, &uri, source, at(source, "Bob")).is_none());

    let definition_at = |marker: &str, delta: usize| {
        definition(&grammar, &uri, source, at(source, marker) + delta).expect("definition")
    };
    let target = definition_at("</to>", 3);
    assert_eq!(target[0]["uri"], path_to_uri(&dtd_path));
    assert_eq!(
        target[0]["range"],
        json!({"start": {"line": 2, "character": 10}, "end": {"line": 2, "character": 12}})
    );
    let target = definition_at("level", 0);
    assert_eq!(
        target[0]["range"]["start"],
        json!({"line": 5, "character": 15})
    );
    let target = definition_at("&sign;", 1);
    assert_eq!(
        target[0]["range"]["start"],
        json!({"line": 6, "character": 9})
    );

    // In the DTD file itself: name referenced in a model.
    let dtd_uri = path_to_uri(&dtd_path);
    let dtd_grammar = fixture.grammar(&dtd_uri, dtd_text).expect("grammar");
    let offset = at(dtd_text, "(to)") + 1;
    let target = definition(&dtd_grammar, &dtd_uri, dtd_text, offset).unwrap();
    assert_eq!(target[0]["uri"], dtd_uri);
    assert_eq!(
        target[0]["range"]["start"],
        json!({"line": 2, "character": 10})
    );
    let offset = at(dtd_text, "level");
    let hovered = hover(&dtd_grammar, &dtd_uri, dtd_text, offset).unwrap();
    let markdown = hovered["contents"]["value"].as_str().unwrap();
    assert!(markdown.contains("Level."), "{markdown}");
    // No "Source" link to the file itself.
    assert!(!markdown.contains("Source"), "{markdown}");
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn publishes_grammar_entity_and_validation_diagnostics() {
    let directory = directory("diagnostics");
    fs::write(
        directory.join("broken.dtd"),
        "<!ELEMENT r (a)>\n<!ELEMENT a (#PCDATA)>\n<!ELEMENT a EMPTY>\n<!BOGUS>",
    )
    .expect("dtd should be written");
    fs::write(
        directory.join("ok.dtd"),
        "<!ELEMENT r (a)>\n<!ELEMENT a (#PCDATA)>",
    )
    .expect("dtd should be written");
    let uri = path_to_uri(&directory.join("doc.xml"));
    let mut fixture = Fixture::new();

    // Errors of an external file summarized on the system identifier.
    let source = "<!DOCTYPE r SYSTEM \"broken.dtd\"><r><a/><b/></r>";
    let diagnostics = fixture.diagnostics(&uri, source);
    assert_eq!(
        kinds(&diagnostics),
        vec![
            kind("dtd-grammar", "externalGrammar", 1),
            kind("dtd-validation", "unexpectedElement", 1),
            kind("dtd-validation", "undeclaredElement", 1),
        ]
    );
    let summary = diagnostics[0]["message"].as_str().unwrap();
    assert!(summary.starts_with("2 errors in the DTD"), "{summary}");
    assert!(summary.contains("(broken.dtd:3)"), "{summary}");
    assert_eq!(
        diagnostics[0]["range"],
        json!({"start": {"line": 0, "character": 20}, "end": {"line": 0, "character": 30}})
    );

    // Valid document except for an unknown entity.
    let source = "<!DOCTYPE r SYSTEM \"ok.dtd\"><r><a>&x; &amp;</a></r>";
    assert_eq!(
        kinds(&fixture.diagnostics(&uri, source)),
        vec![kind("xml-entity", "undefinedEntity", 1)]
    );
    // Remote DTD: warning, no validation, unknown entities as
    // warnings.
    let source = "<!DOCTYPE r SYSTEM \"http://example.com/r.dtd\"><r><zzz>&x;</zzz></r>";
    let diagnostics = fixture.diagnostics(&uri, source);
    assert_eq!(
        kinds(&diagnostics),
        vec![
            kind("dtd-grammar", "externalLoad", 2),
            kind("xml-entity", "undefinedEntity", 2),
        ]
    );
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .contains("xml.catalogs")
    );
    // Missing file: error.
    let source = "<!DOCTYPE r SYSTEM \"absent.dtd\"><r/>";
    assert_eq!(
        kinds(&fixture.diagnostics(&uri, source)),
        vec![kind("dtd-grammar", "externalLoad", 1)]
    );
    // Without a DOCTYPE: unknown entities only.
    assert_eq!(
        kinds(&fixture.diagnostics(&uri, "<r>&nbsp;</r>")),
        vec![kind("xml-entity", "undefinedEntity", 1)]
    );
    // DOCTYPE not allowed: no DTD analysis.
    let disallowed = ValidationSettings {
        disallow_doc_type_decl: true,
        ..ValidationSettings::default()
    };
    let source = "<!DOCTYPE r [<!ELEMENT r EMPTY>]><r>&x;<b/></r>";
    assert!(
        fixture
            .diagnostics_with(&uri, source, &disallowed)
            .is_empty()
    );
    // Unknown `&x;` makes the content of `r` opaque: no EMPTY error.
    assert_eq!(
        kinds(&fixture.diagnostics(&uri, source)),
        vec![
            kind("xml-entity", "undefinedEntity", 1),
            kind("dtd-validation", "undeclaredElement", 1),
        ]
    );

    // External entities: checked only with resolveExternalEntities.
    let source = "<!DOCTYPE r [<!ELEMENT r ANY><!ENTITY chap SYSTEM \"chap.xml\"><!ENTITY here SYSTEM \"ok.dtd\">]><r>&chap;&here;</r>";
    assert!(fixture.diagnostics(&uri, source).is_empty());
    let resolving = ValidationSettings {
        resolve_external_entities: true,
        ..ValidationSettings::default()
    };
    assert_eq!(
        kinds(&fixture.diagnostics_with(&uri, source, &resolving)),
        vec![kind("xml-entity", "externalEntity", 2)]
    );

    // Open DTD file: errors in place.
    let dtd_uri = path_to_uri(&directory.join("edit.dtd"));
    let diagnostics = fixture.diagnostics(
        &dtd_uri,
        "<!ELEMENT a (b,)>\n<!ENTITY % m SYSTEM \"https://example.com/m.ent\">\n%m;",
    );
    assert_eq!(
        kinds(&diagnostics),
        vec![
            kind("dtd-grammar", "dtdSyntax", 1),
            kind("dtd-grammar", "externalLoad", 2),
        ]
    );
    assert_eq!(
        diagnostics[0]["range"]["start"],
        json!({"line": 0, "character": 15})
    );
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn resolves_external_subsets_through_catalogs_and_open_buffers() {
    let directory = directory("catalog");
    fs::create_dir_all(directory.join("dtds")).unwrap();
    fs::write(directory.join("dtds/note.dtd"), "<!ELEMENT note (#PCDATA)>").unwrap();
    let catalog = directory.join("catalog.xml");
    fs::write(
            &catalog,
            "<catalog xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n  <public publicId=\"-//ACME//DTD Note//EN\" uri=\"dtds/note.dtd\"/>\n</catalog>",
        )
        .unwrap();
    let mut fixture = Fixture::new();
    fixture.catalogs.set_roots(vec![catalog]);
    let uri = path_to_uri(&directory.join("note.xml"));
    let source = "<!DOCTYPE note PUBLIC \"-//ACME//DTD Note//EN\" \"http://example.com/note.dtd\"><note><x/></note>";
    assert_eq!(
        kinds(&fixture.diagnostics(&uri, source)),
        vec![
            kind("dtd-validation", "unexpectedElement", 1),
            kind("dtd-validation", "undeclaredElement", 1),
        ]
    );
    let grammar = fixture.grammar(&uri, source).unwrap();
    assert_eq!(
        grammar.dtd.source_path(1),
        Some(directory.join("dtds/note.dtd").as_path())
    );

    // Open buffer takes precedence over the disk (even when unsaved).
    let buffer = directory.join("buffer.dtd");
    fixture.documents.insert(
        path_to_uri(&buffer),
        "<!ELEMENT note (x)><!ELEMENT x EMPTY>".to_owned(),
    );
    let source = "<!DOCTYPE note SYSTEM \"buffer.dtd\"><note><x/></note>";
    assert!(fixture.diagnostics(&uri, source).is_empty());
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn offers_quick_fixes_for_dtd_problems() {
    let fixes = |source: &str, range: Range<usize>| {
        let mut fixture = Fixture::new();
        let uri = "file:///tmp/fix.xml";
        let grammar = fixture.grammar(uri, source);
        let context = json!({});
        let mut actions = Actions::new(uri, source, range, &context);
        code_actions(&mut actions, grammar.as_ref());
        actions
            .actions
            .iter()
            .map(|action| {
                let edits = action["edit"]["changes"][uri].clone();
                (
                    action["title"].as_str().unwrap().to_owned(),
                    crate::formatting::apply_edits(source, &edits),
                )
            })
            .collect::<Vec<_>>()
    };

    let source = "<!DOCTYPE r [\n  <!ELEMENT r ANY>\n]>\n<r>&x;</r>";
    let offset = at(source, "&x;");
    assert_eq!(
        fixes(source, offset..offset),
        vec![(
            "Declare the entity '&x;' in the DOCTYPE".to_owned(),
            "<!DOCTYPE r [\n  <!ELEMENT r ANY>\n  <!ENTITY x \"\">\n]>\n<r>&x;</r>".to_owned()
        )]
    );
    let source = "<!DOCTYPE r SYSTEM \"r.dtd\"><r>&x;</r>";
    let offset = at(source, "&x;");
    assert_eq!(
        fixes(source, offset..offset)[0].1,
        "<!DOCTYPE r SYSTEM \"r.dtd\" [\n  <!ENTITY x \"\">\n]><r>&x;</r>"
    );
    let source = "<?xml version=\"1.0\"?>\n<r a=\"&x;\"/>";
    let offset = at(source, "&x;");
    assert_eq!(
        fixes(source, offset..offset)[0].1,
        "<?xml version=\"1.0\"?>\n<!DOCTYPE r [\n  <!ENTITY x \"\">\n]>\n<r a=\"&x;\"/>"
    );

    let source = "<!DOCTYPE r [<!ELEMENT r EMPTY><!ATTLIST r kind (a|b) #REQUIRED mode (x) #FIXED \"x\">]><r mode=\"y\"  />";
    assert_eq!(
        fixes(source, 0..source.len()),
        vec![
            (
                "Add required attribute 'kind'".to_owned(),
                source.replace("mode=\"y\"  />", "mode=\"y\" kind=\"a\"  />")
            ),
            (
                "Replace with the fixed value 'x'".to_owned(),
                source.replace("mode=\"y\"", "mode=\"x\"")
            ),
        ]
    );
}

#[test]
fn lists_dtd_declarations_as_symbols() {
    let mut fixture = Fixture::new();
    let uri = "file:///tmp/symbols.dtd";
    let source = "<!ENTITY % common \"id ID #IMPLIED\">\n<!ELEMENT book (title)>\n<!ATTLIST book %common; lang CDATA #IMPLIED>\n<!ELEMENT title (#PCDATA)>\n<!ENTITY c \"©\">\n<!NOTATION gif SYSTEM \"g\">";
    let grammar = fixture.grammar(uri, source).unwrap();
    let symbols = document_symbols(&grammar, uri, source, true);
    let names = symbols
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["%common", "book", "title", "&c", "gif"]);
    let children = symbols[1]["children"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0]["name"], "id");
    // Attribute coming from `%common;`: selection on the reference.
    assert_eq!(
        children[0]["selectionRange"]["start"],
        json!({"line": 2, "character": 15})
    );
    assert_eq!(children[1]["detail"], "CDATA #IMPLIED");
    let flat = document_symbols(&grammar, uri, source, false);
    assert_eq!(flat.len(), 7);
    assert_eq!(flat[1]["containerName"], "book");
    assert_eq!(flat[1]["location"]["uri"], uri);
}
