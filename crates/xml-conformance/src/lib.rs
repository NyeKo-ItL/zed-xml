//! Shared helpers for the conformance, corpus and real-world fixture tests.
//!
//! The tests in `tests/` run `xml-core` and `xsd-core` against three kinds of
//! input:
//!
//! - fixtures vendored in `tests/fixtures/` (real-world documents and small
//!   third-party corpora, see `tests/fixtures/SOURCES.md`);
//! - large external suites fetched by `scripts/fetch-test-suites.sh` (W3C XML
//!   conformance suite, W3C XSD test suite, libxml2 schema tests). These tests
//!   are skipped when the suite is absent, unless `XML_TEST_SUITES_REQUIRED`
//!   is set, as it is in CI;
//! - hand-written cases derived from the specifications.
//!
//! Every case of every suite must pass. The only escape hatch is
//! `crates/xml-conformance/exclusions/<suite>.txt`, where a case that tests
//! something deliberately out of scope is listed with the reason.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    env, fmt, fs,
    path::{Path, PathBuf},
};

use xml_core::{format_xml, parse_xml};
use xsd_core::{
    LocatedXsdDiagnostic, XsdSchema, merge_schemas, parse_xsd, resolve_schema_dependencies,
    validate_document_located,
};

/// Root of the repository.
pub fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate lives in crates/xml-conformance")
        .to_path_buf()
}

/// Directory of the vendored fixtures (`tests/fixtures`).
pub fn fixtures_dir() -> PathBuf {
    repository_root().join("tests").join("fixtures")
}

/// Directory holding the fetched external suites: `XML_TEST_SUITES_DIR`, or
/// `target/test-suites` by default (the location used by
/// `scripts/fetch-test-suites.sh`).
pub fn suites_dir() -> PathBuf {
    env::var_os("XML_TEST_SUITES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository_root().join("target").join("test-suites"))
}

/// Locates an external suite, or explains why the test is skipped. Panics
/// when the suite is missing and `XML_TEST_SUITES_REQUIRED` is set.
pub fn external_suite(name: &str, marker: &str) -> Option<PathBuf> {
    let directory = suites_dir().join(name);
    if directory.join(marker).exists() {
        return Some(directory);
    }
    let message = format!(
        "external suite `{name}` not found in {} (run scripts/fetch-test-suites.sh)",
        directory.display()
    );
    if env::var_os("XML_TEST_SUITES_REQUIRED").is_some() {
        panic!("{message}");
    }
    eprintln!("skipped: {message}");
    None
}

/// Decodes a document as an editor would before handing it to the server:
/// byte order mark first, then the `encoding` of the XML declaration, UTF-8
/// otherwise (XML 1.0 appendix F, RFC 7303 section 3). Returns `None` when
/// the bytes are not valid in the detected encoding.
pub fn decode(bytes: &[u8]) -> Option<String> {
    if let Some((encoding, bom_length)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, had_errors) = encoding.decode_without_bom_handling(&bytes[bom_length..]);
        let kind = if encoding == encoding_rs::UTF_16LE {
            xml_core::text::TextEncoding::Utf16Le
        } else if encoding == encoding_rs::UTF_16BE {
            xml_core::text::TextEncoding::Utf16Be
        } else {
            xml_core::text::TextEncoding::Utf8
        };
        return (!had_errors && !xml_core::text::declared_encoding_conflicts(&text, kind, true))
            .then(|| text.into_owned());
    }
    // UTF-16 without a byte order mark, recognised from `<?` (appendix F.1).
    let utf16 = match bytes {
        [0x00, b'<', 0x00, b'?', ..] => Some(encoding_rs::UTF_16BE),
        [b'<', 0x00, b'?', 0x00, ..] => Some(encoding_rs::UTF_16LE),
        _ => None,
    };
    let encoding = utf16
        .or_else(|| declared_encoding(bytes))
        .unwrap_or(encoding_rs::UTF_8);
    let (text, had_errors) = encoding.decode_without_bom_handling(bytes);
    (!had_errors).then(|| text.into_owned())
}

fn declared_encoding(bytes: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let head = &bytes[..bytes.len().min(200)];
    if !head.starts_with(b"<?xml") {
        return None;
    }
    let end = head.windows(2).position(|pair| pair == b"?>")?;
    let declaration = std::str::from_utf8(&head[..end]).ok()?;
    let value = declaration.split("encoding").nth(1)?;
    let value = value.trim_start().strip_prefix('=')?.trim_start();
    let quote = value.chars().next()?;
    let label = value[1..].split(quote).next()?;
    encoding_rs::Encoding::for_label(label.as_bytes())
}

