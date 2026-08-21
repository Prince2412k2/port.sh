//! Loop events to `session/update` notifications.
//!
//! Almost everything the loop reports has a home in the protocol already, which
//! is the reason for speaking real ACP rather than inventing something: a client
//! that renders `tool_call` and `usage_update` needs no cooperation from us to
//! show what the agent is doing.
//!
//! Two things have no native variant and ride in `_meta`, which the spec
//! reserves for exactly this: cache reads, because `usage_update` has `used`,
//! `size` and `cost` but nowhere for the number that explains the cost; and a
//! mid-session model switch, because the client named a model at the handshake
//! and would otherwise keep naming the wrong one.

use agent_client_protocol_schema::v1 as acp;
use parley::types::{Block, Stop};
use serde_json::json;

use crate::event::Event;
use crate::tool::{Kind, Output};

pub const META_CACHE_READ: &str = "cacheRead";
pub const META_MODEL: &str = "model";
pub const META_TURN: &str = "turn";
pub const META_COMPACTED: &str = "compacted";
pub const META_ERROR: &str = "error";

pub fn kind(kind: Kind) -> acp::ToolKind {
    match kind {
        Kind::Read => acp::ToolKind::Read,
        Kind::Edit => acp::ToolKind::Edit,
        Kind::Delete => acp::ToolKind::Delete,
        Kind::Move => acp::ToolKind::Move,
        Kind::Search => acp::ToolKind::Search,
        Kind::Execute => acp::ToolKind::Execute,
        Kind::Think => acp::ToolKind::Think,
        Kind::Fetch => acp::ToolKind::Fetch,
        Kind::SwitchMode => acp::ToolKind::SwitchMode,
        Kind::Other => acp::ToolKind::Other,
    }
}

fn content(output: &Output) -> Vec<acp::ToolCallContent> {
    output
        .content
        .iter()
        .filter_map(|block| match block {
            Block::Text { text } => Some(acp::ToolCallContent::Content(acp::Content::new(
                acp::ContentBlock::from(text.as_str()),
            ))),
            // A tool returning an image or reasoning has nowhere sensible to go
            // in a tool result; nothing we ship does it.
            _ => None,
        })
        .collect()
}

