//! OASIS XML Catalogs 1.1 (`xml.catalogs` setting, like LemMinX).
//!
//! A catalog maps identifiers (system or public identifier of a DTD, URI of
//! a schema or namespace) to local files, which makes it possible to
//! validate and complete offline a document whose `xsi:schemaLocation`
//! points to an `http(s)` URL.
//!
//! Recognized entries (namespace
//! `urn:oasis:names:tc:entity:xmlns:xml:catalog`): `system`, `public`,
//! `uri`, `rewriteSystem`, `rewriteURI`, `systemSuffix`, `uriSuffix`,
//! `delegatePublic`, `delegateSystem`, `delegateURI`, `nextCatalog`, in
//! `catalog` and `group`, with `xml:base` (on any element) and `prefer`
//! (`public` by default, like Xerces). Relative paths are resolved against
//! `xml:base`, otherwise against the catalog file. Elements of another
//! namespace are ignored along with their content.
//!
//! Resolution order (OASIS specification, § 7) in each catalog of the
//! list: exact match, then rewrite with the longest prefix, then the
//! longest suffix, then delegation (new list made of the delegated
//! catalogs, longest prefixes first); failing that, the catalog's
//! `nextCatalog` entries (depth first, protected against cycles), then the
//! next catalog of the list.
//!
//! Catalogs are reread when their modification time or size changes (see
//! [`Catalogs::refresh`]).

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use xml_core::resource::{MAX_RESOURCE_SIZE, is_network_path, read_text_file};
use xml_core::tags::{
    XmlAttribute, XmlTagTree, qualified_name_parts, resolve_namespace, scan_attributes,
};
use xsd_core::{SchemaLocation, SchemaLocationKind, file_uri_to_path};

use crate::{links::unescape, path_to_uri};

/// Namespace of OASIS XML catalogs.
pub const CATALOG_NAMESPACE: &str = "urn:oasis:names:tc:entity:xmlns:xml:catalog";
/// Maximum depth of successive delegations.
const MAX_DELEGATION_DEPTH: usize = 16;
/// Maximum number of catalog files loaded (roots, `nextCatalog` and
/// delegation targets), so that a runaway catalog chain stays bounded.
const MAX_CATALOG_FILES: usize = 64;
/// Name of the catalog detected at the root of the workspace folders
/// (`xml.catalogsAutoDetect`).
pub const AUTO_DETECTED_CATALOG: &str = "catalog.xml";

/// Catalog entry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    System,
    Public,
    Uri,
    RewriteSystem,
    RewriteUri,
    SystemSuffix,
    UriSuffix,
    DelegatePublic,
    DelegateSystem,
    DelegateUri,
    NextCatalog,
}

impl EntryKind {
    /// Kind, key attribute and target attribute of a catalog element.
    fn from_element(local: &str) -> Option<(Self, Option<&'static str>, &'static str)> {
        Some(match local {
            "system" => (Self::System, Some("systemId"), "uri"),
            "public" => (Self::Public, Some("publicId"), "uri"),
            "uri" => (Self::Uri, Some("name"), "uri"),
            "rewriteSystem" => (
                Self::RewriteSystem,
                Some("systemIdStartString"),
                "rewritePrefix",
            ),
            "rewriteURI" => (Self::RewriteUri, Some("uriStartString"), "rewritePrefix"),
            "systemSuffix" => (Self::SystemSuffix, Some("systemIdSuffix"), "uri"),
            "uriSuffix" => (Self::UriSuffix, Some("uriSuffix"), "uri"),
            "delegatePublic" => (Self::DelegatePublic, Some("publicIdStartString"), "catalog"),
            "delegateSystem" => (Self::DelegateSystem, Some("systemIdStartString"), "catalog"),
            "delegateURI" => (Self::DelegateUri, Some("uriStartString"), "catalog"),
            "nextCatalog" => (Self::NextCatalog, None, "catalog"),
            _ => return None,
        })
    }

    /// The target is another catalog.
    pub fn targets_catalog(self) -> bool {
        matches!(
            self,
            Self::DelegatePublic | Self::DelegateSystem | Self::DelegateUri | Self::NextCatalog
        )
    }

    fn is_public(self) -> bool {
        matches!(self, Self::Public | Self::DelegatePublic)
    }
}

/// Catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub kind: EntryKind,
    /// Compared identifier, prefix or suffix, normalized (empty for
    /// `nextCatalog`).
    pub key: String,
    /// Absolute URI of the target (file, rewrite prefix or catalog),
    /// resolved against `xml:base`.
    pub target: String,
    /// `prefer="public"` applies to the entry.
    pub prefer_public: bool,
    /// Range of the target attribute value in the source.
    pub target_range: Range<usize>,
}

