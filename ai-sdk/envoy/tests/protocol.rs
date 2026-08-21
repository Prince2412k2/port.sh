//! The whole thing, over a pipe.
//!
//! This drives the real server the way a client does -- JSON-RPC lines in,
//! notifications and replies out -- with a scripted provider standing in for the
//! network. It is the test that would have caught every integration mistake the
//! unit tests cannot see: a notification shaped wrongly, a session id that does
//! not round-trip, a tool call that never reaches the client, a cancel that
//! arrives while a turn is running and is not read because the reader is busy.

use std::sync::Arc;

use envoy::acp::server::{serve, Setup};
use parley::stream::Event as Wired;
use parley::types::{Api, Cost, Endpoint, Model, Options, Stop};
use parley::Canned;
use serde_json::{json, Value};
use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

struct Client {
    to: DuplexStream,
    from: Lines<BufReader<DuplexStream>>,
    next: i64,
}

impl Client {
    async fn call(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next;
        self.next += 1;
        self.line(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        id
    }

    async fn notify(&mut self, method: &str, params: Value) {
        self.line(&json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await;
    }

    async fn answer(&mut self, id: &Value, result: Value) {
        self.line(&json!({"jsonrpc":"2.0","id":id,"result":result}))
            .await;
    }

    async fn line(&mut self, value: &Value) {
        let mut text = value.to_string();
        text.push('\n');
        self.to.write_all(text.as_bytes()).await.unwrap();
        self.to.flush().await.unwrap();
    }

    async fn next_message(&mut self) -> Value {
        let line = tokio::time::timeout(std::time::Duration::from_secs(5), self.from.next_line())
            .await
            .expect("the agent went quiet")
            .unwrap()
            .expect("the pipe closed");
        serde_json::from_str(&line).expect("the agent wrote something that is not JSON")
    }

    /// Everything until the reply to `id`, and that reply.
    async fn until_reply(&mut self, id: i64) -> (Vec<Value>, Value) {
        let mut seen = Vec::new();
        loop {
            let message = self.next_message().await;
            if message.get("id").and_then(Value::as_i64) == Some(id)
                && message.get("method").is_none()
            {
                return (seen, message);
            }
            seen.push(message);
        }
    }
}

fn model() -> Model {
    Model {
        id: "gpt-oss:120b".into(),
        name: "gpt-oss".into(),
        provider: "ollama-cloud".into(),
        api: Api::OpenaiCompletions,
        context_window: 131_072,
        max_output: None,
        reasoning: false,
        cost: Cost::default(),
    }
}

fn setup(turns: Vec<Vec<Wired>>) -> Arc<Setup> {
    setup_with(turns, None)
}

fn setup_with(turns: Vec<Vec<Wired>>, store: Option<Arc<envoy::Store>>) -> Arc<Setup> {
    Arc::new(Setup {
        wire: Arc::new(Canned::new(turns)),
        model: model(),
        endpoint: Endpoint::default(),
        options: Options::default(),
        tools: Arc::new(envoy::Set::new()),
        budget: envoy::Budget::default(),
        system: Some("be terse".into()),
        compaction: envoy::Compaction::off(),
        summariser: None,
        store,
    })
}

fn start(turns: Vec<Vec<Wired>>) -> Client {
    start_with(setup(turns))
}

fn start_with(setup: Arc<Setup>) -> Client {
    let (client_to, server_from) = duplex(64 * 1024);
    let (server_to, client_from) = duplex(64 * 1024);
    tokio::spawn(async move {
        let _ = serve(server_from, server_to, setup).await;
    });
    Client {
        to: client_to,
        from: BufReader::new(client_from).lines(),
        next: 1,
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("envoy-protocol-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn says(text: &str) -> Vec<Wired> {
    vec![
        Wired::Start { response_id: None },
        Wired::Text {
            index: 0,
            delta: text.into(),
        },
        Wired::Done { stop: Stop::End },
    ]
}

fn asks(id: &str, name: &str, args: Value) -> Vec<Wired> {
    vec![
        Wired::Start { response_id: None },
        Wired::Text {
            index: 0,
            delta: "let me draw that. ".into(),
        },
        Wired::BlockStart {
            index: 1,
            kind: parley::Kind::ToolCall,
            name: Some(name.into()),
            id: Some(id.into()),
        },
        Wired::ToolArgs {
            index: 1,
            delta: args.to_string(),
        },
        Wired::BlockEnd { index: 1 },
        Wired::Done { stop: Stop::ToolUse },
    ]
}

/// `initialize` declaring one client-implemented tool, then a new session.
async fn handshake(client: &mut Client) -> String {
    let id = client
        .call(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
                "_meta": { envoy::client_tool::META_KEY: { "tools": [{
                    "name": "show_map",
                    "title": "Show a map",
                    "description": "put a point on the screen",
                    "kind": "other",
                    "schema": {
                        "type": "object",
                        "properties": { "lat": {"type": "number"}, "lon": {"type": "number"} },
                        "required": ["lat", "lon"],
                        "additionalProperties": false
                    }
                }] } }
            }),
        )
        .await;
    let (_, reply) = client.until_reply(id).await;
    assert_eq!(reply["result"]["protocolVersion"], 1);
    assert_eq!(reply["result"]["agentInfo"]["name"], "envoy");
    // No auth methods: credentials come from a file an operator wrote.
    assert_eq!(reply["result"]["authMethods"], json!([]));
    // The client's tool was accepted.
    assert_eq!(
        reply["result"]["_meta"][envoy::client_tool::META_KEY]["accepted"],
        1
    );

    let id = client.call("session/new", json!({"cwd": "/tmp", "mcpServers": []})).await;
    let (_, reply) = client.until_reply(id).await;
    reply["result"]["sessionId"]
        .as_str()
        .expect("a session id")
        .to_string()
}

fn updates<'a>(messages: &'a [Value], kind: &str) -> Vec<&'a Value> {
    messages
        .iter()
        .filter(|m| m["method"] == "session/update" && m["params"]["update"]["sessionUpdate"] == kind)
        .map(|m| &m["params"]["update"])
        .collect()
}

