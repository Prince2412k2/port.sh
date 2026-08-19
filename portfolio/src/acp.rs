//! Talking to a local agent over the Agent Client Protocol.
//!
//! `opencode acp` speaks JSON-RPC 2.0 over stdio: one document per line. The
//! handshake is initialize, then session/new, then session/set_mode.
//!
//! **What the agent may do.** Plan mode, plus an explicit allow-list, plus a
//! budget. It can read the web -- `webfetch` and `websearch` -- and it can
//! leave Prince a message. It cannot run a shell, and that is the line: this
//! is a public SSH server that accepts any username, and `bash` on it is
//! arbitrary code execution for anyone who can type. Fetching a URL is not
//! the same risk as running a command, so one is granted and the other is
//! refused, by name, in the config below and again at every permission
//! request.
//!
//! **Which model.** A list, tried in order, from data/models.txt. This runs on
//! somebody's personal account: one pinned model means the section is dead for
//! everyone the moment its free tier runs out. A model that will not start, or
//! that fails a question, is dropped and the next one is asked the same
//! question. `opencode acp` takes no `--model` flag, so the pin travels in
//! OPENCODE_CONFIG_CONTENT, which also carries the tool policy -- and needs no
//! writable file, which matters because the container's filesystem is
//! read-only.
//!
//! Threading is one worker owning the child, fed by a single channel that both
//! the UI and the stdout reader write into -- `std::sync::mpsc` has no select,
//! and one queue of one enum is simpler than polling two. Messages carry the
//! attempt they came from, so a killed model's last words are not mistaken for
//! the next one's.

use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread;

use crate::json::{self, Value};

/// What the UI sees.
#[derive(Debug, Clone)]
pub enum Event {
    /// Session established and pinned to plan mode.
    Ready,
    /// A piece of the answer.
    Chunk(String),
    /// Reasoning, shown as marginalia rather than as the answer.
    Thought(String),
    /// A tool call starting, or changing state. Shown live: watching it reach
    /// for something is most of what makes this different from a chat box.
    Tool(Call),
    /// The current answer is complete.
    Done,
    Failed(String),
}

/// One tool call, as much of it as the UI needs.
#[derive(Debug, Clone)]
pub struct Call {
    pub id: String,
    /// What it is doing, in the agent's words: "Fetch https://…".
    pub title: String,
    pub status: Status,
    /// Whatever identifies the target -- a URL, a query. May be empty.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Done,
    Failed,
    /// Refused by us, rather than attempted and failed.
    Refused,
}

/// Questions per session. A public server should not hand strangers an
/// unbounded budget on someone else's account.
pub const MAX_TURNS: usize = 12;

/// Tool calls per session, across every question.
///
/// The limit is on the session rather than the turn because the failure being
/// prevented is somebody using this box as a free web crawler, and that is a
/// total, not a rate. Twelve questions with a handful of fetches each is a
/// generous reading of curiosity and a poor one of automation.
pub const MAX_TOOL_CALLS: usize = 24;

/// What the agent is allowed to reach for. Anything not named here is refused
/// when it asks, whatever the config said.
pub const ALLOWED_TOOLS: [&str; 3] = ["webfetch", "websearch", "reach_out"];

enum In {
    Prompt(String),
    /// A message from the child of attempt `gen`.
    Msg(u64, Value),
    /// The child of attempt `gen` closed its output.
    Eof(u64),
}

pub struct Ask {
    tx: Sender<In>,
    rx: Receiver<Event>,
}

