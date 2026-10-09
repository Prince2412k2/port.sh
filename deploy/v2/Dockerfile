FROM rust:1.98.1-bookworm AS build
ARG TARGETARCH
WORKDIR /src
RUN rustup target add wasm32-unknown-unknown && cargo install wasm-bindgen-cli --version 0.2.127 --locked
RUN apt-get update && apt-get install -y --no-install-recommends python3-pil python3-fonttools python3-brotli && rm -rf /var/lib/apt/lists/*
COPY v2 v2
COPY map map
COPY skills skills
COPY portfolio/data portfolio/data
COPY portfolio/src/portraits.rs portfolio/src/portraits.rs
COPY portfolio/src/paint.rs portfolio/src/paint.rs
COPY portfolio/src/museum.rs portfolio/src/museum.rs
COPY portfolio/src/walls.rs portfolio/src/walls.rs
COPY scripts/build-v2-web.sh scripts/build-v2-web.sh
COPY scripts/build-v2-atlas.py scripts/build-v2-atlas.py
RUN cargo build --manifest-path v2/Cargo.toml --locked --release -p portfolio-v2-native -p portfolio-v2-backend
RUN bash scripts/build-v2-web.sh
RUN mkdir -p /downloads && cp v2/target/release/portfolio-v2-native /downloads/portfolio-v2-native-linux-${TARGETARCH} && cd /downloads && sha256sum portfolio-v2-native-linux-${TARGETARCH} > SHA256SUMS

FROM debian:bookworm-slim AS backend
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /app/sessions && chown 65534:65534 /app/sessions
COPY --from=build /src/v2/target/release/portfolio-v2-backend /usr/local/bin/
COPY --from=build /src/v2/target/release/portfolio-v2-native /usr/local/bin/
COPY --from=build /src/v2/crates/browser/dist /app/web
COPY --from=build /downloads /app/downloads
COPY scripts/install-v2.sh /app/downloads/install.sh
ENV PORTFOLIO_V2_WEB_DIR=/app/web PORTFOLIO_V2_MAP_DIR=/app/map PORTFOLIO_V2_DOWNLOAD_DIR=/app/downloads PORTFOLIO_V2_SESSION_DIR=/app/sessions
USER 65534:65534
EXPOSE 8322
ENTRYPOINT ["portfolio-v2-backend"]

FROM debian:bookworm-slim AS hosted
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates openssh-server mosh && rm -rf /var/lib/apt/lists/* \
    && useradd -m -s /bin/sh portfolio && passwd -d portfolio && mkdir -p /run/sshd
COPY --from=build /src/v2/target/release/portfolio-v2-native /usr/local/bin/
COPY deploy/v2/sshd_config /etc/ssh/sshd_config
COPY deploy/v2/session.sh /usr/local/bin/portfolio-session
COPY deploy/v2/entrypoint.sh /usr/local/bin/hosted-entrypoint
RUN chmod 755 /usr/local/bin/portfolio-session /usr/local/bin/hosted-entrypoint
ENV PORTFOLIO_V2_ENDPOINT=http://127.0.0.1:8322 LANG=C.UTF-8 PORTFOLIO_V2_EPHEMERAL=1
ENTRYPOINT ["/usr/local/bin/hosted-entrypoint"]
