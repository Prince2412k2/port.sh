//! The agent, as a program.
//!
//! Started by a client, speaks the Agent Client Protocol on stdin and stdout,
//! and exits when the pipe closes. Everything it can do is decided by a config
//! file and by what the client says it can do at `initialize`; there are no
//! built-in tools, because this agent is not for one job.
//!
//! Diagnostics go to stderr. Stdout is protocol.

use std::sync::Arc;

use envoy::acp::server::{serve, Setup};
use envoy::config::Config;
use parley::types::{Api, Cost, Endpoint, Model, Options};

const USAGE: &str = "\
envoy -- an agent that speaks the Agent Client Protocol over stdio

    envoy [--config PATH] [--model PROVIDER/MODEL]

    --config PATH   tiers, budget and system prompt (default: $ENVOY_CONFIG,
                    then ./envoy.json). $ENVOY_CONFIG_CONTENT carries the whole
                    document instead, and wins: a client that composes one per
                    session has nowhere to write a file.
    --model  M      use only the tier whose provider/model matches, so a client
                    can pin one without rewriting the file
    --version
";

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let mut path = std::env::var("ENVOY_CONFIG").unwrap_or_else(|_| "envoy.json".into());
    let mut pin: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => match args.next() {
                Some(value) => path = value,
                None => return fail("--config needs a path"),
            },
            "--model" => match args.next() {
                Some(value) => pin = Some(value),
                None => return fail("--model needs a provider/model"),
            },
            "--version" => {
                println!("envoy {}", env!("CARGO_PKG_VERSION"));
                return std::process::ExitCode::SUCCESS;
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return std::process::ExitCode::SUCCESS;
            }
            other => return fail(&format!("unknown argument `{other}`")),
        }
    }

    let (mut config, source) = match Config::from_env_or(&path) {
        Ok(pair) => pair,
        Err(e) => return fail(&format!("{path}: {e}")),
    };
    eprintln!("envoy: configured from {source}");

    if let Some(pin) = &pin {
        config.tiers.retain(|t| {
            format!("{}/{}", t.provider, t.model) == *pin || t.model == *pin
        });
        if config.tiers.is_empty() {
            return fail(&format!("no tier in {source} matches `{pin}`"));
        }
    }

    let (wire, skipped) = config.fallback();
    for reason in &skipped {
        eprintln!("envoy: skipping {reason}");
    }
    let first = wire.first().map(|t| t.model.clone()).unwrap_or_else(|| {
        eprintln!("envoy: no tier has a usable credential; prompts will fail until one does");
        placeholder()
    });

    // MCP servers, if any. Tools are gathered once; a server that will not
    // start is named on stderr and the agent runs without it.
    let mut tools = envoy::Set::new();
    for server in &config.mcp_servers {
        let found = match (&server.url, &server.command) {
            (Some(url), _) => {
                envoy::mcp::Http::new(&server.name, url, server.headers.clone())
                    .tools()
                    .await
            }
            (None, Some(command)) => {
                match envoy::mcp::Connection::spawn(&server.name, command, &server.args).await {
                    Ok(connection) => connection.tools().await,
                    Err(e) => Err(e),
                }
            }
            (None, None) => Err(envoy::Failed::new("neither `url` nor `command` is set")),
        };
        match found {
            Err(e) => eprintln!("envoy: mcp `{}`: {}", server.name, e.0),
            Ok(found) => {
                eprintln!("envoy: mcp `{}` offers {} tool(s)", server.name, found.len());
                for tool in found {
                    tools.add(tool);
                }
            }
        }
    }

    let setup = Arc::new(Setup {
        wire,
        model: first,
        endpoint: Endpoint::default(),
        options: Options::default(),
        // Nothing is compiled in: this agent's tools come from the client that
        // started it, and from the MCP servers named in the config.
        tools: Arc::new(tools),
        budget: config.budget(),
        system: config.system.clone(),
        compaction: config.compaction(),
        summariser: config.summariser(),
        store: config.store(),
    });

    match serve(tokio::io::stdin(), tokio::io::stdout(), setup).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => fail(&format!("{e}")),
    }
}

/// Stands in when nothing is configured, so `initialize` still answers and the
/// client can show why rather than seeing a process that died.
fn placeholder() -> Model {
    Model {
        id: "none".into(),
        name: "no model configured".into(),
        provider: "none".into(),
        api: Api::OpenaiCompletions,
        context_window: 8192,
        max_output: None,
        reasoning: false,
        cost: Cost::default(),
    }
}

fn fail(message: &str) -> std::process::ExitCode {
    eprintln!("envoy: {message}");
    std::process::ExitCode::FAILURE
}