/// The models to try, in order.
pub fn models() -> Vec<String> {
    let disk = std::fs::read_to_string("portfolio/data/models.txt")
        .or_else(|_| std::fs::read_to_string("data/models.txt"))
        .ok();
    let src = disk.as_deref().unwrap_or(include_str!("../data/models.txt"));
    src.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// opencode's whole configuration for one attempt, as a JSON document.
///
/// Passed in the environment rather than written to disk: `opencode acp` has
/// no flag for the model, and the container has no writable filesystem to put
/// a config file on.
fn config(model: &str) -> String {
    format!(
        concat!(
            r#"{{"model":{},"#,
            // Belt and braces. `tools` is what the agent is told it has;
            // `permission` is what happens if it asks anyway. Neither is
            // trusted on its own -- every request is checked again by name in
            // answer_request, because a config key that is renamed upstream
            // should cost a refused tool call rather than a shell.
            r#""tools":{{"bash":false,"edit":false,"write":false,"patch":false,"#,
            r#""webfetch":true,"websearch":true}},"#,
            r#""permission":{{"bash":"deny","edit":"deny","write":"deny",""#,
            r#"webfetch":"allow","websearch":"allow"}}}}"#
        ),
        json::quote(model)
    )
}

impl Ask {
    /// Start the agent. Returns immediately; `Ready` or `Failed` arrives on the
    /// channel later. Called the first time anyone opens the section, so the
    /// portfolio does not spawn a language model to show a landing page.
    pub fn spawn(context: String) -> Ask {
        let (tx_in, rx_in) = channel::<In>();
        let (tx_out, rx_out) = channel::<Event>();
        // The worker keeps a sender so it can hand one to each attempt's
        // reader thread; the UI keeps the original.
        let mine = tx_in.clone();

        thread::spawn(move || {
            let list = models();
            if list.is_empty() {
                let _ = tx_out.send(Event::Failed(
                    "no models configured -- data/models.txt is empty".into(),
                ));
                return;
            }

            let mut state = Session::new(context);
            for (gen, model) in list.iter().enumerate() {
                let gen = gen as u64;
                let last = gen as usize + 1 == list.len();
                match attempt(gen, model, &rx_in, &mine, &tx_out, &mut state) {
                    Outcome::Closed => return,
                    Outcome::Broken(why) => {
                        if last {
                            let _ = tx_out.send(Event::Failed(format!(
                                "every model in the list refused. The last said: {why}"
                            )));
                            return;
                        }
                        // Not shown as a failure. From the visitor's side a
                        // switch is a slower answer, not an error, and naming
                        // the model that ran out is somebody's billing detail.
                    }
                }
            }
        });

        Ask { tx: tx_in, rx: rx_out }
    }

    pub fn send(&self, question: &str) {
        let _ = self.tx.send(In::Prompt(question.to_string()));
    }

    /// Everything that has arrived since the last call. Non-blocking.
    pub fn poll(&self) -> Vec<Event> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(e) => out.push(e),
                Err(TryRecvError::Empty) => return out,
                Err(TryRecvError::Disconnected) => {
                    if out.is_empty() {
                        out.push(Event::Failed("the agent stopped".into()));
                    }
                    return out;
                }
            }
        }
    }
}

/// What survives a model being swapped out from under the conversation.
struct Session {
    context: String,
    /// Whether the context still needs to go out with the next question. A new
    /// model has never seen it, so this goes back to true on every switch.
    first: bool,
    turns: usize,
    tools: usize,
    /// A question asked of a model that then died. Replayed at the next one
    /// rather than dropped -- the visitor asked once and should not have to
    /// notice that the first model was out of quota.
    pending: Option<String>,
}

impl Session {
    fn new(context: String) -> Session {
        Session { context, first: true, turns: 0, tools: 0, pending: None }
    }
}

enum Outcome {
    /// The UI went away. Nothing left to do.
    Closed,
    /// This model is no good. Try the next.
    Broken(String),
}

fn write_line(w: &mut ChildStdin, s: &str) -> bool {
    w.write_all(s.as_bytes()).is_ok() && w.write_all(b"\n").is_ok() && w.flush().is_ok()
}

