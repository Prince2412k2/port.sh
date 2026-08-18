---
### 1. netjail — `github.com/Prince2412k2/netjail`
**Run any command with only its network sandboxed.**

A Linux sandbox that isolates what a process can reach on the network while leaving everything else untouched — same filesystem, same `$HOME`, same binaries, same user. No container, no VM, no volume mounts, no copied files. Enforcement is a real network namespace with a veth pair and nftables default-drop, plus a local filtering proxy that can allow or refuse **individual HTTPS paths**, not just whole hosts. Includes a DNS proxy, CA injection into the system trust path, and blocking of the host daemon sockets that would otherwise bypass the namespace.

Go · 16,700 LOC · 28 packages · 6,100 lines of tests (37%) · design doc with threat model, 16k-word manual, installer, systemd units

---

### 2. watch-party — *private*

**Self-hosted synchronized movie nights, frame-accurate across devices.**

A watch-party platform over a Jellyfin media library: a web client and a native Flutter desktop app (macOS/Windows/Linux) stay in sync to within ~120 ms p95 while playing the same title, with LiveKit voice chat and a Servarr integration (Sonarr/Radarr/qBittorrent/TMDB) for pulling in titles that aren't in the library yet.

The interesting part is the sync engine. One pure decision core is mirrored in JavaScript and Dart and held equivalent by generated JSON conformance vectors run in both CIs. On top of it: a monotonic clock with outlier-rejected, uncertainty-weighted offset estimation and a Theil–Sen skew term; a PI rate controller on a certainty-gated error so it can't chase noise it can't measure; cost-modeled seeks executed as aim-ahead rendezvous plans to avoid HLS chase loops; and a graduated stall ladder instead of a binary freeze. Drift and rate-flutter targets are stated as SLOs and asserted by a scenario test harness.

Dart + JavaScript/TypeScript · ~115k LOC · 471 commits · 152 test files · scenario harness with 11 suites

---

### 3. logify + coolify_logs — `github.com/Prince2412k2/logify` · `github.com/Prince2412k2/coolify_logs`

**Live container logs from a Coolify host, in your terminal.**

Two halves of one tool. **coolify_logs** is the gateway that runs on the Docker host: a FastAPI service exposing REST and WebSocket endpoints for live container logs, with an admin UI for issuing API keys and scoping which containers each key can see, plus a browser UI for viewing them. **logify** is the client — a Go TUI that browses projects and streams logs, shipped as a single static binary with one-line installers for macOS, Linux, and Windows.

Go + Python · 4 tagged releases · CI · Docker Compose · reverse-proxy docs for Coolify, Nginx, and Traefik

---

### 4. clip — `github.com/Prince2412k2/clip`

**Copy on one machine, paste on another — over your own tailnet.**

A targeted cross-platform clipboard for a personal Tailscale fleet. Copy something on your laptop and it lands on a *selected* target machine's clipboard: text, images, or files up to 20 MB. A small phone-facing web app lets you push to any machine from a browser. Each machine runs a daemon that binds only to the Tailscale interface — never `0.0.0.0` — so Tailscale's encryption and ACLs *are* the trust boundary, with no application-level auth to get wrong.

Two correctness invariants are stated up front and unit-tested: content received and written to the clipboard is never echoed back (no A→B→A loop), and identical repeats are deduped by content hash.

Go, stdlib only, no cgo · single static binary · spec, plan, and design docs

---

### 5. Noter — `github.com/Prince2412k2/Noter`

**A terminal notepad that keeps its own history.**

A quick-access note-taking TUI built on Python curses and DuckDB, using your own `$EDITOR` (nvim) for actual editing rather than reimplementing one. Every change is committed to git behind the scenes, so notes have a real dated version history you can browse from inside the app, and it fires native system notifications.

Python · curses + DuckDB · git-backed versioning · screenshots in the README

---

### 6. gitswitch — `github.com/Prince2412k2/gitswitch`

**Stop committing with your work email on personal projects.**

Manages multiple git identities and their SSH keys. Profiles live as TOML in `~/.config/git_conf/`; pick one interactively and it rewrites the current repo's local `.git/config` and sets `core.sshCommand` so the right key is used per repo. Solves the specific problem of having two GitHub accounts on one machine.

Go · v1.0.0 tagged · interactive TUI forms

---

### 7. vcs — `github.com/Prince2412k2/vcs`

**Git, rebuilt from scratch in Python, to find out how it actually works.**

A working toy version-control system implementing the real plumbing: `init`, `commit`, `log`, `checkout`, `merge`, `reset`, and `cat-file`. Content-addressed object store, no network, no libraries doing the hard part. Written after reading the Git internals docs, as a way to stop treating git as a black box.

Python · pip-installable · deliberately not for production

---

### 8. stylized-maps — `github.com/Prince2412k2/stylized-maps`

**A custom vector-tile pipeline for stylized maps of India.**

An end-to-end map asset pipeline: OSM extracts through Planetiler (with custom Java profiles) into vector tiles against a versioned normalized schema, served by a TypeScript API with an OpenAPI spec, rendered by a web client with hand-built cartographic styling. Ships with a resource-governed build CLI (`map-assets start/pause/resume`, with auto/full/manual CPU and RAM profiles) because the full build is heavy. Production deployment via Docker Compose behind Caddy.

Python + TypeScript + Java · in progress

---
