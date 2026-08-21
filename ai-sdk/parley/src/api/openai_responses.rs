//! `POST /responses`.
//!
//! The newer OpenAI shape, and the one reasoning models need. It differs from
//! Chat Completions in three ways that all matter here.
//!
//! **It has indices.** Every event carries an `output_index`, so blocks do not
//! have to be inferred from the order deltas arrive in. That makes the parser
//! simpler than the Chat Completions one rather than harder.
//!
//! **Reasoning is state, not text.** A reasoning item comes back with an id and,
//! when asked for, an `encrypted_content` blob. Both have to go back verbatim on
//! the next request or the call is rejected -- so they are kept in the block's
//! `opaque` field and replayed. This is the reason `Block::Thinking` has that
//! field at all.
//!
//! **The history is a list of items, not messages.** A tool call and its result
//! are two items at the top level rather than a message with an attachment, and
//! reasoning is a third kind. `input` below builds that list.

use serde_json::{json, Map, Value};

use crate::error::Result;
use crate::http;
use crate::sse::Frame;
use crate::stream::{Event, Kind};
use crate::types::{Assistant, Block, Context, Cost, Effort, Message, Request, Stop, Usage};
use crate::wire::{EventStream, Frames, Wire};

pub struct OpenaiResponses;

impl Wire for OpenaiResponses {
    fn stream(&self, request: Request) -> EventStream {
        let url = format!("{}/responses", request.endpoint.base_url.trim_end_matches('/'));
        let mut builder = http::client().post(url).json(&body(&request));
        if let Some(key) = &request.endpoint.api_key {
            builder = builder.bearer_auth(key);
        }
        for (name, value) in &request.endpoint.headers {
            builder = builder.header(name, value);
        }
        http::sse_stream(builder, Parser::new(request.model.cost))
    }
}

pub fn body(request: &Request) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model.id));
    body.insert("input".into(), json!(input(&request.context)));
    body.insert("stream".into(), json!(true));
    if let Some(system) = &request.context.system {
        body.insert("instructions".into(), json!(system));
    }
    if !request.context.tools.is_empty() {
        let tools: Vec<Value> = request
            .context
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.schema,
                })
            })
            .collect();
        body.insert("tools".into(), json!(tools));
    }
    if request.model.reasoning {
        if let Some(effort) = effort(request.options.effort) {
            body.insert(
                "reasoning".into(),
                json!({ "effort": effort, "summary": "auto" }),
            );
        }
        // Without asking, the reasoning comes back with nothing to replay, and
        // the next turn is rejected for referring to an item it cannot prove it
        // was given.
        body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    }
    // We keep the conversation; the provider does not need to. Storing it also
    // makes the reasoning replay unnecessary, which sounds convenient until a
    // session has to move between accounts.
    body.insert("store".into(), json!(false));
    if let Some(max) = request.options.max_output.or(request.model.max_output) {
        body.insert("max_output_tokens".into(), json!(max));
    }
    if let Some(key) = &request.options.cache_key {
        body.insert("prompt_cache_key".into(), json!(key));
    }
    Value::Object(body)
}

fn effort(effort: Effort) -> Option<&'static str> {
    match effort {
        Effort::Off => None,
        Effort::Minimal => Some("minimal"),
        Effort::Low => Some("low"),
        Effort::Medium => Some("medium"),
        Effort::High => Some("high"),
    }
}

fn input(context: &Context) -> Vec<Value> {
    let mut items = Vec::new();
    for message in &context.messages {
        match message {
            Message::User { content } => items.push(json!({
                "type": "message",
                "role": "user",
                "content": user_parts(content),
            })),
            Message::Assistant(a) => assistant_items(a, &mut items),
            Message::ToolResult {
                call_id, content, ..
            } => items.push(json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": flatten(content),
            })),
        }
    }
    items
}

fn user_parts(blocks: &[Block]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } => Some(json!({ "type": "input_text", "text": text })),
            Block::Image { data, mime } => Some(json!({
                "type": "input_image",
                "image_url": format!("data:{mime};base64,{data}")
            })),
            _ => None,
        })
        .collect()
}

