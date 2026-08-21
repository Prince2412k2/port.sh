//! Stream one turn from a configured tier and print every event, or the error.
use std::sync::Arc;
use futures_util::StreamExt;
use parley::types::{Context, Message, Options, Request};
fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let config = envoy::config::Config::read("envoy.json").unwrap();
        let ready = config.ready();
        for reason in &ready.skipped { eprintln!("skipped: {reason}"); }
        let Some(tier) = ready.tiers.into_iter().next() else { eprintln!("no tier"); return };
        eprintln!("tier: {}/{} key={} headers={}",
            tier.model.provider, tier.model.id,
            tier.endpoint.api_key.as_ref().map(|k| k.len()).unwrap_or(0),
            tier.endpoint.headers.len());
        let request = Request {
            model: tier.model.clone(),
            context: Context { system: None, messages: vec![Message::user("say hi")], tools: vec![] },
            endpoint: tier.endpoint.clone(),
            options: Options::default(),
        };
        let wire: Arc<dyn parley::Wire> = tier.wire.clone();
        let mut stream = wire.stream(request);
        let mut n = 0;
        while let Some(event) = stream.next().await {
            n += 1;
            match event { Ok(e) => println!("event: {e:?}"), Err(e) => println!("ERROR: {e}") }
        }
        println!("total events: {n}");
    });
}
