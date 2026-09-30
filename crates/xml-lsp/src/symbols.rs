//! Symbols: hierarchical `textDocument/documentSymbol` and `workspace/symbol`.
//!
//! `workspace/symbol` indexes open documents and the XML/XSD files of the
//! workspace folders (lazy scan on the first call, bounded in number and
//! file size, cached by modification time; an open buffer always replaces
//! the content on disk).
//!
//! Symbol selection, in the manner of IntelliJ ("Go to Symbol"):
//! - XSD: named global components (`xs:element`, `xs:attribute`,
//!   `xs:complexType`, `xs:simpleType`, `xs:group`, `xs:attributeGroup`,
//!   `xs:notation`, including under `xs:redefine`/`xs:override`);
//! - XML: the root element and the elements identified by `xml:id`, `id` or
//!   `name` (`<bean id="x">`). Listing every element of every file would
//!   drown the results (LemMinX only publishes the full tree in
//!   `textDocument/documentSymbol`, not in the workspace).

use std::{
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use serde_json::{Value, json};
use xml_core::tags::{XmlAttribute, XmlTagTree, qualified_name_parts, resolve_namespace};
use xsd_core::model::XSD_NAMESPACE;

use crate::{links::unescape, path_to_uri, selection::LineIndex, uri_to_path};

/// LSP `SymbolKind`s used.
pub(crate) mod kind {
    pub const MODULE: u32 = 2;
    pub const CLASS: u32 = 5;
    pub const PROPERTY: u32 = 7;
    pub const FIELD: u32 = 8;
    pub const ENUM: u32 = 10;
    pub const VARIABLE: u32 = 13;
    pub const CONSTANT: u32 = 14;
    pub const KEY: u32 = 20;
    pub const STRUCT: u32 = 23;
}

/// File name suffixes of the extension's `XML` language (`path_suffixes` of
/// `languages/xml/config.toml`, kept identical by a test): the files indexed
/// on disk and watched.
pub(crate) const XML_PATH_SUFFIXES: &[&str] = &[
    "xml",
    "xsd",
    "xsl",
    "xslt",
    "rng",
    "wsdl",
    "xjb",
    "svg",
    "xhtml",
    "xht",
    "rss",
    "atom",
    "opml",
    "opf",
    "dita",
    "ditamap",
    "xul",
    "plist",
    "entitlements",
    "storyboard",
    "xib",
    "xcscheme",
    "xcworkspacedata",
    "tmTheme",
    "tmLanguage",
    "xaml",
    "axaml",
    "fsproj",
    "vbproj",
    "vcxproj",
    "vcxproj.filters",
    "csproj.user",
    "nuspec",
    "resx",
    "pubxml",
    "wxs",
    "wxi",
    "wxl",
    "pom",
    "fxml",
    "iml",
    "tld",
    "axml",
    "xlf",
    "xliff",
    "tmx",
    "kml",
    "gpx",
    "graphml",
    "musicxml",
    "bpmn",
];

/// Whether a file name ends with `.<suffix>` for one of [`XML_PATH_SUFFIXES`]
/// (ASCII case-insensitive, so `Foo.XML` and `a.tmtheme` are indexed too).
pub(crate) fn is_xml_file_name(name: &str) -> bool {
    XML_PATH_SUFFIXES.iter().any(|suffix| {
        name.len()
            .checked_sub(suffix.len() + 1)
            .and_then(|dot| name.get(dot..))
            .and_then(|tail| tail.strip_prefix('.'))
            .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
    })
}

/// Pattern of the watched files (`workspace/didChangeWatchedFiles`).
pub(crate) fn watched_files_glob() -> String {
    format!("**/*.{{{}}}", XML_PATH_SUFFIXES.join(","))
}
/// Directories never scanned (in addition to hidden directories).
const SKIPPED_DIRECTORIES: &[&str] = &["target", "node_modules", "bower_components"];
/// Maximum number of files indexed on disk.
pub(crate) const MAX_FILES: usize = 5_000;
/// Maximum number of directory entries examined per scan.
const MAX_ENTRIES: usize = 100_000;
/// Maximum size of an indexed file.
pub(crate) const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024;
/// Maximum number of symbols kept per file.
const MAX_SYMBOLS_PER_FILE: usize = 2_000;
/// Depth beyond which elements are no longer published in
/// `textDocument/documentSymbol` (their range stays covered by their
/// ancestors): bounds the recursion of clients and of JSON serialization.
const MAX_SYMBOL_DEPTH: usize = 128;
/// Maximum number of results for a non-empty query.
pub(crate) const MAX_RESULTS: usize = 256;
/// Maximum number of results for an empty query.
pub(crate) const MAX_EMPTY_QUERY_RESULTS: usize = 100;
/// Minimum delay between two disk scans (unless a file change notification
/// arrives).
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// Indexed symbol, LSP positions already computed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IndexedSymbol {
    pub name: String,
    pub kind: u32,
    pub container: Option<String>,
    /// LSP `Range` (UTF-16) of the symbol name.
    pub range: Value,
}

