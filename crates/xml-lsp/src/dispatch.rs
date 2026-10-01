//! Plumbing of the request loop: the queue of incoming messages honouring
//! `$/cancelRequest`, panic isolation and the `shutdown`/`exit` sequence.
//!
//! - A request cancelled while it is still queued is answered with
//!   `RequestCancelled` (-32800) without being handled. The loop is
//!   sequential, so a request already being handled runs to completion
//!   (validation, the slow part, runs on the diagnostics worker).
//! - Each handler runs under `catch_unwind`: a bug answers `InternalError`
//!   (-32603) for that request, or drops that notification, and is logged
//!   on stderr; the server keeps running.
//! - After `shutdown`, requests are answered with `InvalidRequest` (-32600)
//!   and notifications other than `exit` are ignored. `exit` ends the loop
//!   with the process exit code: 0 after `shutdown`, 1 otherwise.

use std::{
    any::Any,
    collections::{HashSet, VecDeque},
    panic::{AssertUnwindSafe, catch_unwind},
};

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use serde_json::Value;

/// The `result` and `error` members of a response (`lsp-server` 0.10 keeps
/// them in one `Result`).
pub(crate) trait ResponseExt {
    fn result(&self) -> Option<Value>;
    fn error(&self) -> Option<lsp_server::ResponseError>;
}

impl ResponseExt for Response {
    fn result(&self) -> Option<Value> {
        self.response_result.as_ref().ok().cloned()
    }

    fn error(&self) -> Option<lsp_server::ResponseError> {
        self.response_result.as_ref().err().cloned()
    }
}

pub(crate) const CANCEL_REQUEST_METHOD: &str = "$/cancelRequest";
pub(crate) const SHUTDOWN_METHOD: &str = "shutdown";

/// Error answered to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestError {
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl RequestError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code as i32,
            message: message.into(),
        }
    }
}

/// Incoming messages read ahead of the one being handled, so that a
/// `$/cancelRequest` arriving after its request is seen before it.
#[derive(Default)]
pub(crate) struct Incoming {
    queue: VecDeque<Message>,
    cancelled: HashSet<RequestId>,
}

impl Incoming {
    /// Next message to handle, `None` once the connection is closed.
    /// Cancellation notifications are consumed here.
    pub(crate) fn next(&mut self, connection: &Connection) -> Option<Message> {
        loop {
            if self.queue.is_empty() {
                let message = connection.receiver.recv().ok()?;
                self.queue.push_back(message);
            }
            while let Ok(message) = connection.receiver.try_recv() {
                self.queue.push_back(message);
            }
            self.absorb_cancellations();
            if let Some(message) = self.queue.pop_front() {
                return Some(message);
            }
        }
    }

    /// Whether `id` was cancelled while queued (forgets it).
    pub(crate) fn take_cancelled(&mut self, id: &RequestId) -> bool {
        self.cancelled.remove(id)
    }

    fn absorb_cancellations(&mut self) {
        let mut index = 0;
        while index < self.queue.len() {
            let id = match &self.queue[index] {
                Message::Notification(notification)
                    if notification.method == CANCEL_REQUEST_METHOD =>
                {
                    Some(cancelled_id(notification))
                }
                _ => None,
            };
            let Some(id) = id else {
                index += 1;
                continue;
            };
            self.queue.remove(index);
            // Only requests still queued can be cancelled; the others have
            // already been answered.
            if let Some(id) = id
                && self
                    .queue
                    .iter()
                    .any(|message| matches!(message, Message::Request(request) if request.id == id))
            {
                self.cancelled.insert(id);
            }
        }
    }
}

fn cancelled_id(notification: &Notification) -> Option<RequestId> {
    let id = notification.params.get("id")?;
    match id {
        Value::Number(number) => Some(RequestId::from(i32::try_from(number.as_i64()?).ok()?)),
        Value::String(text) => Some(RequestId::from(text.clone())),
        _ => None,
    }
}

/// Runs `handler`, turning a panic into an `InternalError` response.
pub(crate) fn guarded_request(
    request: &Request,
    handler: impl FnOnce() -> Result<Value, RequestError>,
) -> Response {
    let id = request.id.clone();
    match catch_unwind(AssertUnwindSafe(handler)) {
        Ok(Ok(result)) => Response::new_ok(id, result),
        Ok(Err(error)) => Response::new_err(id, error.code, error.message),
        Err(payload) => {
            let message = panic_message(payload.as_ref());
            eprintln!(
                "xml-lsp: internal error while handling {}: {message}",
                request.method
            );
            Response::new_err(
                id,
                ErrorCode::InternalError as i32,
                format!(
                    "internal error while handling {}: {message}",
                    request.method
                ),
            )
        }
    }
}

/// Runs a notification `handler`, logging a panic instead of propagating
/// it; returns `None` after a panic.
pub(crate) fn guarded_notification<T>(method: &str, handler: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(handler)) {
        Ok(result) => Some(result),
        Err(payload) => {
            eprintln!(
                "xml-lsp: internal error while handling {method}: {}",
                panic_message(payload.as_ref())
            );
            None
        }
    }
}

/// Text of a panic payload.
pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(id: i32) -> Message {
        Request {
            id: RequestId::from(id),
            method: "textDocument/hover".to_owned(),
            params: json!({}),
        }
        .into()
    }

    fn cancel(id: Value) -> Message {
        Notification {
            method: CANCEL_REQUEST_METHOD.to_owned(),
            params: json!({"id": id}),
        }
        .into()
    }

    #[test]
    fn cancels_only_requests_still_queued() {
        let (server, client) = Connection::memory();
        for message in [
            request(1),
            request(2),
            cancel(json!(2)),
            cancel(json!(7)),
            cancel(json!("x")),
            cancel(json!(null)),
        ] {
            client.sender.send(message).unwrap();
        }
        let mut incoming = Incoming::default();
        let Some(Message::Request(first)) = incoming.next(&server) else {
            panic!("a request should be queued");
        };
        assert!(!incoming.take_cancelled(&first.id));
        let Some(Message::Request(second)) = incoming.next(&server) else {
            panic!("the cancellations should be consumed");
        };
        assert!(incoming.take_cancelled(&second.id));
        assert!(!incoming.take_cancelled(&second.id));
        assert!(incoming.cancelled.is_empty());
        // A queue holding only a cancellation waits for the next message.
        client.sender.send(cancel(json!(1))).unwrap();
        let sender = client.sender.clone();
        let later = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            sender.send(request(9)).unwrap();
        });
        let Some(Message::Request(ninth)) = incoming.next(&server) else {
            panic!("the next request should be returned");
        };
        assert_eq!(ninth.id, RequestId::from(9));
        later.join().unwrap();
        drop(client);
        assert!(incoming.next(&server).is_none());
    }

    #[test]
    fn turns_panics_into_internal_errors() {
        let Message::Request(request) = request(3) else {
            unreachable!()
        };
        let response = guarded_request(&request, || panic!("boom {}", 42));
        let error = response.error().expect("an error should be answered");
        assert_eq!(error.code, ErrorCode::InternalError as i32);
        assert!(error.message.contains("boom 42"), "{}", error.message);
        let response = guarded_request(&request, || {
            Err(RequestError::new(ErrorCode::InvalidParams, "bad"))
        });
        assert_eq!(
            response.error().unwrap().code,
            ErrorCode::InvalidParams as i32
        );
        assert_eq!(guarded_notification("x", || panic!("static")), None::<()>);
        assert_eq!(guarded_notification("x", || 5), Some(5));
    }
}