/// One assistant message becomes several items, in the order its blocks were.
fn assistant_items(a: &Assistant, items: &mut Vec<Value>) {
    for block in &a.content {
        match block {
            Block::Text { text } => items.push(json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": text }],
            })),
            Block::Thinking { opaque, .. } => {
                // Only what the provider gave us goes back. A reasoning item we
                // invented, or one missing its encrypted content, is worse than
                // no reasoning item: it refers to state the provider never
                // issued.
                if let Some(item) = opaque {
                    items.push(item.clone());
                }
            }
            Block::ToolCall { id, name, args } => items.push(json!({
                "type": "function_call",
                "call_id": id,
                "name": name,
                "arguments": args.to_string(),
            })),
            Block::Image { .. } => {}
        }
    }
}

fn flatten(blocks: &[Block]) -> String {
    blocks.iter().filter_map(Block::as_text).collect()
}

pub struct Parser {
    cost: Cost,
    started: bool,
    stop: Option<Stop>,
}

impl Parser {
    pub fn new(cost: Cost) -> Parser {
        Parser {
            cost,
            started: false,
            stop: None,
        }
    }

    fn usage(&self, usage: &Value) -> Usage {
        let n = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let cached = usage
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let mut out = Usage {
            input: n("input_tokens").saturating_sub(cached),
            output: n("output_tokens"),
            cache_read: cached,
            cache_write: 0,
            cost: 0.0,
        };
        out.cost = self.cost.of(&out);
        out
    }
}

impl Frames for Parser {
    fn frame(&mut self, frame: &Frame) -> Vec<Result<Event>> {
        let data = frame.data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Vec::new();
        }
        let Ok(value): std::result::Result<Value, _> = serde_json::from_str(data) else {
            return Vec::new();
        };
        // The event name is in the body as well as the `event:` line, and some
        // gateways drop the line.
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .or(frame.event.as_deref())
            .unwrap_or("");
        let index = value
            .get("output_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let mut out = Vec::new();

        match kind {
            "response.created" => {
                if !self.started {
                    self.started = true;
                    out.push(Ok(Event::Start {
                        response_id: value
                            .get("response")
                            .and_then(|r| r.get("id"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    }));
                }
            }

            "response.output_item.added" => {
                let item = value.get("item");
                let item_type = item
                    .and_then(|i| i.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match item_type {
                    "message" => out.push(Ok(Event::BlockStart {
                        index,
                        kind: Kind::Text,
                        name: None,
                        id: None,
                    })),
                    "reasoning" => out.push(Ok(Event::BlockStart {
                        index,
                        kind: Kind::Thinking,
                        name: None,
                        id: None,
                    })),
                    "function_call" => out.push(Ok(Event::BlockStart {
                        index,
                        kind: Kind::ToolCall,
                        name: item
                            .and_then(|i| i.get("name"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        id: item
                            .and_then(|i| i.get("call_id"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })),
                    _ => {}
                }
            }

            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    out.push(Ok(Event::Text {
                        index,
                        delta: delta.to_string(),
                    }));
                }
            }

            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    out.push(Ok(Event::Thinking {
                        index,
                        delta: delta.to_string(),
                    }));
                }
            }

            "response.function_call_arguments.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    out.push(Ok(Event::ToolArgs {
                        index,
                        delta: delta.to_string(),
                    }));
                }
            }

