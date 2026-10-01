//! Latency budgets of the requests on a large document bound to a schema
//! (about 1 MB, as in `benches/server.rs`): the requests Zed sends on every
//! cursor move must stay interactive, whole-document ones bounded.
//!
//! Budgets are deliberately generous (roughly 10 to 20 times what a release
//! build needs on a CI runner) so the test only catches algorithmic
//! regressions, not noise. They are only enforced in release builds
//! (`cargo test --release -p xml-lsp latency`); debug builds run the
//! requests without measuring.

use std::{
    fs,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{XmlLanguageServer, path_to_uri, position_at};

const SCHEMA: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="catalog"><xs:complexType><xs:sequence>
    <xs:element ref="book" minOccurs="0" maxOccurs="unbounded"/>
  </xs:sequence></xs:complexType></xs:element>
  <xs:element name="book"><xs:complexType><xs:sequence>
    <xs:element name="title" type="xs:string"/>
    <xs:element name="author" type="xs:string" maxOccurs="unbounded"/>
    <xs:element name="price" type="xs:decimal"/>
    <xs:element name="format" type="Format"/>
  </xs:sequence>
  <xs:attribute name="id" type="xs:ID" use="required"/>
  </xs:complexType></xs:element>
  <xs:simpleType name="Format"><xs:restriction base="xs:string">
    <xs:enumeration value="paperback"/><xs:enumeration value="ebook"/>
  </xs:restriction></xs:simpleType>
</xs:schema>
"#;

fn instance(size: usize) -> String {
    let mut text = String::from(
        "<catalog xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"catalog.xsd\">\n",
    );
    let mut index = 0;
    while text.len() < size {
        text.push_str(&format!(
            "  <book id=\"b{index}\"><title>Title {index}</title><author>A</author><price>{index}.5</price><format>ebook</format></book>\n"
        ));
        index += 1;
    }
    text.push_str("</catalog>\n");
    text
}

/// Runs `request` (twice: the first call fills the caches, the second is what
/// an editor sees while typing) and fails when the second exceeds `budget`.
fn within(name: &str, budget: Duration, mut request: impl FnMut() -> Option<Value>) {
    request();
    let start = Instant::now();
    let response = request();
    let elapsed = start.elapsed();
    eprintln!("{name}: {elapsed:?} (budget {budget:?})");
    // An answer is proportional to the document, not to its nesting.
    if let Some(response) = response {
        assert!(
            response.to_string().len() < 64 * 1024 * 1024,
            "{name} answered more than 64 MB"
        );
    }
    if !cfg!(debug_assertions) {
        assert!(
            elapsed <= budget,
            "{name} took {elapsed:?}, budget {budget:?}"
        );
    }
}

#[test]
fn requests_on_a_large_document_stay_within_their_latency_budget() {
    let directory = std::env::temp_dir().join(format!("xml-lsp-latency-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("catalog.xsd"), SCHEMA).unwrap();
    let size = if cfg!(debug_assertions) {
        64 * 1024
    } else {
        1024 * 1024
    };
    let source = instance(size);
    let path = directory.join("catalog.xml");
    fs::write(&path, &source).unwrap();
    let uri = path_to_uri(&path);

    let mut server = XmlLanguageServer::new();
    server.documents.insert(uri.clone(), source.clone());
    let document = json!({"uri": uri});
    let middle = source.len() / 2;
    let middle = source[middle..]
        .find("<title>")
        .map_or(middle, |offset| middle + offset + "<tit".len());
    let at = json!({"textDocument": document, "position": position_at(&source, middle)});

    // Per cursor move.
    within("hover", Duration::from_millis(250), || server.hover(&at));
    within("completion", Duration::from_millis(250), || {
        server.completion(&at)
    });
    within("documentHighlight", Duration::from_millis(250), || {
        server.document_highlight(&at)
    });
    within("linkedEditingRange", Duration::from_millis(250), || {
        server.linked_editing_range(&at)
    });
    within("definition", Duration::from_millis(250), || {
        server.definition(&at)
    });
    within("selectionRange", Duration::from_millis(250), || {
        server.selection_range(&json!({"textDocument": document, "positions": [at["position"]]}))
    });
    // Whole document, after a change.
    within("diagnostics", Duration::from_secs(4), || {
        Some(server.diagnostics(&uri, &source))
    });
    within("documentSymbol", Duration::from_secs(2), || {
        server.symbols(&json!({"textDocument": document}))
    });
    within("foldingRange", Duration::from_secs(2), || {
        server.folding_range(&json!({"textDocument": document}))
    });
    within("codeAction", Duration::from_secs(2), || {
        server.code_action(&json!({
            "textDocument": document,
            "range": {"start": at["position"], "end": at["position"]},
            "context": {"diagnostics": []},
        }))
    });
    within("formatting", Duration::from_secs(4), || {
        server.formatting(&json!({
            "textDocument": document,
            "options": {"tabSize": 2, "insertSpaces": true},
        }))
    });
    within("rangeFormatting", Duration::from_secs(2), || {
        server.range_formatting(&json!({
            "textDocument": document,
            "range": {"start": at["position"], "end": at["position"]},
            "options": {"tabSize": 2, "insertSpaces": true},
        }))
    });
    let _ = fs::remove_dir_all(&directory);
}
