//! Serveur LSP XML natif.

use std::{collections::HashMap, error::Error, fs, path::PathBuf};

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use serde_json::{Value, json};
use xml_core::{XmlDiagnostic, auto_close_tag, complete_xml, format_xml, parse_xml};
use xsd_core::{complete_elements, parse_xsd, resolve_schema_locations, validate_document};

const INITIALIZE_METHOD: &str = "initialize";
const EXIT_METHOD: &str = "exit";
const DID_OPEN_METHOD: &str = "textDocument/didOpen";
const DID_CHANGE_METHOD: &str = "textDocument/didChange";
const PUBLISH_DIAGNOSTICS_METHOD: &str = "textDocument/publishDiagnostics";
const FORMATTING_METHOD: &str = "textDocument/formatting";
const COMPLETION_METHOD: &str = "textDocument/completion";

struct XmlLanguageServer {
    documents: HashMap<String, String>,
}

impl XmlLanguageServer {
    fn new() -> Self {
        Self {
            documents: HashMap::new(),
        }
    }

    fn handle_notification(
        &mut self,
        connection: &Connection,
        notification: Notification,
    ) -> Result<bool, Box<dyn Error + Send + Sync>> {
        if notification.method == EXIT_METHOD {
            return Ok(true);
        }

        let Some((uri, text)) = (match notification.method.as_str() {
            DID_OPEN_METHOD => Self::opened_document(&notification.params),
            DID_CHANGE_METHOD => Self::changed_document(&notification.params),
            _ => None,
        }) else {
            return Ok(false);
        };

        self.documents.insert(uri.clone(), text.clone());
        let diagnostics = parse_xml(&text).diagnostics;
        let schema_diagnostics = self.schema_diagnostics(&uri, &text);
        connection.sender.send(
            Notification {
                method: PUBLISH_DIAGNOSTICS_METHOD.to_owned(),
                params: diagnostics_params(&uri, &text, &diagnostics, &schema_diagnostics),
            }
            .into(),
        )?;

        Ok(false)
    }

    fn opened_document(params: &Value) -> Option<(String, String)> {
        let document = params.get("textDocument")?;
        Some((
            document.get("uri")?.as_str()?.to_owned(),
            document.get("text")?.as_str()?.to_owned(),
        ))
    }

    fn completion(&self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(source, line, character);
        let mut items = complete_xml(source, offset)
            .into_iter()
            .map(|completion| {
                json!({
                    "label": completion.label,
                    "insertText": completion.insert_text,
                })
            })
            .collect::<Vec<_>>();
        if let Some(completion) = auto_close_tag(source, offset) {
            items.push(json!({
                "label": completion.label,
                "insertText": completion.insert_text,
            }));
        }
        items.extend(self.schema_completions(uri, source, offset));
        Some(json!({"isIncomplete": false, "items": items}))
    }

    fn schema_completions(&self, uri: &str, source: &str, offset: usize) -> Vec<Value> {
        let document_path = uri_to_path(uri);
        let references = resolve_schema_locations(schema_resolution_source(source), document_path)
            .unwrap_or_default();
        references
            .into_iter()
            .filter_map(|reference| fs::read_to_string(reference.path).ok())
            .filter_map(|schema_source| parse_xsd(&schema_source).ok())
            .flat_map(|schema| complete_elements(source, offset, &schema))
            .map(|completion| {
                json!({
                    "label": completion.label,
                    "insertText": completion.insert_text,
                })
            })
            .collect()
    }

    fn schema_diagnostics(&self, uri: &str, source: &str) -> Vec<Value> {
        let document_path = uri_to_path(uri);
        let references =
            match resolve_schema_locations(schema_resolution_source(source), &document_path) {
                Ok(references) => references,
                Err(error) => return vec![xsd_error_diagnostic(error)],
            };
        let mut diagnostics = Vec::new();

        for reference in references {
            let schema_source = match fs::read_to_string(&reference.path) {
                Ok(source) => source,
                Err(error) => {
                    diagnostics.push(xsd_error_diagnostic(format!(
                        "impossible de lire le schéma {} : {error}",
                        reference.path.display()
                    )));
                    continue;
                }
            };
            let schema = match parse_xsd(&schema_source) {
                Ok(schema) => schema,
                Err(error) => {
                    diagnostics.push(xsd_error_diagnostic(format!(
                        "schéma XSD invalide ({}): {error}",
                        reference.path.display()
                    )));
                    continue;
                }
            };
            diagnostics.extend(
                validate_document(source, &schema)
                    .into_iter()
                    .map(|diagnostic| xsd_error_diagnostic(diagnostic.message)),
            );
        }

        diagnostics
    }

    fn formatting(&self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        let formatted = format_xml(source).ok()?;
        Some(json!([{
            "range": {
                "start": {"line": 0, "character": 0},
                "end": position_at(source, source.len()),
            },
            "newText": formatted,
        }]))
    }

