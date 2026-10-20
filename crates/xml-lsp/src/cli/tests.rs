use super::*;

fn arguments(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn unique_directory(name: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!("xml-lsp-cli-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn parses_options_and_files() {
    let command = parse(&arguments(&[
        "--check",
        "--tab-size",
        "4",
        "--tabs",
        "a.xml",
    ]))
    .unwrap();
    assert_eq!(
        command,
        Command {
            check: true,
            tab_size: 4,
            tabs: true,
            files: vec!["a.xml".to_owned()],
        }
    );
    assert!(parse(&arguments(&[])).is_err());
    assert!(parse(&arguments(&["--tab-size", "40", "a.xml"])).is_err());
    assert!(parse(&arguments(&["--nope", "a.xml"])).is_err());
}

#[test]
fn keeps_the_byte_order_mark_and_the_line_ending() {
    let command = parse(&arguments(&["a.xml"])).unwrap();
    let formatted = format_source("\u{feff}<a><b/></a>", &command).unwrap();
    assert!(formatted.starts_with("\u{feff}<a>"), "{formatted:?}");
    assert!(formatted.contains("\n  <b/>"), "{formatted:?}");
    let crlf = format_source("<a>\r\n<b/></a>", &command).unwrap();
    assert!(crlf.contains("\r\n"), "{crlf:?}");
    assert!(format_source("<a></b>", &command).is_err());
}

#[test]
fn formats_files_in_place_and_checks_them() {
    let directory = unique_directory("files");
    let file = directory.join("one-line.xml");
    fs::write(&file, "<a><b>x</b></a>").unwrap();
    let path = file.to_str().unwrap();

    assert_eq!(run(&arguments(&["--check", path])), 1);
    assert_eq!(fs::read_to_string(&file).unwrap(), "<a><b>x</b></a>");

    assert_eq!(run(&arguments(&[path])), 0);
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "<a>\n  <b>x</b>\n</a>\n"
    );

    assert_eq!(run(&arguments(&["--check", path])), 0);

    let malformed = directory.join("malformed.xml");
    fs::write(&malformed, "<a></b>").unwrap();
    assert_eq!(run(&arguments(&[malformed.to_str().unwrap()])), 2);
    assert_eq!(
        run(&arguments(&[directory
            .join("missing.xml")
            .to_str()
            .unwrap()])),
        2
    );
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn formats_a_very_large_single_line_document() {
    let directory = unique_directory("large");
    let file = directory.join("large.xml");
    let record = "<item><name>Alpha</name><value>1</value></item>";
    fs::write(&file, format!("<root>{}</root>", record.repeat(40_000))).unwrap();
    assert_eq!(run(&arguments(&[file.to_str().unwrap()])), 0);
    assert!(fs::read_to_string(&file).unwrap().lines().count() > 100_000);
    let _ = fs::remove_dir_all(&directory);
}