/// The notifications one event becomes. Most produce one; some produce none,
/// because the information already went out as chunks or belongs in the reply
/// to `session/prompt` instead.
pub fn updates(event: &Event) -> Vec<acp::SessionUpdate> {
    match event {
        Event::Text { delta, .. } => vec![acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(acp::ContentBlock::from(delta.as_str())),
        )],
        Event::Thinking { delta, .. } => vec![acp::SessionUpdate::AgentThoughtChunk(
            acp::ContentChunk::new(acp::ContentBlock::from(delta.as_str())),
        )],

        Event::ToolStart {
            id,
            name,
            title,
            kind: k,
            args,
        } => {
            let mut call = acp::ToolCall::new(acp::ToolCallId::new(id.as_str()), title.clone());
            call.kind = kind(*k);
            call.status = acp::ToolCallStatus::Pending;
            // The arguments, untouched. This is what lets a client show what
            // the agent is actually about to do rather than a label for it.
            call.raw_input = Some(args.clone());
            let mut meta = acp::Meta::new();
            meta.insert("name".into(), json!(name));
            call.meta = Some(meta);
            vec![acp::SessionUpdate::ToolCall(call)]
        }

        Event::ToolProgress { id, output } => {
            let mut fields = acp::ToolCallUpdateFields::new();
            fields.status = Some(acp::ToolCallStatus::InProgress);
            fields.content = Some(content(output));
            vec![acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(id.as_str()),
                fields,
            ))]
        }

        Event::ToolEnd { id, ok, output } => {
            let mut fields = acp::ToolCallUpdateFields::new();
            fields.status = Some(if *ok {
                acp::ToolCallStatus::Completed
            } else {
                acp::ToolCallStatus::Failed
            });
            fields.content = Some(content(output));
            fields.raw_output = output.raw.clone();
            vec![acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(id.as_str()),
                fields,
            ))]
        }

        Event::Usage {
            used,
            size,
            cost,
            cache_read,
        } => {
            let mut usage = acp::UsageUpdate::new(*used, *size);
            if *cost > 0.0 {
                usage.cost = Some(acp::Cost::new(*cost, "USD"));
            }
            let mut meta = acp::Meta::new();
            meta.insert(META_CACHE_READ.into(), json!(cache_read));
            usage.meta = Some(meta);
            vec![acp::SessionUpdate::UsageUpdate(usage)]
        }

        Event::Switched { provider, model } => {
            let mut chunk = acp::ContentChunk::new(acp::ContentBlock::from(""));
            let mut meta = acp::Meta::new();
            meta.insert(META_MODEL.into(), json!(format!("{provider}/{model}")));
            chunk.meta = Some(meta);
            vec![acp::SessionUpdate::AgentThoughtChunk(chunk)]
        }

        Event::Compacted { before, after } => {
            let mut chunk = acp::ContentChunk::new(acp::ContentBlock::from(""));
            let mut meta = acp::Meta::new();
            meta.insert(
                META_COMPACTED.into(),
                json!({ "before": before, "after": after }),
            );
            chunk.meta = Some(meta);
            vec![acp::SessionUpdate::AgentThoughtChunk(chunk)]
        }

        // A turn boundary has no protocol variant. It is useful for a client
        // that wants to group a transcript, so it goes out as `_meta` on an
        // otherwise empty thought chunk rather than not at all.
        Event::TurnStart { turn } => {
            let mut chunk = acp::ContentChunk::new(acp::ContentBlock::from(""));
            let mut meta = acp::Meta::new();
            meta.insert(META_TURN.into(), json!(turn));
            chunk.meta = Some(meta);
            vec![acp::SessionUpdate::AgentThoughtChunk(chunk)]
        }

        // The chunks already said everything a good turn had to say. A turn that
        // failed said nothing at all, though: the deltas never arrived, so
        // without this the client sees a finished turn with no content and no
        // reason -- which is how a 401 came to look like a model with nothing
        // to add. The message goes out as text because that is what a client
        // already renders, and in `_meta` because that is what it can act on.
        Event::Message(message) => {
            let Some(why) = message.error.as_deref().filter(|_| message.stop == Stop::Error)
            else {
                return Vec::new();
            };
            let mut chunk = acp::ContentChunk::new(acp::ContentBlock::from(
                format!("The model could not answer: {why}").as_str(),
            ));
            let mut meta = acp::Meta::new();
            meta.insert(META_ERROR.into(), json!(why));
            chunk.meta = Some(meta);
            vec![acp::SessionUpdate::AgentMessageChunk(chunk)]
        }
        // The stop reason belongs in the reply to `session/prompt`.
        Event::Ended { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn one(event: &Event) -> Value {
        let updates = updates(event);
        assert_eq!(updates.len(), 1, "expected exactly one update");
        serde_json::to_value(&updates[0]).unwrap()
    }

    #[test]
    fn text_becomes_an_agent_message_chunk() {
        let v = one(&Event::Text {
            index: 0,
            delta: "Jaipur".into(),
        });
        assert_eq!(v["sessionUpdate"], "agent_message_chunk");
        assert_eq!(v["content"]["text"], "Jaipur");
    }

    #[test]
    fn a_tool_call_carries_its_arguments_and_kind() {
        let v = one(&Event::ToolStart {
            id: "c1".into(),
            name: "locate_place".into(),
            title: "Locate Jaipur".into(),
            kind: Kind::Fetch,
            args: json!({"name": "Jaipur"}),
        });
        assert_eq!(v["sessionUpdate"], "tool_call");
        assert_eq!(v["toolCallId"], "c1");
        assert_eq!(v["title"], "Locate Jaipur");
        assert_eq!(v["kind"], "fetch");
        // `pending` is the spec's default for a new tool call, and the schema
        // skips defaults, so a conforming `tool_call` omits `status` entirely.
        // A client reads its absence as "starting", which is what it means.
        assert!(v.get("status").is_none(), "{v}");
        // The whole point: a client can show what the agent asked for.
        assert_eq!(v["rawInput"], json!({"name": "Jaipur"}));
        // The programmatic name is unstable in the spec, so it rides in _meta.
        assert_eq!(v["_meta"]["name"], "locate_place");
    }

    #[test]
    fn progress_and_completion_are_updates_on_the_same_id() {
        let progress = one(&Event::ToolProgress {
            id: "c1".into(),
            output: Output::text("connecting"),
        });
        assert_eq!(progress["sessionUpdate"], "tool_call_update");
        assert_eq!(progress["toolCallId"], "c1");
        assert_eq!(progress["status"], "in_progress");
        assert_eq!(progress["content"][0]["content"]["text"], "connecting");

        let done = one(&Event::ToolEnd {
            id: "c1".into(),
            ok: true,
            output: Output {
                content: vec![Block::text("26.9,75.8")],
                raw: Some(json!({"lat": 26.9, "lon": 75.8})),
            },
        });
        assert_eq!(done["status"], "completed");
        assert_eq!(done["rawOutput"], json!({"lat": 26.9, "lon": 75.8}));
    }

    #[test]
    fn a_refused_tool_is_failed_rather_than_completed() {
        let v = one(&Event::ToolEnd {
            id: "c1".into(),
            ok: false,
            output: Output::text("no tool named `ghost`"),
        });
        assert_eq!(v["status"], "failed");
        assert_eq!(v["content"][0]["content"]["text"], "no tool named `ghost`");
    }

    #[test]
    fn usage_reports_the_window_and_hides_cache_reads_in_meta() {
        let v = one(&Event::Usage {
            used: 1100,
            size: 131_072,
            cost: 0.0008,
            cache_read: 800,
        });
        assert_eq!(v["sessionUpdate"], "usage_update");
        assert_eq!(v["used"], 1100);
        assert_eq!(v["size"], 131_072);
        assert_eq!(v["cost"]["amount"], 0.0008);
        assert_eq!(v["cost"]["currency"], "USD");
        assert_eq!(v["_meta"]["cacheRead"], 800);
    }

    #[test]
    fn a_free_turn_reports_no_cost_at_all() {
        // Zero is not the same as free, but sending `cost: 0` to a client that
        // shows a running total makes a paid session look free. Absent is
        // honest; the tier this matters for is the one with no published price.
        let v = one(&Event::Usage {
            used: 10,
            size: 100,
            cost: 0.0,
            cache_read: 0,
        });
        assert!(v.get("cost").is_none(), "{v}");
    }

    #[test]
    fn a_failed_turn_is_visible_rather_than_a_silent_end() {
        // The bug a live run found: the deltas never arrive for a failed turn,
        // so a client saw `end_turn` with no content and no reason at all.
        let v = one(&Event::Message(parley::Assistant::failed(
            "auth: Unauthorized",
        )));
        assert_eq!(v["sessionUpdate"], "agent_message_chunk");
        assert!(v["content"]["text"]
            .as_str()
            .unwrap()
            .contains("Unauthorized"));
        assert_eq!(v["_meta"]["error"], "auth: Unauthorized");
    }

    #[test]
    fn events_the_protocol_already_covered_produce_nothing() {
        assert!(updates(&Event::Message(parley::Assistant::pending())).is_empty());
        assert!(updates(&Event::Ended {
            reason: crate::End::EndTurn,
            appended: vec![],
        })
        .is_empty());
    }
}
