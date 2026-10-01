//! Notifications of the client: document lifecycle (`didOpen`, `didChange`,
//! `didClose`), configuration, workspace folders and watched files.

use super::*;

impl XmlLanguageServer {
    /// Handles a notification other than `exit`. Diagnostics are computed
    /// and published by the diagnostics worker, which receives the document
    /// and settings changes.
    pub(crate) fn handle_notification(
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
    pub(crate) fn register_watched_files(
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

    pub(crate) fn opened_document(params: &Value) -> Option<(String, String)> {
        let document = params.get("textDocument")?;
        Some((
            document.get("uri")?.as_str()?.to_owned(),
            document.get("text")?.as_str()?.to_owned(),
        ))
    }

    pub(crate) fn changed_document(
        params: &Value,
        current: Option<&String>,
    ) -> Option<(String, String)> {
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