/// Well-formedness verdict of the language server for a document without
/// files around it: the diagnostics of `xml-core` (`parse_xml` includes the
/// tolerant checks of `wellformed::check_well_formedness` and the strict
/// grammar check), the DTD grammar errors of its internal subset and its
/// entity reference errors.
pub fn well_formedness_errors(source: &str) -> Vec<String> {
    server_errors(source, None, false)
}

/// Diagnostics of `xml-core` alone.
pub fn xml_core_errors(source: &str) -> Vec<String> {
    parse_xml(source)
        .diagnostics
        .into_iter()
        .map(|diagnostic| {
            format!(
                "{}@{}: {}",
                diagnostic.code(),
                diagnostic.offset,
                diagnostic.message
            )
        })
        .collect()
}

/// Reads the external subset and parameter entities next to the document,
/// like the language server does for local files.
struct FileLoader;

impl dtd_core::ExternalLoader for FileLoader {
    fn load(
        &mut self,
        _public: Option<&str>,
        system: &str,
        base: Option<&Path>,
    ) -> Result<(PathBuf, String), dtd_core::LoadError> {
        let failure = |message: String| dtd_core::LoadError {
            message,
            remote: false,
        };
        let path = base
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from(system), |directory| directory.join(system));
        let bytes = fs::read(&path).map_err(|error| failure(error.to_string()))?;
        let text = decode(&bytes).ok_or_else(|| failure("undecodable".to_owned()))?;
        Ok((path, text))
    }
}

/// Everything the language server reports about the well-formedness of the
/// document at `path` (`None`: no file, nothing external is read): the diagnostics of `xml-core`, the DTD grammar errors
/// (internal subset and readable external subset), the entity reference
/// errors and, with `validate`, the DTD validity errors.
pub fn server_errors(source: &str, path: Option<&Path>, validate: bool) -> Vec<String> {
    let mut errors = xml_core_errors(source);
    let loaded = match path {
        Some(path) => {
            dtd_core::load_document_dtd(source, Some(path.to_path_buf()), &mut FileLoader)
        }
        None => dtd_core::load_document_dtd(source, None, &mut dtd_core::NoLoader),
    };
    let dtd = loaded.as_ref().map(|(_, dtd)| dtd);
    if let Some(dtd) = dtd {
        errors.extend(
            dtd.problems
                .iter()
                // Validity constraints and unread resources are not
                // well-formedness errors.
                .filter(|problem| {
                    !matches!(
                        problem.kind,
                        dtd_core::DtdProblemKind::ExternalLoad { .. }
                            | dtd_core::DtdProblemKind::DuplicateElement
                            | dtd_core::DtdProblemKind::DuplicateNotation
                            | dtd_core::DtdProblemKind::MultipleIdAttributes
                            | dtd_core::DtdProblemKind::IdAttributeDefault
                            | dtd_core::DtdProblemKind::InvalidDefaultValue
                            | dtd_core::DtdProblemKind::ProperNesting
                            | dtd_core::DtdProblemKind::UndeclaredNotation
                    )
                })
                .map(|problem| format!("dtd-grammar: {}", problem.message)),
        );
    }
    let incomplete = dtd.is_some_and(|dtd| dtd.incomplete || dtd.optional_declarations);
    errors.extend(
        dtd_core::check_entity_references(source, dtd)
            .into_iter()
            // Without the whole DTD an entity may be declared elsewhere.
            .filter(|problem| {
                !(incomplete
                    && matches!(
                        problem.kind,
                        dtd_core::InstanceProblemKind::UndefinedEntity { .. }
                    ))
            })
            .map(|problem| format!("xml-entity: {}", problem.message)),
    );
    if validate
        && let Some(dtd) = dtd
        && !dtd.incomplete
    {
        errors.extend(
            dtd_core::validate_instance(source, dtd)
                .into_iter()
                .map(|problem| format!("dtd-validation: {}", problem.message)),
        );
    }
    errors
}

/// Structural fingerprint of a document, used to check that formatting
/// changes layout only: element names, attributes, comments, processing
/// instructions and non-whitespace text, with text whitespace collapsed.
pub fn content_fingerprint(source: &str) -> Result<Vec<String>, String> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..roxmltree::ParsingOptions::default()
    };
    let document = roxmltree::Document::parse_with_options(source, options)
        .map_err(|error| error.to_string())?;
    let mut fingerprint = Vec::new();
    let mut pending_text = String::new();
    for child in document.root().children() {
        fingerprint_node(child, &mut fingerprint, &mut pending_text);
    }
    flush_text(&mut fingerprint, &mut pending_text);
    Ok(fingerprint)
}

