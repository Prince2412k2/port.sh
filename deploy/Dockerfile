# The portfolio, serving itself over SSH.
#
# No OpenSSH here. The first version used it behind ForceCommand, and it got
# most of the way -- audited transport, ptys, rekeying, rate limiting -- right
# up until the goal became `ssh <domain>` with no username at all: OpenSSH
# looks up the requested account in the OS's user database *before* it will
# run any authentication, AuthorizedKeysCommand included, and there is no
# wildcard for that short of a custom NSS module. The binary now speaks SSH
# itself (the russh crate), where a username is just a string in the protocol
# handshake with no OS account behind it -- so any one, or none at all, is
# accepted by construction. See portfolio/src/net.rs.
#
# That also shrinks what this image needs to almost nothing: no sshd, no PAM,
# no NSS, no privilege-separation directory, no login account for a stranger
# to somehow reach. The process is the whole surface.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# Only the one binary; map and skills build as libraries under it.
RUN cargo build --release -p portfolio
# The agent, from its own workspace. Separate on purpose -- see ai-sdk/Cargo.toml
# -- so a broken experiment in there cannot stop the portfolio compiling, and
# two lockfiles are not held hostage to each other. Two invocations rather than
# one for exactly that reason.
RUN cargo build --release --manifest-path ai-sdk/Cargo.toml -p envoy

FROM debian:bookworm-slim

