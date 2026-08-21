//! Everything about the Codex path that can be checked without spending a token.
//!
//! Reads the credential file, reports what it found without printing any of it,
//! and prints the request body the Responses wire would send.
use parley::auth::Codex;
use parley::types::{Api, Assistant, Block, Context, Cost, Endpoint, Message, Model, Options, Request, Stop};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        format!("{}/.codex/auth.json", std::env::var("HOME").unwrap_or_default())
    });
    match Codex::read(&path) {
        Err(e) => println!("credential: {e}"),
        Ok(codex) => {
            let now = parley::auth::now();
            println!("credential : {path}");
            println!("  mode     : {:?}", codex.auth_mode);
            println!("  account  : {}", codex.account().map(|a| format!("found ({} chars)", a.len())).unwrap_or("MISSING".into()));
            println!("  expires  : {:?}s from now", codex.expires_in(now));
            println!("  stale    : {}", codex.stale(now));
            let resolved = codex.resolved();
            println!("  headers  : {:?}", resolved.headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>());
            println!("  source   : {}", resolved.source);
        }
    }

    // The body a reasoning turn with a replayed thought would produce.
    let model = Model {
        id: "gpt-5-codex".into(), name: "GPT-5 Codex".into(), provider: "openai-codex".into(),
        api: Api::OpenaiResponses, context_window: 272_000, max_output: None,
        reasoning: true, cost: Cost::default(),
    };
    let reasoning = serde_json::json!({
        "type": "reasoning", "id": "rs_abc", "encrypted_content": "gAAAAA...", "summary": []
    });
    let request = Request {
        model,
        context: Context {
            system: Some("be terse".into()),
            messages: vec![
                Message::user("where is Jaipur?"),
                Message::Assistant(Assistant {
                    content: vec![
                        Block::Thinking { text: "recalling".into(), opaque: Some(reasoning) },
                        Block::ToolCall { id: "call_1".into(), name: "locate_place".into(), args: serde_json::json!({"name": "Jaipur"}) },
                    ],
                    stop: Stop::ToolUse,
                    ..Assistant::pending()
                }),
                Message::ToolResult {
                    call_id: "call_1".into(), name: "locate_place".into(),
                    content: vec![Block::text("26.9,75.8")], error: false,
                },
            ],
            tools: vec![parley::Tool {
                name: "locate_place".into(),
                description: "find a place".into(),
                schema: serde_json::json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"],"additionalProperties":false}),
            }],
        },
        endpoint: Endpoint { base_url: "https://chatgpt.com/backend-api/codex".into(), api_key: None, headers: vec![] },
        options: Options { cache_key: Some("envoy-demo".into()), ..Options::default() },
    };
    println!("\nPOST https://chatgpt.com/backend-api/codex/responses");
    println!("{}", serde_json::to_string_pretty(&parley::api::openai_responses::body(&request)).unwrap());
}
