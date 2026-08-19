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
ARG OPENCODE_VERSION=1.17.20
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && curl -fsSL "https://github.com/anomalyco/opencode/releases/download/v${OPENCODE_VERSION}/opencode-linux-x64.tar.gz" \
      | tar xz -C /usr/local/bin opencode \
 && chmod +x /usr/local/bin/opencode \
 && apt-get purge -y curl \
 && apt-get autoremove -y \
 && rm -rf /var/lib/apt/lists/* \
 && HOME=/tmp /usr/local/bin/opencode --version

# A system account with no shell, purely so the process is not root. Nothing
# ever logs into it -- there is no login path here at all any more.
RUN useradd --system --no-create-home --shell /usr/sbin/nologin portfolio \
 && mkdir -p /app/keys /app/messages /app/agent \
 && chown -R portfolio:portfolio /app/keys /app/messages /app/agent

WORKDIR /app
COPY --from=build /src/target/release/portfolio /usr/local/bin/portfolio
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

# opencode wants somewhere to write. Most of it is cache and belongs on the
# tmpfs the compose file mounts -- without it opencode fails at spawn on a
# read-only filesystem, and the ask section dies before the first question.
ENV HOME=/tmp
ENV XDG_CACHE_HOME=/tmp/cache
ENV XDG_CONFIG_HOME=/tmp/config

# Except the credential. `opencode auth login github-copilot` is a device
# flow somebody completes in a browser, and auth.json lands under
# XDG_DATA_HOME -- so that one path is a volume rather than scratch. On tmpfs
# it would work perfectly until the first restart and then quietly drop the
# whole Copilot tier, which is the sort of failure that gets diagnosed as
# "Copilot is down" a week later.
ENV XDG_DATA_HOME=/app/agent

# The image is both services. ENTRYPOINT is the binary alone and the flags
# come from `command:` in docker-compose.yml, so the ssh and web containers
# are the same image started two different ways -- rather than two images, or
# one image with a wrapper script branching on an environment variable.
EXPOSE 2222 8080
ENTRYPOINT ["/usr/local/bin/portfolio"]
CMD ["--serve", "--ssh-addr", "0.0.0.0", "--ssh-port", "2222", \
     "--host-key", "/app/keys/host_key"]
