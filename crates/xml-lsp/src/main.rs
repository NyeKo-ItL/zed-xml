//! Native XML LSP server.

mod analysis;
mod builtin;
mod catalog;
mod code_actions;
mod colors;
mod configuration;
mod diagnostics;
mod dispatch;
mod dtd;
#[cfg(test)]
mod fixture_smoke;
mod folding;
mod formatting;
mod highlight;
mod hover;
mod identity;
#[cfg(test)]
mod latency;
mod linked_editing;
mod links;
mod navigation;
mod notifications;
mod positions;
mod rename;
mod schemas;
mod selection;
mod settings;
mod symbols;
mod worker;
mod xslt;

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    path::PathBuf,
    sync::Arc,
};

use lsp_server::{Connection, ErrorCode, Message, Notification, RequestId, Response};
use quick_xml::{Reader, events::Event};
use serde_json::{Value, json};
#[cfg(test)]
use xml_core::format_xml;
use xml_core::{
    XmlDiagnostic, auto_close_tag, complete_xml, parse_xml,
    resource::{MAX_RESOURCE_SIZE, read_text_file},
};
use xsd_core::{
    LocatedXsdDiagnostic, MAX_SCHEMA_DOCUMENTS, XsdDiagnosticKind, XsdSchema,
    complete_attribute_values, complete_attributes, complete_elements, identity_links,
    is_remote_location, percent_decode, resolve_schema_dependencies_with,
    resolve_schema_locations_with, validate_document_located,
};

#[cfg(test)]
const INITIALIZE_METHOD: &str = "initialize";
const EXIT_METHOD: &str = "exit";
const DID_OPEN_METHOD: &str = "textDocument/didOpen";
const DID_CHANGE_METHOD: &str = "textDocument/didChange";
const DID_CLOSE_METHOD: &str = "textDocument/didClose";
const PUBLISH_DIAGNOSTICS_METHOD: &str = "textDocument/publishDiagnostics";
const FORMATTING_METHOD: &str = "textDocument/formatting";
const RANGE_FORMATTING_METHOD: &str = "textDocument/rangeFormatting";
const SYMBOL_METHOD: &str = "textDocument/documentSymbol";
const HOVER_METHOD: &str = "textDocument/hover";
const DEFINITION_METHOD: &str = "textDocument/definition";
const REFERENCES_METHOD: &str = "textDocument/references";
const COMPLETION_METHOD: &str = "textDocument/completion";
const DOCUMENT_HIGHLIGHT_METHOD: &str = "textDocument/documentHighlight";
const LINKED_EDITING_RANGE_METHOD: &str = "textDocument/linkedEditingRange";
const PREPARE_RENAME_METHOD: &str = "textDocument/prepareRename";
const RENAME_METHOD: &str = "textDocument/rename";
const FOLDING_RANGE_METHOD: &str = "textDocument/foldingRange";
const SELECTION_RANGE_METHOD: &str = "textDocument/selectionRange";
const DOCUMENT_LINK_METHOD: &str = "textDocument/documentLink";
const CODE_ACTION_METHOD: &str = "textDocument/codeAction";
const WORKSPACE_SYMBOL_METHOD: &str = "workspace/symbol";
const DOCUMENT_COLOR_METHOD: &str = "textDocument/documentColor";
const COLOR_PRESENTATION_METHOD: &str = "textDocument/colorPresentation";

/// Requests answered with an empty result for documents larger than
/// `xml.maxFileSize`: their cost grows with the whole document on each
/// call (cursor moves for code actions).
const LARGE_FILE_SKIPPED_METHODS: &[&str] = &[
    SYMBOL_METHOD,
    FOLDING_RANGE_METHOD,
    SELECTION_RANGE_METHOD,
    DOCUMENT_LINK_METHOD,
    CODE_ACTION_METHOD,
    DOCUMENT_COLOR_METHOD,
];

