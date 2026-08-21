//! Talking to an ACP server over stdio.
//!
//! The Agent Client Protocol is JSON-RPC 2.0, one document per line. This is
//! the client half: it starts a server, negotiates what each side may do,
//! carries questions in and an answer out, and answers the requests the agent
//! makes back the other way.
//!
//! **Which server.** Any of them. The command and the way a model name is
//! pinned live in `servers.rs`, chosen per tier in `data/models.txt`; opencode
//! is the default because it is what this box has, not because it is the only
//! thing that fits. Nothing below mentions it by name.
//!
//! **What the agent may do.** `gates.rs`, and only `gates.rs`. The gates are
//! advertised in the handshake, pushed into the server's own tool policy where
//! it has one, and then enforced again here on every inbound request -- three
//! layers deriving from one constant, because the first two are somebody else's
//! code and one upstream rename from meaning nothing. This is a public SSH
//! server that accepts any username; a shell on it is arbitrary code execution
//! for anyone who can type, so the refusal is not left to a config file.
//!
//! **Default deny.** A method we do not implement gets a JSON-RPC error, not an
//! empty result. That distinction is the whole point: answering
//! `terminal/create` with `{}` tells the agent it has a terminal, and it will
//! then ask that terminal for output.
//!
//! **Which model.** A list, tried in order, from the hourly check in
//! `health.rs`. This runs on somebody's personal account: one pinned model means
//! the section is dead for everyone the moment its free tier runs out. A model
//! that will not start, or that fails a question, is dropped and the next is
//! asked the same question.
//!
//! Threading is one worker owning the child, fed by a single channel that both
//! the UI and the stdout reader write into -- `std::sync::mpsc` has no select,
//! and one queue of one enum is simpler than polling two. Messages carry the
//! attempt they came from, so a killed model's last words are not mistaken for
//! the next one's.

use std::io::{BufRead, BufReader, Write};
use std::process::ChildStdin;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread;

use crate::gates::{self, Verdict, GATES};
use crate::health::Shot;
use crate::json::{self, Value};

/// What the UI sees.
#[derive(Debug, Clone)]
pub enum Event {
    /// Session established and restrained. Carries what we ended up talking to,
    /// so the screen can say so rather than guess.
    Ready(Ready),
    /// A piece of the answer.
    Chunk(String),
    /// Reasoning, shown as marginalia rather than as the answer.
    Thought(String),
    /// A tool call starting, or changing state. Shown live: watching it reach
    /// for something is most of what makes this different from a chat box.
    Tool(Call),
    /// The current answer is complete.
    Done,
    /// The visitor stopped it. Not a failure, and not the model's fault -- the
    /// tier stays in play.
    Cancelled,
    Failed(String),
}

/// What the handshake settled on. All of it is for the screen.
#[derive(Debug, Clone, Default)]
pub struct Ready {
    /// The tier name from `models.txt`.
    pub tier: String,
    /// The server command actually running.
    pub server: String,
    /// The protocol version the agent agreed to.
    pub version: i64,
    /// How the session was restrained, in a word for the screen: `plan`,
    /// `readonly`, or empty if the server offered nothing to set.
    pub mode: String,
    /// Whether this session was handed our tool server.
    ///
    /// The page needs to know because it has a keyword guess for when a map
    /// belongs on screen, and that guess must be off the moment the *agent* can
    /// decide instead -- not merely after the agent has decided once. Waiting
    /// for the first tool call meant a map appeared the instant enter was
    /// pressed, before the model had said anything, and then vanished when the
    /// answer turned out to want a different place. Two things deciding one
    /// panel is worse than either.
    pub tools: bool,
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

/// The protocol version this client speaks.
///
/// Sent as an integer because that is what ACP v1 wants. The agent answers with
/// the version it settled on, which is shown rather than assumed -- a server
/// that replies `2` has not been tested against this client and the screen
/// should be able to say which one answered.
pub const PROTOCOL_VERSION: i64 = 1;

/// JSON-RPC's "method not found". What a shut gate and an unimplemented method
/// both look like from the other end.
const METHOD_NOT_FOUND: i64 = -32601;

/// ACP's cancellation error. A response carrying it is the peer agreeing to
/// stop, so it must not be counted against the model.
const REQUEST_CANCELLED: i64 = -32800;

/// Session modes worth asking for, best first.
///
/// A server that offers one of these is told to use it. The names differ between
/// implementations and none of them is in the spec as a required value, so this
/// is a preference list rather than a constant -- and a server offering none of
/// them is not refused, because the gates below restrain it either way.
const RESTRAINTS: [&str; 4] = ["plan", "readonly", "read-only", "ask"];

enum In {
    Prompt(String),
    /// Stop the turn in flight.
    Cancel,
    /// A message from the child of attempt `gen`.
    Msg(u64, Value),
    /// The child of attempt `gen` closed its output.
    Eof(u64),
}

pub struct Ask {
    tx: Sender<In>,
    rx: Receiver<Event>,
}

/// What to try, best first: a model, and the server that runs it.
///
/// Comes from the hourly check rather than straight off disk, so a session
/// starts on a tier that was answering minutes ago instead of discovering a
/// dead one at the visitor's expense. See health.rs.
pub fn plan() -> Vec<Shot> {
    crate::health::plan()
}

impl Ask {
    /// Start the agent. Returns immediately; `Ready` or `Failed` arrives on the
    /// channel later. Called the first time anyone opens the section, so the
    /// portfolio does not spawn a language model to show a landing page.
    /// `board` is the token the tool server addresses this session's screen by.
    /// `None` means no tools are offered -- which is what every test that drives
    /// this module without a page gets, and what a deployment with the tool
    /// server down gets.
    pub fn spawn(context: String, board: Option<String>) -> Ask {
        let (tx_in, rx_in) = channel::<In>();
        let (tx_out, rx_out) = channel::<Event>();
        // The worker keeps a sender so it can hand one to each attempt's
        // reader thread; the UI keeps the original.
        let mine = tx_in.clone();

        thread::spawn(move || {
            let list = plan();
            if list.is_empty() {
                let _ = tx_out.send(Event::Failed(
                    "no models configured -- data/models.txt is empty".into(),
                ));
                return;
            }

            let mut state = Session::new(context, board);
            for (gen, shot) in list.iter().enumerate() {
                let gen = gen as u64;
                let last = gen as usize + 1 == list.len();
                match attempt(gen, shot, &rx_in, &mine, &tx_out, &mut state) {
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

    /// Ask the agent to stop the turn it is on.
    ///
    /// Cooperative, per ACP: the agent may finish anyway, and either way it
    /// still answers the request. So this is a request rather than a kill, and
    /// the UI keeps waiting until the reply arrives.
    pub fn cancel(&self) {
        let _ = self.tx.send(In::Cancel);
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
    /// The token that addresses this session's screen, for the tool server.
    /// `None` when nothing registered one, which is every test that drives this
    /// module without a page attached.
    board: Option<String>,
}

impl Session {
    fn new(context: String, board: Option<String>) -> Session {
        Session { context, first: true, turns: 0, tools: 0, pending: None, board }
    }
}

enum Outcome {
    /// The UI went away. Nothing left to do.
    Closed,
    /// This model is no good. Try the next.
    Broken(String),
}

fn write_line<W: Write>(w: &mut W, s: &str) -> bool {
    w.write_all(s.as_bytes()).is_ok() && w.write_all(b"\n").is_ok() && w.flush().is_ok()
}

/// The `initialize` request, with the gates as `clientCapabilities`.
///
/// Shared with the hourly check in `health.rs`, which must perform the same
/// handshake or it is not a probe of the thing being asked about.
pub fn initialize_request(id: i64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"initialize","params":{{"protocolVersion":{},"clientCapabilities":{}}}}}"#,
        PROTOCOL_VERSION,
        gates::client_capabilities()
    )
}

/// `session/new`, rooted where the portfolio's own data lives.
pub fn session_new_request(id: i64, tools: Option<&str>) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    // Our own tools, or none. `None` is not a degraded mode to hide: an agent
    // that cannot reach an HTTP MCP server gets an empty list and answers in
    // words, which is what it did before any of this existed.
    // `headers` is **required** on the http variant, not optional -- the schema
    // says `required: ["name","url","headers"]`. Leaving it out is what took the
    // whole ask section down: every `session/new` carrying tools came back
    // `Invalid params` while the hourly health check, which sends no tools, kept
    // reporting the tier as healthy. An empty list is the right value; there is
    // nothing to authenticate to a server inside this process.
    let servers = match tools {
        Some(url) => format!(
            r#"[{{"type":"http","name":{},"url":{},"headers":[]}}]"#,
            json::quote(crate::mcp::SERVER_NAME),
            json::quote(url)
        ),
        None => "[]".to_string(),
    };
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"session/new","params":{{"cwd":{},"mcpServers":{servers}}}}}"#,
        json::quote(&cwd.to_string_lossy())
    )
}