            "response.output_item.done" => {
                // A finished reasoning item is the state we have to give back.
                // Kept whole rather than picking fields out of it, because what
                // the provider requires is its own object, not our reading of it.
                if let Some(item) = value.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("reasoning") {
                        out.push(Ok(Event::Opaque {
                            index,
                            value: item.clone(),
                        }));
                    }
                }
                out.push(Ok(Event::BlockEnd { index }));
            }

            "response.completed" | "response.incomplete" | "response.failed" => {
                let response = value.get("response");
                if let Some(usage) = response.and_then(|r| r.get("usage")) {
                    out.push(Ok(Event::Usage(self.usage(usage))));
                }
                let incomplete = response
                    .and_then(|r| r.get("incomplete_details"))
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str);
                let stop = match (kind, incomplete) {
                    ("response.failed", _) => Stop::Error,
                    (_, Some("max_output_tokens")) => Stop::Length,
                    (_, Some("content_filter")) => Stop::Refusal,
                    _ => self.stop.unwrap_or(Stop::End),
                };
                out.push(Ok(Event::Done { stop }));
            }

            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Api, Endpoint, Model, Options, Tool};
    use crate::Accumulator;
    use futures_util::StreamExt;

    fn model(reasoning: bool) -> Model {
        Model {
            id: "gpt-5-codex".into(),
            name: "GPT-5 Codex".into(),
            provider: "openai-codex".into(),
            api: Api::OpenaiResponses,
            context_window: 272_000,
            max_output: None,
            reasoning,
            cost: Cost {
                input: 1.25,
                output: 10.0,
                cache_read: 0.125,
                cache_write: 0.0,
            },
        }
    }

    fn request(context: Context, reasoning: bool) -> Request {
        Request {
            model: model(reasoning),
            context,
            endpoint: Endpoint::default(),
            options: Options::default(),
        }
    }

    async fn parse(body: &str) -> Assistant {
        let mut acc = Accumulator::new();
        let mut events = http::replay(body, Parser::new(model(true).cost));
        while let Some(event) = events.next().await {
            acc.apply(&event.unwrap());
        }
        acc.finish()
    }

    #[test]
    fn the_system_prompt_becomes_instructions() {
        let context = Context {
            system: Some("be terse".into()),
            messages: vec![Message::user("hi")],
            ..Context::default()
        };
        let b = body(&request(context, false));
        assert_eq!(b["instructions"], "be terse");
        assert_eq!(b["input"][0]["content"][0]["type"], "input_text");
        // Not stored: we keep the history, so the provider does not have to.
        assert_eq!(b["store"], json!(false));
    }

    #[test]
    fn a_reasoning_model_asks_for_the_encrypted_content() {
        let context = Context {
            messages: vec![Message::user("think")],
            ..Context::default()
        };
        let b = body(&request(context.clone(), true));
        assert_eq!(b["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(b["reasoning"]["effort"], "medium");

        // A model that does not reason should not be asked to.
        let plain = body(&request(context, false));
        assert!(plain.get("include").is_none());
        assert!(plain.get("reasoning").is_none());
    }

    #[test]
    fn tools_are_flat_rather_than_wrapped() {
        let context = Context {
            messages: vec![Message::user("x")],
            tools: vec![Tool {
                name: "locate_place".into(),
                description: "find a place".into(),
                schema: json!({"type": "object", "properties": {}, "required": []}),
            }],
            ..Context::default()
        };
        let b = body(&request(context, false));
        // Chat Completions nests these under `function`; Responses does not.
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["name"], "locate_place");
        assert!(b["tools"][0].get("function").is_none());
    }

    #[test]
    fn a_reasoning_block_is_replayed_exactly_as_it_arrived() {
        let item = json!({
            "type": "reasoning",
            "id": "rs_abc",
            "encrypted_content": "gAAAAA...",
            "summary": []
        });
        let context = Context {
            messages: vec![
                Message::user("think"),
                Message::Assistant(Assistant {
                    content: vec![
                        Block::Thinking {
                            text: "hmm".into(),
                            opaque: Some(item.clone()),
                        },
                        Block::text("done"),
                    ],
                    ..Assistant::pending()
                }),
            ],
            ..Context::default()
        };
        let b = body(&request(context, true));
        // Byte-for-byte, including the id and the blob. Anything less and the
        // next call is rejected for citing state it cannot prove it was given.
        assert_eq!(b["input"][1], item);
        assert_eq!(b["input"][2]["content"][0]["text"], "done");
    }

    #[test]
    fn reasoning_with_nothing_to_replay_is_left_out() {
        let context = Context {
            messages: vec![Message::Assistant(Assistant {
                content: vec![Block::Thinking {
                    text: "local only".into(),
                    opaque: None,
                }],
                ..Assistant::pending()
            })],
            ..Context::default()
        };
        let b = body(&request(context, true));
        assert_eq!(b["input"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_tool_call_and_its_result_are_two_top_level_items() {
        let context = Context {
            messages: vec![
                Message::Assistant(Assistant {
                    content: vec![Block::ToolCall {
                        id: "call_1".into(),
                        name: "locate_place".into(),
                        args: json!({"name": "Jaipur"}),
                    }],
                    stop: Stop::ToolUse,
                    ..Assistant::pending()
                }),
                Message::ToolResult {
                    call_id: "call_1".into(),
                    name: "locate_place".into(),
                    content: vec![Block::text("26.9,75.8")],
                    error: false,
                },
            ],
            ..Context::default()
        };
        let b = body(&request(context, false));
        assert_eq!(b["input"][0]["type"], "function_call");
        assert_eq!(b["input"][0]["arguments"], json!(r#"{"name":"Jaipur"}"#));
        assert_eq!(b["input"][1]["type"], "function_call_output");
        assert_eq!(b["input"][1]["call_id"], "call_1");
    }

    #[tokio::test]
    async fn reasoning_then_text_then_a_tool_call_keeps_its_order() {
        let body = concat!(
            r#"data: {"type":"response.created","response":{"id":"resp_1"}}"#, "\n\n",
            r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1"}}"#, "\n\n",
            r#"data: {"type":"response.reasoning_text.delta","output_index":0,"delta":"weighing it"}"#, "\n\n",
            r#"data: {"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_1","encrypted_content":"blob"}}"#, "\n\n",
            r#"data: {"type":"response.output_item.added","output_index":1,"item":{"type":"message"}}"#, "\n\n",
            r#"data: {"type":"response.output_text.delta","output_index":1,"delta":"let me look"}"#, "\n\n",
            r#"data: {"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","name":"locate_place","call_id":"call_9"}}"#, "\n\n",
            r#"data: {"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"name\":\"Jaipur\"}"}"#, "\n\n",
            r#"data: {"type":"response.output_item.done","output_index":2,"item":{"type":"function_call"}}"#, "\n\n",
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":1000,"output_tokens":50,"input_tokens_details":{"cached_tokens":900}}}}"#, "\n\n",
        );
        let message = parse(body).await;
        assert_eq!(message.content.len(), 3);
        assert!(matches!(
            &message.content[0],
            Block::Thinking { text, opaque: Some(o) }
                if text == "weighing it" && o["encrypted_content"] == "blob"
        ));
        assert_eq!(message.content[1].as_text(), Some("let me look"));
        let (id, name, args) = message.tool_calls().next().unwrap();
        assert_eq!((id, name), ("call_9", "locate_place"));
        assert_eq!(args, &json!({"name": "Jaipur"}));
        assert_eq!(message.response_id.as_deref(), Some("resp_1"));
        assert_eq!(
            (message.usage.input, message.usage.cache_read),
            (100, 900)
        );
    }

    #[tokio::test]
    async fn running_out_of_output_tokens_is_not_a_clean_finish() {
        let body = concat!(
            r#"data: {"type":"response.created","response":{"id":"r"}}"#, "\n\n",
            r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"message"}}"#, "\n\n",
            r#"data: {"type":"response.output_text.delta","output_index":0,"delta":"half a th"}"#, "\n\n",
            r#"data: {"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}"#, "\n\n",
        );
        let message = parse(body).await;
        assert_eq!(message.stop, Stop::Length);
        assert_eq!(message.text(), "half a th");
    }

    #[tokio::test]
    async fn a_failed_response_is_recorded_as_an_error() {
        let body = concat!(
            r#"data: {"type":"response.created","response":{"id":"r"}}"#, "\n\n",
            r#"data: {"type":"response.failed","response":{"error":{"message":"upstream"}}}"#, "\n\n",
        );
        assert_eq!(parse(body).await.stop, Stop::Error);
    }
}
