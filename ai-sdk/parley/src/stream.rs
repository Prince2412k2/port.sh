//! The event vocabulary every wire is translated into, and the thing that
//! folds it back into a message.
//!
//! Two consumers want different shapes from the same stream. A terminal wants
//! deltas the instant they arrive; the conversation history wants one finished
//! message. Rather than make each wire produce both, wires produce only events
//! and [`Accumulator`] produces the message -- so the fold is written once and
//! is the same for every provider.
//!
//! Every event carries the index of the block it belongs to. That is what lets
//! a client place a delta without guessing, and it is what survives text and
//! tool calls arriving interleaved in one message.

use serde_json::Value;

use crate::types::{Assistant, Block, Stop, Usage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Thinking,
    ToolCall,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The provider accepted the request and is answering.
    Start { response_id: Option<String> },
    /// A new block opened at `index`. For a tool call the name is known here;
    /// its arguments arrive as `ToolArgs`.
    BlockStart {
        index: usize,
        kind: Kind,
        /// Tool name, when `kind` is `ToolCall`.
        name: Option<String>,
        /// Tool call id, when `kind` is `ToolCall`.
        id: Option<String>,
    },
    Text { index: usize, delta: String },
    Thinking { index: usize, delta: String },
    /// A fragment of the tool call's argument JSON. Providers stream this as
    /// text, so it is not parseable until the block ends.
    ToolArgs { index: usize, delta: String },
    /// Provider state for the block that must be handed back verbatim later --
    /// OpenAI's encrypted reasoning is the reason this exists.
    Opaque { index: usize, value: Value },
    BlockEnd { index: usize },
    /// Which model is actually answering. Emitted by a fallback wire when it
    /// has moved off the first tier, so a client can stop naming a model that
    /// stopped answering.
    Model { provider: String, model: String },
    /// Usage as reported, which for most providers arrives once at the end.
    Usage(Usage),
    Done { stop: Stop },
}

/// Folds events into the message they describe.
///
/// Cheap to ask for a snapshot: a UI can render `message()` on every delta.
#[derive(Debug, Default)]
pub struct Accumulator {
    slots: Vec<Slot>,
    usage: Usage,
    stop: Option<Stop>,
    response_id: Option<String>,
}

#[derive(Debug)]
enum Slot {
    Text(String),
    Thinking { text: String, opaque: Option<Value> },
    Tool { id: String, name: String, args: String },
}

impl Accumulator {
    pub fn new() -> Accumulator {
        Accumulator::default()
    }

    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Start { response_id } => self.response_id = response_id.clone(),
            Event::BlockStart {
                index,
                kind,
                name,
                id,
            } => {
                let slot = match kind {
                    Kind::Text => Slot::Text(String::new()),
                    Kind::Thinking => Slot::Thinking {
                        text: String::new(),
                        opaque: None,
                    },
                    Kind::ToolCall => Slot::Tool {
                        id: id.clone().unwrap_or_default(),
                        name: name.clone().unwrap_or_default(),
                        args: String::new(),
                    },
                };
                self.put(*index, slot);
            }
            Event::Text { index, delta } => {
                if let Some(Slot::Text(s)) = self.slot(*index, Kind::Text) {
                    s.push_str(delta);
                }
            }
            Event::Thinking { index, delta } => {
                if let Some(Slot::Thinking { text, .. }) = self.slot(*index, Kind::Thinking) {
                    text.push_str(delta);
                }
            }
            Event::ToolArgs { index, delta } => {
                if let Some(Slot::Tool { args, .. }) = self.slot(*index, Kind::ToolCall) {
                    args.push_str(delta);
                }
            }
            Event::Opaque { index, value } => {
                if let Some(Slot::Thinking { opaque, .. }) = self.slot(*index, Kind::Thinking) {
                    *opaque = Some(value.clone());
                }
            }
            Event::BlockEnd { .. } | Event::Model { .. } => {}
            Event::Usage(u) => self.usage = *u,
            Event::Done { stop } => self.stop = Some(*stop),
        }
    }

    /// The message as it stands. Safe to call mid-stream.
    pub fn message(&self) -> Assistant {
        Assistant {
            content: self.slots.iter().map(Slot::block).collect(),
            stop: self.stop.unwrap_or(Stop::End),
            usage: self.usage,
            error: None,
            response_id: self.response_id.clone(),
            opaque: None,
        }
    }

    /// The message, with a stop reason that says the stream ended early if no
    /// `Done` ever arrived. A truncated stream that silently looks complete is
    /// worse than one that says so.
    pub fn finish(self) -> Assistant {
        let ended = self.stop.is_some();
        let mut msg = self.message();
        if !ended {
            msg.stop = Stop::Error;
            msg.error = Some("stream ended without a stop reason".into());
        }
        msg
    }

    pub fn usage(&self) -> &Usage {
        &self.usage
    }

    fn put(&mut self, index: usize, slot: Slot) {
        while self.slots.len() <= index {
            self.slots.push(Slot::Text(String::new()));
        }
        self.slots[index] = slot;
    }

    /// The slot at `index`, opening one of `kind` if the provider never sent a
    /// start event. Chat Completions does exactly that: content deltas arrive
    /// with no announcement that a block began.
    fn slot(&mut self, index: usize, kind: Kind) -> Option<&mut Slot> {
        if self.slots.len() <= index {
            let slot = match kind {
                Kind::Text => Slot::Text(String::new()),
                Kind::Thinking => Slot::Thinking {
                    text: String::new(),
                    opaque: None,
                },
                Kind::ToolCall => Slot::Tool {
                    id: String::new(),
                    name: String::new(),
                    args: String::new(),
                },
            };
            self.put(index, slot);
        }
        self.slots.get_mut(index)
    }
}