#[tokio::test]
async fn a_prompt_streams_text_and_reports_a_clean_finish() {
    let mut client = start(vec![says("Jaipur is in Rajasthan.")]);
    let session = handshake(&mut client).await;

    let id = client
        .call(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{"type": "text", "text": "where is Jaipur?"}] }),
        )
        .await;
    let (seen, reply) = client.until_reply(id).await;

    assert_eq!(reply["result"]["stopReason"], "end_turn");
    let chunks = updates(&seen, "agent_message_chunk");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0]["content"]["text"], "Jaipur is in Rajasthan.");
    // Every notification is tagged with the session it belongs to, or a client
    // with two open sessions cannot tell them apart.
    for message in seen.iter().filter(|m| m["method"] == "session/update") {
        assert_eq!(message["params"]["sessionId"], session.as_str());
    }
    let usage = updates(&seen, "usage_update");
    assert_eq!(usage[0]["size"], 131_072);
}

#[tokio::test]
async fn a_tool_the_client_implements_is_called_back_and_its_answer_used() {
    // The case `mcp.rs` exists to work around: a tool that has to run where the
    // screen is. No loopback listener, no token, no HTTP hop.
    let mut client = start(vec![
        asks("c1", "show_map", json!({"lat": 26.9, "lon": 75.8})),
        says("drawn."),
    ]);
    let session = handshake(&mut client).await;

    let prompt = client
        .call(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{"type": "text", "text": "show me Jaipur"}] }),
        )
        .await;

    // Collect until the agent asks us to run the tool.
    let mut before = Vec::new();
    let request = loop {
        let message = client.next_message().await;
        if message["method"] == envoy::client_tool::CALL_METHOD {
            break message;
        }
        before.push(message);
    };

    assert_eq!(request["params"]["name"], "show_map");
    assert_eq!(request["params"]["toolCallId"], "c1");
    assert_eq!(request["params"]["arguments"], json!({"lat": 26.9, "lon": 75.8}));

    // The client was told about the call before being asked to run it, with the
    // arguments, so it can draw a row for it either way.
    let calls = updates(&before, "tool_call");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["toolCallId"], "c1");
    assert_eq!(calls[0]["rawInput"], json!({"lat": 26.9, "lon": 75.8}));
    assert_eq!(calls[0]["title"], "Show a map");
    // And the text that preceded the call arrived first.
    let text = updates(&before, "agent_message_chunk");
    assert_eq!(text[0]["content"]["text"], "let me draw that. ");

    client
        .answer(
            &request["id"],
            json!({ "content": [{"type": "text", "text": "the panel is showing Jaipur"}] }),
        )
        .await;

    let (after, reply) = client.until_reply(prompt).await;
    assert_eq!(reply["result"]["stopReason"], "end_turn");
    let done = updates(&after, "tool_call_update");
    assert!(
        done.iter().any(|u| u["status"] == "completed"),
        "{done:#?}"
    );
}

