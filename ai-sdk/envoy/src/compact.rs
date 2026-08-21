//! Keeping a conversation inside the window.
//!
//! Two numbers decide it, and pi's are worth copying because they are measured
//! rather than guessed: reserve 16k for the turn that is about to happen, and
//! keep roughly the most recent 20k tokens when shortening.
//!
//! **Ground truth is the provider, not our arithmetic.** The last assistant
//! message carries the token counts the provider actually charged for; only the
//! messages after it are estimated. Estimating the whole history instead means
//! the error compounds with every turn, and the drift is always in the
//! direction of thinking there is more room than there is.
//!
//! **A cut is only safe at a turn boundary.** Cutting between a tool call and
//! its result leaves the provider holding a call with no answer, which is a 400
//! rather than a conversation that merely reads oddly. Since every turn starts
//! with a user message, the user messages are the cut points, and nothing else
//! is.
//!
//! **What replaces what was dropped.** Given a [`Summariser`], the part being
//! cut is handed to a model and comes back as a précis, which is prepended as a
//! user message so the conversation still knows what happened. Without one --
//! or if the summarising call fails -- the turns are dropped outright, because a
//! shorter history that works beats a full one the provider will reject.
//!
//! Either way the client is told, via `Event::Compacted`. A history that
//! silently got shorter is indistinguishable from a model that forgot
//! something, so the announcement is not optional.

use std::sync::Arc;

use parley::types::{Block, Context, Endpoint, Message, Model, Options, Stop};
use parley::{Accumulator, Wire};

#[derive(Clone, Copy, Debug)]
pub struct Compaction {
    pub enabled: bool,
    /// Room left for the turn about to happen: its output, and a summary if one
    /// is ever generated.
    pub reserve: u64,
    /// Roughly how much recent conversation to keep.
    pub keep_recent: u64,
}

impl Default for Compaction {
    fn default() -> Compaction {
        Compaction {
            enabled: true,
            reserve: 16_384,
            keep_recent: 20_000,
        }
    }
}

impl Compaction {
    pub fn off() -> Compaction {
        Compaction {
            enabled: false,
            ..Compaction::default()
        }
    }

    /// Shorten the history, replacing what is cut with a précis when there is
    /// a model to write one.
    pub async fn apply_with(
        &self,
        messages: Vec<Message>,
        model: &Model,
        summariser: Option<&Summariser>,
        announce: impl FnOnce(u64, u64),
    ) -> Vec<Message> {
        if !self.enabled {
            return messages;
        }
        let before = context_tokens(&messages);
        if before + self.reserve <= model.context_window {
            return messages;
        }
        let Some(cut) = cut_point(&messages, self.keep_recent) else {
            return messages;
        };

        let mut kept = messages[cut..].to_vec();
        if let Some(summariser) = summariser {
            if let Some(summary) = summariser.summarise(&messages[..cut]).await {
                // A user message rather than an assistant one: the model did not
                // say this, and an unpaired assistant turn at the front of a
                // history reads as something it has already committed to.
                kept.insert(
                    0,
                    Message::user(format!(
                        "Summary of the earlier part of this conversation:\n\n{summary}"
                    )),
                );
            }
        }
        let after = context_tokens(&kept);
        announce(before, after);
        kept
    }

    /// Shorten the history by dropping the oldest turns, with no précis.
    ///
    /// `announce` is called with the token counts before and after, and only
    /// when something was actually dropped.
    pub fn apply(
        &self,
        messages: Vec<Message>,
        model: &Model,
        announce: impl FnOnce(u64, u64),
    ) -> Vec<Message> {
        if !self.enabled {
            return messages;
        }
        let before = context_tokens(&messages);
        if before + self.reserve <= model.context_window {
            return messages;
        }
        let Some(cut) = cut_point(&messages, self.keep_recent) else {
            // Nothing can be dropped safely -- a single enormous turn. Better
            // to send it and let the provider say so than to cut mid-turn and
            // send something malformed.
            return messages;
        };
        let kept = messages[cut..].to_vec();
        let after = context_tokens(&kept);
        announce(before, after);
        kept
    }
}

/// What the summarising model is told.
///
/// Written for a record rather than a reply: the next turn reads this as
/// context, so it has to carry identifiers exactly and leave out the courtesies.
pub const SUMMARY_PROMPT: &str = "\
You are compressing the earlier part of a conversation so it can continue in \
less space. Write a record, not a reply.

Keep, exactly as written: file paths, names, identifiers, numbers, URLs, error \
messages, and any decision that was made. Keep anything the user asked for that \
has not been done yet, and say it is outstanding. Drop greetings, apologies, \
restatements, and anything already superseded.

