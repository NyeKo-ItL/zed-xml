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
        rename::prepare_rename(source, offset)
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
        "completionProvider": {"triggerCharacters": ["<", " ", "/", ">", "=", "\"", "?", "&", "%"]},
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
mod tests {
    use super::*;
    use lsp_server::{Request, RequestId};
    use std::thread;

    /// Requests only the tests send, to observe the server deterministically.
    pub(super) const TEST_METHOD_PREFIX: &str = "xml-lsp/test/";
    /// Answers once the diagnostics of the changes sent so far are published.
    const SYNC_DIAGNOSTICS_METHOD: &str = "xml-lsp/test/syncDiagnostics";
    /// Panics in the handler.
    const PANIC_METHOD: &str = "xml-lsp/test/panic";
    /// Keeps the request loop busy for `params.milliseconds`.
    const SLEEP_METHOD: &str = "xml-lsp/test/sleep";

    pub(super) fn handle_test_request(
        server: &mut XmlLanguageServer,
        method: &str,
        params: &Value,
    ) -> Result<Value, dispatch::RequestError> {
        match method {
            SYNC_DIAGNOSTICS_METHOD => {
                if let Some(worker) = &server.diagnostics_worker {
                    assert!(
                        worker.flush(std::time::Duration::from_secs(20)),
                        "the diagnostics worker should become idle"
                    );
                }
            }
            PANIC_METHOD => panic!("test panic in a request handler"),
            SLEEP_METHOD => thread::sleep(std::time::Duration::from_millis(
                params["milliseconds"].as_u64().unwrap_or(100),
            )),
            _ => {
                return Err(dispatch::RequestError::new(
                    ErrorCode::MethodNotFound,
                    method,
                ));
            }
        }
        Ok(Value::Null)
    }