/// Element and the attributes of its start tag.
struct Parsed<'s> {
    source: &'s str,
    tree: XmlTagTree,
    attributes: Vec<Vec<XmlAttribute>>,
}

impl<'s> Parsed<'s> {
    fn new(source: &'s str) -> Self {
        let tree = XmlTagTree::parse(source);
        let attributes = tree
            .elements()
            .iter()
            .map(|element| xml_core::tags::scan_attributes(source, &element.start_tag))
            .collect();
        Self {
            source,
            tree,
            attributes,
        }
    }

    fn attribute(&self, element: usize, name: &str) -> Option<&XmlAttribute> {
        self.attributes[element]
            .iter()
            .find(|attribute| attribute.name(self.source) == name)
    }

    /// Value (entities resolved, surrounding whitespace removed) and range.
    fn attribute_value(&self, element: usize, name: &str) -> Option<(String, Range<usize>)> {
        let range = self.attribute(element, name)?.value.clone()?;
        let value = unescape(self.source[range.clone()].trim());
        (!value.is_empty()).then_some((value, range))
    }

    fn local_name(&self, element: usize) -> &'s str {
        let name = self.tree.elements()[element].start_tag.name.clone();
        let (_, local) = qualified_name_parts(self.source, name);
        &self.source[local]
    }

    /// Namespace of the element (`None`: undeclared prefix).
    fn namespace(&self, element: usize) -> Option<Option<&'s str>> {
        let name = self.tree.elements()[element].start_tag.name.clone();
        let (prefix, _) = qualified_name_parts(self.source, name);
        let prefix = prefix.map(|range| &self.source[range]);
        resolve_namespace(self.source, &self.tree, &self.attributes, element, prefix)
    }

    /// Element in the XSD namespace (undeclared prefix tolerated when
    /// `lenient`, for a `.xsd` being written).
    fn is_xsd(&self, element: usize, lenient: bool) -> bool {
        match self.namespace(element) {
            Some(namespace) => namespace == Some(XSD_NAMESPACE),
            None => lenient,
        }
    }

    /// First identifying attribute (`xml:id`, `id`, then `name`).
    fn identifier(&self, element: usize) -> Option<(&'static str, String, Range<usize>)> {
        ["xml:id", "id", "name"].into_iter().find_map(|name| {
            self.attribute_value(element, name)
                .map(|(value, range)| (name, value, range))
        })
    }
}

fn file_name(uri_or_path: &str) -> &str {
    uri_or_path
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(uri_or_path)
}

fn has_xsd_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xsd"))
}

