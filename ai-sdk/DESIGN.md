# An agent of our own

The `ask` section currently rents its brain. `acp.rs` starts somebody else's
binary -- opencode, or Copilot -- speaks the Agent Client Protocol to it, and
renders whatever comes back. That works, and it is why `ask` exists at all. But
three things about it are unsatisfying, and all three come from the same place:
the agent is a program we did not write.

- **Tools arrive by the long way round.** ACP gives a client no way to hand an
  agent a function, so `mcp.rs` binds a loopback HTTP listener, mints a token
  per screen, and advertises itself as an MCP server in `session/new`. Fifty-one
  thousand bytes to let the agent say "draw Jaipur" to a panel in the same
  process.
- **Switching models restarts the conversation's host.** `Ask::spawn` walks
  `plan()` and each `attempt()` starts a fresh child process. `Session`
  (`acp.rs:240`) exists for no reason other than to survive that.
- **We cannot see the turn.** Whether the model called a tool, what it decided
  in between, how many turns it took -- all of it is inference from
  `session/update` notifications rather than something we hold.

This document is the plan for replacing the borrowed brain with one of ours: a
library that talks to language models and runs an agent loop, and a binary that
wraps the library in ACP so `acp.rs` barely notices the swap.

---

## What is fixed, and what it costs

The agent stays a **separate process** speaking **real ACP v1**, and it handles
**many sessions per process**.

Separate process is the interesting choice, because in-process would make the
tool problem vanish. It is worth the cost anyway: `--serve` accepts any username
on a public port, so a panic or a runaway allocation inside a tool is a thing
that should take down one visitor's chat rather than the terminal everybody else
is looking at. A process boundary is also the only place a sandbox can later go.

Speaking real ACP rather than something bespoke keeps `acp.rs` almost as-is --
`servers.rs` points at our binary and the handshake, the restraint negotiation,
the gates, and the inbound-request answering all keep working. It also means
this agent can be driven by Zed or any other ACP client, and that we can keep
A/B-ing against opencode through the same client code, which is the only
regression suite we are going to get for free.

Many sessions per process is a capability, not a policy. `session/new` returns
an id and the binary tracks state per id; a client that wants one process per
visitor simply spawns one and opens one session. `portfolio` will probably do
exactly that. Zed will not.

## Which ACP

Two protocols share the acronym and only one of them is this one.

**Agent Client Protocol** is what we are building: JSON-RPC 2.0 over stdio, one
document per line, a client that starts an agent as a child process and opens
sessions on it. It is what `acp.rs` implements and what `opencode acp` serves --
its own help text reads "start ACP (Agent Client Protocol) server", and the
handshake it answers with is unambiguous:

    protocolVersion: 1
    agentCapabilities:
      loadSession: true
      mcpCapabilities:     { http: true, sse: true }
      promptCapabilities:  { embeddedContext: true, image: true }
      sessionCapabilities: { close: {}, fork: {}, list: {}, resume: {} }
    authMethods: [ opencode-login ]

**Agent Communication Protocol** (`agentcommunicationprotocol.dev`) is a
different specification: REST over HTTP, MIME-typed message parts, agent
discovery through metadata embedded in distribution packages. Its own
documentation says it has since been folded into A2A under the Linux Foundation.
Nothing in `acp.rs` could talk to it. If this agent is ever wanted over HTTP the
target is A2A, and the work is a second frontend over the same library rather
than a change to anything below.

Worth knowing: `opencode acp` also takes `--port`, `--hostname`, `--mdns` and
`--cors`, so that server can be reached over a network as well as over stdio.
Stdio is the default and is what our client uses.

Measured against that handshake, v1 advertises:

- `sessionCapabilities.list` and `.close`, which are nearly free once sessions
  are keyed by id.
- Not `.resume`, not `.fork`, not `loadSession` -- all three need a session
  store on disk. Out of scope for v1, and worth revisiting straight after,
  because forking a conversation is a genuinely good thing for a TUI to offer.
