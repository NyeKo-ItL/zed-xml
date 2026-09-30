//! Catalogues XML OASIS 1.1 (réglage `xml.catalogs`, comme LemMinX).
//!
//! Un catalogue associe des identifiants (identifiant système ou public
//! d'une DTD, URI d'un schéma ou espace de noms) à des fichiers locaux, ce
//! qui permet de valider et compléter hors ligne un document dont
//! `xsi:schemaLocation` pointe vers une URL `http(s)`.
//!
//! Entrées reconnues (espace de noms
//! `urn:oasis:names:tc:entity:xmlns:xml:catalog`) : `system`, `public`,
//! `uri`, `rewriteSystem`, `rewriteURI`, `systemSuffix`, `uriSuffix`,
//! `delegatePublic`, `delegateSystem`, `delegateURI`, `nextCatalog`, dans
//! `catalog` et `group`, avec `xml:base` (sur tout élément) et `prefer`
//! (`public` par défaut, comme Xerces). Les chemins relatifs sont résolus
//! par rapport à `xml:base`, sinon au fichier catalogue. Les éléments d'un
//! autre espace de noms sont ignorés avec leur contenu.
//!
//! Ordre de résolution (spécification OASIS, § 7) dans chaque catalogue de
//! la liste : correspondance exacte, puis réécriture au plus long préfixe,
//! puis suffixe le plus long, puis délégation (nouvelle liste formée des
//! catalogues délégués, préfixes les plus longs d'abord) ; à défaut, les
//! `nextCatalog` du catalogue (en profondeur, protégés contre les cycles),
//! puis le catalogue suivant de la liste.
//!
//! Les catalogues sont relus lorsque leur date de modification ou leur
//! taille change (voir [`Catalogs::refresh`]).

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use xml_core::tags::{
    XmlAttribute, XmlTagTree, qualified_name_parts, resolve_namespace, scan_attributes,
};
use xsd_core::{SchemaLocation, SchemaLocationKind, file_uri_to_path};

use crate::{links::unescape, path_to_uri};

/// Espace de noms des catalogues XML OASIS.
pub const CATALOG_NAMESPACE: &str = "urn:oasis:names:tc:entity:xmlns:xml:catalog";
/// Profondeur maximale des délégations successives.
const MAX_DELEGATION_DEPTH: usize = 16;
/// Nom du catalogue détecté à la racine des dossiers de l'espace de travail
/// (`xml.catalogsAutoDetect`).
pub const AUTO_DETECTED_CATALOG: &str = "catalog.xml";

/// Type d'entrée de catalogue.
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
    /// Type, attribut clé et attribut cible d'un élément de catalogue.
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

    /// La cible est un autre catalogue.
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

/// Entrée d'un catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub kind: EntryKind,
    /// Identifiant, préfixe ou suffixe comparé, normalisé (vide pour
    /// `nextCatalog`).
    pub key: String,
    /// URI absolue de la cible (fichier, préfixe de réécriture ou
    /// catalogue), résolue contre `xml:base`.
    pub target: String,
    /// `prefer="public"` s'applique à l'entrée.
    pub prefer_public: bool,
    /// Étendue de la valeur de l'attribut cible dans la source.
    pub target_range: Range<usize>,
}

/// Catalogue analysé.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
}

/// Le document est un catalogue OASIS (élément racine `catalog` dans
/// l'espace de noms des catalogues).
pub fn is_catalog(source: &str) -> bool {
    // Test rapide avant l'analyse.
    source.contains(CATALOG_NAMESPACE) && parse_catalog(source, "file:///").is_ok()
}

