# syntax=docker/dockerfile:1
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

# Both binaries in one step, against caches that outlive the layer.
#
# The build stage is a Debian one version for version with the stage below it,
# which is the whole reason the compiling happens in here rather than on
# whatever machine is driving the build. A binary linked against a newer glibc
# than the image it lands in does not start, and the runners this is likely to
# be built on have moved further ahead of bookworm every year. Matching them by
# hand is a thing to get wrong once, quietly, in the one environment nobody
# tests in.
#
# The cache mounts are what make that affordable. `COPY . .` is invalidated by
# any edit at all, so without them every push recompiles four hundred crates to
# change a line of a shader. The registry and both target directories live in
# BuildKit caches instead -- which means they are not in the layer either, so
# the binaries are copied somewhere that is, in the same step, before the mount
# goes away.
#
# `--locked` because a lockfile that is quietly updated mid-build is a
# different program from the one that was tested.
#
# Two cargo invocations: the agent is its own workspace -- see
# ai-sdk/Cargo.toml -- so a broken experiment in there cannot stop the
# portfolio compiling, and two lockfiles are not held hostage to each other.
# Only the one portfolio binary; map and skills build as libraries under it.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    --mount=type=cache,target=/src/ai-sdk/target,sharing=locked \
    cargo build --release --locked -p portfolio \
 && cargo build --release --locked --manifest-path ai-sdk/Cargo.toml -p envoy \
 && mkdir -p /out \
 && cp target/release/portfolio /out/portfolio \
 && cp ai-sdk/target/release/envoy /out/envoy

FROM debian:bookworm-slim

# Third-party agent CLIs are not installed. Envoy talks directly to Codex and
# Ollama through ai-sdk/parley.
# `mosh` is here for `mosh-server` alone -- the third way in, alongside ssh
# and the browser. The client half and the perl wrapper come with it in Debian
# and are never run; `--no-install-recommends` is what keeps openssh-client out
# with them, which matters, because an ssh client is exactly the thing this
# image spent so much effort not having.
#
# Note what this costs, because it is the one place the paragraph at the top of
# this file stops being true. A mosh session is a real pty with a real forked
# child in it, where ssh and the browser have neither. What holds instead: the
# command that child is started with is built in `net.rs` out of constants, so
# no part of it comes from the visitor; it runs as the same unprivileged user
# as everything else here; and the number that can exist at once is the width
# of the published UDP range and nothing else.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates mosh \
 && rm -rf /var/lib/apt/lists/*

# A system account with no shell, purely so the process is not root. Nothing
# ever logs into it -- there is no login path here at all any more.
RUN useradd --system --no-create-home --shell /usr/sbin/nologin portfolio \
 && mkdir -p /app/keys /app/messages /app/agent/codex /app/agent/envoy \
 && chown -R portfolio:portfolio /app/keys /app/messages /app/agent

WORKDIR /app
COPY --from=build /out/portfolio /usr/local/bin/portfolio
# `envoy`, the first tier in models.txt. `ENVOY_CONFIG` below says where its
# model catalogue is; without it, it looks for ./envoy.json and the tier is
# skipped with a line on stderr rather than taking the section down.
COPY --from=build /out/envoy /usr/local/bin/envoy
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

# The agent volume, not the tmpfs: envoy keeps its Codex credential under
# `$HOME`, and on scratch that works perfectly until the first restart and then
# silently drops the tier. This was set twice for a while, tmpfs first, and the
# second one quietly winning is not a thing to leave lying around.
ENV HOME=/app/agent

# Where envoy keeps its own copy of a renewed token pair.
#
# It never writes back to the login it was seeded from -- that file belongs to
# whoever made it -- so it needs one writable path of its own, and on this
# image there is exactly one: the agent volume. Left unset it would work out at
# `$HOME/.local/share/envoy`, which is also on the volume and also correct, and
# is a hidden directory nobody would think to look in. This is the same place
# `parley::auth::keep_for` is documented against.
#
# It has to persist. The issuer rotates a refresh token every time it is
# exchanged, so a renewal that is written to scratch means the copy on disk is
# dead at the next restart and the tier is gone with it.
ENV ENVOY_AUTH_STORE=/app/agent/envoy
ENV ENVOY_CONFIG=/app/data/envoy.json

# The image is both services. ENTRYPOINT is the binary alone and the flags
# come from `command:` in docker-compose.yml, so the ssh and web containers
# are the same image started two different ways -- rather than two images, or
# one image with a wrapper script branching on an environment variable.
# mosh-server refuses to start at all without a UTF-8 locale, and says so in a
# sentence about character sets that reads like a warning rather than the hard
# stop it is. `C.UTF-8` is built into glibc, so this needs no locales package.
ENV LANG=C.UTF-8

# 2222 ssh, 8222 http, and the udp range mosh sessions land on -- one port per
# live session, and so also the ceiling on how many there can be. Kept in step
# with `MOSH_PORTS` in portfolio/src/net.rs, which is what actually binds them.
EXPOSE 2222 8222
EXPOSE 60000-60010/udp
ENTRYPOINT ["/usr/local/bin/portfolio"]
CMD ["--serve", "--ssh-addr", "0.0.0.0", "--ssh-port", "2222", \
     "--host-key", "/app/keys/host_key"]
