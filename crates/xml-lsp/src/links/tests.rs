use super::*;
use std::fs;

use crate::catalog::Catalogs;

/// Test-specific temporary directory, removed at the end.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("xml-lsp-links-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir should be created");
        Self(path)
    }

    fn file(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).expect("parent should be created");
        fs::write(&path, "<root/>").expect("file should be written");
        path
    }

    fn document_uri(&self, name: &str) -> String {
        path_to_uri(&self.0.join(name))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn references(source: &str) -> Vec<(LinkKind, &str, String)> {
    link_references(source)
        .into_iter()
        .map(|reference| {
            (
                reference.kind,
                &source[reference.range.clone()],
                reference.value,
            )
        })
        .collect()
}

#[test]
fn finds_schema_locations_by_namespace_with_any_prefix() {
    let source = r#"<r:root xmlns:r="urn:r" xmlns:inst="http://www.w3.org/2001/XMLSchema-instance"
  inst:schemaLocation="  urn:a  a.xsd
     urn:b	sub/b.xsd urn:odd "
  inst:noNamespaceSchemaLocation=" none.xsd "
  xsi:schemaLocation="urn:c c.xsd"
  other:schemaLocation="urn:d d.xsd" schemaLocation="urn:e e.xsd"/>"#;
    assert_eq!(
        references(source),
        vec![
            (LinkKind::SchemaLocation, "a.xsd", "a.xsd".to_owned()),
            (
                LinkKind::SchemaLocation,
                "sub/b.xsd",
                "sub/b.xsd".to_owned()
            ),
            (
                LinkKind::NoNamespaceSchemaLocation,
                "none.xsd",
                "none.xsd".to_owned()
            ),
            // Undeclared `xsi` prefix: conventional namespace.
            (LinkKind::SchemaLocation, "c.xsd", "c.xsd".to_owned()),
        ]
    );
}

#[test]
fn a_redeclared_xsi_prefix_is_not_the_instance_namespace() {
    let source = r#"<root xmlns:xsi="urn:not-xsi" xsi:noNamespaceSchemaLocation="a.xsd"/>"#;
    assert!(references(source).is_empty());
}

#[test]
fn finds_xsd_xinclude_and_xslt_references() {
    let source = r#"<schema xmlns="http://www.w3.org/2001/XMLSchema" xmlns:x="http://www.w3.org/2001/XInclude">
  <include schemaLocation="inc.xsd"/>
  <import namespace="urn:i" schemaLocation="imp.xsd"></import>
  <import namespace="urn:no-location"/>
  <redefine schemaLocation="red.xsd"/>
  <override schemaLocation="ovr.xsd"/>
  <x:include href="part.xml" parse="xml"/>
  <x:include href=""/>
  <element name="include" type="string"/>
  <t:stylesheet xmlns:t="http://www.w3.org/1999/XSL/Transform">
    <t:import href="base.xsl"/><t:include href='common.xsl'/>
  </t:stylesheet>
  <xsl:include href="undeclared.xsl"/>
</schema>"#;
    assert_eq!(
        references(source)
            .into_iter()
            .map(|(kind, text, _)| (kind, text))
            .collect::<Vec<_>>(),
        vec![
            (LinkKind::XsdInclude, "inc.xsd"),
            (LinkKind::XsdImport, "imp.xsd"),
            (LinkKind::XsdRedefine, "red.xsd"),
            (LinkKind::XsdOverride, "ovr.xsd"),
            (LinkKind::XInclude, "part.xml"),
            (LinkKind::XslImport, "base.xsl"),
            (LinkKind::XslInclude, "common.xsl"),
            (LinkKind::XslInclude, "undeclared.xsl"),
        ]
    );
}

#[test]
fn elements_outside_the_expected_namespace_are_ignored() {
    let source = r#"<root xmlns:xs="urn:other"><xs:include schemaLocation="a.xsd"/><include href="b.xml"/></root>"#;
    assert!(references(source).is_empty());
}

#[test]
fn finds_processing_instruction_and_doctype_references() {
    let source = "<?xml version=\"1.0\"?>\r\n\
<?xml-stylesheet type=\"text/xsl\" href=\"style.xsl\"?>\r\n\
<?xml-model href='model.rng' schematypens=\"http://relaxng.org/ns/structure/1.0\"?>\r\n\
<?xml-stylesheet xhref=\"no.css\"?>\r\n\
<?other href=\"no.css\"?>\r\n\
<!DOCTYPE note PUBLIC \"-//W3C//DTD//EN\" \"note.dtd\" [\r\n<!ENTITY e \"v\">\r\n]>\r\n\
<note/>";
    assert_eq!(
        references(source)
            .into_iter()
            .map(|(kind, text, _)| (kind, text))
            .collect::<Vec<_>>(),
        vec![
            (LinkKind::Stylesheet, "style.xsl"),
            (LinkKind::XmlModel, "model.rng"),
            (LinkKind::Doctype, "note.dtd"),
        ]
    );
    assert_eq!(
        references("<!DOCTYPE note SYSTEM 'sys.dtd'><note/>")[0].1,
        "sys.dtd"
    );
    assert!(references("<!DOCTYPE note [<!ELEMENT note ANY>]><note/>").is_empty());
}

#[test]
fn values_are_unescaped_but_ranges_stay_raw() {
    let source = r#"<root xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="a&amp;b&#x20;c&#46;xsd&unknown;"/>"#;
    assert_eq!(
        references(source),
        vec![(
            LinkKind::NoNamespaceSchemaLocation,
            "a&amp;b&#x20;c&#46;xsd&unknown;",
            "a&b c.xsd&unknown;".to_owned()
        )]
    );
}

#[test]
fn malformed_documents_do_not_panic() {
    for source in [
        "<root xsi:schemaLocation=\"urn:a a.xsd",
        "<?xml-stylesheet href=\"a.css",
        "<?xml-stylesheet href=",
        "<?xml-stylesheet = href",
        "<!DOCTYPE",
        "<!DOCTYPE a SYSTEM",
        "<!DOCTYPE a PUBLIC \"p\"",
        "<xs:include schemaLocation=",
        "é<xi:include href=\"😀.xml\"",
    ] {
        let _ = link_references(source);
    }
}

#[test]
fn resolves_relative_encoded_absolute_and_remote_targets() {
    let dir = TempDir::new("resolve");
    let spaced = dir.file("my schemas/a b.xsd");
    let nested = dir.file("sub/c.xsd");
    let document = dir.document_uri("docs dir/doc.xml");

    assert_eq!(
        resolve_target(&document, "../my schemas/a b.xsd"),
        Some(LinkTarget::File(spaced.clone()))
    );
    assert_eq!(
        resolve_target(&document, "../my%20schemas/a%20b.xsd"),
        Some(LinkTarget::File(spaced.clone()))
    );
    assert_eq!(
        resolve_target(&document, "../sub/./c.xsd#fragment"),
        Some(LinkTarget::File(nested.clone()))
    );
    assert_eq!(
        resolve_target(&document, &nested.to_string_lossy()),
        Some(LinkTarget::File(nested.clone()))
    );
    assert_eq!(
        resolve_target(&document, &path_to_uri(&spaced)),
        Some(LinkTarget::File(spaced))
    );
    assert_eq!(
        resolve_target(&document, "https://example.com/s.xsd?v=1&x=2"),
        Some(LinkTarget::Url(
            "https://example.com/s.xsd?v=1&x=2".to_owned()
        ))
    );
    assert_eq!(
        resolve_target(&document, "HTTP://example.com/s.xsd"),
        Some(LinkTarget::Url("HTTP://example.com/s.xsd".to_owned()))
    );
    for missing in [
        "../missing.xsd",
        "urn:example:schema",
        "ftp://example.com/a.xsd",
        "C:\\schemas\\a.xsd",
        "",
        "  ",
        "../sub",
    ] {
        assert_eq!(resolve_target(&document, missing), None, "{missing}");
    }
    // Unsaved document: no base for a relative path.
    assert_eq!(resolve_target("untitled:Untitled-1", "c.xsd"), None);
}

#[test]
fn document_links_keep_existing_files_and_urls_with_utf16_ranges() {
    let dir = TempDir::new("document");
    dir.file("a.xsd");
    let uri = dir.document_uri("doc.xml");
    let source = "<?xml-stylesheet href=\"missing.css\"?>\r\n<😀 xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n xsi:schemaLocation=\"urn:a a.xsd urn:b http://example.com/b.xsd\"/>";
    let links = document_links_json(&uri, source, &Catalogs::default());
    let a_start = source.find(" a.xsd").unwrap() + 1 - source.rfind('\n').unwrap() - 1;
    assert_eq!(
        links,
        json!([
            {
                "range": {
                    "start": {"line": 2, "character": a_start},
                    "end": {"line": 2, "character": a_start + 5},
                },
                "target": path_to_uri(&dir.0.join("a.xsd")),
                "tooltip": format!("Open XSD schema: {}", dir.0.join("a.xsd").display()),
            },
            {
                "range": {
                    "start": {"line": 2, "character": a_start + 12},
                    "end": {"line": 2, "character": a_start + 36},
                },
                "target": "http://example.com/b.xsd",
                "tooltip": "Open XSD schema: http://example.com/b.xsd",
            },
        ])
    );
    // UTF-16: the emoji counts as two code units.
    let emoji =
        "<a xmlns:xi=\"http://www.w3.org/2001/XInclude\"><xi:include href=\"😀/../a.xsd\"/></a>";
    let emoji_uri = dir.document_uri("emoji.xml");
    let range = &document_links_json(&emoji_uri, emoji, &Catalogs::default())[0]["range"];
    let start = emoji.find("😀").unwrap();
    assert_eq!(range["start"]["character"], start);
    assert_eq!(range["end"]["character"], start + 2 + "/../a.xsd".len());
}

#[test]
fn definition_targets_the_start_of_the_linked_file() {
    let dir = TempDir::new("definition");
    let schema = dir.file("a.xsd");
    let uri = dir.document_uri("doc.xml");
    let source = "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"a.xsd\" xsi:schemaLocation=\"urn:x http://example.com/x.xsd urn:y missing.xsd\"/>";
    let value = source.find("a.xsd").unwrap();
    let zero = json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}});

    for offset in [value, value + 2, value + 5] {
        assert_eq!(
            definition(&uri, source, offset, false, &Catalogs::default()),
            Some(json!([{"uri": path_to_uri(&schema), "range": zero}]))
        );
    }
    assert_eq!(
        definition(&uri, source, value, true, &Catalogs::default()),
        Some(json!([{
            "originSelectionRange": {
                "start": {"line": 0, "character": value},
                "end": {"line": 0, "character": value + 5},
            },
            "targetUri": path_to_uri(&schema),
            "targetRange": zero,
            "targetSelectionRange": zero,
        }]))
    );
    let url = source.find("http://example").unwrap();
    assert_eq!(
        definition(&uri, source, url + 3, false, &Catalogs::default()),
        Some(json!([]))
    );
    let missing = source.find("missing").unwrap();
    assert_eq!(
        definition(&uri, source, missing, false, &Catalogs::default()),
        Some(json!([]))
    );
    // Outside a value: left to element definition.
    assert_eq!(
        definition(&uri, source, 2, false, &Catalogs::default()),
        None
    );
    assert_eq!(
        definition(&uri, source, value - 1, false, &Catalogs::default()),
        None
    );
    let namespace = source.find("urn:x").unwrap();
    assert_eq!(
        definition(&uri, source, namespace + 1, false, &Catalogs::default()),
        None
    );
}

