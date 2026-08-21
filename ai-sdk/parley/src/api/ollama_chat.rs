//! `POST /api/chat` -- Ollama's own api.
//!
//! Not the OpenAI-compatible shim. The compatibility layer is a translation of
//! this one, and translations lose things: reasoning arrives here as a
//! `thinking` field on the message rather than being folded into the text, and
//! the request has a `think` switch the shim has nowhere to put.
//!
//! Four differences from everything else in this directory, each of which will
//! silently produce nothing if you assume otherwise.
//!
//! **It streams newline-delimited JSON, not server-sent events.** One complete
//! document per line, no `data:` prefix. See `ndjson`.
//!
//! **Tool arguments are an object, not a string.** OpenAI streams them as text
//! fragments that have to be reassembled and parsed; here the whole object
//! arrives at once, already parsed.
//!
//! **Tool calls have no id.** Nothing pairs a call with its result on the wire
//! except order and the tool's name, so ids are ours to invent -- and a tool
//! result goes back with `tool_name` rather than a call id.
//!
//! **Usage is named differently.** `prompt_eval_count` and `eval_count`, and
//! there is no cached-token figure at all: Ollama does not report one, so cache
//! reads are zero here rather than unknown.

use serde_json::{json, Map, Value};

use crate::error::Result;
use crate::http;
use crate::sse::Frame;
use crate::stream::{Event, Kind};
use crate::types::{Assistant, Block, Context, Cost, Effort, Message, Request, Stop, Usage};
use crate::wire::{EventStream, Frames, Wire};

pub struct OllamaChat;

impl Wire for OllamaChat {
    fn stream(&self, request: Request) -> EventStream {
        // `base_url` is the host: `https://ollama.com` for the cloud, or a local
        // daemon's address. The path is this api's, not the caller's.
        let url = format!("{}/api/chat", request.endpoint.base_url.trim_end_matches('/'));
        let mut builder = http::client().post(url).json(&body(&request));
        if let Some(key) = &request.endpoint.api_key {
            builder = builder.bearer_auth(key);
        }
        for (name, value) in &request.endpoint.headers {
            builder = builder.header(name, value);
        }
        http::ndjson_stream(builder, Parser::new(request.model.cost))
    }
}

pub fn body(request: &Request) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model.id));
    body.insert("messages".into(), json!(messages(&request.context)));
    body.insert("stream".into(), json!(true));
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
    if request.model.reasoning {
        // A switch rather than a level: the api takes a boolean, and models that
        // accept a level take it as a string here.
        body.insert(
            "think".into(),
            match request.options.effort {
                Effort::Off => json!(false),
                Effort::Minimal | Effort::Low => json!("low"),
                Effort::Medium => json!("medium"),
                Effort::High => json!("high"),
            },
        );
    }
    let mut options = Map::new();
    if let Some(t) = request.options.temperature {
        options.insert("temperature".into(), json!(t));
    }
    if let Some(max) = request.options.max_output.or(request.model.max_output) {
        options.insert("num_predict".into(), json!(max));
    }
    if !options.is_empty() {
        body.insert("options".into(), Value::Object(options));
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
            Message::User { content } => {
                let mut entry = Map::new();
                entry.insert("role".into(), json!("user"));
                entry.insert("content".into(), json!(flatten(content)));
                // Images ride beside the text rather than inside it.
                let images: Vec<&str> = content
                    .iter()
                    .filter_map(|b| match b {
                        Block::Image { data, .. } => Some(data.as_str()),
                        _ => None,
                    })
                    .collect();
                if !images.is_empty() {
                    entry.insert("images".into(), json!(images));
                }
                out.push(Value::Object(entry));
            }
            Message::Assistant(a) => out.push(assistant(a)),
            Message::ToolResult { name, content, .. } => out.push(json!({
                "role": "tool",
                // The name, because there is no id to pair with.
                "tool_name": name,
                "content": flatten(content),
            })),
        }
    }
    out
}

