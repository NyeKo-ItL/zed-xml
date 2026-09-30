//! Native XML LSP server.

mod analysis;
mod catalog;
mod code_actions;
mod colors;
mod dispatch;
mod dtd;
#[cfg(test)]
mod fixture_smoke;
mod folding;
mod formatting;
mod highlight;
mod hover;
mod identity;
mod linked_editing;
mod links;
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

    /// Recomputes the list of catalogs (settings and workspace folders);
    /// returns `true` if resolution may have changed.
    fn update_catalogs(&mut self) -> bool {
        let paths = catalog::catalog_paths(
            &self.settings.catalogs,
            self.workspace.roots(),
            self.settings.auto_detect_catalogs,
        );
        let changed = self.catalogs.set_roots(paths);
        if changed {
            self.log_catalog_errors();
            self.forget_resolutions();
        }
        changed
    }

    /// Rereads the catalogs modified on disk; returns `true` if resolution
    /// may have changed.
    fn refresh_catalog_files(&mut self) -> bool {
        if self.catalogs.is_empty() || !self.catalogs.refresh() {
            return false;
        }
        self.log_catalog_errors();
        self.forget_resolutions();
        true
    }

    /// Drops the cached schema graphs, whose dependencies were resolved
    /// through the previous catalogs.
    fn forget_resolutions(&mut self) {
        self.schemas.clear();
        self.model_cache.clear();
    }

    fn log_catalog_errors(&self) {
        for (path, error) in self.catalogs.errors() {
            eprintln!("xml-lsp: catalog {} ignored: {error}", path.display());
        }
    }

    /// Rereads the catalogs modified on disk; then republishes the
    /// diagnostics of the open documents.
    fn refresh_catalogs(
        &mut self,
        connection: &Connection,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if !self.refresh_catalog_files() {
            return Ok(());
        }
        self.register_catalog_watchers(connection)?;
        self.send_to_worker(worker::Job::ValidateAll);
        Ok(())
    }

    /// Asks the client to watch the catalog files (including outside the
    /// workspace); replaces the previous registration.
    fn register_catalog_watchers(
        &mut self,
        connection: &Connection,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if !self.watched_files_registration {
            return Ok(());
        }
        if let Some(id) = self.catalog_registration.take() {
            connection.sender.send(
                lsp_server::Request {
                    id: RequestId::from(format!("{id}/unregister")),
                    method: UNREGISTER_CAPABILITY_METHOD.to_owned(),
                    // Spelling (sic) mandated by the LSP specification.
                    params: json!({"unregisterations": [{
                        "id": id,
                        "method": DID_CHANGE_WATCHED_FILES_METHOD,
                    }]}),
                }
                .into(),
            )?;
        }
        let files = self.catalogs.files();
        if files.is_empty() {
            return Ok(());
        }
        self.catalog_registrations += 1;
        let id = format!("xml-lsp/watched-catalogs-{}", self.catalog_registrations);
        let watchers = files
            .iter()
            .map(|path| json!({"globPattern": path.to_string_lossy().replace('\\', "/")}))
            .collect::<Vec<_>>();
        connection.sender.send(
            lsp_server::Request {
                id: RequestId::from(id.clone()),
                method: REGISTER_CAPABILITY_METHOD.to_owned(),
                params: json!({"registrations": [{
                    "id": id,
                    "method": DID_CHANGE_WATCHED_FILES_METHOD,
                    "registerOptions": {"watchers": watchers},
                }]}),
            }
            .into(),
        )?;
        self.catalog_registration = Some(id);
        Ok(())
    }

    /// Reads the settings from `initializationOptions`.
    fn initialize_settings(&mut self, initialize_params: &Value) {
        self.initialization_settings = initialize_params
            .get("initializationOptions")
            .and_then(settings::xml_section)
            .cloned()
            .unwrap_or(Value::Null);
        self.settings = settings::Settings::from_value(&self.initialization_settings);
        self.configuration_support = initialize_params
            .pointer("/capabilities/workspace/configuration")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    }

    /// Requests the `xml` section from the client (`workspace/configuration`);
    /// the response is handled by [`Self::handle_response`].
    fn request_configuration(
        &mut self,
        connection: &Connection,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if !self.configuration_support {
            return Ok(());
        }
        self.configuration_requests += 1;
        let id = RequestId::from(format!(
            "xml-lsp/configuration-{}",
            self.configuration_requests
        ));
        self.pending_configuration = Some(id.clone());
        connection.sender.send(
            lsp_server::Request {
                id,
                method: CONFIGURATION_METHOD.to_owned(),
                params: json!({"items": [{"section": "xml"}]}),
            }
            .into(),
        )?;
        Ok(())
    }

    fn handle_response(
        &mut self,
        connection: &Connection,
        response: Response,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.pending_configuration.as_ref() != Some(&response.id) {
            return Ok(());
        }
        self.pending_configuration = None;
        let section = response
            .result
            .as_ref()
            .and_then(|result| result.get(0))
            .cloned()
            .unwrap_or(Value::Null);
        self.apply_settings(connection, &section)
    }

    /// `workspace/didChangeConfiguration`: uses the `xml` section pushed by
    /// the client, otherwise requests it again.
    fn configuration_changed(
        &mut self,
        connection: &Connection,
        params: &Value,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let pushed = params
            .get("settings")
            .and_then(|settings| settings.get("xml"))
            .filter(|section| section.is_object())
            .cloned();
        match pushed {
            Some(section) => self.apply_settings(connection, &section),
            None if self.configuration_support => self.request_configuration(connection),
            None => {
                let section = params
                    .get("settings")
                    .and_then(settings::xml_section)
                    .cloned()
                    .unwrap_or(Value::Null);
                self.apply_settings(connection, &section)
            }
        }
    }

    /// Replaces the settings with `initializationOptions` + `section` and
    /// republishes the diagnostics of open documents if validation changes.
    fn apply_settings(
        &mut self,
        connection: &Connection,
        section: &Value,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut merged = self.initialization_settings.clone();
        if let Some(section) = settings::xml_section(section) {
            settings::merge(&mut merged, section);
        }
        let settings = settings::Settings::from_value(&merged);
        let previous = std::mem::replace(&mut self.settings, settings);
        self.send_to_worker(worker::Job::Settings(Box::new(self.settings.clone())));
        let catalogs_changed = self.update_catalogs();
        if catalogs_changed {
            self.register_catalog_watchers(connection)?;
        }
        if previous.same_validation(&self.settings) && !catalogs_changed {
            return Ok(());
        }
        self.send_to_worker(worker::Job::ValidateAll);
        Ok(())
    }

    /// `publishDiagnostics` parameters of `uri` according to `xml.validation.*`.
    fn diagnostics(&mut self, uri: &str, source: &str) -> Value {
        let validation = self.settings.validation.clone();
        if !validation.enabled {
            return json!({"uri": uri, "diagnostics": []});
        }
        if self.settings.is_large(source) {
            // Beyond `xml.maxFileSize`: well-formedness only (linear).
            let diagnostics = if dtd::is_dtd_uri(uri) {
                Vec::new()
            } else {
                parse_xml(source).diagnostics
            };
            let notice = large_file_diagnostic(source.len(), self.settings.max_file_size);
            return diagnostics_params(uri, source, &diagnostics, &[notice]);
        }
        if dtd::is_dtd_uri(uri) {
            // DTD file: neither XML well-formedness nor schema.
            let diagnostics = self.dtd_diagnostics(uri, source);
            return json!({"uri": uri, "diagnostics": diagnostics});
        }
        let diagnostics = parse_xml(source).diagnostics;
        let mut extra = Vec::new();
        if validation.disallow_doc_type_decl {
            extra.extend(doctype_diagnostics(source));
        }
        extra.extend(catalog_diagnostics(uri, source));
        extra.extend(xslt::diagnostics(uri, source));
        extra.extend(schema_document_diagnostics(source));
        extra.extend(self.dtd_diagnostics(uri, source));
        if validation.schema != settings::SchemaValidation::Never {
            extra.extend(self.schema_diagnostics(uri, source));
        }
        if let Some(severity) = validation.no_grammar.severity()
            && !self.has_grammar(uri, source)
        {
            extra.extend(no_grammar_diagnostic(source, severity));
        }
        diagnostics_params(uri, source, &diagnostics, &extra)
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

    fn dtd_diagnostics(&mut self, uri: &str, source: &str) -> Vec<Value> {
        let validation = self.settings.validation.clone();
        dtd::diagnostics(&mut self.dtd_context(), uri, source, &validation)
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

    /// Handles a notification other than `exit`. Diagnostics are computed
    /// and published by the diagnostics worker, which receives the document
    /// and settings changes.
    fn handle_notification(
        &mut self,
        connection: &Connection,
        notification: Notification,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let params = &notification.params;
        match notification.method.as_str() {
            DID_CHANGE_CONFIGURATION_METHOD => self.configuration_changed(connection, params)?,
            DID_CHANGE_WORKSPACE_FOLDERS_METHOD => {
                self.workspace.change_folders(params);
                self.send_to_worker(worker::Job::WorkspaceFolders(params.clone()));
                if self.update_catalogs() {
                    self.register_catalog_watchers(connection)?;
                    self.send_to_worker(worker::Job::ValidateAll);
                }
            }
            DID_CHANGE_WATCHED_FILES_METHOD => {
                // Modified catalogs are reread by `refresh_catalogs`
                // before each message.
                self.workspace.files_changed(params);
            }
            DID_CLOSE_METHOD => {
                if let Some(uri) = document_uri(params) {
                    self.documents.remove(uri);
                    self.analyses.invalidate(uri);
                    self.send_to_worker(worker::Job::Document {
                        uri: uri.to_owned(),
                        version: None,
                        text: None,
                    });
                }
            }
            DID_OPEN_METHOD | DID_CHANGE_METHOD => {
                let changed = if notification.method == DID_OPEN_METHOD {
                    Self::opened_document(params)
                } else {
                    document_uri(params)
                        .and_then(|uri| Self::changed_document(params, self.documents.get(uri)))
                };
                let Some((uri, text)) = changed else {
                    return Ok(());
                };
                let version = params
                    .pointer("/textDocument/version")
                    .and_then(Value::as_i64);
                if self.diagnostics_worker.is_some() {
                    self.send_to_worker(worker::Job::Document {
                        uri: uri.clone(),
                        version,
                        text: Some(text.clone()),
                    });
                }
                self.analyses.invalidate(&uri);
                self.documents.insert(uri, text);
            }
            _ => {}
        }
        Ok(())
    }

    /// Asks the client to report XML files modified on disk
    /// (invalidates the `workspace/symbol` index).
    fn register_watched_files(
        &self,
        connection: &Connection,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if !self.watched_files_registration || self.workspace.roots().is_empty() {
            return Ok(());
        }
        connection.sender.send(
            lsp_server::Request {
                id: lsp_server::RequestId::from("xml-lsp/watched-files".to_owned()),
                method: REGISTER_CAPABILITY_METHOD.to_owned(),
                params: json!({"registrations": [{
                    "id": "xml-lsp/watched-files",
                    "method": DID_CHANGE_WATCHED_FILES_METHOD,
                    "registerOptions": {"watchers": [{"globPattern": symbols::watched_files_glob()}]},
                }]}),
            }
            .into(),
        )?;
        Ok(())
    }

    fn opened_document(params: &Value) -> Option<(String, String)> {
        let document = params.get("textDocument")?;
        Some((
            document.get("uri")?.as_str()?.to_owned(),
            document.get("text")?.as_str()?.to_owned(),
        ))
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

    fn schema_diagnostics(&mut self, uri: &str, source: &str) -> Vec<Value> {
        let references = match self.schema_references(uri, source) {
            Ok(references) => references,
            Err(error) => return vec![xsd_error_diagnostic(error)],
        };
        let schemas::LoadedSchemas { merged, errors } =
            self.schemas.load(references, &self.catalogs);
        let schema_errors = !errors.is_empty();
        let mut diagnostics = errors
            .into_iter()
            .map(xsd_schema_error_diagnostic)
            .collect::<Vec<_>>();
        if schema_errors
            && self.settings.validation.schema == settings::SchemaValidation::OnValidSchema
        {
            return diagnostics;
        }
        if let Some(schema) = merged {
            let lines = selection::LineIndex::new(source);
            // Values outside an enumeration are published by
            // `code_actions::enumeration_diagnostics`, whose ranges and
            // messages the enumeration quick fixes match.
            diagnostics.extend(
                validate_document_located(source, &schema)
                    .iter()
                    .filter(|diagnostic| diagnostic.kind != XsdDiagnosticKind::InvalidEnumeration)
                    .map(|diagnostic| xsd_error_diagnostic_at(diagnostic, source, &lines)),
            );
            let mut context = self.hover_context(uri);
            diagnostics.extend(code_actions::enumeration_diagnostics(
                &mut context,
                uri,
                source,
            ));
        }

        diagnostics
    }

    fn references(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?.clone();
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(&source, line, character);
        let links = self.identity_links(uri, &source);
        if identity::applies(&links, offset) {
            let include_declaration = params
                .get("context")
                .and_then(|context| context.get("includeDeclaration"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            return identity::references(&links, uri, &source, offset, include_declaration);
        }
        let include_declaration = params
            .pointer("/context/includeDeclaration")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if let Some(locations) = xslt::references(
            &self.xslt_context(),
            uri,
            &source,
            offset,
            include_declaration,
        ) {
            return Some(locations);
        }
        let name = element_name_at(&source, offset)?;
        let mut locations = Vec::new();
        if let Some((path, schema_source, declaration_offset)) =
            self.xsd_definition(uri, &source, &name)
        {
            locations.push(json!({
                "uri": path_to_uri(&path),
                "range": {
                    "start": position_at(&schema_source, declaration_offset),
                    "end": position_at(&schema_source, declaration_offset + name.len()),
                },
            }));
        }
        let lines = selection::LineIndex::new(&source);
        let needle = format!("<{name}");
        let mut search_from = 0usize;
        while let Some(relative) = source[search_from..].find(&needle) {
            let start = search_from + relative;
            locations.push(json!({
                "uri": uri,
                "range": {
                    "start": lines.position(&source, start),
                    "end": lines.position(&source, start + name.len() + 1),
                },
            }));
            search_from = start + name.len() + 1;
        }
        Some(Value::Array(locations))
    }

    fn definition(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?.clone();
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(&source, line, character);
        if let Some(links) = links::definition(
            uri,
            &source,
            offset,
            self.definition_link_support,
            &self.catalogs,
        ) {
            return Some(links);
        }
        let identity_links = self.identity_links(uri, &source);
        if let Some(location) = identity::definition(&identity_links, uri, &source, offset) {
            return Some(location);
        }
        if let Some(location) = xslt::definition(
            &self.xslt_context(),
            uri,
            &source,
            offset,
            self.definition_link_support,
        ) {
            return Some(location);
        }
        if let Some(grammar) = self.dtd_grammar(uri, &source)
            && let Some(location) = dtd::definition(&grammar, uri, &source, offset)
        {
            return Some(location);
        }
        let name = element_name_at(&source, offset)?;
        if let Some((path, schema_source, offset)) = self.xsd_definition(uri, &source, &name) {
            return Some(json!([{
                "uri": path_to_uri(&path),
                "range": {
                    "start": position_at(&schema_source, offset),
                    "end": position_at(&schema_source, offset + name.len()),
                },
            }]));
        }
        let declaration = source.find(&format!("<{name}"))?;
        Some(json!([{
            "uri": uri,
            "range": {
                "start": position_at(&source, declaration),
                "end": position_at(&source, declaration + name.len() + 1),
            },
        }]))
    }

    /// ID/IDREF (XSD and DTD) and key/keyref links of the document.
    fn identity_links(&mut self, uri: &str, source: &str) -> identity::Links {
        let mut links = Vec::new();
        if let Some(schema) = self.load_schema(uri, source) {
            links.extend(
                identity_links(source, &schema)
                    .into_iter()
                    .map(|link| (link.reference, link.target)),
            );
        }
        if let Some(grammar) = self.dtd_grammar(uri, source) {
            for link in dtd_core::id_links(source, &grammar.dtd) {
                if !links.contains(&link) {
                    links.push(link);
                }
            }
        }
        links
    }

    fn xsd_definition(
        &mut self,
        uri: &str,
        source: &str,
        name: &str,
    ) -> Option<(PathBuf, String, usize)> {
        let mut queue = self.schema_references(uri, source).ok()?;
        let mut visited = HashSet::new();
        while let Some(reference) = queue.pop() {
            if visited.len() >= MAX_SCHEMA_DOCUMENTS {
                return None;
            }
            if !visited.insert(reference.path.clone()) || is_remote_location(&reference.path) {
                continue;
            }
            let schema_source = read_text_file(&reference.path, MAX_RESOURCE_SIZE).ok()?;
            if let Some(offset) = xsd_element_name_offset(&schema_source, name) {
                return Some((reference.path, schema_source, offset));
            }
            if let Ok(dependencies) =
                resolve_schema_dependencies_with(&schema_source, &reference.path, &|request| {
                    self.catalogs.resolve_schema(request)
                })
            {
                queue.extend(dependencies);
            }
        }
        None
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

    fn changed_document(params: &Value, current: Option<&String>) -> Option<(String, String)> {
        let document = params.get("textDocument")?;
        let uri = document.get("uri")?.as_str()?.to_owned();
        let changes = params.get("contentChanges")?.as_array()?;
        let mut text = current.cloned();
        for change in changes {
            let replacement = change.get("text")?.as_str()?;
            if let Some(range) = change.get("range").and_then(Value::as_object) {
                let current_text = text.as_ref()?;
                let start = range.get("start")?;
                let end = range.get("end")?;
                let start_offset = offset_at(
                    current_text,
                    start.get("line")?.as_u64()? as usize,
                    start.get("character")?.as_u64()? as usize,
                );
                let end_offset = offset_at(
                    current_text,
                    end.get("line")?.as_u64()? as usize,
                    end.get("character")?.as_u64()? as usize,
                );
                if start_offset > end_offset
                    || end_offset > current_text.len()
                    || !current_text.is_char_boundary(start_offset)
                    || !current_text.is_char_boundary(end_offset)
                {
                    return None;
                }
                let mut updated = current_text.clone();
                updated.replace_range(start_offset..end_offset, replacement);
                text = Some(updated);
            } else {
                // A missing or null range is a valid full-document change.
                text = Some(replacement.to_owned());
            }
        }
        let text = text?;
        Some((uri, text))
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

fn xsd_element_name_offset(source: &str, expected_name: &str) -> Option<usize> {
    let mut reader = Reader::from_str(source);
    let mut search_from = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                let qname = element.name();
                let local = String::from_utf8_lossy(qname.as_ref());
                let event_end = reader.buffer_position() as usize;
                let event_start = source[search_from..event_end]
                    .find('<')
                    .map(|offset| search_from + offset)
                    .unwrap_or(search_from);
                search_from = event_end;
                if !local.ends_with(":element") && local != "element" {
                    continue;
                }
                for attribute in element.attributes().flatten() {
                    if attribute.key.as_ref() == b"name" {
                        let value = attribute.unescape_value().ok()?.into_owned();
                        if value != expected_name {
                            continue;
                        }
                        let attribute_start = source[event_start..event_end]
                            .find("name=\"")
                            .map(|offset| event_start + offset + 6)?;
                        return Some(attribute_start);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

fn xsd_error_diagnostic_at(
    diagnostic: &LocatedXsdDiagnostic,
    source: &str,
    lines: &selection::LineIndex,
) -> Value {
    json!({
        "range": {
            "start": lines.position(source, diagnostic.offset),
            "end": lines.position(source, diagnostic.end),
        },
        "severity": 1,
        "source": "xml-lsp",
        "code": "xsd-validation",
        "data": {"category": "xsd", "kind": "validation", "rule": diagnostic.kind.id()},
        "message": diagnostic.message,
    })
}

/// Problems of a schema document itself (the document is an `xs:schema`).
fn schema_document_diagnostics(source: &str) -> Vec<Value> {
    let problems = xsd_core::schema_check::check_schema_document(source);
    if problems.is_empty() {
        return Vec::new();
    }
    let lines = selection::LineIndex::new(source);
    problems
        .into_iter()
        .map(|problem| {
            json!({
                "range": {
                    "start": lines.position(source, problem.range.start),
                    "end": lines.position(source, problem.range.end),
                },
                "severity": 1,
                "source": "xml-lsp",
                "code": "xsd-schema",
                "data": {"category": "xsd", "kind": problem.rule},
                "message": problem.message,
            })
        })
        .collect()
}

fn xsd_schema_error_diagnostic(error: schemas::SchemaLoadError) -> Value {
    json!({
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 0},
        },
        "severity": if error.remote { 2 } else { 1 },
        "source": "xml-lsp",
        "code": "xsd-validation",
        "data": {
            "category": "xsd",
            "kind": "loading",
            "schemaUri": path_to_uri(&error.path),
            "schemaOffset": error.offset,
        },
        "message": error.message,
    })
}

/// Warnings of an open XML catalog: local targets not found.
fn catalog_diagnostics(uri: &str, source: &str) -> Vec<Value> {
    catalog::catalog_problems(uri, source)
        .into_iter()
        .map(|problem| {
            json!({
                "range": {
                    "start": position_at(source, problem.range.start),
                    "end": position_at(source, problem.range.end),
                },
                "severity": 2,
                "source": "xml-lsp",
                "code": "catalog-target-missing",
                "data": {"category": "catalog", "kind": "missingTarget"},
                "message": problem.message,
            })
        })
        .collect()
}

/// `xml.validation.disallowDocTypeDecl` error on each `<!DOCTYPE>`.
fn doctype_diagnostics(source: &str) -> Vec<Value> {
    xml_core::tags::scan_markup(source)
        .into_iter()
        .filter(|markup| {
            markup.kind == xml_core::tags::XmlMarkupKind::Declaration
                && source[markup.content.clone()].starts_with("DOCTYPE")
        })
        .map(|markup| {
            json!({
                "range": {
                    "start": position_at(source, markup.range.start),
                    "end": position_at(source, markup.range.end),
                },
                "severity": 1,
                "source": "xml-lsp",
                "code": "doctype-disallowed",
                "data": {"category": "xml", "kind": "doctypeDisallowed"},
                "message": "DOCTYPE declarations are not allowed (xml.validation.disallowDocTypeDecl).",
            })
        })
        .collect()
}

/// Information on a document larger than `xml.maxFileSize`: what is
/// disabled and how to change the limit.
fn large_file_diagnostic(size: usize, limit: Option<usize>) -> Value {
    let megabytes = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    json!({
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 0},
        },
        "severity": 3,
        "source": "xml-lsp",
        "code": "large-file",
        "data": {"category": "xml", "kind": "largeFile", "size": size, "limit": limit},
        "message": format!(
            "Large document ({:.1} MiB, xml.maxFileSize is {:.1} MiB): only well-formedness is checked; schema and DTD validation, document symbols, folding, colors, links, selection ranges and code actions are disabled for this file.",
            megabytes(size),
            megabytes(limit.unwrap_or(0)),
        ),
    })
}

/// `xml.validation.noGrammar` diagnostic on the name of the root element.
fn no_grammar_diagnostic(source: &str, severity: u8) -> Option<Value> {
    let tree = xml_core::tags::XmlTagTree::parse(source);
    let root = tree.elements().first()?;
    let name = root.start_tag.name.clone();
    Some(json!({
        "range": {
            "start": position_at(source, name.start),
            "end": position_at(source, name.end),
        },
        "severity": severity,
        "source": "xml-lsp",
        "code": "no-grammar",
        "data": {"category": "xml", "kind": "noGrammar"},
        "message": "No grammar (XSD, DTD) is associated with this document.",
    }))
}

fn xsd_error_diagnostic(diagnostic: impl Into<String>) -> Value {
    json!({
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 0},
        },
        "severity": 1,
        "source": "xml-lsp",
        "code": "xsd-validation",
        "data": {"category": "xsd", "kind": "loading"},
        "message": diagnostic.into(),
    })
}

fn element_name_at(source: &str, offset: usize) -> Option<String> {
    let prefix = &source[..offset.min(source.len())];
    let opening = prefix.rfind('<')?;
    let fragment = &prefix[opening + 1..];
    let fragment = fragment.strip_prefix('/').unwrap_or(fragment);
    fragment.split_whitespace().next().map(str::to_owned)
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

fn diagnostics_params(
    uri: &str,
    source: &str,
    diagnostics: &[XmlDiagnostic],
    schema_diagnostics: &[Value],
) -> Value {
    let lines = selection::LineIndex::new(source);
    let diagnostics = diagnostics.iter().map(|diagnostic| {
        let mut value = json!({
            "range": {
                "start": lines.position(source, diagnostic.offset),
                "end": lines.position(source, diagnostic.end),
            },
            "severity": 1,
            "source": "xml-lsp",
            "code": diagnostic.code(),
            "message": diagnostic.message,
        });
        if let Some(rule) = diagnostic.rule {
            value["data"] = json!({"category": "xml", "kind": rule});
        }
        value
    });

    let mut diagnostics = diagnostics.collect::<Vec<_>>();
    diagnostics.extend(schema_diagnostics.iter().cloned());
    json!({
        "uri": uri,
        "diagnostics": diagnostics,
    })
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
