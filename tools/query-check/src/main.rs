//! Compiles every tree-sitter query of `languages/` against the pinned
//! grammars (Zed silently drops a query that does not compile) and checks how
//! the highlighting behaves on documents being edited: fragments without a
//! root element, unclosed tags, several roots.
//!
//! `scripts/check-queries.sh` fetches the grammar and runs this program.

use std::{fs, path::Path, process::ExitCode};

use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

unsafe extern "C" {
    fn tree_sitter_xml() -> *const ();
    fn tree_sitter_dtd() -> *const ();
}

fn language(name: &str) -> Language {
    // SAFETY: the functions are the grammars' entry points, linked by build.rs.
    let raw = unsafe {
        match name {
            "xml" => tree_sitter_xml(),
            _ => tree_sitter_dtd(),
        }
    };
    unsafe { Language::from_raw(raw.cast()) }
}

/// Captures of `query` on `source`, as `name:text`.
fn captures(language: &Language, query: &Query, source: &str) -> Vec<String> {
    let mut parser = Parser::new();
    parser.set_language(language).expect("grammar version");
    let tree = parser.parse(source, None).expect("parse");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.captures(query, tree.root_node(), source.as_bytes());
    let mut result = Vec::new();
    while let Some((found, index)) = matches.next() {
        let capture = found.captures[*index];
        result.push(format!(
            "{}:{}",
            query.capture_names()[capture.index as usize],
            &source[capture.node.byte_range()]
        ));
    }
    result
}

/// Documents in the state they are in while being typed, with the captures
/// that must still be there (highlighting must degrade gracefully).
const FRAGMENTS: &[(&str, &[&str])] = &[
    ("<a/>", &["tag:a"]),
    ("<a x=\"1\"/><b>t</b>", &["tag:a", "property:x", "string:\"1\"", "tag:b"]),
    (
        "<a>\n  <b x=\"1\"/>\n</a>\n<c k='v'>x</c>",
        &["tag:a", "tag:b", "property:x", "tag:c", "property:k", "string:'v'"],
    ),
    ("<a>\n  <b x=\"1\"", &["tag:a", "property:x", "string:\"1\""]),
    ("<root><child a=\"1\">text &amp; more</child>\n<other>", &["tag:root", "tag:child", "property:a", "constant:&amp;", "tag:other"]),
    ("<a><b></a>", &["tag:a", "tag:b"]),
    ("<!-- c --><a/>", &["comment:<!-- c -->", "tag:a"]),
    ("<![CDATA[x]]><a/>", &["tag:a"]),
];

fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../languages");
    let mut failures = Vec::new();
    for name in ["xml", "dtd"] {
        let grammar = language(name);
        let directory = root.join(name);
        let mut queries = fs::read_dir(&directory)
            .expect("languages directory")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "scm"))
            .collect::<Vec<_>>();
        queries.sort();
        for path in &queries {
            let text = fs::read_to_string(path).expect("query");
            match Query::new(&grammar, &text) {
                Ok(_) => println!("ok      {}", path.display()),
                Err(error) => {
                    println!("INVALID {}: {error}", path.display());
                    failures.push(format!("{} does not compile: {error}", path.display()));
                }
            }
        }
        if name == "xml"
            && let Ok(highlights) = Query::new(&grammar, &fs::read_to_string(directory.join("highlights.scm")).unwrap_or_default())
        {
            for (source, expected) in FRAGMENTS {
                let found = captures(&grammar, &highlights, source);
                for capture in *expected {
                    if !found.iter().any(|found| found == capture) {
                        failures.push(format!("{source:?}: missing capture {capture} in {found:?}"));
                    }
                }
                // Blanket error highlighting would tint everything after the
                // first mistake of a document being edited.
                if found.iter().any(|found| found.starts_with("error:")) {
                    failures.push(format!("{source:?}: the whole fragment is captured as an error"));
                }
            }
        }
    }
    if failures.is_empty() {
        println!("all queries compile and fragments keep their highlighting");
        ExitCode::SUCCESS
    } else {
        for failure in &failures {
            eprintln!("FAIL {failure}");
        }
        ExitCode::FAILURE
    }
}