- `authMethods: []`. Credentials come from a file an operator wrote; a session
  has nothing to log into.
- `mcpCapabilities.http: false` until the MCP client lands -- and once client
  tools work, our own tools do not need it at all.

## What the client sees

The rule is that the client can reconstruct everything the agent did. ACP v1
turns out to carry almost all of it already, so this is mostly a mapping
exercise rather than a design one.

| what | how |
| --- | --- |
| assistant text, streamed | `agent_message_chunk` |
| reasoning, streamed | `agent_thought_chunk` |
| a tool call, and its arguments | `tool_call` with `rawInput`, `title`, `kind`, `status` |
| a tool making progress | `tool_call_update` with appended `content`, `status: in_progress` |
| a tool finishing | `tool_call_update` with `status: completed` and `rawOutput` |
| context filling up | `usage_update` with `used`, `size`, `cost` |
| the agent's plan | `plan` with `entries[{content, priority, status}]` |
| a conversation title | `session_info_update` |
| interrupt | `session/cancel`, answered with `stopReason: "cancelled"` |
| anything else | `_meta`, which the spec reserves on every one of these |

Three details are worth not rediscovering:

**`ContentChunk` carries a `messageId`,** and all chunks of one message share
it. That is what lets a client group chunks into messages while keeping tool
calls in their arrival position between them -- which is the fix for the
transcript ordering, and it belongs in `ask.rs` rather than here. The protocol
never lost the order; flattening chunks into one text buffer and tools into a
separate list is what loses it.

**`ToolCallContent` has three shapes** -- `content`, `diff`, and `terminal`.
A tool that edits something can hand the client a diff to render rather than
prose about a diff. Nothing in v1 needs it; it is the reason not to model tool
output as a string.

**Tool status is `pending | in_progress | completed | failed`,** and `ToolKind`
is a closed list of ten (`read`, `edit`, `delete`, `move`, `search`, `execute`,
`think`, `fetch`, `switch_mode`, `other`). Refusal is not among them: a tool the
gates shut is `failed` with a reason, which is why `Status::Refused` in `acp.rs`
is a client-side distinction rather than a wire one.

There are two cancellations. `session/cancel` interrupts the turn on a session
and is the one that matters; `$/cancel_request` cancels one in-flight request by
id, which is what `acp.rs` currently sends. The binary handles both. Either way
a cancelled prompt **returns a result** with `stopReason: "cancelled"` rather
than an error, because the turn did happen and part of an answer may already be
on screen.

The honest part: the agent side of this is protocol plumbing, and the work is
mostly at the other end. `acp.rs:788` reads a `title` and a `detail` out of
`tool_call` and discards `rawInput`, `rawOutput`, `kind`, `content` and
`locations`; `usage_update`, `plan` and `session_info_update` are not handled at
all. Emitting all of it is cheap. Showing it is a `portfolio` change.

## Where the tools go

Three kinds, and they are genuinely different.

**Native tools** are Rust functions compiled into the agent. Anything that does
not need the client's screen or filesystem belongs here.

**Client tools** are the reason `mcp.rs` exists. `locate_place` and `show_map`
have to run where the panel is. ACP has no mechanism for this, so we add one:
the client advertises its tools in `_meta` at `initialize` -- name, title,
description, JSON Schema -- and the agent registers them as a tool source whose
implementation is a JSON-RPC request back to the client, on a namespaced method
a conforming implementation will ignore. This is the same direction of travel as
`fs/read_text_file` and `session/request_permission`, which ACP already defines,
so it fits the shape of the protocol rather than fighting it.

That deletes the loopback listener, the per-screen token, the `headers: []`
schema trap, `takes_http_tools()`, and the `<server>-<tool>` renaming guess that
`gates.rs` has to make because Copilot namespaces MCP tools. What survives from
`mcp.rs` is the gazetteer lookup itself, which becomes a client tool body.

**MCP tools** are third-party servers -- stdio and streamable HTTP -- exposed
through the same `ToolSource` seam. Not needed for `ask`, needed the moment this
agent is used for anything else.

