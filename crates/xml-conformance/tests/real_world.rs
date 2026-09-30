//! Real-world documents vendored in `tests/fixtures/real-world` (provenance
//! and licences in `tests/fixtures/SOURCES.md`).
//!
//! Every document must be decoded, reported well-formed, and formatted
//! idempotently without changing its content. Documents paired with a schema
//! in [`SCHEMA_CASES`] must also validate, and a copy with an element the
//! schema does not allow must not. Known failures are listed in
//! `baselines/real-world.txt`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use xml_conformance::{
    Outcome, SuiteRun, check_formatting, decode, fixtures_dir, guarded, load_schema_set,
    validate_with_schema, well_formedness_errors,
};

/// `(instance, schema, element into which an unexpected child is inserted)`.
const SCHEMA_CASES: &[(&str, &str, &str)] = &[
    (
        "maven/commons-lang-pom.xml",
        "maven/maven-4.0.0.xsd",
        "<dependencies>",
    ),
    (
        "maven/spring-petclinic-pom.xml",
        "maven/maven-4.0.0.xsd",
        "<dependencies>",
    ),
    (
        "specs/purchase-order.xml",
        "specs/purchase-order.xsd",
        "<items>",
    ),
];

#[test]
fn real_world_documents() {
    let root = fixtures_dir().join("real-world");
    let mut files = Vec::new();
    collect_documents(&root, &mut files);
    files.sort();
    assert!(
        files.len() > 40,
        "the real-world fixtures should be present"
    );

    let mut run = SuiteRun::new("real-world");
    for path in &files {
        let id = relative(&root, path);
        if path.extension().is_some_and(|extension| extension == "xsd") {
            let outcome = guarded(|| match load_schema_set(path) {
                Ok(_) => Outcome::Pass,
                Err(error) => Outcome::Fail(format!("schema rejected: {error}")),
            });
            run.record(format!("{id} [schema]"), outcome);
        }
        let outcome = guarded(|| check_document(path));
        run.record(id, outcome);
    }

    for (instance, schema, parent) in SCHEMA_CASES {
        let instance_path = root.join(instance);
        let schema_path = root.join(schema);
        let source = decode(&fs::read(&instance_path).unwrap()).unwrap();

        let outcome = guarded(|| match validate_with_schema(&source, &schema_path) {
            Ok(diagnostics) if diagnostics.is_empty() => Outcome::Pass,
            Ok(diagnostics) => Outcome::Fail(format!(
                "valid instance rejected: {}",
                diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
            Err(error) => Outcome::Fail(format!("schema rejected: {error}")),
        });
        run.record(format!("{instance} [valid against {schema}]"), outcome);

        let broken = source.replacen(parent, &format!("{parent}<unexpectedElement/>"), 1);
        assert_ne!(broken, source, "{instance} should contain {parent}");
        let outcome = guarded(|| match validate_with_schema(&broken, &schema_path) {
            Ok(diagnostics) if diagnostics.is_empty() => {
                Outcome::Fail("unexpected element accepted".to_owned())
            }
            Ok(_) => Outcome::Pass,
            Err(error) => Outcome::Fail(format!("schema rejected: {error}")),
        });
        run.record(
            format!("{instance} [invalid copy against {schema}]"),
            outcome,
        );
    }

    run.check_against_baseline();
}

fn check_document(path: &Path) -> Outcome {
    let bytes = fs::read(path).unwrap();
    let Some(source) = decode(&bytes) else {
        return Outcome::Fail("undecodable document".to_owned());
    };
    let errors = well_formedness_errors(&source);
    if !errors.is_empty() {
        return Outcome::Fail(format!("reported not well-formed: {}", errors.join("; ")));
    }
    match check_formatting(&source) {
        Ok(()) => Outcome::Pass,
        Err(error) => Outcome::Fail(format!("formatting: {error}")),
    }
}

fn collect_documents(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap().filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_documents(&path, files);
        } else if !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".md"))
        {
            files.push(path);
        }
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}
