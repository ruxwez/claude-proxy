# syntax=docker/dockerfile:1
FROM rust:1.96-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# Optional CA mount supports managed build environments; normal builds need none.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    --mount=type=secret,id=proxy_ca \
    if [ -f /run/secrets/proxy_ca ]; then export CARGO_HTTP_CAINFO=/run/secrets/proxy_ca; fi; \
    cargo build -p claude-proxy-server --release --locked && mkdir -p /out && cp target/release/claude-proxy-server /out/claude-proxy-server
FROM debian:bookworm-slim AS proxy
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --create-home appuser \
    && mkdir /data && chown appuser:appuser /data && chmod 700 /data
WORKDIR /app
COPY --from=build /out/claude-proxy-server /usr/local/bin/claude-proxy-server
ENV PROXY_HOST=0.0.0.0 PROXY_CONFIG_DIR=/data
USER appuser
EXPOSE 10800 10801
VOLUME ["/data"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 CMD curl --fail --silent "http://127.0.0.1:${PROXY_PORT:-10800}/health" || exit 1
ENTRYPOINT ["claude-proxy-server"]