Permission is separate from implementation and already solved: the agent sends
`session/request_permission` before a call and the client answers. `gates.rs`
keeps its default-deny table and keeps being the only place that decides. The
agent enforces its own budget independently, because a client that never refuses
anything should still not be able to make the agent loop forever.

## Layout

Two crates. A third for shared types would only be depended on by both of the
other two, which is not a reason.

    ai-sdk/
      parley/        the model layer: wire protocols, auth, provider catalog
      envoy/         the agent: loop, tools, policy, sessions
                     lib + bin -- the binary is a thin ACP shell over the lib

Names are provisional. `parley` is a conversation held with the other side;
`envoy` acts on your behalf and carries messages both ways. (`envoy` collides
with a well-known proxy, which matters only if this is ever published.)

The library/binary split is for testing more than reuse. Loop semantics get
tested as Rust calls against recorded responses; the protocol tests then only
have to cover framing and dispatch, which is a much smaller surface than
driving a subprocess through stdio for every assertion about tool ordering.

**stdout carries protocol and nothing else.** One stray `println!`, or one
dependency that logs to stdout, corrupts the stream and presents as a parse
error in the client. Logs go to stderr, and stdout gets wrapped in a type
nothing else can reach.

## The model layer

The distinction worth stealing from pi is between an **api** and a
**provider**. An api is a wire protocol -- how a request is framed and how the
server-sent events are shaped. A provider is auth, a base URL, and a catalogue
of models, pointing at one api. pi supports around forty providers on eight
apis, because providers are mostly data.

For v1 that is **two apis** and **three providers**:

| api | providers |
| --- | --- |
| `openai-completions` | Ollama Cloud, any OpenAI-compatible endpoint |
| `openai-responses` | OpenAI Codex (ChatGPT OAuth) |

Ollama Cloud is OpenAI-compatible over a bearer key, so it costs a base URL and
a catalogue entry rather than an integration.

### Messages

A message holds an **ordered** list of blocks, and that order is the whole
point: `Text`, `ToolCall`, `Text` in one assistant message is the model
narrating what it is about to do, doing it, and then reading the result. Nothing
downstream may sort or group them.

    Block = Text | Thinking | ToolCall | Image

`Thinking` and the assistant message both carry an opaque `provider_data`.
Codex returns reasoning as encrypted content that must be handed back verbatim
on the next request or the call is rejected; there is nowhere else to put it,
and retrofitting the field later means rewriting every stored conversation.

Stream events carry a block index so a consumer can place a delta without
inferring which block it belongs to.

### Falling back between models

`health.rs::plan()` already returns an ordered list of things to try. Today
falling through it respawns a child process. Instead, a `Fallback` type wraps
the tiers and implements the same trait a single provider does, so the agent
cannot tell the difference and the conversation never leaves memory.

This needs error classification to be worth anything -- retryable (429, 5xx,
transport, timeout) against fatal (400, 401, schema rejection). Falling back on
a fatal error means every one of our own bugs looks like the model's fault and
gets silently retried against three providers.

One consequence for the client: `Ready.tier` is settled at handshake, so a
mid-session switch has to be announced or the screen keeps naming a model that
stopped answering.

### Auth

Login is an operator's job, not a visitor's. A localhost OAuth callback means
nothing to somebody reached over SSH, so the browser and device-code flows live
in a separate operator-side path and are never reachable from a session. The
library only **reads and refreshes** credentials from a file store -- refresh
has to be in-process because access tokens expire mid-conversation.

For Codex that is PKCE against `auth.openai.com`, a refresh grant, and an
account id decoded from a JWT claim and sent as a header. The constants are in
pi's `auth/oauth/openai-codex.ts`.

## The loop

One turn is one model call plus the tool calls it asked for:

1. Stream an assistant message; emit blocks as they arrive.
2. Collect its tool calls. If there are none, the turn ends the conversation.
3. Per call: validate arguments against the schema, ask permission, execute.
4. Append results as messages.
5. Charge the budget. Check whether anything was queued while we worked.
6. Go to 1.

