//! `POST /chat/completions`.
//!
//! The oldest of the shapes and the one everything imitates, which is why it is
//! first: Ollama Cloud speaks it, and so does every endpoint that describes
//! itself as OpenAI-compatible. One implementation, most of the field.
//!
//! Two things about it are awkward enough to be worth naming.
//!
//! **Nothing announces a block.** Text simply starts arriving as
//! `delta.content`, and tool calls arrive as entries in `delta.tool_calls`
//! carrying their own index, which is not our block index. So the parser
//! allocates block indices as it meets things, and closes the open text block
//! when a tool call starts -- otherwise text after a tool call would be
//! appended to the text before it, and the record of what order the model did
//! things in would be gone.
//!
//! **Usage arrives after the content, in a chunk with no choices,** and only if
//! `stream_options.include_usage` was asked for. Forgetting it is not an error;
//! it just silently reports every conversation as costing nothing.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::error::Result;
use crate::http;
use crate::sse::Frame;
use crate::stream::{Event, Kind};
use crate::types::{Assistant, Block, Context, Cost, Message, Request, Stop, Usage};
use crate::wire::{EventStream, Frames, Wire};

pub struct OpenaiCompletions;

impl Wire for OpenaiCompletions {
    fn stream(&self, request: Request) -> EventStream {
        let url = format!(
            "{}/chat/completions",
            request.endpoint.base_url.trim_end_matches('/')
        );
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

/// The request body. Pure, so it can be asserted on without a socket.
pub fn body(request: &Request) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model.id));
    body.insert("messages".into(), json!(messages(&request.context)));
    body.insert("stream".into(), json!(true));
    // Without this, usage is never reported for a streamed response.
    body.insert(
        "stream_options".into(),
        json!({ "include_usage": true }),
    );
    if !request.context.tools.is_empty() {
        let tools: Vec<Value> = request
            .context
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.schema,
                    }
                })
            })
            .collect();
        body.insert("tools".into(), json!(tools));
    }
    if let Some(max) = request.options.max_output.or(request.model.max_output) {
        body.insert("max_tokens".into(), json!(max));
    }
    if let Some(t) = request.options.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if let Some(key) = &request.options.cache_key {
        body.insert("prompt_cache_key".into(), json!(key));
    }
    Value::Object(body)
}

fn messages(context: &Context) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(system) = &context.system {
        out.push(json!({ "role": "system", "content": system }));
    }
    for message in &context.messages {
        match message {
            Message::User { content } => out.push(json!({
                "role": "user",
                "content": user_content(content),
            })),
            Message::Assistant(a) => out.push(assistant(a)),
            Message::ToolResult {
                call_id, content, ..
            } => out.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": flatten(content),
            })),
        }
    }
    out
}

/// A plain string when it is only text, the array form when there are images.
/// Some compatible endpoints reject the array form for text-only messages.
fn user_content(blocks: &[Block]) -> Value {
    if blocks.iter().all(|b| b.as_text().is_some()) {
        return json!(flatten(blocks));
    }
    let parts: Vec<Value> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } => Some(json!({ "type": "text", "text": text })),
            Block::Image { data, mime } => Some(json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{mime};base64,{data}") }
            })),
            _ => None,
        })
        .collect();
    json!(parts)
}

fn assistant(a: &Assistant) -> Value {
    let mut msg = Map::new();
    msg.insert("role".into(), json!("assistant"));
    let text = flatten(&a.content);
    // Reasoning is dropped on purpose. This api has nowhere to put it, and the
    // providers that invented a field for it disagree about the name; sending a
    // thinking block as ordinary text would make the model treat its own
    // scratch work as something it said out loud.
    msg.insert("content".into(), json!(text));
    let calls: Vec<Value> = a
        .tool_calls()
        .map(|(id, name, args)| {
            json!({
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": args.to_string() }
            })
        })
        .collect();
    if !calls.is_empty() {
        msg.insert("tool_calls".into(), json!(calls));
    }
    Value::Object(msg)
}

fn flatten(blocks: &[Block]) -> String {
    blocks.iter().filter_map(Block::as_text).collect()
}

/// Chunk-to-event translation, and the only stateful part.
pub struct Parser {
    cost: Cost,
    started: bool,
    next_index: usize,
    /// The open text block, if one is open.
    text: Option<usize>,
    /// The provider's `tool_calls[].index` mapped onto our block indices.
    tools: HashMap<u64, usize>,
    stop: Option<Stop>,
}

impl Parser {
    pub fn new(cost: Cost) -> Parser {
        Parser {
            cost,
            started: false,
            next_index: 0,
            text: None,
            tools: HashMap::new(),
            stop: None,
        }
    }

    fn claim(&mut self) -> usize {
        let index = self.next_index;
        self.next_index += 1;
        index
    }
}