# opencode, for the ask section. It is a single self-contained binary built on
# Bun -- no node_modules, no package manager left in the image, nothing to
# resolve at runtime. Pinned: this is the piece most likely to change its
# config schema or its flags underneath us, and finding that out from a
# rebuild of an unrelated commit is not a good afternoon.
#
# ca-certificates because it talks to a model over TLS, and it is the only
# thing here that reaches the network at all.
#
# The project moved from sst/opencode to anomalyco/opencode, and the Linux
# builds are tarballs rather than the zips the macOS ones are. Both of those
# were wrong on the first try and only the download said so, so: the tag, the
# owner and the extension here are all load-bearing. `--version` at the end
# means a bad URL fails the build rather than the ask section.
# Two ACP servers, because data/models.txt names two. Copilot's own CLI serves
# the Copilot tier -- first-party, and the only way to that seat that does not
# route a paid seat through somebody else's provider shim. opencode serves the
# zen and ollama tiers underneath it, so a Copilot outage still has somewhere to
# go. Both are single binaries in a tarball, so neither needs node or unzip.
ARG OPENCODE_VERSION=1.17.20
ARG COPILOT_VERSION=1.0.80
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && curl -fsSL "https://github.com/anomalyco/opencode/releases/download/v${OPENCODE_VERSION}/opencode-linux-x64.tar.gz" \
      | tar xz -C /usr/local/bin opencode \
 && curl -fsSL "https://github.com/github/copilot-cli/releases/download/v${COPILOT_VERSION}/copilot-linux-x64.tar.gz" \
      | tar xz -C /usr/local/bin copilot \
 && chmod +x /usr/local/bin/opencode /usr/local/bin/copilot \
 && apt-get purge -y curl \
 && apt-get autoremove -y \
 && rm -rf /var/lib/apt/lists/* \
 && HOME=/tmp /usr/local/bin/opencode --version \
 && HOME=/tmp /usr/local/bin/copilot --version

# A system account with no shell, purely so the process is not root. Nothing
# ever logs into it -- there is no login path here at all any more.
RUN useradd --system --no-create-home --shell /usr/sbin/nologin portfolio \
 && mkdir -p /app/keys /app/messages /app/agent /app/agent/copilot /app/agent/cache \
 && chown -R portfolio:portfolio /app/keys /app/messages /app/agent

WORKDIR /app
COPY --from=build /src/target/release/portfolio /usr/local/bin/portfolio
# `envoy`, the first tier in models.txt. `ENVOY_CONFIG` below says where its
# model catalogue is; without it, it looks for ./envoy.json and the tier is
# skipped with a line on stderr rather than taking the section down.
COPY --from=build /src/ai-sdk/target/release/envoy /usr/local/bin/envoy
COPY ai-sdk/envoy.json /app/data/envoy.json
COPY portfolio/data/ /app/data/
COPY map/data/places.txt /app/data/places.txt
RUN chown -R portfolio:portfolio /app
USER portfolio

# Where termap looks for the basemap and heightmap. Both are mounted, not
# baked: the India archive is 1.6 GB and does not belong in an image.
ENV TERMAP_DATA=/app/data

# Where `/reach` appends. A volume, because this is the one thing the process
# writes that anybody wants to keep, and the filesystem is otherwise read-only.
ENV PORTFOLIO_MESSAGES=/app/messages/messages.jsonl

# Who came, from where, and what they asked. Append-only JSONL on the same
# volume as the messages, because both are things somebody wants to still have
# next month and neither belongs in an image layer.
ENV PORTFOLIO_VISITS=/app/messages/visits.jsonl

# opencode wants somewhere to write. Most of it is scratch and belongs on the
# tmpfs the compose file mounts -- without it opencode fails at spawn on a
# read-only filesystem, and the ask section dies before the first question.
ENV HOME=/tmp
ENV XDG_CONFIG_HOME=/tmp/config

# The cache is the exception, and it is Copilot's doing. `copilot` is a Node
# single-executable that unpacks itself to `$XDG_CACHE_HOME/copilot/pkg/...`
# on first run, and the tmpfs fails it twice over:
#
#   209 MB of package against a 64 MB tmpfs -- ENOSPC, over and over, three
#   attempts and out; and then, given room, `failed to map segment from shared
#   object` loading `runtime.node`, because compose mounts tmpfs `noexec` and
#   that file is a native addon that has to be mapped executable.
#
# The volume is neither small nor noexec, so the cache goes there. It also
# means the unpacking happens once for the life of the volume instead of once
# per restart. Note the first run pays for it: allow for that when a probe of a
# freshly created volume times out and reports the tier as down.
ENV XDG_CACHE_HOME=/app/agent/cache

# Except the credential. `opencode auth login github-copilot` is a device
# flow somebody completes in a browser, and auth.json lands under
# XDG_DATA_HOME -- so that one path is a volume rather than scratch. On tmpfs
# it would work perfectly until the first restart and then quietly drop the
# whole Copilot tier, which is the sort of failure that gets diagnosed as
# "Copilot is down" a week later.
ENV XDG_DATA_HOME=/app/agent

# Copilot keeps its credential under its config directory, which defaults to
# `~/.copilot` -- and HOME here is the tmpfs below. On tmpfs `copilot login`
# works perfectly until the first restart and then the Copilot tier silently
# stops answering, which is exactly the trap the opencode credential already
# taught us. So it is pointed at the volume instead. Supersedes `--config-dir`,
# which is deprecated.
ENV COPILOT_HOME=/app/agent/copilot

# Where `envoy` finds its model catalogue. Mounted from `ai-sdk/envoy.json`, so
# a tier can be reordered or an endpoint corrected without a rebuild -- the same
# arrangement as models.txt. Its credential is the opencode login on the volume
# above: the catalogue names it as `$XDG_DATA_HOME/opencode/auth.json`, which is
# one path that resolves both here and on a laptop.
ENV ENVOY_CONFIG=/app/data/envoy.json

# The image is both services. ENTRYPOINT is the binary alone and the flags
# come from `command:` in docker-compose.yml, so the ssh and web containers
# are the same image started two different ways -- rather than two images, or
# one image with a wrapper script branching on an environment variable.
EXPOSE 2222 8080
ENTRYPOINT ["/usr/local/bin/portfolio"]
CMD ["--serve", "--ssh-addr", "0.0.0.0", "--ssh-port", "2222", \
     "--host-key", "/app/keys/host_key"]