/// Workspace symbols of a document. `location` is the URI or path of the
/// document (used for the file name and extension).
pub(crate) fn index_document(source: &str, location: &str) -> Vec<IndexedSymbol> {
    let parsed = Parsed::new(source);
    let lines = LineIndex::new(source);
    let range = |range: Range<usize>| {
        json!({
            "start": lines.position(source, range.start),
            "end": lines.position(source, range.end),
        })
    };
    let file = file_name(location);
    let Some(root) = parsed
        .tree
        .elements()
        .iter()
        .position(|element| element.parent.is_none())
    else {
        return Vec::new();
    };

    let mut symbols = Vec::new();
    if parsed.local_name(root) == "schema" && parsed.is_xsd(root, has_xsd_extension(file)) {
        let container = parsed
            .attribute_value(root, "targetNamespace")
            .map(|(namespace, _)| namespace)
            .unwrap_or_else(|| file.to_owned());
        let lenient = has_xsd_extension(file);
        for (index, element) in parsed.tree.elements().iter().enumerate() {
            let Some(parent) = element.parent else {
                continue;
            };
            let top_level = parent == root
                || (parsed.tree.elements()[parent].parent == Some(root)
                    && matches!(parsed.local_name(parent), "redefine" | "override")
                    && parsed.is_xsd(parent, lenient));
            if !top_level || !parsed.is_xsd(index, lenient) {
                continue;
            }
            let symbol_kind = match parsed.local_name(index) {
                "element" => kind::FIELD,
                "attribute" => kind::PROPERTY,
                "complexType" => kind::CLASS,
                "simpleType" if has_enumeration(&parsed, index) => kind::ENUM,
                "simpleType" => kind::CLASS,
                "group" => kind::STRUCT,
                "attributeGroup" => kind::MODULE,
                "notation" => kind::CONSTANT,
                _ => continue,
            };
            let Some((name, name_range)) = parsed.attribute_value(index, "name") else {
                continue;
            };
            symbols.push(IndexedSymbol {
                name,
                kind: symbol_kind,
                container: Some(container.clone()),
                range: range(name_range),
            });
            if symbols.len() >= MAX_SYMBOLS_PER_FILE {
                break;
            }
        }
        return symbols;
    }

    let root_tag = &parsed.tree.elements()[root].start_tag;
    symbols.push(IndexedSymbol {
        name: root_tag.name(source).to_owned(),
        kind: kind::MODULE,
        container: Some(file.to_owned()),
        range: range(root_tag.name.clone()),
    });
    for (index, element) in parsed.tree.elements().iter().enumerate() {
        if symbols.len() >= MAX_SYMBOLS_PER_FILE {
            break;
        }
        let Some((attribute, value, value_range)) = parsed.identifier(index) else {
            continue;
        };
        symbols.push(IndexedSymbol {
            name: value,
            kind: if attribute == "name" {
                kind::FIELD
            } else {
                kind::KEY
            },
            container: Some(element.name(source).to_owned()),
            range: range(value_range),
        });
    }
    symbols
}

fn has_enumeration(parsed: &Parsed, simple_type: usize) -> bool {
    let range = parsed.tree.elements()[simple_type].range();
    parsed
        .tree
        .elements()
        .iter()
        .enumerate()
        .skip(simple_type + 1)
        .take_while(|(_, element)| element.start_tag.range.start < range.end)
        .any(|(index, _)| parsed.local_name(index) == "enumeration")
}

/// Hierarchical `DocumentSymbol[]`: one symbol per element (tolerant of
/// malformed documents), `detail` = identifying attribute (`id="x"`).
/// The range of an unclosed element covers its descendants.
pub(crate) fn document_symbols(source: &str) -> Vec<Value> {
    let parsed = Parsed::new(source);
    let lines = LineIndex::new(source);
    let elements = parsed.tree.elements();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); elements.len()];
    let mut roots = Vec::new();
    for (index, element) in elements.iter().enumerate() {
        match element.parent {
            Some(parent) => children[parent].push(index),
            None => roots.push(index),
        }
    }
    // Parents precede their children: built from the end to the start,
    // without recursion (deeply nested documents).
    let mut built: Vec<Option<(Option<Value>, usize)>> = vec![None; elements.len()];
    for index in (0..elements.len()).rev() {
        let element = &elements[index];
        let mut end = element.range().end;
        let mut nested = Vec::new();
        for &child in &children[index] {
            if let Some((symbol, child_end)) = built[child].take() {
                end = end.max(child_end);
                nested.extend(symbol);
            }
        }
        if element.depth >= MAX_SYMBOL_DEPTH {
            built[index] = Some((None, end));
            continue;
        }
        let start = element.start_tag.range.start;
        let mut symbol = json!({
            "name": element.name(source),
            "kind": kind::VARIABLE,
            "range": {
                "start": lines.position(source, start),
                "end": lines.position(source, end),
            },
            "selectionRange": {
                "start": lines.position(source, element.start_tag.name.start),
                "end": lines.position(source, element.start_tag.name.end),
            },
        });
        if let Some((attribute, value, _)) = parsed.identifier(index) {
            symbol["detail"] = json!(format!("{attribute}=\"{value}\""));
        }
        if !nested.is_empty() {
            symbol["children"] = Value::Array(nested);
        }
        built[index] = Some((Some(symbol), end));
    }
    roots
        .into_iter()
        .filter_map(|index| built[index].take().and_then(|(symbol, _)| symbol))
        .collect()
}