const DID_CHANGE_WORKSPACE_FOLDERS_METHOD: &str = "workspace/didChangeWorkspaceFolders";
const DID_CHANGE_WATCHED_FILES_METHOD: &str = "workspace/didChangeWatchedFiles";
const REGISTER_CAPABILITY_METHOD: &str = "client/registerCapability";
const UNREGISTER_CAPABILITY_METHOD: &str = "client/unregisterCapability";
const DID_CHANGE_CONFIGURATION_METHOD: &str = "workspace/didChangeConfiguration";
const CONFIGURATION_METHOD: &str = "workspace/configuration";

struct XmlLanguageServer {
    documents: HashMap<String, String>,
    /// Flat XSD models of the schema sets (validation, completion).
    schemas: schemas::SchemaStore,
    /// Analyses of the open documents cached per version.
    analyses: analysis::AnalysisCache,
    model_cache: hover::ModelCache,
    folding_settings: folding::FoldingSettings,
    /// The client accepts `LocationLink`s in response to `textDocument/definition`.
    definition_link_support: bool,
    /// The client accepts hierarchical `DocumentSymbol`s.
    hierarchical_document_symbols: bool,
    /// The client accepts dynamic registration of
    /// `workspace/didChangeWatchedFiles`.
    watched_files_registration: bool,
    workspace: symbols::WorkspaceIndex,
    /// Effective `xml.*` settings.
    settings: settings::Settings,
    /// `xml` section received in `initializationOptions`, the base on which
    /// the `workspace/configuration` settings are merged.
    initialization_settings: Value,
    /// The client answers `workspace/configuration`.
    configuration_support: bool,
    /// `workspace/configuration` request awaiting a response.
    pending_configuration: Option<RequestId>,
    configuration_requests: u64,
    /// XML catalogs (`xml.catalogs`, `xml.autoDetectCatalogs`).
    catalogs: catalog::Catalogs,
    /// Current `didChangeWatchedFiles` registration for the catalogs.
    catalog_registration: Option<String>,
    catalog_registrations: u64,
    /// Texts of the external DTDs read from disk.
    dtd_cache: dtd::DtdCache,
    /// Thread computing and publishing the diagnostics (`None` in the
    /// worker's own replica and in unit tests).
    diagnostics_worker: Option<worker::DiagnosticsWorker>,
}

impl XmlLanguageServer {
    fn new() -> Self {
        Self {
            documents: HashMap::new(),
            schemas: schemas::SchemaStore::default(),
            analyses: analysis::AnalysisCache::default(),
            model_cache: HashMap::new(),
            folding_settings: folding::FoldingSettings::default(),
            definition_link_support: false,
            hierarchical_document_symbols: false,
            watched_files_registration: false,
            workspace: symbols::WorkspaceIndex::default(),
            settings: settings::Settings::default(),
            initialization_settings: Value::Null,
            configuration_support: false,
            pending_configuration: None,
            configuration_requests: 0,
            catalogs: catalog::Catalogs::default(),
            catalog_registration: None,
            catalog_registrations: 0,
            dtd_cache: HashMap::new(),
            diagnostics_worker: None,
        }
    }

    /// Server configured from the `initialize` parameters: client
    /// capabilities, workspace folders, settings and catalogs.
    fn from_initialize_params(initialize_params: &Value) -> Self {
        let mut server = Self::new();
        server.folding_settings =
            folding::FoldingSettings::from_initialize_params(initialize_params);
        server.definition_link_support = initialize_params
            .pointer("/capabilities/textDocument/definition/linkSupport")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        server.hierarchical_document_symbols = initialize_params
            .pointer("/capabilities/textDocument/documentSymbol/hierarchicalDocumentSymbolSupport")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        server.watched_files_registration = initialize_params
            .pointer("/capabilities/workspace/didChangeWatchedFiles/dynamicRegistration")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        server.workspace = symbols::WorkspaceIndex::from_initialize_params(initialize_params);
        server.initialize_settings(initialize_params);
        server.update_catalogs();
        server
    }

    /// Sends `job` to the diagnostics worker.
    fn send_to_worker(&self, job: worker::Job) {
        if let Some(worker) = &self.diagnostics_worker {
            worker.send(job);
        }
    }