fn assistant(a: &Assistant) -> Value {
    let mut entry = Map::new();
    entry.insert("role".into(), json!("assistant"));
    entry.insert("content".into(), json!(flatten(&a.content)));
    let thinking: String = a
        .content
        .iter()
        .filter_map(|b| match b {
            Block::Thinking { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if !thinking.is_empty() {
        entry.insert("thinking".into(), json!(thinking));
    }
    let calls: Vec<Value> = a
        .tool_calls()
        .map(|(_, name, args)| json!({ "function": { "name": name, "arguments": args } }))
        .collect();
    if !calls.is_empty() {
        entry.insert("tool_calls".into(), json!(calls));
    }
    Value::Object(entry)
}

fn flatten(blocks: &[Block]) -> String {
    blocks.iter().filter_map(Block::as_text).collect()
}

pub struct Parser {
    cost: Cost,
    started: bool,
    next_index: usize,
    text: Option<usize>,
    thinking: Option<usize>,
    /// Whether the model asked for a tool, which decides the stop reason: the
    /// api reports `done_reason: "stop"` either way.
    asked: bool,
}

impl Parser {
    pub fn new(cost: Cost) -> Parser {
        Parser {
            cost,
            started: false,
            next_index: 0,
            text: None,
            thinking: None,
            asked: false,
        }
    }

    fn claim(&mut self) -> usize {
        let index = self.next_index;
        self.next_index += 1;
        index
    }

    fn usage(&self, line: &Value) -> Usage {
        let n = |key: &str| line.get(key).and_then(Value::as_u64).unwrap_or(0);
        let mut out = Usage {
            input: n("prompt_eval_count"),
            output: n("eval_count"),
            // Ollama reports no cached figure. Zero here means "not reported",
            // and pretending otherwise would make a cache-hit rate up.
            cache_read: 0,
            cache_write: 0,
            cost: 0.0,
        };
        out.cost = self.cost.of(&out);
        out
    }
}

impl Frames for Parser {
    fn frame(&mut self, frame: &Frame) -> Vec<Result<Event>> {
        let Ok(line): std::result::Result<Value, _> = serde_json::from_str(frame.data.trim()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(Ok(Event::Start { response_id: None }));
        }

        if let Some(message) = line.get("message") {
            if let Some(thought) = message.get("thinking").and_then(Value::as_str) {
                if !thought.is_empty() {
                    let index = match self.thinking {
                        Some(index) => index,
                        None => {
                            let index = self.claim();
                            self.thinking = Some(index);
                            out.push(Ok(Event::BlockStart {
                                index,
                                kind: Kind::Thinking,
                                name: None,
                                id: None,
                            }));
                            index
                        }
                    };
                    out.push(Ok(Event::Thinking {
                        index,
                        delta: thought.to_string(),
                    }));
                }
            }
            if let Some(text) = message.get("content").and_then(Value::as_str) {
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
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                // Text after a tool call belongs in its own block, or the record
                // of what order things happened in is lost.
                self.text = None;
                self.thinking = None;
                for call in calls {
                    let function = call.get("function");
                    let name = function
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let index = self.claim();
                    self.asked = true;
                    out.push(Ok(Event::BlockStart {
                        index,
                        kind: Kind::ToolCall,
                        name: Some(name),
                        // Invented, because the wire carries none. Stable within
                        // one turn, which is all anything needs it for.
                        id: Some(format!("oc_{index}")),
                    }));
                    // The whole object at once: no fragments to reassemble.
                    let arguments = function
                        .and_then(|f| f.get("arguments"))
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    out.push(Ok(Event::ToolArgs {
                        index,
                        delta: arguments.to_string(),
                    }));
                    out.push(Ok(Event::BlockEnd { index }));
                }
            }
        }

        if line.get("done").and_then(Value::as_bool) == Some(true) {
            out.push(Ok(Event::Usage(self.usage(&line))));
            let stop = match line.get("done_reason").and_then(Value::as_str) {
                Some("length") => Stop::Length,
                _ if self.asked => Stop::ToolUse,
                _ => Stop::End,
            };
            out.push(Ok(Event::Done { stop }));
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
            id: "qwen3.5:397b".into(),
            name: "qwen3.5".into(),
            provider: "ollama-cloud".into(),
            api: Api::OllamaChat,
            context_window: 262_144,
            max_output: None,
            reasoning,
            cost: Cost {
                input: 1.0,
                output: 2.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
        }
    }

    fn request(context: Context, reasoning: bool) -> Request {
        Request {
            model: model(reasoning),
            context,
            endpoint: Endpoint {
                base_url: "https://ollama.com".into(),
                api_key: Some("k".into()),
                headers: vec![],
            },
            options: Options::default(),
        }
    }

    async fn parse(body: &str) -> Assistant {
        let mut acc = Accumulator::new();
        let mut events = http::replay_ndjson(body, Parser::new(model(true).cost));
        while let Some(event) = events.next().await {
            acc.apply(&event.unwrap());
        }
        acc.finish()
    }

    #[test]
    fn a_reasoning_model_is_asked_to_think_and_a_plain_one_is_not() {
        let context = Context {
            messages: vec![Message::user("hi")],
            ..Context::default()
        };
        assert_eq!(body(&request(context.clone(), true))["think"], json!("medium"));
        assert!(body(&request(context, false)).get("think").is_none());
    }

    #[test]
    fn tuning_goes_under_options_not_at_the_top() {
        let mut r = request(
            Context {
                messages: vec![Message::user("hi")],
                ..Context::default()
            },
            false,
        );
        r.options.temperature = Some(0.2);
        r.options.max_output = Some(256);
        let b = body(&r);
        assert_eq!(b["options"]["temperature"], json!(0.2));
        // Named `num_predict` here, not `max_tokens`.
        assert_eq!(b["options"]["num_predict"], json!(256));
        assert!(b.get("temperature").is_none());
    }

    #[test]
    fn a_tool_result_is_paired_by_name_because_there_is_no_id() {
        let context = Context {
            messages: vec![
                Message::Assistant(Assistant {
                    content: vec![Block::ToolCall {
                        id: "oc_0".into(),
                        name: "locate_place".into(),
                        args: json!({"name": "Jaipur"}),
                    }],
                    stop: Stop::ToolUse,
                    ..Assistant::pending()
                }),
                Message::ToolResult {
                    call_id: "oc_0".into(),
                    name: "locate_place".into(),
                    content: vec![Block::text("26.9,75.8")],
                    error: false,
                },
            ],
            ..Context::default()
        };
        let b = body(&request(context, false));
        // Arguments go back as an object, not a string.
        assert_eq!(
            b["messages"][0]["tool_calls"][0]["function"]["arguments"],
            json!({"name": "Jaipur"})
        );
        assert!(b["messages"][0]["tool_calls"][0].get("id").is_none());
        assert_eq!(b["messages"][1]["role"], json!("tool"));
        assert_eq!(b["messages"][1]["tool_name"], json!("locate_place"));
    }

    #[test]
    fn reasoning_goes_back_in_its_own_field() {
        let context = Context {
            messages: vec![Message::Assistant(Assistant {
                content: vec![
                    Block::Thinking {
                        text: "weighing it".into(),
                        opaque: None,
                    },
                    Block::text("Rajasthan."),
                ],
                ..Assistant::pending()
            })],
            ..Context::default()
        };
        let b = body(&request(context, true));
        assert_eq!(b["messages"][0]["thinking"], json!("weighing it"));
        assert_eq!(b["messages"][0]["content"], json!("Rajasthan."));
    }

    #[test]
    fn an_image_rides_beside_the_text() {
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
        let b = body(&request(context, false));
        assert_eq!(b["messages"][0]["content"], json!("what is this"));
        assert_eq!(b["messages"][0]["images"], json!(["AAAA"]));
    }

    #[tokio::test]
    async fn text_arrives_a_token_at_a_time() {
        let body = concat!(
            r#"{"model":"m","message":{"role":"assistant","content":"Jai"},"done":false}"#, "\n",
            r#"{"model":"m","message":{"role":"assistant","content":"pur"},"done":false}"#, "\n",
            r#"{"model":"m","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":12,"eval_count":3}"#, "\n",
        );
        let message = parse(body).await;
        assert_eq!(message.text(), "Jaipur");
        assert_eq!(message.stop, Stop::End);
        assert_eq!((message.usage.input, message.usage.output), (12, 3));
        // No cached figure is reported, so none is claimed.
        assert_eq!(message.usage.cache_read, 0);
    }

    #[tokio::test]
    async fn thinking_then_talking_then_a_tool_call_keeps_its_order() {
        let body = concat!(
            r#"{"message":{"role":"assistant","content":"","thinking":"which place"},"done":false}"#, "\n",
            r#"{"message":{"role":"assistant","content":"let me look. "},"done":false}"#, "\n",
            r#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"locate_place","arguments":{"name":"Jaipur"}}}]},"done":false}"#, "\n",
            r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":20,"eval_count":8}"#, "\n",
        );
        let message = parse(body).await;
        assert_eq!(message.content.len(), 3, "{:?}", message.content);
        assert!(matches!(&message.content[0], Block::Thinking { text, .. } if text == "which place"));
        assert_eq!(message.content[1].as_text(), Some("let me look. "));
        let (id, name, args) = message.tool_calls().next().unwrap();
        assert_eq!(name, "locate_place");
        // An id we invented, because the wire carries none.
        assert_eq!(id, "oc_2");
        assert_eq!(args, &json!({"name": "Jaipur"}));
        // `done_reason` is "stop" even when tools were asked for, so the stop
        // reason has to come from having seen the call.
        assert_eq!(message.stop, Stop::ToolUse);
    }

    #[tokio::test]
    async fn two_tool_calls_in_one_line_become_two_blocks() {
        let body = concat!(
            r#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"a","arguments":{"x":1}}},{"function":{"name":"b","arguments":{"y":2}}}]},"done":false}"#, "\n",
            r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop"}"#, "\n",
        );
        let message = parse(body).await;
        let calls: Vec<&str> = message.tool_calls().map(|(_, name, _)| name).collect();
        assert_eq!(calls, vec!["a", "b"]);
        let ids: Vec<&str> = message.tool_calls().map(|(id, _, _)| id).collect();
        assert_eq!(ids, vec!["oc_0", "oc_1"], "ids must be distinct");
    }

    #[tokio::test]
    async fn running_out_of_tokens_is_reported_as_such() {
        let body = concat!(
            r#"{"message":{"role":"assistant","content":"half a th"},"done":false}"#, "\n",
            r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"length"}"#, "\n",
        );
        let message = parse(body).await;
        assert_eq!(message.stop, Stop::Length);
        assert_eq!(message.text(), "half a th");
    }

    #[tokio::test]
    async fn a_stream_cut_off_before_done_says_so() {
        let body = r#"{"message":{"role":"assistant","content":"half"},"done":false}"#;
        let message = parse(body).await;
        assert_eq!(message.stop, Stop::Error);
        assert_eq!(message.text(), "half");
    }
}
