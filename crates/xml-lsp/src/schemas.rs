//! Loading of the XSD schema set of a document (flat [`XsdSchema`] model
//! used by validation and completion), with caches.
//!
//! - Each schema file is read, parsed and its `xs:include`/`xs:import`
//!   dependencies resolved once per version on disk (modification time and
//!   length): a request only `stat`s the files of the set. Parse errors are
//!   cached as well, so a broken schema is not parsed again on every
//!   keystroke.
//! - The merged schema of a set is cached by the versions of its files, so
//!   validation and completion do not clone and merge large schema sets
//!   (UBL, DITA) on each request.
//!
//! Dependencies are resolved through the XML catalogs: [`SchemaStore::clear`]
//! must be called when the catalogs change.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use quick_xml::{Reader, events::Event};
use xsd_core::{
    MAX_SCHEMA_DOCUMENTS, SchemaReference, XsdSchema, is_remote_location, merge_schemas, parse_xsd,
    resolve_schema_dependencies_with,
};

use crate::catalog;

/// Merged schema sets kept at most (one per distinct set of files).
const MAX_MERGED_SETS: usize = 16;

#[derive(Debug, Clone)]
pub(crate) struct SchemaLoadError {
    pub(crate) path: PathBuf,
    pub(crate) message: String,
    pub(crate) offset: usize,
    /// Remote schema (`http(s)`) that no catalog resolves: reported as a
    /// warning.
    pub(crate) remote: bool,
}

/// Version of a file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Stamp {
    modified: SystemTime,
    len: u64,
}

impl Stamp {
    fn of(path: &PathBuf) -> std::io::Result<Self> {
        let metadata = fs::metadata(path)?;
        Ok(Self {
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
            len: metadata.len(),
        })
    }
}

/// A schema file as last read.
struct CachedFile {
    stamp: Stamp,
    /// The parsed schema, or the parse error.
    schema: Result<Arc<XsdSchema>, SchemaLoadError>,
    /// `(namespace, path)` of the dependencies, or the resolution error.
    dependencies: Result<Vec<(Option<String>, PathBuf)>, SchemaLoadError>,
}

/// Schemas of a document: the merged model (`None` without any loadable
/// schema) and the loading errors.
pub(crate) struct LoadedSchemas {
    pub(crate) merged: Option<Arc<XsdSchema>>,
    pub(crate) errors: Vec<SchemaLoadError>,
}

#[derive(Default)]
pub(crate) struct SchemaStore {
    files: HashMap<PathBuf, CachedFile>,
    merged: HashMap<Vec<(PathBuf, Stamp)>, Arc<XsdSchema>>,
    /// Schemas loaded so far by target namespace.
    pub(crate) index: HashMap<String, Vec<PathBuf>>,
}

impl SchemaStore {
    /// Forgets everything (the catalogs changed).
    pub(crate) fn clear(&mut self) {
        self.files.clear();
        self.merged.clear();
    }