/// Case-insensitive match score (smaller = better): equality, prefix (of
/// the name or its local part), substring (earliest), then subsequence
/// (fewest gaps). `None`: no match. `query` must be
/// lowercase.
pub(crate) fn match_score(query: &str, name: &str) -> Option<(u8, usize)> {
    if query.is_empty() {
        return Some((0, 0));
    }
    let name = name.to_lowercase();
    if name == query {
        return Some((0, 0));
    }
    if name.starts_with(query) {
        return Some((1, 0));
    }
    if let Some((_, local)) = name.split_once(':')
        && local.starts_with(query)
    {
        return Some((1, 1));
    }
    if let Some(position) = name.find(query) {
        return Some((2, position));
    }
    let mut gaps = 0;
    let mut previous: Option<usize> = None;
    let mut name_chars = name.char_indices();
    for wanted in query.chars() {
        let (position, _) = name_chars.find(|&(_, character)| character == wanted)?;
        if let Some(previous) = previous {
            gaps += position - previous - 1;
        } else {
            gaps += position;
        }
        previous = Some(position + wanted.len_utf8() - 1);
    }
    Some((3, gaps))
}

/// Already computed symbols of a file on disk.
struct CachedFile {
    modified: SystemTime,
    len: u64,
    uri: String,
    symbols: Arc<Vec<IndexedSymbol>>,
}

/// Workspace symbol index.
pub(crate) struct WorkspaceIndex {
    roots: Vec<PathBuf>,
    files: HashMap<PathBuf, CachedFile>,
    last_scan: Option<Instant>,
    refresh_interval: Duration,
}

impl Default for WorkspaceIndex {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            files: HashMap::new(),
            last_scan: None,
            refresh_interval: REFRESH_INTERVAL,
        }
    }
}

fn folder_path(uri: &str) -> Option<PathBuf> {
    uri.starts_with("file://").then(|| uri_to_path(uri))
}

