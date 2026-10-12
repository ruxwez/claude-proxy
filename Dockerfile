# syntax=docker/dockerfile:1
FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry cargo build --workspace --release --locked
FROM debian:bookworm-slim AS proxy
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* && useradd --uid 10001 --create-home appuser
RUN mkdir /data && chown appuser:appuser /data && chmod 700 /data
WORKDIR /app
COPY --from=build /src/target/release/claude-proxy-server /usr/local/bin/claude-proxy-server
USER appuser
EXPOSE 10800 10801
VOLUME ["/data"]
CMD ["--host", "0.0.0.0", "--config-dir", "/data"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 CMD curl --fail --silent "http://127.0.0.1:10800/health" || exit 1
ENTRYPOINT ["claude-proxy-server"]