/// Analyse un catalogue ; `base_uri` est l'URI du fichier catalogue.
/// Tolérant : un catalogue mal formé garde les entrées reconnaissables.
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
        _ => return Err("le document n'est pas un catalogue XML OASIS".to_owned()),
    }

    // Base et préférence de chaque élément (`None` : élément ignoré).
    let mut contexts: Vec<Option<(String, bool)>> = Vec::with_capacity(elements.len());
    let mut catalog = Catalog::default();
    for (index, element) in elements.iter().enumerate() {
        let inherited = match element.parent {
            Some(parent) => contexts[parent].clone(),
            // Seule la première racine compte ; le contenu après elle est ignoré.
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

/// Normalise un identifiant public (espaces de bord retirés, suites
/// d'espaces réduites à une espace).
pub fn normalize_public_id(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalise un identifiant système ou une URI (OASIS § 6.3) : encode les
/// caractères interdits dans une URI (espaces, non-ASCII…).
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

/// Décode un URN `urn:publicid:` en identifiant public (OASIS § 6.4).
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

/// Schéma d'URI (au moins deux caractères, pour ne pas confondre une lettre
/// de lecteur Windows).
fn uri_scheme(value: &str) -> Option<&str> {
    let (scheme, _) = value.split_once(':')?;
    let mut chars = scheme.chars();
    (scheme.len() >= 2
        && chars.next()?.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

/// Résout la référence `reference` contre l'URI absolue `base` (RFC 3986,
/// simplifiée : pas de requête ni de fragment dans la base).
pub fn resolve_uri_reference(base: &str, reference: &str) -> String {
    let reference = reference.trim();
    if uri_scheme(reference).is_some() {
        return reference.to_owned();
    }
    // Chemin Windows absolu (`C:\…`, `C:/…`).
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

/// Fichier local désigné par une URI de catalogue (`file:` uniquement).
pub fn target_path(target: &str) -> Option<PathBuf> {
    uri_scheme(target)
        .filter(|scheme| scheme.eq_ignore_ascii_case("file"))
        .map(|_| file_uri_to_path(target))
}

/// Chemins des catalogues configurés (`xml.catalogs`) : chemin absolu, URI
/// `file:`, `~/…` ou chemin relatif au premier dossier de l'espace de
/// travail qui le contient (au premier dossier sinon). Avec `auto_detect`,
/// `catalog.xml` à la racine de chaque dossier est ajouté s'il existe.
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
                .find(|path| path.exists())
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
                && fs::read_to_string(&path).is_ok_and(|source| is_catalog(&source))
            {
                paths.push(path);
            }
        }
    }
    paths
}

/// Retire les composants `.` et `..` d'un chemin.
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
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

#[derive(Debug, Clone)]
struct LoadedCatalog {
    stamp: Stamp,
    catalog: Result<Arc<Catalog>, String>,
}

/// Recherche dans un catalogue.
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

/// Catalogues configurés et catalogues atteignables (`nextCatalog`,
/// délégations), mis en cache par date de modification et taille.
#[derive(Debug, Clone, Default)]
pub struct Catalogs {
    roots: Vec<PathBuf>,
    files: HashMap<PathBuf, LoadedCatalog>,
}

impl Catalogs {
    /// Catalogues construits à partir de `roots` (chargés immédiatement).
    #[cfg(test)]
    pub fn new(roots: Vec<PathBuf>) -> Self {
        let mut catalogs = Self::default();
        catalogs.set_roots(roots);
        catalogs
    }

    /// Aucun catalogue configuré.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Tous les fichiers catalogues connus (configurés et atteignables),
    /// triés.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut files = self.files.keys().cloned().collect::<Vec<_>>();
        files.sort();
        files
    }

    /// `path` est l'un des catalogues connus.
    #[cfg(test)]
    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    /// Erreurs de lecture ou d'analyse, par fichier.
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

    /// Remplace les catalogues de premier niveau ; retourne `true` si la
    /// résolution peut avoir changé.
    pub fn set_roots(&mut self, roots: Vec<PathBuf>) -> bool {
        let changed = roots != self.roots;
        self.roots = roots;
        self.refresh() || changed
    }

    /// Relit les catalogues modifiés sur disque (date ou taille), charge les
    /// catalogues nouvellement atteignables et oublie les autres. Retourne
    /// `true` si un catalogue a changé.
    pub fn refresh(&mut self) -> bool {
        let mut changed = false;
        let mut reachable = HashSet::new();
        let mut queue = self.roots.iter().cloned().collect::<VecDeque<_>>();
        while let Some(path) = queue.pop_front() {
            if !reachable.insert(path.clone()) {
                continue;
            }
            let current = stamp(&path);
            let up_to_date = self
                .files
                .get(&path)
                .is_some_and(|loaded| loaded.stamp == current);
            if !up_to_date {
                let catalog = match fs::read_to_string(&path) {
                    Ok(source) => parse_catalog(&source, &path_to_uri(&path)).map(Arc::new),
                    Err(error) => Err(format!("catalogue illisible : {error}")),
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

    /// Résout une URI (`uri`, `rewriteURI`, `uriSuffix`, `delegateURI`) ;
    /// retourne l'URI cible.
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

    /// Résout un identifiant système (DTD, entité, emplacement de schéma).
    pub fn resolve_system(&self, system: &str) -> Option<String> {
        self.resolve_external(None, Some(system))
    }

    /// Résout un identifiant externe (`PUBLIC "…" "…"` ou `SYSTEM "…"`) :
    /// entrées système d'abord, puis publiques selon `prefer`.
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

    /// Emplacement quelconque (identifiant système, puis URI) vers un
    /// fichier local.
    pub fn resolve_location(&self, location: &str) -> Option<PathBuf> {
        self.resolve_system(location)
            .and_then(|target| target_path(&target))
            .or_else(|| {
                self.resolve_uri(location)
                    .and_then(|target| target_path(&target))
            })
    }

    /// Résolveur d'emplacements de schéma pour `xsd_core` : l'espace de
    /// noms (entrées `uri`) est prioritaire, comme dans Xerces/LemMinX, puis
    /// l'emplacement (entrées système, puis `uri`). `xs:include` n'est
    /// résolu que par son emplacement.
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

/// Entrée de `entries` dont la clé (préfixe ou suffixe selon `matches`) est
/// la plus longue ; la première en cas d'égalité.
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
    // Tri stable : préfixes les plus longs d'abord, ordre du document sinon.
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
                // Avec un identifiant système, seules les entrées publiques
                // sous `prefer="public"` sont considérées.
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

/// Problème d'un catalogue ouvert : cible locale introuvable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogProblem {
    pub range: Range<usize>,
    pub message: String,
}

/// Cibles locales introuvables du catalogue `source` (vide si le document
/// n'est pas un catalogue). `document_uri` sert de base aux chemins
/// relatifs.
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
            let message = match entry.kind {
                EntryKind::RewriteSystem | EntryKind::RewriteUri => {
                    // Seul un préfixe de dossier (`…/`) est vérifiable.
                    if !entry.target.ends_with('/') || path.is_dir() {
                        return None;
                    }
                    format!("Dossier introuvable : {}", path.display())
                }
                kind if kind.targets_catalog() => {
                    if path.is_file() {
                        return None;
                    }
                    format!("Catalogue introuvable : {}", path.display())
                }
                _ => {
                    if path.is_file() {
                        return None;
                    }
                    format!("Fichier introuvable : {}", path.display())
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
mod tests {
    use super::*;

    const HEADER: &str = r#"<catalog xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog""#;

    fn catalog(body: &str) -> String {
        format!("{HEADER}>\n{body}\n</catalog>")
    }

    /// Dossier temporaire propre au test.
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
        // Préfixe explicite.
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
        // Réécriture : le préfixe le plus long l'emporte, puis le premier.
        assert_eq!(
            catalogs.resolve_system("http://x/a/b/c.dtd"),
            Some(at("long/b/c.dtd"))
        );
        assert_eq!(
            catalogs.resolve_system("http://x/z.dtd"),
            Some(at("short/z.dtd"))
        );
        // Suffixes (après les réécritures) : le plus long l'emporte.
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
        // Identifiants publics et `prefer`.
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
        // Le système l'emporte sur le public.
        assert_eq!(
            catalogs.resolve_external(Some("-//P//EN"), Some("http://x/a/b/exact.dtd")),
            Some(at("exact.dtd"))
        );
        // URN publicid.
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
        // Cycle first -> second -> first : terminaison.
        assert_eq!(catalogs.resolve_uri("urn:none"), None);
        // Délégation : nouvelle liste, préfixe le plus long d'abord.
        assert_eq!(
            catalogs.resolve_uri("http://d/long/x"),
            Some(format!("{base}/from-long.xsd"))
        );
        assert_eq!(
            catalogs.resolve_uri("http://d/long/y"),
            Some(format!("{base}/from-short-y.xsd"))
        );
        // Délégations circulaires : profondeur bornée.
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
        // Cible distante : non résolue localement.
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
        let configured = vec![
            "cat/extra.xml".to_owned(),
            "missing/./c.xml".to_owned(),
            path_to_uri(&root.join("u r i.xml")),
            "/abs/c.xml".to_owned(),
            " ".to_owned(),
        ];
        assert_eq!(
            catalog_paths(&configured, &roots, false),
            vec![
                other.join("cat/extra.xml"),
                root.join("missing/c.xml"),
                root.join("u r i.xml"),
                PathBuf::from("/abs/c.xml"),
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
        assert!(problems[0].message.starts_with("Fichier introuvable"));
        assert!(problems[1].message.starts_with("Dossier introuvable"));
        assert!(problems[2].message.starts_with("Catalogue introuvable"));
        assert!(catalog_problems(&uri, "<root/>").is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
