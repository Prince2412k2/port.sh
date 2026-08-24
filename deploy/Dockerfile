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

# Third-party agent CLIs are not installed. Envoy talks directly to Codex and
# Ollama through ai-sdk/parley.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# A system account with no shell, purely so the process is not root. Nothing
# ever logs into it -- there is no login path here at all any more.
RUN useradd --system --no-create-home --shell /usr/sbin/nologin portfolio \
 && mkdir -p /app/keys /app/messages /app/agent/codex \
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

ENV HOME=/tmp
ENV ENVOY_CONFIG=/app/data/envoy.json
ENV HOME=/app/agent

# The image is both services. ENTRYPOINT is the binary alone and the flags
# come from `command:` in docker-compose.yml, so the ssh and web containers
# are the same image started two different ways -- rather than two images, or
# one image with a wrapper script branching on an environment variable.
EXPOSE 2222 8080
ENTRYPOINT ["/usr/local/bin/portfolio"]
CMD ["--serve", "--ssh-addr", "0.0.0.0", "--ssh-port", "2222", \
     "--host-key", "/app/keys/host_key"]
