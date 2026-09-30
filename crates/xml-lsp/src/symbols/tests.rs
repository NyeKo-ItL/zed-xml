use super::*;

const SCHEMA: &str = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:shop">
  <xs:element name="order" type="OrderType"/>
  <xs:attribute name="currency" type="xs:string"/>
  <xs:complexType name="OrderType">
    <xs:sequence><xs:element name="line" type="xs:string"/></xs:sequence>
    <xs:attribute name="local" type="xs:string"/>
  </xs:complexType>
  <xs:simpleType name="Status"><xs:restriction base="xs:string">
    <xs:enumeration value="open"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Code"><xs:restriction base="xs:token"/></xs:simpleType>
  <xs:group name="Lines"><xs:sequence/></xs:group>
  <xs:attributeGroup name="Common"/>
  <xs:redefine schemaLocation="base.xsd"><xs:complexType name="Base"/></xs:redefine>
  <xs:annotation><xs:documentation>name="ignored"</xs:documentation></xs:annotation>
</xs:schema>"#;

fn names(symbols: &[IndexedSymbol]) -> Vec<(&str, u32)> {
    symbols
        .iter()
        .map(|symbol| (symbol.name.as_str(), symbol.kind))
        .collect()
}

fn temp_directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("xml-lsp-symbols-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("directory should be created");
    directory
}

#[test]
fn indexes_global_xsd_components_with_kinds_and_namespace_container() {
    let symbols = index_document(SCHEMA, "file:///s/shop.xsd");
    assert_eq!(
        names(&symbols),
        vec![
            ("order", kind::FIELD),
            ("currency", kind::PROPERTY),
            ("OrderType", kind::CLASS),
            ("Status", kind::ENUM),
            ("Code", kind::CLASS),
            ("Lines", kind::STRUCT),
            ("Common", kind::MODULE),
            ("Base", kind::CLASS),
        ]
    );
    assert!(
        symbols
            .iter()
            .all(|symbol| symbol.container.as_deref() == Some("urn:shop"))
    );
    // The range covers the value of `name` (without quotes).
    assert_eq!(
        symbols[0].range,
        json!({"start": {"line": 2, "character": 20}, "end": {"line": 2, "character": 25}})
    );
}

#[test]
fn uses_the_file_name_without_target_namespace_and_other_prefixes() {
    let source = "<schema xmlns=\"http://www.w3.org/2001/XMLSchema\">\r\n  <element name=\"é\"/>\r\n  <foo:element xmlns:foo=\"urn:other\" name=\"x\"/>\r\n</schema>";
    let symbols = index_document(source, "file:///a/b/items.xsd");
    assert_eq!(names(&symbols), vec![("é", kind::FIELD)]);
    assert_eq!(symbols[0].container.as_deref(), Some("items.xsd"));
    assert_eq!(
        symbols[0].range["start"],
        json!({"line": 1, "character": 17})
    );
}

#[test]
fn indexes_the_root_and_identified_xml_elements_only() {
    let source = r#"<beans xmlns:p="urn:p">
  <bean id="dataSource" class="x"><property name="url" value="u"/></bean>
  <bean xml:id="a&amp;b"/>
  <p:item id="  "/>
  <plain/>
</beans>"#;
    let symbols = index_document(source, "file:///w/context.xml");
    assert_eq!(
        names(&symbols),
        vec![
            ("beans", kind::MODULE),
            ("dataSource", kind::KEY),
            ("url", kind::FIELD),
            ("a&b", kind::KEY),
        ]
    );
    assert_eq!(symbols[0].container.as_deref(), Some("context.xml"));
    assert_eq!(symbols[1].container.as_deref(), Some("bean"));
    assert_eq!(symbols[2].container.as_deref(), Some("property"));
}

#[test]
fn a_non_xsd_schema_root_is_an_xml_document() {
    let symbols = index_document("<schema name=\"s\"/>", "file:///w/schema.xml");
    assert_eq!(
        names(&symbols),
        vec![("schema", kind::MODULE), ("s", kind::FIELD)]
    );
    assert!(index_document("", "file:///w/empty.xml").is_empty());
    assert!(index_document("text only", "file:///w/empty.xml").is_empty());
}

