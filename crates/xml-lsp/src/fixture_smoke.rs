//! Runs every request the server implements over every shared fixture
//! (`tests/fixtures`: hand-written cases, real-world documents and parser
//! corpora, well-formed or not) at positions spread through each document.
//!
//! The requests must not panic, every range they return must lie inside the
//! document with its start before its end, and formatting through the LSP
//! must be idempotent on well-formed documents.

use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use xml_conformance::{decode, fixtures_dir};

use crate::{XmlLanguageServer, formatting, path_to_uri, position_at};

/// Positions sampled per document, in addition to its start and end.
const SAMPLES: usize = 48;
/// Positions sampled in documents larger than [`LARGE_DOCUMENT`] bytes, so the
/// test stays fast in debug builds (several requests are still linear or
/// worse in the document size).
const LARGE_DOCUMENT_SAMPLES: usize = 4;
const LARGE_DOCUMENT: usize = 16 * 1024;
/// Documents above this size take minutes in a debug build (document
/// symbols, folding and references are superlinear today): they run in
/// release builds only, as in the CI conformance job.
const HUGE_DOCUMENT: usize = 64 * 1024;

/// Problems known today, as `(fixture path suffix, problem)`. The test fails
/// when one of them no longer occurs, so fixes are recorded here.
const KNOWN_PROBLEMS: &[(&str, &str)] = &[
    // `<!DOCTYPE doc>[]>` is not well-formed but raises no diagnostic, and
    // formatting it twice gives two different results.
    (
        "corpus/libxml2-errors/doctype1.xml",
        "formatting through the LSP is not idempotent",
    ),
];

#[test]
fn every_request_handles_every_fixture() {
    let mut files = Vec::new();
    collect(&fixtures_dir(), &mut files);
    files.sort();
    assert!(files.len() > 150, "the shared fixtures should be present");

    let mut failures = Vec::new();
    for path in &files {
        let Some(source) = fs::read(path).ok().and_then(|bytes| decode(&bytes)) else {
            continue;
        };
        if cfg!(debug_assertions) && source.len() > HUGE_DOCUMENT {
            eprintln!("skipped in debug builds: {}", path.display());
            continue;
        }
        let result = catch_unwind(AssertUnwindSafe(|| exercise(path, &source)));
        match result {
            Ok(problems) => {
                failures.extend(problems.into_iter().map(|problem| (path.clone(), problem)))
            }
            Err(_) => failures.push((path.clone(), "a request panicked".to_owned())),
        }
    }
    let root = fixtures_dir();
    let mut unexpected = Vec::new();
    let mut seen = Vec::new();
    for (path, problem) in &failures {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        match KNOWN_PROBLEMS
            .iter()
            .position(|known| *known == (relative.as_str(), problem.as_str()))
        {
            Some(index) => seen.push(index),
            None => unexpected.push(format!("{relative}: {problem}")),
        }
    }
    let fixed = (0..KNOWN_PROBLEMS.len())
        .filter(|index| !seen.contains(index))
        .map(|index| KNOWN_PROBLEMS[index])
        .collect::<Vec<_>>();
    assert!(
        unexpected.is_empty() && fixed.is_empty(),
        "{} new problem(s):\n{}\nfixed, remove from KNOWN_PROBLEMS: {fixed:?}",
        unexpected.len(),
        unexpected.join("\n")
    );
}

/// Extensions of real-world fixtures that the extension's `XML` language
/// deliberately does not claim (another Zed extension does, or the suffix is
/// too generic): see "File types" in README.md.
const NOT_CLAIMED_BY_THE_XML_LANGUAGE: &[&str] = &["config", "csproj", "props", "rdf"];

#[test]
fn the_xml_language_claims_the_real_world_fixtures() {
    let mut files = Vec::new();
    collect(&fixtures_dir().join("real-world"), &mut files);
    let unclaimed = files
        .iter()
        .filter(|path| {
            let name = path.file_name().and_then(|name| name.to_str()).unwrap();
            !crate::symbols::is_xml_file_name(name)
                && !path.extension().is_some_and(|extension| {
                    NOT_CLAIMED_BY_THE_XML_LANGUAGE.contains(&extension.to_str().unwrap())
                })
        })
        .collect::<Vec<_>>();
    assert!(unclaimed.is_empty(), "{unclaimed:?}");
}