    fn changed_document(params: &Value) -> Option<(String, String)> {
        let document = params.get("textDocument")?;
        let uri = document.get("uri")?.as_str()?.to_owned();
        let text = params
            .get("contentChanges")?
            .as_array()?
            .first()?
            .get("text")?
            .as_str()?
            .to_owned();
        Some((uri, text))
    }
}

fn schema_resolution_source(source: &str) -> &str {
    source.strip_suffix('<').unwrap_or(source)
}

fn uri_to_path(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let raw = raw.strip_prefix('/').unwrap_or(raw);
    PathBuf::from(raw.replace("%20", " "))
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
        "message": diagnostic.into(),
    })
}

fn server_capabilities() -> Value {
    json!({
        "completionProvider": {"triggerCharacters": ["<", " ", "/"]},
        "documentFormattingProvider": true,
    })
}

fn offset_at(source: &str, line: usize, character: usize) -> usize {
    let mut current_line = 0;
    let mut current_character = 0;

    for (index, value) in source.char_indices() {
        if current_line == line {
            if current_character >= character {
                return index;
            }
            current_character += value.len_utf16();
        }
        if value == '\n' {
            current_line += 1;
            current_character = 0;
        }
    }

    source.len()
}

fn position_at(source: &str, offset: usize) -> Value {
    let mut line = 0;
    let mut character = 0;
    let bounded_offset = offset.min(source.len());

    for (index, value) in source.char_indices() {
        if index >= bounded_offset {
            break;
        }
        if value == '\n' {
            line += 1;
            character = 0;
        } else {
            character += value.len_utf16();
        }
    }

    json!({ "line": line, "character": character })
}

fn diagnostics_params(
    uri: &str,
    source: &str,
    diagnostics: &[XmlDiagnostic],
    schema_diagnostics: &[Value],
) -> Value {
    let diagnostics = diagnostics.iter().map(|diagnostic| {
        let position = position_at(source, diagnostic.offset);
        json!({
            "range": {
                "start": position,
                "end": position_at(source, diagnostic.offset),
            },
            "severity": 1,
            "source": "xml-lsp",
            "code": diagnostic.code(),
            "message": diagnostic.message,
        })
    });

    let mut diagnostics = diagnostics.collect::<Vec<_>>();
    diagnostics.extend(schema_diagnostics.iter().cloned());
    json!({
        "uri": uri,
        "diagnostics": diagnostics,
    })
}

fn run(connection: Connection) -> Result<(), Box<dyn Error + Send + Sync>> {
    connection.initialize(server_capabilities())?;
    let mut server = XmlLanguageServer::new();

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if request.method == COMPLETION_METHOD {
                    let result = server
                        .completion(&request.params)
                        .unwrap_or_else(|| json!({"isIncomplete": false, "items": []}));
                    connection
                        .sender
                        .send(Response::new_ok(request.id, result).into())?;
                    continue;
                }

                if request.method == FORMATTING_METHOD {
                    let edits = server
                        .formatting(&request.params)
                        .unwrap_or_else(|| json!([]));
                    connection
                        .sender
                        .send(Response::new_ok(request.id, edits).into())?;
                    continue;
                }

                if connection.handle_shutdown(&request)? {
                    break;
                }

                let response = Response::new_err(
                    request.id,
                    -32601,
                    format!("unsupported request: {}", request.method),
                );
                connection.sender.send(response.into())?;
            }
            Message::Notification(notification) => {
                if server.handle_notification(&connection, notification)? {
                    break;
                }
            }
            Message::Response(_) => {}
        }
    }

    Ok(())
}

fn main() {
    let (connection, io_threads) = Connection::stdio();

    if let Err(error) = run(connection) {
        eprintln!("xml-lsp stopped: {error}");
    }

    if let Err(error) = io_threads.join() {
        eprintln!("xml-lsp transport stopped: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

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
                            "completionProvider": {"triggerCharacters": ["<", " ", "/"]},
                            "documentFormattingProvider": true,
                        }
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
                assert_eq!(
                    notification.params["diagnostics"][0]["range"]["start"],
                    json!({"line": 0, "character": 6})
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
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 8},
                        },
                        "newText": "<root />\n",
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

        server_thread.join().expect("server thread should stop");
    }

    #[test]
    fn converts_unicode_offsets_to_utf16_positions() {
        assert_eq!(
            position_at("é\n😀<root>", 7),
            json!({"line": 1, "character": 2})
        );
    }

    #[test]
    fn publishes_xsd_diagnostics_for_a_referenced_schema() {
        let schema_path =
            std::env::temp_dir().join(format!("xml-lsp-schema-{}.xsd", std::process::id()));
        std::fs::write(
            &schema_path,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"><xs:complexType><xs:sequence><xs:element name="child"/></xs:sequence></xs:complexType></xs:element></xs:schema>"#,
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
        let server = XmlLanguageServer::new();
        let diagnostics = server.schema_diagnostics(&uri, &source);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["code"], "xsd-validation");
        assert!(
            diagnostics[0]["message"]
                .as_str()
                .unwrap()
                .contains("<wrong>")
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
}
