# Running it

```bash
docker compose up -d --build
ssh -p 2222 visitor@your-host
```

That is the whole thing. Any SSH key, straight into the portfolio.

Published on **2222** for now rather than 22, on purpose: it means this never
has to fight the host's own sshd, and there is nothing to discover about a port
conflict from outside the machine. Move it to 22 later by changing the one
`"2222:22"` line in `docker-compose.yml` — the container's own sshd already
listens on 22 internally, so nothing else changes.

## What a visitor can do

Authenticate with any key, get a pty, and run one program. That is the entire
surface:

| | |
|---|---|
| any public key | accepted — see `anykey` and the note in `sshd_config` |
| passwords | off |
| forwarding (TCP, agent, X11, unix, tunnel) | off |
| sftp | off |
| shell | unreachable; `ForceCommand` runs the portfolio whatever you ask for |
| root login | off |

The container drops every capability except the four sshd needs, runs with a
read-only root filesystem, `no-new-privileges`, a pid limit, and CPU and memory
caps. A visitor's session cannot outlive `MaxSessions 2`, `LoginGraceTime 20`
and the client-alive timeout.

Anyone with a key gets in because there is nothing here to protect and asking
strangers to register a key to read a CV is a worse trade. If you would rather
gate it, delete the `AuthorizedKeysCommand` lines and put real keys in
`/home/visitor/.ssh/authorized_keys`.

Sessions time out after fifteen minutes with no keystroke
(`PORTFOLIO_IDLE_SECS`, or `0` to disable). sshd's own keepalive only catches a
client that has stopped answering; a laptop left open on a page answers
keepalives all afternoon and holds the slot the whole time.

## The map data

The archives are not in the image. The India basemap is 1.6 GB, and both it and
the heightmap are rebuildable from `map/scripts/`. The compose file bind-mounts
them:

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

`ask` shells out to `opencode acp`, which is not in the image — the section will
say it could not start the agent, and everything else works. To enable it,
install opencode in the runtime stage and give the container credentials for
whichever provider you use.

Two things to decide before you do. It spends your tokens on questions from
strangers, and the only brake is `MAX_TURNS` in `portfolio/src/acp.rs` — twelve
questions per connection, with nothing stopping a reconnect. And the session is
pinned to plan mode, which is what makes it safe to expose: it refuses every
permission request and has no tools, so everything it answers from is the
context the portfolio hands it.

## Fingerprints

The host key is generated once into the `hostkeys` volume and reused. Do not
delete that volume unless you mean to: a new key shows every returning visitor
a man-in-the-middle warning.

Publish the fingerprint so people can check it:

```bash
docker compose exec portfolio ssh-keygen -lf /etc/ssh/keys/ssh_host_ed25519_key.pub
```

## Checking it without a terminal

Piped, the binary prints a plain-text CV instead of raw-mode escapes, which is
what the healthcheck uses:

```bash
ssh -p 2222 visitor@your-host | cat        # plain text
ssh -p 2222 -t visitor@your-host   # force the interactive one
```