fn exercise(path: &Path, source: &str) -> Vec<String> {
    exercise_offsets(path, source, sample_offsets(source))
}

/// Runs every request on `source`, the positional ones at `offsets`.
fn exercise_offsets(path: &Path, source: &str, offsets: Vec<usize>) -> Vec<String> {
    let uri = path_to_uri(path);
    let mut server = XmlLanguageServer::new();
    server.documents.insert(uri.clone(), source.to_owned());
    let document = json!({"uri": uri});
    let whole = json!({"start": position_at(source, 0), "end": position_at(source, source.len())});
    let mut problems = Vec::new();
    let mut check = |request: &str, response: Option<Value>| {
        if let Some(response) = response {
            check_ranges(source, request, &response, &mut problems);
        }
    };

    let diagnostics = server.diagnostics(&uri, source);
    check("publishDiagnostics", Some(diagnostics.clone()));
    check(
        "documentSymbol",
        server.symbols(&json!({"textDocument": document})),
    );
    check(
        "foldingRange",
        server.folding_range(&json!({"textDocument": document})),
    );
    check(
        "documentLink",
        server.document_links(&json!({"textDocument": document})),
    );
    check(
        "documentColor",
        server.document_colors(&json!({"textDocument": document})),
    );
    check(
        "workspaceSymbol",
        Some(server.workspace_symbols(&json!({"query": ""}))),
    );
    check(
        "codeAction",
        server.code_action(&json!({
            "textDocument": document,
            "range": whole,
            "context": {"diagnostics": diagnostics["diagnostics"]},
        })),
    );
    check(
        "rangeFormatting",
        server.range_formatting(&json!({
            "textDocument": document,
            "range": whole,
            "options": {"tabSize": 2, "insertSpaces": true},
        })),
    );

    let positions = offsets
        .into_iter()
        .map(|offset| position_at(source, offset))
        .collect::<Vec<_>>();
    check(
        "selectionRange",
        server.selection_range(&json!({"textDocument": document, "positions": positions})),
    );
    for position in &positions {
        let at = json!({"textDocument": document, "position": position});
        check("hover", server.hover(&at));
        check("completion", server.completion(&at));
        check("documentHighlight", server.document_highlight(&at));
        check("linkedEditingRange", server.linked_editing_range(&at));
        check("definition", server.definition(&at));
        check(
            "references",
            server.references(&json!({
                "textDocument": document,
                "position": position,
                "context": {"includeDeclaration": true},
            })),
        );
        if let Some(prepared) = server.prepare_rename(&at) {
            check("prepareRename", Some(prepared));
            let rename =
                json!({"textDocument": document, "position": position, "newName": "renamed"});
            check("rename", server.rename(&rename).ok().flatten());
        }
    }

    // Formatting through the LSP is idempotent on well-formed documents.
    let format = json!({"textDocument": document, "options": {"tabSize": 2, "insertSpaces": true}});
    if xml_core::parse_xml(source).diagnostics.is_empty()
        && let Some(edits) = server.formatting(&format)
    {
        check("formatting", Some(edits.clone()));
        let formatted = formatting::apply_edits(source, &edits);
        server.documents.insert(uri.clone(), formatted.clone());
        server.analyses.invalidate(&uri);
        if let Some(again) = server.formatting(&format) {
            let reformatted = formatting::apply_edits(&formatted, &again);
            if reformatted != formatted {
                problems.push("formatting through the LSP is not idempotent".to_owned());
            }
        }
    }
    problems
}