/// Whether the agent will accept an MCP server reached over HTTP.
///
/// Read rather than assumed, and this is the whole reason the tools can live in
/// this process at all. ACP has three MCP transports; `http` means a URL, which
/// means the session that owns the screen can answer the tool call itself
/// instead of a second copy of this binary doing it and having to shout the
/// answer back. An agent that does not advertise it gets no tools, is logged
/// saying so, and falls back to prose.
pub fn takes_http_tools(hello: &Value) -> bool {
    hello
        .get("agentCapabilities")
        .and_then(|c| c.get("mcpCapabilities"))
        .and_then(|m| m.get("http"))
        .and_then(|h| h.as_bool())
        .unwrap_or(false)
}

/// One way an agent will accept being logged in, as advertised by `initialize`.
#[derive(Debug, Clone)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    /// What a person would have to do. Copilot's is "Run `copilot login` in the
    /// terminal", which is the single most useful string in the exchange when
    /// the tier will not start -- so it is carried out to the log rather than
    /// dropped.
    pub description: String,
}

/// The ways this agent says it can be authenticated.
pub fn auth_methods(res: &Value) -> Vec<AuthMethod> {
    let Some(list) = res.get("authMethods").and_then(|m| m.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(|v| v.as_str())?.to_string();
            Some(AuthMethod {
                id,
                name: m.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                description: m
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

pub fn authenticate_request(id: i64, method_id: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"authenticate","params":{{"methodId":{}}}}}"#,
        json::quote(method_id)
    )
}

/// How a server lets us hold a session back, if it does at all.
///
/// Two shapes, because implementations differ and both are in the spec's orbit.
/// `session/set_mode` is ACP's own; `session/set_config_option` is what opencode
/// actually advertises -- the recorded reply in the tests below carries a
/// `configOptions` entry with `plan` among its values and no `modes` field at
/// all. A client that only knew the first would silently fail to restrain the
/// one server this box has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restraint {
    Mode(String),
    Config { id: String, value: String },
}

impl Restraint {
    /// The word for the screen.
    pub fn label(&self) -> &str {
        match self {
            Restraint::Mode(m) => m,
            Restraint::Config { value, .. } => value,
        }
    }

    pub fn request(&self, id: i64, sid: &str) -> String {
        match self {
            Restraint::Mode(m) => format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"session/set_mode","params":{{"sessionId":{},"modeId":{}}}}}"#,
                json::quote(sid),
                json::quote(m)
            ),
            Restraint::Config { id: opt, value } => format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"session/set_config_option","params":{{"sessionId":{},"optionId":{},"value":{}}}}}"#,
                json::quote(sid),
                json::quote(opt),
                json::quote(value)
            ),
        }
    }
}

/// Pick the safest thing the `session/new` reply says is available.
///
/// `None` means the server offered nothing recognisable, which is allowed: the
/// gates in this file are what actually restrain it, and refusing every server
/// that names its modes differently would leave the section with nothing to
/// talk to.
pub fn restraint(res: &Value) -> Option<Restraint> {
    // ACP's own: modes.availableModes[].id
    if let Some(list) = res.get("modes").and_then(|m| m.get("availableModes")).and_then(|a| a.as_array())
    {
        let ids: Vec<String> = list
            .iter()
            .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(str::to_string))
            .collect();
        if let Some(pick) = prefer(&ids) {
            return Some(Restraint::Mode(pick));
        }
    }
    // opencode's: configOptions[] with id "mode" and options[].value
    let opts = res.get("configOptions").and_then(|o| o.as_array())?;
    for o in opts {
        let id = o.get("id").and_then(|i| i.as_str()).unwrap_or("");
        if id != "mode" {
            continue;
        }
        let values: Vec<String> = o
            .get("options")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| {
                        v.get("value")
                            .and_then(|x| x.as_str())
                            .or_else(|| v.as_str())
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(pick) = prefer(&values) {
            return Some(Restraint::Config { id: id.to_string(), value: pick });
        }
    }
    None
}

/// The first of `RESTRAINTS` that appears in `offered`, matched case-blind.
fn prefer(offered: &[String]) -> Option<String> {
    RESTRAINTS.iter().find_map(|want| {
        offered.iter().find(|got| got.eq_ignore_ascii_case(want)).cloned()
    })
}

