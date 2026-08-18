//! Talking to a local agent over the Agent Client Protocol.
//!
//! `opencode acp` speaks JSON-RPC 2.0 over stdio: one document per line. The
//! handshake is initialize, then session/new, then session/set_mode — and the
//! mode matters more than anything else here. **Plan** is the read-only one.
//! This is a portfolio that may sit on a public SSH server, so the agent must
//! not be able to write files, and every permission request is refused rather
//! than negotiated.
//!
//! Everything the agent needs to answer is pushed into the first prompt as
//! context, which means it never needs a tool, which means refusing tools costs
//! nothing. That is the design: not a sandbox around a capable agent, but an
//! agent that was never given anything to reach for.
//!
//! Threading is one worker owning the child, fed by a single channel that both
//! the UI and the stdout reader write into — `std::sync::mpsc` has no select,
//! and one queue of one enum is simpler than polling two.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
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
    /// The current answer is complete.
    Done,
    Failed(String),
}

/// Questions per session. A public server should not hand strangers an
/// unbounded budget on someone else's account.
pub const MAX_TURNS: usize = 12;

enum In {
    Prompt(String),
    Msg(Value),
    Eof,
}

pub struct Ask {
    tx: Sender<In>,
    rx: Receiver<Event>,
}

impl Ask {
    /// Start the agent. Returns immediately; `Ready` or `Failed` arrives on the
    /// channel later. Called the first time anyone opens the section, so the
    /// portfolio does not spawn a language model to show a landing page.
    pub fn spawn(context: String) -> Ask {
        let (tx_in, rx_in) = channel::<In>();
        let (tx_out, rx_out) = channel::<Event>();
        let tx_reader = tx_in.clone();

        thread::spawn(move || {
            let mut child = match Command::new("opencode")
                .arg("acp")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx_out.send(Event::Failed(format!(
                        "could not start `opencode acp`: {e}"
                    )));
                    return;
                }
            };

            let stdout = child.stdout.take().expect("piped");
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Some(v) = json::parse(line.trim()) {
                        if tx_reader.send(In::Msg(v)).is_err() {
                            return;
                        }
                    }
                }
                let _ = tx_reader.send(In::Eof);
            });

            let stdin = child.stdin.take().expect("piped");
            run(&mut child, stdin, rx_in, tx_out, context);
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

fn write_line(w: &mut ChildStdin, s: &str) -> bool {
    w.write_all(s.as_bytes()).is_ok() && w.write_all(b"\n").is_ok() && w.flush().is_ok()
}

fn run(
    child: &mut Child,
    mut w: ChildStdin,
    rx: Receiver<In>,
    tx: Sender<Event>,
    context: String,
) {
    let fail = |tx: &Sender<Event>, m: &str| {
        let _ = tx.send(Event::Failed(m.to_string()));
    };

    // 1. initialize.
    if !write_line(&mut w, concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"#,
        r#""clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false}}}}"#
    )) {
        fail(&tx, "the agent closed its input");
        return;
    }
    if wait_for(&rx, 1, &tx).is_none() {
        fail(&tx, "no answer to initialize");
        return;
    }

    // 2. a session, rooted where the portfolio's own data lives.
    let cwd = std::env::current_dir().unwrap_or_default();
    let req = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"session/new","params":{{"cwd":{},"mcpServers":[]}}}}"#,
        json::quote(&cwd.to_string_lossy())
    );
    if !write_line(&mut w, &req) {
        fail(&tx, "the agent closed its input");
        return;
    }
    let Some(res) = wait_for(&rx, 2, &tx) else {
        fail(&tx, "no answer to session/new");
        return;
    };
    let Some(sid) = res.get("sessionId").and_then(|v| v.as_str()).map(str::to_string) else {
        fail(&tx, "the agent opened no session");
        return;
    };

    // 3. plan mode. Not optional, and not recoverable: if this fails the agent
    //    can write files, and this may be a public server.
    let req = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"session/set_mode","params":{{"sessionId":{},"modeId":"plan"}}}}"#,
        json::quote(&sid)
    );
    if !write_line(&mut w, &req) || wait_for(&rx, 3, &tx).is_none() {
        fail(&tx, "could not pin the session to plan mode -- refusing to continue");
        let _ = child.kill();
        return;
    }
    let _ = tx.send(Event::Ready);

    let mut id = 10i64;
    let mut first = true;
    let mut turns = 0usize;

    while let Ok(msg) = rx.recv() {
        match msg {
            In::Eof => break,
            In::Msg(v) => {
                // Requests the agent makes of us, outside any prompt.
                answer_request(&mut w, &v);
            }
            In::Prompt(q) => {
                if turns >= MAX_TURNS {
                    let _ = tx.send(Event::Chunk(format!(
                        "That is {MAX_TURNS} questions, which is where this stops. \
                         It runs on someone's account. Reconnect for a fresh session."
                    )));
                    let _ = tx.send(Event::Done);
                    continue;
                }
                turns += 1;
                id += 1;

                let text = if first {
                    first = false;
                    format!("{context}\n\n---\n\nThe first question:\n\n{q}")
                } else {
                    q
                };
                let req = format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"method":"session/prompt","params":{{"sessionId":{},"prompt":[{{"type":"text","text":{}}}]}}}}"#,
                    json::quote(&sid),
                    json::quote(&text)
                );
                if !write_line(&mut w, &req) {
                    fail(&tx, "the agent closed its input");
                    return;
                }
                if !stream(&rx, &tx, &mut w, id) {
                    return;
                }
            }
        }
    }
    let _ = child.kill();
}

