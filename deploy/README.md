# Running it

```bash
docker compose up -d --build
```

Two ways in, one program:

```bash
ssh -p 2222 your-host          # terminal-native
open http://127.0.0.1:8080     # the same thing, in a browser
```

That address is loopback on purpose. The web service speaks plain HTTP, so
the only thing that should be able to reach it is whatever is terminating TLS
in front of it — see **Putting it on the internet** below. SSH is published
to the world, because there is no proxying an ssh session through a web
server.

No username needed for ssh. There is no OS account to name — the binary
speaks SSH itself, so a login name is just a string in the handshake, and any
one (or none at all) is accepted.

Two containers from one image, started with different flags. Separate on
purpose: a crash or restart on one transport does not take the other down,
and each gets its own resource ceiling.

## Putting it on the internet

The box needs Docker, a domain pointed at it, and something terminating TLS.
It does not need a toolchain: the image builds itself.

```bash
git clone <this> /srv/terminal && cd /srv/terminal
printf 'OLLAMA_API_KEY=...\nEXA_API_KEY=...\nJINA_API_KEY=...\n' > .env
chmod 600 .env
PMTILES=/srv/tiles/vector.pmtiles docker compose up -d --build
```

Put `PMTILES` in the `.env` too and every later `docker compose up -d` picks
it up on its own.

### In front of it

Only the web side is proxied. Caddy needs one block and no WebSocket
configuration — it upgrades them itself, and it sets `X-Forwarded-For`, which
is where the visitor log and the per-address limits get the address from:

```caddy
prince.dev {
        reverse_proxy 127.0.0.1:8080
}
```

If your Caddy is itself in Docker, do not publish the port at all. Put both on
one network and point it at the service:

```yaml
# docker-compose.override.yml
services:
  web:
    ports: !reset []
    networks: [caddy]
networks:
  caddy:
    external: true
```

```caddy
prince.dev {
        reverse_proxy terminal-web-1:8080
}
```

Either way the page notices it arrived over TLS and opens a `wss://` socket
rather than a `ws://` one. There is nothing to configure for that.

SSH is not proxied and cannot be. Open 2222 in the firewall, or move it with
`SSH_BIND=0.0.0.0:22` if nothing else on the box wants 22.

### Going live

In order, because two of these are one-way:

1. **`docker compose logs portfolio | grep ssh_host_key`.** Record the
   fingerprint and publish the `SSHFP` line beside it. Do this before anybody
   connects: the alternative is asking people who already trusted a key to
   trust a different one. Never delete the `hostkey` volume.
2. **`docker compose exec web /usr/local/bin/portfolio --probe`.** It prints
   which agent tier answered. `no agent tier answered` means the ask section
   will tell visitors so, politely, and everything else works — see **The chat
   section** for logging the agent in.
3. **Check the limits.** `--probe` prints them too. The defaults cap a day at
   $10 of model spend and 24 visits an address; both are `.env` settings and
   both matter the moment the URL is somewhere public.
4. **`curl -sI https://prince.dev`** and then open it. Then
   `ssh -p 2222 prince.dev`. Both paths run the same program, and either can
   be broken while the other is fine.

### Updating

```bash
git pull && docker compose up -d --build
```

The data, the host key, the messages and the agent's login are all volumes or
binds, so none of them are touched. Text in `about.txt`, `taste.txt` and
`places.txt` needs no build at all — see below.

### One thing still outside

The browser fetches xterm from `cdn.jsdelivr.net`, which is the only thing on
the page that comes from anywhere but this box. If it does not arrive the page
says so and points at the ssh line rather than showing a black rectangle, but
that is a fallback and not a fix. Two ways to close it, whenever it is worth
doing: pin what is fetched, which is a one-time job per version --

```bash
for f in xterm@5.3.0/lib/xterm.js xterm-addon-fit@0.8.0/lib/addon-fit.js; do
  curl -sL "https://cdn.jsdelivr.net/npm/$f" |
    openssl dgst -sha384 -binary | openssl base64 -A | sed "s|^|$f sha384-|"
done
```

-- and put each hash in an `integrity=` on its tag, so a compromised CDN is a
page that does not load rather than a page that runs somebody else's code. Or
vendor the four files into the image and serve them from here, which ends the
question but means tracking their releases by hand.

## The web terminal

A browser opens a WebSocket, gets its own session, and renders the ANSI
frames with xterm.js. **It runs the same session code the SSH path does** —
see `session::run`, which neither transport appears in — so the two cannot
drift apart as the app changes.

There is no pty and no subprocess. The far end of that socket is a `Shell`
struct drawing frames into a buffer, nothing more:

| | |
|---|---|
| shell access | none — there is no shell to escape from, no process to exec |
| what the server accepts | key and mouse bytes, via the same `wire::Decoder` SSH uses |
| resize | an out-of-band `r<cols>x<rows>` text message, kept off the input path |
| per visitor | one `Shell` on one thread, not one OS process |

That last row matters at scale. The obvious way to build this (a real pty
plus a subprocess per browser tab, the ttyd pattern) gives every visitor
their own full copy of the basemap and terrain. This design does not: one
process serves everyone, so a memory optimisation benefits every visitor at
once rather than being multiplied by them.

### The shaders

xterm.js renders into a `<canvas>`, and the `crt` switch in the corner takes
that canvas as a texture and runs it through four programs: a composite
signal, a phosphor that keeps a decaying copy of the last frame, a pair of
blurs for bloom and halation, and then the tube itself — glass curvature, a
beam that widens as it brightens, a shadow mask or an aperture grille or an
LCD subpixel grid, convergence error, a hum bar, and the rim of the bezel.
The switch beside it picks between seven screens.

None of it touches the server: the same bytes go over the wire either way,
and the whole effect is the visitor's GPU rewriting frames it has already
been given. It repaints when the terminal repaints and for as long as it has
something still moving to show, then stops — an idle tab does no work unless
the visitor has gone and chosen a screen that never settles.

WebGL is required for the switch and for xterm's fast path; without it the
switch disables itself and everything else carries on. Turning the tube on
also swaps xterm to its canvas renderer, because a WebGL drawing buffer is
not reliably readable as a texture from another context.

Published on **2222** rather than 22, so this never has to fight whatever the
host's own sshd is doing. `SSH_BIND=0.0.0.0:22` moves it once nothing else on
the box wants that port.

## Why there is no OpenSSH here

The first version of this ran behind a real sshd with `ForceCommand`, and it
worked right up until the goal became a bare `ssh <domain>` with no username
at all. OpenSSH looks up the requested account in the OS's user database
*before* it will run any authentication — `AuthorizedKeysCommand` included —
and there is no wildcard for that short of writing a custom NSS module.

So the binary now speaks SSH directly, using the same [`russh`](https://
github.com/Eugeny/russh) crate an adjacent project (`harbr`) already uses to
solve the identical problem: a ratatui TUI served to many SSH sessions at
once. A login is accepted regardless of username or key — there is nothing
behind it worth gating, and gating a public CV only turns away the people it
exists for.

That also shrinks the container to almost nothing. No sshd, no PAM, no NSS,
no privilege-separation directory, no login account for a stranger to somehow
reach. The image runs one already-unprivileged process, with:

| | |
|---|---|
| filesystem | read-only root with a bounded scratch `tmpfs` and explicit data volumes |
| web transport | no pty, no subprocess, no shell — see above |
| capabilities | all dropped, none re-added |
| account | a system user with `nologin`, never used to log in |
| password / forwarding / sftp | never implemented — there is no code path for them |

Every session gets its own OS thread and its own single-threaded async
runtime (see the comment in `portfolio/src/net.rs` for why), so one visitor's
map tile decoding cannot stall anyone else's session, and a `Shell` never has
to be made `Send` to satisfy a shared executor.

## Fingerprint

The host key is generated once, on first start, into the `hostkey` volume,
and reused after that — do not delete that volume unless you mean to; a new
key shows every returning visitor a changed-fingerprint warning.

Its fingerprint is printed to the log on **every** start, not only when it is
generated, because there is no `ssh-keygen` in this image to compute it after
the fact:

```bash
docker compose logs portfolio | grep ssh_host_key
```

The next log line prints the DNS record payload:

```text
SSHFP 4 2 <sha256-hex>
```

Publish it as an `SSHFP` record on the SSH hostname. The owner is the hostname,
not the port. DNSSEC is required for clients to authenticate this record rather
than merely display it. Verify it with `dig SSHFP <host>` and OpenSSH's
`VerifyHostKeyDNS yes`.

## Session limits

A stranger's session should not be able to take the box down or run forever:

| | |
|---|---|
| concurrent sessions | 128 (`MAX_SESSIONS` in `net.rs`) |
| concurrent SSH connections | 192 globally, 4 per address |
| idle timeout | 15 minutes with no keystroke (`PORTFOLIO_IDLE_SECS`, `0` disables it) |
| maximum session | 1 hour (`PORTFOLIO_MAX_SESSION_SECS`, `0` disables it) |
| daily visits | 24 per key/id and address (`PORTFOLIO_DAILY_VISITS`) |
| daily AI allocation | $10, reserving $0.25 per provider attempt (`PORTFOLIO_DAILY_AI_USD`, `PORTFOLIO_AI_REQUEST_USD`) |
| container | 1.5 CPUs, 512 MB, 256 pids (`docker-compose.yml`) |