    /// DTD loading context.
    fn dtd_context(&mut self) -> dtd::DtdContext<'_> {
        dtd::DtdContext {
            documents: &self.documents,
            catalogs: &self.catalogs,
            cache: &mut self.dtd_cache,
        }
    }

    /// DTD grammar of the document (`<!DOCTYPE>` or `.dtd` file).
    fn dtd_grammar(&mut self, uri: &str, source: &str) -> Option<dtd::Grammar> {
        dtd::load(&mut self.dtd_context(), uri, source)
    }

    /// The document is associated with a grammar (XSD, DTD, `xml-model`,
    /// `xml.fileAssociations`) or is itself a schema.
    fn has_grammar(&self, uri: &str, source: &str) -> bool {
        if is_xsd_uri(uri) || dtd::is_dtd_uri(uri) || !self.associated_schemas(uri).is_empty() {
            return true;
        }
        if !matches!(
            self.resolve_schema_locations(uri, source),
            Ok(references) if references.is_empty()
        ) {
            return true;
        }
        if catalog::is_catalog(source) {
            return true;
        }
        xml_core::tags::scan_markup(source).iter().any(|markup| {
            let content = &source[markup.content.clone()];
            match markup.kind {
                xml_core::tags::XmlMarkupKind::Declaration => content.starts_with("DOCTYPE"),
                xml_core::tags::XmlMarkupKind::ProcessingInstruction => {
                    content.starts_with("xml-model")
                }
                _ => false,
            }
        })
    }

    /// Schemas associated with `uri` by `xml.fileAssociations`.
    fn associated_schemas(&self, uri: &str) -> Vec<PathBuf> {
        if self.settings.file_associations.is_empty() || is_xsd_uri(uri) {
            return Vec::new();
        }
        settings::associated_schemas(
            &self.settings.file_associations,
            self.workspace.roots(),
            &uri_to_path(uri),
            &self.catalogs,
        )
    }

    /// `xsi:schemaLocation` / `xsi:noNamespaceSchemaLocation` of the document,
    /// resolved through the XML catalogs.
    fn resolve_schema_locations(
        &self,
        uri: &str,
        source: &str,
    ) -> Result<Vec<xsd_core::SchemaReference>, String> {
        resolve_schema_locations_with(
            schema_resolution_source(source),
            uri_to_path(uri),
            &|request| self.catalogs.resolve_schema(request),
        )
    }

    /// Schemas declared by the document, or failing that associated by
    /// `xml.fileAssociations`.
    fn schema_references(
        &self,
        uri: &str,
        source: &str,
    ) -> Result<Vec<xsd_core::SchemaReference>, String> {
        let references = self.resolve_schema_locations(uri, source)?;
        if !references.is_empty() {
            return Ok(references);
        }
        Ok(self
            .associated_schemas(uri)
            .into_iter()
            .map(|path| xsd_core::SchemaReference {
                namespace: None,
                path,
                kind: xsd_core::SchemaLocationKind::NoNamespaceSchemaLocation,
            })
            .collect())
    }

    fn xslt_context(&self) -> xslt::XsltContext<'_> {
        xslt::XsltContext {
            documents: &self.documents,
            catalogs: &self.catalogs,
        }
    }

    fn hover_context(&mut self, uri: &str) -> hover::HoverContext<'_> {
        let associated_schemas = self.associated_schemas(uri);
        hover::HoverContext {
            documents: &self.documents,
            cache: &mut self.model_cache,
            associated_schemas,
            catalogs: &self.catalogs,
        }
    }

    fn completion(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?.clone();
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(&source, line, character);
        let grammar = self.dtd_grammar(uri, &source);
        if dtd::in_dtd_text(grammar.as_ref(), uri, &source, offset) {
            // DTD file or internal subset: DTD suggestions only.
            let items = dtd::completions(grammar.as_ref(), uri, &source, offset);
            return Some(json!({"isIncomplete": false, "items": items}));
        }
        let mut items = complete_xml(&source, offset)
            .into_iter()
            .map(|completion| {
                json!({
                    "label": completion.label,
                    "insertText": completion.insert_text,
                })
            })
            .collect::<Vec<_>>();
        if self.settings.auto_close_tags
            && let Some(completion) = auto_close_tag(&source, offset)
        {
            items.push(json!({
                "label": completion.label,
                "insertText": completion.insert_text,
            }));
        }
        if let Some(schema) = self.load_schema(uri, &source) {
            let schema_items = complete_elements(&source, offset, &schema)
                .into_iter()
                .chain(complete_attributes(&source, offset, &schema))
                .chain(complete_attribute_values(&source, offset, &schema))
                .map(|completion| {
                    json!({
                        "label": completion.label,
                        "insertText": completion.insert_text,
                    })
                });
            items.extend(schema_items);
        }
        items.extend(dtd::completions(grammar.as_ref(), uri, &source, offset));
        items.extend(xslt::completions(
            &self.xslt_context(),
            uri,
            &source,
            offset,
        ));
        deduplicate_completion_items(&mut items);
        Some(json!({"isIncomplete": false, "items": items}))
    }

    /// Merged schema set of the document (cached, see [`schemas`]).
    fn load_schema(&mut self, uri: &str, source: &str) -> Option<Arc<XsdSchema>> {
        let references = self.schema_references(uri, source).unwrap_or_default();
        self.schemas.load(references, &self.catalogs).merged
    }

    fn references_schema(&self, document_uri: &str, schema_uri: &str) -> bool {
        let Some(source) = self.documents.get(document_uri) else {
            return false;
        };
        let schema_path = uri_to_path(schema_uri);
        self.schema_references(document_uri, source)
            .map(|references| {
                references
                    .into_iter()
                    .any(|reference| reference.path == schema_path)
            })
            .unwrap_or(false)
    }

    /// Answers `textDocument/codeAction` (empty list without actions).
    fn code_action(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?.clone();
        let range = params.get("range")?;
        let offset = |position: &Value| {
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            Some(offset_at(&source, line, character))
        };
        let start = offset(range.get("start")?)?;
        let end = offset(range.get("end")?)?;
        let range = start.min(end)..start.max(end);
        let request_context = params.get("context").unwrap_or(&Value::Null);
        let grammar = self.dtd_grammar(uri, &source);
        let mut context = self.hover_context(uri);
        let mut actions =
            code_actions::code_actions(&mut context, uri, &source, range.clone(), request_context);
        let mut dtd_actions = code_actions::Actions::new(uri, &source, range, request_context);
        dtd::code_actions(&mut dtd_actions, grammar.as_ref());
        actions.extend(dtd_actions.actions);
        Some(Value::Array(actions))
    }

    fn document_colors(&self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        if !self.settings.colors_enabled {
            return Some(json!([]));
        }
        Some(colors::document_colors(uri, source))
    }

    fn color_presentations(&self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        let color = colors::Rgba::from_json(params.get("color")?)?;
        let range = params.get("range")?;
        let offset = |position: &Value| {
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            Some(offset_at(source, line, character))
        };
        let start = offset(range.get("start")?)?;
        let end = offset(range.get("end")?)?;
        Some(colors::color_presentations(
            uri,
            source,
            start.min(end)..start.max(end),
            color,
            range,
        ))
    }

    fn document_links(&self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        Some(links::document_links_json(uri, source, &self.catalogs))
    }

    fn hover(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(source, line, character);
        let source = source.clone();
        if let Some(hover) = xslt::hover(&self.xslt_context(), uri, &source, offset) {
            return Some(hover);
        }
        if let Some(grammar) = self.dtd_grammar(uri, &source)
            && let Some(hover) = dtd::hover(&grammar, uri, &source, offset)
        {
            return Some(hover);
        }
        let mut context = self.hover_context(uri);
        hover::hover(&mut context, uri, &source, offset)
    }

    fn document_highlight(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        let tree = self.analyses.tree(uri, source);
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(source, line, character);
        Some(Value::Array(highlight::highlights_in(
            source, &tree, offset,
        )))
    }

    fn linked_editing_range(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        let tree = self.analyses.tree(uri, source);
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(source, line, character);
        linked_editing::linked_editing_ranges_in(source, &tree, offset)
    }

    fn prepare_rename(&self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(source, line, character);
        xslt::prepare_rename(uri, source, offset).or_else(|| rename::prepare_rename(source, offset))
    }

    /// Returns a `WorkspaceEdit` (`changes`), `None` if nothing can be
    /// renamed at the requested position. Renaming a global XSD component
    /// is propagated to the open documents referencing the schema.
    fn rename(&self, params: &Value) -> Result<Option<Value>, rename::RenameError> {
        let location = (|| {
            let uri = params.get("textDocument")?.get("uri")?.as_str()?;
            let source = self.documents.get(uri)?;
            let position = params.get("position")?;
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            Some((uri, source, offset_at(source, line, character)))
        })();
        let Some((uri, source, offset)) = location else {
            return Ok(None);
        };
        let Some(new_name) = params.get("newName").and_then(Value::as_str) else {
            return Err(rename::RenameError {
                code: rename::INVALID_PARAMS,
                message: "Missing `newName` parameter.".to_owned(),
            });
        };
        if let Some(result) = xslt::rename(&self.xslt_context(), uri, source, offset, new_name) {
            return result.map(Some);
        }
        let Some(plan) = rename::rename(source, offset, new_name)? else {
            return Ok(None);
        };
        let mut changes = serde_json::Map::new();
        changes.insert(
            uri.to_owned(),
            Value::Array(rename::text_edits(source, &plan.ranges, new_name)),
        );
        if let Some(component) = &plan.component {
            for (document_uri, document) in &self.documents {
                if document_uri == uri || !self.references_schema(document_uri, uri) {
                    continue;
                }
                let ranges = rename::instance_ranges(document, component);
                if !ranges.is_empty() {
                    changes.insert(
                        document_uri.clone(),
                        Value::Array(rename::text_edits(document, &ranges, new_name)),
                    );
                }
            }
        }
        Ok(Some(json!({"changes": changes})))
    }

    fn folding_range(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        let tree = self.analyses.tree(uri, source);
        Some(Value::Array(folding::folding_ranges_in(
            source,
            &tree,
            &self.folding_settings,
        )))
    }

    fn selection_range(&self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        let offsets = params
            .get("positions")?
            .as_array()?
            .iter()
            .map(|position| {
                let line = position.get("line")?.as_u64()? as usize;
                let character = position.get("character")?.as_u64()? as usize;
                Some(offset_at(source, line, character))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Value::Array(selection::selection_ranges(source, &offsets)))
    }

    fn symbols(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?.clone();
        let source = &source;
        if !self.settings.symbols_enabled {
            return Some(json!([]));
        }
        let mut symbols = if dtd::is_dtd_uri(uri) {
            let hierarchical = self.hierarchical_document_symbols;
            self.dtd_grammar(uri, source)
                .map(|grammar| dtd::document_symbols(&grammar, uri, source, hierarchical))
                .unwrap_or_default()
        } else if self.hierarchical_document_symbols {
            symbols::document_symbols_in(source, self.analyses.tree(uri, source))
        } else {
            match xml_symbols(source) {
                Value::Array(symbols) => symbols,
                _ => Vec::new(),
            }
        };
        if let Some(limit) = self.settings.symbols_max_items {
            settings::limit_symbols(&mut symbols, limit);
        }
        Some(Value::Array(symbols))
    }

    fn workspace_symbols(&mut self, params: &Value) -> Value {
        let query = params.get("query").and_then(Value::as_str).unwrap_or("");
        Value::Array(self.workspace.query(&self.documents, query))
    }

    fn formatting(&self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        if !self.settings.format.enabled || dtd::is_dtd_uri(uri) {
            // The XML formatter does not apply to DTD files.
            return Some(json!([]));
        }
        let options = formatting::format_options(params, source, &self.settings.format);
        formatting::document_edits(source, &options)
    }

    fn range_formatting(&self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        if dtd::is_dtd_uri(uri) {
            return Some(json!([]));
        }
        let range = params.get("range")?;
        let offset = |position: &Value| {
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            Some(offset_at(source, line, character))
        };
        let start = offset(range.get("start")?)?;
        let end = offset(range.get("end")?)?;
        if !self.settings.format.enabled {
            return Some(json!([]));
        }
        let options = formatting::format_options(params, source, &self.settings.format);
        formatting::range_edits(source, start.min(end)..start.max(end), &options)
    }
}