Write in the third person, in short labelled sections. Do not address the user \
and do not offer to continue.";

/// A model that can be asked for a précis.
///
/// Its own model rather than the session's: summarising is a cheap, mechanical
/// job, and spending a reasoning model's budget on it is waste. Nothing stops
/// it being the same one.
pub struct Summariser {
    pub wire: Arc<dyn Wire>,
    pub model: Model,
    pub endpoint: Endpoint,
    pub options: Options,
}

impl Summariser {
    /// Boil the given messages down to prose.
    ///
    /// Returns `None` rather than an error: a failed summary means the history
    /// gets dropped instead, which is worse but still works. Failing the turn
    /// because a *housekeeping* call failed would be the wrong trade.
    pub async fn summarise(&self, messages: &[Message]) -> Option<String> {
        use futures_util::StreamExt;

        let transcript = transcript(messages);
        if transcript.trim().is_empty() {
            return None;
        }
        let request = parley::types::Request {
            model: self.model.clone(),
            context: Context {
                system: Some(SUMMARY_PROMPT.to_string()),
                messages: vec![Message::user(transcript)],
                tools: Vec::new(),
            },
            endpoint: self.endpoint.clone(),
            options: self.options.clone(),
        };
        let mut events = self.wire.stream(request);
        let mut acc = Accumulator::new();
        while let Some(event) = events.next().await {
            match event {
                Ok(event) => acc.apply(&event),
                Err(_) => return None,
            }
        }
        let message = acc.finish();
        if message.stop == Stop::Error {
            return None;
        }
        let text = message.text();
        (!text.trim().is_empty()).then_some(text)
    }
}

/// The messages as something a model can read.
fn transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        match message {
            Message::User { content } => {
                out.push_str("User: ");
                out.push_str(&text_of(content));
            }
            Message::Assistant(a) => {
                out.push_str("Assistant: ");
                out.push_str(&text_of(&a.content));
                for (_, name, args) in a.tool_calls() {
                    out.push_str(&format!("\n  [called {name} with {args}]"));
                }
            }
            Message::ToolResult {
                name,
                content,
                error,
                ..
            } => {
                out.push_str(&format!(
                    "Tool {name}{}: ",
                    if *error { " (failed)" } else { "" }
                ));
                out.push_str(&text_of(content));
            }
        }
        out.push('\n');
    }
    out
}

fn text_of(blocks: &[Block]) -> String {
    blocks.iter().filter_map(Block::as_text).collect()
}

/// Roughly four characters to a token, and a flat cost for an image.
///
/// The constant for images is pi's, and it is a stand-in rather than a
/// measurement: providers price images by dimensions in ways that differ, and
/// being wrong by a fixed amount is easier to reason about than being wrong by
/// a variable one.
const CHARS_PER_TOKEN: u64 = 4;
const IMAGE_TOKENS: u64 = 1_200;

pub fn estimate(message: &Message) -> u64 {
    let blocks = match message {
        Message::User { content } => content,
        Message::Assistant(a) => &a.content,
        Message::ToolResult { content, .. } => content,
    };
    let mut chars = 0u64;
    let mut tokens = 0u64;
    for block in blocks {
        match block {
            Block::Text { text } => chars += text.len() as u64,
            Block::Thinking { text, .. } => chars += text.len() as u64,
            Block::ToolCall { name, args, .. } => {
                chars += name.len() as u64 + args.to_string().len() as u64
            }
            Block::Image { .. } => tokens += IMAGE_TOKENS,
        }
    }
    // A few tokens of envelope per message: role, ids, framing.
    tokens + chars / CHARS_PER_TOKEN + 4
}

/// What the window currently holds, trusting the provider where we can.
pub fn context_tokens(messages: &[Message]) -> u64 {
    let anchor = messages.iter().enumerate().rev().find_map(|(i, m)| match m {
        Message::Assistant(a)
            if a.stop != Stop::Error
                && a.stop != Stop::Aborted
                && a.usage.context_tokens() > 0 =>
        {
            Some((i, a.usage.context_tokens()))
        }
        _ => None,
    });
    match anchor {
        Some((index, reported)) => {
            reported + messages[index + 1..].iter().map(estimate).sum::<u64>()
        }
        None => messages.iter().map(estimate).sum(),
    }
}

