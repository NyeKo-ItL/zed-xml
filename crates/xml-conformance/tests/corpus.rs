//! Well-formedness corpora vendored from other XML parsers, with the verdict
//! of each file in `expectations.txt`:
//!
//! - `tests/fixtures/corpus/roxmltree`: the roxmltree test files (MIT or
//!   Apache-2.0), focused on attributes, entities, namespaces and text;
//! - `tests/fixtures/corpus/libxml2-errors`: libxml2 `test/errors` (MIT),
//!   documents that once crashed or misled libxml2.
//!
//! Known disagreements are listed in `baselines/corpus-<name>.txt`.

use std::fs;

use xml_conformance::{
    Outcome, SuiteRun, check_formatting, decode, fixtures_dir, guarded, well_formedness_errors,
};

#[test]
fn roxmltree_corpus() {
    run_corpus("roxmltree");
}

#[test]
fn libxml2_error_corpus() {
    run_corpus("libxml2-errors");
}

fn run_corpus(name: &str) {
    let directory = fixtures_dir().join("corpus").join(name);
    let expectations = fs::read_to_string(directory.join("expectations.txt"))
        .expect("every corpus has an expectations.txt");
    let mut run = SuiteRun::new(&format!("corpus-{name}"));
    let mut listed = 0;
    for line in expectations.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(file), Some(verdict)) = (fields.next(), fields.next()) else {
            panic!("malformed expectation line: {line}");
        };
        listed += 1;
        let path = directory.join(file);
        let bytes = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let outcome = guarded(|| match (verdict, decode(&bytes)) {
            ("not-wf", None) => Outcome::Pass,
            (_, None) => Outcome::Fail("undecodable document".to_owned()),
            ("wf", Some(source)) => {
                let errors = well_formedness_errors(&source);
                if !errors.is_empty() {
                    return Outcome::Fail(format!(
                        "reported not well-formed: {}",
                        errors.join("; ")
                    ));
                }
                match check_formatting(&source) {
                    Ok(()) => Outcome::Pass,
                    Err(error) => Outcome::Fail(format!("formatting: {error}")),
                }
            }
            ("not-wf", Some(source)) => {
                if well_formedness_errors(&source).is_empty() {
                    Outcome::Fail("accepted a not well-formed document".to_owned())
                } else {
                    Outcome::Pass
                }
            }
            (other, _) => panic!("unknown verdict {other} for {file}"),
        });
        run.record(file, outcome);
    }

    let files = fs::read_dir(&directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "xml")
        })
        .count();
    assert_eq!(
        files, listed,
        "every .xml file of {name} needs an expectation"
    );
    run.check_against_baseline();
}