/// `textDocument.uri` of request or notification parameters.
fn document_uri(params: &Value) -> Option<&str> {
    params.get("textDocument")?.get("uri")?.as_str()
}

fn is_xsd_uri(uri: &str) -> bool {
    uri_to_path(uri)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xsd"))
}

fn deduplicate_completion_items(items: &mut Vec<Value>) {
    let mut seen = HashSet::new();
    items.retain(|item| {
        let key = (
            item.get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            item.get("insertText")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        );
        seen.insert(key)
    });
}

fn schema_resolution_source(source: &str) -> &str {
    source.strip_suffix('<').unwrap_or(source)
}

/// Local path of a `file:` URI. On Windows the leading `/` is removed only
/// before a drive letter (`file:///C:/a` -> `C:/a`): `file:///opt/a` stays
/// the rooted path `/opt/a`, never the relative `opt/a`.
fn uri_to_path(uri: &str) -> PathBuf {
    xsd_core::file_uri_to_path(uri)
}

fn path_to_uri(path: &std::path::Path) -> String {
    let path = encode_uri_path(&path.to_string_lossy().replace('\\', "/"));
    if path.as_bytes().get(1) == Some(&b':') {
        format!("file:///{path}")
    } else if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn encode_uri_path(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b':' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn xml_symbols(source: &str) -> Value {
    let lines = selection::LineIndex::new(source);
    let mut reader = Reader::from_str(source);
    let mut stack: Vec<(String, usize)> = Vec::new();
    let mut search_from = 0usize;
    let mut symbols = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                let start = source[search_from..]
                    .find(&format!("<{name}"))
                    .map(|offset| search_from + offset)
                    .unwrap_or(search_from);
                search_from = start + name.len() + 1;
                stack.push((name, start));
            }
            Ok(Event::Empty(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                let start = source[search_from..]
                    .find(&format!("<{name}"))
                    .map(|offset| search_from + offset)
                    .unwrap_or(search_from);
                search_from = start + name.len() + 1;
                let end = reader.buffer_position() as usize;
                symbols.push(symbol_value(&name, start, end, source, &lines));
            }
            Ok(Event::End(_)) => {
                if let Some((name, start)) = stack.pop() {
                    symbols.push(symbol_value(
                        &name,
                        start,
                        reader.buffer_position() as usize,
                        source,
                        &lines,
                    ));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    Value::Array(symbols)
}

fn symbol_value(
    name: &str,
    start: usize,
    end: usize,
    source: &str,
    lines: &selection::LineIndex,
) -> Value {
    json!({
        "name": name,
        "kind": 13,
        "range": {"start": lines.position(source, start), "end": lines.position(source, end)},
        "selectionRange": {"start": lines.position(source, start), "end": lines.position(source, start + name.len() + 1)},
    })
}

fn server_capabilities(encoding: positions::PositionEncoding) -> Value {
    json!({
        "positionEncoding": encoding.as_str(),
        "textDocumentSync": {"openClose": true, "change": 2},
        "completionProvider": {"triggerCharacters": ["<", " ", "/", ">", "=", "\"", "?", "&", "%", "$"]},
        "documentFormattingProvider": true,
        "documentRangeFormattingProvider": true,
        "documentSymbolProvider": true,
        "hoverProvider": true,
        "definitionProvider": true,
        "referencesProvider": true,
        "documentHighlightProvider": true,
        "linkedEditingRangeProvider": true,
        "renameProvider": {"prepareProvider": true},
        "foldingRangeProvider": true,
        "selectionRangeProvider": true,
        "documentLinkProvider": {"resolveProvider": false},
        "codeActionProvider": {"codeActionKinds": code_actions::CODE_ACTION_KINDS},
        "workspaceSymbolProvider": true,
        "colorProvider": true,
        "workspace": {"workspaceFolders": {"supported": true, "changeNotifications": true}},
    })
}

/// UTF-8 offset of the LSP position (`line`, `character` in the negotiated
/// [`positions::PositionEncoding`]); the end of the line beyond it, the end
/// of the document beyond the last line.
pub(crate) fn offset_at(source: &str, line: usize, character: usize) -> usize {
    let start = if line == 0 {
        0
    } else {
        match source.match_indices('\n').nth(line - 1) {
            Some((index, _)) => index + 1,
            None => return source.len(),
        }
    };
    let content = positions::line_content(source, start);
    start + positions::PositionEncoding::current().offset_in_line(content, character)
}

/// LSP position of the UTF-8 `offset` (clamped to the document, floored to a
/// character boundary outside a CRLF).
pub(crate) fn position_at(source: &str, offset: usize) -> Value {
    let offset = positions::floor_position_offset(source, offset);
    let before = &source[..offset];
    let line = before.bytes().filter(|&byte| byte == b'\n').count();
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = positions::PositionEncoding::current().len(&source[line_start..offset]);
    json!({ "line": line, "character": character })
}

impl XmlLanguageServer {
    /// Result of the request `method` (`MethodNotFound` for an unknown one).
    fn handle_request(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<Value, dispatch::RequestError> {
        let empty = || json!([]);
        if LARGE_FILE_SKIPPED_METHODS.contains(&method)
            && document_uri(params)
                .and_then(|uri| self.documents.get(uri))
                .is_some_and(|source| self.settings.is_large(source))
        {
            // Whole-document features beyond `xml.maxFileSize` (reported by
            // the `large-file` diagnostic).
            return Ok(match method {
                SELECTION_RANGE_METHOD => Value::Null,
                _ => empty(),
            });
        }
        Ok(match method {
            COMPLETION_METHOD => self
                .completion(params)
                .unwrap_or_else(|| json!({"isIncomplete": false, "items": []})),
            SYMBOL_METHOD => self.symbols(params).unwrap_or_else(empty),
            WORKSPACE_SYMBOL_METHOD => self.workspace_symbols(params),
            DEFINITION_METHOD => self.definition(params).unwrap_or_else(empty),
            REFERENCES_METHOD => self.references(params).unwrap_or_else(empty),
            DOCUMENT_HIGHLIGHT_METHOD => self.document_highlight(params).unwrap_or_else(empty),
            LINKED_EDITING_RANGE_METHOD => self.linked_editing_range(params).unwrap_or(Value::Null),
            PREPARE_RENAME_METHOD => self.prepare_rename(params).unwrap_or(Value::Null),
            RENAME_METHOD => self
                .rename(params)
                .map_err(|error| dispatch::RequestError {
                    code: error.code,
                    message: error.message,
                })?
                .unwrap_or(Value::Null),
            FOLDING_RANGE_METHOD => self.folding_range(params).unwrap_or_else(empty),
            SELECTION_RANGE_METHOD => self.selection_range(params).unwrap_or(Value::Null),
            CODE_ACTION_METHOD => self.code_action(params).unwrap_or_else(empty),
            DOCUMENT_COLOR_METHOD => self.document_colors(params).unwrap_or_else(empty),
            COLOR_PRESENTATION_METHOD => self.color_presentations(params).unwrap_or_else(empty),
            DOCUMENT_LINK_METHOD => self.document_links(params).unwrap_or_else(empty),
            HOVER_METHOD => self.hover(params).unwrap_or(Value::Null),
            FORMATTING_METHOD => self.formatting(params).unwrap_or_else(empty),
            RANGE_FORMATTING_METHOD => self.range_formatting(params).unwrap_or_else(empty),
            #[cfg(test)]
            method if method.starts_with(tests::TEST_METHOD_PREFIX) => {
                return tests::handle_test_request(self, method, params);
            }
            _ => {
                return Err(dispatch::RequestError::new(
                    ErrorCode::MethodNotFound,
                    format!("unsupported request: {method}"),
                ));
            }
        })
    }
}

/// Serves one client connection; returns the process exit code: 0 when
/// `exit` follows `shutdown`, 1 otherwise (LSP 3.17).
fn run(connection: Connection) -> Result<i32, Box<dyn Error + Send + Sync>> {
    let (initialize_id, initialize_params) = connection.initialize_start()?;
    let encoding = positions::PositionEncoding::negotiate(&initialize_params);
    connection.initialize_finish(
        initialize_id,
        json!({
            "capabilities": server_capabilities(encoding),
            "serverInfo": {
                "name": "xml-lsp",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
    )?;
    encoding.install();
    let mut server = XmlLanguageServer::from_initialize_params(&initialize_params);
    let publisher = connection.sender.clone();
    server.diagnostics_worker = Some(worker::DiagnosticsWorker::spawn(
        XmlLanguageServer::from_initialize_params(&initialize_params),
        encoding,
        move |params| {
            publisher
                .send(
                    Notification {
                        method: PUBLISH_DIAGNOSTICS_METHOD.to_owned(),
                        params,
                    }
                    .into(),
                )
                .is_ok()
        },
    ));
    // `initialize_finish` has already consumed the `initialized` notification.
    server.register_watched_files(&connection)?;
    server.register_catalog_watchers(&connection)?;
    server.request_configuration(&connection)?;

    let mut incoming = dispatch::Incoming::default();
    let mut shutdown = false;
    let exit_code = loop {
        let Some(message) = incoming.next(&connection) else {
            // The client went away without `exit`.
            break 1;
        };
        if !shutdown && !matches!(message, Message::Response(_)) {
            dispatch::guarded_notification("catalog refresh", || {
                server.refresh_catalogs(&connection)
            })
            .transpose()?;
        }
        match message {
            Message::Request(request) => {
                let response = if shutdown {
                    Response::new_err(
                        request.id,
                        ErrorCode::InvalidRequest as i32,
                        "the server is shutting down".to_owned(),
                    )
                } else if incoming.take_cancelled(&request.id) {
                    Response::new_err(
                        request.id,
                        ErrorCode::RequestCanceled as i32,
                        "request cancelled".to_owned(),
                    )
                } else if request.method == dispatch::SHUTDOWN_METHOD {
                    shutdown = true;
                    Response::new_ok(request.id, Value::Null)
                } else {
                    dispatch::guarded_request(&request, || {
                        server.handle_request(&request.method, &request.params)
                    })
                };
                connection.sender.send(response.into())?;
            }
            Message::Notification(notification) if notification.method == EXIT_METHOD => {
                break if shutdown { 0 } else { 1 };
            }
            Message::Notification(_) if shutdown => {}
            Message::Notification(notification) => {
                let method = notification.method.clone();
                dispatch::guarded_notification(&method, || {
                    server.handle_notification(&connection, notification)
                })
                .transpose()?;
            }
            Message::Response(response) => {
                dispatch::guarded_notification("response", || {
                    server.handle_response(&connection, response)
                })
                .transpose()?;
            }
        }
    };
    // Stops the diagnostics worker (and its clone of the sender) so that
    // the transport can close.
    drop(server);
    Ok(exit_code)
}

fn main() {
    if std::env::args().any(|argument| argument == "--version") {
        println!("xml-lsp {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let (connection, io_threads) = Connection::stdio();

    let exit_code = run(connection).unwrap_or_else(|error| {
        eprintln!("xml-lsp stopped: {error}");
        1
    });

    if let Err(error) = io_threads.join() {
        eprintln!("xml-lsp transport stopped: {error}");
    }
    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests;
