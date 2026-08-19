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
# A system account with no shell, purely so the process is not root. Nothing
# ever logs into it -- there is no login path here at all any more.
RUN useradd --system --no-create-home --shell /usr/sbin/nologin portfolio \
 && mkdir -p /app/keys \
 && chown -R portfolio:portfolio /app/keys

WORKDIR /app
COPY --from=build /src/target/release/portfolio /usr/local/bin/portfolio
COPY portfolio/data/ /app/data/
COPY map/data/places.txt /app/data/places.txt
RUN chown -R portfolio:portfolio /app
USER portfolio

# Where termap looks for the basemap and heightmap. Both are mounted, not
# baked: the India archive is 1.6 GB and does not belong in an image.
ENV TERMAP_DATA=/app/data

# The image is both services. ENTRYPOINT is the binary alone and the flags
# come from `command:` in docker-compose.yml, so the ssh and web containers
# are the same image started two different ways -- rather than two images, or
# one image with a wrapper script branching on an environment variable.
EXPOSE 2222 8080
ENTRYPOINT ["/usr/local/bin/portfolio"]
CMD ["--serve", "--ssh-addr", "0.0.0.0", "--ssh-port", "2222", \
     "--host-key", "/app/keys/host_key"]
