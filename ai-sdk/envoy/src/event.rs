//! What the loop tells whoever is listening.
//!
//! Deliberately a superset of what ACP's `session/update` can carry, so the
//! binary's job is a mapping rather than a decision. The two things ACP has no
//! variant for -- a turn beginning, and a model being swapped mid-session --
//! ride in `_meta`, and the rest goes on the wire as itself.
//!
//! Every text and reasoning event carries the index of the block it belongs to.
//! A client that appends by index keeps talk, tool call, talk in the order the
//! model did them; one that appends to a single buffer does not.

use parley::types::{Assistant, Message};
use serde_json::Value;

use crate::tool::{Kind, Output};

#[derive(Clone, Debug)]
pub enum Event {
    /// One model call is starting. `turn` counts from 1 and is what the budget
    /// is spent against.
    TurnStart { turn: usize },
    Text { index: usize, delta: String },
    Thinking { index: usize, delta: String },
    /// The assistant message, complete. Arrives before any tool runs, so a
    /// client can show what was said before showing what it did.
    Message(Assistant),
    ToolStart {
        id: String,
        name: String,
        /// Built from the arguments where the tool bothers: "Fetch example.com"
        /// rather than "Fetch".
        title: String,
        kind: Kind,
        /// Exactly what the model asked for. ACP's `rawInput`.
        args: Value,
    },
    /// A tool reporting progress without being finished.
    ToolProgress { id: String, output: Output },
    ToolEnd {
        id: String,
        ok: bool,
        output: Output,
    },
    /// Context consumption after a turn. `used` and `size` are ACP's
    /// `usage_update` fields; `cache_read` has nowhere to go in the protocol
    /// and rides in `_meta`, because it is the number that explains the cost.
    Usage {
        used: u64,
        size: u64,
        cost: f64,
        cache_read: u64,
    },
    /// The conversation was shortened to fit. Announced because a history that
    /// silently got smaller is indistinguishable from a model that forgot.
    Compacted { before: u64, after: u64 },
    /// A tier fell through to the next model. The client named a model at the
    /// handshake and would otherwise keep naming the wrong one.
    Switched { provider: String, model: String },
    Ended {
        reason: End,
        /// Everything added to the history by this run, in order. The caller
        /// owns the history; the loop only says what to append.
        appended: Vec<Message>,
    },
}

/// Why the loop stopped. Maps onto ACP's `StopReason`, which is why the names
/// look the way they do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// The model finished and asked for nothing more.
    EndTurn,
    /// It ran out of output tokens.
    MaxTokens,
    /// Our own ceiling on turns or tool calls, not the model's.
    MaxTurns,
    Refusal,
    Cancelled,
    Failed(String),
}

impl End {
    /// The ACP spelling.
    pub fn acp(&self) -> &'static str {
        match self {
            End::EndTurn => "end_turn",
            End::MaxTokens => "max_tokens",
            End::MaxTurns => "max_turn_requests",
            End::Refusal => "refusal",
            End::Cancelled => "cancelled",
            // ACP has no failure stop reason. `end_turn` with an error message
            // already delivered as content is the closest honest answer.
            End::Failed(_) => "end_turn",
        }
    }
}
