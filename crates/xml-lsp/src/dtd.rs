//! DTD support, like LemMinX: grammar of the `<!DOCTYPE>` (internal subset
//! and external `SYSTEM`/`PUBLIC` DTD) and `.dtd`/`.ent` files.
//!
//! - Loading ([`load`]): the external DTD and external parameter entities
//!   are resolved through the XML catalogs (`xml.catalogs`), then relative
//!   to the declaring source; only local files are read (open buffers
//!   first, then disk with a cache by modification time), never an
//!   `http(s)` URL (warning pointing to `xml.catalogs`). Loading the
//!   external DTD is enabled by default,
//!   like in LemMinX.
//! - Diagnostics ([`diagnostics`]): DTD errors (`dtd-grammar`), undeclared
//!   or invalid entity references (`xml-entity`, also without a DTD),
//!   document validation (`dtd-validation`), in addition to XSD
//!   validation. `xml.validation.disallowDocTypeDecl` disables all of it
//!   for a document that has a `<!DOCTYPE>`; with
//!   `xml.validation.resolveExternalEntities`, referenced external general
//!   entities must be resolvable (otherwise they are never
//!   read).
//! - Completion ([`completions`]): elements allowed by the parent's content
//!   model, declared attributes and enumerated values (existing IDs for
//!   IDREF), entities after `&`; in a DTD: keywords after `<!`, `#…`,
//!   parameter entities after `%`, element names in declarations.
//! - Hover ([`hover`]) and definition ([`definition`]): DTD declaration of an
//!   element, an attribute, an entity (`&name;`, `%name;`) or a notation,
//!   with the comment preceding it as documentation.
//! - Fixes ([`code_actions`]): declare a missing entity, add a required
//!   attribute, replace a value outside the enumeration or a fixed value.
//! - Symbols ([`document_symbols`]) of a `.dtd` file.

use std::{
    collections::HashMap,
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use dtd_core::{
    AttributeType, Doctype, Dtd, DtdProblemKind, EntityValue, ExternalLoader, InstanceProblemKind,
    LoadError, Location, check_entity_references, entity_reference_at, find_doctype,
    general_entity_references, is_name_char, load_document_dtd, parse_dtd, validate_instance,
};
use serde_json::{Value, json};
use xml_core::{
    resource::{is_local_file, is_network_path, read_text_file},
    tags::{XmlTagKind, XmlTagTree, scan_attributes, scan_markup, scan_tags},
};
use xsd_core::{file_uri_to_path, percent_decode};

use crate::{
    catalog::{Catalogs, target_path},
    code_actions::{Actions, QUICK_FIX},
    links::uri_scheme,
    path_to_uri,
    selection::LineIndex,
    settings::ValidationSettings,
    uri_to_path,
};

/// Maximum size of a DTD file read from disk.
const MAX_DTD_SIZE: u64 = 4 * 1024 * 1024;
/// Code of the diagnostics of a DTD (syntax, loading).
pub(crate) const GRAMMAR_CODE: &str = "dtd-grammar";
/// Code of the document validation diagnostics against the DTD.
pub(crate) const VALIDATION_CODE: &str = "dtd-validation";
/// Code of the entity reference diagnostics.
pub(crate) const ENTITY_CODE: &str = "xml-entity";
/// Maximum number of values offered as replacements.
const MAX_VALUE_ACTIONS: usize = 20;

/// Texts of the DTDs read from disk, invalidated by modification time and
/// size.
pub(crate) type DtdCache = HashMap<PathBuf, (SystemTime, u64, Arc<str>)>;

/// DTD file (`.dtd`, `.ent`).
pub(crate) fn is_dtd_uri(uri: &str) -> bool {
    uri_to_path(uri).extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("dtd") || extension.eq_ignore_ascii_case("ent")
    })
}

/// DTD loading context.
pub(crate) struct DtdContext<'a> {
    /// Open documents (URI -> content), taking precedence over the disk.
    pub(crate) documents: &'a HashMap<String, String>,
    pub(crate) catalogs: &'a Catalogs,
    pub(crate) cache: &'a mut DtdCache,
}

/// Grammar of a document: `doctype` for an instance document, `None` for a
/// DTD file (source 0 in both cases).
pub(crate) struct Grammar {
    pub(crate) doctype: Option<Doctype>,
    pub(crate) dtd: Dtd,
}

fn document_path(uri: &str) -> Option<PathBuf> {
    uri.starts_with("file:").then(|| uri_to_path(uri))
}

struct Loader<'c, 'a> {
    context: &'c mut DtdContext<'a>,
}

impl ExternalLoader for Loader<'_, '_> {
    fn load(
        &mut self,
        public: Option<&str>,
        system: &str,
        base: Option<&Path>,
    ) -> Result<(PathBuf, String), LoadError> {
        let path = resolve_external(self.context, public, system, base)?;
        read_text(self.context, &path).map(|text| (path, text))
    }
}

fn not_found(path: &Path) -> LoadError {
    if is_network_path(path) {
        return LoadError {
            message: format!(
                "DTD '{}' not loaded: network paths are never accessed, map it to a local file with xml.catalogs",
                path.display()
            ),
            remote: true,
        };
    }
    LoadError {
        message: format!("DTD '{}' not found", path.display()),
        remote: false,
    }
}

