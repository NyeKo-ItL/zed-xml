use serde_json::json;

use super::*;
use crate::catalog::Catalogs;

#[test]
fn missing_or_invalid_settings_keep_defaults() {
    let defaults = Settings::default();
    assert_eq!(Settings::from_value(&Value::Null), defaults);
    assert_eq!(Settings::from_value(&json!({})), defaults);
    assert_eq!(Settings::from_value(&json!({"xml": null})), defaults);
    assert_eq!(Settings::from_value(&json!("xml")), defaults);
    assert_eq!(
        Settings::from_value(&json!({"xml": {
            "format": {"enabled": "no", "maxLineWidth": -3, "splitAttributes": "diagonal",
                       "emptyElements": 1, "tabSize": "4", "preservedNewlines": null},
            "validation": {"enabled": 0, "schema": {"enabled": "sometimes"}, "noGrammar": true},
            "completion": [],
            "symbols": {"maxItemsComputed": "10"},
            "catalogs": "catalog.xml",
            "autoDetectCatalogs": "yes",
            "fileAssociations": [{"pattern": 1, "systemId": "a.xsd"}, {"pattern": "*.xml"}, "x"],
        }})),
        defaults
    );
    // The formatting defaults are those of `FormatOptions`.
    let mut options = FormatOptions::default();
    defaults.format.apply(&mut options);
    assert_eq!(options, FormatOptions::default());
}

#[test]
fn reads_every_lemminx_setting() {
    let settings = Settings::from_value(&json!({"settings": {"xml": {
        "format": {
            "enabled": false, "splitAttributes": "alignWithFirstAttr", "maxLineWidth": 80,
            "preservedNewlines": 2, "closingBracketNewLine": true, "emptyElements": "collapse",
            "preserveAttributeLineBreaks": false, "insertSpaces": false, "tabSize": 4,
            "trimFinalNewlines": false,
        },
        "validation": {
            "enabled": false, "schema": {"enabled": "onValidSchema"}, "noGrammar": "warning",
            "disallowDocTypeDecl": true, "resolveExternalEntities": true, "debounce": 50,
        },
        "completion": {"autoCloseTags": false},
        "symbols": {"enabled": false, "maxItemsComputed": 10},
        "colors": {"enabled": false},
        "catalogs": ["catalog.xml", 3],
        "autoDetectCatalogs": true,
        "maxFileSize": 1024,
        "fileAssociations": [{"pattern": "**/*.pom", "systemId": "maven.xsd"}],
    }}}));
    assert!(settings.auto_detect_catalogs);
    assert_eq!(settings.max_file_size, Some(1024));
    assert!(settings.is_large(&"x".repeat(1025)));
    assert!(!settings.is_large(&"x".repeat(1024)));
    assert!(!settings.format.enabled);
    assert_eq!(
        settings.format.split_attributes,
        SplitAttributes::AlignWithFirstAttr
    );
    assert_eq!(settings.format.max_line_width, 80);
    assert_eq!(settings.format.empty_elements, EmptyElements::Collapse);
    let mut options = FormatOptions::default();
    settings.format.apply(&mut options);
    assert_eq!(
        options,
        FormatOptions {
            tab_size: 4,
            insert_spaces: false,
            trim_final_newlines: false,
            preserved_newlines: 2,
            split_attributes: SplitAttributes::AlignWithFirstAttr,
            max_line_width: 80,
            closing_bracket_new_line: true,
            empty_elements: EmptyElements::Collapse,
            preserve_attribute_line_breaks: false,
            ..FormatOptions::default()
        }
    );
    assert_eq!(
        settings.validation,
        ValidationSettings {
            enabled: false,
            schema: SchemaValidation::OnValidSchema,
            no_grammar: NoGrammar::Warning,
            disallow_doc_type_decl: true,
            resolve_external_entities: true,
            debounce_ms: 50,
        }
    );
    assert!(!settings.auto_close_tags);
    assert!(!settings.symbols_enabled);
    assert_eq!(settings.symbols_max_items, Some(10));
    assert!(!settings.colors_enabled);
    assert_eq!(settings.catalogs, vec!["catalog.xml"]);
    assert_eq!(
        settings.file_associations,
        vec![FileAssociation {
            pattern: "**/*.pom".to_owned(),
            system_id: "maven.xsd".to_owned(),
        }]
    );
}

#[test]
fn reads_performance_settings_with_their_defaults_and_bounds() {
    let defaults = Settings::default();
    assert_eq!(defaults.max_file_size, Some(DEFAULT_MAX_FILE_SIZE));
    assert_eq!(defaults.validation.debounce_ms, DEFAULT_VALIDATION_DEBOUNCE);
    let read = |value: Value| Settings::from_value(&json!({"xml": value}));
    // 0 or null: no limit; invalid types keep the default.
    assert_eq!(read(json!({"maxFileSize": 0})).max_file_size, None);
    assert_eq!(read(json!({"maxFileSize": null})).max_file_size, None);
    assert_eq!(
        read(json!({"maxFileSize": "big"})).max_file_size,
        Some(DEFAULT_MAX_FILE_SIZE)
    );
    assert_eq!(
        read(json!({"validation": {"debounce": 0}}))
            .validation
            .debounce_ms,
        0
    );
    assert_eq!(
        read(json!({"validation": {"debounce": 999_999}}))
            .validation
            .debounce_ms,
        10_000
    );
    assert!(!read(json!({"maxFileSize": 0})).is_large(&"x".repeat(1 << 20)));
    // A changed limit changes the published diagnostics.
    assert!(!defaults.same_validation(&read(json!({"maxFileSize": 5}))));
}

