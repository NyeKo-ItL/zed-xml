use super::*;

const HEADER: &str = r#"<catalog xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog""#;

fn catalog(body: &str) -> String {
    format!("{HEADER}>\n{body}\n</catalog>")
}

/// Test-specific temporary directory.
fn directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("xml-lsp-catalog {name} {}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    directory
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn parses_every_entry_type() {
    let source = catalog(
        r#"<system systemId="http://x/s.dtd" uri="s.dtd"/>
<public publicId="  -//X//DTD  Y//EN " uri="p.dtd"/>
<uri name="urn:ns" uri="n.xsd"/>
<rewriteSystem systemIdStartString="http://x/" rewritePrefix="rw/"/>
<rewriteURI uriStartString="http://u/" rewritePrefix="file:///abs/"/>
<systemSuffix systemIdSuffix="/s.dtd" uri="suffix.dtd"/>
<uriSuffix uriSuffix="/n.xsd" uri="suffix.xsd"/>
<delegatePublic publicIdStartString="-//X" catalog="d1.xml"/>
<delegateSystem systemIdStartString="http://d/" catalog="d2.xml"/>
<delegateURI uriStartString="http://e/" catalog="d3.xml"/>
<nextCatalog catalog="next.xml"/>
<other:ignored xmlns:other="urn:other" uri="x"><uri name="urn:hidden" uri="h"/></other:ignored>
<system systemId="missing-uri"/>"#,
    );
    let parsed = parse_catalog(&source, "file:///cat/catalog.xml").unwrap();
    let summary = parsed
        .entries
        .iter()
        .map(|entry| (entry.kind, entry.key.as_str(), entry.target.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        vec![
            (EntryKind::System, "http://x/s.dtd", "file:///cat/s.dtd"),
            (EntryKind::Public, "-//X//DTD Y//EN", "file:///cat/p.dtd"),
            (EntryKind::Uri, "urn:ns", "file:///cat/n.xsd"),
            (EntryKind::RewriteSystem, "http://x/", "file:///cat/rw/"),
            (EntryKind::RewriteUri, "http://u/", "file:///abs/"),
            (EntryKind::SystemSuffix, "/s.dtd", "file:///cat/suffix.dtd"),
            (EntryKind::UriSuffix, "/n.xsd", "file:///cat/suffix.xsd"),
            (EntryKind::DelegatePublic, "-//X", "file:///cat/d1.xml"),
            (EntryKind::DelegateSystem, "http://d/", "file:///cat/d2.xml"),
            (EntryKind::DelegateUri, "http://e/", "file:///cat/d3.xml"),
            (EntryKind::NextCatalog, "", "file:///cat/next.xml"),
        ]
    );
    let entry = &parsed.entries[0];
    assert_eq!(&source[entry.target_range.clone()], "s.dtd");
    assert!(parsed.entries.iter().all(|entry| entry.prefer_public));
}

#[test]
fn rejects_documents_that_are_not_catalogs() {
    assert!(parse_catalog("<catalog/>", "file:///c.xml").is_err());
    assert!(
        parse_catalog(
            "<a xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\"/>",
            "file:///c.xml"
        )
        .is_err()
    );
    assert!(!is_catalog("<root/>"));
    assert!(is_catalog(&catalog("")));
    // Explicit prefix.
    assert!(is_catalog(
        "<c:catalog xmlns:c=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\"><c:uri name=\"a\" uri=\"b\"/></c:catalog>"
    ));
}

#[test]
fn applies_groups_xml_base_and_prefer() {
    let source = format!(
        r#"{HEADER} prefer="system" xml:base="schemas/">
<public publicId="-//A" uri="a.dtd"/>
<group prefer="public" xml:base="http://mirror/base/">
  <public publicId="-//B" uri="b.dtd"/>
  <system systemId="s" uri="../up/s.dtd" xml:base="nested/"/>
</group>
<group xml:base="/abs/dir/">
  <uri name="u" uri="./u.xsd"/>
</group>
<uri name="after" uri="x.xsd"/>
</catalog>"#
    );
    let parsed = parse_catalog(&source, "file:///root/catalog.xml").unwrap();
    let summary = parsed
        .entries
        .iter()
        .map(|entry| {
            (
                entry.key.as_str(),
                entry.target.as_str(),
                entry.prefer_public,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        vec![
            ("-//A", "file:///root/schemas/a.dtd", false),
            ("-//B", "http://mirror/base/b.dtd", true),
            ("s", "http://mirror/base/up/s.dtd", true),
            ("u", "file:///abs/dir/u.xsd", false),
            ("after", "file:///root/schemas/x.xsd", false),
        ]
    );
}

#[test]
fn resolves_uri_references() {
    let base = "file:///a/b/c.xml";
    assert_eq!(resolve_uri_reference(base, "d.xsd"), "file:///a/b/d.xsd");
    assert_eq!(resolve_uri_reference(base, "../d.xsd"), "file:///a/d.xsd");
    assert_eq!(
        resolve_uri_reference(base, "../../../d.xsd"),
        "file:///d.xsd"
    );
    assert_eq!(resolve_uri_reference(base, "./x/./y/"), "file:///a/b/x/y/");
    assert_eq!(resolve_uri_reference(base, "/abs.xsd"), "file:///abs.xsd");
    assert_eq!(
        resolve_uri_reference(base, "sub\\win.xsd"),
        "file:///a/b/sub/win.xsd"
    );
    assert_eq!(resolve_uri_reference(base, "http://h/x"), "http://h/x");
    assert_eq!(
        resolve_uri_reference("http://h/p/q.xml", "../r"),
        "http://h/r"
    );
    assert_eq!(
        resolve_uri_reference("http://h/p/q.xml", "//o/r"),
        "http://o/r"
    );
    assert_eq!(
        resolve_uri_reference(base, "C:\\s\\d.xsd"),
        "file:///C:/s/d.xsd"
    );
}

#[test]
fn normalizes_identifiers() {
    assert_eq!(normalize_public_id("  -//A \n B//EN "), "-//A B//EN");
    assert_eq!(
        normalize_uri(" http://x/a b\u{e9}.xsd "),
        "http://x/a%20b%C3%A9.xsd"
    );
    assert_eq!(
        unwrap_public_id_urn("urn:publicid:-:OASIS:DTD+DocBook+XML+V4.1.2:EN").as_deref(),
        Some("-//OASIS//DTD DocBook XML V4.1.2//EN")
    );
    assert_eq!(
        unwrap_public_id_urn("urn:publicid:a;b%3Ac%2B%zz").as_deref(),
        Some("a::b:c+%zz")
    );
    assert_eq!(unwrap_public_id_urn("http://x"), None);
}

#[test]
fn resolves_entries_in_specification_order() {
    let root = directory("order");
    let main = root.join("catalog.xml");
    write(
        &main,
        &catalog(
            r#"<system systemId="http://x/a/b/exact.dtd" uri="exact.dtd"/>
<rewriteSystem systemIdStartString="http://x/" rewritePrefix="short/"/>
<rewriteSystem systemIdStartString="http://x/a/" rewritePrefix="long/"/>
<rewriteSystem systemIdStartString="http://x/a/" rewritePrefix="ignored-tie/"/>
<systemSuffix systemIdSuffix=".dtd" uri="short-suffix.dtd"/>
<systemSuffix systemIdSuffix="/tail.dtd" uri="long-suffix.dtd"/>
<uri name="urn:ns" uri="ns.xsd"/>
<rewriteURI uriStartString="http://u/" rewritePrefix="u/"/>
<uriSuffix uriSuffix="/end.xsd" uri="end.xsd"/>
<public publicId="-//P//EN" uri="public.dtd"/>
<group prefer="system"><public publicId="-//S//EN" uri="system-pref.dtd"/></group>"#,
        ),
    );
    let catalogs = Catalogs::new(vec![main]);
    let base = path_to_uri(&root);
    let at = |relative: &str| format!("{base}/{relative}");
    assert_eq!(
        catalogs.resolve_system("http://x/a/b/exact.dtd"),
        Some(at("exact.dtd"))
    );
    // Rewrite: the longest prefix wins, then the first one.
    assert_eq!(
        catalogs.resolve_system("http://x/a/b/c.dtd"),
        Some(at("long/b/c.dtd"))
    );
    assert_eq!(
        catalogs.resolve_system("http://x/z.dtd"),
        Some(at("short/z.dtd"))
    );
    // Suffixes (after rewrites): the longest wins.
    assert_eq!(
        catalogs.resolve_system("http://y/tail.dtd"),
        Some(at("long-suffix.dtd"))
    );
    assert_eq!(
        catalogs.resolve_system("http://y/other.dtd"),
        Some(at("short-suffix.dtd"))
    );
    assert_eq!(catalogs.resolve_system("http://y/other.xsd"), None);
    // URI.
    assert_eq!(catalogs.resolve_uri("urn:ns"), Some(at("ns.xsd")));
    assert_eq!(
        catalogs.resolve_uri("http://u/p/q.xsd"),
        Some(at("u/p/q.xsd"))
    );
    assert_eq!(
        catalogs.resolve_uri("http://z/end.xsd"),
        Some(at("end.xsd"))
    );
    assert_eq!(catalogs.resolve_uri("http://x/a/b/exact.dtd"), None);
    // Public identifiers and `prefer`.
    assert_eq!(
        catalogs.resolve_external(Some("-//P//EN"), None),
        Some(at("public.dtd"))
    );
    assert_eq!(
        catalogs.resolve_external(Some("-//P//EN"), Some("unknown.ent")),
        Some(at("public.dtd"))
    );
    assert_eq!(
        catalogs.resolve_external(Some("-//S//EN"), None),
        Some(at("system-pref.dtd"))
    );
    assert_eq!(
        catalogs.resolve_external(Some("-//S//EN"), Some("unknown.ent")),
        None
    );
    // System wins over public.
    assert_eq!(
        catalogs.resolve_external(Some("-//P//EN"), Some("http://x/a/b/exact.dtd")),
        Some(at("exact.dtd"))
    );
    // publicid URN.
    assert_eq!(
        catalogs.resolve_system("urn:publicid:-:P:EN"),
        Some(at("public.dtd"))
    );
    assert_eq!(
        catalogs.resolve_uri("urn:publicid:-:P:EN"),
        Some(at("public.dtd"))
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn follows_next_catalogs_and_delegations_without_cycles() {
    let root = directory("next");
    let first = root.join("first.xml");
    write(
        &first,
        &catalog(
            r#"<nextCatalog catalog="sub/second.xml"/>
<nextCatalog catalog="missing.xml"/>
<uri name="urn:first" uri="first.xsd"/>
<delegateURI uriStartString="http://d/" catalog="delegate-short.xml"/>
<delegateURI uriStartString="http://d/long/" catalog="delegate-long.xml"/>
<delegateURI uriStartString="http://cycle/" catalog="cycle.xml"/>"#,
        ),
    );
    write(
        &root.join("cycle.xml"),
        &catalog(r#"<delegateURI uriStartString="http://cycle/" catalog="first.xml"/>"#),
    );
    write(
        &root.join("sub/second.xml"),
        &catalog(
            r#"<nextCatalog catalog="../first.xml"/>
<uri name="urn:second" uri="second.xsd"/>
<uri name="urn:first" uri="shadowed.xsd"/>"#,
        ),
    );
    write(
        &root.join("delegate-long.xml"),
        &catalog(r#"<uri name="http://d/long/x" uri="from-long.xsd"/>"#),
    );
    write(
        &root.join("delegate-short.xml"),
        &catalog(
            r#"<uri name="http://d/long/x" uri="from-short.xsd"/>
<uri name="http://d/long/y" uri="from-short-y.xsd"/>"#,
        ),
    );
    let catalogs = Catalogs::new(vec![first.clone()]);
    let base = path_to_uri(&root);
    assert_eq!(
        catalogs.resolve_uri("urn:first"),
        Some(format!("{base}/first.xsd"))
    );
    assert_eq!(
        catalogs.resolve_uri("urn:second"),
        Some(format!("{base}/sub/second.xsd"))
    );
    // Cycle first -> second -> first: terminates.
    assert_eq!(catalogs.resolve_uri("urn:none"), None);
    // Delegation: new list, longest prefix first.
    assert_eq!(
        catalogs.resolve_uri("http://d/long/x"),
        Some(format!("{base}/from-long.xsd"))
    );
    assert_eq!(
        catalogs.resolve_uri("http://d/long/y"),
        Some(format!("{base}/from-short-y.xsd"))
    );
    // Circular delegations: bounded depth.
    assert_eq!(catalogs.resolve_uri("http://cycle/x"), None);
    assert!(catalogs.contains(&root.join("sub/second.xml")));
    assert!(catalogs.contains(&root.join("missing.xml")));
    assert_eq!(catalogs.errors().len(), 1);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn refreshes_modified_catalogs() {
    let root = directory("refresh");
    let path = root.join("catalog.xml");
    write(&path, &catalog(r#"<uri name="urn:a" uri="a.xsd"/>"#));
    let mut catalogs = Catalogs::new(vec![path.clone()]);
    assert!(!catalogs.refresh());
    assert!(catalogs.resolve_uri("urn:a").is_some());
    write(&path, &catalog(r#"<uri name="urn:b" uri="b-longer.xsd"/>"#));
    assert!(catalogs.refresh());
    assert!(catalogs.resolve_uri("urn:a").is_none());
    assert!(catalogs.resolve_uri("urn:b").is_some());
    assert!(!catalogs.set_roots(vec![path.clone()]));
    assert!(catalogs.set_roots(Vec::new()));
    assert!(catalogs.files().is_empty());
    assert!(catalogs.resolve_uri("urn:b").is_none());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolves_schema_locations_by_namespace_then_location() {
    let root = directory("schemas");
    let path = root.join("catalog.xml");
    write(
        &path,
        &catalog(
            r#"<uri name="urn:ns" uri="by-namespace.xsd"/>
<system systemId="http://x/by-system.xsd" uri="by-system.xsd"/>
<uri name="http://x/by-uri.xsd" uri="by-uri.xsd"/>
<uri name="http://remote/only" uri="http://mirror/only.xsd"/>"#,
        ),
    );
    let catalogs = Catalogs::new(vec![path]);
    let request = |kind, namespace, location| SchemaLocation {
        kind,
        namespace,
        location,
        base_directory: Path::new("/docs"),
    };
    use SchemaLocationKind as Kind;
    assert_eq!(
        catalogs.resolve_schema(&request(
            Kind::SchemaLocation,
            Some("urn:ns"),
            Some("http://x/by-system.xsd")
        )),
        Some(root.join("by-namespace.xsd"))
    );
    assert_eq!(
        catalogs.resolve_schema(&request(Kind::Import, Some("urn:ns"), None)),
        Some(root.join("by-namespace.xsd"))
    );
    assert_eq!(
        catalogs.resolve_schema(&request(
            Kind::Include,
            Some("urn:ns"),
            Some("http://x/by-uri.xsd")
        )),
        Some(root.join("by-uri.xsd"))
    );
    assert_eq!(
        catalogs.resolve_schema(&request(
            Kind::NoNamespaceSchemaLocation,
            None,
            Some("http://x/by-system.xsd")
        )),
        Some(root.join("by-system.xsd"))
    );
    // Remote target: not resolved locally.
    assert_eq!(catalogs.resolve_location("http://remote/only"), None);
    assert_eq!(
        Catalogs::default().resolve_schema(&request(Kind::Import, Some("urn:ns"), None)),
        None
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolves_configured_catalog_paths() {
    let root = directory("paths");
    let other = directory("paths-other");
    write(&other.join("cat/extra.xml"), "<x/>");
    write(&root.join(AUTO_DETECTED_CATALOG), &catalog(""));
    write(&other.join(AUTO_DETECTED_CATALOG), "<notACatalog/>");
    let roots = vec![root.clone(), other.clone()];
    // Absolute on every platform (`/abs` has no drive letter on Windows).
    let absolute = std::env::temp_dir().join("abs").join("c.xml");
    let configured = vec![
        "cat/extra.xml".to_owned(),
        "missing/./c.xml".to_owned(),
        path_to_uri(&root.join("u r i.xml")),
        absolute.to_string_lossy().into_owned(),
        " ".to_owned(),
    ];
    assert_eq!(
        catalog_paths(&configured, &roots, false),
        vec![
            other.join("cat/extra.xml"),
            root.join("missing/c.xml"),
            root.join("u r i.xml"),
            absolute,
        ]
    );
    let detected = catalog_paths(&[], &roots, true);
    assert_eq!(detected, vec![root.join(AUTO_DETECTED_CATALOG)]);
    assert!(catalog_paths(&["relative.xml".to_owned()], &[], false).is_empty());
    if let Some(home) = home_directory() {
        assert_eq!(
            catalog_paths(&["~/c.xml".to_owned()], &[], false),
            vec![home.join("c.xml")]
        );
    }
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&other);
}

#[test]
fn reports_missing_targets_of_an_open_catalog() {
    let root = directory("problems");
    write(&root.join("present.xsd"), "<x/>");
    fs::create_dir_all(root.join("dir")).unwrap();
    let source = catalog(
        r#"<uri name="a" uri="present.xsd"/>
<uri name="b" uri="absent.xsd"/>
<uri name="c" uri="http://remote/x.xsd"/>
<rewriteURI uriStartString="http://r/" rewritePrefix="dir/"/>
<rewriteURI uriStartString="http://s/" rewritePrefix="nodir/"/>
<nextCatalog catalog="nocatalog.xml"/>"#,
    );
    let uri = path_to_uri(&root.join("catalog.xml"));
    let problems = catalog_problems(&uri, &source);
    let texts = problems
        .iter()
        .map(|problem| &source[problem.range.clone()])
        .collect::<Vec<_>>();
    assert_eq!(texts, vec!["absent.xsd", "nodir/", "nocatalog.xml"]);
    assert!(problems[0].message.starts_with("File not found"));
    assert!(problems[1].message.starts_with("Directory not found"));
    assert!(problems[2].message.starts_with("Catalog not found"));
    assert!(catalog_problems(&uri, "<root/>").is_empty());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn bounds_catalog_chains_and_never_reads_network_or_devices() {
    let root = directory("limits");
    // A `nextCatalog` cycle, then a chain longer than the bound.
    write(
        &root.join("a.xml"),
        &catalog(r#"<nextCatalog catalog="b.xml"/><uri name="urn:a" uri="a.xsd"/>"#),
    );
    write(
        &root.join("b.xml"),
        &catalog(r#"<nextCatalog catalog="a.xml"/><uri name="urn:b" uri="b.xsd"/>"#),
    );
    let catalogs = Catalogs::new(vec![root.join("a.xml")]);
    assert_eq!(catalogs.files().len(), 2);
    assert!(catalogs.resolve_uri("urn:b").is_some());
    assert_eq!(catalogs.resolve_uri("urn:missing"), None);

    for index in 0..MAX_CATALOG_FILES + 10 {
        write(
            &root.join(format!("chain{index}.xml")),
            &catalog(&format!(
                r#"<nextCatalog catalog="chain{}.xml"/><uri name="urn:{index}" uri="{index}.xsd"/>"#,
                index + 1
            )),
        );
    }
    let catalogs = Catalogs::new(vec![root.join("chain0.xml")]);
    assert_eq!(catalogs.files().len(), MAX_CATALOG_FILES);
    assert!(catalogs.resolve_uri("urn:0").is_some());
    assert_eq!(
        catalogs.resolve_uri(&format!("urn:{}", MAX_CATALOG_FILES + 5)),
        None
    );

    // Network shares are neither read as catalogs nor checked as targets.
    write(
        &root.join("network.xml"),
        &catalog(
            r#"<nextCatalog catalog="file://server/share/catalog.xml"/>
<uri name="urn:share" uri="file://server/share/s.xsd"/>"#,
        ),
    );
    let catalogs = Catalogs::new(vec![root.join("network.xml")]);
    let share = catalogs.resolve_location("urn:share").unwrap();
    assert!(is_network_path(&share), "{share:?}");
    let source = fs::read_to_string(root.join("network.xml")).unwrap();
    let problems = catalog_problems(&path_to_uri(&root.join("network.xml")), &source);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems
            .iter()
            .all(|problem| problem.message.starts_with("Network path never accessed"))
    );
    #[cfg(unix)]
    {
        let device = root.join("device.xml");
        std::os::unix::fs::symlink("/dev/zero", &device).unwrap();
        let catalogs = Catalogs::new(vec![device]);
        assert!(catalogs.resolve_uri("urn:a").is_none());
    }
    let _ = fs::remove_dir_all(&root);
}