/// Parsed catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
}

/// The document is an OASIS catalog (`catalog` root element in the
/// catalog namespace).
pub fn is_catalog(source: &str) -> bool {
    // Quick check before parsing.
    source.contains(CATALOG_NAMESPACE) && parse_catalog(source, "file:///").is_ok()
}

/// Parses a catalog; `base_uri` is the URI of the catalog file.
/// Tolerant: a malformed catalog keeps the recognizable entries.
pub fn parse_catalog(source: &str, base_uri: &str) -> Result<Catalog, String> {
    let tree = XmlTagTree::parse(source);
    let elements = tree.elements();
    let attributes: Vec<Vec<XmlAttribute>> = elements
        .iter()
        .map(|element| scan_attributes(source, &element.start_tag))
        .collect();
    let namespace_of = |index: usize| -> Option<&str> {
        let (prefix, _) = qualified_name_parts(source, elements[index].start_tag.name.clone());
        resolve_namespace(
            source,
            &tree,
            &attributes,
            index,
            prefix.map(|range| &source[range]),
        )
        .flatten()
    };
    let local_name = |index: usize| -> &str {
        let (_, local) = qualified_name_parts(source, elements[index].start_tag.name.clone());
        &source[local]
    };
    let attribute = |index: usize, name: &str| -> Option<(String, Range<usize>)> {
        attributes[index]
            .iter()
            .find(|attribute| attribute.name(source) == name)
            .and_then(|attribute| {
                let range = attribute.value.clone()?;
                Some((unescape(&source[range.clone()]), range))
            })
    };

    match elements.iter().position(|element| element.parent.is_none()) {
        Some(root)
            if local_name(root) == "catalog" && namespace_of(root) == Some(CATALOG_NAMESPACE) => {}
        _ => return Err("the document is not an OASIS XML catalog".to_owned()),
    }

    // Base and preference of each element (`None`: element ignored).
    let mut contexts: Vec<Option<(String, bool)>> = Vec::with_capacity(elements.len());
    let mut catalog = Catalog::default();
    for (index, element) in elements.iter().enumerate() {
        let inherited = match element.parent {
            Some(parent) => contexts[parent].clone(),
            // Only the first root counts; content after it is ignored.
            None if index == 0 => Some((base_uri.to_owned(), true)),
            None => None,
        };
        let context = inherited.filter(|_| namespace_of(index) == Some(CATALOG_NAMESPACE));
        let Some((mut base, mut prefer_public)) = context else {
            contexts.push(None);
            continue;
        };
        if let Some((value, _)) = attribute(index, "xml:base") {
            base = resolve_uri_reference(&base, &value);
        }
        let local = local_name(index);
        if matches!(local, "catalog" | "group")
            && let Some((value, _)) = attribute(index, "prefer")
        {
            match value.trim() {
                "public" => prefer_public = true,
                "system" => prefer_public = false,
                _ => {}
            }
        }
        if let Some((kind, key_attribute, target_attribute)) = EntryKind::from_element(local)
            && let Some((target, target_range)) = attribute(index, target_attribute)
        {
            let key = match key_attribute {
                Some(name) => attribute(index, name).map(|(key, _)| {
                    if kind.is_public() {
                        normalize_public_id(&key)
                    } else {
                        normalize_uri(&key)
                    }
                }),
                None => Some(String::new()),
            };
            if let Some(key) = key.filter(|key| !key.is_empty() || kind == EntryKind::NextCatalog)
                && !target.trim().is_empty()
            {
                catalog.entries.push(CatalogEntry {
                    kind,
                    key,
                    target: resolve_uri_reference(&base, &target),
                    prefer_public,
                    target_range,
                });
            }
        }
        contexts.push(Some((base, prefer_public)));
    }
    Ok(catalog)
}

