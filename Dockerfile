# syntax=docker/dockerfile:1
FROM node:24-bookworm-slim AS ui
WORKDIR /src/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci --no-fund --no-audit
COPY frontend ./
RUN npm run build

FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY --from=ui /src/crates/proxy/src/web/dist ./crates/proxy/src/web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build -p claude-proxy --release --locked

FROM debian:bookworm-slim AS proxy
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --create-home appuser
RUN mkdir /data && chown appuser:appuser /data
WORKDIR /app
COPY --from=build /src/target/release/claude-proxy /usr/local/bin/claude-proxy
USER appuser
EXPOSE 10800 10801
VOLUME ["/data"]
CMD ["--host", "0.0.0.0", "--database", "/data/proxy.sqlite3"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl --fail --silent "http://127.0.0.1:10800/health" || exit 1
ENTRYPOINT ["claude-proxy"]
