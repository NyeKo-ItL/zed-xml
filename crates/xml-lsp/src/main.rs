//! Serveur LSP XML natif.

use std::{collections::HashMap, error::Error};

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use serde_json::{Value, json};
use xml_core::{XmlDiagnostic, parse_xml};

const INITIALIZE_METHOD: &str = "initialize";
const EXIT_METHOD: &str = "exit";
const DID_OPEN_METHOD: &str = "textDocument/didOpen";
const DID_CHANGE_METHOD: &str = "textDocument/didChange";
const PUBLISH_DIAGNOSTICS_METHOD: &str = "textDocument/publishDiagnostics";

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
        connection.sender.send(
            Notification {
                method: PUBLISH_DIAGNOSTICS_METHOD.to_owned(),
                params: diagnostics_params(&uri, &text, &diagnostics),
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

fn server_capabilities() -> Value {
    json!({})
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

fn diagnostics_params(uri: &str, source: &str, diagnostics: &[XmlDiagnostic]) -> Value {
    let diagnostics = diagnostics.iter().map(|diagnostic| {
        let position = position_at(source, diagnostic.offset);
        json!({
            "range": {
                "start": position,
                "end": position_at(source, diagnostic.offset),
            },
            "severity": 1,
            "source": "xml-lsp",
            "message": diagnostic.message,
        })
    });

    json!({
        "uri": uri,
        "diagnostics": diagnostics.collect::<Vec<_>>(),
    })
}

fn run(connection: Connection) -> Result<(), Box<dyn Error + Send + Sync>> {
    connection.initialize(server_capabilities())?;
    let mut server = XmlLanguageServer::new();

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
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
                assert_eq!(response.result, Some(json!({"capabilities": {}})));
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
}