/// Normalizes a public identifier (surrounding whitespace removed, runs of
/// whitespace collapsed to one space).
pub fn normalize_public_id(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalizes a system identifier or a URI (OASIS § 6.3): encodes the
/// characters not allowed in a URI (spaces, non-ASCII…).
pub fn normalize_uri(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for byte in value.trim().bytes() {
        if byte <= 0x20
            || byte >= 0x7f
            || matches!(
                byte,
                b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
            )
        {
            normalized.push_str(&format!("%{byte:02X}"));
        } else {
            normalized.push(byte as char);
        }
    }
    normalized
}

/// Decodes a `urn:publicid:` URN into a public identifier (OASIS § 6.4).
pub fn unwrap_public_id_urn(value: &str) -> Option<String> {
    let rest = value
        .get(..13)
        .filter(|prefix| prefix.eq_ignore_ascii_case("urn:publicid:"))
        .map(|_| &value[13..])?;
    let mut result = String::with_capacity(rest.len());
    let mut chars = rest.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        match c {
            '+' => result.push(' '),
            ':' => result.push_str("//"),
            ';' => result.push_str("::"),
            '%' => {
                let code = rest.get(index + 1..index + 3).unwrap_or_default();
                let decoded = match code.to_ascii_uppercase().as_str() {
                    "2B" => Some('+'),
                    "3A" => Some(':'),
                    "2F" => Some('/'),
                    "3B" => Some(';'),
                    "27" => Some('\''),
                    "3F" => Some('?'),
                    "23" => Some('#'),
                    "25" => Some('%'),
                    _ => None,
                };
                match decoded {
                    Some(decoded) => {
                        result.push(decoded);
                        chars.next();
                        chars.next();
                    }
                    None => result.push('%'),
                }
            }
            c => result.push(c),
        }
    }
    Some(normalize_public_id(&result))
}

/// URI scheme (at least two characters, so that a Windows drive letter is
/// not mistaken for one).
fn uri_scheme(value: &str) -> Option<&str> {
    let (scheme, _) = value.split_once(':')?;
    let mut chars = scheme.chars();
    (scheme.len() >= 2
        && chars.next()?.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

/// Resolves the reference `reference` against the absolute URI `base` (RFC
/// 3986, simplified: no query or fragment in the base).
pub fn resolve_uri_reference(base: &str, reference: &str) -> String {
    let reference = reference.trim();
    if uri_scheme(reference).is_some() {
        return reference.to_owned();
    }
    // Absolute Windows path (`C:\…`, `C:/…`).
    if reference.as_bytes().get(1) == Some(&b':') && reference.as_bytes()[0].is_ascii_alphabetic() {
        return path_to_uri(Path::new(reference));
    }
    let reference = reference.replace('\\', "/");
    let base = base.split(['#', '?']).next().unwrap_or(base);
    let Some(scheme_end) = base.find(':') else {
        return reference;
    };
    let (scheme, rest) = (&base[..=scheme_end], &base[scheme_end + 1..]);
    let (authority, path) = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find('/').unwrap_or(after.len());
            (&rest[..end + 2], &after[end..])
        }
        None => ("", rest),
    };
    if reference.starts_with("//") {
        return format!("{scheme}{reference}");
    }
    let merged = if reference.starts_with('/') {
        reference
    } else if reference.is_empty() {
        path.to_owned()
    } else {
        let directory = path.rfind('/').map_or("", |index| &path[..=index]);
        format!("{directory}{reference}")
    };
    format!("{scheme}{authority}{}", remove_dot_segments(&merged))
}

fn remove_dot_segments(path: &str) -> String {
    let mut output: Vec<&str> = Vec::new();
    let segments = path.split('/').collect::<Vec<_>>();
    let last = segments.len().saturating_sub(1);
    for (index, segment) in segments.iter().enumerate() {
        match *segment {
            "." => {
                if index == last {
                    output.push("");
                }
            }
            ".." => {
                if output.len() > 1 {
                    output.pop();
                }
                if index == last {
                    output.push("");
                }
            }
            segment => output.push(segment),
        }
    }
    output.join("/")
}

/// Local file designated by a catalog URI (`file:` only).
pub fn target_path(target: &str) -> Option<PathBuf> {
    uri_scheme(target)
        .filter(|scheme| scheme.eq_ignore_ascii_case("file"))
        .map(|_| file_uri_to_path(target))
}