/// Run one model until it finishes the conversation or fails.
fn attempt(
    gen: u64,
    model: &str,
    rx: &Receiver<In>,
    tx_in: &Sender<In>,
    tx: &Sender<Event>,
    state: &mut Session,
) -> Outcome {
    let mut child = match Command::new("opencode")
        .arg("acp")
        .env("OPENCODE_CONFIG_CONTENT", config(model))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Outcome::Broken(format!("could not start `opencode acp`: {e}")),
    };

    let stdout = child.stdout.take().expect("piped");
    let reader = tx_in.clone();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(v) = json::parse(line.trim()) {
                if reader.send(In::Msg(gen, v)).is_err() {
                    return;
                }
            }
        }
        let _ = reader.send(In::Eof(gen));
    });

    let mut w = child.stdin.take().expect("piped");
    let out = converse(gen, &mut w, rx, tx, state);
    let _ = child.kill();
    let _ = child.wait();
    out
}

fn converse(
    gen: u64,
    w: &mut ChildStdin,
    rx: &Receiver<In>,
    tx: &Sender<Event>,
    state: &mut Session,
) -> Outcome {
    // 1. initialize.
    if !write_line(w, concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"#,
        r#""clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false}}}}"#
    )) {
        return Outcome::Broken("the agent closed its input".into());
    }
    if let Err(e) = wait_for(gen, rx, 1) {
        return Outcome::Broken(e);
    }

    // 2. a session, rooted where the portfolio's own data lives.
    let cwd = std::env::current_dir().unwrap_or_default();
    let req = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"session/new","params":{{"cwd":{},"mcpServers":[]}}}}"#,
        json::quote(&cwd.to_string_lossy())
    );
    if !write_line(w, &req) {
        return Outcome::Broken("the agent closed its input".into());
    }
    let res = match wait_for(gen, rx, 2) {
        Ok(v) => v,
        Err(e) => return Outcome::Broken(e),
    };
    let Some(sid) = res.and_then(|r| r.get("sessionId").and_then(|v| v.as_str()).map(str::to_string))
    else {
        return Outcome::Broken("the agent opened no session".into());
    };

    // 3. plan mode. Not optional and not recoverable: it is the outermost of
    //    the three things stopping this from being a writable shell on a
    //    public box, and a model that will not enter it is not used at all.
    let req = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"session/set_mode","params":{{"sessionId":{},"modeId":"plan"}}}}"#,
        json::quote(&sid)
    );
    if !write_line(w, &req) || wait_for(gen, rx, 3).is_err() {
        return Outcome::Broken("would not enter plan mode".into());
    }
    let _ = tx.send(Event::Ready);

    let mut id = 10i64;

    loop {
        // A question left over from a model that died mid-answer goes first.
        let q = match state.pending.take() {
            Some(q) => q,
            None => match rx.recv() {
                Ok(In::Prompt(q)) => q,
                Ok(In::Eof(g)) if g == gen => {
                    return Outcome::Broken("stopped without answering".into())
                }
                // A dead model's leftovers, or a request outside any prompt.
                Ok(In::Eof(_)) => continue,
                Ok(In::Msg(g, v)) => {
                    if g == gen {
                        answer_request(w, &v, tx, state);
                    }
                    continue;
                }
                Err(_) => return Outcome::Closed,
            },
        };

        if state.turns >= MAX_TURNS {
            let _ = tx.send(Event::Chunk(format!(
                "That is {MAX_TURNS} questions, which is where this stops. \
                 It runs on someone's account. Reconnect for a fresh session."
            )));
            let _ = tx.send(Event::Done);
            continue;
        }
        state.turns += 1;
        id += 1;

        let text = if state.first {
            state.first = false;
            format!("{}\n\n---\n\nThe first question:\n\n{q}", state.context)
        } else {
            q.clone()
        };
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"session/prompt","params":{{"sessionId":{},"prompt":[{{"type":"text","text":{}}}]}}}}"#,
            json::quote(&sid),
            json::quote(&text)
        );
        if !write_line(w, &req) {
            state.pending = Some(q);
            state.first = true;
            state.turns -= 1;
            return Outcome::Broken("the agent closed its input".into());
        }
        match stream(gen, rx, tx, w, id, state) {
            Ok(()) => {}
            Err(why) => {
                // The question goes with us to the next model, and the context
                // with it -- that model has never seen either.
                state.pending = Some(q);
                state.first = true;
                state.turns -= 1;
                return Outcome::Broken(why);
            }
        }
    }
}

