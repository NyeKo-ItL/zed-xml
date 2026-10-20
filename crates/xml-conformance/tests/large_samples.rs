//! Large documents of `tests/large-samples` written on one or two lines:
//! they must be reported well-formed and formatted idempotently without
//! changing their content.

use std::fs;

use xml_conformance::{check_formatting, decode, repository_root, well_formedness_errors};

#[test]
fn large_samples_are_formatted() {
    let directory = repository_root().join("tests").join("large-samples");
    let mut checked = 0;
    for entry in fs::read_dir(&directory).expect("the large samples should be present") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_none_or(|extension| extension != "xml") {
            continue;
        }
        let bytes = fs::read(&path).expect("the sample should be readable");
        let source = decode(&bytes).expect("the sample should be decoded");
        let errors = well_formedness_errors(&source);
        assert!(errors.is_empty(), "{path:?} is not well-formed: {errors:?}");
        if let Err(error) = check_formatting(&source) {
            panic!("{path:?}: {error}");
        }
        checked += 1;
    }
    assert_eq!(checked, 2, "both large samples should be checked");
}