/// Open document or local regular file; network paths are never touched.
fn is_available(context: &DtdContext<'_>, path: &Path) -> bool {
    context.documents.contains_key(&path_to_uri(path)) || is_local_file(path)
}

/// Resolves an external identifier: XML catalogs, then `file:` URI,
/// absolute path or path relative to `base`. A remote resource is never
/// downloaded.
fn resolve_external(
    context: &DtdContext<'_>,
    public: Option<&str>,
    system: &str,
    base: Option<&Path>,
) -> Result<PathBuf, LoadError> {
    let system = system.trim();
    if let Some(target) = context.catalogs.resolve_external(public, Some(system)) {
        return match target_path(&target) {
            Some(path) if is_available(context, &path) => Ok(path),
            Some(path) => Err(not_found(&path)),
            None => Err(LoadError {
                message: format!("the catalog target '{target}' is remote and is never downloaded"),
                remote: true,
            }),
        };
    }
    if let Some(scheme) = uri_scheme(system) {
        if scheme.eq_ignore_ascii_case("file") {
            let path = file_uri_to_path(system);
            return if is_available(context, &path) {
                Ok(path)
            } else {
                Err(not_found(&path))
            };
        }
        return Err(LoadError {
            message: format!(
                "DTD '{system}' not loaded: remote resources are never downloaded, map it to a local file with xml.catalogs"
            ),
            remote: true,
        });
    }
    let decoded = percent_decode(system);
    let candidate = Path::new(&decoded);
    let path = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        match base.and_then(Path::parent) {
            Some(directory) => directory.join(candidate),
            None => {
                return Err(LoadError {
                    message: format!(
                        "relative system identifier '{system}' not resolved: the document is not a local file"
                    ),
                    remote: false,
                });
            }
        }
    };
    if is_available(context, &path) {
        Ok(path)
    } else {
        Err(not_found(&path))
    }
}

fn read_text(context: &mut DtdContext<'_>, path: &Path) -> Result<String, LoadError> {
    if let Some(text) = context.documents.get(&path_to_uri(path)) {
        return Ok(text.clone());
    }
    let error = |message: String| LoadError {
        message,
        remote: false,
    };
    if is_network_path(path) {
        return Err(LoadError {
            message: format!(
                "DTD '{}' not loaded: network paths are never accessed",
                path.display()
            ),
            remote: true,
        });
    }
    let metadata = fs::metadata(path)
        .map_err(|cause| error(format!("DTD '{}' is unreadable: {cause}", path.display())))?;
    if metadata.len() > MAX_DTD_SIZE {
        return Err(error(format!(
            "DTD '{}' is too large (more than {MAX_DTD_SIZE} bytes)",
            path.display()
        )));
    }
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    if let Some((time, length, text)) = context.cache.get(path)
        && *time == modified
        && *length == metadata.len()
    {
        return Ok(text.to_string());
    }
    let text = read_text_file(path, MAX_DTD_SIZE)
        .map_err(|cause| error(format!("DTD '{}' is unreadable: {cause}", path.display())))?;
    context.cache.insert(
        path.to_path_buf(),
        (modified, metadata.len(), Arc::from(text.as_str())),
    );
    Ok(text)
}

