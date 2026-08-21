//! What a conversation is, independent of who is going to answer it.
//!
//! These types are the normalised middle. A provider's request builder reads
//! them and a provider's event parser writes them, so nothing above this file
//! has to know which endpoint answered -- which is the whole point, because a
//! session that switches models mid-conversation keeps the same history.
//!
//! **`content` is ordered and stays ordered.** One assistant message can hold
//! text, then a tool call, then more text: the model saying what it is about to
//! do, doing it, and reading the result. Sorting or grouping that vector throws
//! away the only record of what happened in what order.
//!
//! **`opaque` is not ours to read.** Providers hand back state they expect
//! returned verbatim -- OpenAI's encrypted reasoning is the case that forced
//! this field to exist. It has no schema, it is not portable between providers,
//! and dropping it turns the *next* request into a 400 rather than failing
//! where the mistake was made.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One piece of a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    /// Reasoning. Shown as marginalia rather than as the answer, and carried
    /// back to the provider through `opaque`.
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opaque: Option<Value>,
    },
    /// The model asking for a tool. `args` is whatever it produced, which is
    /// not necessarily valid against the tool's schema -- validation happens
    /// where the tool is run, so that a bad call can be reported as a tool
    /// failure the model can read rather than as a crash it cannot.
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    Image {
        /// base64, no data-url prefix.
        data: String,
        mime: String,
    },
}

impl Block {
    pub fn text(s: impl Into<String>) -> Block {
        Block::Text { text: s.into() }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Block::Text { text } => Some(text),
            _ => None,
        }
    }
}

/// Why the model stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    /// It finished saying what it had to say.
    End,
    /// It hit the output token ceiling.
    Length,
    /// It wants tools run, and expects to be called again with the results.
    ToolUse,
    /// It declined.
    Refusal,
    /// Something went wrong. `Assistant::error` says what.
    Error,
    /// We stopped it.
    Aborted,
}

impl Stop {
    /// Whether this turn expects the loop to come back with tool results.
    pub fn wants_tools(self) -> bool {
        self == Stop::ToolUse
    }
}

/// Tokens and money, as reported by the provider rather than estimated.
///
/// `cache_read` matters out of proportion to its size: it is the difference
/// between a long conversation costing linearly and costing quadratically, and
/// it is the number to look at first when a session gets expensive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// In USD. Zero when the provider does not price the model or we do not
    /// know the rate -- which is not the same as free, and is why this is not
    /// an `Option` anyone will remember to check.
    pub cost: f64,
}

impl Usage {
    /// Everything occupying the context window after this turn.
    pub fn context_tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    pub fn add(&mut self, other: &Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.cost += other.cost;
    }
}

/// One turn's answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Assistant {
    pub content: Vec<Block>,
    pub stop: Stop,
    #[serde(default)]
    pub usage: Usage,
    /// Set when `stop` is `Error`. The message stays in the history rather than
    /// becoming an `Err`, because a failed turn still happened: the provider
    /// saw the request, the visitor may have seen half an answer, and a history
    /// that omits it no longer matches either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque: Option<Value>,
}

impl Assistant {
    pub fn pending() -> Assistant {
        Assistant {
            content: Vec::new(),
            stop: Stop::End,
            usage: Usage::default(),
            error: None,
            response_id: None,
            opaque: None,
        }
    }

    pub fn failed(why: impl Into<String>) -> Assistant {
        Assistant {
            stop: Stop::Error,
            error: Some(why.into()),
            ..Assistant::pending()
        }
    }

