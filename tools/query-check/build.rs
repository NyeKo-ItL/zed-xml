use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(
        std::env::var("TS_XML_DIR").expect("TS_XML_DIR: checkout of tree-sitter-grammars/tree-sitter-xml"),
    );
    println!("cargo:rerun-if-env-changed=TS_XML_DIR");
    for language in ["xml", "dtd"] {
        let source = root.join(language).join("src");
        let mut build = cc::Build::new();
        build.include(&source).file(source.join("parser.c")).warnings(false);
        let scanner = source.join("scanner.c");
        if scanner.exists() {
            build.file(scanner);
        }
        build.compile(&format!("tree-sitter-{language}"));
    }
}
