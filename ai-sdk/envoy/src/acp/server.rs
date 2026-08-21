//! The agent, as a program a client starts.
//!
//! One process, many sessions. `session/new` mints an id and the client decides
//! whether to open one session or several; `portfolio` will open one per SSH
//! visitor, an editor will open one per pane, and neither has to care what the
//! other does.
//!
//! **Requests are handled off the read loop.** A prompt runs for as long as the
//! model and its tools take, and `session/cancel` arrives on the same pipe. If
//! the reader awaited the handler, the notification that stops the turn could
//! not be read until the turn it was stopping had finished -- interrupt would be
//! decorative. So each request gets a task and the reader keeps reading.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_client_protocol_schema::v1 as acp;
use futures_util::StreamExt;
use parley::types::{Block, Endpoint, Message, Model, Options};
use parley::Wire;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::acp::rpc::{self, Error, Incoming, Peer};
use crate::acp::update;
use crate::budget::Budget;
use crate::compact::{Compaction, Summariser};
use crate::event::{End, Event};
use crate::store::Store;
use crate::tool::Set;

/// The protocol version we speak. An integer, which is what v1 wants.
pub const PROTOCOL_VERSION: i64 = 1;

/// Everything a session needs, decided once at startup.
pub struct Setup {
    pub wire: Arc<dyn Wire>,
    pub model: Model,
    pub endpoint: Endpoint,
    pub options: Options,
    pub tools: Arc<Set>,
    pub budget: Budget,
    pub system: Option<String>,
    pub compaction: Compaction,
    /// A model to write a précis of what compaction drops. Absent means the
    /// oldest turns are dropped outright, which still works.
    pub summariser: Option<Summariser>,
    /// Where conversations live between runs. Absent means resuming and forking
    /// are advertised as absent, because they would be.
    pub store: Option<Arc<Store>>,
}

struct Session {
    id: String,
    history: Mutex<Vec<Message>>,
    /// The turn in flight, if any. Cancelling this is what `session/cancel`
    /// does; a new prompt replaces it.
    running: Mutex<Option<CancellationToken>>,
}

struct Inner {
    peer: Arc<Peer>,
    setup: Arc<Setup>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Request id to the token that stops it, for `$/cancel_request`.
    in_flight: Mutex<HashMap<String, CancellationToken>>,
    next_session: Mutex<u64>,
    /// Tools the client says it implements, learned at `initialize`.
    client_tools: Mutex<Vec<crate::client_tool::Declared>>,
}

#[derive(Clone)]
pub struct Server(Arc<Inner>);

/// Read requests from `input`, write protocol to `output`, until the pipe ends.
pub async fn serve<R, W>(input: R, output: W, setup: Arc<Setup>) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut output = output;
        while let Some(line) = out_rx.recv().await {
            if output.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if output.write_all(b"\n").await.is_err() {
                break;
            }
            let _ = output.flush().await;
        }
    });

    let server = Server(Arc::new(Inner {
        peer: Arc::new(Peer::new(out_tx)),
        setup,
        sessions: Mutex::new(HashMap::new()),
        in_flight: Mutex::new(HashMap::new()),
        next_session: Mutex::new(1),
        client_tools: Mutex::new(Vec::new()),
    }));

    let mut lines = BufReader::new(input).lines();
    let mut tasks = Vec::new();
    while let Some(line) = lines.next_line().await? {
        match rpc::parse(&line) {
            None => continue,
            Some(Incoming::Response { id, result }) => server.0.peer.resolve(&id, result),
            Some(Incoming::Notification { method, params }) => server.notification(&method, params),
            Some(Incoming::Request { id, method, params }) => {
                let server = server.clone();
                tasks.push(tokio::spawn(async move {
                    server.request(id, method, params).await;
                }));
            }
        }
    }
    // The pipe closed. Stop everything still running rather than leaving tasks
    // talking to a channel nobody drains.
    for session in server.0.sessions.lock().unwrap().values() {
        if let Some(token) = session.running.lock().unwrap().take() {
            token.cancel();
        }
    }
    for task in tasks {
        let _ = task.await;
    }
    drop(server);
    let _ = writer.await;
    Ok(())
}