fn flush_text(fingerprint: &mut Vec<String>, pending_text: &mut String) {
    let text = pending_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !text.is_empty() {
        fingerprint.push(format!("text {text}"));
    }
    pending_text.clear();
}

fn fingerprint_node(
    node: roxmltree::Node<'_, '_>,
    fingerprint: &mut Vec<String>,
    pending_text: &mut String,
) {
    if node.is_text() {
        pending_text.push_str(node.text().unwrap_or_default());
        pending_text.push(' ');
        return;
    }
    flush_text(fingerprint, pending_text);
    match node.node_type() {
        roxmltree::NodeType::Element => {
            let mut attributes = node
                .attributes()
                .map(|attribute| {
                    format!(
                        "{{{}}}{}={:?}",
                        attribute.namespace().unwrap_or_default(),
                        attribute.name(),
                        attribute.value()
                    )
                })
                .collect::<Vec<_>>();
            attributes.sort();
            fingerprint.push(format!(
                "start {{{}}}{} {}",
                node.tag_name().namespace().unwrap_or_default(),
                node.tag_name().name(),
                attributes.join(" ")
            ));
            for child in node.children() {
                fingerprint_node(child, fingerprint, pending_text);
            }
            flush_text(fingerprint, pending_text);
            fingerprint.push("end".to_owned());
        }
        roxmltree::NodeType::Comment => {
            let comment = node.text().unwrap_or_default();
            fingerprint.push(format!(
                "comment {}",
                comment.split_whitespace().collect::<Vec<_>>().join(" ")
            ));
        }
        roxmltree::NodeType::PI => {
            let pi = node.pi().expect("processing instruction node");
            fingerprint.push(format!(
                "pi {} {}",
                pi.target,
                pi.value.unwrap_or_default().trim()
            ));
        }
        _ => {}
    }
}

/// Formats `source` twice and checks the formatter contract: formatting
/// succeeds on a well-formed document, is idempotent and keeps the content.
pub fn check_formatting(source: &str) -> Result<(), String> {
    let formatted = format_xml(source).map_err(|error| format!("formatting failed: {error}"))?;
    let reformatted =
        format_xml(&formatted).map_err(|error| format!("reformatting failed: {error}"))?;
    if reformatted != formatted {
        return Err(format!(
            "formatting is not idempotent:\n--- first pass\n{formatted}\n--- second pass\n{reformatted}"
        ));
    }
    // The content oracle (roxmltree) does not expand external or recursive
    // entities; without its view of the source only idempotence is checked.
    let Ok(before) = content_fingerprint(source) else {
        return Ok(());
    };
    let after = content_fingerprint(&formatted)
        .map_err(|error| format!("formatted output is not well-formed: {error}\n{formatted}"))?;
    if before != after {
        let first_difference = before
            .iter()
            .zip(&after)
            .position(|(left, right)| left != right)
            .unwrap_or(before.len().min(after.len()));
        return Err(format!(
            "formatting changed the content at item {first_difference}: {:?} became {:?}",
            before.get(first_difference),
            after.get(first_difference)
        ));
    }
    Ok(())
}

/// Loads a schema and every `xs:include`/`xs:import` it reaches on disk, the
/// way the language server builds its schema set.
pub fn load_schema_set(path: &Path) -> Result<XsdSchema, String> {
    let mut queue = vec![path.to_path_buf()];
    let mut visited = HashSet::new();
    let mut schemas = Vec::new();
    while let Some(path) = queue.pop() {
        let path = normalize(&path);
        if !visited.insert(path.clone()) {
            continue;
        }
        let bytes = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let source = decode(&bytes).ok_or_else(|| format!("{}: undecodable", path.display()))?;
        let schema = parse_xsd(&source).map_err(|error| format!("{}: {error}", path.display()))?;
        schemas.push(schema);
        let namespace_of = |dependency: &Path| {
            let bytes = fs::read(dependency).ok()?;
            xsd_core::schema_target_namespace(&decode(&bytes)?)
        };
        if let Some(problem) =
            xsd_core::dependency_problems(&source, &path, &|_| None, &namespace_of)
                .into_iter()
                .next()
        {
            return Err(format!("invalid schema: {}: {problem}", path.display()));
        }
        for reference in resolve_schema_dependencies(&source, &path)? {
            if reference.path.exists() {
                queue.push(reference.path);
            }
        }
    }
    schema_set(merge_schemas(schemas))
}

/// The merged schema set, or its component errors (an invalid schema).
pub fn schema_set(schema: XsdSchema) -> Result<XsdSchema, String> {
    match schema.problems.first() {
        Some(problem) => Err(format!("invalid schema: {problem}")),
        None => Ok(schema),
    }
}