Parallel execution by default, with a per-tool sequential override; if any tool
in a batch is sequential the whole batch is, which is less clever than
interleaving and much easier to reason about when something goes wrong.

**Budgets belong here, not in a hook.** `gates.rs` caps a session at twelve
turns and twenty-four tool calls because this is a public port. A hook can be
left unregistered; a field on the loop config cannot. Exhausting one ends the
turn with ACP's own `max_turn_requests` stop reason rather than an error.

Steering (inject a message into the running turn) and follow-up (queue one for
after) are the two queues pi has, and they are what make a TUI feel live rather
than transactional.

## Context and caching

These two are one subject, because they fight.

**Caching.** Neither of our v1 apis takes explicit cache breakpoints the way
Anthropic does -- OpenAI-shaped endpoints cache automatically on a stable
prefix, helped along by a cache key we should pass so repeated sessions route to
the same place. So caching is not a feature to build, it is a property to avoid
breaking: anything that rewrites the front of the conversation throws it away.

**Compaction.** pi's rule is worth copying because it is measured rather than
guessed. Ground truth is the `usage` reported by the last assistant message,
plus an estimate for the messages after it -- not an estimate of everything,
which drifts. Compact when

    tokens > contextWindow - reserveTokens          reserve 16k

by summarising the oldest part and keeping roughly the most recent 20k tokens.
The subtle part is where to cut: never between a tool call and its result, and
never mid-turn. pi has `findValidCutPoints` and `findTurnStartIndex` for exactly
this, and a wrong cut produces a conversation the provider rejects rather than
one that merely reads badly.

Because compaction rewrites the prefix, it invalidates the cache by definition.
So: compact as late as the reserve allows, never trim the middle to save a
little, and treat every compaction as a cost worth reporting -- `usage_update`
before and after, and a note in `_meta` that it happened. A conversation that
silently got shorter is indistinguishable from a model that forgot something.

One gap: `usage_update` has `used`, `size` and `cost`, but nowhere for cache
reads. Cache hit ratio is the most useful single number for what a session
costs, so it goes in `_meta`.

## Testing, given the network

Only GitHub is reachable from the host; every model provider is blocked and
works solely in the container. So a **cassette** api -- replaying recorded
server-sent events from a file, with optional pacing -- is not a testing
convenience, it is the only way to develop this where the editor is. A recording
wrapper captures real traffic in the container; everything else runs offline and
deterministically.

## Dependencies

The registry is offline: a crate can only be added if its `.crate` is already
cached, pinned to a cached version. The intended stack resolves and compiles
here in under ten seconds. Four things worth knowing before trying:

- `reqwest`'s `rustls` feature pulls `quinn`, which is not cached, and fails.
  Use `default-features = false, features = ["rustls-no-provider", "stream",
  "json", "http2"]` with an explicit `rustls` on `ring`.
- `schemars` 1.2.1 is cached but `schemars_derive` 1.x is not. Only 0.8.22 has a
  matching derive, which is one reason tool schemas are written by hand.
- Not cached at all: `eventsource-stream`, `oauth2`, `tiny_http`, `webbrowser`.
  So server-sent-event framing, the OAuth loopback listener and opening a
  browser are ours. None is large; SSE framing is about eighty lines.
- `serde` **is** cached. `SESSION.md` says otherwise and is out of date --
  `json.rs` predates the cache it describes. The agent uses `serde_json`;
  `portfolio` can keep `json.rs`, since two processes are under no obligation to
  agree about how they parse.

Tool schemas are hand-written JSON Schema in `serde_json::Value`, with arguments
deserialised into a derived struct. Providers in strict mode reject `$ref` and
`definitions` and want `additionalProperties: false` with a complete `required`
array; writing the schema is less work than post-processing a generated one into
that shape.

## Built so far

Everything below is in the tree with tests, and the numbers are `cargo test`.