/// Pump messages until the response to `id` arrives, forwarding the stream.
/// `Err` means this model is finished and the caller should move on.
fn stream(
    gen: u64,
    rx: &Receiver<In>,
    tx: &Sender<Event>,
    w: &mut ChildStdin,
    id: i64,
    state: &mut Session,
) -> Result<(), String> {
    loop {
        let Ok(msg) = rx.recv() else { return Err("the UI went away".into()) };
        match msg {
            In::Eof(g) if g == gen => return Err("stopped mid-answer".into()),
            In::Eof(_) => {}
            In::Prompt(_) => {
                // A question asked while one is still running. Dropped rather
                // than queued: the UI does not offer it, and silently running
                // two prompts against one session interleaves the answers.
            }
            In::Msg(g, _) if g != gen => {}
            In::Msg(_, v) => {
                if answer_request(w, &v, tx, state) {
                    continue;
                }
                if v.get("method").and_then(|m| m.as_str()) == Some("session/update") {
                    if let Some(u) = v.get("params").and_then(|p| p.get("update")) {
                        forward(tx, u);
                    }
                    continue;
                }
                if v.get("id").and_then(|i| i.as_f64()) == Some(id as f64) {
                    if let Some(e) = v.get("error") {
                        let m = e.get("message").and_then(|m| m.as_str()).unwrap_or("failed");
                        return Err(m.to_string());
                    }
                    let _ = tx.send(Event::Done);
                    return Ok(());
                }
            }
        }
    }
}

fn forward(tx: &Sender<Event>, u: &Value) {
    let kind = u.get("sessionUpdate").and_then(|k| k.as_str()).unwrap_or("");
    let text = u
        .get("content")
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    match kind {
        "agent_message_chunk" if !text.is_empty() => {
            let _ = tx.send(Event::Chunk(text.to_string()));
        }
        "agent_thought_chunk" if !text.is_empty() => {
            let _ = tx.send(Event::Thought(text.to_string()));
        }
        "tool_call" | "tool_call_update" => {
            if let Some(c) = call_of(u) {
                let _ = tx.send(Event::Tool(c));
            }
        }
        _ => {}
    }
}

/// Read a tool call out of a session update.
fn call_of(u: &Value) -> Option<Call> {
    let id = u.get("toolCallId").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if id.is_empty() {
        return None;
    }
    let title = u.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let status = match u.get("status").and_then(|v| v.as_str()).unwrap_or("pending") {
        "completed" => Status::Done,
        "failed" => Status::Failed,
        _ => Status::Running,
    };
    // The interesting half of a fetch is the URL, and it is the one field
    // worth putting on screen next to a spinner.
    let detail = u
        .get("rawInput")
        .and_then(|i| {
            for k in ["url", "query", "message", "pattern"] {
                if let Some(s) = i.get(k).and_then(|v| v.as_str()) {
                    return Some(s.to_string());
                }
            }
            None
        })
        .unwrap_or_default();
    Some(Call { id, title, status, detail })
}

