//! Schemas built into the server, for locations that would otherwise need
//! the network.
//!
//! The server never downloads anything, so `schemaLocation="http://www.w3.org/2001/xml.xsd"`
//! (the most common import of a schema) would only be reported. The
//! attributes of the XML namespace are few and fixed by the XML and `xml:id`
//! specifications: a schema describing them is embedded, written to the
//! temporary directory once (the schema loader reads files) and used when no
//! XML catalog maps the location. A catalog entry always wins.

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use xsd_core::SchemaLocation;

/// Namespace of the `xml:` attributes.
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

/// Schema text by file name.
const XML_XSD: &str = include_str!("../builtin/xml.xsd");

/// Locations (compared without scheme and case) standing for `xml.xsd`.
const XML_XSD_LOCATIONS: &[&str] = &[
    "www.w3.org/2001/xml.xsd",
    "www.w3.org/xml/1998/namespace",
    "www.w3.org/xml/1998/namespace.xsd",
];

/// Whether `location` is an `http(s)` URL standing for the XML namespace
/// schema.
fn is_xml_schema_location(location: &str) -> bool {
    let location = location.trim();
    let Some(rest) = location
        .strip_prefix("http://")
        .or_else(|| location.strip_prefix("https://"))
    else {
        return false;
    };
    XML_XSD_LOCATIONS.contains(&rest.to_ascii_lowercase().trim_end_matches('/'))
}

/// Built-in schema for `request`, as a local file.
pub fn resolve(request: &SchemaLocation<'_>) -> Option<PathBuf> {
    let wanted = match request.location {
        Some(location) => is_xml_schema_location(location),
        // An import without location for the XML namespace is the same schema.
        None => request.namespace == Some(XML_NAMESPACE),
    };
    wanted.then(|| materialize("xml.xsd", XML_XSD)).flatten()
}

/// Writes `text` once per process under a versioned temporary directory.
fn materialize(name: &str, text: &str) -> Option<PathBuf> {
    static WRITTEN: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    let mut written = WRITTEN.get_or_init(Mutex::default).lock().ok()?;
    written
        .entry(name.to_owned())
        .or_insert_with(|| {
            let directory = std::env::temp_dir().join(format!(
                "xml-lsp-builtin-{}-{}",
                env!("CARGO_PKG_VERSION"),
                std::process::id()
            ));
            fs::create_dir_all(&directory).ok()?;
            let path = directory.join(name);
            fs::write(&path, text).ok()?;
            Some(path)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use xsd_core::SchemaLocationKind;

    use super::*;

    fn request<'a>(namespace: Option<&'a str>, location: Option<&'a str>) -> SchemaLocation<'a> {
        SchemaLocation {
            kind: SchemaLocationKind::Import,
            namespace,
            location,
            base_directory: Path::new("."),
        }
    }

    #[test]
    fn maps_the_xml_namespace_schema_without_the_network() {
        for location in [
            "http://www.w3.org/2001/xml.xsd",
            "https://www.w3.org/2001/XML.xsd",
            " http://www.w3.org/XML/1998/namespace ",
        ] {
            let path = resolve(&request(Some(XML_NAMESPACE), Some(location))).expect(location);
            assert!(fs::read_to_string(path).unwrap().contains("name=\"lang\""));
        }
        assert!(resolve(&request(Some(XML_NAMESPACE), None)).is_some());
        assert!(resolve(&request(Some("urn:other"), None)).is_none());
        assert!(resolve(&request(None, Some("http://example.com/xml.xsd"))).is_none());
        assert!(resolve(&request(Some(XML_NAMESPACE), Some("local/xml.xsd"))).is_none());
    }

    #[test]
    fn validates_xml_attributes_of_a_schema_importing_the_remote_xml_xsd() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-builtin-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("s.xsd"),
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xml="http://www.w3.org/XML/1998/namespace">
  <xs:import namespace="http://www.w3.org/XML/1998/namespace" schemaLocation="http://www.w3.org/2001/xml.xsd"/>
  <xs:element name="doc"><xs:complexType>
    <xs:attribute ref="xml:lang"/><xs:attribute ref="xml:space"/>
  </xs:complexType></xs:element>
</xs:schema>"#,
        )
        .unwrap();
        let path = directory.join("doc.xml");
        let uri = crate::path_to_uri(&path);
        let mut server = crate::XmlLanguageServer::new();
        let mut check = |attributes: &str| {
            let source = format!(
                r#"<doc xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="s.xsd" {attributes}/>"#
            );
            server.documents.insert(uri.clone(), source.clone());
            server.diagnostics(&uri, &source)["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .map(|diagnostic| diagnostic["message"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            check(r#"xml:lang="fr-CA" xml:space="preserve""#),
            Vec::<String>::new()
        );
        let problems = check(r#"xml:lang="fr" xml:space="bogus""#);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("xml:space"), "{problems:?}");
        let _ = fs::remove_dir_all(&directory);
    }
}
