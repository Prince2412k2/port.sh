# Running it

```bash
docker compose up -d --build
```

Two ways in, one program:

```bash
ssh -p 2222 your-host          # terminal-native; 22 and 1234 in production
open http://127.0.0.1:8222     # the same thing, in a browser
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
git clone <this> /opt/port.sh && cd /opt/port.sh
printf 'OLLAMA_API_KEY=...\nEXA_API_KEY=...\nJINA_API_KEY=...\n' > .env
chmod 600 .env
PMTILES=/srv/tiles/vector.pmtiles docker compose up -d --build
```

Put `PMTILES` in the `.env` too and every later `docker compose up -d` picks
it up on its own.

### In front of it

Only the web side is proxied. Caddy needs one block and no WebSocket
configuration — it upgrades them itself, and it sets `X-Forwarded-For`, which
is where the visitor log and the per-address limits get the address from.

For a Caddy that is itself in Docker on a shared network, that is
`docker-compose.prod.yml` — ssh on 22 and 1234, the web publishing nothing and
reachable by name on the proxy's network:

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d --build
```

The deploy names both files rather than relying on either being picked up. To
make plain `docker compose` mean the same thing in a shell on that box, put
this in its `.env`:

```
COMPOSE_FILE=docker-compose.yml:docker-compose.prod.yml
```

The process inside stays unprivileged either way: 22 and 1234 are published
host ports mapped to the container's 2222, and nothing in it binds a
privileged port.

```caddy
port.sniffkin.tech {
        reverse_proxy portfolio-web:8222

        encode gzip zstd

        header {
                Strict-Transport-Security "max-age=31536000; includeSubDomains"
                X-Content-Type-Options "nosniff"
                X-Frame-Options "SAMEORIGIN"
                Referrer-Policy "strict-origin-when-cross-origin"
        }

        log {
                output stdout
                format console
        }
}
```

For a Caddy on the host instead, leave the port published on loopback — which
is the default — and point at that:

```caddy
port.sniffkin.tech {
        reverse_proxy 127.0.0.1:8222
}
```

Either way the page notices it arrived over TLS and opens a `wss://` socket
rather than a `ws://` one. There is nothing to configure for that.

**The service is `portfolio-web`, not `web`.** A compose service name is a DNS
alias on every network it joins, so two containers called `web` on one proxy
network are two answers to the same question and Docker picks one per lookup.
If something else on that network is already `web`, both sites break
intermittently and neither log says why. Renaming ours was cheaper than
debugging that once.

### SSH cannot go through Caddy

Caddy is an HTTP server. Publishing `22:22` on the Caddy container does
nothing for this: stock `caddy:2` has no layer4 module, and a Caddyfile made
of site blocks never listens on 22 at all. There is also nothing to gain —
there is no TLS to terminate on an SSH stream and no hostname in it to route
on.

So SSH is published straight from this container, which is what
`docker-compose.prod.yml` does, and the `22:22` line comes off the Caddy
service.

**Check what has 22 first.** `ss -lntp | grep :22` — if the host's own sshd is
there, moving it is a thing to do carefully and from a second session you keep
open, because getting it wrong is being locked out of the box. Nothing here
needs 22: `ssh -p 2222` is a working answer and the default.

### Going live

In order, because two of these are one-way:

1. **`docker compose logs portfolio | grep ssh_host_key`.** Record the
   fingerprint and publish the `SSHFP` line beside it. Do this before anybody
   connects: the alternative is asking people who already trusted a key to
   trust a different one. Never delete the `hostkey` volume.
2. **`docker compose exec portfolio-web /usr/local/bin/portfolio --probe`.** It prints
   which agent tier answered. `no agent tier answered` means the ask section
   will tell visitors so, politely, and everything else works — see **The chat
   section** for logging the agent in.
3. **Check the limits.** `--probe` prints them too. The defaults cap a day at
   $10 of model spend and 24 visits an address; both are `.env` settings and
   both matter the moment the URL is somewhere public.
4. **`curl -sI https://prince.dev`** and then open it. Then
   `ssh -p 2222 prince.dev`. Both paths run the same program, and either can
   be broken while the other is fine.

### When a service is renamed