/// Paths of the configured catalogs (`xml.catalogs`): absolute path, `file:`
/// URI, `~/…` or a path relative to the first workspace folder containing
/// it (the first folder otherwise). With `auto_detect`, `catalog.xml` at
/// the root of each folder is added when it exists.
pub fn catalog_paths(configured: &[String], roots: &[PathBuf], auto_detect: bool) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for value in configured {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let path = if uri_scheme(value).is_some_and(|scheme| scheme.eq_ignore_ascii_case("file")) {
            file_uri_to_path(value)
        } else if let Some(rest) = value
            .strip_prefix("~/")
            .or_else(|| value.strip_prefix("~\\"))
        {
            match home_directory() {
                Some(home) => home.join(rest),
                None => continue,
            }
        } else if Path::new(value).is_absolute() {
            PathBuf::from(value)
        } else {
            match roots
                .iter()
                .map(|root| root.join(value))
                .find(|path| !is_network_path(path) && path.exists())
                .or_else(|| roots.first().map(|root| root.join(value)))
            {
                Some(path) => path,
                None => continue,
            }
        };
        let path = normalize_path(&path);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    if auto_detect {
        for root in roots {
            let path = root.join(AUTO_DETECTED_CATALOG);
            if !paths.contains(&path)
                && read_text_file(&path, MAX_RESOURCE_SIZE).is_ok_and(|source| is_catalog(&source))
            {
                paths.push(path);
            }
        }
    }
    paths
}

/// Removes the `.` and `..` components of a path.
fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    if is_network_path(path) {
        return None;
    }
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

#[derive(Debug, Clone)]
struct LoadedCatalog {
    stamp: Stamp,
    catalog: Result<Arc<Catalog>, String>,
}

/// Lookup in a catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Query {
    External {
        public: Option<String>,
        system: Option<String>,
    },
    Uri(String),
}

enum Lookup {
    Found(String),
    Delegate(Vec<PathBuf>),
    NotFound,
}

/// Configured catalogs and reachable catalogs (`nextCatalog`,
/// delegations), cached by modification time and size.
#[derive(Debug, Clone, Default)]
pub struct Catalogs {
    roots: Vec<PathBuf>,
    files: HashMap<PathBuf, LoadedCatalog>,
}

impl Catalogs {
    /// Catalogs built from `roots` (loaded immediately).
    #[cfg(test)]
    pub fn new(roots: Vec<PathBuf>) -> Self {
        let mut catalogs = Self::default();
        catalogs.set_roots(roots);
        catalogs
    }