## The map data

The archives are not in the image. The India basemap is 1.6 GB, and both it
and the heightmap are rebuildable from `map/scripts/`. The compose file
bind-mounts them:

| file | what | without it |
|---|---|---|
| `india.pmtiles` | the basemap | the map section says "no basemap mounted" |
| `india.tmhg` | terrain | flat ground, no relief |
| `states.tmap` | state borders and names | search finds no states |

`india.pmtiles` is the big one and the compose file takes its path from
`$PMTILES`, so a box that already has that archive for something else does
not need a second copy:

```bash
PMTILES=/srv/tiles/vector.pmtiles docker compose up -d
```

The other two are small enough to live in the checkout. If yours do not, edit
the left-hand side of those lines. The container reads all of them through
`TERMAP_DATA`.

## Editing the text without rebuilding

`about.txt`, `taste.txt` and `places.txt` are read from disk before the copies
built into the binary, and the compose file mounts all three. Edit, then
`docker compose restart portfolio`. No rebuild, no recompile.

## The chat section

`ask` runs `opencode acp` inside the container. The binary is in the image; what
it needs from you is a provider key. Put one in a `.env` file beside
`docker-compose.yml`:

```
OPENCODE_API_KEY=...
```

With no key the section reports that the agent would not start and every other
section carries on working. That is the intended failure: a missing key should
cost one tab, not the site.

**Which tier.** `portfolio/data/models.txt` holds tiers, not a flat list, and
they are tried in order:

1. **github copilot** — a seat that is already paid for.
2. **opencode zen** — free, and free means a daily allowance rather than a
   guarantee.
3. **ollama cloud** — the backstop, slower and still an answer.

A background check runs once an hour and settles which tier sessions start on,
so nobody has to discover a dead one mid-question. Within the chosen tier the
lazy fallback still applies: a model that fails is dropped and the next is
asked the same question. Both layers exist because a tier can go down between
checks and the visitor should not pay for that either.

The check is one real one-word question to one model — the only thing that
distinguishes *configured* from *answering*, since a listed model can be out of
quota, unauthenticated, or withdrawn and all of those look fine until you ask.
The walk stops at the first tier that answers, so a normal hour costs a single
`ping`. Change the interval with `PORTFOLIO_PROBE_SECS`.

To see the current state, without waiting an hour:

```bash
docker compose exec portfolio portfolio --probe
```

That prints the tiers, runs one check, and says which one it would use. It is
the same code the server runs on its timer.

**Logging Copilot in.** The Copilot tier runs Copilot's own ACP server
(`copilot --acp`) rather than opencode's Copilot provider, so it authenticates
Copilot's way, not opencode's. It will not open a session until it has: it
answers `session/new` with `Authentication required` and advertises
`copilot-login` in the handshake. Two ways in, and either is enough.

A token, which needs no login step and no interactive terminal:

```bash
# .env beside docker-compose.yml — compose reads it, git does not get it
GH_TOKEN=github_pat_...
```

A fine-grained PAT with the **Copilot Requests** permission. `GH_TOKEN` wins
over `GITHUB_TOKEN` if both are set.

Or interactively, once:

```bash
docker compose exec -it portfolio copilot login
```

That credential lands in the `agent` volume via `COPILOT_HOME=/app/agent/copilot`,
and both services share it. Keep that volume, and keep `COPILOT_HOME` pointed
into it: Copilot defaults its config directory to `~/.copilot`, and `HOME` here
is the tmpfs — where a login works perfectly until the first restart and then
silently drops the whole tier. The opencode credential learned this the same way.

To check which way it went, and whether it took:

```bash
docker compose exec -T portfolio portfolio --probe
```

**Copilot unpacks itself, and needs somewhere real to do it.** `copilot` is a
Node single-executable: on first run it extracts ~209 MB to
`$XDG_CACHE_HOME/copilot/pkg/…`. The tmpfs fails that twice over — 64 MB is far
too small, and even given room the tmpfs is `noexec`, so loading the native
`runtime.node` fails with `failed to map segment from shared object`. Raising
the tmpfs size fixes only the first half, and costs RAM against a 512 MB limit.

So `XDG_CACHE_HOME` is on the `agent` volume. If you ever move it back to
scratch, Copilot fails at spawn with a wall of
`TAR_ENTRY_ERROR(ENOSPC): no space left on device` — which reads like a full
disk and is not one. Check the *mount*, not `df` on the host.

