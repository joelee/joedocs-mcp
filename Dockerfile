# syntax=docker/dockerfile:1
#
# JoeDocs MCP server image, with two targets on one runtime stage:
#
# - `local`, the default (the last stage), builds joedocs from source.
#   `docker compose build` and `just docker-test` use it; see
#   docs/developer-guide.md ("Running with Docker").
# - `release` copies a prebuilt static binary from dist/linux/<arch>/joedocs:
#   the musl binaries of a GitHub release, as the release workflow does
#   (.github/workflows/release.yml, job `image`). For example:
#   docker buildx build --target release --platform linux/amd64,linux/arm64 .

# The Rust version matches rust-toolchain.toml (which is not copied in, so
# the image's own toolchain is used without downloading components).
FROM rust:1.99-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY templates ./templates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin joedocs \
    && cp target/release/joedocs /usr/local/bin/joedocs

# The runtime both targets share. The base is pinned by the digest of its
# multi-platform index (linux/amd64 and linux/arm64); Dependabot proposes
# updates (.github/dependabot.yml).
FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2 AS runtime
# git is the history store; JoeDocs runs it with the user's config ignored.
RUN apt-get update \
    && apt-get install -y --no-install-recommends git \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --create-home --home-dir /home/joedocs joedocs \
    && mkdir -p /data/vault /data/state \
    && chown -R joedocs:joedocs /data

USER joedocs
WORKDIR /data
# A standalone server: the web interface (and the admin API it needs) is on.
# Signing in over plain HTTP works only on a loopback address, such as a
# port published on 127.0.0.1; see docs/docker.md.
ENV JOEDOCS__STORAGE__HOME=/data/vault \
    JOEDOCS__STORAGE__STATE_DIR=/data/state \
    JOEDOCS__SERVER__BIND=0.0.0.0:8750 \
    JOEDOCS__ADMIN__ENABLED=true \
    JOEDOCS__ADMIN__WEB_UI=true
VOLUME ["/data/vault", "/data/state"]
EXPOSE 8750

# /health needs no API key; bash's /dev/tcp avoids installing curl.
HEALTHCHECK --interval=5s --timeout=3s --start-period=5s --retries=5 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8750 && printf 'GET /health HTTP/1.0\\r\\nHost: localhost\\r\\n\\r\\n' >&3 && grep -q '^ok' <&3"]

# The entrypoint runs `joedocs init` (idempotent), creates the first user
# on a store with none and prints how to sign in, then serves. Other
# arguments run as a command instead (docker/entrypoint.sh).
COPY --chmod=0755 docker/entrypoint.sh /usr/local/bin/docker-entrypoint.sh
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["serve"]

# A GitHub release's binary: dist/linux/amd64/joedocs or dist/linux/arm64/joedocs.
FROM runtime AS release
ARG TARGETARCH
COPY --chmod=0755 dist/linux/${TARGETARCH}/joedocs /usr/local/bin/joedocs

# Built from source; the default target, so it stays last.
FROM runtime AS local
COPY --from=build /usr/local/bin/joedocs /usr/local/bin/joedocs
