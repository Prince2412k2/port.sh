# Portfolio V2

V2 is an isolated client/server workspace. The backend publishes structured content and map data. Direct terminals, hosted SSH/mosh terminals, and the browser render locally using the same client engine.

**Implementation status:** all six sections have an end-to-end client/server path. See [STATUS.md](STATUS.md) for verification and production release gates.

## Crates

- `portfolio-v2-protocol`: bounded, renderer-independent wire types.
- `portfolio-v2-assets`: immutable authored and baked client assets.
- `portfolio-v2-scene`: renderer-neutral visual scene and canonical cell compositor.
- `portfolio-v2-client-core`: deterministic state, layout, semantics, and visual scene.
- `portfolio-v2-native`: direct client and ANSI cell-diff adapter.
- `portfolio-v2-browser`: worker-owned WASM engine, transferable cell frames, WebGPU/WebGL2 instanced terminal glyph rendering, and semantic fallback.
- `portfolio-v2-backend`: content publication and `/api/v2` server.

## Run

From the repository root:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127 --locked
# Debian/Ubuntu: install python3-pil python3-fonttools python3-brotli
bash scripts/build-v2-web.sh
```

Then, from `v2/`:

```sh
cargo run --release -p portfolio-v2-backend
```

Open `http://127.0.0.1:8322/v2/`. The server listens on `0.0.0.0:8322` by default.

The backend reads `../portfolio/data/about.txt` by default. Override the content path with `PORTFOLIO_V2_ABOUT`, the browser distribution with `PORTFOLIO_V2_WEB_DIR`, and the listener with `PORTFOLIO_V2_ADDR`.

## Map data

Set `PORTFOLIO_V2_MAP_DIR` to a directory containing `vector.pmtiles`, `states.tmap`, and `terrain.tmhg`. The backend hashes the data at startup and advertises `/map/v2/<digest>/...` URLs through `/api/v2/map`. Do not modify published files while the process is running; restart after publishing a new revision.

For the supplied map-asset release:

```sh
# Debian/Ubuntu prerequisites: python3-numpy python3-pil
python3 scripts/prepare-v2-map.py /root/map-assets
```

This creates a vector symlink in ignored `map-data/`, converts Natural Earth administrative boundaries, and downloads/caches real Terrarium elevation tiles to prepare the transitional TMHG adapter. Source URLs and SHA-256 digests are recorded in `map-data/sources.json`. The supplied `rdr/terrain.pmtiles` contains shaded imagery, not elevation samples, and is not decoded as elevation.

The backend retains that regional source and publishes small, immutable elevation
tiles at `/map/v2/<revision>/terrain/<z>/<x>/<y>`. Each tile is a 49×49 quantized
grid with a guard band; the catalogue advertises `tmhg-tiles-v1`. Clients acquire
the same tile demand for vectors and elevations and install both together.
They do not download or build a mip pyramid for the regional heightmap. Optional
`buildings.tmap` data is loaded only when useful and uses its actual geographic
coverage.

Published content overrides are `PORTFOLIO_V2_ABOUT`, `PORTFOLIO_V2_PROJECTS`,
`PORTFOLIO_V2_PLACES`, and `PORTFOLIO_V2_TASTE`. Restart the backend after changing
published content; the bootstrap revision covers the complete published data.

## Direct terminal client

```sh
cargo run --manifest-path v2/Cargo.toml --release -p portfolio-v2-native -- \
  --endpoint http://127.0.0.1:8322
```

`PORTFOLIO_V2_ENDPOINT` also sets the endpoint. `--help` lists controls. `1`/`2` switch sections, arrows pan, `+`/`-` zoom, `n`/`b` navigate the tour, `i` changes theme, and `q` exits. Mouse dragging and wheel zoom are supported. Redirecting stdout prints the profile as plain text.

`3` opens Projects, `4` Skills, `5` Taste, and `6` Ask. `/` opens help outside
Ask, `?` opens map search, and Escape returns home. Projects/Taste support
left/right navigation and clickable pips. Skills supports a pointer lift,
drag/throw momentum, and Space to toggle drift. Ask accepts text locally;
Enter submits, Ctrl+X cancels, and `/new` opens a fresh conversation.

Color detection uses `COLORTERM` and `TERM`; override with `PORTFOLIO_V2_COLOR=truecolor|256|16`. Set `PORTFOLIO_V2_ASCII=1` for deterministic ASCII fallback.

The Docker backend serves its matching Linux amd64 executable and checksum at `/downloads/v2/`. Install it with:

```sh
curl -f http://127.0.0.1:8322/downloads/v2/install.sh -o /tmp/install-v2.sh
sh /tmp/install-v2.sh http://127.0.0.1:8322
~/.local/bin/portfolio-v2-native --endpoint http://127.0.0.1:8322
```

Use HTTPS for a public backend. The installer verifies SHA-256 and defaults to `~/.local/bin`; `PORTFOLIO_V2_INSTALL_DIR` overrides this. Update by running the installer again. Uninstall by removing that executable. Other OS/architecture binaries are built by `.github/workflows/v2.yml`; publication/signing is a separate release gate.

## Hosted SSH and mosh

```sh
docker compose -f docker-compose.v2.yml up -d --build
ssh -p 2223 portfolio@localhost
mosh --ssh="ssh -p 2223" portfolio@localhost
```

The Docker web listener is published on `0.0.0.0:8332`; open
`http://<server-address>:8332/v2/`. The backend service itself listens on
`0.0.0.0:8322` inside the shared container network namespace.