The unpacking happens once per volume, not per restart. The very first run pays
for it, so a probe against a freshly created volume can time out and report the
tier down; the next one will not.

**Our own agent is the first tier.** `envoy`, from `ai-sdk/` in this
repository: it speaks the same ACP over stdio as the others, and the turn loop,
the tool calls and the compaction are all code somebody here can fix. It is
built into the image by its own `cargo build` -- a separate workspace, so a
broken experiment in there cannot stop the portfolio compiling.

Two things about it are unlike the other tiers, and both are lines in
`models.txt`:

- `pin flag --model` -- it takes the model as a flag and refuses to start when
  no tier in its own catalogue matches, which is a loud failure rather than a
  quiet fall back to something else.
- `tools env ENVOY_MCP_HTTP` -- it does not advertise `mcpCapabilities.http`, so
  it cannot be handed our tool server in `session/new` the way opencode and
  Copilot are. It reads the address out of that variable instead. **Without that
  line the map and web tools do not exist on this tier and nothing says so** --
  the agent simply never mentions a map.

Its model catalogue is `ai-sdk/envoy.json`, mounted at `/app/data/envoy.json`
and pointed at by `ENVOY_CONFIG`. That file is the only place endpoints, context
windows and credentials live; `models.txt` says which tiers to try and in what
order. Two catalogues that can disagree about which models exist would be worse
than one in a second file.

Its credential is the **opencode login already on the volume** -- the catalogue
names it `$XDG_DATA_HOME/opencode/auth.json`, which resolves to
`/app/agent/opencode/auth.json` here and to `~/.local/share/opencode/auth.json`
on a laptop. So `opencode auth login` (pick openai) is the one step, and it
serves both this tier and the one below it. Nothing needs to go in `.env`, and
the tier declares `secrets none`: a key it does not use is a key it should not
see.

**It keeps no transcripts.** Its catalogue leaves `sessionDir` unset, so
`loadSession`, `resume` and `fork` are advertised as absent -- honestly, which is
the point: with a store configured, every anonymous visitor's conversation would
be written into one directory in the container, and its `session/list` would
enumerate all of them to whoever asked next. A directory per visitor would be the
fix, and until there is one the answer is not to keep them. Visits are still
logged, as they always were -- see *Who came*.

To check it end to end rather than by reading:

```bash
portfolio --probe                 # says which tier is answering, and via what
portfolio --tools                 # prints a tool server URL and names every call
```

`--tools` is the one that answers "can that agent actually use them". Point an
agent at the URL it prints; every call arrives named, with its arguments.

**Keys for the other tiers.** Everything below Copilot is reached through
opencode, which takes credentials two ways — an environment variable, or its own
login. Either is enough.

| tier | provider | variable |
|---|---|---|
| opencode zen | `opencode` | `OPENCODE_API_KEY` |
| ollama cloud | `ollama-cloud` | `OLLAMA_API_KEY` |

Put them in a `.env` beside `docker-compose.yml` — compose reads it on its own,
and it is not a file to commit:

```bash
OLLAMA_API_KEY=...
OPENCODE_API_KEY=...
EXA_API_KEY=...          # search_web, below
JINA_API_KEY=...         # fetch_page, below
```

Or log in instead, which writes to `auth.json` on the `agent` volume and so
survives a restart:

```bash
docker compose exec -it portfolio opencode auth login -p ollama-cloud
docker compose exec -T  portfolio opencode auth list      # what it has
```

**`ollama-cloud`, not `ollama`.** There is no plain `ollama` provider in the
registry opencode reads: the local daemon and the hosted service are one name in
Ollama's own tooling and two different things here. A key from ollama.com goes to
`https://ollama.com/v1` directly, which is what `ollama-cloud` talks to. The
`-cloud` suffix on model ids (`gpt-oss:120b-cloud`) belongs to the *local*
daemon proxying hosted models, and means nothing here — this image has no daemon.
Use the bare id, `ollama-cloud/gpt-oss:120b`.

opencode still serves the zen and ollama tiers underneath, and still has its own
login (`opencode auth login <provider>`) if you ever point a tier at a provider
that needs one.

**Its own web tools.** The section used to be able to look something up only
when the answering server happened to bring web tools of its own: Copilot's seat
does, most of the free models do not. So two of them are ours now, served from
this process like the map tools — `search_web` (Exa) and `fetch_page` (Jina's
reader) — and every tier has them.

| what | variable | without it |
|---|---|---|
| search | `EXA_API_KEY` | `search_web` is not offered at all |
| reading a page | `JINA_API_KEY` | `fetch_page` is not offered at all |