impl Frames for Parser {
    fn frame(&mut self, frame: &Frame) -> Vec<Result<Event>> {
        let data = frame.data.trim();
        if data.is_empty() {
            return Vec::new();
        }
        if data == "[DONE]" {
            return vec![Ok(Event::Done {
                stop: self.stop.unwrap_or(Stop::End),
            })];
        }
        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            // A frame we cannot read is not worth ending the turn over; the
            // stream carries keep-alives and, on some gateways, status objects
            // that are not chunks at all.
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();

        if !self.started {
            self.started = true;
            out.push(Ok(Event::Start {
                response_id: chunk
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }));
        }

        if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
            out.push(Ok(Event::Usage(self.usage(usage))));
        }

        let choice = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first());
        let Some(choice) = choice else {
            return out;
        };

        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    let index = match self.text {
                        Some(index) => index,
                        None => {
                            let index = self.claim();
                            self.text = Some(index);
                            out.push(Ok(Event::BlockStart {
                                index,
                                kind: Kind::Text,
                                name: None,
                                id: None,
                            }));
                            index
                        }
                    };
                    out.push(Ok(Event::Text {
                        index,
                        delta: text.to_string(),
                    }));
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                // A tool call means whatever text was being written is over.
                self.text = None;
                for call in calls {
                    let slot = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let function = call.get("function");
                    let index = match self.tools.get(&slot) {
                        Some(index) => *index,
                        None => {
                            let index = self.claim();
                            self.tools.insert(slot, index);
                            out.push(Ok(Event::BlockStart {
                                index,
                                kind: Kind::ToolCall,
                                name: function
                                    .and_then(|f| f.get("name"))
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                                id: call
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            }));
                            index
                        }
                    };
                    if let Some(args) = function
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                    {
                        if !args.is_empty() {
                            out.push(Ok(Event::ToolArgs {
                                index,
                                delta: args.to_string(),
                            }));
                        }
                    }
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop = Some(match reason {
                "length" => Stop::Length,
                "tool_calls" | "function_call" => Stop::ToolUse,
                "content_filter" => Stop::Refusal,
                _ => Stop::End,
            });
        }
        out
    }

    fn finish(&mut self) -> Vec<Result<Event>> {
        let mut out: Vec<Result<Event>> = Vec::new();
        for index in 0..self.next_index {
            out.push(Ok(Event::BlockEnd { index }));
        }
        out
    }
}

