//! Tools the client implements.
//!
//! This is the one place we add to ACP, and it exists because a process
//! boundary took something away. `mcp.rs` in `portfolio` binds a loopback HTTP
//! listener and mints a token per screen for exactly one reason: "ACP has no way
//! for a client to hand an agent a tool", so the only route the protocol offers
//! is an MCP server named in `session/new`. A tool that draws on the visitor's
//! panel has to run where the panel is.
//!
//! The protocol already sends requests the other way -- `fs/read_text_file`,
//! `session/request_permission` -- so a tool call in that direction fits its
//! shape rather than fighting it. The client declares what it implements in
//! `_meta` at `initialize`, which a conforming implementation ignores, and we
//! call back on a namespaced method. No listener, no token, no HTTP hop, and no
//! guessing how an agent will rename an MCP tool.

use std::sync::Arc;

use async_trait::async_trait;
use parley::types::Block;
use serde_json::{json, Value};

use crate::acp::rpc::Peer;
use crate::tool::{Cx, Failed, Kind, Mode, Output, Set, Spec, Tool};

/// The `_meta` key the client declares its tools under, and the method we call
/// back on. Namespaced so that a client which has never heard of either ignores
/// both.
pub const META_KEY: &str = "envoy/clientTools";
pub const CALL_METHOD: &str = "_envoy/tools/call";

/// One tool the client says it can run.
#[derive(Clone, Debug)]
pub struct Declared {
    pub name: String,
    pub title: String,
    pub description: String,
    pub schema: Value,
    pub kind: Kind,
}

/// Read the declarations out of an `initialize` request.
///
/// Anything malformed is skipped rather than refused: a client offering one
/// broken tool should still get the others, and `initialize` failing over a
/// `_meta` field it did not have to send at all would be a poor trade.
pub fn declared(params: &Value) -> Vec<Declared> {
    let Some(list) = params
        .get("_meta")
        .and_then(|m| m.get(META_KEY))
        .and_then(|c| c.get("tools"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|item| {
            let name = item.get("name").and_then(Value::as_str)?.to_string();
            let description = item
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(&name)
                .to_string();
            let schema = item.get("schema").cloned().unwrap_or_else(|| {
                json!({ "type": "object", "properties": {}, "required": [] })
            });
            let kind = match item.get("kind").and_then(Value::as_str) {
                Some("read") => Kind::Read,
                Some("edit") => Kind::Edit,
                Some("delete") => Kind::Delete,
                Some("move") => Kind::Move,
                Some("search") => Kind::Search,
                Some("execute") => Kind::Execute,
                Some("think") => Kind::Think,
                Some("fetch") => Kind::Fetch,
                Some("switch_mode") => Kind::SwitchMode,
                _ => Kind::Other,
            };
            Some(Declared {
                name,
                title,
                description,
                schema,
                kind,
            })
        })
        .collect()
}

/// A tool whose body is a request to the client.
pub struct Remote {
    spec: Spec,
    peer: Arc<Peer>,
}

impl Remote {
    pub fn new(declared: &Declared, peer: Arc<Peer>) -> Remote {
        Remote {
            spec: Spec {
                name: declared.name.clone(),
                title: declared.title.clone(),
                description: declared.description.clone(),
                schema: declared.schema.clone(),
                kind: declared.kind,
                // The client is a single process drawing on a single screen.
                // Two of its tools running at once is its problem to have, and
                // it did not ask for it.
                mode: Mode::Sequential,
            },
            peer,
        }
    }
}

#[async_trait]
impl Tool for Remote {
    fn spec(&self) -> &Spec {
        &self.spec
    }

    async fn call(&self, args: Value, cx: Cx) -> Result<Output, Failed> {
        let answer = tokio::select! {
            biased;
            // The client may be slow, and the visitor may have given up. A
            // cancelled call abandons the request rather than waiting for a
            // screen nobody is looking at.
            _ = cx.cancel.cancelled() => return Err(Failed::new("cancelled")),
            answer = self.peer.request(
                CALL_METHOD,
                json!({ "name": self.spec.name, "toolCallId": cx.call_id, "arguments": args }),
            ) => answer,
        };
        match answer {
            Err(e) => Err(Failed::new(e.message)),
            Ok(value) => {
                if value
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return Err(Failed::new(text_of(&value).unwrap_or_else(|| {
                        "the client reported a failure with no message".into()
                    })));
                }
                Ok(Output {
                    content: vec![Block::text(text_of(&value).unwrap_or_default())],
                    raw: value.get("rawOutput").cloned(),
                })
            }
        }
    }
}

/// The text out of a client's answer, accepting either a bare string or ACP's
/// content-block array.
fn text_of(value: &Value) -> Option<String> {
    if let Some(s) = value.get("content").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    let blocks = value.get("content").and_then(Value::as_array)?;
    let joined: String = blocks
        .iter()
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    (!joined.is_empty()).then_some(joined)
}

