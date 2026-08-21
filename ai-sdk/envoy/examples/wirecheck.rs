//! Prove the protocol types serialise to what `portfolio/src/acp.rs` parses.
use agent_client_protocol_schema::v1::*;
use serde_json::json;

fn main() {
    let chunk = SessionNotification::new(
        SessionId::new("s1"),
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::from("Jaipur"))),
    );
    println!("{}", serde_json::to_string(&chunk).unwrap());

    let mut call = ToolCall::new(ToolCallId::new("c1"), "Locate Jaipur");
    call.kind = ToolKind::Fetch;
    call.status = ToolCallStatus::InProgress;
    call.raw_input = Some(json!({"name": "Jaipur"}));
    println!("{}", serde_json::to_string(&SessionUpdate::ToolCall(call)).unwrap());

    println!("{}", serde_json::to_string(&SessionUpdate::UsageUpdate(
        UsageUpdate::new(1100, 131072)
    )).unwrap());
}