/// Validates `instance` against the schema set rooted at `schema_path`.
pub fn validate_with_schema(
    instance: &str,
    schema_path: &Path,
) -> Result<Vec<LocatedXsdDiagnostic>, String> {
    let schema = load_schema_set(schema_path)?;
    Ok(validate_document_located(instance, &schema))
}

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}

/// Outcome of one case of an external suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    /// The implementation disagrees with the expected result.
    Fail(String),
    /// The case exercises something out of scope (reported, never compared).
    Skip(String),
}

/// Runs one case, turning a panic of the implementation into a failure.
pub fn guarded(case: impl FnOnce() -> Outcome) -> Outcome {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(case)) {
        Ok(outcome) => outcome,
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    payload
                        .downcast_ref::<&str>()
                        .map(|text| (*text).to_owned())
                })
                .unwrap_or_default();
            Outcome::Fail(format!("panicked: {message}"))
        }
    }
}

/// Results of a suite run, keyed by case identifier.
#[derive(Default)]
pub struct SuiteRun {
    pub name: String,
    pub outcomes: BTreeMap<String, Outcome>,
}

impl SuiteRun {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            outcomes: BTreeMap::new(),
        }
    }

    pub fn record(&mut self, id: impl Into<String>, outcome: Outcome) {
        let id = id.into();
        let previous = self.outcomes.insert(id.clone(), outcome);
        assert!(previous.is_none(), "duplicate case identifier {id}");
    }

    fn failures(&self) -> BTreeSet<&str> {
        self.outcomes
            .iter()
            .filter(|(_, outcome)| matches!(outcome, Outcome::Fail(_)))
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Checks the run against `exclusions/<name>.txt`: every failing case
    /// must be listed there with a reason (`<id><TAB><reason>`), and an
    /// excluded case that passes (or does not exist any more) must be removed
    /// from it. A suite without exclusions has no file: any failure fails the
    /// build.
    pub fn check(&self) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("exclusions")
            .join(format!("{}.txt", self.name));
        let failures = self.failures();
        eprintln!("{self}");

        let listing = fs::read_to_string(&path).unwrap_or_default();
        let mut excluded = BTreeMap::new();
        let mut problems = String::new();
        for line in listing.lines() {
            let line = line.trim_end();
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            match line.split_once('\t') {
                Some((id, reason)) if !reason.trim().is_empty() => {
                    excluded.insert(id.trim().to_owned(), reason.trim().to_owned());
                }
                _ => problems.push_str(&format!(
                    "{}: `{line}` has no reason (expected `<id><TAB><reason>`)\n",
                    path.display()
                )),
            }
        }
        let unexpected = failures
            .iter()
            .filter(|id| !excluded.contains_key(**id))
            .map(|id| format!("  {id}: {:?}", self.outcomes[*id]))
            .collect::<Vec<_>>();
        let stale = excluded
            .keys()
            .filter(|id| !failures.contains(id.as_str()))
            .collect::<Vec<_>>();
        if !unexpected.is_empty() {
            problems.push_str(&format!(
                "{} case(s) of `{}` fail:\n{}\n",
                unexpected.len(),
                self.name,
                unexpected.join("\n")
            ));
        }
        if !stale.is_empty() {
            problems.push_str(&format!(
                "{} excluded case(s) now pass or no longer exist; remove them from {}: {:?}\n",
                stale.len(),
                path.display(),
                stale
            ));
        }
        assert!(problems.is_empty(), "{problems}");
    }
}

impl fmt::Display for SuiteRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let passed = self
            .outcomes
            .values()
            .filter(|outcome| **outcome == Outcome::Pass)
            .count();
        let failed = self.failures().len();
        let skipped = self.outcomes.len() - passed - failed;
        let run = passed + failed;
        let rate = if run == 0 {
            0.0
        } else {
            passed as f64 * 100.0 / run as f64
        };
        write!(
            formatter,
            "{}: {passed}/{run} passed ({rate:.1}%), {failed} failed, {skipped} skipped",
            self.name
        )?;
        let mut reasons = BTreeMap::<String, usize>::new();
        for outcome in self.outcomes.values() {
            let (kind, reason) = match outcome {
                Outcome::Pass => continue,
                Outcome::Fail(reason) => ("fail", reason),
                Outcome::Skip(reason) => ("skip", reason),
            };
            let reason = reason.split(':').next().unwrap_or_default();
            *reasons.entry(format!("{kind}  {reason}")).or_default() += 1;
        }
        for (reason, count) in &reasons {
            write!(formatter, "\n  {count:>5}  {reason}")?;
        }
        Ok(())
    }
}
