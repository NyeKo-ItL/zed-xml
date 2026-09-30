//! W3C XML Conformance Test Suite (xmlts 20130923), as packaged by the
//! `xml-conformance-suite` npm package (MIT for the packaging, W3C Software
//! Notice and License for the suite). Fetched by
//! `scripts/fetch-test-suites.sh`; skipped when absent.
//!
//! `xml-core` is a non-validating, namespace-aware checker that does not read
//! external entities, so the selection follows XML 1.0 fifth edition section
//! 5.1 for such processors:
//!
//! - `valid` and `invalid` documents must be reported well-formed (validity
//!   against the DTD is not checked);
//! - `not-wf` documents must be reported with at least one diagnostic;
//! - `error` cases (optional errors), XML 1.1 cases, cases superseded by the
//!   fifth edition and cases meant for namespace-unaware processors are
//!   skipped.

use std::{fs, path::Path};

use xml_conformance::{Outcome, SuiteRun, decode, external_suite, guarded, server_errors};

#[test]
fn w3c_xml_conformance_suite() {
    let Some(suite) = external_suite("xmlconf", "cleaned/xmlconf-flattened.xml") else {
        return;
    };
    let manifest = fs::read_to_string(suite.join("cleaned/xmlconf-flattened.xml")).unwrap();
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..roxmltree::ParsingOptions::default()
    };
    let manifest = roxmltree::Document::parse_with_options(&manifest, options)
        .expect("the flattened manifest should parse");
    let base_directory = suite.join("xmlconf");

    let mut run = SuiteRun::new("xmlconf");
    for test in manifest
        .descendants()
        .filter(|node| node.has_tag_name("TEST"))
    {
        let id = test.attribute("ID").expect("every test has an ID");
        let kind = test.attribute("TYPE").unwrap_or_default();
        let uri = test.attribute("URI").expect("every test has a URI");
        let path = base_of(test, &base_directory).join(uri);

        let outcome = if let Some(reason) = skip_reason(test) {
            Outcome::Skip(reason)
        } else {
            guarded(|| run_case(kind, &path))
        };
        run.record(id, outcome);
    }
    run.check_against_baseline();
}

fn base_of(node: roxmltree::Node<'_, '_>, root: &Path) -> std::path::PathBuf {
    let mut bases = node
        .ancestors()
        .filter_map(|ancestor| ancestor.attribute(("http://www.w3.org/XML/1998/namespace", "base")))
        .collect::<Vec<_>>();
    bases.reverse();
    bases
        .into_iter()
        .fold(root.to_path_buf(), |path, base| path.join(base))
}

fn skip_reason(test: roxmltree::Node<'_, '_>) -> Option<String> {
    let kind = test.attribute("TYPE").unwrap_or_default();
    if kind == "error" {
        return Some("optional error".to_owned());
    }
    if test.attribute("VERSION") == Some("1.1")
        || test
            .attribute("RECOMMENDATION")
            .is_some_and(|recommendation| {
                recommendation.starts_with("XML1.1") || recommendation.starts_with("NS1.1")
            })
    {
        return Some("XML 1.1".to_owned());
    }
    if test
        .attribute("EDITION")
        .is_some_and(|editions| !editions.split_whitespace().any(|edition| edition == "5"))
    {
        return Some("superseded by the fifth edition".to_owned());
    }
    if test.attribute("NAMESPACE") == Some("no") {
        return Some("namespace-unaware processors only".to_owned());
    }
    None
}

fn run_case(kind: &str, path: &Path) -> Outcome {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return Outcome::Skip(format!("unreadable: {error}")),
    };
    let Some(source) = decode(&bytes) else {
        // An editor cannot open such a file as text: nothing reaches the
        // server. Invalid byte sequences are a not-wf reason, so the case
        // passes when it expects an error.
        return if kind == "not-wf" {
            Outcome::Pass
        } else {
            Outcome::Skip("encoding not supported by the test driver".to_owned())
        };
    };
    let errors = server_errors(&source, Some(path), kind == "valid");
    match (kind, errors.is_empty()) {
        ("valid" | "invalid", true) | ("not-wf", false) => Outcome::Pass,
        ("valid" | "invalid", false) => {
            Outcome::Fail(format!("reported not well-formed: {}", errors.join("; ")))
        }
        ("not-wf", true) => Outcome::Fail("accepted a not well-formed document".to_owned()),
        _ => Outcome::Skip(format!("unknown test type {kind}")),
    }
}