impl Slot {
    fn block(&self) -> Block {
        match self {
            Slot::Text(text) => Block::Text { text: text.clone() },
            Slot::Thinking { text, opaque } => Block::Thinking {
                text: text.clone(),
                opaque: opaque.clone(),
            },
            Slot::Tool { id, name, args } => Block::ToolCall {
                id: id.clone(),
                name: name.clone(),
                // Unparseable arguments become a string rather than an error.
                // A tool handed a string where it wanted an object fails with a
                // message the model can read and correct, which is a better
                // outcome than the turn dying here.
                args: serde_json::from_str(args)
                    .unwrap_or_else(|_| Value::String(args.clone())),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(events: &[Event]) -> Assistant {
        let mut a = Accumulator::new();
        for e in events {
            a.apply(e);
        }
        a.finish()
    }

    #[test]
    fn text_deltas_become_one_block() {
        let msg = fold(&[
            Event::Start { response_id: None },
            Event::Text { index: 0, delta: "he".into() },
            Event::Text { index: 0, delta: "llo".into() },
            Event::Done { stop: Stop::End },
        ]);
        assert_eq!(msg.content, vec![Block::text("hello")]);
        assert_eq!(msg.stop, Stop::End);
    }

    #[test]
    fn talk_then_tool_then_talk_keeps_its_order() {
        // The whole reason blocks are indexed rather than grouped by type.
        let msg = fold(&[
            Event::Text { index: 0, delta: "let me look".into() },
            Event::BlockStart {
                index: 1,
                kind: Kind::ToolCall,
                name: Some("locate_place".into()),
                id: Some("c1".into()),
            },
            Event::ToolArgs { index: 1, delta: r#"{"name":"Jaipur"}"# .into() },
            Event::BlockEnd { index: 1 },
            Event::Text { index: 2, delta: "it is in Rajasthan".into() },
            Event::Done { stop: Stop::End },
        ]);
        assert_eq!(msg.content.len(), 3);
        assert_eq!(msg.content[0].as_text(), Some("let me look"));
        assert!(matches!(&msg.content[1], Block::ToolCall { name, .. } if name == "locate_place"));
        assert_eq!(msg.content[2].as_text(), Some("it is in Rajasthan"));
    }

    #[test]
    fn tool_arguments_are_parsed_from_their_fragments() {
        let msg = fold(&[
            Event::BlockStart {
                index: 0,
                kind: Kind::ToolCall,
                name: Some("t".into()),
                id: Some("1".into()),
            },
            Event::ToolArgs { index: 0, delta: r#"{"a":"#.into() },
            Event::ToolArgs { index: 0, delta: "1}".into() },
            Event::Done { stop: Stop::ToolUse },
        ]);
        let (_, _, args) = msg.tool_calls().next().unwrap();
        assert_eq!(args, &serde_json::json!({"a": 1}));
    }

    #[test]
    fn unparseable_arguments_survive_as_a_string() {
        let msg = fold(&[
            Event::BlockStart {
                index: 0,
                kind: Kind::ToolCall,
                name: Some("t".into()),
                id: Some("1".into()),
            },
            Event::ToolArgs { index: 0, delta: "{not json".into() },
            Event::Done { stop: Stop::ToolUse },
        ]);
        let (_, _, args) = msg.tool_calls().next().unwrap();
        assert_eq!(args, &Value::String("{not json".into()));
    }

    #[test]
    fn reasoning_keeps_the_state_the_provider_wants_back() {
        let msg = fold(&[
            Event::BlockStart { index: 0, kind: Kind::Thinking, name: None, id: None },
            Event::Thinking { index: 0, delta: "hmm".into() },
            Event::Opaque { index: 0, value: Value::String("enc:abc".into()) },
            Event::Done { stop: Stop::End },
        ]);
        assert_eq!(
            msg.content[0],
            Block::Thinking {
                text: "hmm".into(),
                opaque: Some(Value::String("enc:abc".into()))
            }
        );
    }

    #[test]
    fn a_delta_with_no_block_start_still_lands() {
        // Chat Completions never announces a block; it just starts sending
        // `delta.content`.
        let msg = fold(&[
            Event::Text { index: 0, delta: "x".into() },
            Event::Done { stop: Stop::End },
        ]);
        assert_eq!(msg.content, vec![Block::text("x")]);
    }

    #[test]
    fn a_stream_cut_short_says_so() {
        let msg = fold(&[Event::Text { index: 0, delta: "half".into() }]);
        assert_eq!(msg.stop, Stop::Error);
        assert!(msg.error.is_some());
        // ...and keeps what did arrive, because a visitor already saw it.
        assert_eq!(msg.content[0].as_text(), Some("half"));
    }

    #[test]
    fn a_snapshot_mid_stream_reads_what_has_arrived() {
        let mut a = Accumulator::new();
        a.apply(&Event::Text { index: 0, delta: "par".into() });
        assert_eq!(a.message().text(), "par");
        a.apply(&Event::Text { index: 0, delta: "tial".into() });
        assert_eq!(a.message().text(), "partial");
    }
}