impl Server {
    async fn request(&self, id: Value, method: String, params: Value) {
        let result = match method.as_str() {
            "initialize" => self.initialize(params),
            // No auth methods are advertised: credentials come from a file an
            // operator wrote, so there is nothing for a session to log into.
            // Answering rather than refusing, because a client that asks is
            // being polite, not wrong.
            "authenticate" => Ok(Value::Null),
            "session/new" => self.session_new(),
            "session/load" => self.session_open(&params, true),
            "session/resume" => self.session_open(&params, false),
            "session/fork" => self.session_fork(&params),
            "session/list" => self.session_list(),
            "session/close" => self.session_close(&params, false),
            "session/delete" => self.session_close(&params, true),
            // Accepted and ignored: we have one mode and no config options, and
            // a client that sets them should not see a failure for it.
            "session/set_mode" | "session/set_config_option" => Ok(Value::Null),
            "session/prompt" => self.prompt(&id, params).await,
            other => Err(Error::method_not_found(other)),
        };
        match result {
            Ok(value) => self.0.peer.reply(&id, value),
            Err(error) => self.0.peer.reply_error(&id, &error),
        }
        self.0.in_flight.lock().unwrap().remove(&key_of(&id));
    }

    fn notification(&self, method: &str, params: Value) {
        match method {
            "session/cancel" => {
                if let Some(id) = params.get("sessionId").and_then(Value::as_str) {
                    if let Some(session) = self.0.sessions.lock().unwrap().get(id) {
                        if let Some(token) = session.running.lock().unwrap().as_ref() {
                            token.cancel();
                        }
                    }
                }
            }
            "$/cancel_request" => {
                if let Some(request) = params.get("requestId") {
                    let key = key_of(request);
                    if let Some(token) = self.0.in_flight.lock().unwrap().get(&key) {
                        token.cancel();
                    }
                }
            }
            // Notifications are not answerable, so an unknown one is dropped.
            _ => {}
        }
    }