impl WorkspaceIndex {
    /// Workspace folders from the `initialize` parameters
    /// (`workspaceFolders`, otherwise `rootUri`, otherwise `rootPath`).
    pub(crate) fn from_initialize_params(params: &Value) -> Self {
        let mut roots = params
            .get("workspaceFolders")
            .and_then(Value::as_array)
            .map(|folders| {
                folders
                    .iter()
                    .filter_map(|folder| folder.get("uri")?.as_str())
                    .filter_map(folder_path)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if roots.is_empty() {
            roots.extend(
                params
                    .get("rootUri")
                    .and_then(Value::as_str)
                    .and_then(folder_path),
            );
        }
        if roots.is_empty() {
            roots.extend(
                params
                    .get("rootPath")
                    .and_then(Value::as_str)
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from),
            );
        }
        let mut index = Self::default();
        for root in roots {
            index.add_root(root);
        }
        index
    }

    #[cfg(test)]
    pub(crate) fn with_roots(roots: Vec<PathBuf>) -> Self {
        let mut index = Self {
            refresh_interval: Duration::ZERO,
            ..Self::default()
        };
        for root in roots {
            index.add_root(root);
        }
        index
    }

    pub(crate) fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    fn add_root(&mut self, root: PathBuf) {
        if !self.roots.contains(&root) {
            self.roots.push(root);
        }
    }

    /// `workspace/didChangeWorkspaceFolders`.
    pub(crate) fn change_folders(&mut self, params: &Value) {
        let folders = |key: &str| {
            params
                .pointer(&format!("/event/{key}"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|folder| folder.get("uri")?.as_str())
                .filter_map(folder_path)
                .collect::<Vec<_>>()
        };
        let removed = folders("removed");
        self.roots.retain(|root| !removed.contains(root));
        for root in folders("added") {
            self.add_root(root);
        }
        self.last_scan = None;
    }

    /// `workspace/didChangeWatchedFiles`: forgets the modified files and
    /// forces a new scan on the next request.
    pub(crate) fn files_changed(&mut self, params: &Value) {
        for change in params
            .get("changes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(uri) = change.get("uri").and_then(Value::as_str) {
                self.files.remove(&uri_to_path(uri));
            }
        }
        self.last_scan = None;
    }

    /// Updates the cache from the disk (lazy, bounded).
    fn refresh(&mut self) {
        if self
            .last_scan
            .is_some_and(|last| last.elapsed() < self.refresh_interval)
        {
            return;
        }
        self.last_scan = Some(Instant::now());
        let mut seen = HashSet::new();
        for (path, modified, len) in scan_workspace(&self.roots) {
            let fresh = self
                .files
                .get(&path)
                .is_some_and(|cached| cached.modified == modified && cached.len == len);
            if !fresh {
                let Ok(source) = fs::read_to_string(&path) else {
                    continue;
                };
                let location = path.to_string_lossy();
                let symbols = Arc::new(index_document(&source, &location));
                self.files.insert(
                    path.clone(),
                    CachedFile {
                        modified,
                        len,
                        uri: path_to_uri(&path),
                        symbols,
                    },
                );
            }
            seen.insert(path);
        }
        self.files.retain(|path, _| seen.contains(path));
    }

    /// Answers `workspace/symbol` (`SymbolInformation[]`), open documents
    /// taking precedence over the disk.
    pub(crate) fn query(&mut self, documents: &HashMap<String, String>, query: &str) -> Vec<Value> {
        self.refresh();
        let query = query.trim().to_lowercase();
        let open_paths = documents
            .keys()
            .filter(|uri| uri.starts_with("file://"))
            .map(|uri| uri_to_path(uri))
            .collect::<HashSet<_>>();
        let open = documents
            .iter()
            .map(|(uri, source)| (uri.clone(), Arc::new(index_document(source, uri))));
        let disk = self
            .files
            .iter()
            .filter(|(path, _)| !open_paths.contains(*path))
            .map(|(_, cached)| (cached.uri.clone(), cached.symbols.clone()));

        let mut matches = Vec::new();
        for (uri, symbols) in open.collect::<Vec<_>>().into_iter().chain(disk) {
            for symbol in symbols.iter() {
                if let Some(score) = match_score(&query, &symbol.name) {
                    matches.push((score, symbol.clone(), uri.clone()));
                }
            }
        }
        matches.sort_by(|(score_a, a, uri_a), (score_b, b, uri_b)| {
            score_a
                .cmp(score_b)
                .then(a.name.len().cmp(&b.name.len()))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| uri_a.cmp(uri_b))
                .then_with(|| a.kind.cmp(&b.kind))
        });
        let limit = if query.is_empty() {
            MAX_EMPTY_QUERY_RESULTS
        } else {
            MAX_RESULTS
        };
        matches
            .into_iter()
            .take(limit)
            .map(|(_, symbol, uri)| {
                let mut value = json!({
                    "name": symbol.name,
                    "kind": symbol.kind,
                    "location": {"uri": uri, "range": symbol.range},
                });
                if let Some(container) = symbol.container {
                    value["containerName"] = json!(container);
                }
                value
            })
            .collect()
    }
}

/// Indexable files of the folders: `(path, modification time, size)`;
/// hidden directories, `target/`, `node_modules/`... ignored, symbolic
/// links not followed, bounded by [`MAX_FILES`] and [`MAX_FILE_SIZE`].
pub(crate) fn scan_workspace(roots: &[PathBuf]) -> Vec<(PathBuf, SystemTime, u64)> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut entries = 0usize;
    for root in roots {
        let mut stack = vec![root.clone()];
        while let Some(directory) = stack.pop() {
            let Ok(read) = fs::read_dir(&directory) else {
                continue;
            };
            let mut children = read.flatten().collect::<Vec<_>>();
            children.sort_by_key(|entry| entry.file_name());
            let mut subdirectories = Vec::new();
            for entry in children {
                entries += 1;
                if entries > MAX_ENTRIES || files.len() >= MAX_FILES {
                    return files;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if file_type.is_dir() {
                    if !name.starts_with('.') && !SKIPPED_DIRECTORIES.contains(&name.as_ref()) {
                        subdirectories.push(entry.path());
                    }
                    continue;
                }
                if !file_type.is_file() || name.starts_with('.') {
                    continue;
                }
                if !is_xml_file_name(&name) {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                let path = entry.path();
                if metadata.len() > MAX_FILE_SIZE || !seen.insert(path.clone()) {
                    continue;
                }
                let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                files.push((path, modified, metadata.len()));
            }
            // Deterministic scan order (first folder first).
            stack.extend(subdirectories.into_iter().rev());
        }
    }
    files
}

#[cfg(test)]
mod tests {
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
        let config = include_str!("../../../languages/xml/config.toml");
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
}