#[tokio::test]
async fn a_client_that_refuses_a_tool_gets_a_failed_call_not_a_dead_turn() {
    let mut client = start(vec![
        asks("c1", "show_map", json!({"lat": 1.0, "lon": 2.0})),
        says("the gates said no."),
    ]);
    let session = handshake(&mut client).await;
    let prompt = client
        .call(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{"type": "text", "text": "draw"}] }),
        )
        .await;

    let request = loop {
        let message = client.next_message().await;
        if message["method"] == envoy::client_tool::CALL_METHOD {
            break message;
        }
    };
    client
        .answer(
            &request["id"],
            json!({ "isError": true, "content": "refused by the gates" }),
        )
        .await;

    let (after, reply) = client.until_reply(prompt).await;
    // The run continues: the model is told it was refused and answers anyway.
    assert_eq!(reply["result"]["stopReason"], "end_turn");
    assert!(updates(&after, "tool_call_update")
        .iter()
        .any(|u| u["status"] == "failed"));
}

#[tokio::test]
async fn cancelling_mid_turn_is_read_while_the_turn_is_still_running() {
    // The reason requests are handled off the read loop. A tool that never
    // finishes unless cancelled would hang forever if `session/cancel` could
    // not be read until the prompt returned.
    let mut client = start(vec![asks("c1", "show_map", json!({"lat": 1.0, "lon": 2.0}))]);
    let session = handshake(&mut client).await;
    let prompt = client
        .call(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{"type": "text", "text": "draw forever"}] }),
        )
        .await;

    // Wait until the agent is blocked on us, then never answer.
    loop {
        let message = client.next_message().await;
        if message["method"] == envoy::client_tool::CALL_METHOD {
            break;
        }
    }
    client
        .notify("session/cancel", json!({ "sessionId": session }))
        .await;

    let (_, reply) = client.until_reply(prompt).await;
    // A cancelled prompt still returns a result: the turn happened, and part of
    // an answer is already on screen.
    assert_eq!(reply["result"]["stopReason"], "cancelled");
}

#[tokio::test]
async fn two_sessions_in_one_process_keep_their_own_history() {
    let mut client = start(vec![says("first"), says("second")]);
    let a = handshake(&mut client).await;
    let id = client.call("session/new", json!({})).await;
    let (_, reply) = client.until_reply(id).await;
    let b = reply["result"]["sessionId"].as_str().unwrap().to_string();
    assert_ne!(a, b);

    for session in [&a, &b] {
        let id = client
            .call(
                "session/prompt",
                json!({ "sessionId": session, "prompt": [{"type": "text", "text": "hi"}] }),
            )
            .await;
        let (seen, reply) = client.until_reply(id).await;
        assert_eq!(reply["result"]["stopReason"], "end_turn");
        for message in seen.iter().filter(|m| m["method"] == "session/update") {
            assert_eq!(message["params"]["sessionId"], session.as_str());
        }
    }

    let id = client.call("session/list", json!({})).await;
    let (_, reply) = client.until_reply(id).await;
    assert_eq!(reply["result"]["sessions"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn an_unknown_method_is_refused_without_taking_the_connection_down() {
    let mut client = start(vec![says("still here")]);
    let session = handshake(&mut client).await;

    let id = client.call("fs/chmod", json!({})).await;
    let (_, reply) = client.until_reply(id).await;
    assert_eq!(reply["error"]["code"], -32601);

    // And the session still works afterwards.
    let id = client
        .call(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{"type": "text", "text": "hi"}] }),
        )
        .await;
    let (_, reply) = client.until_reply(id).await;
    assert_eq!(reply["result"]["stopReason"], "end_turn");
}