    /// No catalog configured.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// All known catalog files (configured and reachable),
    /// sorted.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut files = self.files.keys().cloned().collect::<Vec<_>>();
        files.sort();
        files
    }

    /// `path` is one of the known catalogs.
    #[cfg(test)]
    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    /// Read or parse errors, by file.
    pub fn errors(&self) -> Vec<(PathBuf, String)> {
        let mut errors = self
            .files
            .iter()
            .filter_map(|(path, loaded)| {
                loaded
                    .catalog
                    .as_ref()
                    .err()
                    .map(|error| (path.clone(), error.clone()))
            })
            .collect::<Vec<_>>();
        errors.sort();
        errors
    }

    /// Replaces the top-level catalogs; returns `true` if resolution may
    /// have changed.
    pub fn set_roots(&mut self, roots: Vec<PathBuf>) -> bool {
        let changed = roots != self.roots;
        self.roots = roots;
        self.refresh() || changed
    }

    /// Rereads the catalogs modified on disk (time or size), loads the
    /// newly reachable catalogs and forgets the others. Returns `true` if a
    /// catalog changed.
    pub fn refresh(&mut self) -> bool {
        let mut changed = false;
        let mut reachable = HashSet::new();
        let mut queue = self.roots.iter().cloned().collect::<VecDeque<_>>();
        while let Some(path) = queue.pop_front() {
            if reachable.contains(&path) {
                continue;
            }
            if reachable.len() >= MAX_CATALOG_FILES {
                break;
            }
            reachable.insert(path.clone());
            let current = stamp(&path);
            let up_to_date = self
                .files
                .get(&path)
                .is_some_and(|loaded| loaded.stamp == current);
            if !up_to_date {
                let catalog = match read_text_file(&path, MAX_RESOURCE_SIZE) {
                    Ok(source) => parse_catalog(&source, &path_to_uri(&path)).map(Arc::new),
                    Err(error) => Err(format!("unreadable catalog: {error}")),
                };
                self.files.insert(
                    path.clone(),
                    LoadedCatalog {
                        stamp: current,
                        catalog,
                    },
                );
                changed = true;
            }
            if let Some(Ok(catalog)) = self.files.get(&path).map(|loaded| &loaded.catalog) {
                queue.extend(
                    catalog
                        .entries
                        .iter()
                        .filter(|entry| entry.kind.targets_catalog())
                        .filter_map(|entry| target_path(&entry.target)),
                );
            }
        }
        let known = self.files.len();
        self.files.retain(|path, _| reachable.contains(path));
        changed || known != self.files.len()
    }

    fn catalog(&self, path: &Path) -> Option<&Catalog> {
        self.files
            .get(path)
            .and_then(|loaded| loaded.catalog.as_deref().ok())
    }

    /// Resolves a URI (`uri`, `rewriteURI`, `uriSuffix`, `delegateURI`);
    /// returns the target URI.
    pub fn resolve_uri(&self, uri: &str) -> Option<String> {
        if self.is_empty() || uri.trim().is_empty() {
            return None;
        }
        let query = match unwrap_public_id_urn(uri.trim()) {
            Some(public) => Query::External {
                public: Some(public),
                system: None,
            },
            None => Query::Uri(normalize_uri(uri)),
        };
        self.resolve_in(&self.roots, &query, &mut HashSet::new(), 0)
    }

    /// Resolves a system identifier (DTD, entity, schema location).
    pub fn resolve_system(&self, system: &str) -> Option<String> {
        self.resolve_external(None, Some(system))
    }

    /// Resolves an external identifier (`PUBLIC "…" "…"` or `SYSTEM "…"`):
    /// system entries first, then public ones according to `prefer`.
    pub fn resolve_external(&self, public: Option<&str>, system: Option<&str>) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut public = public
            .map(|public| {
                unwrap_public_id_urn(public).unwrap_or_else(|| normalize_public_id(public))
            })
            .filter(|public| !public.is_empty());
        let mut system = system
            .map(normalize_uri)
            .filter(|system| !system.is_empty());
        if let Some(unwrapped) = system.as_deref().and_then(unwrap_public_id_urn) {
            system = None;
            if public.is_none() {
                public = Some(unwrapped);
            }
        }
        if public.is_none() && system.is_none() {
            return None;
        }
        self.resolve_in(
            &self.roots,
            &Query::External { public, system },
            &mut HashSet::new(),
            0,
        )
    }

    fn resolve_in(
        &self,
        files: &[PathBuf],
        query: &Query,
        visited: &mut HashSet<PathBuf>,
        depth: usize,
    ) -> Option<String> {
        for path in files {
            if !visited.insert(path.clone()) {
                continue;
            }
            let Some(catalog) = self.catalog(path) else {
                continue;
            };
            match lookup(catalog, query) {
                Lookup::Found(target) => return Some(target),
                Lookup::Delegate(catalogs) => {
                    if depth >= MAX_DELEGATION_DEPTH {
                        return None;
                    }
                    return self.resolve_in(&catalogs, query, &mut HashSet::new(), depth + 1);
                }
                Lookup::NotFound => {}
            }
            let next = catalog
                .entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::NextCatalog)
                .filter_map(|entry| target_path(&entry.target))
                .collect::<Vec<_>>();
            if let Some(target) = self.resolve_in(&next, query, visited, depth) {
                return Some(target);
            }
        }
        None
    }

    /// Any location (system identifier, then URI) to a local
    /// file.
    pub fn resolve_location(&self, location: &str) -> Option<PathBuf> {
        self.resolve_system(location)
            .and_then(|target| target_path(&target))
            .or_else(|| {
                self.resolve_uri(location)
                    .and_then(|target| target_path(&target))
            })
    }

    /// Schema location resolver for `xsd_core`: the namespace (`uri`
    /// entries) takes precedence, as in Xerces/LemMinX, then the location
    /// (system entries, then `uri`). `xs:include` is only resolved by its
    /// location.
    pub fn resolve_schema(&self, request: &SchemaLocation<'_>) -> Option<PathBuf> {
        if self.is_empty() {
            return None;
        }
        if request.kind != SchemaLocationKind::Include
            && let Some(path) = request
                .namespace
                .and_then(|namespace| self.resolve_uri(namespace))
                .and_then(|target| target_path(&target))
        {
            return Some(path);
        }
        self.resolve_location(request.location?)
    }
}

/// Entry of `entries` whose key (prefix or suffix according to `matches`)
/// is the longest; the first one in case of a tie.
fn longest<'e>(
    entries: impl Iterator<Item = &'e CatalogEntry>,
    matches: impl Fn(&str) -> bool,
) -> Option<&'e CatalogEntry> {
    entries
        .filter(|entry| matches(&entry.key))
        .fold(None, |best: Option<&CatalogEntry>, entry| match best {
            Some(best) if best.key.len() >= entry.key.len() => Some(best),
            _ => Some(entry),
        })
}