/// The earliest turn boundary that leaves at most `keep_recent` tokens after it.
///
/// Earliest rather than latest, so as much history is kept as will fit. Returns
/// `None` when the only boundary is the start, since dropping everything is not
/// compaction.
pub fn cut_point(messages: &[Message], keep_recent: u64) -> Option<usize> {
    let boundaries: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, Message::User { .. }))
        .map(|(i, _)| i)
        .collect();
    boundaries
        .into_iter()
        .find(|start| {
            *start > 0 && messages[*start..].iter().map(estimate).sum::<u64>() <= keep_recent
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use parley::types::{Api, Assistant, Cost, Usage};
    use serde_json::json;

    fn model(window: u64) -> Model {
        Model {
            id: "m".into(),
            name: "m".into(),
            provider: "p".into(),
            api: Api::OpenaiCompletions,
            context_window: window,
            max_output: None,
            reasoning: false,
            cost: Cost::default(),
        }
    }

    fn big(text: &str, n: usize) -> Message {
        Message::user(text.repeat(n))
    }

    /// A whole turn: user, an assistant that calls a tool, the result, an answer.
    fn turn(tag: &str) -> Vec<Message> {
        vec![
            Message::user(format!("question {tag}")),
            Message::Assistant(Assistant {
                content: vec![Block::ToolCall {
                    id: format!("c{tag}"),
                    name: "echo".into(),
                    args: json!({"text": tag}),
                }],
                stop: Stop::ToolUse,
                ..Assistant::pending()
            }),
            Message::ToolResult {
                call_id: format!("c{tag}"),
                name: "echo".into(),
                content: vec![Block::text(tag)],
                error: false,
            },
            Message::Assistant(Assistant {
                content: vec![Block::text(format!("answer {tag}"))],
                ..Assistant::pending()
            }),
        ]
    }

    #[test]
    fn a_provider_report_beats_an_estimate() {
        let messages = vec![
            Message::user("x".repeat(4000)),
            Message::Assistant(Assistant {
                content: vec![Block::text("ok")],
                usage: Usage {
                    input: 5000,
                    output: 10,
                    ..Usage::default()
                },
                ..Assistant::pending()
            }),
            Message::user("y".repeat(400)),
        ];
        // 5010 reported, plus ~104 estimated for the trailing user message --
        // not the 1100 a whole-history estimate would produce.
        let total = context_tokens(&messages);
        assert!((5100..5200).contains(&total), "{total}");
    }

    #[test]
    fn an_errored_turn_is_not_trusted_as_an_anchor() {
        // Its usage describes a request that failed, so it does not describe
        // what the window holds.
        let messages = vec![
            Message::user("hello"),
            Message::Assistant(Assistant {
                stop: Stop::Error,
                error: Some("boom".into()),
                usage: Usage {
                    input: 900_000,
                    ..Usage::default()
                },
                ..Assistant::pending()
            }),
        ];
        assert!(context_tokens(&messages) < 100, "{}", context_tokens(&messages));
    }

    #[test]
    fn a_cut_lands_on_a_user_message_and_never_splits_a_turn() {
        let mut messages = Vec::new();
        for tag in ["one", "two", "three"] {
            messages.extend(turn(tag));
        }
        // Small enough that only the last turn fits.
        let cut = cut_point(&messages, 40).expect("a boundary exists");
        assert!(matches!(messages[cut], Message::User { .. }));
        // Everything kept is a whole turn: the first thing is a user message,
        // and no tool result is left without the call that produced it.
        let kept = &messages[cut..];
        assert!(matches!(kept[0], Message::User { .. }));
        for (i, message) in kept.iter().enumerate() {
            if let Message::ToolResult { call_id, .. } = message {
                let has_call = kept[..i].iter().any(|m| match m {
                    Message::Assistant(a) => a.tool_calls().any(|(id, _, _)| id == call_id),
                    _ => false,
                });
                assert!(has_call, "a result lost its call at {i}");
            }
        }
    }

    #[test]
    fn the_earliest_fitting_boundary_is_chosen_so_history_is_kept() {
        let mut messages = Vec::new();
        for tag in ["one", "two", "three"] {
            messages.extend(turn(tag));
        }
        // Generous budget: the cut should be the first boundary after zero,
        // keeping two turns rather than one.
        let cut = cut_point(&messages, 10_000).expect("a boundary exists");
        assert_eq!(cut, 4, "expected the second turn's start");
    }

    #[test]
    fn nothing_is_dropped_while_there_is_room() {
        let messages = turn("one");
        let before = messages.len();
        let mut announced = false;
        let after = Compaction::default().apply(messages, &model(200_000), |_, _| announced = true);
        assert_eq!(after.len(), before);
        assert!(!announced);
    }

    #[test]
    fn a_full_window_is_shortened_and_announced() {
        let mut messages = vec![big("a", 40_000)];
        messages.extend(turn("one"));
        messages.push(big("b", 400));
        messages.extend(turn("two"));
        let compaction = Compaction {
            enabled: true,
            reserve: 1_000,
            keep_recent: 2_000,
        };
        let mut seen = None;
        let after = compaction.apply(messages.clone(), &model(8_000), |b, a| seen = Some((b, a)));
        let (before, small) = seen.expect("compaction should have been announced");
        assert!(before > small, "{before} -> {small}");
        assert!(after.len() < messages.len());
        assert!(matches!(after[0], Message::User { .. }));
    }

    #[test]
    fn one_enormous_turn_is_sent_rather_than_cut_in_half() {
        // No boundary past zero, so there is nothing safe to drop. Sending it
        // and letting the provider complain beats inventing a malformed
        // history.
        let messages = vec![big("a", 100_000)];
        let mut announced = false;
        let after = Compaction::default().apply(messages.clone(), &model(1_000), |_, _| {
            announced = true
        });
        assert_eq!(after.len(), messages.len());
        assert!(!announced);
    }

    fn summariser(text: &str) -> Summariser {
        use parley::stream::Event;
        Summariser {
            wire: Arc::new(parley::Canned::new(vec![vec![
                Event::Text { index: 0, delta: text.into() },
                Event::Done { stop: Stop::End },
            ]])),
            model: model(100_000),
            endpoint: Endpoint::default(),
            options: Options::default(),
        }
    }

    fn crowded() -> Vec<Message> {
        let mut messages = vec![Message::user("a".repeat(40_000))];
        messages.extend(turn("one"));
        messages.push(Message::user("b".repeat(400)));
        messages.extend(turn("two"));
        messages
    }

    fn tight() -> Compaction {
        Compaction { enabled: true, reserve: 1_000, keep_recent: 2_000 }
    }

    #[tokio::test]
    async fn what_is_dropped_comes_back_as_a_precis() {
        let mut seen = None;
        let after = tight()
            .apply_with(crowded(), &model(8_000), Some(&summariser("They discussed Jaipur.")), |b, a| {
                seen = Some((b, a))
            })
            .await;
        assert!(seen.is_some(), "compaction should have been announced");
        // The précis is first, and it is a user message: the model never said
        // it, and an unpaired assistant turn at the front reads as a commitment.
        assert!(matches!(
            &after[0],
            Message::User { content } if content[0].as_text().unwrap().contains("They discussed Jaipur.")
        ));
        assert!(matches!(&after[1], Message::User { .. }), "then a real turn");
    }

    #[tokio::test]
    async fn a_summariser_that_fails_falls_back_to_dropping() {
        // Housekeeping failing should not fail the turn.
        let broken = Summariser {
            // An empty script errors on the first call.
            wire: Arc::new(parley::Canned::new(vec![])),
            model: model(100_000),
            endpoint: Endpoint::default(),
            options: Options::default(),
        };
        let mut seen = None;
        let after = tight()
            .apply_with(crowded(), &model(8_000), Some(&broken), |b, a| seen = Some((b, a)))
            .await;
        assert!(seen.is_some(), "still compacted");
        assert!(matches!(&after[0], Message::User { .. }));
        let first = match &after[0] {
            Message::User { content } => content[0].as_text().unwrap().to_string(),
            _ => unreachable!(),
        };
        assert!(!first.contains("Summary of the earlier part"), "{first}");
    }

    #[tokio::test]
    async fn nothing_is_summarised_while_there_is_room() {
        let messages = turn("one");
        let before = messages.len();
        let after = Compaction::default()
            .apply_with(messages, &model(200_000), Some(&summariser("unused")), |_, _| {
                panic!("should not announce")
            })
            .await;
        assert_eq!(after.len(), before);
    }

    #[test]
    fn a_transcript_names_tools_and_marks_failures() {
        let text = transcript(&[
            Message::user("where is Jaipur"),
            Message::Assistant(parley::Assistant {
                content: vec![Block::ToolCall {
                    id: "c1".into(),
                    name: "locate_place".into(),
                    args: serde_json::json!({"name": "Jaipur"}),
                }],
                ..parley::Assistant::pending()
            }),
            Message::ToolResult {
                call_id: "c1".into(),
                name: "locate_place".into(),
                content: vec![Block::text("not found")],
                error: true,
            },
        ]);
        assert!(text.contains("User: where is Jaipur"));
        assert!(text.contains("[called locate_place"));
        assert!(text.contains("Tool locate_place (failed)"), "{text}");
    }

    #[test]
    fn compaction_can_be_turned_off() {
        let messages = vec![big("a", 100_000)];
        let after = Compaction::off().apply(messages.clone(), &model(10), |_, _| {
            panic!("should not announce")
        });
        assert_eq!(after.len(), messages.len());
    }
}