Compose only manages what is in the file, so a service that has been renamed
leaves its old container running under the old name — still up, still holding
the ports it was given. The replacement then cannot bind them and never
starts, and `docker compose exec` on the new name says it is not running,
which is true and not the reason.

```bash
docker compose ps -a                       # both names will be here
docker compose up -d --remove-orphans      # the old one goes, the new one binds
```

The deploy passes `--remove-orphans` for this, so it only bites a stack that
was last brought up by hand across the rename. `web` became `portfolio-web` in
this repo, so a deployment older than that needs it once.

Anything on the `messages` volume can be read from either container, and they
both mount it — so while one is down the other still has it:

```bash
docker compose exec -it portfolio portfolio --visitors
```

### Updating

By hand:

```bash
git pull && docker compose up -d --build
```

The data, the host key, the messages and the agent's login are all volumes or
binds, so none of them are touched. Text in `about.txt`, `taste.txt` and
`places.txt` needs no build at all — see below.

### On push to main

`.github/workflows/deploy.yml` does the same two commands over ssh. GitHub
holds the key and says when; the box does the work. Nothing is compiled on the
runner, deliberately — the binary is linked inside the same bookworm image it
runs in, and a runner is two Ubuntu releases ahead of that. A binary built
against a newer glibc than its runtime does not start, and that is a thing you
find out in production or not at all.

In **Settings → Secrets and variables → Actions**. It runs
`ssh -p $PORT $USER@$HOST <command>`, from these:

| secret | what |
|---|---|
| `HOST` | the box, by name or address |
| `PORT` | **the box's own sshd.** No default — see below |
| `USER` | the account the checkout belongs to |
| `SSH_KEY` | the private half of a key made only for this. Not the `.pub` |
| `KNOWN_HOSTS` | optional, and the only one about trust — see below |

The checkout path is not a secret and is not one of them: it is
`DEPLOY_PATH: /opt/port.sh` at the top of the workflow, where it can be read
without opening the settings page.

**`PORT` has no default and that is deliberate.** It used to fall back to 22 —
which, once this app is deployed on 22, is this app. An unset secret would
have pointed the deploy at the portfolio's own ssh server rather than at the
box. It refuses any command it does not recognise so the job fails, which is
the right outcome reached by a route nobody can read. Every missing secret is
now named before a connection is attempted at all.

```bash
# on your machine: a key that does nothing else
ssh-keygen -t ed25519 -f deploy -C 'github actions -> port.sh' -N ''

# SSH_KEY is the private half -- the file with no extension
cat deploy

# on the box: let the public half in, and let it do nothing but this
echo "restrict $(cat deploy.pub)" >> ~/.ssh/authorized_keys
```

### Pinning the box, or not

`KNOWN_HOSTS` is the answer to "which machine is allowed to be that address".
Set it and the runner talks to that host key and no other:

```bash
ssh-keyscan -p "$PORT" "$HOST"
```

Run that with **the same host and port the secrets hold**. A known_hosts entry
is keyed by exactly what was typed: an address and a name for one machine are
two different entries, and a non-default port is recorded as `[host]:port`.
Scanned one way and connected the other, the deploy fails on a host key it has
never seen — the check working, and reading exactly like the check being
broken.

Leave it unset and the deploy asks the address who it is and believes the
answer, which is not a check. That is a real gap and the workflow says so in
the log rather than hiding it behind `StrictHostKeyChecking=no`. It is also
smaller than it sounds: ssh signs a challenge belonging to the session it is
in, so an impostor cannot replay it against the real box or learn the key from
it. What it would get is this command, and your deploy not happening.

`restrict` turns off port forwarding, agent forwarding, X11 and pty allocation
for that key. The deploy needs none of them.

The deploy user must be in the `docker` group and own the checkout. The box
also needs to reach GitHub itself for the `git fetch` — a read-only deploy key
on the repo if it is private, or an `https://` remote if it is not.

What it does, in order: fetch, `reset --hard` to the pushed commit, build, and
only then swap the containers over. Two consequences worth knowing:

- **A failed build changes nothing.** `docker compose up` never gets there, so
  the containers that are serving keep serving. The site cannot be taken down
  by a commit that does not compile.
