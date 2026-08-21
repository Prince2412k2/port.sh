//! JSON-RPC 2.0 over a pipe, one document per line.
//!
//! Both directions. The client asks us things and we answer; we also ask the
//! client things -- a tool it implements, and nothing else for now -- so
//! outbound requests need ids and somewhere to put the answer when it comes
//! back. That is the whole reason this file is more than a serialiser.
//!
//! **Stdout is protocol and nothing else.** Writes go through one channel to
//! one writer, so no other part of the program is in a position to interleave a
//! stray line. A `println!` from a dependency would still corrupt the stream --
//! there is no way to prevent that from inside the process, only to keep our
//! own hands off it. Diagnostics go to stderr.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// ACP's own: the peer agreeing to stop. Not a failure.
pub const REQUEST_CANCELLED: i64 = -32800;

#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    pub code: i64,
    pub message: String,
}

impl Error {
    pub fn new(code: i64, message: impl Into<String>) -> Error {
        Error {
            code,
            message: message.into(),
        }
    }

    pub fn method_not_found(method: &str) -> Error {
        Error::new(METHOD_NOT_FOUND, format!("no method `{method}`"))
    }

    pub fn invalid_params(why: impl Into<String>) -> Error {
        Error::new(INVALID_PARAMS, why)
    }
}

#[derive(Debug)]
pub enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    /// The answer to something we asked.
    Response {
        id: Value,
        result: Result<Value, Error>,
    },
}

/// Read one line. `None` for a blank line or something unrecognisable, since a
/// peer sending junk should not take the connection down with it.
pub fn parse(line: &str) -> Option<Incoming> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    let id = value.get("id").cloned();
    let method = value.get("method").and_then(Value::as_str).map(str::to_string);
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    match (id, method) {
        (Some(id), Some(method)) if !id.is_null() => Some(Incoming::Request { id, method, params }),
        (_, Some(method)) => Some(Incoming::Notification { method, params }),
        (Some(id), None) => {
            let result = match value.get("error") {
                Some(e) => Err(Error {
                    code: e.get("code").and_then(Value::as_i64).unwrap_or(INTERNAL_ERROR),
                    message: e
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("no message")
                        .to_string(),
                }),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            Some(Incoming::Response { id, result })
        }
        _ => None,
    }
}

/// The other end of the pipe.
pub struct Peer {
    out: mpsc::UnboundedSender<String>,
    next: AtomicI64,
    waiting: Mutex<HashMap<String, oneshot::Sender<Result<Value, Error>>>>,
}

impl Peer {
    pub fn new(out: mpsc::UnboundedSender<String>) -> Peer {
        Peer {
            out,
            next: AtomicI64::new(1),
            waiting: Mutex::new(HashMap::new()),
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    pub fn reply(&self, id: &Value, result: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    pub fn reply_error(&self, id: &Value, error: &Error) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": error.code, "message": error.message }
        }));
    }

    /// Ask the client something and wait for the answer.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, Error> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let key = id.to_string();
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(key.clone(), tx);
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        match rx.await {
            Ok(result) => result,
            // The reader is gone, which means the pipe closed mid-question.
            Err(_) => {
                self.waiting.lock().unwrap().remove(&key);
                Err(Error::new(INTERNAL_ERROR, "the client went away"))
            }
        }
    }

    /// An answer arrived. Unknown ids are dropped: a client answering something
    /// we never asked is odd but not fatal.
    pub fn resolve(&self, id: &Value, result: Result<Value, Error>) {
        let key = key_of(id);
        if let Some(tx) = self.waiting.lock().unwrap().remove(&key) {
            let _ = tx.send(result);
        }
    }

    fn send(&self, value: Value) {
        // `to_string` never emits a raw newline, so one document really is one
        // line. A closed channel means we are shutting down.
        let _ = self.out.send(value.to_string());
    }
}

fn key_of(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_a_notification_and_a_response_are_told_apart() {
        assert!(matches!(
            parse(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#),
            Some(Incoming::Request { .. })
        ));
        assert!(matches!(
            parse(r#"{"jsonrpc":"2.0","method":"session/cancel","params":{}}"#),
            Some(Incoming::Notification { .. })
        ));
        assert!(matches!(
            parse(r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#),
            Some(Incoming::Response { result: Ok(_), .. })
        ));
    }

    #[test]
    fn an_error_response_carries_its_code() {
        let Some(Incoming::Response { result: Err(e), .. }) =
            parse(r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"nope"}}"#)
        else {
            panic!("not an error response")
        };
        assert_eq!(e.code, METHOD_NOT_FOUND);
        assert_eq!(e.message, "nope");
    }

    #[test]
    fn junk_and_blank_lines_are_ignored_rather_than_fatal() {
        assert!(parse("").is_none());
        assert!(parse("   ").is_none());
        assert!(parse("not json").is_none());
        assert!(parse("{}").is_none());
    }

    #[tokio::test]
    async fn an_outbound_request_is_matched_to_its_answer() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let peer = std::sync::Arc::new(Peer::new(tx));
        let asking = {
            let peer = peer.clone();
            tokio::spawn(async move { peer.request("tools/call", json!({"name": "show_map"})).await })
        };
        let line = rx.recv().await.unwrap();
        let sent: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(sent["method"], "tools/call");
        peer.resolve(&sent["id"], Ok(json!({"drawn": true})));
        assert_eq!(asking.await.unwrap(), Ok(json!({"drawn": true})));
    }

    #[tokio::test]
    async fn a_string_id_resolves_the_same_as_a_number() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let peer = Peer::new(tx);
        // Nothing is waiting on this id; the point is that it does not panic
        // and does not match the wrong waiter.
        peer.resolve(&json!("abc"), Ok(Value::Null));
        peer.resolve(&json!(99), Ok(Value::Null));
    }
}