/// Small documents mixing what usually breaks offset arithmetic: a byte
/// order mark, CRLF, characters outside the BMP, truncated markup, DTD
/// internal subsets and namespaces.
const TRICKY_DOCUMENTS: &[&str] = &[
    "\u{FEFF}<?xml version=\"1.0\"?>\r\n<é𝄞 a=\"1\" b='2'>\r\n  <x:y xmlns:x=\"urn:x\">t&amp;𝄞</x:y>\r\n</é𝄞>",
    "<!DOCTYPE r [\r\n<!ELEMENT r (a|b)*>\r\n<!ATTLIST r id ID #IMPLIED>\r\n<!ENTITY e \"é\">\r\n]>\r\n<r id=\"1\">&e;<a/><",
    "<r><!-- 𝄞 --><![CDATA[<é>]]><?pi 𝄞?><a b=\"\r\n",
    "<a\r\n  b=\"𝄞\"\r\n  c",
    "</é><é/>< é>&;&#xZ;<a:b:c/>\r\n",
    "\r\n\r\n",
    "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:element name=\"é\" type=\"xs:str",
    "<?xml-model href=\"missing.rng\"?><r xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:schemaLocation=\"urn:a missing.xsd urn:b\"><",
    "<!DOCTYPE r [<!ENTITY a \"&b;\"><!ENTITY b \"&a;\"><!ENTITY % p \"<!ELEMENT\"> %p; r ANY>é]><r>&a;</r>",
    "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect fill=\"#fé0\" stroke=\"rgb(1,2,\"/></svg>",
];

#[test]
fn every_request_handles_every_offset_of_tricky_documents() {
    for source in TRICKY_DOCUMENTS {
        for extension in ["xml", "xsd", "dtd"] {
            let path = std::env::temp_dir().join(format!("xml-lsp-tricky/document.{extension}"));
            let result = catch_unwind(AssertUnwindSafe(|| {
                exercise_offsets(&path, source, all_offsets(source))
            }));
            match result {
                Ok(problems) => assert!(problems.is_empty(), "{source:?}: {problems:?}"),
                Err(_) => panic!("a request panicked on {source:?} ({extension})"),
            }
        }
    }
}

/// Every character boundary of `source`.
fn all_offsets(source: &str) -> Vec<usize> {
    source
        .char_indices()
        .map(|(offset, _)| offset)
        .chain([source.len()])
        .collect()
}

/// Offsets on character boundaries spread over the document, plus its ends
/// and the positions right after each of the first `<`.
fn sample_offsets(source: &str) -> Vec<usize> {
    let boundaries = source
        .char_indices()
        .map(|(offset, _)| offset)
        .chain([source.len()])
        .collect::<Vec<_>>();
    let samples = if source.len() > LARGE_DOCUMENT {
        LARGE_DOCUMENT_SAMPLES
    } else {
        SAMPLES
    };
    let step = (boundaries.len() / samples).max(1);
    let mut offsets = boundaries.iter().step_by(step).copied().collect::<Vec<_>>();
    offsets.extend(
        source
            .match_indices('<')
            .take(samples / 2)
            .map(|(offset, _)| offset + 1),
    );
    offsets.push(source.len());
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

/// Checks every `{"start": position, "end": position}` object in `response`.
fn check_ranges(source: &str, request: &str, response: &Value, problems: &mut Vec<String>) {
    match response {
        Value::Object(object) => {
            if let (Some(start), Some(end)) = (object.get("start"), object.get("end"))
                && let (Some(start), Some(end)) = (position(start), position(end))
            {
                let valid = in_document(source, start) && in_document(source, end) && start <= end;
                if !valid {
                    problems.push(format!(
                        "{request} returned an invalid range {start:?}..{end:?}"
                    ));
                }
            }
            for value in object.values() {
                check_ranges(source, request, value, problems);
            }
        }
        Value::Array(values) => {
            for value in values {
                check_ranges(source, request, value, problems);
            }
        }
        _ => {}
    }
}

fn position(value: &Value) -> Option<(u64, u64)> {
    Some((
        value.get("line")?.as_u64()?,
        value.get("character")?.as_u64()?,
    ))
}

/// Whether a (line, UTF-16 character) position lies inside `source`.
fn in_document(source: &str, (line, character): (u64, u64)) -> bool {
    let mut lines = source.split('\n');
    let Some(text) = lines.nth(line as usize) else {
        return false;
    };
    let text = text.strip_suffix('\r').unwrap_or(text);
    character <= text.encode_utf16().count() as u64
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap().filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            if !path.ends_with("licenses") {
                collect(&path, files);
            }
        } else if !path
            .extension()
            .is_some_and(|extension| extension == "md" || extension == "txt")
        {
            files.push(path);
        }
    }
}