- **`reset --hard`, never `git clean`.** Reset moves the tracked files and
  leaves everything else alone, which is what keeps `.env` on the box. Clean
  would delete it along with the credentials in it.

It waits for `--health` to answer before reporting success, because "compose
exited 0" and "the site is up" are not the same claim. If it never answers,
the job fails and prints `docker compose ps` and the last forty log lines.

The build caches — the cargo registry and both target directories — are
BuildKit caches on the box, which is what keeps a one-line change from
recompiling four hundred crates. They grow. `docker builder prune` when the
disk starts to matter; the next deploy is slow once and then fast again.

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

Published on **2222** by default, so this never has to fight whatever the
host's own sshd is doing. `SSH_BIND=0.0.0.0:22` — or the override in *Putting
it on the internet* — moves it to 22 once nothing else on the box wants that
port, which is worth checking rather than assuming.

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
| concurrent sessions | 128 each transport (`MAX_SESSIONS`) |
| concurrent SSH connections | 192 globally, 4 per address; 1 session per address |
| concurrent browser sessions | 3 per address — a browser is a thing people have two of |
| new browser sessions | 12 a minute per address |
| page requests | 60 a minute per address; it is 62 KB of HTML |
| idle timeout | 15 minutes with no keystroke (`PORTFOLIO_IDLE_SECS`, `0` disables it) |
| maximum session | 1 hour (`PORTFOLIO_MAX_SESSION_SECS`, `0` disables it) |
| daily visits | 24 per key/id and address (`PORTFOLIO_DAILY_VISITS`) |
| daily AI allocation | $10, reserving $0.25 per provider attempt (`PORTFOLIO_DAILY_AI_USD`, `PORTFOLIO_AI_REQUEST_USD`) |
| container | 1.5 CPUs, 512 MB, 256 pids (`docker-compose.yml`) |

An address is a v4 address or a v6 **/64**, because a visitor is given a whole
/64 and counting single addresses there is a rule only the honest obey. The
browser limits are per address and not per browser id: an id is something the
client chooses and can throw away, which makes it useful for recognising a
returning visitor and useless for refusing one. Loopback is exempt throughout
— it is the health check and you, mid-deploy.

Refusals answer `429` with a `Retry-After`, and each one is logged with the
address, so a lot of them arriving from one place is visible rather than
mysterious:

```bash
docker compose logs web | grep -E 'web_(page|session)_(refused|crowded)'
```

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

`ask` runs `envoy`, which is in the image. What it needs from you is a
credential, and **it will not go and get one**: `parley::auth` opens with
"credentials, read rather than obtained", and means it. Logging in is an
operator's job. A browser callback on localhost means nothing to somebody
reached over SSH, and a visitor to a portfolio is not going to be handed an
OAuth prompt — so nothing in the running program opens a browser or waits on a
device code. It reads what is already on disk and keeps it fresh.

With nothing on disk the section says the agent would not start and every
other section carries on working. That is the intended failure: a missing
credential should cost one tab, not the site.

Two tiers, in `ai-sdk/envoy.json`, tried in order. They sign up differently.

### Ollama Cloud — the one to do first

