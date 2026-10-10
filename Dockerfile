# syntax=docker/dockerfile:1
FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build -p claude-proxy --release --locked

FROM debian:bookworm-slim AS proxy
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --create-home appuser
WORKDIR /app
COPY --from=build /src/target/release/claude-proxy /usr/local/bin/claude-proxy
USER appuser
EXPOSE 10800
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl --fail --silent "http://127.0.0.1:${PROXY_PORT:-10800}/health" || exit 1
ENTRYPOINT ["claude-proxy"]