The account is `portfolio`, with public, passwordless admission and a forced application command. Arbitrary shell commands, forwarding, SFTP, user startup scripts, and user environment injection are disabled. The hosted container launches the **same executable** copied into the backend's downloads directory. It shares the backend network namespace and connects to `127.0.0.1:8322`; the backend's filesystem/process namespace remains separate.

Hosted clients do not persist visitor state in a shared home directory. The Ask
header shows a resume code; reconnect explicitly with:

```sh
ssh -t -p 2223 portfolio@localhost resume <32-character-code>
portfolio-v2-native --endpoint https://your-backend --session <32-character-code>
```

Direct clients retain their code under `~/.local/state/portfolio-v2/`, or
`PORTFOLIO_V2_STATE_DIR`. `PORTFOLIO_V2_EPHEMERAL=1` disables this persistence.

V2 uses SSH port 2223 and UDP 60100–60110 to coexist with V1. `V2_WEB_BIND`, `V2_SSH_BIND`, `V2_MAP_DIR`, and `V2_VECTOR` configure published addresses and data mounts. The map files are mounted individually so an absolute vector symlink does not become a broken in-container symlink. Host keys persist in the `v2-hostkeys` volume.

## Browser rendering and caching

- The main thread handles DOM/input and GPU submission. Map decode, terrain preparation, simulation, and canonical composition run in `worker.js` and its WASM engine.
- At most one worker presentation is in flight. Inputs are coalesced once per browser frame, and camera simulation catches up when presentation drops frames.
- The renderer uses persistent GPU cell instances and changed-range uploads. It does not expand every glyph into CPU triangle arrays or parse ANSI.
- Each web build publishes a fingerprinted bundle directory. HTML revalidates; scripts, workers, fonts, and WASM resolve within the same immutable bundle to prevent mixed-version cache failures.
- Canonical, CRT, and VHS share the canonical logical surface. Light theme selects Canonical; dark selects CRT. `p` cycles these three packages and `c` toggles monochrome. Saved Ink/Pixel preferences resolve to Canonical.
- The bounded grid scales to fill the browser viewport; pointer coordinates and the Ask editor use the same cell metrics. Linear glyph sampling keeps fractional cell scaling smooth. CRT history settles after a short bounded repaint interval.
- V2 tilted maps cover visible ground rather than an inset display slab, without edge fading; distance fog retains distant detail.
- Browser caches are revision-keyed: 32 MiB encoded RAM cache, 96 MiB persistent Cache Storage budget, and 48 MiB decoded tile cache. Requests are coalesced with two concurrent reads, declared-empty tiles are cached, and errors have retry backoff. Authored tour prefetch is bounded and runs behind visible demand.
- Vector/elevation generations install atomically. The browser initially requests a small coarse parent generation before detailed tiles; incomplete/error generations retain the previous visible generation. Partial arrivals do not trigger identical full-frame composition. Loading status is shown at the edge of the viewport only for a map view. Native output similarly retains only the latest pending frame and computes diffs against the last frame actually written.

## Ask backend

Configure `OLLAMA_API_KEY` and optionally `PORTFOLIO_V2_AI_MODEL` (default
`qwen3.5:397b`) and `PORTFOLIO_V2_AI_URL` (default `https://ollama.com/api/chat`).
Credentials and AI/tool execution remain backend-only. `EXA_API_KEY` enables
bounded web search; `JINA_API_KEY` enables bounded public-page retrieval.

Sessions persist in `PORTFOLIO_V2_SESSION_DIR` (`sessions/` locally, the
`v2-sessions` volume in Docker). REST creates/submits/cancels requests, and
`/api/v2/session?session=<code>` delivers renderer-independent CBOR snapshots
over WebSocket. Reconnection uses a complete snapshot fallback. Reservations
are durable before execution; duplicate IDs never restart work. Interrupted
requests reconcile to a terminal state after a backend restart.

Limits are four concurrent executions, 32 exchanges per conversation, eight
tool calls/four turns per request, 64 KiB answer text, and a persisted daily
request count (default 64; `PORTFOLIO_V2_DAILY_REQUESTS` overrides it). The active
session cache is bounded to 64, with durable inactive sessions loaded on demand.
Missing AI credentials produce a recoverable Ask error. `/reach <message>`
writes a `.message.json` record in the session store without calling the model.

## Checks

```sh
cargo test --manifest-path v2/Cargo.toml --workspace
python3 scripts/test-v2-http.py http://127.0.0.1:8322
node scripts/test-v2-parity.mjs http://127.0.0.1:8322
node scripts/test-v2-browser-engine.mjs http://127.0.0.1:8322
python3 scripts/test-v2-sessions.py v2/target/release/portfolio-v2-backend
python3 scripts/test-v2-pty.py -- v2/target/release/portfolio-v2-native --endpoint http://127.0.0.1:8322
python3 scripts/test-v2-pty.py -- ssh -tt -p 2223 portfolio@localhost
python3 scripts/test-v2-pty.py -- mosh --ssh="ssh -p 2223" portfolio@localhost
```

Pass `--sections` before `--` to exercise every section in the PTY test. The
session test starts an isolated local provider fixture and never calls a paid
provider. `scripts/test-v2-webgpu.mjs` can verify actual GPU targets against a
dedicated Chromium CDP endpoint; it requires a usable WebGPU adapter.

DevTools diagnostics are available at `window.portfolioV2Worker` and `window.portfolioV2RenderMetrics`. Use a performance trace for end-to-end timings; JavaScript submission timings alone do not measure GPU/compositor latency.