Both go in the same `.env`. Neither is handed to an agent: no tier declares them,
so `spawn_command` strips them from every agent's environment, and the only thing
that spends them is this binary. **A search costs about seven tenths of a cent,**
and the box takes any username, so the ceiling is compiled in with the rest of
the policy — `gates.rs: web_calls`, twelve a conversation, counted per session
and reported back to the agent in every reply so it can spend them deliberately.
An empty variable counts as absent, which is what compose passes when the host
has not set one.

`portfolio --probe` prints whether each key is present, without printing it.

**What it may do.** One table, `portfolio/src/gates.rs`, and everything else
derives from it: the `clientCapabilities` in the ACP handshake, the server's own
`tools` and `permission` blocks where it has them, and the check on every inbound
request. It can fetch and search the web. It cannot run a shell, and that is not
a close call: this box takes any username over SSH, so `bash` on it is arbitrary
code execution for anyone who can type. Enforced three times over, because the
first two layers are somebody else's code and one upstream rename from meaning
nothing.

A shut gate answers with a JSON-RPC error, not an empty result. Answering
`terminal/create` with `{}` would tell the agent it has a terminal, and it would
then read from it.

`portfolio --probe` prints the gates the running binary was built with, along
with which tier is answering.

**Why compile time.** These gates decide whether a public server runs a shell for
a stranger. A file on disk is one bad mount from being absent or empty — see the
dangling-symlink incident — so turning one on is a rebuild and a redeploy. That
is the right amount of friction for the question.

**What it costs you.** Strangers' questions spend your tokens, and their
searches spend real money. Three brakes, all in `gates.rs`: `turns` (twelve
questions a connection), `tool_calls` (twenty-four a session) and `web_calls`
(twelve searches or page reads a session). None of them stops a reconnect. If this gets found by something automated,
the lever is `models.txt` — empty it and the section turns itself off.

**Which server.** Any ACP server, not just opencode. A tier in `models.txt` may
name a `command` and how the model is pinned; left out, it is `opencode acp`. The
gates apply to all of them, so adding a server cannot widen what an agent may do.

## Who came

Visits are logged, so there is an answer to the question a portfolio exists to
ask: is anybody looking, and from where. Append-only JSONL beside the messages,
on the same volume:

```bash
docker compose exec portfolio cat /app/messages/visits.jsonl | jq
```

Four kinds of line, all keyed by `session`:

| event | what |
|---|---|
| `arrive` | transport, username, identity, address, client, and whether they have been here before |
| `where` | city, region, country, lat/lon — appended when the lookup returns |
| `ask` | one exchange, question and answer |
| `leave` | how long they stayed and how many questions they asked |

**What identifies somebody.** Over SSH, two things arrive as part of logging in
and neither is taken behind anyone's back: the username they typed in front of
the `@`, and the fingerprint of the key they authenticated with. The fingerprint
is what makes a return visit recognisable, because it is stable across addresses.
In a browser there is no equivalent, so the page keeps a random id in
`localStorage` and sends it — clearing site data makes somebody a new visitor,
which is the right amount of control to leave with them.

**Where** reuses the geolocation the map already had (`ip-api.com`, plain HTTP),
so this codebase has one such call and not two. It runs on its own thread and is
appended when it returns; nothing waits on it. Private addresses are never sent.

A useful thing to run:

```bash
# everyone who has been here more than once
jq -r 'select(.event=="arrive" and .returning) | "\(.user) \(.id)"' visits.jsonl | sort | uniq -c | sort -rn
```

One thing to decide rather than inherit: nothing on screen tells visitors this is
kept. That is a one-line change to `OPENING` in `ask.rs` if you want it, and in
some jurisdictions it is the difference between analytics and a problem. Your
call, not mine — but it is easier to add now than to explain later.

## Messages

`/reach <message>` in the ask section appends a line of JSON to a volume. It
never touches the agent: a message meant for a person should arrive whether or
not a model is up, and word for word rather than as something's summary.

```bash
docker compose exec portfolio cat /app/messages/messages.jsonl
```

Both services mount the same volume, so it does not matter whether somebody
came in over SSH or the browser.

## Checking it without a terminal

Piped, the binary prints a plain-text CV instead of raw-mode escapes, which is
what the healthcheck uses:

```bash
ssh -p 2222 your-host | cat        # plain text
ssh -p 2222 -t your-host           # force the interactive one
```

## Debugging inside the container

There is no shell-based login path any more, but `docker exec` still works —
it runs a command directly in the container's namespace rather than going
through any login mechanism, so the `nologin` shell on the app's own account
does not get in the way:

```bash
docker compose exec portfolio sh
```