#[test]
fn resolves_links_through_xml_catalogs() {
    let dir = TempDir::new("catalog");
    let by_namespace = dir.file("local/ns.xsd");
    let by_system = dir.file("local/system.xsd");
    let by_public = dir.file("local/public.dtd");
    let imported = dir.file("local/imported.xsd");
    let catalog = dir.0.join("catalog.xml");
    fs::write(
        &catalog,
        r#"<catalog xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog">
  <uri name="urn:ns" uri="local/ns.xsd"/>
  <uri name="urn:imported" uri="local/imported.xsd"/>
  <rewriteSystem systemIdStartString="http://example.com/" rewritePrefix="local/"/>
  <public publicId="-//Example//DTD Doc//EN" uri="local/public.dtd"/>
  <uri name="urn:missing" uri="local/missing.xsd"/>
</catalog>"#,
    )
    .unwrap();
    let catalogs = Catalogs::new(vec![catalog]);
    let uri = dir.document_uri("doc.xml");
    let source = r#"<!DOCTYPE doc PUBLIC "-//Example//DTD Doc//EN" "http://nowhere/doc.dtd">
<doc xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xs="http://www.w3.org/2001/XMLSchema"
  xsi:schemaLocation="urn:ns http://remote/ns.xsd urn:other http://example.com/system.xsd urn:missing http://remote/m.xsd">
  <xs:import namespace="urn:imported" schemaLocation="http://remote/imported.xsd"/>
</doc>"#;
    let targets = document_links(&uri, source, &catalogs)
        .into_iter()
        .map(|link| (link.reference.kind, link.reference.key, link.target))
        .collect::<Vec<_>>();
    assert_eq!(
        targets,
        vec![
            (
                LinkKind::Doctype,
                Some("-//Example//DTD Doc//EN".to_owned()),
                LinkTarget::File(by_public)
            ),
            (
                LinkKind::SchemaLocation,
                Some("urn:ns".to_owned()),
                LinkTarget::File(by_namespace)
            ),
            (
                LinkKind::SchemaLocation,
                Some("urn:other".to_owned()),
                LinkTarget::File(by_system)
            ),
            // Missing catalog target: the original URL is kept.
            (
                LinkKind::SchemaLocation,
                Some("urn:missing".to_owned()),
                LinkTarget::Url("http://remote/m.xsd".to_owned())
            ),
            (
                LinkKind::XsdImport,
                Some("urn:imported".to_owned()),
                LinkTarget::File(imported.clone())
            ),
        ]
    );
    let offset = source.find("http://remote/imported").unwrap();
    assert_eq!(
        definition(&uri, source, offset, false, &catalogs),
        Some(json!([{
            "uri": path_to_uri(&imported),
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
        }]))
    );
}