/// Answer a request the agent makes of the client. Returns true if `v` was one.
///
/// This is the last of the three gates, and the only one that is ours: plan
/// mode and the config are opencode's, and both are one upstream rename away
/// from silently meaning nothing. A permission request is granted only if the
/// tool is named in `ALLOWED_TOOLS` and the session still has budget. Anything
/// else -- a shell, an editor, a tool added to opencode next month -- is
/// refused without being asked about, because "ask the visitor to approve a
/// tool call" is not a question a portfolio should be putting to strangers.
fn answer_request(w: &mut ChildStdin, v: &Value, tx: &Sender<Event>, state: &mut Session) -> bool {
    let Some(method) = v.get("method").and_then(|m| m.as_str()) else { return false };
    let Some(id) = v.get("id").and_then(|i| i.as_f64()) else { return false };
    let id = id as i64;

    let body = match method {
        "session/request_permission" => {
            let p = v.get("params");
            let tool = p
                .and_then(|p| p.get("toolCall"))
                .and_then(|t| t.get("title").or_else(|| t.get("kind")))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let named = ALLOWED_TOOLS.iter().any(|a| tool.contains(a));
            let budget = state.tools < MAX_TOOL_CALLS;
            if named && budget {
                state.tools += 1;
                // The option id the agent offered for "yes". Picking the first
                // allow-shaped one rather than inventing a name, because the
                // set is the agent's to define.
                let opt = p
                    .and_then(|p| p.get("options"))
                    .and_then(|o| o.as_array())
                    .and_then(|o| {
                        o.iter().find(|c| {
                            c.get("kind")
                                .and_then(|k| k.as_str())
                                .is_some_and(|k| k.starts_with("allow"))
                        })
                    })
                    .and_then(|c| c.get("optionId").and_then(|i| i.as_str()))
                    .unwrap_or("allow")
                    .to_string();
                format!(
                    r#"{{"outcome":{{"outcome":"selected","optionId":{}}}}}"#,
                    json::quote(&opt)
                )
            } else {
                if !tool.is_empty() {
                    let why = if named { "out of tool budget" } else { "not allowed here" };
                    let _ = tx.send(Event::Tool(Call {
                        id: format!("refused-{id}"),
                        title: tool.to_string(),
                        status: Status::Refused,
                        detail: why.into(),
                    }));
                }
                r#"{"outcome":{"outcome":"cancelled"}}"#.to_string()
            }
        }
        // Anything else it might ask for: politely, nothing.
        _ => r#"{}"#.to_string(),
    };
    write_line(w, &format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{body}}}"#));
    true
}