    /// Loads the schemas `references` and their dependencies.
    pub(crate) fn load(
        &mut self,
        references: Vec<SchemaReference>,
        catalogs: &catalog::Catalogs,
    ) -> LoadedSchemas {
        let mut queue = references
            .into_iter()
            .map(|reference| reference.path)
            .collect::<Vec<_>>();
        let mut visited = HashSet::new();
        let mut loaded = Vec::new();
        let mut errors = Vec::new();

        while let Some(path) = queue.pop() {
            if visited.contains(&path) {
                // Already loaded: `xs:include`/`xs:import` cycles end here.
                continue;
            }
            if visited.len() >= MAX_SCHEMA_DOCUMENTS {
                errors.push(SchemaLoadError {
                    message: format!(
                        "too many schema documents: only the first {MAX_SCHEMA_DOCUMENTS} are loaded ({} not loaded)",
                        path.display()
                    ),
                    path,
                    offset: 0,
                    remote: false,
                });
                break;
            }
            visited.insert(path.clone());
            if is_remote_location(&path) {
                errors.push(SchemaLoadError {
                    message: format!(
                        "unresolved remote schema: {} (map it to a local file with an XML catalog, xml.catalogs setting)",
                        path.display()
                    ),
                    path,
                    offset: 0,
                    remote: true,
                });
                continue;
            }
            let stamp = match Stamp::of(&path) {
                Ok(stamp) => stamp,
                Err(error) => {
                    errors.push(unreadable(&path, error));
                    continue;
                }
            };
            if self
                .files
                .get(&path)
                .is_none_or(|cached| cached.stamp != stamp)
            {
                match read_schema(&path, stamp, catalogs) {
                    Ok(file) => {
                        self.files.insert(path.clone(), file);
                    }
                    Err(error) => {
                        errors.push(error);
                        continue;
                    }
                }
            }
            let Some(file) = self.files.get(&path) else {
                continue;
            };
            let schema = match &file.schema {
                Ok(schema) => schema.clone(),
                Err(error) => {
                    errors.push(error.clone());
                    continue;
                }
            };
            // Component errors: the schema is invalid but still used.
            for problem in &schema.problems {
                errors.push(SchemaLoadError {
                    path: path.clone(),
                    message: format!("invalid XSD schema: {problem}"),
                    offset: 0,
                    remote: false,
                });
            }
            match &file.dependencies {
                Ok(dependencies) => {
                    queue.extend(dependencies.iter().map(|(_, path)| path.clone()));
                }
                Err(error) => errors.push(error.clone()),
            }
            if let Some(namespace) = &schema.target_namespace {
                let paths = self.index.entry(namespace.clone()).or_default();
                if !paths.contains(&path) {
                    paths.push(path.clone());
                }
            }
            loaded.push((path, stamp, schema));
        }

        if loaded.is_empty() {
            return LoadedSchemas {
                merged: None,
                errors,
            };
        }
        let key = loaded
            .iter()
            .map(|(path, stamp, _)| (path.clone(), *stamp))
            .collect::<Vec<_>>();
        let merged = match self.merged.get(&key) {
            Some(merged) => merged.clone(),
            None => {
                let merged = Arc::new(if loaded.len() == 1 {
                    loaded[0].2.as_ref().clone()
                } else {
                    merge_schemas(loaded.iter().map(|(_, _, schema)| schema.as_ref().clone()))
                });
                if self.merged.len() >= MAX_MERGED_SETS {
                    self.merged.clear();
                }
                self.merged.insert(key, merged.clone());
                merged
            }
        };
        LoadedSchemas {
            merged: Some(merged),
            errors,
        }
    }
}

fn unreadable(path: &Path, cause: impl std::fmt::Display) -> SchemaLoadError {
    SchemaLoadError {
        path: path.to_path_buf(),
        message: format!("cannot read the schema: {cause}"),
        offset: 0,
        remote: false,
    }
}

/// Reads and parses a schema file; `Err` when it cannot be read.
fn read_schema(
    path: &PathBuf,
    stamp: Stamp,
    catalogs: &catalog::Catalogs,
) -> Result<CachedFile, SchemaLoadError> {
    let source = xml_core::resource::read_text_file(path, xml_core::resource::MAX_RESOURCE_SIZE)
        .map_err(|error| unreadable(path, error))?;
    let schema = parse_xsd(&source)
        .map(Arc::new)
        .map_err(|error| SchemaLoadError {
            path: path.clone(),
            message: format!("invalid XSD schema: {error}"),
            offset: xsd_parse_error_offset(&source),
            remote: false,
        });
    let dependencies = resolve_schema_dependencies_with(&source, path, &|request| {
        catalogs.resolve_schema(request)
    })
    .map(|dependencies| {
        dependencies
            .into_iter()
            .map(|dependency| (dependency.namespace, dependency.path))
            .collect()
    })
    .map_err(|error| SchemaLoadError {
        path: path.clone(),
        message: format!("invalid XSD dependencies: {error}"),
        offset: xsd_parse_error_offset(&source),
        remote: false,
    });
    Ok(CachedFile {
        stamp,
        schema,
        dependencies,
    })
}

/// Offset of the first XML error of a schema (its length when none).
pub(crate) fn xsd_parse_error_offset(source: &str) -> usize {
    let mut reader = Reader::from_str(source);
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => return source.len(),
            Err(_) => return reader.buffer_position() as usize,
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
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
        // A modified file (other length) is read again.
        fs::write(
            &part,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="c"/></xs:schema>"#,
        )
        .unwrap();
        let third = store.load(reference(), &catalogs).merged.unwrap();
        assert_eq!(names(&third), ["a", "c"]);
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
}