#[tokio::test]
async fn a_prompt_for_a_session_that_does_not_exist_says_so() {
    let mut client = start(vec![]);
    let _ = handshake(&mut client).await;
    let id = client
        .call(
            "session/prompt",
            json!({ "sessionId": "nope", "prompt": [{"type": "text", "text": "hi"}] }),
        )
        .await;
    let (_, reply) = client.until_reply(id).await;
    assert_eq!(reply["error"]["code"], -32602);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("nope"));
}

#[tokio::test]
async fn a_second_prompt_on_a_busy_session_is_refused_rather_than_interleaved() {
    // Requests are handled concurrently so a cancel can arrive mid-turn. That
    // means two prompts could otherwise each clone the same history and each
    // append to it, and one turn would vanish.
    let mut client = start(vec![
        asks("c1", "show_map", json!({"lat": 1.0, "lon": 2.0})),
        says("done"),
    ]);
    let session = handshake(&mut client).await;

    let first = client
        .call(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{"type": "text", "text": "one"}] }),
        )
        .await;

    // Wait until the first turn is blocked on the client tool, so it is
    // demonstrably still running.
    let request = loop {
        let message = client.next_message().await;
        if message["method"] == envoy::client_tool::CALL_METHOD {
            break message;
        }
    };

    let second = client
        .call(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{"type": "text", "text": "two"}] }),
        )
        .await;
    let (_, refused) = client.until_reply(second).await;
    assert_eq!(refused["error"]["code"], -32602);
    assert!(refused["error"]["message"]
        .as_str()
        .unwrap()
        .contains("already answering"));

    // The first turn is unharmed and finishes normally.
    client
        .answer(&request["id"], json!({ "content": "drawn" }))
        .await;
    let (_, reply) = client.until_reply(first).await;
    assert_eq!(reply["result"]["stopReason"], "end_turn");
}

#[tokio::test]
async fn a_provider_failure_reaches_the_client_as_text_and_as_metadata() {
    // Found by pointing this at a real endpoint with a rejected key: the turn
    // ended, nothing was said, and the client had no way to know why.
    let mut client = start(vec![]); // an empty script fails the first call
    let session = handshake(&mut client).await;
    let id = client
        .call(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{"type": "text", "text": "hi"}] }),
        )
        .await;
    let (seen, reply) = client.until_reply(id).await;

    let chunks = updates(&seen, "agent_message_chunk");
    assert!(!chunks.is_empty(), "a failed turn must say something");
    assert!(chunks[0]["_meta"]["error"].is_string(), "{:#?}", chunks[0]);
    assert!(reply["result"]["_meta"]["error"].is_string(), "{reply:#?}");
}


#[tokio::test]
async fn a_conversation_survives_the_process_that_had_it() {
    let store = Arc::new(envoy::Store::open(scratch("resume")).unwrap());

    // One process answers a question.
    let session = {
        let mut client = start_with(setup_with(vec![says("Jaipur is in Rajasthan.")], Some(store.clone())));
        let session = handshake(&mut client).await;
        let id = client
            .call(
                "session/prompt",
                json!({ "sessionId": session, "prompt": [{"type": "text", "text": "where is Jaipur?"}] }),
            )
            .await;
        let (_, reply) = client.until_reply(id).await;
        assert_eq!(reply["result"]["stopReason"], "end_turn");
        session
    };

    // A second process picks it up and hands the transcript back.
    let mut client = start_with(setup_with(vec![says("Still Rajasthan.")], Some(store)));
    let init = client.call("initialize", json!({"protocolVersion": 1, "clientCapabilities": {}})).await;
    let (_, reply) = client.until_reply(init).await;
    // The capability is advertised because there is somewhere to keep them.
    assert_eq!(reply["result"]["agentCapabilities"]["loadSession"], true);
    assert!(reply["result"]["agentCapabilities"]["sessionCapabilities"]["resume"].is_object());

    let id = client.call("session/load", json!({"sessionId": session})).await;
    let (replayed, reply) = client.until_reply(id).await;
    assert_eq!(reply["result"]["sessionId"], session.as_str());
    // The stored conversation comes back as notifications so a client can draw
    // what was said before it existed.
    let user = updates(&replayed, "user_message_chunk");
    let agent = updates(&replayed, "agent_message_chunk");
    assert_eq!(user[0]["content"]["text"], "where is Jaipur?");
    assert_eq!(agent[0]["content"]["text"], "Jaipur is in Rajasthan.");

    // And the loaded history is really the model's context: asking again works.
    let id = client
        .call(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{"type": "text", "text": "and again?"}] }),
        )
        .await;
    let (_, reply) = client.until_reply(id).await;
    assert_eq!(reply["result"]["stopReason"], "end_turn");
}

