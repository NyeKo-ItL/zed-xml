use super::*;

#[test]
fn caches_files_and_merged_sets_until_they_change_on_disk() {
    let directory =
        std::env::temp_dir().join(format!("xml-lsp-schema-store-{}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    let main = directory.join("main.xsd");
    let part = directory.join("part.xsd");
    fs::write(
            &main,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:include schemaLocation="part.xsd"/><xs:element name="a"/></xs:schema>"#,
        )
        .unwrap();
    fs::write(
            &part,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="b"/></xs:schema>"#,
        )
        .unwrap();
    let catalogs = catalog::Catalogs::default();
    let reference = || {
        vec![SchemaReference {
            namespace: None,
            path: main.clone(),
        }]
    };
    let mut store = SchemaStore::default();
    let first = store.load(reference(), &catalogs);
    assert!(first.errors.is_empty());
    let merged = first.merged.expect("merged schema");
    let names = |schema: &XsdSchema| {
        let mut names = schema
            .elements
            .iter()
            .map(|element| element.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    assert_eq!(names(&merged), ["a", "b"]);
    // Unchanged files: the same merged schema is reused.
    let second = store.load(reference(), &catalogs).merged.unwrap();
    assert!(Arc::ptr_eq(&merged, &second));
    // A modified file is read again (its length differs: the modification time
    // alone is too coarse on some file systems).
    fs::write(
            &part,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="ccc"/></xs:schema>"#,
        )
        .unwrap();
    let third = store.load(reference(), &catalogs).merged.unwrap();
    assert_eq!(names(&third), ["a", "ccc"]);
    // Parse errors are reported each time, from the cache.
    fs::write(&part, "<xs:schema").unwrap();
    for _ in 0..2 {
        let broken = store.load(reference(), &catalogs);
        assert_eq!(broken.errors.len(), 1);
        assert!(broken.errors[0].message.starts_with("invalid XSD schema"));
        assert_eq!(names(&broken.merged.unwrap()), ["a"]);
    }
    fs::remove_file(&part).unwrap();
    let missing = store.load(reference(), &catalogs);
    assert!(
        missing.errors[0]
            .message
            .starts_with("cannot read the schema")
    );
    let _ = fs::remove_dir_all(&directory);
}