impl Parser {
    fn usage(&self, usage: &Value) -> Usage {
        let n = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let cached = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        // `prompt_tokens` counts the cached part too. Subtracting keeps a
        // context-window total from double-counting it.
        let mut out = Usage {
            input: n("prompt_tokens").saturating_sub(cached),
            output: n("completion_tokens"),
            cache_read: cached,
            cache_write: 0,
            cost: 0.0,
        };
        out.cost = self.cost.of(&out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Api, Endpoint, Model, Options, Tool};
    use futures_util::StreamExt;

    fn model() -> Model {
        Model {
            id: "qwen3-coder:480b".into(),
            name: "Qwen3 Coder".into(),
            provider: "ollama".into(),
            api: Api::OpenaiCompletions,
            context_window: 262_144,
            max_output: None,
            reasoning: false,
            cost: Cost {
                input: 1.0,
                output: 2.0,
                cache_read: 0.5,
                cache_write: 0.0,
            },
        }
    }

    fn request(context: Context) -> Request {
        Request {
            model: model(),
            context,
            endpoint: Endpoint {
                base_url: "https://ollama.com/v1".into(),
                api_key: Some("k".into()),
                headers: vec![],
            },
            options: Options::default(),
        }
    }

    async fn parse(body: &str) -> Vec<Event> {
        http::replay(body, Parser::new(model().cost))
            .filter_map(|e| async move { e.ok() })
            .collect()
            .await
    }

    #[test]
    fn a_text_only_message_is_sent_as_a_string() {
        let context = Context {
            messages: vec![Message::user("hi")],
            ..Context::default()
        };
        let b = body(&request(context));
        assert_eq!(b["messages"][0]["content"], json!("hi"));
        assert_eq!(b["stream_options"]["include_usage"], json!(true));
    }

    #[test]
    fn an_image_forces_the_array_form() {
        let context = Context {
            messages: vec![Message::User {
                content: vec![
                    Block::text("what is this"),
                    Block::Image {
                        data: "AAAA".into(),
                        mime: "image/png".into(),
                    },
                ],
            }],
            ..Context::default()
        };
        let b = body(&request(context));
        let parts = b["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], json!("text"));
        assert_eq!(
            parts[1]["image_url"]["url"],
            json!("data:image/png;base64,AAAA")
        );
    }

    #[test]
    fn a_tool_result_is_paired_with_the_call_that_asked_for_it() {
        let context = Context {
            messages: vec![
                Message::Assistant(Assistant {
                    content: vec![Block::ToolCall {
                        id: "c1".into(),
                        name: "locate_place".into(),
                        args: json!({"name": "Jaipur"}),
                    }],
                    stop: Stop::ToolUse,
                    ..Assistant::pending()
                }),
                Message::ToolResult {
                    call_id: "c1".into(),
                    name: "locate_place".into(),
                    content: vec![Block::text("26.9,75.8")],
                    error: false,
                },
            ],
            ..Context::default()
        };
        let b = body(&request(context));
        let call = &b["messages"][0]["tool_calls"][0];
        assert_eq!(call["id"], json!("c1"));
        // Arguments go on the wire as a JSON *string*, not an object.
        assert_eq!(call["function"]["arguments"], json!(r#"{"name":"Jaipur"}"#));
        assert_eq!(b["messages"][1]["role"], json!("tool"));
        assert_eq!(b["messages"][1]["tool_call_id"], json!("c1"));
    }

    #[test]
    fn tools_are_wrapped_in_the_function_envelope() {
        let context = Context {
            messages: vec![Message::user("x")],
            tools: vec![Tool {
                name: "show_map".into(),
                description: "put a point on screen".into(),
                schema: json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false}),
            }],
            ..Context::default()
        };
        let b = body(&request(context));
        assert_eq!(b["tools"][0]["type"], json!("function"));
        assert_eq!(b["tools"][0]["function"]["name"], json!("show_map"));
    }

    #[tokio::test]
    async fn text_chunks_become_one_block() {
        let body = concat!(
            "data: {\"id\":\"c-1\",\"choices\":[{\"delta\":{\"content\":\"Jai\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c-1\",\"choices\":[{\"delta\":{\"content\":\"pur\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let events = parse(body).await;
        assert!(matches!(&events[0], Event::Start { response_id } if response_id.as_deref() == Some("c-1")));
        let mut acc = crate::Accumulator::new();
        for e in &events {
            acc.apply(e);
        }
        let msg = acc.finish();
        assert_eq!(msg.text(), "Jaipur");
        assert_eq!(msg.stop, Stop::End);
    }

    #[tokio::test]
    async fn a_tool_call_arrives_across_chunks_and_ends_up_parsed() {
        let body = concat!(
            "data: {\"id\":\"c\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"locate_place\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"id\":\"c\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"name\\\":\"}}]}}]}\n\n",
            "data: {\"id\":\"c\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"Jaipur\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut acc = crate::Accumulator::new();
        for e in parse(body).await {
            acc.apply(&e);
        }
        let msg = acc.finish();
        assert_eq!(msg.stop, Stop::ToolUse);
        let (id, name, args) = msg.tool_calls().next().unwrap();
        assert_eq!((id, name), ("call_1", "locate_place"));
        assert_eq!(args, &json!({"name": "Jaipur"}));
    }

    #[tokio::test]
    async fn text_after_a_tool_call_opens_a_new_block() {
        // The interleaving case. If the parser reused the first text block,
        // "before" and "after" would merge and the tool call would look like it
        // happened at the end.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"before \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"t\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"after\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut acc = crate::Accumulator::new();
        for e in parse(body).await {
            acc.apply(&e);
        }
        let msg = acc.finish();
        assert_eq!(msg.content.len(), 3, "{:?}", msg.content);
        assert_eq!(msg.content[0].as_text(), Some("before "));
        assert!(matches!(msg.content[1], Block::ToolCall { .. }));
        assert_eq!(msg.content[2].as_text(), Some("after"));
    }

    #[tokio::test]
    async fn usage_is_read_and_priced_with_the_cache_read_split_out() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1000,\"completion_tokens\":100,\"prompt_tokens_details\":{\"cached_tokens\":800}}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut acc = crate::Accumulator::new();
        for e in parse(body).await {
            acc.apply(&e);
        }
        let u = *acc.usage();
        assert_eq!((u.input, u.cache_read, u.output), (200, 800, 100));
        // 200 in at $1/M, 800 cached at $0.50/M, 100 out at $2/M.
        assert!((u.cost - (0.0002 + 0.0004 + 0.0002)).abs() < 1e-9, "{}", u.cost);
        assert_eq!(u.context_tokens(), 1100);
    }

    #[tokio::test]
    async fn a_keep_alive_or_junk_frame_does_not_end_the_turn() {
        let body = concat!(
            ": ping\n\n",
            "data: not json at all\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut acc = crate::Accumulator::new();
        for e in parse(body).await {
            acc.apply(&e);
        }
        assert_eq!(acc.finish().text(), "ok");
    }
}