#[tokio::test]
async fn forking_copies_a_conversation_without_disturbing_it() {
    let store = Arc::new(envoy::Store::open(scratch("fork")).unwrap());
    let mut client = start_with(setup_with(
        vec![says("first answer"), says("branch answer")],
        Some(store),
    ));
    let session = handshake(&mut client).await;
    let id = client
        .call(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{"type": "text", "text": "one"}] }),
        )
        .await;
    let _ = client.until_reply(id).await;

    let id = client.call("session/fork", json!({"sessionId": session})).await;
    let (_, reply) = client.until_reply(id).await;
    let branch = reply["result"]["sessionId"].as_str().unwrap().to_string();
    assert_ne!(branch, session);

    // The branch can be prompted, and the original is untouched.
    let id = client
        .call(
            "session/prompt",
            json!({ "sessionId": branch, "prompt": [{"type": "text", "text": "two"}] }),
        )
        .await;
    let (_, reply) = client.until_reply(id).await;
    assert_eq!(reply["result"]["stopReason"], "end_turn");

    let id = client.call("session/list", json!({})).await;
    let (_, reply) = client.until_reply(id).await;
    let listed: Vec<&str> = reply["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["sessionId"].as_str().unwrap())
        .collect();
    assert!(listed.contains(&session.as_str()) && listed.contains(&branch.as_str()));
}

#[tokio::test]
async fn without_a_store_resuming_is_advertised_as_absent_and_refused() {
    let mut client = start(vec![]);
    let id = client
        .call("initialize", json!({"protocolVersion": 1, "clientCapabilities": {}}))
        .await;
    let (_, reply) = client.until_reply(id).await;
    assert_eq!(reply["result"]["agentCapabilities"]["loadSession"], false);
    assert!(reply["result"]["agentCapabilities"]["sessionCapabilities"]["resume"].is_null());

    let id = client.call("session/load", json!({"sessionId": "anything"})).await;
    let (_, reply) = client.until_reply(id).await;
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("keeps no sessions"));
}

#[tokio::test]
async fn loading_a_session_that_was_never_stored_says_so() {
    let store = Arc::new(envoy::Store::open(scratch("absent")).unwrap());
    let mut client = start_with(setup_with(vec![], Some(store)));
    let _ = handshake(&mut client).await;
    let id = client.call("session/load", json!({"sessionId": "ghost"})).await;
    let (_, reply) = client.until_reply(id).await;
    assert_eq!(reply["error"]["code"], -32602);
    assert!(reply["error"]["message"].as_str().unwrap().contains("ghost"));
}

#[tokio::test]
async fn closing_keeps_a_session_and_deleting_throws_it_away() {
    let store = Arc::new(envoy::Store::open(scratch("delete")).unwrap());
    let mut client = start_with(setup_with(vec![says("kept")], Some(store.clone())));
    let session = handshake(&mut client).await;
    let id = client
        .call(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{"type": "text", "text": "hi"}] }),
        )
        .await;
    let _ = client.until_reply(id).await;

    let id = client.call("session/close", json!({"sessionId": session})).await;
    let _ = client.until_reply(id).await;
    assert!(store.exists(&session), "close must not delete");

    let id = client.call("session/delete", json!({"sessionId": session})).await;
    let _ = client.until_reply(id).await;
    assert!(!store.exists(&session), "delete must");
}
