//! Symboles : `textDocument/documentSymbol` hiérarchique et `workspace/symbol`.
//!
//! `workspace/symbol` indexe les documents ouverts et les fichiers XML/XSD des
//! dossiers du workspace (parcours paresseux au premier appel, borné en nombre
//! et en taille de fichiers, cache par date de modification ; un buffer ouvert
//! remplace toujours le contenu sur disque).
//!
//! Choix des symboles, à la manière d'IntelliJ (« Go to Symbol ») :
//! - XSD : composants globaux nommés (`xs:element`, `xs:attribute`,
//!   `xs:complexType`, `xs:simpleType`, `xs:group`, `xs:attributeGroup`,
//!   `xs:notation`, y compris sous `xs:redefine`/`xs:override`) ;
//! - XML : l'élément racine et les éléments identifiés par `xml:id`, `id` ou
//!   `name` (`<bean id="x">`). Lister chaque élément de chaque fichier
//!   noierait les résultats (LemMinX ne publie l'arbre complet que dans
//!   `textDocument/documentSymbol`, pas dans le workspace).

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

/// `SymbolKind` LSP utilisés.
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

/// Extensions indexées sur disque (celles du langage XML de l'extension).
const INDEXED_EXTENSIONS: &[&str] = &[
    "xml", "xsd", "xsl", "xslt", "svg", "wsdl", "plist", "xjb", "axml",
];
/// Motif des fichiers surveillés (`workspace/didChangeWatchedFiles`).
pub(crate) const WATCHED_FILES_GLOB: &str = "**/*.{xml,xsd,xsl,xslt,svg,wsdl,plist,xjb,axml}";
/// Dossiers jamais parcourus (en plus des dossiers cachés).
const SKIPPED_DIRECTORIES: &[&str] = &["target", "node_modules", "bower_components"];
/// Nombre maximal de fichiers indexés sur disque.
pub(crate) const MAX_FILES: usize = 5_000;
/// Nombre maximal d'entrées de répertoire examinées par parcours.
const MAX_ENTRIES: usize = 100_000;
/// Taille maximale d'un fichier indexé.
pub(crate) const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024;
/// Nombre maximal de symboles retenus par fichier.
const MAX_SYMBOLS_PER_FILE: usize = 2_000;
/// Profondeur au-delà de laquelle les éléments ne sont plus publiés dans
/// `textDocument/documentSymbol` (leur étendue reste couverte par leurs
/// ancêtres) : borne la récursion des clients et de la sérialisation JSON.
const MAX_SYMBOL_DEPTH: usize = 128;
/// Nombre maximal de résultats pour une requête non vide.
pub(crate) const MAX_RESULTS: usize = 256;
/// Nombre maximal de résultats pour une requête vide.
pub(crate) const MAX_EMPTY_QUERY_RESULTS: usize = 100;
/// Délai minimal entre deux parcours du disque (sauf notification de
/// changement de fichiers).
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// Symbole indexé, positions LSP déjà calculées.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IndexedSymbol {
    pub name: String,
    pub kind: u32,
    pub container: Option<String>,
    /// `Range` LSP (UTF-16) du nom du symbole.
    pub range: Value,
}

/// Élément et attributs de sa balise ouvrante.
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

    /// Valeur (entités résolues, espaces de bord retirés) et étendue.
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

    /// Espace de noms de l'élément (`None` : préfixe non déclaré).
    fn namespace(&self, element: usize) -> Option<Option<&'s str>> {
        let name = self.tree.elements()[element].start_tag.name.clone();
        let (prefix, _) = qualified_name_parts(self.source, name);
        let prefix = prefix.map(|range| &self.source[range]);
        resolve_namespace(self.source, &self.tree, &self.attributes, element, prefix)
    }

    /// Élément dans l'espace de noms XSD (préfixe non déclaré toléré quand
    /// `lenient`, pour un `.xsd` en cours d'écriture).
    fn is_xsd(&self, element: usize, lenient: bool) -> bool {
        match self.namespace(element) {
            Some(namespace) => namespace == Some(XSD_NAMESPACE),
            None => lenient,
        }
    }

    /// Premier attribut identifiant (`xml:id`, `id`, puis `name`).
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

/// Symboles de workspace d'un document. `location` est l'URI ou le chemin
/// du document (utilisé pour le nom de fichier et l'extension).
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

/// `DocumentSymbol[]` hiérarchiques : un symbole par élément (tolérant aux
/// documents mal formés), `detail` = attribut identifiant (`id="x"`).
/// L'étendue d'un élément non fermé couvre ses descendants.
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
    // Les parents précèdent leurs enfants : construction de la fin vers le
    // début, sans récursion (documents très imbriqués).
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

/// Score de correspondance insensible à la casse (plus petit = meilleur) :
/// égalité, préfixe (du nom ou de sa partie locale), sous-chaîne (au plus
/// tôt), puis sous-séquence (au moins de trous). `None` : pas de
/// correspondance. `query` doit être en minuscules.
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

/// Symboles déjà calculés d'un fichier sur disque.
struct CachedFile {
    modified: SystemTime,
    len: u64,
    uri: String,
    symbols: Arc<Vec<IndexedSymbol>>,
}

/// Index des symboles du workspace.
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
    /// Dossiers du workspace depuis les paramètres `initialize`
    /// (`workspaceFolders`, sinon `rootUri`, sinon `rootPath`).
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

    /// `workspace/didChangeWatchedFiles` : oublie les fichiers modifiés et
    /// force un nouveau parcours à la prochaine requête.
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

    /// Met à jour le cache depuis le disque (paresseux, borné).
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

    /// Répond à `workspace/symbol` (`SymbolInformation[]`), documents ouverts
    /// prioritaires sur le disque.
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

/// Fichiers indexables des dossiers : `(chemin, date de modification,
/// taille)`, dossiers cachés, `target/`, `node_modules/`... ignorés, liens
/// symboliques non suivis, bornes [`MAX_FILES`] et [`MAX_FILE_SIZE`].
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
                let indexed = Path::new(name.as_ref())
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        INDEXED_EXTENSIONS
                            .iter()
                            .any(|indexed| extension.eq_ignore_ascii_case(indexed))
                    });
                if !indexed {
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
            // Ordre de parcours déterministe (premier dossier d'abord).
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
        // L'étendue couvre la valeur de `name` (sans guillemets).
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
        // Élément non fermé : son étendue couvre ses descendants.
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

        // Le buffer ouvert (non enregistré) remplace le fichier sur disque.
        documents.insert(
            path_to_uri(&directory.join("beans.xml")),
            "<beans><bean id=\"orderRepository\"/></beans>".to_owned(),
        );
        assert_eq!(
            names(&index.query(&documents, "orderr")),
            vec!["orderRepository"]
        );
        documents.clear();

        // Modification sur disque : le cache est invalidé par la taille ou la
        // date de modification ; un fichier supprimé disparaît.
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

        // Requête vide : liste bornée.
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
