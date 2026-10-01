//! Settings and XML catalogs: `xml.*` configuration requests and changes,
//! catalog (re)loading and the file watchers that keep them fresh.

use super::*;
use crate::dispatch::ResponseExt;

impl XmlLanguageServer {
    /// Recomputes the list of catalogs (settings and workspace folders);
    /// returns `true` if resolution may have changed.
    pub(crate) fn update_catalogs(&mut self) -> bool {
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
    pub(crate) fn refresh_catalog_files(&mut self) -> bool {
        if self.catalogs.is_empty() || !self.catalogs.refresh() {
            return false;
        }
        self.log_catalog_errors();
        self.forget_resolutions();
        true
    }

    /// Drops the cached schema graphs, whose dependencies were resolved
    /// through the previous catalogs.
    pub(crate) fn forget_resolutions(&mut self) {
        self.schemas.clear();
        self.model_cache.clear();
    }

    pub(crate) fn log_catalog_errors(&self) {
        for (path, error) in self.catalogs.errors() {
            eprintln!("xml-lsp: catalog {} ignored: {error}", path.display());
        }
    }

    /// Rereads the catalogs modified on disk; then republishes the
    /// diagnostics of the open documents.
    pub(crate) fn refresh_catalogs(
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
    pub(crate) fn register_catalog_watchers(
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
    pub(crate) fn initialize_settings(&mut self, initialize_params: &Value) {
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
    pub(crate) fn request_configuration(
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

    pub(crate) fn handle_response(
        &mut self,
        connection: &Connection,
        response: Response,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.pending_configuration.as_ref() != Some(&response.id) {
            return Ok(());
        }
        self.pending_configuration = None;
        let section = response
            .result()
            .as_ref()
            .and_then(|result| result.get(0))
            .cloned()
            .unwrap_or(Value::Null);
        self.apply_settings(connection, &section)
    }

    /// `workspace/didChangeConfiguration`: uses the `xml` section pushed by
    /// the client, otherwise requests it again.
    pub(crate) fn configuration_changed(
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
    pub(crate) fn apply_settings(
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
}
