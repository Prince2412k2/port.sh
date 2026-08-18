# The portfolio, behind a real sshd.
#
# The alternative was an SSH server inside the Rust binary. OpenSSH is better
# audited than anything that would have gone here, it already knows how to do
# ptys and rekeying and rate limiting, and it means the only thing this project
# has to get right is refusing everything except one command.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# Only the one binary; the map and sheet crates build as libraries under it.
RUN cargo build --release -p portfolio

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /run/sshd /etc/ssh/keys

# The account visitors land on. A real shell, because sshd runs ForceCommand
# through it -- nologin would refuse before the portfolio ever started -- and
# no password, so the shell is unreachable by any other route.
RUN useradd --create-home --shell /bin/sh visitor \
 && passwd --lock visitor

WORKDIR /app
COPY --from=build /src/target/release/portfolio /usr/local/bin/portfolio
COPY portfolio/data/ /app/data/
COPY map/data/places.txt /app/data/places.txt
COPY deploy/sshd_config /etc/ssh/sshd_config
COPY deploy/anykey /usr/local/bin/anykey
COPY deploy/entrypoint /usr/local/bin/entrypoint
RUN chmod 0755 /usr/local/bin/anykey /usr/local/bin/entrypoint /usr/local/bin/portfolio \
 && chown -R visitor:visitor /app

# Where termap looks for the basemap and heightmap. Both are mounted, not
# baked: the India archive is 1.6 GB and does not belong in an image.
ENV TERMAP_DATA=/app/data

EXPOSE 22
ENTRYPOINT ["/usr/local/bin/entrypoint"]
