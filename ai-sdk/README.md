# envoy

An AI agent that speaks the Agent Client Protocol over stdio, and the model
layer under it.

    parley/   talking to a language model, without caring which one
    envoy/    the agent: a turn loop, tools, and an ACP server

Two crates because they answer different questions. `parley` answers one
question at a time and has no opinion about tools beyond passing their schemas
along. `envoy` turns that into a conversation, and its binary is a thin shell
that makes the conversation reachable by a client.

## Running it

    cargo build --release
    ./target/release/envoy --config envoy.json

It reads JSON-RPC from stdin and writes protocol to stdout; stderr is
diagnostics. A client starts it as a child process -- `portfolio`'s
`servers.rs` would name it the way it names `opencode acp` today.

    --config PATH        tiers, budget, system prompt (default $ENVOY_CONFIG,
                         then ./envoy.json)
    --model PROVIDER/ID  use only the matching tier

Try it by hand. Wait for each reply before sending the next line: requests are
handled concurrently, so a pipelined `session/prompt` can outrun the
`session/new` that created its session.

    {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}
    {"jsonrpc":"2.0","id":2,"method":"session/new","params":{}}
    {"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"s1","prompt":[{"type":"text","text":"hello"}]}}

## Configuration

`envoy.json` carries the model catalogue, because the catalogue is not
discoverable: Ollama Cloud's `/v1/models` returns ids with no context window
and no prices, and the Codex backend publishes no list at all. Tiers are tried
in order and a tier that stops answering is fallen through without restarting
anything.

```json
{
  "system": "be concise",
  "budget": { "turns": 12, "toolCalls": 24 },
  "compaction": { "enabled": true, "reserve": 16384, "keepRecent": 20000 },
  "tiers": [
    { "provider": "ollama-cloud", "model": "gpt-oss:120b",
      "api": "openai-completions", "baseUrl": "https://ollama.com/v1",
      "contextWindow": 131072, "auth": { "env": ["OLLAMA_API_KEY"] } },
    { "provider": "openai-codex", "model": "gpt-5-codex",
      "api": "openai-responses", "baseUrl": "https://chatgpt.com/backend-api/codex",
      "contextWindow": 272000, "reasoning": true,
      "auth": { "codex": "~/.codex/auth.json" } }
  ],
  "mcpServers": [ { "name": "files", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"] } ]
}
```

Logging in is not this program's job. `auth.codex` reads the Codex CLI's own
credential file and never writes it back -- a refresh rotates the token, and
clobbering a working CLI login to save a copy would be a poor trade.

## Where tools come from

Three places, and the loop cannot tell them apart.

**The client.** Declared in `_meta` at `initialize` and called back on
`_envoy/tools/call`. This is the one addition to ACP, and it exists because a
tool that draws on the client's screen has to run where the screen is. It
replaces the loopback MCP server that `portfolio/src/mcp.rs` needs today --
no listener, no per-screen token, no HTTP hop.

```json
{"_meta": {"envoy/clientTools": {"tools": [
  {"name": "show_map", "title": "Show a map", "description": "put a point on screen",
   "schema": {"type": "object", "properties": {"lat": {"type": "number"}}, "required": ["lat"]}}
]}}}
```

**MCP servers**, named in the config. A `command` is a child process spoken to
over pipes; a `url` is the streamable HTTP transport, which may answer a POST
with either a JSON body or an event stream and does not say which in advance --
both are accepted, and a session id handed out at `initialize` is echoed back on
every later request.

**Native Rust tools**, for anything compiled in. There are none by default: this
agent is not for one job.

Permission is the client's business. The agent does not send
`session/request_permission`; a client gates by tool name, and a client tool it
refuses comes back as a failed call the model can read and work around.

## What the client is told

Everything. Text and reasoning stream as `agent_message_chunk` and
`agent_thought_chunk`; a tool call arrives as `tool_call` carrying `rawInput`,
so the client can show what the agent actually asked for rather than a label;
progress arrives as `tool_call_update` while the tool is still running; context
consumption arrives as `usage_update`, with cache reads in `_meta` because the
protocol has nowhere for the number that explains the cost. A turn boundary, a
mid-session model switch, and a compaction all ride in `_meta`, which the spec
reserves for exactly that.

## Sessions

`session/new` mints an id and the client decides how many to open. Given a
`sessionDir`, conversations are kept as one JSONL file per session and
`loadSession`, `session/resume` and `session/fork` are advertised; without one
they are advertised as absent, because a client told it can resume and then
cannot has been lied to in a way it acts on.

    "sessionDir": "~/.local/share/envoy/sessions"

`session/load` replays the stored conversation as notifications so a client can
draw what was said before it existed. `session/close` puts a session down and
leaves the file; `session/delete` throws it away. Ids carry a timestamp so two
runs cannot both call their first session `s1` and read each other's history.

## Context

Compaction is measured against what the provider reported rather than against
our own arithmetic: the last assistant message carries the real token counts,
and only what follows it is estimated. When the window fills, the oldest whole
turns are cut -- only ever at a user message, so a tool result never loses the
call that produced it -- and, given a `summariser`, replaced by a précis.

    "summariser": { "provider": "ollama-cloud", "model": "gpt-oss:120b", ... }

Without one the turns are dropped outright, which still works. Either way the
client is told: a history that silently got shorter is indistinguishable from a
model that forgot something.

Prompt caching is not a feature so much as a property not to break. Both apis
cache on a stable prefix, so each session gets one `prompt_cache_key` for its
whole life, and compaction -- which rewrites the prefix by definition -- happens
as late as the reserve allows.

## Testing

    cargo test

Everything runs offline. `parley::Canned` answers with prepared events, which is
what a test about loop behaviour wants; `parley::Cassette` replays a recorded
response body through the real parser, which is what a test about wire format
wants. `envoy/tests/protocol.rs` drives the actual server over a pipe, client
tools and cancellation included.

Two examples are diagnostics rather than demos:

    cargo run --example wirecheck            # what our notifications look like on the wire
    cargo run --release --example probe      # stream one turn from the first configured tier
    cargo run --release --example sweep      # try several Ollama Cloud models, report each
    cargo run --example codexcheck           # the Codex credential and the body it would send

`sweep` is the one to reach for when a provider is refusing you: it prints what
each model answered, so a key problem (every model identical) is immediately
distinguishable from a model problem (one of them different).