/// Run one model until it finishes the conversation or fails.
fn attempt(
    gen: u64,
    shot: &Shot,
    rx: &Receiver<In>,
    tx_in: &Sender<In>,
    tx: &Sender<Event>,
    state: &mut Session,
) -> Outcome {
    // Our tool server for this session, if there is a screen to draw on. Needed
    // before the spawn because some servers are told where it is in their
    // environment rather than in `session/new`.
    let ours = state.board.as_deref().and_then(crate::mcp::url_for);
    let mut child = match shot.server.spawn_command(&shot.model, ours.as_deref()).spawn() {
        Ok(c) => c,
        Err(e) => {
            return Outcome::Broken(format!(
                "could not start `{}`: {e}",
                shot.server.label()
            ))
        }
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
    let out = converse(gen, shot, &mut w, rx, tx, state);
    let _ = child.kill();
    let _ = child.wait();
    out
}

/// Ask for a session and wait for the answer.
fn open_session<W: Write>(
    gen: u64,
    w: &mut W,
    rx: &Receiver<In>,
    state: &mut Session,
    id: i64,
    tools: Option<&str>,
) -> Result<Option<Value>, String> {
    if !write_line(w, &session_new_request(id, tools)) {
        return Err("the agent closed its input".into());
    }
    wait_for(gen, rx, id, state)
}

fn converse(
    gen: u64,
    shot: &Shot,
    w: &mut ChildStdin,
    rx: &Receiver<In>,
    tx: &Sender<Event>,
    state: &mut Session,
) -> Outcome {
    // 1. initialize, carrying the gates.
    if !write_line(w, &initialize_request(1)) {
        return Outcome::Broken("the agent closed its input".into());
    }
    let hello = match wait_for(gen, rx, 1, state) {
        Ok(v) => v,
        Err(e) => return Outcome::Broken(e),
    };
    let version = hello
        .as_ref()
        .and_then(|r| r.get("protocolVersion"))
        .and_then(|v| v.as_f64())
        .map(|v| v as i64)
        .unwrap_or(PROTOCOL_VERSION);

    // 2. a session -- authenticating first if the agent will not open one
    //    otherwise. Copilot refuses `session/new` outright with
    //    "Authentication required" and advertises how to fix it in
    //    `initialize`; without this the tier fails and the next one is tried,
    //    which is graceful and tells nobody anything.
    // The tool server, if this agent can reach one and this session has a
    // screen registered to draw on.
    // Two routes, and a server takes one of them. `session/new` is the
    // protocol's own and needs the agent to advertise `mcpCapabilities.http`;
    // the environment is for an agent that does not, and was already handed the
    // address at spawn -- see `attempt`.
    let ours = state.board.as_deref().and_then(crate::mcp::url_for);
    let by_env = shot.server.tools_by_env();
    let tools = match (by_env, hello.as_ref().is_some_and(takes_http_tools)) {
        (true, _) => None,
        (false, true) => ours.clone(),
        (false, false) => None,
    };
    // Whether the agent has our tools at all, which is a different question
    // from which route they took. `ask.rs` reads it to switch off the keyword
    // guess, so getting it wrong here is a map that appears unasked for.
    let handed = match by_env {
        true => ours.clone(),
        false => tools.clone(),
    };
    match (&handed, by_env, hello.as_ref().is_some_and(takes_http_tools)) {
        (Some(_), true, _) => eprintln!(
            "portfolio: `{}` was handed our tools in its environment",
            shot.server.label()
        ),
        (Some(_), false, _) => {
            eprintln!("portfolio: offering our tools to `{}`", shot.server.label())
        }
        (None, _, true) => {}
        (None, false, false) => eprintln!(
            "portfolio: `{}` takes no http mcp server, so it gets no tools",
            shot.server.label()
        ),
        (None, true, _) => {}
    }

    let methods = hello.as_ref().map(auth_methods).unwrap_or_default();
    let mut opened = open_session(gen, w, rx, state, 2, tools.as_deref());

    // A session refused *with our tools attached* is retried without them before
    // anything else is concluded. Our own extras must never be able to take the
    // section down: this exact failure did, and it was invisible because the
    // health check sends no tools and so went on reporting the tier as fine
    // while every real session died. It also sent the diagnosis somewhere
    // useless -- the next thing tried was `authenticate`, and `Invalid params`
    // is emphatically not an authentication problem.
    let mut offered = handed.clone();
    if let Err(why) = &opened {
        // Only the `session/new` route can be retried without them. Tools that
        // travelled in the environment were configured before the process
        // started, so there is nothing to take back out of the request.
        if tools.is_some() {
            eprintln!(
                "portfolio: `{}` refused a session with our tools attached ({why}); \
                 retrying without them -- the agent answers, the map does not",
                shot.server.label()
            );
            offered = None;
            opened = open_session(gen, w, rx, state, 3, None);
        }
    }

    if let (Err(why), Some(m)) = (&opened, methods.first()) {
        eprintln!(
            "portfolio: `{}` would not open a session ({why}); trying authenticate `{}`",
            shot.server.label(),
            m.id
        );
        if write_line(w, &authenticate_request(4, &m.id))
            && wait_for(gen, rx, 4, state).is_ok()
        {
            opened = open_session(gen, w, rx, state, 5, offered.as_deref());
        }
        if let Err(why) = &opened {
            // Carefully worded, because the obvious wording was wrong once and
            // cost an evening. An agent that says "Authentication required" is
            // not necessarily one that has never been logged in: Copilot
            // revalidates a perfectly good token against api.github.com on every
            // session, and when it cannot reach it, it reports exactly the same
            // thing. Telling the operator to run `copilot login` when they
            // already had sends them to fix something that is not broken.
            //
            // So: report what the agent said, offer what it offered, and do not
            // assert which of the two it is.
            let hint = if m.description.is_empty() { &m.name } else { &m.description };
            eprintln!(
                "portfolio: `{}` refused a session: {why}. It offers `{}`{}. \
                 If it is already logged in, check it can reach its own provider \
                 -- the same message covers a token that is missing and a token \
                 that could not be verified.",
                shot.server.label(),
                m.id,
                if hint.is_empty() { String::new() } else { format!(" ({hint})") },
            );
            return Outcome::Broken(format!("would not authenticate ({why})"));
        }
    }
    let res = match opened {
        Ok(Some(v)) => v,
        Ok(None) => return Outcome::Broken("the agent opened no session".into()),
        Err(e) => return Outcome::Broken(e),
    };
    let Some(sid) = res.get("sessionId").and_then(|v| v.as_str()).map(str::to_string) else {
        return Outcome::Broken("the agent opened no session".into());
    };

    // 3. hold it back, if it says it can be held back. A server that offers a
    //    restraint and then refuses to apply it is not used at all -- that is a
    //    server disagreeing with itself, and the safe reading is to walk away.
    //    A server that offers none is used, restrained by the gates alone.
    let held = restraint(&res);
    if let Some(r) = &held {
        if !write_line(w, &r.request(3, &sid)) || wait_for(gen, rx, 3, state).is_err() {
            return Outcome::Broken(format!("would not enter {} mode", r.label()));
        }
    }

    // The model is deliberately not in here. Which tier is answering is already
    // public -- the wait says "waking github copilot" -- but which model inside
    // it is somebody's billing detail, and `--probe` is where that belongs.
    let _ = tx.send(Event::Ready(Ready {
        tier: shot.tier.clone(),
        server: shot.server.label().to_string(),
        version,
        mode: held.as_ref().map(|r| r.label().to_string()).unwrap_or_default(),
        tools: offered.is_some(),
    }));

    let mut id = 10i64;

    loop {
        // A question left over from a model that died mid-answer goes first.
        let q = match state.pending.take() {
            Some(q) => q,
            None => match rx.recv() {
                Ok(In::Prompt(q)) => q,
                // Nothing is running, so there is nothing to stop.
                Ok(In::Cancel) => continue,
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

        if state.turns >= GATES.turns {
            let _ = tx.send(Event::Chunk(format!(
                "That is {} questions, which is where this stops. \
                 It runs on someone's account. Reconnect for a fresh session.",
                GATES.turns
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
fn stream<W: Write>(
    gen: u64,
    rx: &Receiver<In>,
    tx: &Sender<Event>,
    w: &mut W,
    id: i64,
    state: &mut Session,
) -> Result<(), String> {
    let mut stopping = false;
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
            In::Cancel => {
                // Cooperative and idempotent-ish: asking twice is harmless, but
                // there is no reason to.
                if GATES.cancel && !stopping {
                    stopping = true;
                    write_line(
                        w,
                        &format!(
                            r#"{{"jsonrpc":"2.0","method":"$/cancel_request","params":{{"requestId":{id}}}}}"#
                        ),
                    );
                }
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
                        let code =
                            e.get("code").and_then(|c| c.as_f64()).map(|c| c as i64).unwrap_or(0);
                        // The peer agreeing to stop is not the peer failing.
                        // Counting it as a failure would drop a working tier
                        // every time somebody pressed escape.
                        if code == REQUEST_CANCELLED || stopping {
                            let _ = tx.send(Event::Cancelled);
                            return Ok(());
                        }
                        let m = e.get("message").and_then(|m| m.as_str()).unwrap_or("failed");
                        return Err(m.to_string());
                    }
                    // A cancelled turn that finished anyway is still cancelled
                    // as far as the screen is concerned -- the visitor asked
                    // for it to stop and should not be handed a wall of text.
                    let _ = tx.send(if stopping { Event::Cancelled } else { Event::Done });
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
/// This is the gate that is ours. The handshake's capabilities and the server's
/// tool policy are both real, and both are somebody else's code -- one upstream
/// rename from meaning nothing. So every inbound request is checked again here,
/// against the same table.
///
/// Three shapes of answer:
///
/// - A permission request is answered by name and budget. Granted only if the
///   tool is open in `gates.rs` and the session has calls left. Anything else is
///   refused without being put to the visitor, because "approve this tool call"
///   is not a question a portfolio should be asking strangers.
/// - A method behind a shut gate, or one we do not implement, gets a JSON-RPC
///   **error**. Not `{}`: an empty result is a success, and an agent told that
///   `terminal/create` succeeded will go on to read from the terminal it thinks
///   it has.
/// - A notification -- no `id`, including everything `$/`-prefixed -- is
///   dropped in silence, which is what the protocol asks for. Answering an
///   optional notification with method-not-found is itself a protocol error.
fn answer_request<W: Write>(
    w: &mut W,
    v: &Value,
    tx: &Sender<Event>,
    state: &mut Session,
) -> bool {
    let Some(method) = v.get("method").and_then(|m| m.as_str()) else { return false };
    // No id: a notification. Nothing to answer, and saying so would be wrong.
    let Some(id) = v.get("id").and_then(|i| i.as_f64()) else { return false };
    let id = id as i64;

    match gates::verdict(method) {
        Verdict::Open => {}
        Verdict::Shut(gate) => {
            refuse(w, tx, id, method, &format!("{method} is not available here ({gate} is off)"));
            return true;
        }
        Verdict::Unimplemented => {
            refuse(w, tx, id, method, &format!("{method} is not implemented by this client"));
            return true;
        }
    }

    // The only open one: a permission request.
    let p = v.get("params");
    let call = p.and_then(|p| p.get("toolCall"));
    // `name` is the machine name and the thing worth gating on -- ACP's own
    // "read_file" rather than "Read configuration". It is optional, so `kind`
    // and then `title` stand in; a title is prose and grants nothing by itself,
    // which is `gates::tool_open`'s business rather than ours.
    // Exactly one field decides, and prose never overrules a machine name. An
    // earlier version let any field veto the grant, which reads as safer and is
    // not: with `read` and `view` shut, a perfectly good `web_fetch` whose title
    // happened to say "Reading https://…" was refused. The `name` is the field
    // the agent will actually execute; the title is a label for humans.
    //
    // With no machine name the title is all there is, and it is judged as prose
    // -- which almost always means refused, because `tool_open` wants a tool
    // name and a sentence is not one. That is the safe direction and the reason
    // the fallback is allowed to be this blunt.
    // `kind` is **not** a name and must never be read as one. ACP's `ToolCall`
    // has no `name` field at all -- `toolCallId` and `title` are the only
    // required ones -- and `kind` is a fixed category: read, edit, delete,
    // move, search, execute, think, fetch, switch_mode, other. `name` is a
    // non-standard extra that opencode sends and Copilot does not.
    //
    // Reading `kind` as a name is what refused our own map tools. Copilot
    // follows the spec, so its permission request carried no `name`, and `kind`
    // came through as `other` -- which is not a tool anybody has ever heard of,
    // so the gate said no and the model reported that it had no map tools.
    let tool = ["name", "title"]
        .into_iter()
        .filter_map(|k| call.and_then(|t| t.get(k)).and_then(|t| t.as_str()))
        .find(|s| !s.is_empty())
        .unwrap_or("");
    // The category, used only to refuse. A tool that says it edits, deletes,
    // moves or executes is refused whatever it calls itself -- this is the one
    // thing `kind` is good for, and it is a veto rather than a permit.
    let kind = call.and_then(|t| t.get("kind")).and_then(|k| k.as_str()).unwrap_or("");
    let dangerous = matches!(kind, "edit" | "delete" | "move" | "execute");
    let named = !dangerous && gates::tool_open(tool);
    if !named {
        // The whole request, when we say no. A refusal we cannot explain is the
        // expensive kind: this gate turned our own tools away for an entire
        // deploy and the only thing on screen was the word `other`.
        eprintln!(
            "portfolio: refusing a tool call -- title `{tool}`, kind `{kind}`: {}",
            call.map(|c| format!("{c:?}")).unwrap_or_default()
        );
    }
    let budget = state.tools < GATES.tool_calls;
    let body = if named && budget {
        state.tools += 1;
        // The option id the agent offered for "yes". Picking the first
        // allow-shaped one rather than inventing a name, because the set is the
        // agent's to define.
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
        format!(r#"{{"outcome":{{"outcome":"selected","optionId":{}}}}}"#, json::quote(&opt))
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
    };
    write_line(w, &format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{body}}}"#));
    true
}

/// Refuse a request properly, and say so on screen.
///
/// The row matters as much as the reply. A gate that shuts silently looks
/// exactly like a gate that is not there, and the one thing this section is
/// really showing is what the agent is and is not allowed to do.
fn refuse<W: Write>(w: &mut W, tx: &Sender<Event>, id: i64, method: &str, why: &str) {
    let _ = tx.send(Event::Tool(Call {
        id: format!("gate-{id}"),
        title: method.to_string(),
        status: Status::Refused,
        detail: why.to_string(),
    }));
    write_line(
        w,
        &format!(
            r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{METHOD_NOT_FOUND},"message":{}}}}}"#,
            json::quote(why)
        ),
    );
}

/// Block until the response with `id` arrives, forwarding nothing.
///
/// A question that turns up mid-handshake is kept rather than dropped. The UI
/// cannot currently send one -- it will not accept a question until `Ready` --
/// but this used to swallow it silently, and a visitor's question disappearing
/// because of when it arrived is not a failure anybody would think to look for.
fn wait_for(
    gen: u64,
    rx: &Receiver<In>,
    id: i64,
    state: &mut Session,
) -> Result<Option<Value>, String> {
    loop {
        match rx.recv() {
            Err(_) => return Err("the UI went away".into()),
            Ok(In::Eof(g)) if g == gen => return Err("stopped during the handshake".into()),
            Ok(In::Prompt(q)) => {
                if state.pending.is_none() {
                    state.pending = Some(q);
                }
            }
            Ok(In::Eof(_)) | Ok(In::Cancel) => {}
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
        let v = json::parse(REAL_SESSION_NEW).unwrap();
        let sid = v.get("result").unwrap().get("sessionId").unwrap().as_str();
        assert_eq!(sid, Some("ses_feaa71059ffenpcDdN63WtENkB"));
    }

    /// The capture above, which is the whole reason `Restraint` has two shapes:
    /// opencode advertises its modes as `configOptions`, not as ACP's `modes`.
    /// A client that only looked for `modes` would find nothing and leave the
    /// one server this box actually has entirely unrestrained.
    const REAL_SESSION_NEW: &str = r#"{"jsonrpc":"2.0","id":2,"result":{
        "sessionId":"ses_feaa71059ffenpcDdN63WtENkB",
        "configOptions":[{"id":"mode","name":"Session Mode","currentValue":"build",
        "options":[{"value":"build"},{"value":"plan"}]}]}}"#;

    #[test]
    fn the_real_reply_is_restrained_through_its_config_option() {
        let v = json::parse(REAL_SESSION_NEW).unwrap();
        let r = restraint(v.get("result").unwrap()).expect("nothing to restrain with");
        assert_eq!(r, Restraint::Config { id: "mode".into(), value: "plan".into() });
        assert_eq!(r.label(), "plan");
        let req = r.request(3, "ses_1");
        let sent = json::parse(&req).expect("valid JSON");
        assert_eq!(
            sent.get("method").and_then(|m| m.as_str()),
            Some("session/set_config_option")
        );
    }

    #[test]
    fn acps_own_modes_are_preferred_and_used_as_a_mode() {
        let v = upd(
            r#"{"sessionId":"s","modes":{"currentModeId":"build",
               "availableModes":[{"id":"build"},{"id":"plan"}]}}"#,
        );
        let r = restraint(&v).expect("nothing to restrain with");
        assert_eq!(r, Restraint::Mode("plan".into()));
        let sent = json::parse(&r.request(3, "s")).expect("valid JSON");
        assert_eq!(sent.get("method").and_then(|m| m.as_str()), Some("session/set_mode"));
    }

    /// A server that offers nothing recognisable is still used. The gates are
    /// what restrain it; refusing here would mean only opencode ever worked.
    #[test]
    fn a_server_offering_no_modes_is_not_refused() {
        assert_eq!(restraint(&upd(r#"{"sessionId":"s"}"#)), None);
        assert_eq!(
            restraint(&upd(r#"{"sessionId":"s","modes":{"availableModes":[{"id":"build"}]}}"#)),
            None
        );
    }

    #[test]
    fn the_safest_offered_mode_wins_whatever_its_case() {
        assert_eq!(
            restraint(&upd(
                r#"{"modes":{"availableModes":[{"id":"yolo"},{"id":"ReadOnly"},{"id":"Plan"}]}}"#
            )),
            Some(Restraint::Mode("Plan".into())),
            "plan is preferred over readonly"
        );
    }

    #[test]
    fn the_handshake_advertises_exactly_what_the_gates_say() {
        let v = json::parse(&initialize_request(1)).expect("valid JSON");
        let p = v.get("params").expect("no params");
        assert_eq!(p.get("protocolVersion").and_then(|x| x.as_f64()), Some(1.0));
        let fs = p.get("clientCapabilities").and_then(|c| c.get("fs")).expect("no fs");
        assert_eq!(fs.get("readTextFile").and_then(|x| x.as_bool()), Some(GATES.fs_read));
        assert_eq!(fs.get("writeTextFile").and_then(|x| x.as_bool()), Some(GATES.fs_write));
        assert_eq!(
            p.get("clientCapabilities").and_then(|c| c.get("terminal")).and_then(|x| x.as_bool()),
            Some(GATES.terminal)
        );
    }

    /// The MCP server we name has every field the protocol says is required.
    ///
    /// This is the test that was missing, and its absence was an outage. ACP's
    /// `McpServerHttp` requires `name`, `url` **and** `headers`; the first
    /// version of this sent the first two, so every `session/new` carrying tools
    /// came back `Invalid params` and the ask section was dead for every
    /// visitor. Nothing caught it: the hourly health check sends no tools, so it
    /// went on reporting the tier as healthy the whole time.
    #[test]
    fn the_mcp_server_we_offer_has_the_fields_the_schema_demands() {
        let req = session_new_request(2, Some("http://127.0.0.1:9/mcp/abc"));
        let v = json::parse(&req).expect("not json");
        let servers = v
            .get("params")
            .and_then(|p| p.get("mcpServers"))
            .and_then(|m| m.as_array())
            .expect("no mcpServers");
        assert_eq!(servers.len(), 1);
        let one = &servers[0];
        // Straight from schema/v1: required = ["name", "url", "headers"], plus
        // the `type` that selects the http variant at all.
        for key in ["type", "name", "url", "headers"] {
            assert!(one.get(key).is_some(), "`{key}` is required and missing:\n{req}");
        }
        assert_eq!(one.get("type").and_then(|t| t.as_str()), Some("http"));
        assert_eq!(one.get("url").and_then(|u| u.as_str()), Some("http://127.0.0.1:9/mcp/abc"));
        assert!(one.get("headers").and_then(|h| h.as_array()).is_some(), "headers is not a list");

        // And with no tools it is an empty list, which is what it always was.
        let bare = session_new_request(2, None);
        let v = json::parse(&bare).expect("not json");
        let servers = v
            .get("params")
            .and_then(|p| p.get("mcpServers"))
            .and_then(|m| m.as_array())
            .expect("no mcpServers");
        assert!(servers.is_empty(), "a session with no tools named one anyway:\n{bare}");
    }

    /// Only an agent that says it can take an http server is offered one.
    #[test]
    fn http_tools_are_offered_only_where_they_are_advertised() {
        let yes = json::parse(
            r#"{"agentCapabilities":{"mcpCapabilities":{"http":true,"sse":false}}}"#,
        )
        .unwrap();
        assert!(takes_http_tools(&yes));
        for no in [
            r#"{"agentCapabilities":{"mcpCapabilities":{"http":false}}}"#,
            r#"{"agentCapabilities":{"mcpCapabilities":{}}}"#,
            r#"{"agentCapabilities":{}}"#,
            r#"{}"#,
            // A string rather than a bool: an agent's malformed capability must
            // read as "no", not panic and not "yes".
            r#"{"agentCapabilities":{"mcpCapabilities":{"http":"yes"}}}"#,
        ] {
            let v = json::parse(no).unwrap();
            assert!(!takes_http_tools(&v), "offered tools on {no}");
        }
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

    /// Put one agent-to-client request through the real `answer_request` and
    /// return what went back down the pipe, plus what reached the screen.
    ///
    /// This is the gate that matters and, until it was made generic over `Write`,
    /// the one thing here that could not be tested at all -- it took a
    /// `ChildStdin`, so proving it refused anything needed a live agent, and this
    /// box has never had one. Every claim in the module docs about shut gates was
    /// a claim about code that had only ever run its failure path.
    fn asked(req: &str) -> (Vec<Value>, Vec<Event>) {
        let mut out: Vec<u8> = Vec::new();
        let (tx, rx) = channel();
        let mut state = Session::new(String::new(), None);
        let v = json::parse(req).expect("the request itself is not valid JSON");
        answer_request(&mut out, &v, &tx, &mut state);
        drop(tx);
        let sent = String::from_utf8(out).expect("not utf8");
        let replies = sent
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| json::parse(l).unwrap_or_else(|| panic!("reply is not JSON: {l}")))
            .collect();
        (replies, rx.iter().collect())
    }

    fn error_code(v: &Value) -> Option<i64> {
        v.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_f64()).map(|c| c as i64)
    }

    /// The bug this whole change exists to fix. A shut gate must answer with an
    /// error; `{}` is a *success*, and an agent told that `terminal/create`
    /// worked will go on to read from the terminal it thinks it now has.
    #[test]
    fn a_shut_gate_answers_with_an_error_and_never_an_empty_result() {
        for method in [
            "fs/read_text_file",
            "fs/write_text_file",
            "terminal/create",
            "terminal/output",
            "terminal/kill",
            "elicitation/create",
        ] {
            let req = format!(r#"{{"jsonrpc":"2.0","id":7,"method":"{method}","params":{{}}}}"#);
            let (replies, events) = asked(&req);
            assert_eq!(replies.len(), 1, "{method} was not answered exactly once");
            let r = &replies[0];
            assert_eq!(error_code(r), Some(METHOD_NOT_FOUND), "{method}: {r:?}");
            assert!(r.get("result").is_none(), "{method} got a result as well");
            assert_eq!(r.get("id").and_then(|i| i.as_f64()), Some(7.0));
            // And it is visible, because a gate that shuts silently looks
            // exactly like a gate that was never there.
            assert!(
                matches!(&events[0], Event::Tool(c) if c.status == Status::Refused),
                "{method} was refused without saying so: {events:?}"
            );
        }
    }

    #[test]
    fn a_method_nobody_implements_is_refused_the_same_way() {
        let (replies, _) = asked(r#"{"jsonrpc":"2.0","id":1,"method":"fs/chmod","params":{}}"#);
        assert_eq!(error_code(&replies[0]), Some(METHOD_NOT_FOUND));
    }

    /// A notification has no id, so there is nothing to answer -- and answering
    /// an optional `$/` notification with method-not-found is itself a protocol
    /// error. It must go out in silence.
    #[test]
    fn a_notification_is_dropped_without_a_reply() {
        for n in [
            r#"{"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":3}}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#,
        ] {
            let (replies, _) = asked(n);
            assert!(replies.is_empty(), "{n} was answered: {replies:?}");
        }
    }

    /// An open tool, named the way ACP names one, is granted using the option id
    /// the agent itself offered rather than an invented string.
    #[test]
    fn an_allowed_tool_is_granted_with_the_agents_own_option_id() {
        let (replies, _) = asked(
            r#"{"jsonrpc":"2.0","id":5,"method":"session/request_permission","params":{
               "toolCall":{"toolCallId":"t1","name":"webfetch","title":"Fetch https://example.com"},
               "options":[{"optionId":"reject-once","kind":"reject_once"},
                          {"optionId":"allow-always","kind":"allow_always"}]}}"#,
        );
        let outcome = replies[0].get("result").and_then(|r| r.get("outcome")).expect("no outcome");
        assert_eq!(outcome.get("outcome").and_then(|o| o.as_str()), Some("selected"));
        assert_eq!(outcome.get("optionId").and_then(|o| o.as_str()), Some("allow-always"));
    }

    /// The one that matters. A shell is refused under either vocabulary, and a
    /// friendly title does not get it through.
    ///
    /// Note the third case: `name` wins over `kind`, so a shell announcing
    /// itself as `shell` is refused even where the kind sounds harmless.
    #[test]
    fn a_shell_is_refused_under_either_name() {
        for call in [
            r#"{"toolCallId":"t","name":"bash","title":"Run a command"}"#,
            r#"{"toolCallId":"t","name":"shell","title":"Check the tests"}"#,
            r#"{"toolCallId":"t","name":"bash","kind":"execute"}"#,
            // Title-only, and the title names a shell: refused as prose.
            r#"{"toolCallId":"t","title":"run bash for me"}"#,
        ] {
            let req = format!(
                r#"{{"jsonrpc":"2.0","id":9,"method":"session/request_permission","params":{{"toolCall":{call},"options":[{{"optionId":"ok","kind":"allow_once"}}]}}}}"#
            );
            let (replies, events) = asked(&req);
            let outcome =
                replies[0].get("result").and_then(|r| r.get("outcome")).expect("no outcome");
            assert_eq!(
                outcome.get("outcome").and_then(|o| o.as_str()),
                Some("cancelled"),
                "granted a shell: {call}"
            );
            assert!(
                events.iter().any(|e| matches!(e, Event::Tool(c) if c.status == Status::Refused)),
                "refused silently: {call}"
            );
        }
    }

    /// A permission request naming nothing at all is refused, not granted.
    #[test]
    fn a_permission_request_with_no_tool_named_is_refused() {
        let (replies, _) = asked(
            r#"{"jsonrpc":"2.0","id":2,"method":"session/request_permission","params":{
               "toolCall":{"toolCallId":"t"},"options":[{"optionId":"ok","kind":"allow_once"}]}}"#,
        );
        let outcome = replies[0].get("result").and_then(|r| r.get("outcome")).expect("no outcome");
        assert_eq!(outcome.get("outcome").and_then(|o| o.as_str()), Some("cancelled"));
    }

    /// The budget is a total across the session, and it actually stops.
    #[test]
    fn the_tool_budget_runs_out_and_then_refuses() {
        let mut out: Vec<u8> = Vec::new();
        let (tx, rx) = channel();
        let mut state = Session::new(String::new(), None);
        let req = json::parse(
            r#"{"jsonrpc":"2.0","id":4,"method":"session/request_permission","params":{
               "toolCall":{"toolCallId":"t","name":"webfetch"},
               "options":[{"optionId":"ok","kind":"allow_once"}]}}"#,
        )
        .unwrap();
        for _ in 0..GATES.tool_calls + 3 {
            answer_request(&mut out, &req, &tx, &mut state);
        }
        drop(tx);
        assert_eq!(state.tools, GATES.tool_calls, "the budget did not cap");
        let refused = rx
            .iter()
            .filter(|e| matches!(e, Event::Tool(c) if c.status == Status::Refused))
            .count();
        assert_eq!(refused, 3, "the calls past the budget were not refused");
    }

    /// A real `initialize` reply from `copilot --acp` 1.0.80, captured off the
    /// wire. Copilot refuses `session/new` with "Authentication required" until
    /// it has been logged in, and this is where it says so and how to fix it.
    const REAL_COPILOT_INIT: &str = r#"{"jsonrpc":"2.0","id":1,"result":{
        "protocolVersion":1,
        "agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":true}},
        "agentInfo":{"name":"Copilot","title":"Copilot","version":"1.0.80"},
        "authMethods":[{"id":"copilot-login","name":"Log in with Copilot CLI",
        "description":"Run `copilot login` in the terminal"}]}}"#;

    #[test]
    fn copilots_advertised_login_is_read_off_its_real_handshake() {
        let v = json::parse(REAL_COPILOT_INIT).unwrap();
        let res = v.get("result").unwrap();
        let m = auth_methods(res);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].id, "copilot-login");
        // The description is the actionable half and must survive to the log.
        assert_eq!(m[0].description, "Run `copilot login` in the terminal");
        // And its version negotiation is the one we speak.
        assert_eq!(res.get("protocolVersion").and_then(|v| v.as_f64()), Some(1.0));
    }

    /// An agent that needs nothing yields no methods, so the authenticate step
    /// is skipped rather than sent speculatively.
    #[test]
    fn an_agent_that_wants_no_login_advertises_none() {
        let v = upd(r#"{"protocolVersion":1,"agentCapabilities":{}}"#);
        assert!(auth_methods(&v).is_empty());
        // A malformed entry with no id is not a method we can name.
        let v = upd(r#"{"authMethods":[{"name":"nameless"}]}"#);
        assert!(auth_methods(&v).is_empty());
    }

    #[test]
    fn the_authenticate_request_names_the_method_it_was_offered() {
        let v = json::parse(&authenticate_request(4, "copilot-login")).expect("valid JSON");
        assert_eq!(v.get("method").and_then(|m| m.as_str()), Some("authenticate"));
        assert_eq!(
            v.get("params").and_then(|p| p.get("methodId")).and_then(|m| m.as_str()),
            Some("copilot-login")
        );
    }

    /// Copilot names its tools differently from opencode, and the gate has to
    /// hold under both vocabularies -- `web_fetch` is the same permission as
    /// `webfetch`, and `shell` is the same refusal as `bash`.
    #[test]
    fn copilots_tool_names_are_gated_the_same_as_opencodes() {
        for open in ["webfetch", "web_fetch", "websearch", "web_search"] {
            assert!(gates::tool_open(open), "{open} should be granted");
        }
        for shut in ["bash", "shell", "view", "read", "glob", "grep", "task", "str_replace"] {
            assert!(!gates::tool_open(shut), "{shut} should be refused");
            assert!(gates::tool_shut(shut), "{shut} should be named as shut");
        }
    }

    /// Cancellation is cooperative, so the agent still answers -- and the answer
    /// it is entitled to give is the standard cancellation error. Reading that as
    /// the model failing would drop a working tier every time somebody pressed
    /// escape, and the next question would go to a slower model for no reason.
    #[test]
    fn a_cancelled_turn_is_not_counted_against_the_model() {
        let (tx_in, rx_in) = channel::<In>();
        let (tx_out, rx_out) = channel::<Event>();
        let mut out: Vec<u8> = Vec::new();
        let mut state = Session::new(String::new(), None);
        tx_in
            .send(In::Msg(
                0,
                json::parse(
                    r#"{"jsonrpc":"2.0","id":11,"error":{"code":-32800,"message":"Request cancelled"}}"#,
                )
                .unwrap(),
            ))
            .unwrap();

        let r = stream(0, &rx_in, &tx_out, &mut out, 11, &mut state);
        assert!(r.is_ok(), "a cancellation was read as the model failing: {r:?}");
        drop(tx_out);
        let events: Vec<Event> = rx_out.iter().collect();
        assert!(matches!(events.first(), Some(Event::Cancelled)), "{events:?}");
    }

    /// Escape becomes a `$/cancel_request` naming the turn in flight, as a
    /// notification -- no id, because there is nothing to answer.
    #[test]
    fn stopping_sends_a_cancel_request_for_the_turn_in_flight() {
        let (tx_in, rx_in) = channel::<In>();
        let (tx_out, rx_out) = channel::<Event>();
        let mut out: Vec<u8> = Vec::new();
        let mut state = Session::new(String::new(), None);
        tx_in.send(In::Cancel).unwrap();
        // The agent finishes anyway, which it is allowed to do.
        tx_in
            .send(In::Msg(
                0,
                json::parse(r#"{"jsonrpc":"2.0","id":11,"result":{"stopReason":"end_turn"}}"#)
                    .unwrap(),
            ))
            .unwrap();

        assert!(stream(0, &rx_in, &tx_out, &mut out, 11, &mut state).is_ok());
        let sent = String::from_utf8(out).unwrap();
        let note = json::parse(sent.trim()).unwrap_or_else(|| panic!("not JSON: {sent}"));
        assert_eq!(note.get("method").and_then(|m| m.as_str()), Some("$/cancel_request"));
        assert_eq!(
            note.get("params").and_then(|p| p.get("requestId")).and_then(|i| i.as_f64()),
            Some(11.0)
        );
        assert!(note.get("id").is_none(), "a notification must carry no id: {sent}");

        // A turn the visitor stopped reads as stopped even if it completed, so
        // they are not handed a wall of text they asked not to receive.
        drop(tx_out);
        let events: Vec<Event> = rx_out.iter().collect();
        assert!(matches!(events.first(), Some(Event::Cancelled)), "{events:?}");
    }

    /// An agent that refuses our tools still gets a session.
    ///
    /// The regression this pins took the ask section down for every visitor:
    /// `session/new` carrying an `mcpServers` entry came back `Invalid params`
    /// -- the http variant requires `headers` and it was missing -- and the
    /// client concluded it was an authentication problem and gave up. The
    /// failure was invisible because the hourly health check names no server, so
    /// it kept reporting the tier as healthy while nothing worked.
    ///
    /// The rule that came out of it: our own extras may degrade, never break.
    /// A session refused *with* tools attached is retried without them before
    /// anything else is concluded, and the visitor gets an agent that answers in
    /// words rather than a section that reports every model as down.
    #[test]
fn tools_in_the_environment_are_not_named_in_the_request_and_still_count() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/fake_agent.py");
        let mut server = crate::servers::Server::default();
        // The peer refuses any `session/new` that names an MCP server. So this
        // one test proves both halves: the session opening at all says our
        // server was *not* named in the request, and `tools` being true says it
        // was handed over anyway -- which is what switches the keyword guess
        // off. Get either wrong and this fails.
        server.command_line(&format!("python3 {script} --refuse-mcp"));
        server.tools_line("env FAKE_MCP_HTTP");

        let shot = Shot { tier: "fake".into(), server, model: "fake/one".into() };
        let (tx_in, rx_in) = channel::<In>();
        let (tx_out, rx_out) = channel::<Event>();
        tx_in.send(In::Prompt("hello".into())).unwrap();

        let (dtx, _drx) = std::sync::mpsc::channel();
        crate::mcp::serve();
        let board = crate::mcp::register(dtx, None);
        let mut state = Session::new("context".into(), Some(board.clone()));
        let _ = attempt(0, &shot, &rx_in, &tx_in, &tx_out, &mut state);
        drop(tx_out);
        let events: Vec<Event> = rx_out.iter().collect();
        crate::mcp::forget(&board);

        let ready = events.iter().find_map(|e| match e {
            Event::Ready(r) => Some(r),
            _ => None,
        });
        let ready = ready.unwrap_or_else(|| panic!("no session opened at all: {events:?}"));
        assert!(
            ready.tools,
            "the agent was handed our tools and reported as having none, so the \
             keyword guess stays on and draws maps nobody asked for"
        );
    }

    #[test]
    fn tools_the_agent_will_not_take_do_not_cost_us_the_session() {        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/fake_agent.py");
        let mut server = crate::servers::Server::default();
        // The peer rejects any `session/new` that names an MCP server, which is
        // exactly what the real agents did. As an argument rather than an
        // environment variable: env vars are per-process and this suite is
        // threaded, and two tests fighting over one has already happened twice
        // in this project.
        server.command_line(&format!("python3 {script} --refuse-mcp"));

        let shot = Shot { tier: "fake".into(), server, model: "fake/one".into() };
        let (tx_in, rx_in) = channel::<In>();
        let (tx_out, rx_out) = channel::<Event>();
        tx_in.send(In::Prompt("hello".into())).unwrap();

        // A registered board is what makes the client offer tools at all.
        let (dtx, _drx) = std::sync::mpsc::channel();
        crate::mcp::serve();
        let board = crate::mcp::register(dtx, None);
        let mut state = Session::new("context".into(), Some(board.clone()));
        let _ = attempt(0, &shot, &rx_in, &tx_in, &tx_out, &mut state);
        drop(tx_out);
        let events: Vec<Event> = rx_out.iter().collect();
        crate::mcp::forget(&board);

        assert!(
            events.iter().any(|e| matches!(e, Event::Ready(_))),
            "a refused tool server cost us the whole session: {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, Event::Failed(_))),
            "reported as down when only the tools were refused: {events:?}"
        );
    }

    /// The whole client, against a real ACP server in another process.
    ///
    /// `scripts/fake_agent.py` is not a model, but it is a real peer: real pipes,
    /// real JSON-RPC, real interleaving of its requests with our responses. It
    /// is also the case that matters -- an agent that ignores the capabilities we
    /// advertised and asks for a file, a terminal and a shell regardless.
    ///
    /// It doubles as the proof that a server other than opencode works at all:
    /// it is reached by `command` and `Pin::None`, which is the path every
    /// non-opencode server in `models.txt` takes.
    #[test]
    fn the_client_talks_to_a_real_acp_server_and_refuses_it_everything() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/fake_agent.py");
        let mut server = crate::servers::Server::default();
        server.command_line(&format!("python3 {script}"));
        assert_eq!(server.pin, crate::servers::Pin::None, "not the generic path");

        let shot = Shot { tier: "fake".into(), server, model: "fake/one".into() };
        let (tx_in, rx_in) = channel::<In>();
        let (tx_out, rx_out) = channel::<Event>();
        tx_in.send(In::Prompt("what may you do?".into())).unwrap();

        let mut state = Session::new("context".into(), None);
        // Returns once the agent has answered and exited: the reader sees EOF and
        // the prompt loop gives up waiting for a second question.
        let _ = attempt(0, &shot, &rx_in, &tx_in, &tx_out, &mut state);
        drop(tx_out);
        let events: Vec<Event> = rx_out.iter().collect();

        // The handshake settled, on the mode the agent offered as a config option.
        let ready = events
            .iter()
            .find_map(|e| match e {
                Event::Ready(r) => Some(r),
                _ => None,
            })
            .unwrap_or_else(|| panic!("never got ready: {events:?}"));
        assert_eq!(ready.mode, "plan", "the session was not restrained");
        assert_eq!(ready.version, 1);
        assert_eq!(ready.server, "python3");
        assert_eq!(ready.tier, "fake");

        let said: String = events
            .iter()
            .filter_map(|e| match e {
                Event::Chunk(s) => Some(s.clone()),
                _ => None,
            })
            .collect();

        // What we advertised was the gate table, not a hard-coded literal.
        assert!(
            said.contains(r#""readTextFile":false"#) && said.contains(r#""terminal":false"#),
            "the handshake did not carry the gates: {said}"
        );

        // And what the agent was actually told when it asked anyway. The two
        // filesystem and terminal requests are method-not-found; the shell is a
        // cancelled permission; the web fetch is the one thing granted.
        let report = said.split("answered ").nth(1).unwrap_or_default();
        let v = json::parse(report).unwrap_or_else(|| panic!("no report in: {said}"));
        for (gate, want) in [
            ("fs.read", "error -32601"),
            ("terminal", "error -32601"),
            ("bash", "cancelled"),
            ("webfetch", "selected"),
        ] {
            assert_eq!(
                v.get(gate).and_then(|x| x.as_str()),
                Some(want),
                "{gate} was answered wrongly -- full report: {report}"
            );
        }

        // The refusals reached the screen rather than happening quietly.
        let refused: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Event::Tool(c) if c.status == Status::Refused => Some(c.title.clone()),
                _ => None,
            })
            .collect();
        for expect in ["fs/read_text_file", "terminal/create", "bash"] {
            assert!(
                refused.iter().any(|t| t.contains(expect)),
                "{expect} was refused invisibly -- shown: {refused:?}"
            );
        }
        assert!(events.iter().any(|e| matches!(e, Event::Done)), "the turn never finished");
    }

    #[test]
    fn the_plan_is_never_empty_and_every_shot_is_runnable() {
        let list = plan();
        assert!(!list.is_empty(), "no models configured");
        for s in &list {
            assert!(!s.model.starts_with('#'), "{} is a comment", s.model);
            assert!(!s.model.is_empty(), "a blank model in tier {}", s.tier);
            assert!(!s.server.label().is_empty(), "a tier with no server command");
        }
        // The plan follows the file's order, whatever that order currently is.
        // It used to assert that Copilot came first, which is a fact about
        // somebody's preference rather than about this code -- and it broke the
        // moment the tiers were reordered, which is what the file is *for*.
        let want: Vec<String> = crate::health::tiers().into_iter().map(|t| t.name).collect();
        let mut seen: Vec<String> = Vec::new();
        for s in &list {
            if seen.last() != Some(&s.tier) {
                seen.push(s.tier.clone());
            }
        }
        assert_eq!(seen, want, "the plan reordered the tiers");

        // Whichever tier names Copilot gets Copilot's shape: a flag for the
        // model and a flag for the allow-list. That *is* about this code.
        for s in list.iter().filter(|s| s.server.label() == "copilot") {
            assert_eq!(s.server.pin, crate::servers::Pin::Flag("--model".into()));
            assert_eq!(s.server.tool_flag.as_deref(), Some("--available-tools"));
        }
    }
}