    /// The tool calls in this message, in the order the model made them.
    pub fn tool_calls(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|b| match b {
            Block::ToolCall { id, name, args } => Some((id.as_str(), name.as_str(), args)),
            _ => None,
        })
    }

    /// Every text block run together. For logging and for tests; a UI should
    /// walk `content` instead, or it loses where the tool calls were.
    pub fn text(&self) -> String {
        self.content.iter().filter_map(Block::as_text).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Message {
    User {
        content: Vec<Block>,
    },
    Assistant(Assistant),
    /// One tool's answer. Separate from `User` because providers frame it
    /// differently and because the loop needs to pair it with its call.
    ToolResult {
        call_id: String,
        name: String,
        content: Vec<Block>,
        #[serde(default)]
        error: bool,
    },
}

impl Message {
    pub fn user(text: impl Into<String>) -> Message {
        Message::User {
            content: vec![Block::text(text)],
        }
    }
}

/// A tool as the *provider* needs to see it: a name, a sentence, and a schema.
///
/// Everything a client needs to draw it -- a title, a kind, an icon -- lives a
/// layer up. This struct is what goes on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema, written by hand. Providers in strict mode reject `$ref`
    /// and `definitions` and want `additionalProperties: false` with a
    /// complete `required`; writing that is less work than teaching a
    /// generator to produce it.
    pub schema: Value,
}

/// Everything the model is given.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Context {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
}

/// How hard to think, in the one vocabulary all the providers get mapped onto.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Off,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
}

/// Which wire protocol a model speaks. Not which company sells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Api {
    /// `POST /chat/completions`. Ollama Cloud, and every endpoint that calls
    /// itself OpenAI-compatible.
    OpenaiCompletions,
    /// `POST /responses`. Reasoning models, and the Codex flavour.
    OpenaiResponses,
    /// `POST /api/chat`. Ollama's own, which is not the compatibility shim: it
    /// streams newline-delimited JSON and keeps reasoning in its own field.
    OllamaChat,
}

/// Price per million tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Cost {
    pub fn of(&self, u: &Usage) -> f64 {
        let m = 1_000_000.0;
        (u.input as f64 * self.input
            + u.output as f64 * self.output
            + u.cache_read as f64 * self.cache_read
            + u.cache_write as f64 * self.cache_write)
            / m
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub api: Api,
    pub context_window: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub cost: Cost,
}

/// Where to send it and what to put in the headers.
///
/// Resolved before the request rather than looked up during it, so that a
/// missing credential is an error with a name attached instead of a 401 from
/// somebody else's server.
#[derive(Clone, Debug, Default)]
pub struct Endpoint {
    pub base_url: String,
    pub api_key: Option<String>,
    pub headers: Vec<(String, String)>,
}

/// Per-request knobs.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub max_output: Option<u64>,
    pub temperature: Option<f64>,
    pub effort: Effort,
    /// Passed to the provider so repeated turns of one conversation land on the
    /// same cache. Stable for the life of a session; changing it silently costs
    /// money.
    pub cache_key: Option<String>,
}

/// Per-tier settings, applied to a request when that tier answers.
///
/// Separate from [`Options`] because these come from configuration and those
/// come from the session: a tier says how hard to think, a session says which
/// cache its turns belong to, and neither should overwrite the other.
#[derive(Clone, Debug, Default)]
pub struct Tuning {
    /// f64 rather than f32: JSON numbers are f64, and `0.2f32` serialises as
    /// `0.20000000298023224`, which is both ugly on the wire and a needless
    /// difference from what the config file said.
    pub temperature: Option<f64>,
    pub effort: Option<Effort>,
    pub max_output: Option<u64>,
}

impl Tuning {
    /// Fill in what this tier has an opinion about, leaving the rest alone.
    pub fn apply(&self, options: &mut Options) {
        if let Some(t) = self.temperature {
            options.temperature = Some(t);
        }
        if let Some(e) = self.effort {
            options.effort = e;
        }
        if let Some(m) = self.max_output {
            options.max_output = Some(m);
        }
    }
}

/// One call, owned outright.
///
/// Owned rather than borrowed so the stream a wire returns can be `'static` and
/// live in a task without borrowing the session that made it.
#[derive(Clone, Debug)]
pub struct Request {
    pub model: Model,
    pub context: Context,
    pub endpoint: Endpoint,
    pub options: Options,
}