    #[test]
    fn preserves_a_user_change_from_self_closing_to_explicit_empty_element() {
        let uri = "file:///document.xml";
        let previous = "<root><item /></root>".to_owned();
        let params = json!({
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": [{
                "range": {
                    "start": {"line": 0, "character": 6},
                    "end": {"line": 0, "character": 14}
                },
                "text": "<item></item>"
            }]
        });

        let (_, current) = XmlLanguageServer::changed_document(&params, Some(&previous))
            .expect("the incremental change should be applied");
        assert_eq!(current, "<root><item></item></root>");

        let mut server = XmlLanguageServer::new();
        server.documents.insert(uri.to_owned(), current);
        let edits = server
            .formatting(&json!({"textDocument": {"uri": uri}}))
            .expect("the document should be formatted");
        assert_eq!(
            formatting::apply_edits(&server.documents[uri], &edits),
            "<root>\n  <item>\n  </item>\n</root>\n"
        );
    }

    #[test]
    fn accepts_a_full_document_change_with_a_null_range() {
        let params = json!({
            "textDocument": {"uri": "file:///document.xml", "version": 2},
            "contentChanges": [{"range": null, "text": "<root />"}]
        });

        let (_, current) =
            XmlLanguageServer::changed_document(&params, Some(&"<old />".to_owned()))
                .expect("a null range should mean full-document replacement");
        assert_eq!(current, "<root />");
    }

    #[test]
    fn formats_many_realistic_user_edit_sequences_without_stale_content() {
        let scenarios = [
            [
                "<root />",
                "<root><item /></root>",
                "<root><item>one</item></root>",
                "<root><item id=\"1\">two</item><empty /></root>",
                "<root><item id=\"2\">trois &amp; quatre</item><empty></empty></root>",
            ],
            [
                "<catalog />",
                "<catalog><book /></catalog>",
                "<catalog><book><title>XML</title></book></catalog>",
                "<catalog><book id=\"é\"><title>Édition</title><author /></book></catalog>",
                "<catalog><!-- note --><book><![CDATA[a < b]]></book></catalog>",
            ],
            [
                "<Message />",
                "<Message><Header /></Message>",
                "<Message><Header><Id>1</Id></Header></Message>",
                "<Message><Header><Id>2</Id><Date>2026-09-29</Date></Header><Body /></Message>",
                "<Message><Header><Id>3</Id></Header><Body><Value>42.5</Value></Body></Message>",
            ],
        ];

        for scenario in scenarios.iter().cycle().take(30) {
            let mut current = scenario[0].to_owned();
            for desired in scenario.iter().skip(1) {
                let params = incremental_replacement_params(&current, desired);
                let (_, updated) = XmlLanguageServer::changed_document(&params, Some(&current))
                    .expect("the simulated editor change should be applied");
                assert_eq!(&updated, desired);
                current = updated;

                let formatted = format_xml(&current).expect("valid edited XML should format");
                assert_eq!(
                    format_xml(&formatted).unwrap(),
                    formatted,
                    "formatting must be idempotent for {current:?}"
                );
                assert_eq!(
                    formatted.matches("<root").count(),
                    current.matches("<root").count()
                );
                assert_eq!(
                    formatted.matches("<catalog").count(),
                    current.matches("<catalog").count()
                );
                assert_eq!(
                    formatted.matches("<Message").count(),
                    current.matches("<Message").count()
                );
            }
        }
    }

    #[test]
    fn completion_after_typing_a_greater_than_sign_suggests_the_closing_tag() {
        let mut server = XmlLanguageServer::new();
        server.documents.insert(
            "file:///document.xml".to_owned(),
            "<root><child>".to_owned(),
        );

        let result = server
            .completion(&json!({
                "textDocument": {"uri": "file:///document.xml"},
                "position": {"line": 0, "character": 13}
            }))
            .expect("completion should be available after >");

        assert_eq!(
            result["items"],
            json!([{"label": "</child>", "insertText": "</child>"}])
        );
    }

    #[test]
    fn completion_after_xml_processing_instruction_prefix_offers_xsd_template() {
        let mut server = XmlLanguageServer::new();
        server
            .documents
            .insert("file:///document.xml".to_owned(), "<?".to_owned());

        let result = server
            .completion(&json!({
                "textDocument": {"uri": "file:///document.xml"},
                "position": {"line": 0, "character": 2}
            }))
            .expect("processing instruction completion should be available");

        assert_eq!(
            result["items"][1],
            json!({
                "label": "xml-model",
                "insertText": "xml-model href=\"schema.xsd\" type=\"application/xml\" schematypens=\"http://www.w3.org/2001/XMLSchema\"?>"
            })
        );
    }

    #[test]
    fn completion_triggers_cover_xml_typing_contexts() {
        let capabilities = server_capabilities(positions::PositionEncoding::default());
        assert_eq!(
            capabilities["completionProvider"]["triggerCharacters"],
            json!(["<", " ", "/", ">", "=", "\"", "?", "&", "%"])
        );
    }

    fn incremental_replacement_params(current: &str, desired: &str) -> Value {
        let prefix = current
            .bytes()
            .zip(desired.bytes())
            .take_while(|(left, right)| left == right)
            .count();
        let suffix = current[prefix..]
            .bytes()
            .rev()
            .zip(desired[prefix..].bytes().rev())
            .take_while(|(left, right)| left == right)
            .count();
        let end = current.len() - suffix;
        json!({
            "textDocument": {"uri": "file:///document.xml", "version": 2},
            "contentChanges": [{
                "range": {
                    "start": position_at(current, prefix),
                    "end": position_at(current, end)
                },
                "text": &desired[prefix..desired.len() - suffix]
            }]
        })
    }

    #[test]
    fn removes_duplicate_completion_items() {
        let mut items = vec![
            json!({"label": "child", "insertText": "child"}),
            json!({"label": "child", "insertText": "child"}),
            json!({"label": "other", "insertText": "other"}),
        ];
        deduplicate_completion_items(&mut items);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn tracks_open_workspace_documents_referencing_an_xsd() {
        let schema_path =
            std::env::temp_dir().join(format!("xml-lsp-workspace-{}.xsd", std::process::id()));
        std::fs::write(
            &schema_path,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"/></xs:schema>"#,
        )
        .expect("schema should be written");
        let xml_path = schema_path.with_file_name("workspace.xml");
        let xml_uri = path_to_uri(&xml_path);
        let xsd_uri = path_to_uri(&schema_path);
        let xml_source = format!(
            "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{}\" />",
            schema_path.file_name().unwrap().to_string_lossy()
        );
        let mut server = XmlLanguageServer::new();
        server.documents.insert(xml_uri.clone(), xml_source);
        server.documents.insert(xsd_uri.clone(), String::new());
        assert!(server.references_schema(&xml_uri, &xsd_uri));
        assert!(is_xsd_uri(&xsd_uri));
        std::fs::remove_file(schema_path).expect("schema should be removed");
    }

    #[test]
    fn serves_document_highlight_requests() {
        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let request = |id: i32, method: &str, params: Value| {
            client
                .sender
                .send(
                    Request {
                        id: RequestId::from(id),
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("request should be sent");
            loop {
                match client.receiver.recv().expect("a message should arrive") {
                    Message::Response(response) => {
                        assert_eq!(response.id, RequestId::from(id));
                        return response.result;
                    }
                    Message::Notification(_) => {}
                    message => panic!("unexpected message {message:?}"),
                }
            }
        };
        let notify = |method: &str, params: Value| {
            client
                .sender
                .send(
                    Notification {
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("notification should be sent");
        };

        let initialize = request(1, INITIALIZE_METHOD, json!({}));
        assert_eq!(
            initialize.unwrap()["capabilities"]["documentHighlightProvider"],
            true
        );
        notify("initialized", json!({}));
        notify(
            DID_OPEN_METHOD,
            json!({
                "textDocument": {
                    "uri": "file:///document.xml",
                    "text": "<ns:root>\r\n  <ns:item><ns:item/></ns:item>\r\n</ns:root>",
                }
            }),
        );
        let params = |line: u32, character: u32| {
            json!({
                "textDocument": {"uri": "file:///document.xml"},
                "position": {"line": line, "character": character},
            })
        };

        assert_eq!(
            request(2, DOCUMENT_HIGHLIGHT_METHOD, params(2, 4)),
            Some(json!([
                {"range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 8}}, "kind": 2},
                {"range": {"start": {"line": 2, "character": 2}, "end": {"line": 2, "character": 9}}, "kind": 2},
            ]))
        );
        assert_eq!(
            request(3, DOCUMENT_HIGHLIGHT_METHOD, params(1, 5)),
            Some(json!([
                {"range": {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 10}}, "kind": 2},
                {"range": {"start": {"line": 1, "character": 23}, "end": {"line": 1, "character": 30}}, "kind": 2},
            ]))
        );
        assert_eq!(
            request(4, DOCUMENT_HIGHLIGHT_METHOD, params(1, 13)),
            Some(json!([
                {"range": {"start": {"line": 1, "character": 12}, "end": {"line": 1, "character": 19}}, "kind": 2},
            ]))
        );
        assert_eq!(
            request(5, DOCUMENT_HIGHLIGHT_METHOD, params(1, 0)),
            Some(json!([]))
        );

        assert_eq!(request(6, "shutdown", json!(null)), Some(Value::Null));
        notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
    }

    #[test]
    fn serves_linked_editing_range_requests() {
        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let request = |id: i32, method: &str, params: Value| {
            client
                .sender
                .send(
                    Request {
                        id: RequestId::from(id),
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("request should be sent");
            loop {
                match client.receiver.recv().expect("a message should arrive") {
                    Message::Response(response) => {
                        assert_eq!(response.id, RequestId::from(id));
                        assert!(response.error.is_none(), "{:?}", response.error);
                        return response.result;
                    }
                    Message::Notification(_) => {}
                    message => panic!("unexpected message {message:?}"),
                }
            }
        };
        let notify = |method: &str, params: Value| {
            client
                .sender
                .send(
                    Notification {
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("notification should be sent");
        };

        let initialize = request(1, INITIALIZE_METHOD, json!({}));
        assert_eq!(
            initialize.unwrap()["capabilities"]["linkedEditingRangeProvider"],
            true
        );
        notify("initialized", json!({}));
        notify(
            DID_OPEN_METHOD,
            json!({
                "textDocument": {
                    "uri": "file:///document.xml",
                    "text": "<ns:root>\r\n  <ns:item><ns:item/></ns:item>\r\n</ns:root>",
                }
            }),
        );
        let params = |line: u32, character: u32| {
            json!({
                "textDocument": {"uri": "file:///document.xml"},
                "position": {"line": line, "character": character},
            })
        };

        assert_eq!(
            request(2, LINKED_EDITING_RANGE_METHOD, params(2, 9)),
            Some(json!({
                "ranges": [
                    {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 8}},
                    {"start": {"line": 2, "character": 2}, "end": {"line": 2, "character": 9}},
                ],
                "wordPattern": linked_editing::XML_NAME_WORD_PATTERN,
            }))
        );
        assert_eq!(
            request(3, LINKED_EDITING_RANGE_METHOD, params(1, 5)),
            Some(json!({
                "ranges": [
                    {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 10}},
                    {"start": {"line": 1, "character": 23}, "end": {"line": 1, "character": 30}},
                ],
                "wordPattern": linked_editing::XML_NAME_WORD_PATTERN,
            }))
        );
        // Self-closing element, content and unknown document: `null`.
        assert_eq!(
            request(4, LINKED_EDITING_RANGE_METHOD, params(1, 13)),
            Some(Value::Null)
        );
        assert_eq!(
            request(5, LINKED_EDITING_RANGE_METHOD, params(1, 0)),
            Some(Value::Null)
        );
        assert_eq!(
            request(
                6,
                LINKED_EDITING_RANGE_METHOD,
                json!({
                    "textDocument": {"uri": "file:///missing.xml"},
                    "position": {"line": 0, "character": 1},
                })
            ),
            Some(Value::Null)
        );

        assert_eq!(request(7, "shutdown", json!(null)), Some(Value::Null));
        notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
    }

    #[test]
    fn serves_folding_range_requests_within_the_client_range_limit() {
        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let request = |id: i32, method: &str, params: Value| {
            client
                .sender
                .send(
                    Request {
                        id: RequestId::from(id),
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("request should be sent");
            loop {
                match client.receiver.recv().expect("a message should arrive") {
                    Message::Response(response) => {
                        assert_eq!(response.id, RequestId::from(id));
                        return response.result;
                    }
                    Message::Notification(_) => {}
                    message => panic!("unexpected message {message:?}"),
                }
            }
        };
        let notify = |method: &str, params: Value| {
            client
                .sender
                .send(
                    Notification {
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("notification should be sent");
        };

        let initialize = request(
            1,
            INITIALIZE_METHOD,
            json!({"capabilities": {"textDocument": {"foldingRange": {
                "lineFoldingOnly": true,
                "rangeLimit": 3,
            }}}}),
        );
        assert_eq!(
            initialize.unwrap()["capabilities"]["foldingRangeProvider"],
            true
        );
        notify("initialized", json!({}));
        notify(
            DID_OPEN_METHOD,
            json!({
                "textDocument": {
                    "uri": "file:///document.xml",
                    "text": "<root>\r\n  <!-- #region items -->\r\n  <item\r\n    a=\"1\"\r\n    b=\"2\"/>\r\n  <!-- #endregion -->\r\n  <!--\r\n    note\r\n  -->\r\n</root>\r\n",
                }
            }),
        );
        let params = json!({"textDocument": {"uri": "file:///document.xml"}});

        assert_eq!(
            request(2, FOLDING_RANGE_METHOD, params.clone()),
            Some(json!([
                {"startLine": 0, "endLine": 8},
                {"startLine": 1, "endLine": 4, "kind": "region"},
                {"startLine": 6, "endLine": 7, "kind": "comment"},
            ]))
        );
        notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": "file:///document.xml", "version": 2},
                "contentChanges": [{"text": "<root><item/></root>"}],
            }),
        );
        assert_eq!(request(3, FOLDING_RANGE_METHOD, params), Some(json!([])));
        assert_eq!(
            request(
                4,
                FOLDING_RANGE_METHOD,
                json!({"textDocument": {"uri": "file:///missing.xml"}})
            ),
            Some(json!([]))
        );

        assert_eq!(request(5, "shutdown", json!(null)), Some(Value::Null));
        notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
    }

    #[test]
    fn serves_selection_range_requests_for_several_positions() {
        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let request = |id: i32, method: &str, params: Value| {
            client
                .sender
                .send(
                    Request {
                        id: RequestId::from(id),
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("request should be sent");
            loop {
                match client.receiver.recv().expect("a message should arrive") {
                    Message::Response(response) => {
                        assert_eq!(response.id, RequestId::from(id));
                        return response.result;
                    }
                    Message::Notification(_) => {}
                    message => panic!("unexpected message {message:?}"),
                }
            }
        };
        let notify = |method: &str, params: Value| {
            client
                .sender
                .send(
                    Notification {
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("notification should be sent");
        };
        let range = |start: (u32, u32), end: (u32, u32)| {
            json!({
                "start": {"line": start.0, "character": start.1},
                "end": {"line": end.0, "character": end.1},
            })
        };
        let ranges = |mut selection: &Value| {
            let mut ranges = vec![selection["range"].clone()];
            while let Some(parent) = selection.get("parent") {
                ranges.push(parent["range"].clone());
                selection = parent;
            }
            ranges
        };

        let initialize = request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
        assert_eq!(
            initialize.unwrap()["capabilities"]["selectionRangeProvider"],
            true
        );
        notify("initialized", json!({}));
        notify(
            DID_OPEN_METHOD,
            json!({
                "textDocument": {
                    "uri": "file:///document.xml",
                    "text": "<root>\r\n  <ns:😀 a=\"x y\">text</ns:😀>\r\n</root>",
                }
            }),
        );

        let result = request(
            2,
            SELECTION_RANGE_METHOD,
            json!({
                "textDocument": {"uri": "file:///document.xml"},
                "positions": [
                    {"line": 1, "character": 12},
                    {"line": 1, "character": 18},
                    {"line": 1, "character": 26},
                ],
            }),
        )
        .expect("selection ranges should be returned");
        let root = range((0, 0), (2, 7));
        let content = range((0, 6), (2, 0));
        let element = range((1, 2), (1, 29));
        assert_eq!(
            ranges(&result[0]),
            vec![
                range((1, 12), (1, 13)),
                range((1, 12), (1, 15)),
                range((1, 11), (1, 16)),
                range((1, 9), (1, 16)),
                range((1, 2), (1, 17)),
                element.clone(),
                content.clone(),
                root.clone(),
            ]
        );
        assert_eq!(
            ranges(&result[1]),
            vec![
                range((1, 17), (1, 21)),
                element.clone(),
                content.clone(),
                root.clone(),
            ]
        );
        assert_eq!(
            ranges(&result[2]),
            vec![
                range((1, 26), (1, 28)),
                range((1, 23), (1, 28)),
                range((1, 21), (1, 29)),
                element,
                content,
                root,
            ]
        );

        assert_eq!(
            request(
                3,
                SELECTION_RANGE_METHOD,
                json!({
                    "textDocument": {"uri": "file:///missing.xml"},
                    "positions": [{"line": 0, "character": 0}],
                })
            ),
            Some(Value::Null)
        );

        assert_eq!(request(4, "shutdown", json!(null)), Some(Value::Null));
        notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
    }

    /// Test LSP client: sends messages and collects the received
    /// `publishDiagnostics` notifications.
    struct TestClient {
        connection: Connection,
        diagnostics: std::cell::RefCell<Vec<Value>>,
    }

    impl TestClient {
        fn send(&self, message: Message) {
            self.connection
                .sender
                .send(message)
                .expect("message should be sent");
        }

        fn notify(&self, method: &str, params: Value) {
            self.send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            );
        }

        /// Next message that is not a diagnostics publication.
        fn next(&self) -> Message {
            loop {
                match self
                    .connection
                    .receiver
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("a message should arrive")
                {
                    Message::Notification(notification)
                        if notification.method == PUBLISH_DIAGNOSTICS_METHOD =>
                    {
                        self.diagnostics.borrow_mut().push(notification.params);
                    }
                    message => return message,
                }
            }
        }

        fn request(&self, id: i32, method: &str, params: Value) -> Value {
            self.send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            );
            match self.next() {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    response.result.expect("request should succeed")
                }
                message => panic!("unexpected message {message:?}"),
            }
        }

        /// Diagnostics published so far, then cleared. The synchronisation
        /// request guarantees that previous publications have been received.
        fn take_diagnostics(&self, id: i32) -> Vec<Value> {
            self.request(id, SYNC_DIAGNOSTICS_METHOD, Value::Null);
            std::mem::take(&mut *self.diagnostics.borrow_mut())
        }
    }

    fn codes(publication: &Value) -> Vec<String> {
        publication["diagnostics"]
            .as_array()
            .expect("diagnostics should be an array")
            .iter()
            .map(|diagnostic| diagnostic["code"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// Next message: a server -> client `method` request, acknowledged.
    fn expect_server_request(client: &TestClient, method: &str) -> Value {
        match client.next() {
            Message::Request(request) => {
                assert_eq!(request.method, method, "{request:?}");
                client.send(Response::new_ok(request.id, Value::Null).into());
                request.params
            }
            message => panic!("unexpected message {message:?}"),
        }
    }

    /// `data.kind` of the published diagnostics.
    fn diagnostic_kinds(publication: &Value) -> Vec<String> {
        publication["diagnostics"]
            .as_array()
            .expect("diagnostics should be an array")
            .iter()
            .map(|diagnostic| {
                diagnostic["data"]["kind"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn resolves_remote_schemas_through_xml_catalogs() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-catalogs {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("schemas")).expect("directory should be created");
        std::fs::create_dir_all(directory.join("my schemas")).expect("directory should be created");
        let strict = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"project\">\n    <xs:annotation><xs:documentation>Project resolved through a catalog.</xs:documentation></xs:annotation>\n    <xs:complexType><xs:sequence><xs:element name=\"name\" type=\"xs:string\"/></xs:sequence></xs:complexType>\n  </xs:element>\n</xs:schema>";
        let lenient = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"project\">\n    <xs:complexType><xs:sequence><xs:element name=\"other\" type=\"xs:string\"/></xs:sequence></xs:complexType>\n  </xs:element>\n</xs:schema>";
        let strict_path = directory.join("schemas/project.xsd");
        std::fs::write(&strict_path, strict).expect("schema should be written");
        std::fs::write(directory.join("schemas/lenient.xsd"), lenient)
            .expect("schema should be written");
        std::fs::write(directory.join("my schemas/project.xsd"), strict)
            .expect("schema should be written");
        let catalog_path = directory.join("catalog.xml");
        let catalog = |target: &str| {
            format!(
                "<catalog xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n  <system systemId=\"http://example.com/schemas/project.xsd\" uri=\"{target}\"/>\n</catalog>"
            )
        };
        std::fs::write(&catalog_path, catalog("schemas/project.xsd"))
            .expect("catalog should be written");

        let remote_uri = path_to_uri(&directory.join("remote.xml"));
        let remote = "<project xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"http://example.com/schemas/project.xsd\"><other/></project>";
        let spaced_uri = path_to_uri(&directory.join("spaced.xml"));
        let spaced = "<project xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"my%20schemas/project.xsd\"><other/></project>";
        let unmapped_uri = path_to_uri(&directory.join("unmapped.xml"));
        let unmapped = "<project xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"http://unmapped.example.com/x.xsd\"><other/></project>";
        let open_catalog_uri = path_to_uri(&directory.join("open-catalog.xml"));
        let open_catalog = "<catalog xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n  <uri name=\"a\" uri=\"schemas/project.xsd\"/>\n  <uri name=\"b\" uri=\"schemas/absent.xsd\"/>\n</catalog>";

        let (server, connection) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let client = TestClient {
            connection,
            diagnostics: Default::default(),
        };
        client.request(
            1,
            INITIALIZE_METHOD,
            json!({
                "rootUri": path_to_uri(&directory),
                "capabilities": {
                    "workspace": {"didChangeWatchedFiles": {"dynamicRegistration": true}},
                    "textDocument": {"definition": {"linkSupport": false}},
                },
                "initializationOptions": {"xml": {
                    "catalogs": ["catalog.xml"],
                    "validation": {"noGrammar": "hint"},
                }},
            }),
        );
        client.notify("initialized", json!({}));
        expect_server_request(&client, REGISTER_CAPABILITY_METHOD);
        let registration = expect_server_request(&client, REGISTER_CAPABILITY_METHOD);
        assert_eq!(
            registration["registrations"][0]["registerOptions"]["watchers"],
            json!([{"globPattern": catalog_path.to_string_lossy().replace('\\', "/")}])
        );

        for (uri, text) in [
            (&remote_uri, remote),
            (&spaced_uri, spaced),
            (&unmapped_uri, unmapped),
            (&open_catalog_uri, open_catalog),
        ] {
            client.notify(
                DID_OPEN_METHOD,
                json!({"textDocument": {"uri": uri, "text": text}}),
            );
        }
        let published = client.take_diagnostics(2);
        assert_eq!(published.len(), 4, "{published:?}");
        // Remote URL resolved offline by the catalog: XSD validation.
        assert_eq!(published[0]["uri"], remote_uri);
        assert!(
            diagnostic_kinds(&published[0]).contains(&"validation".to_owned()),
            "{published:?}"
        );
        assert!(!diagnostic_kinds(&published[0]).contains(&"loading".to_owned()));
        // Percent-encoded path (`my%20schemas`): same validation.
        assert!(
            diagnostic_kinds(&published[1]).contains(&"validation".to_owned()),
            "{published:?}"
        );
        assert!(!diagnostic_kinds(&published[1]).contains(&"loading".to_owned()));
        // Uncataloged URL: explicit warning, no error.
        assert_eq!(diagnostic_kinds(&published[2]), vec!["loading"]);
        assert_eq!(published[2]["diagnostics"][0]["severity"], 2);
        assert!(
            published[2]["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("xml.catalogs")
        );
        // Open catalog: missing target reported, no "noGrammar".
        assert_eq!(codes(&published[3]), vec!["catalog-target-missing"]);
        assert_eq!(
            published[3]["diagnostics"][0]["range"],
            json!({"start": {"line": 2, "character": 21}, "end": {"line": 2, "character": 39}})
        );

        // Completion driven by the cataloged schema.
        let character = remote.find("<other").unwrap() + 1;
        let completion = client.request(
            3,
            COMPLETION_METHOD,
            json!({"textDocument": {"uri": remote_uri}, "position": {"line": 0, "character": character}}),
        );
        let labels = completion["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["label"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"name".to_owned()), "{labels:?}");

        // Hover: documentation of the cataloged schema.
        let hover = client.request(
            4,
            HOVER_METHOD,
            json!({"textDocument": {"uri": remote_uri}, "position": {"line": 0, "character": 2}}),
        );
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("Project resolved through a catalog."),
            "{hover:?}"
        );

        // Link and definition to the local file.
        let links = client.request(
            5,
            DOCUMENT_LINK_METHOD,
            json!({"textDocument": {"uri": remote_uri}}),
        );
        assert_eq!(links[0]["target"], path_to_uri(&strict_path));
        let location = remote.find("http://example.com").unwrap() + 4;
        let definition = client.request(
            6,
            DEFINITION_METHOD,
            json!({"textDocument": {"uri": remote_uri}, "position": {"line": 0, "character": location}}),
        );
        assert_eq!(definition[0]["uri"], path_to_uri(&strict_path));

        // Catalog modified on disk: reread, diagnostics republished.
        std::fs::write(&catalog_path, catalog("schemas/lenient.xsd"))
            .expect("catalog should be written");
        client.notify(
            DID_CHANGE_WATCHED_FILES_METHOD,
            json!({"changes": [{"uri": path_to_uri(&catalog_path), "type": 2}]}),
        );
        expect_server_request(&client, UNREGISTER_CAPABILITY_METHOD);
        expect_server_request(&client, REGISTER_CAPABILITY_METHOD);
        let published = client.take_diagnostics(7);
        assert_eq!(published.len(), 4, "{published:?}");
        let remote_publication = |published: &[Value]| {
            published
                .iter()
                .find(|publication| publication["uri"] == remote_uri)
                .cloned()
                .expect("remote.xml should be republished")
        };
        assert_eq!(remote_publication(&published)["diagnostics"], json!([]));

        // Catalogs removed from the settings: the URL is remote again.
        client.notify(
            DID_CHANGE_CONFIGURATION_METHOD,
            json!({"settings": {"xml": {"catalogs": []}}}),
        );
        expect_server_request(&client, UNREGISTER_CAPABILITY_METHOD);
        let published = client.take_diagnostics(8);
        assert_eq!(published.len(), 4, "{published:?}");
        assert_eq!(
            diagnostic_kinds(&remote_publication(&published)),
            vec!["loading"]
        );

        assert_eq!(client.request(9, "shutdown", json!(null)), Value::Null);
        client.notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn navigates_ids_and_keys_and_reports_identity_problems() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-identity {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("directory should be created");
        std::fs::write(
            directory.join("library.xsd"),
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="library">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="book" maxOccurs="unbounded">
          <xs:complexType>
            <xs:attribute name="id" type="xs:ID"/>
            <xs:attribute name="isbn" type="xs:string"/>
          </xs:complexType>
        </xs:element>
        <xs:element name="loan" minOccurs="0" maxOccurs="unbounded">
          <xs:complexType>
            <xs:attribute name="book" type="xs:IDREF"/>
            <xs:attribute name="isbn" type="xs:string"/>
          </xs:complexType>
        </xs:element>
      </xs:sequence>
    </xs:complexType>
    <xs:key name="isbn">
      <xs:selector xpath="book"/>
      <xs:field xpath="@isbn"/>
    </xs:key>
    <xs:keyref name="loanIsbn" refer="isbn">
      <xs:selector xpath="loan"/>
      <xs:field xpath="@isbn"/>
    </xs:keyref>
  </xs:element>
</xs:schema>"#,
        )
        .expect("schema should be written");
        let uri = path_to_uri(&directory.join("library.xml"));

        let (server, connection) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let client = TestClient {
            connection,
            diagnostics: Default::default(),
        };
        client.request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
        client.notify("initialized", json!({}));

        // Line 1: the book "é1" with the ISBN "42"; line 2: a duplicate
        // ISBN; line 3: two loans.
        let source = "<library xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"library.xsd\">\r\n<book id=\"é1\" isbn=\"42\"/>\r\n<book id=\"b2\" isbn=\"42\"/>\r\n<loan book=\"é1\" isbn=\"42\"/><loan book=\"é1\" isbn=\"7\"/>\r\n</library>";
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "text": source}}),
        );
        let published = client.take_diagnostics(2);
        let rules = published.last().expect("diagnostics should be published")["diagnostics"]
            .as_array()
            .expect("diagnostics should be an array")
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic["data"]["rule"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    diagnostic["range"]["start"].clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rules,
            [
                (
                    "duplicateKey".to_owned(),
                    json!({"line": 2, "character": 20})
                ),
                (
                    "unknownKeyref".to_owned(),
                    json!({"line": 3, "character": 49})
                ),
            ]
        );

        // From the IDREF of the first loan to the ID of the book.
        let definition = client.request(
            3,
            "textDocument/definition",
            json!({"textDocument": {"uri": uri}, "position": {"line": 3, "character": 12}}),
        );
        assert_eq!(
            definition,
            json!([{"uri": uri, "range": {
                "start": {"line": 1, "character": 10},
                "end": {"line": 1, "character": 12},
            }}])
        );
        // From the keyref value to the key value.
        let definition = client.request(
            4,
            "textDocument/definition",
            json!({"textDocument": {"uri": uri}, "position": {"line": 3, "character": 23}}),
        );
        assert_eq!(
            definition[0]["range"]["start"],
            json!({"line": 1, "character": 20})
        );
        // References of the ID: both loans.
        let references = client.request(
            5,
            "textDocument/references",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 11},
                "context": {"includeDeclaration": false},
            }),
        );
        let starts = references
            .as_array()
            .expect("references should be an array")
            .iter()
            .map(|location| location["range"]["start"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            starts,
            [
                json!({"line": 3, "character": 12}),
                json!({"line": 3, "character": 39}),
            ]
        );

        assert_eq!(client.request(6, "shutdown", json!(null)), Value::Null);
        client.notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn publishes_datatype_diagnostics_on_values() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-datatypes {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("directory should be created");
        std::fs::write(
            directory.join("order.xsd"),
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"order\">\n    <xs:complexType>\n      <xs:sequence>\n        <xs:element name=\"date\" type=\"xs:date\"/>\n        <xs:element name=\"total\" type=\"Total\"/>\n      </xs:sequence>\n      <xs:attribute name=\"count\" type=\"xs:unsignedByte\"/>\n    </xs:complexType>\n  </xs:element>\n  <xs:simpleType name=\"Total\">\n    <xs:restriction base=\"xs:decimal\">\n      <xs:enumeration value=\"1.0\"/>\n      <xs:enumeration value=\"2.5\"/>\n    </xs:restriction>\n  </xs:simpleType>\n</xs:schema>",
        )
        .expect("schema should be written");
        let uri = path_to_uri(&directory.join("order.xml"));

        let (server, connection) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let client = TestClient {
            connection,
            diagnostics: Default::default(),
        };
        client.request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
        client.notify("initialized", json!({}));

        // Non-ASCII text before the values and CRLF line endings: ranges are
        // UTF-16 positions of the values.
        let source = "<order xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n  xsi:noNamespaceSchemaLocation=\"order.xsd\" é=\"\" count=\"300\">\r\n  <date>2024-13-01</date>\r\n  <total>1</total>\r\n</order>";
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "text": source}}),
        );
        let published = client.take_diagnostics(2);
        let diagnostics = published.last().expect("diagnostics should be published")["diagnostics"]
            .as_array()
            .expect("diagnostics should be an array")
            .iter()
            .filter(|diagnostic| diagnostic["data"]["rule"] != "unexpectedAttribute")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
        assert_eq!(diagnostics[0]["code"], "xsd-validation");
        assert_eq!(diagnostics[0]["data"]["rule"], "invalidAttributeValue");
        assert_eq!(
            diagnostics[0]["range"],
            json!({"start": {"line": 1, "character": 56}, "end": {"line": 1, "character": 59}})
        );
        assert_eq!(
            diagnostics[0]["message"],
            "attribute @count of <order>: '300' is not a valid xs:unsignedByte: the value must be at most 255"
        );
        assert_eq!(diagnostics[1]["data"]["rule"], "invalidContent");
        assert_eq!(
            diagnostics[1]["message"],
            "content of <date>: '2024-13-01' is not a valid xs:date: month must be 01-12"
        );
        // `1` equals the enumerated `1.0` in the value space of xs:decimal.

        client.notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": source.replace("count=\"300\"", "count=\"30\"").replace("2024-13-01", "2024-12-01").replace("<total>1<", "<total>3<")}],
            }),
        );
        let published = client.take_diagnostics(3);
        let diagnostics = published.last().expect("diagnostics should be published")["diagnostics"]
            .as_array()
            .expect("diagnostics should be an array")
            .iter()
            .filter(|diagnostic| diagnostic["data"]["rule"] != "unexpectedAttribute")
            .cloned()
            .collect::<Vec<_>>();
        // The enumeration is reported once, on the value.
        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
        assert_eq!(diagnostics[0]["data"]["rule"], "invalidEnumeration");
        assert_eq!(
            diagnostics[0]["range"],
            json!({"start": {"line": 3, "character": 9}, "end": {"line": 3, "character": 10}})
        );

        assert_eq!(client.request(4, "shutdown", json!(null)), Value::Null);
        client.notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn applies_initialization_option_settings() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-settings {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("schemas")).expect("directory should be created");
        std::fs::write(
            directory.join("schemas/project.xsd"),
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"project\">\n    <xs:complexType><xs:sequence><xs:element name=\"name\" type=\"xs:string\"/></xs:sequence></xs:complexType>\n  </xs:element>\n</xs:schema>",
        )
        .expect("schema should be written");
        let associated_uri = path_to_uri(&directory.join("app/build.project"));
        let plain_uri = path_to_uri(&directory.join("plain.xml"));
        let doctype_uri = path_to_uri(&directory.join("doctype.xml"));

        let (server, connection) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let client = TestClient {
            connection,
            diagnostics: Default::default(),
        };
        client.request(
            1,
            INITIALIZE_METHOD,
            json!({
                "rootUri": path_to_uri(&directory),
                "capabilities": {},
                "initializationOptions": {"settings": {"xml": {
                    "format": {"splitAttributes": "splitNewLine", "emptyElements": "collapse", "enabled": true},
                    "validation": {"noGrammar": "warning", "disallowDocTypeDecl": true},
                    "completion": {"autoCloseTags": false},
                    "symbols": {"maxItemsComputed": 2},
                    "colors": {"enabled": false},
                    "fileAssociations": [{"pattern": "**/*.project", "systemId": "schemas/project.xsd"}],
                }}},
            }),
        );
        client.notify("initialized", json!({}));

        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": associated_uri, "text": "<project><other/></project>"}}),
        );
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": plain_uri, "text": "<svg a=\"1\" b=\"2\"><rect fill=\"red\"></rect><g><c/></g></svg>"}}),
        );
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": doctype_uri, "text": "<!DOCTYPE r [<!ELEMENT r ANY>]>\n<r/>"}}),
        );
        let published = client.take_diagnostics(2);
        assert_eq!(published.len(), 3);
        // File association: XSD validation without xsi:schemaLocation.
        assert_eq!(published[0]["uri"], associated_uri);
        assert!(
            codes(&published[0]).contains(&"xsd-validation".to_owned()),
            "{published:?}"
        );
        assert_eq!(codes(&published[1]), vec!["no-grammar"]);
        assert_eq!(published[1]["diagnostics"][0]["severity"], 2);
        assert_eq!(
            published[1]["diagnostics"][0]["range"],
            json!({"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 4}})
        );
        // A DTD is a grammar, but the declaration is not allowed.
        assert_eq!(codes(&published[2]), vec!["doctype-disallowed"]);

        // Completion driven by the associated schema, without auto-closing.
        let labels = |id: i32, character: u32| {
            let completion = client.request(
                id,
                COMPLETION_METHOD,
                json!({"textDocument": {"uri": associated_uri}, "position": {"line": 0, "character": character}}),
            );
            completion["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["label"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        let after_start_tag = labels(30, 9);
        assert!(
            !after_start_tag.iter().any(|label| label.starts_with("</")),
            "{after_start_tag:?}"
        );
        let children = labels(3, 10);
        assert!(children.contains(&"name".to_owned()), "{children:?}");

        let edits = client.request(
            4,
            FORMATTING_METHOD,
            json!({"textDocument": {"uri": plain_uri}, "options": {"tabSize": 2, "insertSpaces": true}}),
        );
        assert_eq!(
            formatting::apply_edits(
                "<svg a=\"1\" b=\"2\"><rect fill=\"red\"></rect><g><c/></g></svg>",
                &edits
            ),
            "<svg\n  a=\"1\"\n  b=\"2\">\n  <rect fill=\"red\"/>\n  <g>\n    <c/>\n  </g>\n</svg>\n"
        );
        assert_eq!(
            client.request(
                5,
                DOCUMENT_COLOR_METHOD,
                json!({"textDocument": {"uri": plain_uri}})
            ),
            json!([])
        );
        let symbols = client.request(
            6,
            SYMBOL_METHOD,
            json!({"textDocument": {"uri": plain_uri}}),
        );
        assert_eq!(symbols.as_array().map(Vec::len), Some(2));

        assert_eq!(client.request(7, "shutdown", json!(null)), Value::Null);
        client.notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn pulls_and_reacts_to_configuration_changes() {
        let uri = "file:///configuration/broken.xml";
        let (server, connection) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let client = TestClient {
            connection,
            diagnostics: Default::default(),
        };
        client.request(
            1,
            INITIALIZE_METHOD,
            json!({
                "capabilities": {"workspace": {"configuration": true}},
                "initializationOptions": {"xml": {"format": {"tabSize": 4}}},
            }),
        );
        client.notify("initialized", json!({}));
        let answer_configuration = |section: Value| match client.next() {
            Message::Request(request) => {
                assert_eq!(request.method, CONFIGURATION_METHOD);
                assert_eq!(request.params, json!({"items": [{"section": "xml"}]}));
                client.send(Response::new_ok(request.id, json!([section])).into());
            }
            message => panic!("unexpected message {message:?}"),
        };
        // The client returns nothing: the initialization options remain.
        answer_configuration(Value::Null);

        let source = "<root><a></root>";
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "text": source}}),
        );
        let published = client.take_diagnostics(2);
        assert_eq!(published.len(), 1);
        assert!(!codes(&published[0]).is_empty());

        // Pushed settings: validation is disabled, diagnostics cleared.
        client.notify(
            DID_CHANGE_CONFIGURATION_METHOD,
            json!({"settings": {"xml": {"validation": {"enabled": false}}}}),
        );
        let published = client.take_diagnostics(3);
        assert_eq!(published, vec![json!({"uri": uri, "diagnostics": []})]);

        // A change without effect on validation republishes nothing, and the
        // initialization options remain the base.
        client.notify(
            DID_CHANGE_CONFIGURATION_METHOD,
            json!({"settings": {"xml": {"validation": {"enabled": false}, "format": {"insertSpaces": false}}}}),
        );
        assert!(client.take_diagnostics(4).is_empty());
        let edits = client.request(5, FORMATTING_METHOD, json!({"textDocument": {"uri": uri}}));
        assert_eq!(edits, json!([]), "malformed documents are not formatted");

        // Without a pushed section, the server requests the configuration again.
        client.notify(DID_CHANGE_CONFIGURATION_METHOD, json!({"settings": null}));
        answer_configuration(json!({"validation": {"enabled": true}}));
        let published = client.take_diagnostics(6);
        assert_eq!(published.len(), 1);
        assert_eq!(published[0]["uri"], uri);
        assert!(!codes(&published[0]).is_empty());

        // Formatting disabled.
        client.notify(
            DID_CHANGE_CONFIGURATION_METHOD,
            json!({"settings": {"xml": {"format": {"enabled": false}}}}),
        );
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": "file:///configuration/ok.xml", "text": "<a><b/></a>"}}),
        );
        assert_eq!(
            client.request(
                7,
                FORMATTING_METHOD,
                json!({"textDocument": {"uri": "file:///configuration/ok.xml"}, "options": {"tabSize": 2, "insertSpaces": true}}),
            ),
            json!([])
        );

        assert_eq!(client.request(8, "shutdown", json!(null)), Value::Null);
        client.notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
    }

    #[test]
    fn validation_settings_select_schema_diagnostics() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-schema-setting {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("directory should be created");
        std::fs::write(
            directory.join("valid.xsd"),
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:element name=\"root\"/></xs:schema>",
        )
        .expect("schema should be written");
        let document_path = directory.join("doc.xml");
        let uri = path_to_uri(&document_path);
        let source = "<other xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"valid.xsd\"/>";
        let broken = "<other xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"missing.xsd\"/>";
        let mut server = XmlLanguageServer::new();
        server.documents.insert(uri.clone(), source.to_owned());
        let codes_for = |server: &mut XmlLanguageServer, source: &str, settings: Value| {
            server.settings = settings::Settings::from_value(&settings);
            codes(&server.diagnostics(&uri, source))
        };
        assert_eq!(
            codes_for(&mut server, source, json!({})),
            vec!["xsd-validation"]
        );
        assert!(
            codes_for(
                &mut server,
                source,
                json!({"validation": {"schema": {"enabled": "never"}}})
            )
            .is_empty()
        );
        let on_valid = json!({"validation": {"schema": {"enabled": "onValidSchema"}}});
        assert_eq!(
            codes_for(&mut server, source, on_valid.clone()),
            vec!["xsd-validation"]
        );
        // Schema not found: only the loading error is reported.
        let diagnostics = {
            server.settings = settings::Settings::from_value(&on_valid);
            server.diagnostics(&uri, broken)
        };
        assert_eq!(diagnostics["diagnostics"].as_array().map(Vec::len), Some(1));
        assert_eq!(diagnostics["diagnostics"][0]["data"]["kind"], "loading");
        // A document bound to a schema is not reported by noGrammar.
        assert_eq!(
            codes_for(
                &mut server,
                source,
                json!({"validation": {"noGrammar": "hint"}})
            ),
            vec!["xsd-validation"]
        );
        assert_eq!(
            codes_for(
                &mut server,
                "<?xml-model href=\"x.rng\"?><r/>",
                json!({"validation": {"noGrammar": "hint"}})
            ),
            Vec::<String>::new()
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn serves_document_colors_and_color_presentations() {
        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let send = |message: Message| client.sender.send(message).expect("message should be sent");
        let request = |id: i32, method: &str, params: Value| {
            send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            );
            loop {
                match client.receiver.recv().expect("a message should arrive") {
                    Message::Response(response) => {
                        assert_eq!(response.id, RequestId::from(id));
                        return response.result.expect("request should succeed");
                    }
                    Message::Notification(_) => {}
                    message => panic!("unexpected message {message:?}"),
                }
            }
        };
        let notify = |method: &str, params: Value| {
            send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
        };

        let initialize = request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
        assert_eq!(initialize["capabilities"]["colorProvider"], true);
        notify("initialized", json!({}));

        let svg = "file:///icon.svg";
        notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": svg, "text": "<svg>\r\n  <!-- é -->\r\n  <rect fill=\"#F008\" stroke=\"rgb(0 128 0)\"/>\r\n</svg>"}}),
        );
        let colors = request(
            2,
            DOCUMENT_COLOR_METHOD,
            json!({"textDocument": {"uri": svg}}),
        );
        assert_eq!(
            colors,
            json!([
                {
                    "range": {"start": {"line": 2, "character": 14}, "end": {"line": 2, "character": 19}},
                    "color": {"red": 1.0, "green": 0.0, "blue": 0.0, "alpha": 136.0 / 255.0},
                },
                {
                    "range": {"start": {"line": 2, "character": 29}, "end": {"line": 2, "character": 41}},
                    "color": {"red": 0.0, "green": 128.0 / 255.0, "blue": 0.0, "alpha": 1.0},
                },
            ])
        );

        let range = colors[0]["range"].clone();
        let presentations = request(
            3,
            COLOR_PRESENTATION_METHOD,
            json!({
                "textDocument": {"uri": svg},
                "color": {"red": 0.0, "green": 0.0, "blue": 1.0, "alpha": 1.0},
                "range": range,
            }),
        );
        let labels = presentations
            .as_array()
            .unwrap()
            .iter()
            .map(|presentation| presentation["label"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                "#00FF",
                "#0000FF",
                "rgb(0, 0, 255)",
                "hsl(240, 100%, 50%)",
                "blue"
            ]
        );
        assert_eq!(
            presentations[0]["textEdit"],
            json!({"range": range, "newText": "#00FF"})
        );

        let resources = "file:///app/res/values/colors.xml";
        notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": resources, "text": "<resources>\n  <color name=\"accent\">#80FF0000</color>\n</resources>"}}),
        );
        let colors = request(
            4,
            DOCUMENT_COLOR_METHOD,
            json!({"textDocument": {"uri": resources}}),
        );
        assert_eq!(colors[0]["color"]["red"], 1.0);
        assert_eq!(colors[0]["color"]["alpha"], 128.0 / 255.0);
        let presentations = request(
            5,
            COLOR_PRESENTATION_METHOD,
            json!({
                "textDocument": {"uri": resources},
                "color": {"red": 0.0, "green": 1.0, "blue": 0.0, "alpha": 1.0},
                "range": colors[0]["range"],
            }),
        );
        assert_eq!(presentations[0]["label"], "#FF00FF00");
        assert_eq!(presentations[1]["label"], "#00FF00");
        assert_eq!(presentations.as_array().map(Vec::len), Some(2));

        let unknown = request(
            6,
            DOCUMENT_COLOR_METHOD,
            json!({"textDocument": {"uri": "file:///closed.svg"}}),
        );
        assert_eq!(unknown, json!([]));

        assert_eq!(request(7, "shutdown", json!(null)), Value::Null);
        notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
    }

    #[test]
    fn serves_the_file_types_of_the_xml_language() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-file-types {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        for (path, content) in [
            (
                "ui/MainWindow.xaml",
                "<Window xmlns=\"http://schemas.microsoft.com/winfx/2006/xaml/presentation\">\n  <Button id=\"saveButton\"/>\n</Window>",
            ),
            (
                "res/Strings.resx",
                "<root>\n  <data name=\"saveLabel\"><value>Save</value></data>\n</root>",
            ),
            (
                "vc/App.vcxproj.filters",
                "<Project>\n  <Filter id=\"saveFilter\"/>\n</Project>",
            ),
            // Claimed by the C# extension, not by the XML language.
            (
                "App.csproj",
                "<Project>\n  <PropertyGroup id=\"saveIgnored\"/>\n</Project>",
            ),
        ] {
            let path = directory.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).expect("directory should be created");
            std::fs::write(path, content).expect("file should be written");
        }

        let (server, connection) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let client = TestClient {
            connection,
            diagnostics: Default::default(),
        };
        client.request(
            1,
            INITIALIZE_METHOD,
            json!({
                "rootUri": path_to_uri(&directory),
                "capabilities": {
                    "workspace": {"didChangeWatchedFiles": {"dynamicRegistration": true}},
                },
            }),
        );
        client.notify("initialized", json!({}));
        let registration = expect_server_request(&client, REGISTER_CAPABILITY_METHOD);
        let glob =
            registration["registrations"][0]["registerOptions"]["watchers"][0]["globPattern"]
                .as_str()
                .expect("a watched files glob")
                .to_owned();
        assert_eq!(glob, symbols::watched_files_glob());
        assert!(settings::glob_match(&glob, "ui/MainWindow.xaml"), "{glob}");
        assert!(!settings::glob_match(&glob, "App.csproj"), "{glob}");

        let mut names = client
            .request(2, WORKSPACE_SYMBOL_METHOD, json!({"query": "save"}))
            .as_array()
            .expect("symbols should be an array")
            .iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["saveButton", "saveFilter", "saveLabel"]);

        // Open documents of the new types get the usual diagnostics, whatever
        // language identifier the client sends.
        let storyboard = path_to_uri(&directory.join("Main.storyboard"));
        let gpx = path_to_uri(&directory.join("track.gpx"));
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": storyboard, "languageId": "xml", "version": 1,
                "text": "<document>\r\n  <scenes>\r\n</document>"}}),
        );
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": gpx, "languageId": "XML", "version": 1,
                "text": "<gpx version=\"1.1\" creator=\"t\"><trk><name>\u{e9}t\u{e9}</name></trk></gpx>"}}),
        );
        let published = client.take_diagnostics(3);
        assert_eq!(published.len(), 2, "{published:?}");
        assert_eq!(published[0]["uri"], storyboard);
        assert!(
            codes(&published[0])
                .iter()
                .any(|code| code.starts_with("xml-")),
            "{published:?}"
        );
        assert_eq!(published[1]["uri"], gpx);
        assert!(
            !codes(&published[1])
                .iter()
                .any(|code| code.starts_with("xml-")),
            "{published:?}"
        );

        assert_eq!(client.request(4, "shutdown", json!(null)), Value::Null);
        client.notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn serves_workspace_and_hierarchical_document_symbols() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-workspace-symbols {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        for (path, content) in [
            (
                "schemas/shop.xsd",
                "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" targetNamespace=\"urn:shop\">\n  <xs:element name=\"order\"/>\n  <xs:complexType name=\"OrderType\"/>\n</xs:schema>",
            ),
            (
                "config/beans.xml",
                "<beans>\n  <bean id=\"orderService\"/>\n</beans>",
            ),
            (
                "node_modules/lib/order.xsd",
                "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:element name=\"orderIgnored\"/></xs:schema>",
            ),
            (".hidden/order.xml", "<order id=\"orderHidden\"/>"),
            ("target/order.xml", "<order id=\"orderBuilt\"/>"),
        ] {
            let path = directory.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).expect("directory should be created");
            std::fs::write(path, content).expect("file should be written");
        }
        let beans_uri = path_to_uri(&directory.join("config/beans.xml"));

        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let send = |message: Message| client.sender.send(message).expect("message should be sent");
        let request = |id: i32, method: &str, params: Value| {
            send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            );
            loop {
                match client.receiver.recv().expect("a message should arrive") {
                    Message::Response(response) => {
                        assert_eq!(response.id, RequestId::from(id));
                        return response.result.expect("request should succeed");
                    }
                    Message::Notification(_) => {}
                    message => panic!("unexpected message {message:?}"),
                }
            }
        };
        let notify = |method: &str, params: Value| {
            send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
        };
        let names = |symbols: &Value| {
            symbols
                .as_array()
                .expect("symbols should be an array")
                .iter()
                .map(|symbol| symbol["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };

        let initialize = request(
            1,
            INITIALIZE_METHOD,
            json!({
                "rootUri": "file:///ignored-when-workspace-folders-exist",
                "workspaceFolders": [{"uri": path_to_uri(&directory), "name": "shop"}],
                "capabilities": {
                    "textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": true}},
                    "workspace": {"didChangeWatchedFiles": {"dynamicRegistration": true}},
                },
            }),
        );
        assert_eq!(initialize["capabilities"]["workspaceSymbolProvider"], true);
        notify("initialized", json!({}));
        match client
            .receiver
            .recv()
            .expect("a registration should arrive")
        {
            Message::Request(registration) => {
                assert_eq!(registration.method, REGISTER_CAPABILITY_METHOD);
                assert_eq!(
                    registration.params["registrations"][0]["method"],
                    DID_CHANGE_WATCHED_FILES_METHOD
                );
                send(Response::new_ok(registration.id, Value::Null).into());
            }
            message => panic!("unexpected message {message:?}"),
        }

        let symbols = request(2, WORKSPACE_SYMBOL_METHOD, json!({"query": "order"}));
        assert_eq!(names(&symbols), vec!["order", "OrderType", "orderService"]);
        assert_eq!(
            symbols[0],
            json!({
                "name": "order",
                "kind": symbols::kind::FIELD,
                "containerName": "urn:shop",
                "location": {
                    "uri": path_to_uri(&directory.join("schemas/shop.xsd")),
                    "range": {
                        "start": {"line": 1, "character": 20},
                        "end": {"line": 1, "character": 25},
                    },
                },
            })
        );
        assert_eq!(symbols[1]["kind"], symbols::kind::CLASS);
        assert_eq!(symbols[2]["containerName"], "bean");

        // An unsaved open buffer replaces the file on disk.
        notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": beans_uri, "text": "<beans>\r\n  <bean id=\"orderRepository\"><property name=\"dataSource\"/></bean>\r\n</beans>"}}),
        );
        let symbols = request(3, WORKSPACE_SYMBOL_METHOD, json!({"query": "OrdRep"}));
        assert_eq!(names(&symbols), vec!["orderRepository"]);
        assert_eq!(
            symbols[0]["location"]["range"]["start"],
            json!({"line": 1, "character": 12})
        );

        let document = request(
            4,
            SYMBOL_METHOD,
            json!({"textDocument": {"uri": beans_uri}}),
        );
        assert_eq!(document[0]["name"], "beans");
        assert_eq!(document[0]["children"][0]["name"], "bean");
        assert_eq!(
            document[0]["children"][0]["detail"],
            "id=\"orderRepository\""
        );
        assert_eq!(
            document[0]["children"][0]["children"][0]["detail"],
            "name=\"dataSource\""
        );

        let all = request(5, WORKSPACE_SYMBOL_METHOD, json!({"query": ""}));
        assert_eq!(all.as_array().map(Vec::len), Some(5));

        notify(
            DID_CHANGE_WORKSPACE_FOLDERS_METHOD,
            json!({"event": {"added": [], "removed": [{"uri": path_to_uri(&directory), "name": "shop"}]}}),
        );
        let symbols = request(6, WORKSPACE_SYMBOL_METHOD, json!({"query": "order"}));
        assert_eq!(names(&symbols), vec!["orderRepository"]);

        assert_eq!(request(7, "shutdown", json!(null)), Value::Null);
        notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn serves_code_actions_that_fix_published_diagnostics() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-code-actions {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("directory should be created");
        std::fs::write(
            directory.join("items.xsd"),
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="items"><xs:complexType><xs:sequence>
    <xs:element name="item" maxOccurs="unbounded"><xs:complexType>
      <xs:attribute name="id" type="xs:string" use="required"/>
      <xs:attribute name="kind" use="required"><xs:simpleType><xs:restriction base="xs:string">
        <xs:enumeration value="book"/><xs:enumeration value="disc"/>
      </xs:restriction></xs:simpleType></xs:attribute>
    </xs:complexType></xs:element>
  </xs:sequence></xs:complexType></xs:element>
</xs:schema>"#,
        )
        .expect("schema should be written");
        let uri = path_to_uri(&directory.join("items.xml"));

        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let send = |message: Message| client.sender.send(message).expect("message should be sent");
        let request = |id: i32, method: &str, params: Value| {
            send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            );
            loop {
                match client.receiver.recv().expect("a message should arrive") {
                    Message::Response(response) => {
                        assert_eq!(response.id, RequestId::from(id));
                        return response.result.expect("request should succeed");
                    }
                    Message::Notification(_) => {}
                    message => panic!("unexpected message {message:?}"),
                }
            }
        };
        let notify = |method: &str, params: Value| {
            send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
        };
        let diagnostics = || match client
            .receiver
            .recv()
            .expect("diagnostics should be published")
        {
            Message::Notification(notification)
                if notification.method == PUBLISH_DIAGNOSTICS_METHOD =>
            {
                notification.params["diagnostics"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            }
            message => panic!("unexpected message {message:?}"),
        };

        let initialize = request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
        assert_eq!(
            initialize["capabilities"]["codeActionProvider"],
            json!({"codeActionKinds": ["quickfix", "refactor", "source"]})
        );
        notify("initialized", json!({}));

        // Each preferred quick fix is applied, then the document is
        // revalidated: the fixed diagnostic disappears.
        let mut source = "<items xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n       xsi:noNamespaceSchemaLocation=\"items.xsd\">\r\n  <item id=\"é1\" kind=\"bok\"></itme>\r\n  <item/>\r\n</items>".to_owned();
        notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "text": source}}),
        );
        let mut published = diagnostics();
        let mut version = 1;
        let mut applied = Vec::new();
        for id in 2.. {
            let Some(diagnostic) = published.first().cloned() else {
                break;
            };
            assert!(id < 10, "diagnostics should converge: {published:#?}");
            let actions = request(
                id,
                CODE_ACTION_METHOD,
                json!({
                    "textDocument": {"uri": uri},
                    "range": diagnostic["range"],
                    "context": {"diagnostics": [diagnostic], "only": ["quickfix"]},
                }),
            );
            let action = actions
                .as_array()
                .unwrap()
                .iter()
                .find(|action| action["isPreferred"] == true)
                .unwrap_or_else(|| panic!("no preferred fix for {diagnostic:#?}: {actions:#?}"))
                .clone();
            assert_eq!(action["diagnostics"], json!([diagnostic]));
            applied.push(action["title"].as_str().unwrap().to_owned());
            source = formatting::apply_edits(&source, &action["edit"]["changes"][&uri]);
            version += 1;
            notify(
                DID_CHANGE_METHOD,
                json!({
                    "textDocument": {"uri": uri, "version": version},
                    "contentChanges": [{"text": source}],
                }),
            );
            let remaining = diagnostics();
            assert!(
                !remaining.contains(&diagnostic),
                "{diagnostic:#?} should be fixed by {applied:?}"
            );
            published = remaining;
        }
        assert_eq!(
            applied,
            vec![
                "Replace </itme> with </item>",
                "Add required attributes id, kind",
                "Replace with `book`",
            ]
        );
        assert_eq!(
            source,
            "<items xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n       xsi:noNamespaceSchemaLocation=\"items.xsd\">\r\n  <item id=\"é1\" kind=\"book\"></item>\r\n  <item id=\"\" kind=\"book\"/>\r\n</items>"
        );

        // Rewrite and schema binding on a document without a schema.
        let other = path_to_uri(&directory.join("other.xml"));
        notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": other, "text": "<root><a></a></root>"}}),
        );
        assert_eq!(diagnostics(), Vec::<Value>::new());
        let actions = request(
            20,
            CODE_ACTION_METHOD,
            json!({
                "textDocument": {"uri": other},
                "range": {"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 7}},
                "context": {"diagnostics": []},
            }),
        );
        let titles = actions
            .as_array()
            .unwrap()
            .iter()
            .map(|action| {
                (
                    action["kind"].as_str().unwrap(),
                    action["title"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            vec![
                ("refactor.rewrite", "Convert <a></a> to self-closing <a/>"),
                ("source", "Bind the document to the XSD schema items.xsd"),
            ]
        );
        assert_eq!(
            request(
                21,
                CODE_ACTION_METHOD,
                json!({
                    "textDocument": {"uri": "file:///missing.xml"},
                    "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
                    "context": {"diagnostics": []},
                }),
            ),
            json!([])
        );

        assert_eq!(request(22, "shutdown", json!(null)), Value::Null);
        notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
        std::fs::remove_dir_all(directory).expect("directory should be removed");
    }

    #[test]
    fn serves_dtd_diagnostics_completion_hover_definition_and_fixes() {
        let directory = std::env::temp_dir().join(format!("xml-lsp-dtd {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("directory should be created");
        let dtd_path = directory.join("note.dtd");
        std::fs::write(
            &dtd_path,
            "<!-- Root note. -->\n<!ELEMENT note (to, body?)>\n<!ELEMENT to (#PCDATA)>\n<!ELEMENT body (#PCDATA)>\n<!ATTLIST note lang (fr | en) #REQUIRED>\n",
        )
        .expect("dtd should be written");
        let uri = path_to_uri(&directory.join("note.xml"));
        // Internal subset + external DTD, CRLF line endings.
        let source = "<!DOCTYPE note SYSTEM \"note.dtd\" [\r\n  <!ENTITY sig \"Alice\">\r\n]>\r\n<note lang=\"de\">\r\n  <to>Bob &sig; &unknown;</to>\r\n  <extra/>\r\n</note>";
        let broken_uri = path_to_uri(&directory.join("broken.dtd"));
        let broken = "<!ELEMENT a (b | c, d)>\n<!ELEMENT b EMPTY>";
        let typing_uri = path_to_uri(&directory.join("typing.xml"));
        let typing = "<!DOCTYPE note SYSTEM \"note.dtd\"><note lang=\"fr\"><";

        let (server, connection) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let client = TestClient {
            connection,
            diagnostics: Default::default(),
        };
        let initialize = client.request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
        assert_eq!(
            initialize["capabilities"]["completionProvider"]["triggerCharacters"],
            json!(["<", " ", "/", ">", "=", "\"", "?", "&", "%"])
        );
        client.notify("initialized", json!({}));
        for (document_uri, text) in [(&uri, source), (&broken_uri, broken), (&typing_uri, typing)] {
            client.notify(
                DID_OPEN_METHOD,
                json!({"textDocument": {"uri": document_uri, "text": text}}),
            );
        }
        let published = client.take_diagnostics(2);
        assert_eq!(published.len(), 3, "{published:?}");
        assert_eq!(published[0]["uri"], uri);
        assert_eq!(
            codes(&published[0]),
            vec![
                "xml-entity",
                "dtd-validation",
                "dtd-validation",
                "dtd-validation",
            ],
            "{published:?}"
        );
        assert_eq!(
            diagnostic_kinds(&published[0]),
            vec![
                "undefinedEntity",
                "invalidEnumeration",
                "unexpectedElement",
                "undeclaredElement",
            ]
        );
        assert_eq!(
            published[0]["diagnostics"][1]["range"],
            json!({"start": {"line": 3, "character": 12}, "end": {"line": 3, "character": 14}})
        );
        // DTD file: DTD errors only (no XML well-formedness).
        assert_eq!(codes(&published[1]), vec!["dtd-grammar"]);
        assert_eq!(
            published[1]["diagnostics"][0]["range"]["start"],
            json!({"line": 0, "character": 18})
        );
        // Document being typed: XML errors, no DTD error.
        assert!(
            !codes(&published[2])
                .iter()
                .any(|code| code.starts_with("dtd")),
            "{published:?}"
        );

        // Completion: children allowed by the content model.
        let completion = client.request(
            3,
            COMPLETION_METHOD,
            json!({
                "textDocument": {"uri": typing_uri},
                "position": {"line": 0, "character": typing.len()},
            }),
        );
        let items = completion["items"].as_array().unwrap();
        let to = items
            .iter()
            .find(|item| item["label"] == "to")
            .expect("`to` should be proposed");
        assert_eq!(to["detail"], "<!ELEMENT to (#PCDATA)>");
        assert!(!items.iter().any(|item| item["label"] == "body"));
        // Completion in the internal subset.
        let completion = client.request(
            4,
            COMPLETION_METHOD,
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 4},
            }),
        );
        let labels = completion["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["label"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["ELEMENT", "ATTLIST", "ENTITY", "NOTATION"]);

        // Hover: declaration, attributes and comment of the external DTD.
        let hover = client.request(
            5,
            HOVER_METHOD,
            json!({"textDocument": {"uri": uri}, "position": {"line": 3, "character": 2}}),
        );
        let markdown = hover["contents"]["value"].as_str().unwrap();
        assert!(
            markdown.contains("<!ELEMENT note (to, body?)>"),
            "{markdown}"
        );
        assert!(
            markdown.contains("<!ATTLIST note lang (fr | en) #REQUIRED>"),
            "{markdown}"
        );
        assert!(markdown.contains("Root note."), "{markdown}");
        assert_eq!(
            hover["range"],
            json!({"start": {"line": 3, "character": 1}, "end": {"line": 3, "character": 5}})
        );

        // Definition: entity of the internal subset, element of the DTD.
        let definition = client.request(
            6,
            DEFINITION_METHOD,
            json!({"textDocument": {"uri": uri}, "position": {"line": 4, "character": 11}}),
        );
        assert_eq!(
            definition,
            json!([{"uri": uri, "range": {"start": {"line": 1, "character": 11}, "end": {"line": 1, "character": 14}}}])
        );
        let definition = client.request(
            7,
            DEFINITION_METHOD,
            json!({"textDocument": {"uri": uri}, "position": {"line": 4, "character": 3}}),
        );
        assert_eq!(definition[0]["uri"], path_to_uri(&dtd_path));
        assert_eq!(
            definition[0]["range"]["start"],
            json!({"line": 2, "character": 10})
        );

        // Fixes: enumerated value and entity to declare (CRLF preserved).
        let actions = client.request(
            8,
            CODE_ACTION_METHOD,
            json!({
                "textDocument": {"uri": uri},
                "range": {"start": {"line": 3, "character": 12}, "end": {"line": 4, "character": 20}},
                "context": {"diagnostics": published[0]["diagnostics"].clone()},
            }),
        );
        let titles = actions
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action["title"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        for expected in [
            "Declare the entity '&unknown;' in the DOCTYPE",
            "Replace with 'fr'",
            "Replace with 'en'",
        ] {
            assert!(titles.iter().any(|title| title == expected), "{titles:?}");
        }
        let declare = actions
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["title"] == "Declare the entity '&unknown;' in the DOCTYPE")
            .unwrap();
        assert_eq!(
            declare["edit"]["changes"][&uri][0],
            json!({
                "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 0}},
                "newText": "  <!ENTITY unknown \"\">\r\n",
            })
        );
        assert_eq!(declare["diagnostics"][0]["data"]["kind"], "undefinedEntity");

        // Symbols and formatting of a DTD file.
        let symbols = client.request(
            9,
            SYMBOL_METHOD,
            json!({"textDocument": {"uri": broken_uri}}),
        );
        let names = symbols
            .as_array()
            .unwrap()
            .iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["a", "b"]);
        let formatting = client.request(
            10,
            FORMATTING_METHOD,
            json!({"textDocument": {"uri": broken_uri}, "options": {"tabSize": 2, "insertSpaces": true}}),
        );
        assert_eq!(formatting, json!([]));

        // Fixing the document: no diagnostics left.
        client.notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": "<!DOCTYPE note SYSTEM \"note.dtd\" [\r\n  <!ENTITY sig \"Alice\">\r\n]>\r\n<note lang=\"fr\">\r\n  <to>Bob &sig;</to>\r\n</note>"}],
            }),
        );
        let published = client.take_diagnostics(11);
        assert_eq!(published.len(), 1);
        assert!(codes(&published[0]).is_empty(), "{published:?}");

        client.request(12, "shutdown", json!(null));
        client.notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn serves_document_links_and_link_definitions() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-document-links {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("schémas")).expect("directory should be created");
        let schema_path = directory.join("schémas").join("a b.xsd");
        std::fs::write(
            &schema_path,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"/></xs:schema>"#,
        )
        .expect("schema should be written");
        let stylesheet_path = directory.join("style.xsl");
        std::fs::write(&stylesheet_path, "<xsl:stylesheet/>")
            .expect("stylesheet should be written");
        let uri = path_to_uri(&directory.join("document.xml"));
        let source = "<?xml-stylesheet type=\"text/xsl\" href=\"style.xsl\"?>\r\n\
<root xmlns:i=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n      \
i:schemaLocation=\"urn:😀 sch%C3%A9mas/a%20b.xsd urn:x https://example.com/x.xsd\"\r\n      \
i:noNamespaceSchemaLocation=\"missing.xsd\">\r\n  \
<child/>\r\n\
</root>";

        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let request = |id: i32, method: &str, params: Value| {
            client
                .sender
                .send(
                    Request {
                        id: RequestId::from(id),
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("request should be sent");
            loop {
                match client.receiver.recv().expect("a message should arrive") {
                    Message::Response(response) => {
                        assert_eq!(response.id, RequestId::from(id));
                        return response.result;
                    }
                    Message::Notification(_) => {}
                    message => panic!("unexpected message {message:?}"),
                }
            }
        };
        let notify = |method: &str, params: Value| {
            client
                .sender
                .send(
                    Notification {
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("notification should be sent");
        };
        let range = |start: (u32, u32), end: (u32, u32)| {
            json!({
                "start": {"line": start.0, "character": start.1},
                "end": {"line": end.0, "character": end.1},
            })
        };
        let zero = range((0, 0), (0, 0));

        let initialize = request(
            1,
            INITIALIZE_METHOD,
            json!({"capabilities": {"textDocument": {"definition": {"linkSupport": true}}}}),
        );
        assert_eq!(
            initialize.unwrap()["capabilities"]["documentLinkProvider"],
            json!({"resolveProvider": false})
        );
        notify("initialized", json!({}));
        notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "text": source}}),
        );

        // Value at column 24; "urn:😀 ": the emoji counts as two UTF-16 code units.
        let schema = 24 + 7;
        let schema_range = range((2, schema), (2, schema + 22));
        assert_eq!(
            request(
                2,
                DOCUMENT_LINK_METHOD,
                json!({"textDocument": {"uri": uri}})
            ),
            Some(json!([
                {
                    "range": range((0, 39), (0, 48)),
                    "target": path_to_uri(&stylesheet_path),
                    "tooltip": format!("Open stylesheet: {}", stylesheet_path.display()),
                },
                {
                    "range": schema_range,
                    "target": path_to_uri(&schema_path),
                    "tooltip": format!("Open XSD schema: {}", schema_path.display()),
                },
                {
                    "range": range((2, schema + 29), (2, schema + 54)),
                    "target": "https://example.com/x.xsd",
                    "tooltip": "Open XSD schema: https://example.com/x.xsd",
                },
            ]))
        );

        let definition = |id: i32, line: u32, character: u32| {
            request(
                id,
                DEFINITION_METHOD,
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": line, "character": character},
                }),
            )
        };
        assert_eq!(
            definition(3, 2, schema + 4),
            Some(json!([{
                "originSelectionRange": schema_range,
                "targetUri": path_to_uri(&schema_path),
                "targetRange": zero,
                "targetSelectionRange": zero,
            }]))
        );
        assert_eq!(
            definition(4, 0, 42).unwrap()[0]["targetUri"],
            path_to_uri(&stylesheet_path)
        );
        // URL and missing file: no location (the URL remains a link).
        assert_eq!(definition(5, 2, schema + 30), Some(json!([])));
        assert_eq!(definition(6, 3, 36), Some(json!([])));
        // Outside link values, element definition is kept.
        let element = definition(7, 1, 5).expect("element definition should be found");
        assert_eq!(element[0]["uri"], uri);
        assert_eq!(element[0]["range"], range((1, 0), (1, 5)));

        assert_eq!(
            request(
                8,
                DOCUMENT_LINK_METHOD,
                json!({"textDocument": {"uri": "file:///missing.xml"}})
            ),
            Some(json!([]))
        );

        assert_eq!(request(9, "shutdown", json!(null)), Some(Value::Null));
        notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
        std::fs::remove_dir_all(directory).expect("directory should be removed");
    }

    #[test]
    fn serves_document_and_range_formatting_with_options() {
        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let request = |id: i32, method: &str, params: Value| {
            client
                .sender
                .send(
                    Request {
                        id: RequestId::from(id),
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("request should be sent");
            loop {
                match client.receiver.recv().expect("a message should arrive") {
                    Message::Response(response) => {
                        assert_eq!(response.id, RequestId::from(id));
                        return response.result.expect("a result should be returned");
                    }
                    Message::Notification(_) => {}
                    message => panic!("unexpected message {message:?}"),
                }
            }
        };
        let notify = |method: &str, params: Value| {
            client
                .sender
                .send(
                    Notification {
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("notification should be sent");
        };
        let open = |uri: &str, text: &str| {
            notify(
                DID_OPEN_METHOD,
                json!({"textDocument": {"uri": uri, "text": text}}),
            );
        };
        let range = |start: (u32, u32), end: (u32, u32)| {
            json!({
                "start": {"line": start.0, "character": start.1},
                "end": {"line": end.0, "character": end.1},
            })
        };

        let initialize = request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
        assert_eq!(
            initialize["capabilities"]["documentRangeFormattingProvider"],
            true
        );
        notify("initialized", json!({}));

        // Whole formatting: four spaces, CRLF, UTF-16, trailing whitespace.
        let uri = "file:///crlf.xml";
        let source = "<root>\r\n<outer><😀 a=\"1\"><b/></😀></outer>\r\n<c>t  \r\n</c>\r\n</root>";
        open(uri, source);
        let options = json!({
            "tabSize": 4,
            "insertSpaces": true,
            "trimTrailingWhitespace": true,
            "insertFinalNewline": true,
        });
        let edits = request(
            2,
            FORMATTING_METHOD,
            json!({"textDocument": {"uri": uri}, "options": options}),
        );
        assert!(edits.as_array().unwrap().len() > 1, "{edits}");
        assert!(
            edits
                .as_array()
                .unwrap()
                .iter()
                .all(|edit| edit["range"]["start"]["line"] != 0),
            "the unchanged first line must not be edited: {edits}"
        );
        let formatted = formatting::apply_edits(source, &edits);
        assert_eq!(
            formatted,
            "<root>\r\n    <outer>\r\n        <😀 a=\"1\">\r\n            <b/>\r\n        </😀>\r\n    </outer>\r\n    <c>t\r\n</c>\r\n</root>\r\n"
        );
        notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": formatted}],
            }),
        );
        assert_eq!(
            request(
                3,
                FORMATTING_METHOD,
                json!({"textDocument": {"uri": uri}, "options": options}),
            ),
            json!([]),
            "formatting twice must not change anything"
        );
        // Range on an already formatted document: no change.
        assert_eq!(
            request(
                4,
                RANGE_FORMATTING_METHOD,
                json!({
                    "textDocument": {"uri": uri},
                    "range": range((2, 9), (4, 3)),
                    "options": options,
                }),
            ),
            json!([])
        );

        // Range formatting with tabs in a document that is invalid outside
        // the range.
        let uri = "file:///malformed.xml";
        let source = "<root>\n<broken>\n  <outer><é><b/></é></outer>\n<oops></root>\n";
        open(uri, source);
        let tabs = json!({"tabSize": 4, "insertSpaces": false});
        let format_range = |id: i32, selection: Value| {
            let edits = request(
                id,
                RANGE_FORMATTING_METHOD,
                json!({
                    "textDocument": {"uri": uri},
                    "range": selection,
                    "options": tabs,
                }),
            );
            formatting::apply_edits(source, &edits)
        };
        // Nested element: `<b/>` starts at UTF-16 column 12.
        assert_eq!(
            format_range(5, range((2, 12), (2, 16))),
            "<root>\n<broken>\n  <outer><é>\n\t\t\t<b/>\n\t\t</é></outer>\n<oops></root>\n"
        );
        // Range starting in `<outer` and ending in `</é>`: expanded to the
        // whole `outer` element.
        let expanded = format_range(6, range((2, 4), (2, 17)));
        assert_eq!(
            expanded,
            "<root>\n<broken>\n\t<outer>\n\t\t<é>\n\t\t\t<b/>\n\t\t</é>\n\t</outer>\n<oops></root>\n"
        );
        // The invalid region is never modified.
        assert_eq!(format_range(7, range((3, 0), (3, 6))), source);
        // Range formatting is idempotent.
        notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": expanded}],
            }),
        );
        assert_eq!(
            request(
                8,
                RANGE_FORMATTING_METHOD,
                json!({
                    "textDocument": {"uri": uri},
                    "range": range((2, 1), (6, 3)),
                    "options": tabs,
                }),
            ),
            json!([])
        );
        // Whole formatting refuses the invalid document.
        assert_eq!(
            request(
                9,
                FORMATTING_METHOD,
                json!({"textDocument": {"uri": uri}, "options": tabs}),
            ),
            json!([])
        );

        assert_eq!(request(10, "shutdown", json!(null)), Value::Null);
        notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
    }

    #[test]
    fn serves_prepare_rename_and_rename_requests() {
        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let request = |id: i32, method: &str, params: Value| {
            client
                .sender
                .send(
                    Request {
                        id: RequestId::from(id),
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("request should be sent");
            loop {
                match client.receiver.recv().expect("a message should arrive") {
                    Message::Response(response) => {
                        assert_eq!(response.id, RequestId::from(id));
                        return response;
                    }
                    Message::Notification(_) => {}
                    message => panic!("unexpected message {message:?}"),
                }
            }
        };
        let notify = |method: &str, params: Value| {
            client
                .sender
                .send(
                    Notification {
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("notification should be sent");
        };
        let range = |line: u32, start: u32, end: u32| json!({"start": {"line": line, "character": start}, "end": {"line": line, "character": end}});

        let initialize = request(1, INITIALIZE_METHOD, json!({})).result.unwrap();
        assert_eq!(
            initialize["capabilities"]["renameProvider"],
            json!({"prepareProvider": true})
        );
        notify("initialized", json!({}));
        notify(
            DID_OPEN_METHOD,
            json!({
                "textDocument": {
                    "uri": "file:///document.xml",
                    "text": "<ns:root xmlns:ns=\"urn:x\">\r\n  <ns:é😀 a=\"1\"/>\r\n  <item>text</item>\r\n</ns:root>",
                }
            }),
        );
        let params = |line: u32, character: u32| {
            json!({
                "textDocument": {"uri": "file:///document.xml"},
                "position": {"line": line, "character": character},
            })
        };
        let rename_params = |line: u32, character: u32, new_name: &str| {
            let mut params = params(line, character);
            params["newName"] = json!(new_name);
            params
        };

        assert_eq!(
            request(2, PREPARE_RENAME_METHOD, params(2, 16)).result,
            Some(json!({"range": range(2, 14, 18), "placeholder": "item"}))
        );
        assert_eq!(
            request(3, RENAME_METHOD, rename_params(2, 16, "entry")).result,
            Some(json!({"changes": {"file:///document.xml": [
                {"range": range(2, 3, 7), "newText": "entry"},
                {"range": range(2, 14, 18), "newText": "entry"},
            ]}}))
        );
        // Prefix: declaration and uses, UTF-16 positions.
        assert_eq!(
            request(4, PREPARE_RENAME_METHOD, params(1, 4)).result,
            Some(json!({"range": range(1, 3, 5), "placeholder": "ns"}))
        );
        assert_eq!(
            request(5, RENAME_METHOD, rename_params(0, 16, "p")).result,
            Some(json!({"changes": {"file:///document.xml": [
                {"range": range(0, 1, 3), "newText": "p"},
                {"range": range(0, 15, 17), "newText": "p"},
                {"range": range(1, 3, 5), "newText": "p"},
                {"range": range(3, 2, 4), "newText": "p"},
            ]}}))
        );
        assert_eq!(
            request(6, PREPARE_RENAME_METHOD, params(1, 8)).result,
            Some(json!({"range": range(1, 3, 9), "placeholder": "ns:é😀"}))
        );
        // Content: nothing to rename.
        assert_eq!(
            request(7, PREPARE_RENAME_METHOD, params(2, 10)).result,
            Some(Value::Null)
        );
        assert_eq!(
            request(8, RENAME_METHOD, rename_params(2, 10, "x")).result,
            Some(Value::Null)
        );
        // Invalid name: InvalidParams error.
        let invalid = request(9, RENAME_METHOD, rename_params(2, 16, "1 bad"));
        let error = invalid.error.expect("an error should be returned");
        assert_eq!(error.code, rename::INVALID_PARAMS);
        assert!(error.message.contains("1 bad"));
        assert!(
            request(10, RENAME_METHOD, params(2, 16))
                .error
                .is_some_and(|error| error.code == rename::INVALID_PARAMS)
        );
        assert_eq!(
            request(
                11,
                RENAME_METHOD,
                json!({
                    "textDocument": {"uri": "file:///missing.xml"},
                    "position": {"line": 0, "character": 1},
                    "newName": "x",
                })
            )
            .result,
            Some(Value::Null)
        );

        assert_eq!(
            request(12, "shutdown", json!(null)).result,
            Some(Value::Null)
        );
        notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
    }

    #[test]
    fn renames_xsd_components_across_open_instance_documents() {
        let schema_path =
            std::env::temp_dir().join(format!("xml-lsp-rename-{}.xsd", std::process::id()));
        let schema_source = concat!(
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">",
            "<xs:element name=\"root\"><xs:complexType><xs:sequence>",
            "<xs:element ref=\"item\"/></xs:sequence></xs:complexType></xs:element>",
            "<xs:element name=\"item\" type=\"xs:string\"/>",
            "</xs:schema>"
        );
        std::fs::write(&schema_path, schema_source).expect("schema should be written");
        let schema_uri = path_to_uri(&schema_path);
        let schema_name = schema_path.file_name().unwrap().to_string_lossy();
        let bound_uri = path_to_uri(&schema_path.with_file_name("rename-bound.xml"));
        let bound = format!(
            "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{schema_name}\"><item>a</item><item/></root>"
        );
        let unrelated_uri = path_to_uri(&schema_path.with_file_name("rename-unrelated.xml"));
        let mut server = XmlLanguageServer::new();
        server
            .documents
            .insert(schema_uri.clone(), schema_source.to_owned());
        server.documents.insert(bound_uri.clone(), bound.clone());
        server
            .documents
            .insert(unrelated_uri, "<root><item/></root>".to_owned());

        let offset = schema_source.find("\"item\" type").unwrap() + 1;
        let character = position_at(schema_source, offset)["character"].clone();
        let edit = server
            .rename(&json!({
                "textDocument": {"uri": schema_uri},
                "position": {"line": 0, "character": character},
                "newName": "entry",
            }))
            .expect("rename should succeed")
            .expect("an edit should be returned");
        let changes = edit["changes"].as_object().unwrap();
        assert_eq!(changes.len(), 2, "{edit}");
        assert_eq!(changes[&schema_uri].as_array().unwrap().len(), 2);
        let bound_edits = changes[&bound_uri].as_array().unwrap();
        assert_eq!(bound_edits.len(), 3);
        let item = bound.find("<item>").unwrap() + 1;
        assert_eq!(bound_edits[0]["range"]["start"], position_at(&bound, item));
        assert_eq!(bound_edits[0]["newText"], "entry");

        std::fs::remove_file(schema_path).expect("schema should be removed");
    }

    #[test]
    fn serves_initialize_diagnostics_shutdown_and_exit() {
        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));

        client
            .sender
            .send(
                Request {
                    id: RequestId::from(1),
                    method: INITIALIZE_METHOD.to_owned(),
                    params: json!({}),
                }
                .into(),
            )
            .expect("initialize should be sent");

        let initialize_response = client
            .receiver
            .recv()
            .expect("initialize response should be received");
        match initialize_response {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(1));
                assert_eq!(
                    response.result,
                    Some(json!({
                        "capabilities": {
                            "positionEncoding": "utf-16",
                            "textDocumentSync": {"openClose": true, "change": 2},
                            "completionProvider": {"triggerCharacters": ["<", " ", "/", ">", "=", "\"", "?", "&", "%"]},
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
                            "codeActionProvider": {"codeActionKinds": ["quickfix", "refactor", "source"]},
                            "workspaceSymbolProvider": true,
                            "colorProvider": true,
                            "workspace": {"workspaceFolders": {"supported": true, "changeNotifications": true}},
                        },
                        "serverInfo": {
                            "name": "xml-lsp",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                    }))
                );
            }
            message => panic!("expected initialize response, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: "initialized".to_owned(),
                    params: json!({}),
                }
                .into(),
            )
            .expect("initialized should be sent");

        client
            .sender
            .send(
                Notification {
                    method: DID_OPEN_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {
                            "uri": "file:///document.xml",
                            "text": "<root>",
                        }
                    }),
                }
                .into(),
            )
            .expect("didOpen should be sent");

        let diagnostics_notification = client
            .receiver
            .recv()
            .expect("diagnostics should be published");
        match diagnostics_notification {
            Message::Notification(notification) => {
                assert_eq!(notification.method, PUBLISH_DIAGNOSTICS_METHOD);
                // Unclosed element: reported on the name of its start tag.
                assert_eq!(
                    notification.params["diagnostics"][0]["range"],
                    json!({
                        "start": {"line": 0, "character": 1},
                        "end": {"line": 0, "character": 5},
                    })
                );
                assert_eq!(
                    notification.params["diagnostics"][0]["data"],
                    json!({"category": "xml", "kind": "unclosedElement"})
                );
                assert_eq!(notification.params["diagnostics"][0]["severity"], 1);
                assert_eq!(
                    notification.params["diagnostics"][0]["code"],
                    "xml-structure"
                );
            }
            message => panic!("expected diagnostics notification, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: DID_CHANGE_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml", "version": 2},
                        "contentChanges": [{"text": "<root />"}],
                    }),
                }
                .into(),
            )
            .expect("didChange should be sent");

        let clean_notification = client
            .receiver
            .recv()
            .expect("clean diagnostics should be published");
        match clean_notification {
            Message::Notification(notification) => {
                assert_eq!(notification.method, PUBLISH_DIAGNOSTICS_METHOD);
                assert_eq!(notification.params["diagnostics"], json!([]));
            }
            message => panic!("expected clean diagnostics notification, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: DID_CHANGE_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml", "version": 3},
                        "contentChanges": [{"text": "<root><item /></root><it"}],
                    }),
                }
                .into(),
            )
            .expect("completion source should be sent");
        client
            .receiver
            .recv()
            .expect("diagnostics for completion source should be published");

        client
            .sender
            .send(
                Request {
                    id: RequestId::from(4),
                    method: COMPLETION_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml"},
                        "position": {"line": 0, "character": 24},
                    }),
                }
                .into(),
            )
            .expect("completion should be sent");

        let completion_response = client
            .receiver
            .recv()
            .expect("completion response should be received");
        match completion_response {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(4));
                assert_eq!(
                    response.result,
                    Some(json!({
                        "isIncomplete": false,
                        "items": [{"label": "item", "insertText": "item"}],
                    }))
                );
            }
            message => panic!("expected completion response, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: DID_CHANGE_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml", "version": 4},
                        "contentChanges": [{"text": "<root />"}],
                    }),
                }
                .into(),
            )
            .expect("formatting source should be sent");
        client
            .receiver
            .recv()
            .expect("diagnostics for formatting source should be published");

        client
            .sender
            .send(
                Request {
                    id: RequestId::from(3),
                    method: FORMATTING_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml"},
                        "options": {"tabSize": 2, "insertSpaces": true},
                    }),
                }
                .into(),
            )
            .expect("formatting should be sent");

        let formatting_response = client
            .receiver
            .recv()
            .expect("formatting response should be received");
        match formatting_response {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(3));
                assert_eq!(
                    response.result,
                    Some(json!([{
                        "range": {
                            "start": {"line": 0, "character": 8},
                            "end": {"line": 0, "character": 8},
                        },
                        "newText": "\n",
                    }]))
                );
            }
            message => panic!("expected formatting response, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: DID_CHANGE_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml", "version": 5},
                        "contentChanges": [{"text": "<root>"}],
                    }),
                }
                .into(),
            )
            .expect("auto-close source should be sent");
        client
            .receiver
            .recv()
            .expect("diagnostics for auto-close source should be published");

        client
            .sender
            .send(
                Request {
                    id: RequestId::from(5),
                    method: COMPLETION_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml"},
                        "position": {"line": 0, "character": 6},
                    }),
                }
                .into(),
            )
            .expect("auto-close completion should be sent");

        let auto_close_response = client
            .receiver
            .recv()
            .expect("auto-close response should be received");
        match auto_close_response {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(5));
                assert_eq!(
                    response.result,
                    Some(json!({
                        "isIncomplete": false,
                        "items": [{"label": "</root>", "insertText": "</root>"}],
                    }))
                );
            }
            message => panic!("expected auto-close response, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: DID_CLOSE_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml"},
                    }),
                }
                .into(),
            )
            .expect("didClose should be sent");

        let close_notification = client
            .receiver
            .recv()
            .expect("clear diagnostics should be published on close");
        match close_notification {
            Message::Notification(notification) => {
                assert_eq!(notification.method, PUBLISH_DIAGNOSTICS_METHOD);
                assert_eq!(notification.params["diagnostics"], json!([]));
            }
            message => panic!("expected close diagnostics notification, got {message:?}"),
        }

        client
            .sender
            .send(
                Request {
                    id: RequestId::from(2),
                    method: "shutdown".to_owned(),
                    params: json!(null),
                }
                .into(),
            )
            .expect("shutdown should be sent");

        let shutdown_response = client
            .receiver
            .recv()
            .expect("shutdown response should be received");
        match shutdown_response {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(2));
                assert_eq!(response.result, Some(json!(null)));
            }
            message => panic!("expected shutdown response, got {message:?}"),
        }

        // After `shutdown`, requests are rejected and notifications ignored.
        client
            .sender
            .send(
                Request {
                    id: RequestId::from(6),
                    method: HOVER_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml"},
                        "position": {"line": 0, "character": 1},
                    }),
                }
                .into(),
            )
            .expect("request after shutdown should be sent");
        match client.receiver.recv().expect("an error should be received") {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(6));
                assert_eq!(
                    response.error.map(|error| error.code),
                    Some(ErrorCode::InvalidRequest as i32)
                );
            }
            message => panic!("expected an error response, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: EXIT_METHOD.to_owned(),
                    params: json!(null),
                }
                .into(),
            )
            .expect("exit should be sent");

        let exit_code = server_thread.join().expect("server thread should stop");
        assert_eq!(exit_code, 0, "exit after shutdown is a clean exit");
    }

    #[test]
    fn applies_incremental_lsp_changes() {
        let updated = XmlLanguageServer::changed_document(
            &json!({
                "textDocument": {"uri": "file:///document.xml"},
                "contentChanges": [{
                    "range": {
                        "start": {"line": 0, "character": 6},
                        "end": {"line": 0, "character": 11}
                    },
                    "text": "item"
                }]
            }),
            Some(&"<root>child</root>".to_owned()),
        )
        .unwrap();

        assert_eq!(updated.1, "<root>item</root>");
    }

    #[test]
    fn applies_all_incremental_changes_in_one_notification() {
        let updated = XmlLanguageServer::changed_document(
            &json!({
                "textDocument": {"uri": "file:///document.xml"},
                "contentChanges": [
                    {
                        "range": {
                            "start": {"line": 0, "character": 6},
                            "end": {"line": 0, "character": 11}
                        },
                        "text": "thing"
                    },
                    {
                        "range": {
                            "start": {"line": 0, "character": 12},
                            "end": {"line": 0, "character": 17}
                        },
                        "text": "entry"
                    }
                ]
            }),
            Some(&"<root>child value</root>".to_owned()),
        )
        .unwrap();

        assert_eq!(updated.1, "<root>thing entry</root>");
    }

    #[test]
    fn finds_local_xml_element_references() {
        let mut server = XmlLanguageServer::new();
        server.documents.insert(
            "file:///document.xml".to_owned(),
            "<root><child /><child /></root>".to_owned(),
        );
        let references = server
            .references(&json!({
                "textDocument": {"uri": "file:///document.xml"},
                "position": {"line": 0, "character": 12},
            }))
            .unwrap();

        assert_eq!(references.as_array().unwrap().len(), 2);
    }

    #[test]
    fn resolves_local_xml_element_definitions() {
        let mut server = XmlLanguageServer::new();
        server.documents.insert(
            "file:///document.xml".to_owned(),
            "<root><child /></root>".to_owned(),
        );
        let definition = server
            .definition(&json!({
                "textDocument": {"uri": "file:///document.xml"},
                "position": {"line": 0, "character": 12},
            }))
            .unwrap();

        assert_eq!(definition[0]["uri"], "file:///document.xml");
        assert_eq!(
            definition[0]["range"]["start"],
            json!({"line": 0, "character": 6})
        );
    }

    #[test]
    fn resolves_xsd_element_definitions() {
        let schema_path =
            std::env::temp_dir().join(format!("xml-lsp-definition-{}.xsd", std::process::id()));
        let schema_source = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"/><xs:attribute name="root"/></xs:schema>"#;
        std::fs::write(&schema_path, schema_source).expect("schema should be written");

        let document_path = schema_path.with_file_name("definition.xml");
        let uri = path_to_uri(&document_path);
        let schema_name = schema_path.file_name().unwrap().to_string_lossy();
        let source = format!(
            "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{schema_name}\" />"
        );
        let mut server = XmlLanguageServer::new();
        server.documents.insert(uri.clone(), source);
        let definition = server
            .definition(&json!({
                "textDocument": {"uri": uri},
                "position": {"line": 0, "character": 5},
            }))
            .expect("XSD definition should be found");

        assert_eq!(definition[0]["uri"], path_to_uri(&schema_path));
        let expected_offset = schema_source.find("name=\"root\"").unwrap() + 6;
        assert_eq!(
            definition[0]["range"]["start"],
            position_at(schema_source, expected_offset)
        );
        assert_eq!(
            definition[0]["range"]["end"],
            position_at(schema_source, expected_offset + "root".len())
        );

        std::fs::remove_file(schema_path).expect("schema should be removed");
    }

    #[test]
    fn finds_xsd_and_xml_element_references() {
        let schema_path =
            std::env::temp_dir().join(format!("xml-lsp-references-{}.xsd", std::process::id()));
        let schema_source = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"/></xs:schema>"#;
        std::fs::write(&schema_path, schema_source).expect("schema should be written");
        let uri = path_to_uri(&schema_path.with_file_name("references.xml"));
        let schema_name = schema_path.file_name().unwrap().to_string_lossy();
        let source = format!(
            "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{schema_name}\"><root /></root>"
        );
        let mut server = XmlLanguageServer::new();
        server.documents.insert(uri.clone(), source);
        let references = server
            .references(&json!({
                "textDocument": {"uri": uri},
                "position": {"line": 0, "character": 5},
            }))
            .expect("references should be returned");
        assert_eq!(references.as_array().unwrap().len(), 3);
        assert_eq!(references[0]["uri"], path_to_uri(&schema_path));
        assert_eq!(references[1]["uri"], uri);
        assert_eq!(references[2]["uri"], uri);
        std::fs::remove_file(schema_path).expect("schema should be removed");
    }

    #[test]
    fn serves_xsd_documentation_hover_requests() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-hover-lsp-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("directory should be written");
        let schema_path = directory.join("order.xsd");
        let schema_source = concat!(
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:o=\"urn:order\" targetNamespace=\"urn:order\" elementFormDefault=\"qualified\">\n",
            "  <xs:element name=\"order\"><xs:annotation><xs:documentation>A customer order.</xs:documentation></xs:annotation>\n",
            "    <xs:complexType><xs:sequence>\n",
            "      <xs:element name=\"status\" type=\"o:Status\"><xs:annotation><xs:documentation>Order status.</xs:documentation></xs:annotation></xs:element>\n",
            "    </xs:sequence>\n",
            "    <xs:attribute name=\"priority\" type=\"xs:int\" use=\"required\"><xs:annotation><xs:documentation>Priority level.</xs:documentation></xs:annotation></xs:attribute>\n",
            "    </xs:complexType>\n",
            "  </xs:element>\n",
            "  <xs:simpleType name=\"Status\"><xs:restriction base=\"xs:string\">\n",
            "    <xs:enumeration value=\"open\"><xs:annotation><xs:documentation>Not shipped yet.</xs:documentation></xs:annotation></xs:enumeration>\n",
            "    <xs:enumeration value=\"closed\"/>\n",
            "  </xs:restriction></xs:simpleType>\n",
            "</xs:schema>",
        );
        std::fs::write(&schema_path, schema_source).expect("schema should be written");
        let document_uri = path_to_uri(&directory.join("order.xml"));
        let schema_uri = path_to_uri(&schema_path);

        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let request = |id: i32, method: &str, params: Value| {
            client
                .sender
                .send(
                    Request {
                        id: RequestId::from(id),
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("request should be sent");
            loop {
                match client.receiver.recv().expect("a message should arrive") {
                    Message::Response(response) => {
                        assert_eq!(response.id, RequestId::from(id));
                        assert!(response.error.is_none(), "{:?}", response.error);
                        return response.result;
                    }
                    Message::Notification(_) => {}
                    message => panic!("unexpected message {message:?}"),
                }
            }
        };
        let notify = |method: &str, params: Value| {
            client
                .sender
                .send(
                    Notification {
                        method: method.to_owned(),
                        params,
                    }
                    .into(),
                )
                .expect("notification should be sent");
        };

        let initialize = request(1, INITIALIZE_METHOD, json!({}));
        assert_eq!(initialize.unwrap()["capabilities"]["hoverProvider"], true);
        notify("initialized", json!({}));
        let text = "<order xmlns=\"urn:order\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n       xsi:schemaLocation=\"urn:order order.xsd\" priority=\"1\">\r\n  <status>open</status>\r\n</order>";
        notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": document_uri, "text": text}}),
        );
        notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": "file:///plain.xml", "text": "<note/>"}}),
        );
        let params = |uri: &str, line: u32, character: u32| {
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            })
        };

        assert_eq!(
            request(2, HOVER_METHOD, params(&document_uri, 3, 4)),
            Some(json!({
                "contents": {
                    "kind": "markdown",
                    "value": format!("**Element** `<order>`\n\n- Namespace: `urn:order`\n- Type: anonymous complex\n\nA customer order.\n\nSource: [order.xsd]({schema_uri})"),
                },
                "range": {"start": {"line": 3, "character": 2}, "end": {"line": 3, "character": 7}},
            }))
        );
        let status = request(3, HOVER_METHOD, params(&document_uri, 2, 4)).unwrap();
        assert_eq!(
            status["range"],
            json!({"start": {"line": 2, "character": 3}, "end": {"line": 2, "character": 9}})
        );
        let status = status["contents"]["value"].as_str().unwrap();
        assert!(status.contains("- Type: `o:Status` (restriction of `xs:string`)"));
        assert!(status.contains("Order status."), "{status}");
        let priority = request(4, HOVER_METHOD, params(&document_uri, 1, 50)).unwrap();
        let priority = priority["contents"]["value"].as_str().unwrap();
        assert!(
            priority.starts_with("**Attribute** `priority`"),
            "{priority}"
        );
        assert!(priority.contains("- Use: required"), "{priority}");
        assert!(priority.contains("Priority level."), "{priority}");
        let value = request(5, HOVER_METHOD, params(&document_uri, 2, 11)).unwrap();
        assert_eq!(
            value["range"],
            json!({"start": {"line": 2, "character": 10}, "end": {"line": 2, "character": 14}})
        );
        let value = value["contents"]["value"].as_str().unwrap();
        assert!(value.contains("Not shipped yet."), "{value}");
        assert!(value.contains("- Allowed values: `open`, `closed`"));
        assert_eq!(
            request(6, HOVER_METHOD, params("file:///plain.xml", 0, 2)),
            Some(json!({
                "contents": {"kind": "markdown", "value": "**Element** `<note>`"},
                "range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 5}},
            }))
        );
        assert_eq!(
            request(7, HOVER_METHOD, params("file:///plain.xml", 0, 0)),
            Some(Value::Null)
        );

        assert_eq!(request(8, "shutdown", json!(null)), Some(Value::Null));
        notify(EXIT_METHOD, json!(null));
        server_thread.join().expect("server thread should stop");
        std::fs::remove_dir_all(directory).expect("directory should be removed");
    }

    #[test]
    fn provides_hover_for_xml_elements() {
        let mut server = XmlLanguageServer::new();
        server.documents.insert(
            "file:///document.xml".to_owned(),
            "<root><child /></root>".to_owned(),
        );
        let hover = server
            .hover(&json!({
                "textDocument": {"uri": "file:///document.xml"},
                "position": {"line": 0, "character": 5},
            }))
            .unwrap();

        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("root")
        );
    }

    #[test]
    fn builds_document_symbols_from_xml_elements() {
        let symbols = xml_symbols("<root><child /></root>");
        let symbols = symbols.as_array().expect("symbols should be an array");

        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0]["name"], "child");
        assert_eq!(symbols[1]["name"], "root");
        assert_eq!(symbols[0]["kind"], 13);
    }

    #[test]
    fn round_trips_file_uris_with_spaces_and_reserved_characters() {
        #[cfg(windows)]
        let path = std::path::PathBuf::from(r"C:\workspace\xml files\schema#1.xsd");
        #[cfg(not(windows))]
        let path = std::path::PathBuf::from("/workspace/xml files/schema#1.xsd");
        let uri = path_to_uri(&path);
        #[cfg(windows)]
        assert_eq!(uri, "file:///C:/workspace/xml%20files/schema%231.xsd");
        #[cfg(not(windows))]
        assert_eq!(uri, "file:///workspace/xml%20files/schema%231.xsd");
        assert_eq!(uri_to_path(&uri), path);
    }

    #[test]
    fn formats_nested_xml_through_lsp_request() {
        let uri = "file:///document.xml";
        let source =
            "<?xml version=\"1.0\" encoding=\"iso-8859-1\" ?><root><outer><inner /></outer></root>";
        let mut server = XmlLanguageServer::new();
        server.documents.insert(uri.to_owned(), source.to_owned());
        let response = server
            .formatting(&json!({"textDocument": {"uri": uri}}))
            .expect("formatting should return a workspace edit");
        assert_eq!(
            formatting::apply_edits(source, &response),
            "<?xml version=\"1.0\" encoding=\"iso-8859-1\" ?>\n<root>\n  <outer>\n    <inner />\n  </outer>\n</root>\n"
        );
    }

    #[test]
    fn converts_unicode_offsets_to_utf16_positions() {
        assert_eq!(
            position_at("é\n😀<root>", 7),
            json!({"line": 1, "character": 2})
        );
    }

    #[test]
    fn converts_positions_in_every_encoding_outside_crlf_line_breaks() {
        let source = "é\r\n😀<r>\r\n";
        let index = selection::LineIndex::new(source);
        // Offset 3 is between `\r` and `\n`: the end of the first line.
        assert_eq!(position_at(source, 3), json!({"line": 0, "character": 1}));
        assert_eq!(index.position(source, 3), position_at(source, 3));
        // A character beyond the line is its end, before the line break.
        assert_eq!(offset_at(source, 0, 40), 2);
        assert_eq!(offset_at(source, 1, 40), 11);
        assert_eq!(offset_at(source, 9, 0), source.len());
        for (encoding, character) in [
            (positions::PositionEncoding::Utf8, 5),
            (positions::PositionEncoding::Utf16, 3),
            (positions::PositionEncoding::Utf32, 2),
        ] {
            // Each thread has its own encoding: this one is left unchanged
            // for the other tests.
            thread::spawn(move || {
                encoding.install();
                let offset = source.find("r>").unwrap();
                let expected = json!({"line": 1, "character": character});
                assert_eq!(position_at(source, offset), expected, "{encoding:?}");
                assert_eq!(
                    selection::LineIndex::new(source).position(source, offset),
                    expected
                );
                assert_eq!(offset_at(source, 1, character), offset, "{encoding:?}");
            })
            .join()
            .expect("conversions should succeed");
        }
    }

    #[test]
    fn locates_xsd_parse_errors() {
        let source = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"></xs:schema>"#;
        let offset = schemas::xsd_parse_error_offset(source);
        assert!(offset > source.find("</xs:schema>").unwrap());
        assert!(offset <= source.len());
    }

    #[test]
    fn publishes_xsd_diagnostics_for_a_referenced_schema() {
        let schema_path =
            std::env::temp_dir().join(format!("xml-lsp-schema-{}.xsd", std::process::id()));
        std::fs::write(
            &schema_path,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:test"><xs:element name="root"><xs:complexType><xs:sequence><xs:element name="child"/></xs:sequence></xs:complexType></xs:element></xs:schema>"#,
        )
        .expect("schema should be written");

        let document_path = schema_path.with_file_name("document.xml");
        let uri = format!(
            "file:///{}",
            document_path.to_string_lossy().replace('\\', "/")
        );
        let source = format!(
            "<wrong xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{}\" />",
            schema_path.file_name().unwrap().to_string_lossy()
        );
        let mut server = XmlLanguageServer::new();
        let diagnostics = server.schema_diagnostics(&uri, &source);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["code"], "xsd-validation");
        assert!(
            diagnostics[0]["message"]
                .as_str()
                .unwrap()
                .contains("<wrong>")
        );
        assert!(
            server
                .schemas
                .index
                .get("urn:test")
                .is_some_and(|paths| paths.contains(&schema_path))
        );

        let completion_source = format!(
            "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{}\"><",
            schema_path.file_name().unwrap().to_string_lossy()
        );
        let mut server = XmlLanguageServer::new();
        server
            .documents
            .insert(uri.clone(), completion_source.clone());
        let completion = server
            .completion(&json!({
                "textDocument": {"uri": uri},
                "position": {"line": 0, "character": completion_source.len()},
            }))
            .expect("completion should be available");
        assert!(
            completion["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == "child")
        );

        std::fs::remove_file(schema_path).expect("schema should be removed");
    }

    /// Starts a server and sends `initialize` (with `params`) and
    /// `initialized`; returns the client, the `initialize` result and the
    /// server thread (its exit code).
    fn start_server(params: Value) -> (TestClient, Value, thread::JoinHandle<i32>) {
        let (server, connection) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
        let client = TestClient {
            connection,
            diagnostics: Default::default(),
        };
        let initialize = client.request(0, INITIALIZE_METHOD, params);
        client.notify("initialized", json!({}));
        (client, initialize, server_thread)
    }

    /// Response (result or error) to the request `id`.
    fn response(client: &TestClient, id: i32) -> Response {
        match client.next() {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(id));
                response
            }
            message => panic!("unexpected message {message:?}"),
        }
    }

    fn send_request(client: &TestClient, id: i32, method: &str, params: Value) {
        client.send(
            Request {
                id: RequestId::from(id),
                method: method.to_owned(),
                params,
            }
            .into(),
        );
    }

    fn error_code(response: &Response) -> Option<i32> {
        response.error.as_ref().map(|error| error.code)
    }

    #[test]
    fn answers_requests_cancelled_while_queued() {
        let (client, _, server_thread) = start_server(json!({}));
        let uri = "file:///cancel.xml";
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "version": 1, "text": "<root/>"}}),
        );
        let hover = json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 2}});
        // The loop is busy while the hover and its cancellation are queued.
        send_request(&client, 1, SLEEP_METHOD, json!({"milliseconds": 300}));
        send_request(&client, 2, HOVER_METHOD, hover.clone());
        send_request(&client, 3, HOVER_METHOD, hover.clone());
        client.notify(dispatch::CANCEL_REQUEST_METHOD, json!({"id": 2}));
        // Unknown requests: ignored.
        client.notify(dispatch::CANCEL_REQUEST_METHOD, json!({"id": 99}));
        assert_eq!(error_code(&response(&client, 1)), None);
        assert_eq!(
            error_code(&response(&client, 2)),
            Some(ErrorCode::RequestCanceled as i32)
        );
        // Already answered: ignored.
        client.notify(dispatch::CANCEL_REQUEST_METHOD, json!({"id": 1}));
        assert_eq!(error_code(&response(&client, 3)), None);
        // String ids are cancelled as well.
        send_request(&client, 4, SLEEP_METHOD, json!({"milliseconds": 300}));
        client.send(
            Request {
                id: RequestId::from("five".to_owned()),
                method: HOVER_METHOD.to_owned(),
                params: hover,
            }
            .into(),
        );
        client.notify(dispatch::CANCEL_REQUEST_METHOD, json!({"id": "five"}));
        assert_eq!(error_code(&response(&client, 4)), None);
        match client.next() {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from("five".to_owned()));
                assert_eq!(
                    error_code(&response),
                    Some(ErrorCode::RequestCanceled as i32)
                );
            }
            message => panic!("unexpected message {message:?}"),
        }
        assert_eq!(client.request(6, "shutdown", Value::Null), Value::Null);
        client.notify(EXIT_METHOD, Value::Null);
        assert_eq!(server_thread.join().expect("server should stop"), 0);
    }

    #[test]
    fn a_panicking_handler_answers_an_internal_error_and_the_server_keeps_running() {
        let (client, _, server_thread) = start_server(json!({}));
        let uri = "file:///panic.xml";
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "version": 1, "text": "<root><a/>"}}),
        );
        send_request(&client, 1, PANIC_METHOD, Value::Null);
        let failed = response(&client, 1);
        assert_eq!(error_code(&failed), Some(ErrorCode::InternalError as i32));
        assert!(
            failed
                .error
                .as_ref()
                .is_some_and(|error| error.message.contains("test panic")),
            "{failed:?}"
        );
        // The server still answers and validates.
        let symbols = client.request(2, SYMBOL_METHOD, json!({"textDocument": {"uri": uri}}));
        assert_eq!(symbols.as_array().map(Vec::len), Some(1));
        let published = client.take_diagnostics(3);
        assert_eq!(
            codes(published.last().expect("diagnostics")),
            ["xml-structure"]
        );
        // Unknown requests are still `MethodNotFound`.
        send_request(&client, 4, "xml/unknown", Value::Null);
        assert_eq!(
            error_code(&response(&client, 4)),
            Some(ErrorCode::MethodNotFound as i32)
        );
        assert_eq!(client.request(5, "shutdown", Value::Null), Value::Null);
        client.notify(EXIT_METHOD, Value::Null);
        assert_eq!(server_thread.join().expect("server should stop"), 0);
    }

    #[test]
    fn exit_without_shutdown_ends_with_code_1() {
        let (client, _, server_thread) = start_server(json!({}));
        client.notify(EXIT_METHOD, Value::Null);
        assert_eq!(server_thread.join().expect("server should stop"), 1);

        // A client that goes away without `exit`.
        let (client, _, server_thread) = start_server(json!({}));
        drop(client);
        assert_eq!(server_thread.join().expect("server should stop"), 1);
    }

    #[test]
    fn negotiates_utf8_positions() {
        let (client, initialize, server_thread) = start_server(json!({
            "capabilities": {"general": {"positionEncodings": ["utf-16", "utf-8"]}},
        }));
        assert_eq!(initialize["capabilities"]["positionEncoding"], "utf-8");
        let uri = "file:///utf8.xml";
        // `é` is 2 bytes, `𝄞` 4 bytes: UTF-8 columns differ from UTF-16.
        let text = "<é𝄞 a=\"1\">\r\n  <b></c>\r\n</é𝄞>";
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "version": 1, "text": text}}),
        );
        let published = client.take_diagnostics(1);
        let diagnostic = &published.last().expect("diagnostics")["diagnostics"][0];
        assert_eq!(
            diagnostic["range"]["start"],
            json!({"line": 1, "character": 7})
        );
        let highlights = client.request(
            2,
            DOCUMENT_HIGHLIGHT_METHOD,
            json!({"textDocument": {"uri": uri}, "position": {"line": 2, "character": 3}}),
        );
        assert_eq!(
            highlights,
            json!([
                {"range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 7}}, "kind": 2},
                {"range": {"start": {"line": 2, "character": 2}, "end": {"line": 2, "character": 8}}, "kind": 2},
            ])
        );
        // Incremental change in UTF-8 columns: `</c>` becomes `</b>`.
        client.notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{
                    "range": {"start": {"line": 1, "character": 7}, "end": {"line": 1, "character": 8}},
                    "text": "b",
                }],
            }),
        );
        let published = client.take_diagnostics(3);
        let last = published.last().expect("diagnostics");
        assert_eq!(last["diagnostics"], json!([]));
        assert_eq!(last["version"], 2);
        assert_eq!(client.request(4, "shutdown", Value::Null), Value::Null);
        client.notify(EXIT_METHOD, Value::Null);
        assert_eq!(server_thread.join().expect("server should stop"), 0);
    }

    #[test]
    fn serves_documents_with_a_byte_order_mark_and_utf16_schemas() {
        let directory = std::env::temp_dir().join(format!("xml-lsp-bom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("directory should be created");
        // UTF-16 LE schema with a byte order mark, as Windows tools write it.
        let schema = "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\r\n  <xs:element name=\"root\"><xs:complexType><xs:sequence>\r\n    <xs:element name=\"item\" minOccurs=\"0\"/>\r\n  </xs:sequence></xs:complexType></xs:element>\r\n</xs:schema>\r\n";
        let bytes = [0xFF, 0xFE]
            .into_iter()
            .chain(schema.encode_utf16().flat_map(u16::to_le_bytes))
            .collect::<Vec<u8>>();
        std::fs::write(directory.join("schema.xsd"), bytes).expect("schema should be written");
        let uri = path_to_uri(&directory.join("document.xml"));
        let text = "\u{FEFF}<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"schema.xsd\"><wrong/></root>";

        let (client, _, server_thread) = start_server(json!({}));
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "version": 1, "text": text}}),
        );
        let published = client.take_diagnostics(1);
        let publication = published.last().expect("diagnostics");
        assert_eq!(codes(publication), ["xsd-validation"], "{publication}");
        assert!(
            publication["diagnostics"][0]["message"]
                .as_str()
                .is_some_and(|message| message.contains("wrong")),
            "{publication}"
        );
        // The mark is one UTF-16 code unit before `<root`.
        let symbols = client.request(2, SYMBOL_METHOD, json!({"textDocument": {"uri": uri}}));
        let root = symbols
            .as_array()
            .and_then(|symbols| symbols.iter().find(|symbol| symbol["name"] == "root"))
            .expect("root symbol");
        assert_eq!(root["range"]["start"], json!({"line": 0, "character": 1}));
        // Formatting keeps the mark.
        let edits = client.request(
            3,
            FORMATTING_METHOD,
            json!({"textDocument": {"uri": uri}, "options": {"tabSize": 2, "insertSpaces": true}}),
        );
        let formatted = formatting::apply_edits(text, &edits);
        assert!(formatted.starts_with("\u{FEFF}<root"), "{formatted:?}");
        assert!(formatted.contains("\n  <wrong/>\n"), "{formatted:?}");
        // Completion reads the UTF-16 schema.
        client.notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": text.replace("<wrong/>", "<")}],
            }),
        );
        let character = text.find("<wrong/>").expect("element") + 1 - 2;
        let completion = client.request(
            4,
            COMPLETION_METHOD,
            json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": character}}),
        );
        assert!(
            completion["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == "item")),
            "{completion}"
        );
        assert_eq!(client.request(5, "shutdown", Value::Null), Value::Null);
        client.notify(EXIT_METHOD, Value::Null);
        assert_eq!(server_thread.join().expect("server should stop"), 0);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn publishes_the_diagnostics_of_the_latest_version_only_last() {
        let (client, _, server_thread) = start_server(json!({}));
        let uri = "file:///typing.xml";
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "version": 1, "text": "<root>"}}),
        );
        let mut text = "<root>".to_owned();
        for version in 2..=40 {
            text.push_str("<a/>");
            if version == 40 {
                text.push_str("</root>");
            }
            client.notify(
                DID_CHANGE_METHOD,
                json!({
                    "textDocument": {"uri": uri, "version": version},
                    "contentChanges": [{"text": text}],
                }),
            );
        }
        let published = client.take_diagnostics(1);
        let versions = published
            .iter()
            .map(|publication| publication["version"].as_i64().expect("version"))
            .collect::<Vec<_>>();
        assert!(
            versions.windows(2).all(|pair| pair[0] < pair[1]),
            "{versions:?}"
        );
        let last = published.last().expect("diagnostics");
        assert_eq!(last["version"], 40);
        assert_eq!(last["diagnostics"], json!([]));
        // Closing clears the diagnostics.
        client.notify(DID_CLOSE_METHOD, json!({"textDocument": {"uri": uri}}));
        let published = client.take_diagnostics(2);
        assert_eq!(published, [json!({"uri": uri, "diagnostics": []})]);
        assert_eq!(client.request(3, "shutdown", Value::Null), Value::Null);
        client.notify(EXIT_METHOD, Value::Null);
        assert_eq!(server_thread.join().expect("server should stop"), 0);
    }

    #[test]
    fn debounces_the_validation_of_changes() {
        let (client, _, server_thread) = start_server(
            json!({"initializationOptions": {"xml": {"validation": {"debounce": 300}}}}),
        );
        let uri = "file:///debounce.xml";
        // Opening validates at once.
        let opened = std::time::Instant::now();
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "version": 1, "text": "<a>"}}),
        );
        let receive_publication = || match client
            .connection
            .receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("diagnostics should be published")
        {
            Message::Notification(notification)
                if notification.method == PUBLISH_DIAGNOSTICS_METHOD =>
            {
                notification.params
            }
            message => panic!("unexpected message {message:?}"),
        };
        assert_eq!(receive_publication()["version"], 1);
        assert!(opened.elapsed() < std::time::Duration::from_millis(250));
        // A burst of changes: one validation, of the last version, once the
        // typing pauses.
        let mut changed = std::time::Instant::now();
        for version in 2..=6 {
            changed = std::time::Instant::now();
            client.notify(
                DID_CHANGE_METHOD,
                json!({
                    "textDocument": {"uri": uri, "version": version},
                    "contentChanges": [{"text": "<a>".repeat(version as usize)}],
                }),
            );
        }
        let publication = receive_publication();
        assert!(changed.elapsed() >= std::time::Duration::from_millis(300));
        assert_eq!(publication["version"], 6);
        assert!(
            client.take_diagnostics(1).is_empty(),
            "a single publication"
        );
        // Requests are answered during the debounce delay.
        client.notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": 7},
                "contentChanges": [{"text": "<a/>"}],
            }),
        );
        let started = std::time::Instant::now();
        client.request(
            2,
            DOCUMENT_HIGHLIGHT_METHOD,
            json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 1}}),
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(250));
        let published = client.take_diagnostics(3);
        assert_eq!(published.last().expect("diagnostics")["version"], 7);
        assert_eq!(client.request(4, "shutdown", Value::Null), Value::Null);
        client.notify(EXIT_METHOD, Value::Null);
        assert_eq!(server_thread.join().expect("server should stop"), 0);
    }

    #[test]
    fn limits_documents_larger_than_max_file_size_to_well_formedness() {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-large-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("directory should be created");
        std::fs::write(
            directory.join("a.xsd"),
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="a"/></xs:schema>"#,
        )
        .expect("schema should be written");
        let uri = path_to_uri(&directory.join("large.xml"));
        let text = format!(
            "<a xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"a.xsd\">\n{}<b></c>\n</a>\n",
            "  <!-- padding -->\n".repeat(20)
        );
        let (client, _, server_thread) = start_server(json!({
            "initializationOptions": {"xml": {"maxFileSize": 200}},
            "capabilities": {"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": true}}},
        }));
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "version": 1, "text": text}}),
        );
        let published = client.take_diagnostics(1);
        let publication = published.last().expect("diagnostics");
        // The mismatched end tag is still reported; `<b>` (not declared by
        // the schema) is not validated.
        assert_eq!(
            codes(publication),
            ["xml-structure", "large-file"],
            "{publication}"
        );
        let notice = &publication["diagnostics"][1];
        assert_eq!(notice["severity"], 3);
        assert_eq!(notice["data"]["kind"], "largeFile");
        assert!(
            notice["message"]
                .as_str()
                .is_some_and(|message| message.contains("xml.maxFileSize")),
            "{notice}"
        );
        let document = json!({"textDocument": {"uri": uri}});
        assert_eq!(
            client.request(2, FOLDING_RANGE_METHOD, document.clone()),
            json!([])
        );
        assert_eq!(
            client.request(3, SYMBOL_METHOD, document.clone()),
            json!([])
        );
        // Features proportional to the request still work.
        let highlights = client.request(
            4,
            DOCUMENT_HIGHLIGHT_METHOD,
            json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 1}}),
        );
        assert_eq!(highlights.as_array().map(Vec::len), Some(2));
        // Raising the limit restores validation and the features.
        client.notify(
            DID_CHANGE_CONFIGURATION_METHOD,
            json!({"settings": {"xml": {"maxFileSize": 0}}}),
        );
        let published = client.take_diagnostics(5);
        let publication = published.last().expect("diagnostics");
        assert!(
            codes(publication).contains(&"xsd-validation".to_owned()),
            "{publication}"
        );
        assert!(!codes(publication).contains(&"large-file".to_owned()));
        assert_ne!(client.request(6, SYMBOL_METHOD, document), json!([]));
        assert_eq!(client.request(7, "shutdown", Value::Null), Value::Null);
        client.notify(EXIT_METHOD, Value::Null);
        assert_eq!(server_thread.join().expect("server should stop"), 0);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