An API key from [ollama.com](https://ollama.com), and nothing else:

```
# .env beside docker-compose.yml
OLLAMA_API_KEY=...
```

`docker compose up -d` and the tier is live. This is the whole signup, it
backs two models, and it is the difference between a working ask tab today and
an empty one.

### Codex — a ChatGPT account, not an API key

The first tier talks to `https://chatgpt.com/backend-api/codex`, which is
reached with a **ChatGPT subscription's OAuth tokens** and not with a platform
API key. There is nothing to paste into `.env`: the credential is a token pair
in the file the Codex CLI writes.

So it is made elsewhere and brought here:

```bash
# on a machine with a browser -- the OAuth callback is localhost, which is
# exactly why this cannot be done over ssh on the server
codex login
cat ~/.codex/auth.json          # {"tokens":{"access_token":…,"refresh_token":…}}

# onto the agent volume, where envoy.json points: $HOME/codex/auth.json,
# and HOME is /app/agent
docker compose cp ~/.codex/auth.json portfolio:/app/agent/codex/auth.json
docker compose restart portfolio portfolio-web
```

**A refresh token has one owner, and that is the issuer's rule rather than
ours.** It is rotated the moment it is exchanged, so the copy that was not the
one to exchange it is dead. Envoy only refreshes when the token is nearly
expired — the point at which the other copy was about to stop working anyway —
and takes whichever of the two was issued more recently, so it heals in both
directions. What it cannot do is make one login serve two programs for ever. If
you also use `codex` on your laptop against the same account, expect one of
them to need signing in again; give the server its own login if that matters.

Its renewed copy goes to `$ENVOY_AUTH_STORE`, which the image points at
`/app/agent/envoy` — on the volume, because a renewal written to scratch is a
dead credential at the next restart. Never write back to the seed file: that
one belongs to whoever made it, and they may be writing it at the same moment.

### Which one answered

```bash
docker compose exec portfolio-web portfolio --probe
```

That prints the tiers, runs one real one-word question, and says which would be
used. It is the same code the server runs on its timer — the only thing that
tells *configured* from *answering*, since a listed model can be out of quota,
unauthenticated or withdrawn and all three look fine until something asks.
`no agent tier answered` means neither of the above has been done.

A background check repeats it hourly so nobody discovers a dead tier mid-
question, and within a tier a model that fails is dropped and the next is asked
the same thing. Change the interval with `PORTFOLIO_PROBE_SECS`.

`portfolio/data/models.txt` is mounted, so the order of tiers and models can
change without a rebuild. What a model *is* — endpoint, context window, which
credential — lives in `ai-sdk/envoy.json`, also mounted, deliberately the only
place that exists.

## Who came

Visits are logged, so there is an answer to the question a portfolio exists to
ask: is anybody looking, and from where.

```bash
docker compose exec -it portfolio-web portfolio --visitors
```

People, most recent first, folded across their visits. Enter opens one; inside,
their conversations are numbered, and pressing the number **opens that
conversation back up in the chat itself** — read-only, with the map or the
diagram or the project card that came with each answer. Those are the app's own
renderers, and there is only one honest way to show somebody what a visitor
saw, which is to show them it.

```
  visitors  3 visitors   5 visits   12 questions

  › prince            3 visits   3 questions    15m00s   Ahmedabad, India   2026-08-24 19:33
    someone           1 visit    9 questions    open                        2026-08-24 19:30
    w-mf3k2p-a91      1 visit    0 questions    59s      Berlin, Germany    2026-08-24 19:25

  enter open   / search   s sort   r returning   q quit
```

`/` searches names, addresses, places and the questions themselves. `s` sorts
by recency, visits or questions. `r` hides anyone who came only once.

A question appears once whatever became of it, marked `[cancelled]`,
`[failed]` or `[unanswered]` when it did not finish, and `[x9]` when the same
one was asked nine times. `open` in place of a duration is a visit still going,
or one whose process was killed under it.

`bin/visits --docker` prints the same log as plain text, reading it out of
whichever of the two containers is up. It is the fallback for a machine with no
build of this, and it cannot open the conversations — that needs the
renderers, and they are in the binary.

Underneath is append-only JSONL, beside the messages on the same volume:

```bash
docker compose exec portfolio-web cat /app/messages/visits.jsonl | jq
```

Five kinds of line, all keyed by `session`:

| event | what |
|---|---|
| `arrive` | transport, username, identity, address, client, and whether they have been here before |
| `where` | city, region, country, lat/lon — appended when the lookup returns |
| `question` | one asked, at the moment it was sent |
| `ask` | one exchange that finished, question and answer, and what it cost |
| `leave` | how long they stayed and how many questions they asked |

**What identifies somebody, and what to call them.** Over SSH, two things
arrive as part of logging in and neither is taken behind anyone's back: the
username they typed in front of the `@`, and the fingerprint of the key they
authenticated with. Any string is accepted as a username, so it is what
somebody chose to be called rather than an account — which makes it the closest
thing to a name here, and the report leads with it. A browser has nowhere to
type one, so its stable id stands in: not a name, but the thing that makes two
visits the same person. The fingerprint
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
