//! W3C XML Schema test suite (`w3c/xsdtests`, W3C licence), fetched by
//! `scripts/fetch-test-suites.sh`; skipped when absent.
//!
//! Only XSD 1.0 expectations and tests whose status is `accepted` are run
//! (queried or disputed tests are skipped). A schema test passes when
//! `xsd-core` loads a valid schema set and rejects an invalid one; an
//! instance test passes when validation reports no diagnostic for a valid
//! instance and at least one for an invalid instance (well-formedness errors
//! count, as the server reports them together).

use std::{
    fs,
    path::{Path, PathBuf},
};

use xml_conformance::{
    Outcome, SuiteRun, decode, external_suite, guarded, load_schema_set, well_formedness_errors,
};
use xsd_core::{XsdSchema, merge_schemas, validate_document_located};

const XLINK: &str = "http://www.w3.org/1999/xlink";

#[test]
fn w3c_xsd_test_suite() {
    let Some(suite) = external_suite("xsdtests", "suite.xml") else {
        return;
    };
    let mut run = SuiteRun::new("xsts");
    let suite_manifest = fs::read_to_string(suite.join("suite.xml")).unwrap();
    let suite_manifest = roxmltree::Document::parse(&suite_manifest).unwrap();
    for reference in suite_manifest
        .descendants()
        .filter(|node| node.has_tag_name("testSetRef"))
    {
        let href = reference.attribute((XLINK, "href")).unwrap();
        let path = suite.join(href);
        run_test_set(&path, &mut run);
    }
    run.check_against_baseline();
}

fn run_test_set(path: &Path, run: &mut SuiteRun) {
    let source = fs::read_to_string(path).unwrap();
    let document = roxmltree::Document::parse(&source).unwrap();
    let set = document.root_element();
    let set_name = set.attribute("name").unwrap_or_default();
    if !supports_xsd_10(set) {
        return;
    }
    let directory = path.parent().unwrap();

    for group in set.children().filter(|node| node.has_tag_name("testGroup")) {
        let group_name = group.attribute("name").unwrap_or_default();
        if !supports_xsd_10(group) {
            continue;
        }
        let mut schema: Option<Result<XsdSchema, String>> = None;
        let mut schema_expected_valid = true;

        for test in group.children().filter(|node| node.is_element()) {
            let kind = test.tag_name().name();
            if kind != "schemaTest" && kind != "instanceTest" {
                continue;
            }
            let id = format!(
                "{set_name}/{group_name}/{}",
                test.attribute("name").unwrap_or_default()
            );
            let documents = test
                .children()
                .filter(|node| {
                    node.has_tag_name("schemaDocument") || node.has_tag_name("instanceDocument")
                })
                .filter_map(|node| node.attribute((XLINK, "href")))
                .map(|href| directory.join(href))
                .collect::<Vec<_>>();
            let Some(expected_valid) = expected_validity(test) else {
                if kind == "schemaTest" {
                    schema_expected_valid = false;
                }
                run.record(id, Outcome::Skip("no XSD 1.0 expectation".to_owned()));
                continue;
            };
            if !is_accepted(test) {
                if kind == "schemaTest" {
                    schema_expected_valid = false;
                }
                run.record(id, Outcome::Skip("test not accepted".to_owned()));
                continue;
            }

            if kind == "schemaTest" {
                schema_expected_valid = expected_valid;
                let loaded = guarded_load(&documents);
                let outcome = match (&loaded, expected_valid) {
                    (Ok(_), true) | (Err(_), false) => Outcome::Pass,
                    (Err(error), true) => Outcome::Fail(format!("schema rejected: {error}")),
                    (Ok(_), false) => Outcome::Fail("invalid schema accepted".to_owned()),
                };
                run.record(id, outcome);
                schema = Some(loaded);
            } else {
                let outcome = match (&schema, schema_expected_valid) {
                    (_, false) => Outcome::Skip("schema expected invalid".to_owned()),
                    (None, _) => Outcome::Skip("no schema".to_owned()),
                    (Some(Err(_)), _) => {
                        Outcome::Fail("schema rejected: see schema test".to_owned())
                    }
                    (Some(Ok(schema)), true) => {
                        guarded(|| validate_instance(&documents[0], schema, expected_valid))
                    }
                };
                run.record(id, outcome);
            }
        }
    }
}

fn guarded_load(documents: &[PathBuf]) -> Result<XsdSchema, String> {
    let mut error = None;
    let mut schemas = Vec::new();
    let outcome = guarded(|| {
        for document in documents {
            match load_schema_set(document) {
                Ok(schema) => schemas.push(schema),
                Err(message) => {
                    error = Some(message);
                    break;
                }
            }
        }
        Outcome::Pass
    });
    if let Outcome::Fail(message) = outcome {
        return Err(message);
    }
    match error {
        Some(error) => Err(error),
        None => Ok(merge_schemas(schemas)),
    }
}

fn validate_instance(path: &Path, schema: &XsdSchema, expected_valid: bool) -> Outcome {
    let Some(source) = fs::read(path).ok().and_then(|bytes| decode(&bytes)) else {
        return Outcome::Skip("unreadable instance".to_owned());
    };
    let mut problems = well_formedness_errors(&source);
    problems.extend(
        validate_document_located(&source, schema)
            .into_iter()
            .map(|diagnostic| diagnostic.message),
    );
    match (expected_valid, problems.is_empty()) {
        (true, true) | (false, false) => Outcome::Pass,
        (true, false) => Outcome::Fail(format!("valid instance rejected: {}", problems.join("; "))),
        (false, true) => Outcome::Fail("invalid instance accepted".to_owned()),
    }
}

/// `version` tokens on test sets and groups restrict them to a spec version.
fn supports_xsd_10(node: roxmltree::Node<'_, '_>) -> bool {
    node.attribute("version").is_none_or(|version| {
        version
            .split_whitespace()
            .any(|token| token == "1.0" || token.starts_with("XSD-1.0") || !token.starts_with("1."))
            && !version.split_whitespace().any(|token| token == "1.1")
    })
}

fn expected_validity(test: roxmltree::Node<'_, '_>) -> Option<bool> {
    test.children()
        .filter(|node| node.has_tag_name("expected"))
        .find(|expected| {
            expected
                .attribute("version")
                .is_none_or(|version| version.split_whitespace().any(|token| token == "1.0"))
        })
        .and_then(|expected| match expected.attribute("validity") {
            Some("valid") => Some(true),
            Some("invalid") => Some(false),
            _ => None,
        })
}

fn is_accepted(test: roxmltree::Node<'_, '_>) -> bool {
    test.children()
        .find(|node| node.has_tag_name("current"))
        .is_none_or(|current| current.attribute("status") == Some("accepted"))
}
