# syntax=docker/dockerfile:1
#
# JoeDocs MCP server image. Build with `docker compose build`; see
# docs/developer-guide.md ("Running with Docker").

# The Rust version matches rust-toolchain.toml (which is not copied in, so
# the image's own toolchain is used without downloading components).
FROM rust:1.99-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin joedocs \
    && cp target/release/joedocs /usr/local/bin/joedocs

FROM debian:trixie-slim
# git is the history store; JoeDocs runs it with the user's config ignored.
RUN apt-get update \
    && apt-get install -y --no-install-recommends git \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --create-home --home-dir /home/joedocs joedocs \
    && mkdir -p /data/vault /data/state \
    && chown -R joedocs:joedocs /data
COPY --from=build /usr/local/bin/joedocs /usr/local/bin/joedocs

USER joedocs
WORKDIR /data
ENV JOEDOCS__STORAGE__HOME=/data/vault \
    JOEDOCS__STORAGE__STATE_DIR=/data/state \
    JOEDOCS__SERVER__BIND=0.0.0.0:8750
VOLUME ["/data/vault", "/data/state"]
EXPOSE 8750

# /health needs no API key; bash's /dev/tcp avoids installing curl.
HEALTHCHECK --interval=5s --timeout=3s --start-period=5s --retries=5 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8750 && printf 'GET /health HTTP/1.0\\r\\nHost: localhost\\r\\n\\r\\n' >&3 && grep -q '^ok' <&3"]

# `init` is idempotent, so the first start creates the vault, database and
# history, and later starts only check them.
CMD ["sh", "-c", "joedocs init && exec joedocs serve"]