/// Native tools plus whatever the client offers.
///
/// A client tool cannot shadow a native one: the native tool is added second
/// and wins. Otherwise a client could redefine what a name means for a model
/// that was told what it does.
pub fn merge(native: &Set, declared: &[Declared], peer: &Arc<Peer>) -> Set {
    let mut set = Set::new();
    for one in declared {
        set.add(Arc::new(Remote::new(one, peer.clone())));
    }
    for tool in native.iter() {
        set.add(tool.clone());
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn init_params() -> Value {
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
            "_meta": { META_KEY: { "tools": [
                {
                    "name": "show_map",
                    "title": "Show a map",
                    "description": "put a point on screen",
                    "kind": "other",
                    "schema": {"type": "object", "properties": {"lat": {"type": "number"}}, "required": ["lat"]}
                },
                { "name": "nameless_is_skipped" , "description": "no name field below"},
                { "description": "this one has no name at all" }
            ] } }
        })
    }

    #[test]
    fn declarations_are_read_and_the_broken_one_is_skipped() {
        let tools = declared(&init_params());
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "show_map");
        assert_eq!(tools[0].title, "Show a map");
    }

    #[test]
    fn a_client_that_declares_nothing_is_not_an_error() {
        assert!(declared(&json!({"protocolVersion": 1})).is_empty());
        assert!(declared(&json!({"_meta": {}})).is_empty());
    }

    #[test]
    fn a_declaration_without_a_title_falls_back_to_its_name() {
        let params = json!({"_meta": { META_KEY: { "tools": [{"name": "t"}] } }});
        assert_eq!(declared(&params)[0].title, "t");
    }

    #[tokio::test]
    async fn calling_a_client_tool_is_a_request_the_client_answers() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let peer = Arc::new(Peer::new(tx));
        let tool = Remote::new(&declared(&init_params())[0], peer.clone());
        let cancel = tokio_util::sync::CancellationToken::new();
        let (ptx, _prx) = mpsc::unbounded_channel();
        let cx = Cx::new("c1".into(), cancel, ptx);

        let calling = tokio::spawn(async move { tool.call(json!({"lat": 26.9}), cx).await });
        let line = rx.recv().await.unwrap();
        let sent: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(sent["method"], CALL_METHOD);
        assert_eq!(sent["params"]["name"], "show_map");
        assert_eq!(sent["params"]["toolCallId"], "c1");
        assert_eq!(sent["params"]["arguments"], json!({"lat": 26.9}));

        peer.resolve(
            &sent["id"],
            Ok(json!({"content": [{"type": "text", "text": "drawn"}], "rawOutput": {"ok": true}})),
        );
        let output = calling.await.unwrap().unwrap();
        assert_eq!(output.content[0].as_text(), Some("drawn"));
        assert_eq!(output.raw, Some(json!({"ok": true})));
    }

    #[tokio::test]
    async fn a_client_reporting_a_failure_becomes_a_tool_failure() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let peer = Arc::new(Peer::new(tx));
        let tool = Remote::new(&declared(&init_params())[0], peer.clone());
        let (ptx, _prx) = mpsc::unbounded_channel();
        let cx = Cx::new(
            "c1".into(),
            tokio_util::sync::CancellationToken::new(),
            ptx,
        );
        let calling = tokio::spawn(async move { tool.call(json!({"lat": 1.0}), cx).await });
        let sent: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        peer.resolve(
            &sent["id"],
            Ok(json!({"isError": true, "content": "the gates refused it"})),
        );
        let err = calling.await.unwrap().unwrap_err();
        assert_eq!(err.0, "the gates refused it");
    }

    #[tokio::test]
    async fn a_cancelled_call_does_not_wait_for_the_client() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let peer = Arc::new(Peer::new(tx));
        let tool = Remote::new(&declared(&init_params())[0], peer);
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let (ptx, _prx) = mpsc::unbounded_channel();
        let cx = Cx::new("c1".into(), cancel, ptx);
        assert!(tool.call(json!({"lat": 1.0}), cx).await.is_err());
    }

    #[test]
    fn a_native_tool_wins_a_name_collision() {
        struct Native;
        #[async_trait]
        impl Tool for Native {
            fn spec(&self) -> &Spec {
                static S: std::sync::OnceLock<Spec> = std::sync::OnceLock::new();
                S.get_or_init(|| Spec {
                    name: "show_map".into(),
                    title: "Native".into(),
                    description: "the real one".into(),
                    schema: json!({"type": "object", "properties": {}, "required": []}),
                    kind: Kind::Other,
                    mode: Mode::Parallel,
                })
            }
            async fn call(&self, _args: Value, _cx: Cx) -> Result<Output, Failed> {
                Ok(Output::text("native"))
            }
        }
        let (tx, _rx) = mpsc::unbounded_channel();
        let peer = Arc::new(Peer::new(tx));
        let native = Set::new().with(Arc::new(Native));
        let merged = merge(&native, &declared(&init_params()), &peer);
        assert_eq!(merged.get("show_map").unwrap().spec().title, "Native");
    }
}