/// Grammar of the document `uri`: DTD of its `<!DOCTYPE>` (`None` without a
/// DOCTYPE) or, for a `.dtd` file, the file itself.
pub(crate) fn load(context: &mut DtdContext<'_>, uri: &str, source: &str) -> Option<Grammar> {
    let path = document_path(uri);
    let mut loader = Loader { context };
    if is_dtd_uri(uri) {
        return Some(Grammar {
            doctype: None,
            dtd: parse_dtd(source, path, &mut loader),
        });
    }
    let (doctype, dtd) = load_document_dtd(source, path, &mut loader)?;
    Some(Grammar {
        doctype: Some(doctype),
        dtd,
    })
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

fn diagnostic(
    lines: &LineIndex,
    source: &str,
    range: &Range<usize>,
    severity: u8,
    (code, category, kind): (&str, &str, &str),
    message: String,
) -> Value {
    json!({
        "range": {
            "start": lines.position(source, range.start),
            "end": lines.position(source, range.end),
        },
        "severity": severity,
        "source": "xml-lsp",
        "code": code,
        "data": {"category": category, "kind": kind},
        "message": message,
    })
}

/// DTD diagnostics of the document `uri` (see the module).
pub(crate) fn diagnostics(
    context: &mut DtdContext<'_>,
    uri: &str,
    source: &str,
    validation: &ValidationSettings,
) -> Vec<Value> {
    let lines = LineIndex::new(source);
    let mut diagnostics = Vec::new();
    if is_dtd_uri(uri) {
        if let Some(grammar) = load(context, uri, source) {
            grammar_diagnostics(&grammar.dtd, source, &lines, &mut diagnostics);
        }
        return diagnostics;
    }
    if validation.disallow_doc_type_decl && find_doctype(source).is_some() {
        // The DOCTYPE itself is reported; its grammar is ignored.
        return diagnostics;
    }
    let grammar = load(context, uri, source);
    let dtd = grammar.as_ref().map(|grammar| &grammar.dtd);
    if let Some(dtd) = dtd {
        grammar_diagnostics(dtd, source, &lines, &mut diagnostics);
    }
    let incomplete = dtd.is_some_and(|dtd| dtd.incomplete);
    for problem in check_entity_references(source, dtd) {
        // Declaration possibly in a DTD that was not loaded: warning.
        let severity =
            if incomplete && matches!(problem.kind, InstanceProblemKind::UndefinedEntity { .. }) {
                2
            } else {
                1
            };
        diagnostics.push(diagnostic(
            &lines,
            source,
            &problem.range,
            severity,
            (ENTITY_CODE, "xml", problem.kind.id()),
            problem.message,
        ));
    }
    if let Some(dtd) = dtd {
        if validation.resolve_external_entities {
            external_entity_diagnostics(context, dtd, source, &lines, &mut diagnostics);
        }
        // Without the complete DTD, any declaration may be missing: no
        // validation (the loading failure is already reported).
        if !dtd.incomplete {
            for problem in validate_instance(source, dtd) {
                diagnostics.push(diagnostic(
                    &lines,
                    source,
                    &problem.range,
                    1,
                    (VALIDATION_CODE, "dtd", problem.kind.id()),
                    problem.message,
                ));
            }
        }
    }
    diagnostics
}

/// Problems of the grammar: those of the document (internal subset, `.dtd`
/// file) in place, those of an external file summarized on the reference
/// that loaded it.
fn grammar_diagnostics(dtd: &Dtd, source: &str, lines: &LineIndex, out: &mut Vec<Value>) {
    let mut external: Vec<(Range<usize>, u8, Vec<String>)> = Vec::new();
    for problem in &dtd.problems {
        let severity = match problem.kind {
            DtdProblemKind::ExternalLoad { remote: true } => 2,
            _ => 1,
        };
        let anchored = dtd.anchor(&problem.location);
        if anchored.source == 0 {
            out.push(diagnostic(
                lines,
                source,
                &anchored.range,
                severity,
                (GRAMMAR_CODE, "dtd", problem.kind.id()),
                problem.message.clone(),
            ));
            continue;
        }
        let Some(origin) = dtd.origin_in(&problem.location, 0) else {
            continue;
        };
        let file = dtd
            .source_path(anchored.source)
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let line = dtd.source_text(anchored.source)[..anchored
            .range
            .start
            .min(dtd.source_text(anchored.source).len())]
            .matches('\n')
            .count()
            + 1;
        let detail = format!("{} ({file}:{line})", problem.message);
        match external
            .iter_mut()
            .find(|(range, _, _)| *range == origin.range)
        {
            Some((_, existing, details)) => {
                *existing = (*existing).min(severity);
                details.push(detail);
            }
            None => external.push((origin.range, severity, vec![detail])),
        }
    }
    for (range, severity, details) in external {
        let message = if details.len() == 1 {
            format!("error in the DTD: {}", details[0])
        } else {
            format!(
                "{} errors in the DTD, including: {}",
                details.len(),
                details[0]
            )
        };
        out.push(diagnostic(
            lines,
            source,
            &range,
            severity,
            (GRAMMAR_CODE, "dtd", "externalGrammar"),
            message,
        ));
    }
}

/// `xml.validation.resolveExternalEntities`: references to external general
/// entities that cannot be found.
fn external_entity_diagnostics(
    context: &mut DtdContext<'_>,
    dtd: &Dtd,
    source: &str,
    lines: &LineIndex,
    out: &mut Vec<Value>,
) {
    for reference in general_entity_references(source) {
        let name = &source[reference.name.clone()];
        let Some(entity) = dtd.general_entity(name) else {
            continue;
        };
        let EntityValue::External {
            public,
            system,
            notation: None,
            base,
        } = &entity.value
        else {
            continue;
        };
        if let Err(error) = resolve_external(context, public.as_deref(), system, base.as_deref()) {
            out.push(diagnostic(
                lines,
                source,
                &reference.range,
                2,
                (ENTITY_CODE, "xml", "externalEntity"),
                format!("external entity '{name}' not resolved: {}", error.message),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Contextes
// Contexts

/// Range of the DTD text of the document: whole file or internal
/// subset.
fn dtd_region(grammar: Option<&Grammar>, uri: &str, source: &str) -> Option<Range<usize>> {
    if is_dtd_uri(uri) {
        return Some(0..source.len());
    }
    grammar?.doctype.as_ref()?.internal_subset.clone()
}

/// The cursor is in DTD text (`.dtd` file or internal subset of the
/// DOCTYPE).
pub(crate) fn in_dtd_text(
    grammar: Option<&Grammar>,
    uri: &str,
    source: &str,
    offset: usize,
) -> bool {
    dtd_region(grammar, uri, source)
        .is_some_and(|region| region.start <= offset && offset <= region.end)
}

/// Start of the name being typed before `offset`.
fn typed_start(source: &str, offset: usize) -> usize {
    source[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, character)| is_name_char(*character))
        .last()
        .map_or(offset, |(index, _)| index)
}

/// Name (run of `NameChar`s) containing `offset`.
fn word_at(source: &str, offset: usize) -> Option<Range<usize>> {
    let start = typed_start(source, offset);
    let end = source[offset..]
        .char_indices()
        .find(|(_, character)| !is_name_char(*character))
        .map_or(source.len(), |(index, _)| offset + index);
    (start < end).then_some(start..end)
}

pub(crate) fn floor_boundary(source: &str, offset: usize) -> usize {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// The cursor is inside a comment, a CDATA section, a processing
/// instruction or a declaration.
pub(crate) fn in_markup(source: &str, offset: usize) -> bool {
    scan_markup(source)
        .iter()
        .any(|markup| markup.range.start < offset && (offset < markup.range.end || !markup.closed))
}

/// Context in a start tag.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TagContext<'s> {
    ElementName,
    AttributeName {
        element: &'s str,
        present: Vec<&'s str>,
    },
    AttributeValue {
        element: &'s str,
        attribute: &'s str,
    },
}

/// Parses the tag start `fragment` (after `<`, up to the cursor).
pub(crate) fn tag_context(fragment: &str) -> Option<TagContext<'_>> {
    if fragment.starts_with(['/', '!', '?']) {
        return None;
    }
    let element_end = fragment
        .find(|character: char| character.is_whitespace())
        .unwrap_or(fragment.len());
    let element = &fragment[..element_end];
    if !element.chars().all(is_name_char) {
        return None;
    }
    if element_end == fragment.len() {
        return Some(TagContext::ElementName);
    }
    let mut present = Vec::new();
    let mut token_start: Option<usize> = None;
    let mut last_name: Option<&str> = None;
    let mut pending: Option<&str> = None;
    let mut quote: Option<(char, &str)> = None;
    for (index, character) in fragment
        .char_indices()
        .skip_while(|(index, _)| *index < element_end)
    {
        if let Some((open, _)) = quote {
            if character == open {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => {
                quote = Some((character, pending.take().unwrap_or_default()));
                token_start = None;
            }
            '=' => {
                if let Some(start) = token_start.take() {
                    last_name = Some(&fragment[start..index]);
                }
                pending = last_name.take();
                present.extend(pending);
            }
            character if character.is_whitespace() => {
                if let Some(start) = token_start.take() {
                    last_name = Some(&fragment[start..index]);
                }
            }
            _ => {
                if token_start.is_none() {
                    token_start = Some(index);
                    last_name = None;
                }
            }
        }
    }
    if let Some((_, attribute)) = quote {
        return Some(TagContext::AttributeValue { element, attribute });
    }
    if pending.is_some() {
        return None;
    }
    Some(TagContext::AttributeName { element, present })
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

fn item(
    label: &str,
    kind: u8,
    insert_text: String,
    snippet: bool,
    detail: Option<String>,
    documentation: Option<&str>,
) -> Value {
    let mut item = json!({
        "label": label,
        "kind": kind,
        "insertText": insert_text,
    });
    if snippet {
        item["insertTextFormat"] = json!(2);
    }
    if let Some(detail) = detail {
        item["detail"] = json!(detail);
    }
    if let Some(documentation) = documentation {
        item["documentation"] =
            json!({"kind": "markdown", "value": escape_markdown(documentation)});
    }
    item
}

const ELEMENT_KIND: u8 = 10;
const ATTRIBUTE_KIND: u8 = 5;
const VALUE_KIND: u8 = 12;
const ENTITY_KIND: u8 = 21;
const KEYWORD_KIND: u8 = 14;

fn element_declaration_text(dtd: &Dtd, name: &str) -> Option<String> {
    let declaration = dtd.element(name)?;
    Some(format!(
        "<!ELEMENT {} {}>",
        declaration.name, declaration.content
    ))
}

fn element_item(dtd: &Dtd, name: &str) -> Value {
    let documentation = dtd
        .element(name)
        .and_then(|declaration| declaration.documentation.as_deref());
    item(
        name,
        ELEMENT_KIND,
        name.to_owned(),
        false,
        element_declaration_text(dtd, name),
        documentation,
    )
}

/// General (and predefined) entities after `&`.
fn entity_items(dtd: Option<&Dtd>) -> Vec<Value> {
    let mut items = dtd_core::PREDEFINED_ENTITIES
        .iter()
        .map(|(name, value)| {
            item(
                name,
                ENTITY_KIND,
                format!("{name};"),
                false,
                Some(format!("&{name}; → {value}")),
                None,
            )
        })
        .collect::<Vec<_>>();
    for entity in dtd.into_iter().flat_map(|dtd| &dtd.general_entities) {
        if entity.expansion.unparsed
            || dtd_core::PREDEFINED_ENTITIES
                .iter()
                .any(|(name, _)| *name == entity.name)
        {
            continue;
        }
        items.push(item(
            &entity.name,
            ENTITY_KIND,
            format!("{};", entity.name),
            false,
            Some(entity.display()),
            entity.documentation.as_deref(),
        ));
    }
    items
}

/// DTD suggestions at the cursor `offset` of the document `uri`.
pub(crate) fn completions(
    grammar: Option<&Grammar>,
    uri: &str,
    source: &str,
    offset: usize,
) -> Vec<Value> {
    let offset = floor_boundary(source, offset);
    let dtd = grammar.map(|grammar| &grammar.dtd);
    if let Some(region) = dtd_region(grammar, uri, source)
        && region.start <= offset
        && offset <= region.end
    {
        return dtd_text_completions(dtd, source, region.start, offset);
    }
    if in_markup(source, offset) {
        return Vec::new();
    }
    let start = typed_start(source, offset);
    let prefix = &source[..start];
    if prefix.ends_with('&') {
        return entity_items(dtd);
    }
    let Some(grammar) = grammar else {
        return Vec::new();
    };
    let dtd = &grammar.dtd;
    let Some(opening) = prefix.rfind('<') else {
        return Vec::new();
    };
    if source[opening..offset].contains('>') {
        return Vec::new();
    }
    match tag_context(&source[opening + 1..offset]) {
        Some(TagContext::ElementName) => {
            let tree = XmlTagTree::parse(&source[..opening]);
            let elements = tree.elements();
            let parent = elements
                .iter()
                .enumerate()
                .rev()
                .find(|(_, element)| {
                    element.start_tag.kind == XmlTagKind::Start
                        && element.start_tag.closed
                        && element.end_tag.is_none()
                })
                .map(|(index, _)| index);
            let preceding = elements
                .iter()
                .filter(|element| element.parent == parent)
                .map(|element| element.name(source))
                .collect::<Vec<_>>();
            let parent_name = parent.map(|index| elements[index].name(source));
            dtd.allowed_children(parent_name, &preceding)
                .iter()
                .map(|name| element_item(dtd, name))
                .collect()
        }
        Some(TagContext::AttributeName { element, present }) => dtd
            .attributes_of(element)
            .filter(|attribute| !present.contains(&attribute.name.as_str()))
            .map(|attribute| {
                item(
                    &attribute.name,
                    ATTRIBUTE_KIND,
                    format!("{}=\"$1\"", attribute.name),
                    true,
                    Some(attribute.display()),
                    attribute.documentation.as_deref(),
                )
            })
            .collect(),
        Some(TagContext::AttributeValue { element, attribute }) => {
            let Some(declaration) = dtd.attribute(element, attribute) else {
                return Vec::new();
            };
            let mut values = declaration
                .attribute_type
                .values()
                .map(<[String]>::to_vec)
                .unwrap_or_default();
            if matches!(
                declaration.attribute_type,
                AttributeType::IdRef | AttributeType::IdRefs
            ) {
                values.extend(document_ids(dtd, source));
            }
            if let Some(value) = declaration.default.value()
                && !values.iter().any(|existing| existing == value)
            {
                values.push(value.to_owned());
            }
            values
                .iter()
                .map(|value| {
                    item(
                        value,
                        VALUE_KIND,
                        value.clone(),
                        false,
                        Some(declaration.display()),
                        None,
                    )
                })
                .collect()
        }
        None => Vec::new(),
    }
}

/// Values of the ID attributes of the document.
fn document_ids(dtd: &Dtd, source: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for tag in scan_tags(source) {
        if tag.kind == XmlTagKind::End {
            continue;
        }
        let element = tag.name(source);
        for attribute in scan_attributes(source, &tag) {
            let is_id = dtd
                .attribute(element, attribute.name(source))
                .is_some_and(|declaration| declaration.attribute_type == AttributeType::Id);
            if is_id
                && let Some(value) = attribute.value(source)
                && !value.is_empty()
                && !ids.iter().any(|id| id == value)
            {
                ids.push(value.to_owned());
            }
        }
    }
    ids
}

/// Suggestions in DTD text starting at `region_start`.
fn dtd_text_completions(
    dtd: Option<&Dtd>,
    source: &str,
    region_start: usize,
    offset: usize,
) -> Vec<Value> {
    let before = &source[region_start..offset];
    // In a comment or a literal: nothing.
    if before
        .rfind("<!--")
        .is_some_and(|start| !before[start..].contains("-->"))
    {
        return Vec::new();
    }
    let start = typed_start(source, offset).max(region_start);
    let preceding = &source[region_start..start];
    if preceding.ends_with('%') {
        return dtd
            .into_iter()
            .flat_map(|dtd| &dtd.parameter_entities)
            .map(|entity| {
                item(
                    &entity.name,
                    ENTITY_KIND,
                    format!("{};", entity.name),
                    false,
                    Some(entity.display()),
                    entity.documentation.as_deref(),
                )
            })
            .collect();
    }
    let declaration = preceding
        .rfind("<!")
        .filter(|&index| !preceding[index..].contains('>'))
        .map(|index| &preceding[index + 2..]);
    if declaration.is_some_and(|declaration| {
        let quotes = declaration.matches('"').count() + declaration.matches('\'').count();
        quotes % 2 == 1
    }) {
        if preceding.ends_with('&') {
            return entity_items(dtd);
        }
        return Vec::new();
    }
    if preceding.ends_with("<!") {
        return [
            ("ELEMENT", "ELEMENT ${1:name} ${2:(#PCDATA)}>"),
            (
                "ATTLIST",
                "ATTLIST ${1:element} ${2:attribute} ${3:CDATA} ${4:#IMPLIED}>",
            ),
            ("ENTITY", "ENTITY ${1:name} \"${2:value}\">"),
            ("NOTATION", "NOTATION ${1:name} SYSTEM \"${2:uri}\">"),
        ]
        .iter()
        .map(|(label, snippet)| item(label, KEYWORD_KIND, (*snippet).to_owned(), true, None, None))
        .collect();
    }
    let keywords = |words: &[&str]| {
        words
            .iter()
            .map(|word| item(word, KEYWORD_KIND, (*word).to_owned(), false, None, None))
            .collect::<Vec<_>>()
    };
    if preceding.ends_with('#') {
        return keywords(&["PCDATA", "REQUIRED", "IMPLIED", "FIXED"]);
    }
    let Some(declaration) = declaration else {
        return Vec::new();
    };
    let element_names = || {
        dtd.into_iter()
            .flat_map(|dtd| {
                dtd.elements
                    .iter()
                    .map(|element| element_item(dtd, &element.name))
            })
            .collect::<Vec<_>>()
    };
    let tokens = declaration.split_whitespace().collect::<Vec<_>>();
    let complete_tokens = if declaration.ends_with(char::is_whitespace) {
        tokens.len()
    } else {
        tokens.len().saturating_sub(1)
    };
    match tokens.first().copied() {
        Some("ELEMENT") if declaration.contains('(') => {
            let trimmed = preceding.trim_end();
            if trimmed.ends_with(['(', '|', ',']) {
                element_names()
            } else {
                Vec::new()
            }
        }
        Some("ELEMENT") if complete_tokens == 2 => keywords(&["EMPTY", "ANY"]),
        Some("ATTLIST") if complete_tokens == 1 => element_names(),
        Some("ATTLIST") if complete_tokens >= 3 && complete_tokens % 3 == 0 => keywords(&[
            "CDATA", "ID", "IDREF", "IDREFS", "ENTITY", "ENTITIES", "NMTOKEN", "NMTOKENS",
            "NOTATION",
        ]),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Hover and definition
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Target {
    Element(String),
    Attribute { element: String, name: String },
    Entity(String),
    ParameterEntity(String),
    Notation(String),
}

/// DTD construct under the cursor and its range.
fn target_at(
    grammar: &Grammar,
    uri: &str,
    source: &str,
    offset: usize,
) -> Option<(Target, Range<usize>)> {
    let offset = floor_boundary(source, offset);
    let dtd = &grammar.dtd;
    if let Some(region) = dtd_region(Some(grammar), uri, source)
        && region.start <= offset
        && offset <= region.end
    {
        if let Some(name) = entity_reference_at(source, offset, true) {
            return Some((
                Target::ParameterEntity(source[name.clone()].to_owned()),
                name,
            ));
        }
        if let Some(name) = entity_reference_at(source, offset, false) {
            return Some((Target::Entity(source[name.clone()].to_owned()), name));
        }
        let word = word_at(source, offset)?;
        let name = &source[word.clone()];
        let before = &source[region.start..word.start];
        let declaration = before
            .rfind("<!")
            .filter(|&index| !before[index..].contains('>'))
            .map(|index| &before[index + 2..]);
        let mut tokens = declaration.unwrap_or_default().split_whitespace();
        let keyword = tokens.next();
        if keyword == Some("ATTLIST")
            && let Some(element) = tokens.next()
            && dtd.attribute(element, name).is_some()
        {
            return Some((
                Target::Attribute {
                    element: element.to_owned(),
                    name: name.to_owned(),
                },
                word,
            ));
        }
        if keyword == Some("ENTITY") {
            let parameter = declaration.is_some_and(|text| {
                text.trim_start()["ENTITY".len()..]
                    .trim_start()
                    .starts_with('%')
            });
            if parameter && dtd.parameter_entity(name).is_some() {
                return Some((Target::ParameterEntity(name.to_owned()), word));
            }
            if !parameter && dtd.general_entity(name).is_some() {
                return Some((Target::Entity(name.to_owned()), word));
            }
        }
        if dtd.element(name).is_some() {
            return Some((Target::Element(name.to_owned()), word));
        }
        if dtd.notation(name).is_some() {
            return Some((Target::Notation(name.to_owned()), word));
        }
        return None;
    }
    if in_markup(source, offset) {
        return None;
    }
    if let Some(name) = entity_reference_at(source, offset, false)
        && general_entity_references(source)
            .iter()
            .any(|reference| reference.name == name)
    {
        return Some((Target::Entity(source[name.clone()].to_owned()), name));
    }
    let tag = scan_tags(source)
        .into_iter()
        .find(|tag| tag.range.start < offset && offset < tag.range.end.max(tag.range.start + 1))?;
    if tag.name_contains(offset) {
        let name = tag.name(source).to_owned();
        return dtd
            .element(&name)
            .is_some()
            .then(|| (Target::Element(name), tag.name.clone()));
    }
    if tag.kind == XmlTagKind::End {
        return None;
    }
    let element = tag.name(source);
    let attribute = scan_attributes(source, &tag)
        .into_iter()
        .find(|attribute| attribute.name.start <= offset && offset <= attribute.name.end)?;
    let name = attribute.name(source);
    dtd.attribute(element, name).is_some().then(|| {
        (
            Target::Attribute {
                element: element.to_owned(),
                name: name.to_owned(),
            },
            attribute.name.clone(),
        )
    })
}

/// Declaration of the target: text, documentation and location.
fn declaration_of(dtd: &Dtd, target: &Target) -> Option<(String, Option<String>, Location)> {
    Some(match target {
        Target::Element(name) => {
            let declaration = dtd.element(name)?;
            let mut text = format!("<!ELEMENT {} {}>", declaration.name, declaration.content);
            for attribute in dtd.attributes_of(name) {
                text.push('\n');
                text.push_str(&attribute.display());
            }
            (
                text,
                declaration.documentation.clone(),
                declaration.location.clone(),
            )
        }
        Target::Attribute { element, name } => {
            let declaration = dtd.attribute(element, name)?;
            (
                declaration.display(),
                declaration.documentation.clone(),
                declaration.location.clone(),
            )
        }
        Target::Entity(name) => {
            let declaration = dtd.general_entity(name)?;
            (
                declaration.display(),
                declaration.documentation.clone(),
                declaration.location.clone(),
            )
        }
        Target::ParameterEntity(name) => {
            let declaration = dtd.parameter_entity(name)?;
            (
                declaration.display(),
                declaration.documentation.clone(),
                declaration.location.clone(),
            )
        }
        Target::Notation(name) => {
            let declaration = dtd.notation(name)?;
            let external = match (&declaration.public, &declaration.system) {
                (Some(public), Some(system)) => format!("PUBLIC \"{public}\" \"{system}\""),
                (Some(public), None) => format!("PUBLIC \"{public}\""),
                (None, Some(system)) => format!("SYSTEM \"{system}\""),
                (None, None) => String::new(),
            };
            (
                format!("<!NOTATION {} {external}>", declaration.name),
                declaration.documentation.clone(),
                declaration.location.clone(),
            )
        }
    })
}

fn escape_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(
            character,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' | '~'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Actual location (URI, text, range) of a declaration.
fn resolve_location<'g>(
    grammar: &'g Grammar,
    uri: &str,
    source: &'g str,
    location: &Location,
) -> Option<(String, &'g str, Range<usize>)> {
    let anchored = grammar.dtd.anchor(location);
    if anchored.source == 0 {
        return Some((uri.to_owned(), source, anchored.range));
    }
    let path = grammar.dtd.source_path(anchored.source)?;
    Some((
        path_to_uri(path),
        grammar.dtd.source_text(anchored.source),
        anchored.range,
    ))
}

/// Answers `textDocument/hover` on a construct declared by the DTD.
pub(crate) fn hover(grammar: &Grammar, uri: &str, source: &str, offset: usize) -> Option<Value> {
    let (target, range) = target_at(grammar, uri, source, offset)?;
    let (text, documentation, location) = declaration_of(&grammar.dtd, &target)?;
    let mut sections = vec![format!("```xml\n{text}\n```")];
    if let Some(documentation) = documentation {
        sections.push(escape_markdown(&documentation));
    }
    if let Some((target_uri, _, _)) = resolve_location(grammar, uri, source, &location)
        && target_uri != uri
        && let Some(name) = uri_to_path(&target_uri).file_name()
    {
        sections.push(format!(
            "Source: [{}]({target_uri})",
            escape_markdown(&name.to_string_lossy())
        ));
    }
    let lines = LineIndex::new(source);
    Some(json!({
        "contents": {"kind": "markdown", "value": sections.join("\n\n")},
        "range": {
            "start": lines.position(source, range.start),
            "end": lines.position(source, range.end),
        },
    }))
}

/// Answers `textDocument/definition`: DTD declaration of the element,
/// attribute, entity or notation under the cursor.
pub(crate) fn definition(
    grammar: &Grammar,
    uri: &str,
    source: &str,
    offset: usize,
) -> Option<Value> {
    let (target, _) = target_at(grammar, uri, source, offset)?;
    let (_, _, location) = declaration_of(&grammar.dtd, &target)?;
    let (target_uri, text, range) = resolve_location(grammar, uri, source, &location)?;
    let lines = LineIndex::new(text);
    Some(json!([{
        "uri": target_uri,
        "range": {
            "start": lines.position(text, range.start),
            "end": lines.position(text, range.end),
        },
    }]))
}

// ---------------------------------------------------------------------------
// Fixes
// ---------------------------------------------------------------------------

fn escape_value(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

/// Inserts `<!ENTITY name "">` into the DOCTYPE (created if needed).
fn declare_entity_edit(
    grammar: Option<&Grammar>,
    source: &str,
    name: &str,
) -> Option<(Range<usize>, String)> {
    let declaration = format!("<!ENTITY {name} \"\">");
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let doctype = grammar
        .and_then(|grammar| grammar.doctype.clone())
        .or_else(|| find_doctype(source));
    match doctype {
        Some(doctype) => match doctype.internal_subset {
            Some(subset) => {
                let text = if source[..subset.end].ends_with('\n') {
                    format!("  {declaration}{newline}")
                } else {
                    format!("{newline}  {declaration}{newline}")
                };
                Some((subset.end..subset.end, text))
            }
            None => {
                let end = doctype.range.end;
                if !source[..end].ends_with('>') {
                    return None;
                }
                Some((
                    end - 1..end - 1,
                    format!(" [{newline}  {declaration}{newline}]"),
                ))
            }
        },
        None => {
            let tree = XmlTagTree::parse(source);
            let root = tree.elements().first()?;
            let start = root.start_tag.range.start;
            Some((
                start..start,
                format!(
                    "<!DOCTYPE {} [{newline}  {declaration}{newline}]>{newline}",
                    root.name(source)
                ),
            ))
        }
    }
}

/// DTD fixes for the requested range.
pub(crate) fn code_actions(actions: &mut Actions<'_>, grammar: Option<&Grammar>) {
    if !actions.wants(QUICK_FIX) {
        return;
    }
    let source = actions.source;
    let dtd = grammar.map(|grammar| &grammar.dtd);
    for problem in check_entity_references(source, dtd) {
        let InstanceProblemKind::UndefinedEntity { name } = &problem.kind else {
            continue;
        };
        if !actions.requested(&problem.range) {
            continue;
        }
        let Some(edit) = declare_entity_edit(grammar, source, name) else {
            continue;
        };
        let diagnostics = actions.matching(ENTITY_CODE, "kind", problem.kind.id(), &problem.range);
        actions.push(
            format!("Declare the entity '&{name};' in the DOCTYPE"),
            QUICK_FIX,
            vec![edit],
            diagnostics,
            true,
        );
    }
    let Some(dtd) = dtd.filter(|dtd| !dtd.incomplete) else {
        return;
    };
    for problem in validate_instance(source, dtd) {
        if !actions.requested(&problem.range) {
            continue;
        }
        let diagnostics =
            actions.matching(VALIDATION_CODE, "kind", problem.kind.id(), &problem.range);
        match &problem.kind {
            InstanceProblemKind::MissingAttribute {
                attribute,
                value,
                insert_at,
                ..
            } => actions.push(
                format!("Add required attribute '{attribute}'"),
                QUICK_FIX,
                vec![(
                    *insert_at..*insert_at,
                    format!(" {attribute}=\"{}\"", escape_value(value)),
                )],
                diagnostics,
                true,
            ),
            InstanceProblemKind::InvalidEnumeration { values } => {
                for value in values.iter().take(MAX_VALUE_ACTIONS) {
                    actions.push(
                        format!("Replace with '{value}'"),
                        QUICK_FIX,
                        vec![(problem.range.clone(), escape_value(value))],
                        diagnostics.clone(),
                        values.len() == 1,
                    );
                }
            }
            InstanceProblemKind::FixedValue { expected } => actions.push(
                format!("Replace with the fixed value '{expected}'"),
                QUICK_FIX,
                vec![(problem.range.clone(), escape_value(expected))],
                diagnostics,
                true,
            ),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------

mod symbol_kind {
    pub(super) const CLASS: u8 = 5;
    pub(super) const PROPERTY: u8 = 7;
    pub(super) const CONSTANT: u8 = 14;
    pub(super) const TYPE_PARAMETER: u8 = 26;
}

/// Symbols of a DTD file: elements (with their attributes), entities and
/// notations declared in the file itself.
pub(crate) fn document_symbols(
    grammar: &Grammar,
    uri: &str,
    source: &str,
    hierarchical: bool,
) -> Vec<Value> {
    let dtd = &grammar.dtd;
    let lines = LineIndex::new(source);
    let range = |range: &Range<usize>| {
        json!({
            "start": lines.position(source, range.start),
            "end": lines.position(source, range.end),
        })
    };
    // Declaration and name anchored in the document, when they are there.
    let local = |declaration: &Location, name: &Location| {
        let declaration = dtd.anchor(declaration);
        let name = dtd.anchor(name);
        (declaration.source == 0 && name.source == 0).then_some((declaration.range, name.range))
    };
    let symbol = |name: &str,
                  kind: u8,
                  detail: String,
                  spans: (Range<usize>, Range<usize>),
                  children: Vec<Value>,
                  container: Option<&str>| {
        if hierarchical {
            let mut symbol = json!({
                "name": name,
                "detail": detail,
                "kind": kind,
                "range": range(&spans.0),
                "selectionRange": range(&spans.1),
            });
            if !children.is_empty() {
                symbol["children"] = Value::Array(children);
            }
            symbol
        } else {
            let mut symbol = json!({
                "name": name,
                "kind": kind,
                "location": {"uri": uri, "range": range(&spans.0)},
            });
            if let Some(container) = container {
                symbol["containerName"] = json!(container);
            }
            symbol
        }
    };
    let mut symbols = Vec::new();
    for element in &dtd.elements {
        let Some(spans) = local(&element.declaration, &element.location) else {
            continue;
        };
        let attributes = dtd
            .attributes_of(&element.name)
            .filter_map(|attribute| {
                let spans = local(&attribute.declaration, &attribute.location)?;
                Some(symbol(
                    &attribute.name,
                    symbol_kind::PROPERTY,
                    format!("{} {}", attribute.attribute_type, attribute.default),
                    spans,
                    Vec::new(),
                    Some(&element.name),
                ))
            })
            .collect::<Vec<_>>();
        let flat_attributes = if hierarchical {
            Vec::new()
        } else {
            attributes.clone()
        };
        symbols.push(symbol(
            &element.name,
            symbol_kind::CLASS,
            element.content.to_string(),
            spans,
            if hierarchical { attributes } else { Vec::new() },
            None,
        ));
        symbols.extend(flat_attributes);
    }
    for entity in dtd.parameter_entities.iter().chain(&dtd.general_entities) {
        let Some(spans) = local(&entity.declaration, &entity.location) else {
            continue;
        };
        let name = if entity.parameter {
            format!("%{}", entity.name)
        } else {
            format!("&{}", entity.name)
        };
        symbols.push(symbol(
            &name,
            symbol_kind::CONSTANT,
            entity.display(),
            spans,
            Vec::new(),
            None,
        ));
    }
    for notation in &dtd.notations {
        let Some(spans) = local(&notation.declaration, &notation.location) else {
            continue;
        };
        symbols.push(symbol(
            &notation.name,
            symbol_kind::TYPE_PARAMETER,
            String::new(),
            spans,
            Vec::new(),
            None,
        ));
    }
    if hierarchical {
        symbols.sort_by_key(|symbol| {
            (
                symbol["range"]["start"]["line"].as_u64(),
                symbol["range"]["start"]["character"].as_u64(),
            )
        });
    }
    symbols
}

#[cfg(test)]
mod tests;
