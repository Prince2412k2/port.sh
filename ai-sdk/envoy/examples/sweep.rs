//! Try several models through whatever the config's first tier says, and report
//! what each one answered. The tool to reach for when a provider is refusing
//! you: every model failing identically is a credential, one differing is a
//! model.
use std::sync::Arc;
use futures_util::StreamExt;
use parley::types::{Context, Message, Request};

#[tokio::main]
async fn main() {
    let config = envoy::config::Config::read("envoy.json").expect("envoy.json");
    let ready = config.ready();
    for reason in &ready.skipped {
        println!("skipped: {reason}");
    }
    let Some(tier) = ready.tiers.first() else {
        println!("no tier has a usable credential");
        return;
    };
    let first = &config.tiers[0];
    println!(
        "api={:?} base={} key={} chars",
        first.api,
        first.base_url,
        tier.endpoint.api_key.as_ref().map(|k| k.len()).unwrap_or(0)
    );

    let ids: Vec<String> = std::env::args().skip(1).collect();
    let ids = if ids.is_empty() {
        vec![
            "gpt-oss:20b".into(),
            "gpt-oss:120b".into(),
            "qwen3.5:397b".into(),
            "kimi-k3".into(),
        ]
    } else {
        ids
    };

    for id in ids {
        let mut model = tier.model.clone();
        model.id = id.clone();
        let request = Request {
            model,
            context: Context {
                system: None,
                messages: vec![Message::user("Reply with exactly: ok")],
                tools: vec![],
            },
            endpoint: tier.endpoint.clone(),
            options: Default::default(),
        };
        let wire: Arc<dyn parley::Wire> = tier.wire.clone();
        let mut events = wire.stream(request);
        let mut acc = parley::Accumulator::new();
        let mut error = None;
        while let Some(event) = events.next().await {
            match event {
                Ok(e) => acc.apply(&e),
                Err(e) => {
                    error = Some(e.to_string());
                    break;
                }
            }
        }
        match error {
            Some(e) => println!("{id:22} ERROR  {e}"),
            None => {
                let m = acc.finish();
                println!(
                    "{id:22} {:?}  text={:?} in={} out={}",
                    m.stop,
                    m.text().trim(),
                    m.usage.input,
                    m.usage.output
                );
            }
        }
    }
}