#[test]
fn accepts_alternative_setting_shapes() {
    // Section content, booleans of older LemMinX versions and dotted
    // keys.
    let settings = Settings::from_value(&json!({
        "format.splitAttributes": true,
        "validation": {"schema": false},
    }));
    assert_eq!(
        settings.format.split_attributes,
        SplitAttributes::SplitNewLine
    );
    assert_eq!(settings.validation.schema, SchemaValidation::Never);
    let settings = Settings::from_value(
        &json!({"xml": {"format": {"splitAttributes": "indent"}, "validation": {"schema": {"enabled": "never"}}}}),
    );
    assert_eq!(
        settings.format.split_attributes,
        SplitAttributes::SplitNewLine
    );
    assert_eq!(settings.validation.schema, SchemaValidation::Never);
}

#[test]
fn merges_settings_recursively() {
    let mut base = json!({"format": {"enabled": false, "tabSize": 4}, "catalogs": ["a"]});
    merge(
        &mut base,
        &json!({"format": {"tabSize": 8}, "catalogs": ["b"], "colors": null}),
    );
    assert_eq!(
        base,
        json!({"format": {"enabled": false, "tabSize": 8}, "catalogs": ["b"]})
    );
}

#[test]
fn limits_nested_symbols_in_document_order() {
    let mut symbols = vec![
        json!({"name": "a", "children": [{"name": "b"}, {"name": "c", "children": [{"name": "d"}]}]}),
        json!({"name": "e"}),
    ];
    limit_symbols(&mut symbols, 3);
    assert_eq!(
        symbols,
        vec![json!({"name": "a", "children": [{"name": "b"}, {"name": "c", "children": []}]})]
    );
}

#[test]
fn matches_glob_patterns() {
    assert!(glob_match("*.xml", "pom.xml"));
    assert!(!glob_match("*.xml", "dir/pom.xml"));
    assert!(glob_match("**/*.xml", "pom.xml"));
    assert!(glob_match("**/*.xml", "a/b/pom.xml"));
    assert!(glob_match("src/**/beans-?.xml", "src/main/beans-1.xml"));
    assert!(glob_match("src/**/beans-?.xml", "src/beans-1.xml"));
    assert!(!glob_match("src/**/beans-?.xml", "src/beans-10.xml"));
    assert!(glob_match("config/*.{xml,xsl}", "config/a.xsl"));
    assert!(!glob_match("config/*.{xml,xsl}", "config/a.xsd"));
    assert!(glob_match("**", "any/thing"));
    assert!(glob_match("/abs/**/x.xml", "/abs/x.xml"));
    assert!(!glob_match("a*b", "a/b"));
}

#[test]
fn resolves_file_associations() {
    let root = PathBuf::from("/work/project");
    let associations = [
        FileAssociation {
            pattern: "**/*.pom".to_owned(),
            system_id: "schemas/maven.xsd".to_owned(),
        },
        FileAssociation {
            pattern: "beans.xml".to_owned(),
            system_id: "file:///opt/spring%20beans.xsd".to_owned(),
        },
        FileAssociation {
            pattern: "config/*.xml".to_owned(),
            system_id: "/abs/config.xsd".to_owned(),
        },
        FileAssociation {
            pattern: "*.xml".to_owned(),
            system_id: "https://example.com/remote.xsd".to_owned(),
        },
    ];
    let roots = [root.clone()];
    let none = Catalogs::default();
    assert_eq!(
        associated_schemas(&associations, &roots, &root.join("a/b/project.pom"), &none),
        vec![root.join("schemas/maven.xsd")]
    );
    assert_eq!(
        associated_schemas(&associations, &roots, &root.join("x/beans.xml"), &none),
        vec![PathBuf::from("/opt/spring beans.xsd")]
    );
    assert_eq!(
        associated_schemas(&associations, &roots, &root.join("config/app.xml"), &none),
        vec![PathBuf::from("/abs/config.xsd")]
    );
    assert!(
        associated_schemas(&associations, &roots, &root.join("other/app.xml"), &none).is_empty()
    );
    // Outside a workspace: relative to the document's directory.
    assert_eq!(
        associated_schemas(&associations, &[], Path::new("/tmp/x/a.pom"), &none),
        vec![PathBuf::from("/tmp/x/schemas/maven.xsd")]
    );

    // A catalog makes the remote URL usable.
    let directory =
        std::env::temp_dir().join(format!("xml-lsp-associations {}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let catalog = directory.join("catalog.xml");
    std::fs::write(
        &catalog,
        r#"<catalog xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog">
                <system systemId="https://example.com/remote.xsd" uri="local.xsd"/>
            </catalog>"#,
    )
    .unwrap();
    let catalogs = Catalogs::new(vec![catalog]);
    assert_eq!(
        associated_schemas(
            &associations,
            &roots,
            &root.join("other/app.xml"),
            &catalogs
        ),
        vec![directory.join("local.xsd")]
    );
    let _ = std::fs::remove_dir_all(&directory);
}
