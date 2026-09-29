//! libxml2 XML Schema regression tests (`test/schemas`, MIT licence), fetched
//! by `scripts/fetch-test-suites.sh`; skipped when absent.
//!
//! Each `result/schemas/<name>_<schema>_<instance>.err` records whether
//! `test/schemas/<name>_<instance>.xml` validates against
//! `test/schemas/<name>_<schema>.xsd` in libxml2 (`validates` or `fails to
//! validate`). Pairs whose schema does not compile in libxml2 are skipped.

use std::fs;

use xml_conformance::{
    Outcome, SuiteRun, decode, external_suite, guarded, validate_with_schema,
    well_formedness_errors,
};

#[test]
fn libxml2_schema_regression_tests() {
    let Some(suite) = external_suite("libxml2", "result/schemas") else {
        return;
    };
    let mut run = SuiteRun::new("libxml2-schemas");
    let mut results = fs::read_dir(suite.join("result/schemas"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "err"))
        .collect::<Vec<_>>();
    results.sort();

    for result in results {
        let stem = result.file_stem().unwrap().to_string_lossy().into_owned();
        let mut parts = stem.rsplitn(3, '_');
        let (Some(instance), Some(schema), Some(name)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let schema_path = suite.join(format!("test/schemas/{name}_{schema}.xsd"));
        let instance_path = suite.join(format!("test/schemas/{name}_{instance}.xml"));
        let report = fs::read_to_string(&result).unwrap_or_default();
        let expected_valid = if report.contains("fails to validate") {
            false
        } else if report.contains(" validates") {
            true
        } else {
            run.record(stem, Outcome::Skip("no verdict in libxml2".to_owned()));
            continue;
        };

        let outcome = guarded(|| {
            let Some(source) = fs::read(&instance_path)
                .ok()
                .and_then(|bytes| decode(&bytes))
            else {
                return Outcome::Skip("unreadable instance".to_owned());
            };
            let diagnostics = match validate_with_schema(&source, &schema_path) {
                Ok(diagnostics) => diagnostics,
                Err(error) => return Outcome::Fail(format!("schema rejected: {error}")),
            };
            let valid = diagnostics.is_empty() && well_formedness_errors(&source).is_empty();
            match (expected_valid, valid) {
                (true, true) | (false, false) => Outcome::Pass,
                (true, false) => Outcome::Fail(format!(
                    "valid instance rejected: {}",
                    diagnostics
                        .iter()
                        .map(|diagnostic| diagnostic.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                )),
                (false, true) => Outcome::Fail("invalid instance accepted".to_owned()),
            }
        });
        run.record(stem, outcome);
    }
    run.check_against_baseline();
}