/// Pump messages until the response to `id` arrives, forwarding the stream.
/// Returns false if the agent went away.
fn stream(rx: &Receiver<In>, tx: &Sender<Event>, w: &mut ChildStdin, id: i64) -> bool {
    loop {
        let Ok(msg) = rx.recv() else {
            let _ = tx.send(Event::Failed("the agent stopped mid-answer".into()));
            return false;
        };
        match msg {
            In::Eof => {
                let _ = tx.send(Event::Failed("the agent stopped mid-answer".into()));
                return false;
            }
            In::Prompt(_) => {
                // A question asked while one is still running. Dropped rather
                // than queued: the UI does not offer it, and silently running
                // two prompts against one session interleaves the answers.
            }
            In::Msg(v) => {
                if answer_request(w, &v) {
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
                        let _ = tx.send(Event::Failed(m.to_string()));
                    } else {
                        let _ = tx.send(Event::Done);
                    }
                    return true;
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
        _ => {}
    }
}

/// Answer a request the agent makes of the client. Returns true if `v` was one.
///
/// Everything is refused. In plan mode with the context already in the prompt
/// there is nothing legitimate to grant, and "ask the visitor to approve a tool
/// call" is not a question a portfolio should be putting to strangers.
fn answer_request(w: &mut ChildStdin, v: &Value) -> bool {
    let Some(method) = v.get("method").and_then(|m| m.as_str()) else { return false };
    let Some(id) = v.get("id").and_then(|i| i.as_f64()) else { return false };
    let id = id as i64;

    let body = match method {
        "session/request_permission" => {
            r#"{"outcome":{"outcome":"cancelled"}}"#.to_string()
        }
        // Anything else it might ask for: politely, nothing.
        _ => r#"{}"#.to_string(),
    };
    write_line(w, &format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{body}}}"#));
    true
}

/// Block until the response with `id` arrives, forwarding nothing.
fn wait_for(rx: &Receiver<In>, id: i64, tx: &Sender<Event>) -> Option<Value> {
    loop {
        match rx.recv().ok()? {
            In::Eof => return None,
            In::Prompt(_) => {}
            In::Msg(v) => {
                if v.get("id").and_then(|i| i.as_f64()) == Some(id as f64) {
                    if let Some(e) = v.get("error") {
                        let m = e.get("message").and_then(|m| m.as_str()).unwrap_or("failed");
                        let _ = tx.send(Event::Failed(m.to_string()));
                        return None;
                    }
                    return v.get("result").cloned();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upd(kind: &str, text: &str) -> Value {
        json::parse(&format!(
            r#"{{"sessionUpdate":"{kind}","content":{{"type":"text","text":{}}}}}"#,
            json::quote(text)
        ))
        .unwrap()
    }

    /// The stream handling is the part a live model would exercise, and this
    /// machine cannot reach one — so it is driven from recorded shapes instead.
    #[test]
    fn message_chunks_become_answer_and_thoughts_stay_separate() {
        let (tx, rx) = channel();
        forward(&tx, &upd("agent_message_chunk", "hello "));
        forward(&tx, &upd("agent_thought_chunk", "considering"));
        forward(&tx, &upd("agent_message_chunk", "there"));
        forward(&tx, &upd("usage_update", "ignored"));
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
        forward(&tx, &upd("agent_message_chunk", ""));
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
}
