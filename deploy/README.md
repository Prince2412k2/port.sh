# Running it

```bash
docker compose up -d --build
```

Two ways in, one program:

```bash
ssh -p 2222 your-host          # terminal-native
open http://your-host:8080     # the same thing, in a browser
```

No username needed for ssh. There is no OS account to name — the binary
speaks SSH itself, so a login name is just a string in the handshake, and any
one (or none at all) is accepted.

Two containers from one image, started with different flags. Separate on
purpose: a crash or restart on one transport does not take the other down,
and each gets its own resource ceiling.

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

### Shaders, later

xterm.js renders through its WebGL addon into a `<canvas>`, which is the
right surface for a post-processing pass — CRT curvature, bloom, scanlines,
phosphor persistence. The client already loads `WebglAddon` and falls back
to the canvas renderer if WebGL is unavailable, so the hook is in place; a
shader pass would sample that canvas as a texture rather than touching any
of the server code.

Published on **2222** for now rather than 22, so this never has to fight
whatever the host's own sshd is doing. Move it to 22 later by changing the
left side of the `"2222:2222"` line in `docker-compose.yml`.

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
| filesystem | fully read-only, no `tmpfs` anywhere |
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
docker compose logs portfolio | grep fingerprint
```

Publish that so people can check it before trusting the connection.

## Session limits

A stranger's session should not be able to take the box down or run forever:

| | |
|---|---|
| concurrent sessions | 128 (`MAX_SESSIONS` in `net.rs`) |
| idle timeout | 15 minutes with no keystroke (`PORTFOLIO_IDLE_SECS`, `0` disables it) |
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

If yours live elsewhere, edit the left-hand side of those three lines. The
container reads them through `TERMAP_DATA`.

## Editing the text without rebuilding

`about.txt`, `taste.txt` and `places.txt` are read from disk before the copies
built into the binary, and the compose file mounts all three. Edit, then
`docker compose restart portfolio`. No rebuild, no recompile.

## The chat section

`ask` shells out to `opencode acp`, which is not in the image — the section
will say it could not start the agent, and everything else works. To enable
it, install opencode in the runtime stage and give the container credentials
for whichever provider you use.

Two things to decide before you do. It spends your tokens on questions from
strangers, and the only brake is `MAX_TURNS` in `portfolio/src/acp.rs` —
twelve questions per connection, with nothing stopping a reconnect. And the
session is pinned to plan mode, which is what makes it safe to expose: it
refuses every permission request and has no tools, so everything it answers
from is the context the portfolio hands it.

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