#[test]
fn scores_exact_prefix_substring_and_fuzzy_matches() {
    assert_eq!(match_score("order", "Order"), Some((0, 0)));
    assert_eq!(match_score("ord", "OrderType"), Some((1, 0)));
    assert_eq!(match_score("item", "ns:itemList"), Some((1, 1)));
    assert_eq!(match_score("type", "OrderType"), Some((2, 5)));
    assert_eq!(match_score("otp", "OrderType"), Some((3, 5)));
    assert_eq!(match_score("éb", "ÉtatBase"), Some((3, 3)));
    assert_eq!(match_score("zz", "OrderType"), None);
    assert_eq!(match_score("", "anything"), Some((0, 0)));
}

#[test]
fn builds_hierarchical_document_symbols_with_details() {
    let source = "<root>\r\n  <bean id=\"x\"><p:prop name=\"n\"/></bean>\r\n  <open>\r\n    <leaf/>\r\n</root>";
    let symbols = document_symbols(source);
    assert_eq!(symbols.len(), 1);
    let root = &symbols[0];
    assert_eq!(root["name"], "root");
    assert!(root.get("detail").is_none());
    assert_eq!(
        root["selectionRange"],
        json!({"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 5}})
    );
    let children = root["children"].as_array().expect("children");
    assert_eq!(children[0]["name"], "bean");
    assert_eq!(children[0]["detail"], "id=\"x\"");
    assert_eq!(children[0]["children"][0]["name"], "p:prop");
    assert_eq!(children[0]["children"][0]["detail"], "name=\"n\"");
    // Unclosed element: its range covers its descendants.
    let open = &children[1];
    assert_eq!(open["name"], "open");
    assert_eq!(open["children"][0]["name"], "leaf");
    assert_eq!(open["range"]["end"], json!({"line": 3, "character": 11}));
    assert!(document_symbols("").is_empty());
}

#[test]
fn handles_deeply_nested_documents_without_recursion() {
    let source = "<a>".repeat(20_000) + &"</a>".repeat(20_000);
    let symbols = document_symbols(&source);
    assert_eq!(symbols.len(), 1);
    let mut depth = 0;
    let mut current = &symbols[0];
    while let Some(child) = current.get("children").and_then(|children| children.get(0)) {
        current = child;
        depth += 1;
    }
    assert_eq!(depth, MAX_SYMBOL_DEPTH - 1);
    assert_eq!(symbols[0]["range"]["end"]["character"], source.len());
}

#[test]
fn scans_workspace_files_skipping_ignored_directories_and_large_files() {
    let directory = temp_directory("scan");
    for path in [
        "a.xml",
        "b.xsd",
        "c.txt",
        "sub/d.svg",
        "target/e.xml",
        "node_modules/f.xml",
        ".git/g.xml",
        ".hidden/h.xml",
        ".i.xml",
    ] {
        let path = directory.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "<r/>").unwrap();
    }
    fs::write(
        directory.join("big.xml"),
        vec![b' '; MAX_FILE_SIZE as usize + 1],
    )
    .unwrap();
    let mut files = scan_workspace(std::slice::from_ref(&directory))
        .into_iter()
        .map(|(path, _, _)| {
            path.strip_prefix(&directory)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(files, vec!["a.xml", "b.xsd", "sub/d.svg"]);
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn the_indexed_suffixes_are_those_of_the_xml_language() {
    let config = include_str!("../../../../languages/xml/config.toml");
    let start = config
        .find("path_suffixes = [")
        .expect("path_suffixes in languages/xml/config.toml");
    let array = &config[start..];
    let array = &array[..array.find("\n]").expect("end of path_suffixes")];
    let suffixes = array
        .lines()
        .skip(1)
        .map(|line| line.split('#').next().unwrap_or_default())
        .flat_map(|line| line.split('"').skip(1).step_by(2))
        .collect::<Vec<_>>();
    assert_eq!(suffixes, XML_PATH_SUFFIXES);
    let mut unique = suffixes.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), suffixes.len(), "duplicate path suffix");
}

#[test]
fn recognizes_the_file_names_of_the_xml_language() {
    for name in [
        "a.xml",
        "Foo.XML",
        "pom.xml",
        "project.pom",
        "MainWindow.xaml",
        "App.axaml",
        "Strings.resx",
        "Main.storyboard",
        "View.xib",
        "Sample.fxml",
        "schema.rng",
        "messages.xlf",
        "messages.xliff",
        "places.kml",
        "track.gpx",
        "Library.fsproj",
        "App.vcxproj.filters",
        "App.csproj.user",
        "Package.nuspec",
        "Product.wxs",
        "Dark.tmTheme",
        "dark.tmtheme",
        "page.xhtml",
        "\u{e9}t\u{e9}.svg",
    ] {
        assert!(is_xml_file_name(name), "{name}");
    }
    for name in [
        "xml",
        "a.csproj",
        "Directory.Build.props",
        "Build.targets",
        "App.slnx",
        "index.html",
        "a.filters",
        "a.user",
        "axml",
        "\u{e9}xml",
        "a.xml.bak",
        "",
    ] {
        assert!(!is_xml_file_name(name), "{name}");
    }
}

#[test]
fn watches_every_file_of_the_xml_language() {
    let glob = watched_files_glob();
    assert!(glob.starts_with("**/*.{xml,xsd,"), "{glob}");
    for path in [
        "a.xml",
        "src/MainWindow.xaml",
        "vc/App.vcxproj.filters",
        "maps/track.gpx",
    ] {
        assert!(crate::settings::glob_match(&glob, path), "{path}");
    }
    assert!(!crate::settings::glob_match(&glob, "a/App.csproj"));
}

#[test]
fn queries_open_documents_over_disk_and_refreshes_changed_files() {
    let directory = temp_directory("query");
    let schema = directory.join("shop.xsd");
    fs::write(&schema, SCHEMA).unwrap();
    fs::write(
        directory.join("beans.xml"),
        "<beans><bean id=\"orderService\"/></beans>",
    )
    .unwrap();
    let mut index = WorkspaceIndex::with_roots(vec![directory.clone()]);
    let mut documents = HashMap::new();

    let names = |results: &[Value]| {
        results
            .iter()
            .map(|result| result["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let results = index.query(&documents, "ORDER");
    assert_eq!(names(&results), vec!["order", "OrderType", "orderService"]);
    assert_eq!(results[0]["location"]["uri"], path_to_uri(&schema));
    assert_eq!(results[0]["containerName"], "urn:shop");

    // The open (unsaved) buffer replaces the file on disk.
    documents.insert(
        path_to_uri(&directory.join("beans.xml")),
        "<beans><bean id=\"orderRepository\"/></beans>".to_owned(),
    );
    assert_eq!(
        names(&index.query(&documents, "orderr")),
        vec!["orderRepository"]
    );
    documents.clear();

    // Change on disk: the cache is invalidated by size or modification
    // time; a deleted file disappears.
    fs::write(
        directory.join("beans.xml"),
        "<beans><bean id=\"orderServiceImpl\"/></beans>",
    )
    .unwrap();
    fs::remove_file(&schema).unwrap();
    assert_eq!(
        names(&index.query(&documents, "order")),
        vec!["orderServiceImpl"]
    );

    // Empty query: bounded list.
    let many = (0..300)
        .map(|index| format!("<item id=\"i{index}\"/>"))
        .collect::<String>();
    documents.insert("untitled:1".to_owned(), format!("<r>{many}</r>"));
    assert_eq!(index.query(&documents, "").len(), MAX_EMPTY_QUERY_RESULTS);
    assert_eq!(index.query(&documents, "i").len(), MAX_RESULTS);
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn reads_workspace_folders_from_initialize_params_and_changes() {
    let index = WorkspaceIndex::from_initialize_params(&json!({
        "rootUri": "file:///root",
        "workspaceFolders": [{"uri": "file:///a%20b", "name": "a"}, {"uri": "file:///c", "name": "c"}],
    }));
    assert_eq!(index.roots(), &[PathBuf::from("/a b"), PathBuf::from("/c")]);
    let mut index = WorkspaceIndex::from_initialize_params(&json!({"rootUri": "file:///root"}));
    assert_eq!(index.roots(), &[PathBuf::from("/root")]);
    index.change_folders(&json!({"event": {
        "added": [{"uri": "file:///new"}],
        "removed": [{"uri": "file:///root"}],
    }}));
    assert_eq!(index.roots(), &[PathBuf::from("/new")]);
    let index = WorkspaceIndex::from_initialize_params(&json!({"rootPath": "/legacy"}));
    assert_eq!(index.roots(), &[PathBuf::from("/legacy")]);
    assert!(
        WorkspaceIndex::from_initialize_params(&json!({"rootUri": null}))
            .roots()
            .is_empty()
    );
}