    fn initialize(&self, params: Value) -> Result<Value, Error> {
        let declared = crate::client_tool::declared(&params);
        *self.0.client_tools.lock().unwrap() = declared;
        let count = self.0.client_tools.lock().unwrap().len();
        let keeps = self.0.setup.store.is_some();
        let mut sessions = serde_json::Map::new();
        sessions.insert("list".into(), json!({}));
        sessions.insert("close".into(), json!({}));
        if keeps {
            sessions.insert("resume".into(), json!({}));
            sessions.insert("fork".into(), json!({}));
        }
        let sessions = Value::Object(sessions);
        Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "agentCapabilities": {
                // Listing and closing are free once sessions are keyed by id.
                // Resuming and forking need somewhere to keep them, so they are
                // advertised only when there is: claiming them without a store
                // would be a lie a client acts on.
                "sessionCapabilities": sessions,
                "loadSession": keeps,
                "promptCapabilities": { "image": true, "embeddedContext": false },
                // Our own tools do not need an MCP hop. Third-party servers are
                // configured on this side, not handed over in `session/new`.
                "mcpCapabilities": { "http": false, "sse": false }
            },
            "authMethods": [],
            "agentInfo": { "name": "envoy", "version": env!("CARGO_PKG_VERSION") },
            "_meta": { crate::client_tool::META_KEY: { "accepted": count } }
        }))
    }

    /// A session id unique across restarts.
    ///
    /// A per-process counter was enough until conversations went on disk; two
    /// runs would then both call their first session `s1` and the second would
    /// read the first one's history back.
    fn mint(&self) -> String {
        let mut next = self.0.next_session.lock().unwrap();
        let n = *next;
        *next += 1;
        drop(next);
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("{millis:x}{n:02x}")
    }

    fn register(&self, id: &str, history: Vec<Message>) -> Arc<Session> {
        let session = Arc::new(Session {
            id: id.to_string(),
            history: Mutex::new(history),
            running: Mutex::new(None),
        });
        self.0
            .sessions
            .lock()
            .unwrap()
            .insert(id.to_string(), session.clone());
        session
    }

    fn session_new(&self) -> Result<Value, Error> {
        let id = self.mint();
        self.register(&id, Vec::new());
        Ok(json!({ "sessionId": id, "modes": Value::Null }))
    }

    /// Attach to a stored conversation. `replay` sends it back as notifications,
    /// which is what `session/load` is for; `session/resume` only attaches.
    fn session_open(&self, params: &Value, replay: bool) -> Result<Value, Error> {
        let id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid_params("sessionId is required"))?;
        let store = self
            .0
            .setup
            .store
            .as_ref()
            .ok_or_else(|| Error::new(rpc::INTERNAL_ERROR, "this agent keeps no sessions"))?;
        if !store.exists(id) {
            return Err(Error::invalid_params(format!("no stored session `{id}`")));
        }
        let history = store
            .read(id)
            .map_err(|e| Error::new(rpc::INTERNAL_ERROR, format!("{id}: {e}")))?;
        if replay {
            self.replay(id, &history);
        }
        self.register(id, history);
        Ok(json!({ "sessionId": id, "modes": Value::Null }))
    }

    /// Copy a conversation and carry on from the copy.
    fn session_fork(&self, params: &Value) -> Result<Value, Error> {
        let from = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid_params("sessionId is required"))?;
        let history = match self.0.sessions.lock().unwrap().get(from) {
            Some(session) => session.history.lock().unwrap().clone(),
            None => match self.0.setup.store.as_ref().filter(|s| s.exists(from)) {
                Some(store) => store
                    .read(from)
                    .map_err(|e| Error::new(rpc::INTERNAL_ERROR, format!("{from}: {e}")))?,
                None => return Err(Error::invalid_params(format!("no session `{from}`"))),
            },
        };
        let id = self.mint();
        self.register(&id, history.clone());
        if let Some(store) = &self.0.setup.store {
            if let Err(e) = store.append(&id, &history) {
                eprintln!("envoy: {id}: cannot write the fork: {e}");
            }
        }
        Ok(json!({ "sessionId": id, "modes": Value::Null }))
    }

    /// Send a stored conversation back so a client can draw it.
    fn replay(&self, session: &str, history: &[Message]) {
        for message in history {
            let update = match message {
                Message::User { content } => Some(acp::SessionUpdate::UserMessageChunk(
                    acp::ContentChunk::new(acp::ContentBlock::from(text_of(content).as_str())),
                )),
                Message::Assistant(a) => {
                    let text = a.text();
                    (!text.is_empty()).then(|| {
                        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                            acp::ContentBlock::from(text.as_str()),
                        ))
                    })
                }
                // Tool results without their calls would be noise; a replayed
                // transcript is for reading, not for re-running.
                Message::ToolResult { .. } => None,
            };
            if let Some(update) = update {
                let notification =
                    acp::SessionNotification::new(acp::SessionId::new(session), update);
                if let Ok(params) = serde_json::to_value(&notification) {
                    self.0.peer.notify("session/update", params);
                }
            }
        }
    }

    fn session_list(&self) -> Result<Value, Error> {
        let mut ids: Vec<String> = self.0.sessions.lock().unwrap().keys().cloned().collect();
        if let Some(store) = &self.0.setup.store {
            // Stored sessions the client could open, as well as the ones already
            // open. A client listing sessions wants to know what it can resume.
            ids.extend(store.list().unwrap_or_default());
        }
        ids.sort();
        ids.dedup();
        Ok(json!({ "sessions": ids.iter().map(|id| json!({"sessionId": id})).collect::<Vec<_>>() }))
    }

    fn session_close(&self, params: &Value, delete: bool) -> Result<Value, Error> {
        let id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid_params("sessionId is required"))?;
        if let Some(session) = self.0.sessions.lock().unwrap().remove(id) {
            if let Some(token) = session.running.lock().unwrap().take() {
                token.cancel();
            }
        }
        // `close` puts a session down; `delete` throws it away. Closing must
        // leave the file alone or resuming would never work.
        if delete {
            if let Some(store) = &self.0.setup.store {
                if let Err(e) = store.remove(id) {
                    eprintln!("envoy: {id}: cannot delete: {e}");
                }
            }
        }
        Ok(Value::Null)
    }

    async fn prompt(&self, request_id: &Value, params: Value) -> Result<Value, Error> {
        let id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid_params("sessionId is required"))?
            .to_string();
        let session = self
            .0
            .sessions
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::invalid_params(format!("no session `{id}`")))?;

        let content = prompt_blocks(&params);
        if content.is_empty() {
            return Err(Error::invalid_params("prompt carried no content"));
        }

        // One turn at a time per session. Requests are handled concurrently so
        // that a cancel can be read while a prompt runs, and the price of that
        // is this guard: two prompts on one session would each clone the same
        // history and each append to it, losing whichever finished first.
        // ACP clients prompt one at a time; a client that does not should be
        // told rather than quietly given a corrupted conversation.
        let cancel = CancellationToken::new();
        {
            let mut running = session.running.lock().unwrap();
            if running.is_some() {
                return Err(Error::invalid_params(format!(
                    "session `{id}` is already answering; cancel it or wait"
                )));
            }
            *running = Some(cancel.clone());
        }
        self.0
            .in_flight
            .lock()
            .unwrap()
            .insert(key_of(request_id), cancel.clone());

        let user = Message::User { content };
        let mut history = session.history.lock().unwrap().clone();
        history.push(user.clone());

        // Client tools are registered per session, because they are the client's
        // and a different client may implement different ones.
        let tools = crate::client_tool::merge(
            &self.0.setup.tools,
            &self.0.client_tools.lock().unwrap(),
            &self.0.peer,
        );

        // One cache key per session, stable for its life. Both apis cache on a
        // stable prefix and route by this key; changing it mid-conversation
        // quietly stops the caching and costs money without saying so.
        let mut options = self.0.setup.options.clone();
        options.cache_key = Some(format!("envoy-{}", session.id));

        let cfg = Arc::new(crate::agent::Config {
            wire: self.0.setup.wire.clone(),
            model: self.0.setup.model.clone(),
            endpoint: self.0.setup.endpoint.clone(),
            options,
            tools: Arc::new(tools),
            budget: self.0.setup.budget,
            system: self.0.setup.system.clone(),
        });

        let history = self
            .0
            .setup
            .compaction
            .apply_with(
                history,
                &self.0.setup.model,
                self.0.setup.summariser.as_ref(),
                |before, after| {
                    self.send(&session.id, &Event::Compacted { before, after });
                },
            )
            .await;

        // Pinned because the loop is a generator, and a generator holds
        // references into itself across yields.
        let mut stream = Box::pin(crate::agent::run(cfg, history, cancel));
        let mut reason = End::EndTurn;
        let mut appended = Vec::new();
        while let Some(event) = stream.next().await {
            self.send(&session.id, &event);
            if let Event::Ended {
                reason: r,
                appended: a,
            } = event
            {
                reason = r;
                appended = a;
            }
        }
        drop(stream);

        {
            let mut stored = session.history.lock().unwrap();
            stored.push(user.clone());
            stored.extend(appended.clone());
        }
        if let Some(store) = &self.0.setup.store {
            let mut written = vec![user];
            written.extend(appended);
            // Written after the turn rather than during it: a half-finished turn
            // on disk would be replayed as a conversation that never happened.
            if let Err(e) = store.append(&session.id, &written) {
                eprintln!("envoy: {}: cannot write the turn: {e}", session.id);
            }
        }
        *session.running.lock().unwrap() = None;

        // A cancelled or failed prompt still returns a result. The turn
        // happened, and part of an answer may already be on screen. ACP has no
        // stop reason for a failure, so the reason rides in `_meta` alongside
        // the message already sent as content.
        let mut reply = json!({ "stopReason": reason.acp() });
        if let End::Failed(why) = &reason {
            reply["_meta"] = json!({ update::META_ERROR: why });
        }
        Ok(reply)
    }

    fn send(&self, session: &str, event: &Event) {
        for one in update::updates(event) {
            let notification = acp::SessionNotification::new(acp::SessionId::new(session), one);
            match serde_json::to_value(&notification) {
                Ok(params) => self.0.peer.notify("session/update", params),
                // Unserialisable is a bug in the mapping, not in the peer.
                Err(e) => eprintln!("envoy: could not encode an update: {e}"),
            }
        }
    }
}

fn text_of(blocks: &[Block]) -> String {
    blocks.iter().filter_map(Block::as_text).collect()
}

/// The content of a `session/prompt`, as blocks we understand.
fn prompt_blocks(params: &Value) -> Vec<Block> {
    let Some(items) = params.get("prompt").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item.get("type").and_then(Value::as_str) {
            Some("text") => item
                .get("text")
                .and_then(Value::as_str)
                .map(Block::text),
            Some("image") => {
                let data = item.get("data").and_then(Value::as_str)?;
                let mime = item.get("mimeType").and_then(Value::as_str)?;
                Some(Block::Image {
                    data: data.to_string(),
                    mime: mime.to_string(),
                })
            }
            _ => None,
        })
        .collect()
}

fn key_of(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
