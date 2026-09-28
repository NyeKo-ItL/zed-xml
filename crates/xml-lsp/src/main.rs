//! Serveur LSP XML natif.

use std::error::Error;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use serde_json::json;

const INITIALIZE_METHOD: &str = "initialize";
const EXIT_METHOD: &str = "exit";

fn server_capabilities() -> serde_json::Value {
    json!({})
}

fn run(connection: Connection) -> Result<(), Box<dyn Error + Send + Sync>> {
    connection.initialize(server_capabilities())?;

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
            Message::Notification(notification) if notification.method == EXIT_METHOD => break,
            Message::Notification(_) => {}
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
    fn serves_initialize_shutdown_and_exit() {
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
}