/// Block until the response with `id` arrives, forwarding nothing.
fn wait_for(gen: u64, rx: &Receiver<In>, id: i64) -> Result<Option<Value>, String> {
    loop {
        match rx.recv() {
            Err(_) => return Err("the UI went away".into()),
            Ok(In::Eof(g)) if g == gen => return Err("stopped during the handshake".into()),
            Ok(In::Eof(_)) | Ok(In::Prompt(_)) => {}
            Ok(In::Msg(g, _)) if g != gen => {}
            Ok(In::Msg(_, v)) => {
                if v.get("id").and_then(|i| i.as_f64()) == Some(id as f64) {
                    if let Some(e) = v.get("error") {
                        let m = e.get("message").and_then(|m| m.as_str()).unwrap_or("failed");
                        return Err(m.to_string());
                    }
                    return Ok(v.get("result").cloned());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upd(json: &str) -> Value {
        json::parse(json).unwrap()
    }

    fn text_upd(kind: &str, text: &str) -> Value {
        upd(&format!(
            r#"{{"sessionUpdate":"{kind}","content":{{"type":"text","text":{}}}}}"#,
            json::quote(text)
        ))
    }

    /// The stream handling is the part a live model would exercise, and this
    /// machine cannot reach one — so it is driven from recorded shapes instead.
    #[test]
    fn message_chunks_become_answer_and_thoughts_stay_separate() {
        let (tx, rx) = channel();
        forward(&tx, &text_upd("agent_message_chunk", "hello "));
        forward(&tx, &text_upd("agent_thought_chunk", "considering"));
        forward(&tx, &text_upd("agent_message_chunk", "there"));
        forward(&tx, &text_upd("usage_update", "ignored"));
        drop(tx);

        let got: Vec<Event> = rx.iter().collect();
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(matches!(&got[0], Event::Chunk(s) if s == "hello "));
        assert!(matches!(&got[1], Event::Thought(s) if s == "considering"));
        assert!(matches!(&got[2], Event::Chunk(s) if s == "there"));
    }

    #[test]
    fn an_empty_chunk_is_not_an_event() {
        let (tx, rx) = channel();
        forward(&tx, &text_upd("agent_message_chunk", ""));
        drop(tx);
        assert_eq!(rx.iter().count(), 0);
    }

    /// A real capture from `opencode acp`, so the field path is not guessed.
    #[test]
    fn the_real_session_new_reply_yields_a_session_id() {
        let v = json::parse(
            r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"ses_feaa71059ffenpcDdN63WtENkB",
               "configOptions":[{"id":"mode","name":"Session Mode","currentValue":"build",
               "options":[{"value":"build"},{"value":"plan"}]}]}}"#,
        )
        .unwrap();
        let sid = v.get("result").unwrap().get("sessionId").unwrap().as_str();
        assert_eq!(sid, Some("ses_feaa71059ffenpcDdN63WtENkB"));
    }

    #[test]
    fn a_tool_call_carries_its_url_to_the_screen() {
        let (tx, rx) = channel();
        forward(&tx, &upd(
            r#"{"sessionUpdate":"tool_call","toolCallId":"t1","title":"Fetch docs",
               "status":"pending","rawInput":{"url":"https://example.com/a"}}"#,
        ));
        forward(&tx, &upd(
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"t1","title":"Fetch docs",
               "status":"completed","rawInput":{"url":"https://example.com/a"}}"#,
        ));
        drop(tx);

        let got: Vec<Event> = rx.iter().collect();
        assert_eq!(got.len(), 2);
        let Event::Tool(a) = &got[0] else { panic!("{got:?}") };
        assert_eq!(a.id, "t1");
        assert_eq!(a.detail, "https://example.com/a");
        assert_eq!(a.status, Status::Running);
        let Event::Tool(b) = &got[1] else { panic!("{got:?}") };
        assert_eq!(b.status, Status::Done);
    }

    /// An update with no tool id is not a tool call, and must not become an
    /// empty row on screen.
    #[test]
    fn an_update_without_a_tool_id_is_ignored() {
        let (tx, rx) = channel();
        forward(&tx, &upd(r#"{"sessionUpdate":"tool_call","title":"nameless"}"#));
        drop(tx);
        assert_eq!(rx.iter().count(), 0);
    }

    #[test]
    fn the_model_list_skips_comments_and_blanks() {
        let list = models();
        assert!(!list.is_empty(), "no models configured");
        for m in &list {
            assert!(!m.starts_with('#'), "{m} is a comment");
            assert!(m.contains('/'), "{m} is not provider/model");
        }
    }

    /// The config is the pin and the tool policy in one document, and it is
    /// built by string concatenation -- so check it is really JSON and really
    /// says what the module claims.
    #[test]
    fn the_config_pins_the_model_and_refuses_a_shell() {
        let v = json::parse(&config("opencode/grok-code")).expect("config is not valid JSON");
        assert_eq!(v.get("model").and_then(|m| m.as_str()), Some("opencode/grok-code"));
        let tools = v.get("tools").expect("no tools");
        for banned in ["bash", "edit", "write", "patch"] {
            assert_eq!(
                tools.get(banned).and_then(|b| b.as_bool()),
                Some(false),
                "{banned} is not disabled"
            );
        }
        assert_eq!(tools.get("webfetch").and_then(|b| b.as_bool()), Some(true));
        let perm = v.get("permission").expect("no permission block");
        assert_eq!(perm.get("bash").and_then(|b| b.as_str()), Some("deny"));
        assert_eq!(perm.get("webfetch").and_then(|b| b.as_str()), Some("allow"));
    }

    /// A model name with a quote in it must not be able to close the JSON
    /// string and add keys of its own -- models.txt is a mounted file.
    #[test]
    fn a_hostile_model_name_cannot_rewrite_the_config() {
        let v = json::parse(&config(r#"x","tools":{"bash":true},"junk":"#))
            .expect("config is not valid JSON");
        assert_eq!(
            v.get("tools").and_then(|t| t.get("bash")).and_then(|b| b.as_bool()),
            Some(false),
            "a model name reopened the tools block"
        );
    }
}