fn delegates<'e>(
    entries: impl Iterator<Item = &'e CatalogEntry>,
    matches: impl Fn(&str) -> bool,
) -> Option<Vec<PathBuf>> {
    let mut matching = entries
        .filter(|entry| matches(&entry.key))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return None;
    }
    // Stable sort: longest prefixes first, document order otherwise.
    matching.sort_by_key(|entry| std::cmp::Reverse(entry.key.len()));
    let mut paths = Vec::new();
    for path in matching
        .into_iter()
        .filter_map(|entry| target_path(&entry.target))
    {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    Some(paths)
}

fn lookup(catalog: &Catalog, query: &Query) -> Lookup {
    let of = |kind: EntryKind| {
        catalog
            .entries
            .iter()
            .filter(move |entry| entry.kind == kind)
    };
    match query {
        Query::External { public, system } => {
            if let Some(system) = system {
                if let Some(entry) = of(EntryKind::System).find(|entry| entry.key == *system) {
                    return Lookup::Found(entry.target.clone());
                }
                if let Some(entry) =
                    longest(of(EntryKind::RewriteSystem), |key| system.starts_with(key))
                {
                    return Lookup::Found(format!(
                        "{}{}",
                        entry.target,
                        &system[entry.key.len()..]
                    ));
                }
                if let Some(entry) =
                    longest(of(EntryKind::SystemSuffix), |key| system.ends_with(key))
                {
                    return Lookup::Found(entry.target.clone());
                }
                if let Some(catalogs) =
                    delegates(of(EntryKind::DelegateSystem), |key| system.starts_with(key))
                {
                    return Lookup::Delegate(catalogs);
                }
            }
            if let Some(public) = public {
                // With a system identifier, only public entries under
                // `prefer="public"` are considered.
                let eligible = |entry: &&CatalogEntry| system.is_none() || entry.prefer_public;
                if let Some(entry) = of(EntryKind::Public)
                    .filter(eligible)
                    .find(|entry| entry.key == *public)
                {
                    return Lookup::Found(entry.target.clone());
                }
                if let Some(catalogs) =
                    delegates(of(EntryKind::DelegatePublic).filter(eligible), |key| {
                        public.starts_with(key)
                    })
                {
                    return Lookup::Delegate(catalogs);
                }
            }
            Lookup::NotFound
        }
        Query::Uri(uri) => {
            if let Some(entry) = of(EntryKind::Uri).find(|entry| entry.key == *uri) {
                return Lookup::Found(entry.target.clone());
            }
            if let Some(entry) = longest(of(EntryKind::RewriteUri), |key| uri.starts_with(key)) {
                return Lookup::Found(format!("{}{}", entry.target, &uri[entry.key.len()..]));
            }
            if let Some(entry) = longest(of(EntryKind::UriSuffix), |key| uri.ends_with(key)) {
                return Lookup::Found(entry.target.clone());
            }
            if let Some(catalogs) =
                delegates(of(EntryKind::DelegateUri), |key| uri.starts_with(key))
            {
                return Lookup::Delegate(catalogs);
            }
            Lookup::NotFound
        }
    }
}

/// Problem of an open catalog: local target not found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogProblem {
    pub range: Range<usize>,
    pub message: String,
}

/// Missing local targets of the catalog `source` (empty if the document is
/// not a catalog). `document_uri` is the base of relative
/// paths.
pub fn catalog_problems(document_uri: &str, source: &str) -> Vec<CatalogProblem> {
    if !source.contains(CATALOG_NAMESPACE) {
        return Vec::new();
    }
    let Ok(catalog) = parse_catalog(source, document_uri) else {
        return Vec::new();
    };
    catalog
        .entries
        .iter()
        .filter_map(|entry| {
            let path = target_path(&entry.target)?;
            if is_network_path(&path) {
                return Some(CatalogProblem {
                    range: entry.target_range.clone(),
                    message: format!(
                        "Network path never accessed: {} (use a local copy)",
                        path.display()
                    ),
                });
            }
            let message = match entry.kind {
                EntryKind::RewriteSystem | EntryKind::RewriteUri => {
                    // Only a directory prefix (`…/`) can be checked.
                    if !entry.target.ends_with('/') || path.is_dir() {
                        return None;
                    }
                    format!("Directory not found: {}", path.display())
                }
                kind if kind.targets_catalog() => {
                    if path.is_file() {
                        return None;
                    }
                    format!("Catalog not found: {}", path.display())
                }
                _ => {
                    if path.is_file() {
                        return None;
                    }
                    format!("File not found: {}", path.display())
                }
            };
            Some(CatalogProblem {
                range: entry.target_range.clone(),
                message,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