| | what it is | tests |
| --- | --- | --- |
| `parley` types, accumulator, SSE, errors | the normalised middle | |
| `parley::api::openai_completions` | Ollama Cloud and every compatible endpoint | |
| `parley::api::openai_responses` | reasoning models, and the Codex shape | |
| `parley::fallback` | tiering as a wire, so the loop cannot tell | |
| `parley::auth` | env keys and the Codex credential file | |
| `parley::canned` | prepared events, and recorded bodies replayed | |
| `envoy::agent` | the turn loop, budgets, parallel and sequential tools | |
| `envoy::compact` | cut points that never split a turn | |
| `envoy::acp` | JSON-RPC over stdio, sessions, the event mapping | |
| `envoy::client_tool` | the `_meta` extension | |
| `envoy::mcp` | stdio MCP client | |
| `envoy` binary | reads a config, serves a client | |

Since then: per-tier tuning (`effort`, `temperature`, `maxOutput`, which
`models.txt` already spells per tier), a rate-limit wait before falling through
a tier, one `prompt_cache_key` per session, summarising compaction, MCP over
streamable HTTP behind a `Transport` trait, and session persistence with
`load`/`resume`/`fork`.

Four decisions changed once the network opened up, all for the better:

- The ACP types are a **crates.io dependency**, not copied code. The plan to take
  a subset out of a clone existed only because the crate was unreachable.
- `reqwest` uses its ordinary `rustls` feature; the `rustls-no-provider`
  workaround for an absent `quinn` is gone.
- Compaction summarises what it cuts when a `summariser` is configured, and
  drops it otherwise. A failed summary drops rather than failing the turn:
  housekeeping should not cost an answer.
- MCP has **both transports**. `Transport` is the seam; everything above it --
  handshake, tool list, tool wrapper -- is shared.

Three bugs worth remembering, each now a test named after the failure:

- **A tool's progress could arrive after its result.** A tool that reports and
  returns without ever awaiting completes inside one poll, so the finish was
  ready before the channel had been read.
- **Two prompts on one session would corrupt its history.** Requests are handled
  off the read loop so a cancel can be read mid-turn; the price is that a second
  prompt has to be refused rather than interleaved.
- **A failed turn was invisible.** The deltas never arrive when a call is
  rejected, so a client saw `end_turn` with no content and no reason -- a 401
  looked like a model with nothing to add. Found by pointing it at a real
  endpoint, which is the argument for doing that early.

Not done, and each for a stated reason:

- **A live Codex call.** Everything up to the socket is verified against the real
  credential file -- the account id is extracted, the headers are the ones the
  backend wants, and a reasoning item replays byte-exact. The access token
  expired 26 hours ago, and refreshing rotates the refresh token: without
  writing the new one back to `~/.codex/auth.json`, the Codex CLI's own login
  would break. That file is outside this directory.
- **A successful Ollama completion.** `ollama.com` is reachable and the wire
  produces a correct `auth: Unauthorized` for the key in `.env`; every scheme
  was tried (bearer, raw, `X-Api-Key`, an alternate host) and all return 401.
- **Any change to `portfolio`.** The client is now sent far more than
  `acp.rs:788` reads, and teaching it is work outside this directory.

## Order of work

1. Types, message accumulation, SSE framing, cassette api. Entirely offline.
2. `openai-completions` against Ollama Cloud. Record cassettes in the container.
3. The loop: native tools, permission, budgets, queues. Tested on cassettes.
4. The ACP binary and session handling. Point `servers.rs` at it and diff
   against opencode on the same prompts.
5. Client tools over the protocol extension. `mcp.rs` loses its listener.
6. `openai-responses` and Codex OAuth.
7. MCP client.

Codex is last on purpose. It combines the two hardest problems -- an OAuth token
lifecycle and reasoning items that must round-trip exactly -- with a wire format
that cannot be exercised from the host at all. Ollama Cloud proves the
architecture on a bearer token and no reasoning state, and by the time Codex is
wired the only new thing about it is Codex.
