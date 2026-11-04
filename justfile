set shell := ["bash", "-euo", "pipefail", "-c"]

# MSRV read from Cargo.toml, used by the `msrv` recipe
msrv := `sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml`

# List available recipes
default:
    @just --list

# One-off setup: cargo tools, canonical Cargo.lock, git hooks (gitleaks and pre-commit come from your package manager)
setup:
    rustup show active-toolchain
    cargo install --locked cargo-nextest cargo-llvm-cov cargo-deny
    cargo fetch
    pre-commit install

# Download dependencies exactly as pinned in Cargo.lock
fetch:
    cargo fetch --locked

# Format all code
fmt:
    cargo fmt --all

# Fail if any code is not formatted
fmt-check:
    cargo fmt --all --check

# Clippy with every warning as an error
lint:
    cargo clippy --all-targets --all-features --locked -- -D warnings

# Run tests with nextest (set NEXTEST_PROFILE=ci in CI)
test:
    cargo nextest run --all-features --workspace --locked

# Run tests under coverage; fails below 80% line coverage
coverage:
    cargo llvm-cov nextest --all-features --workspace --locked --fail-under-lines 80

# Coverage report as HTML, opened in the browser
coverage-html:
    cargo llvm-cov nextest --all-features --workspace --locked --html --open

# Run doc-tests (nextest does not run them)
doctest:
    cargo test --doc --all-features --locked

# Build API docs with warnings as errors
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features --locked

# Supply-chain checks: advisories, licences, bans, sources
deny:
    cargo deny check

# Scan full git history (CI needs a full-depth checkout)
secrets:
    gitleaks git --redact .

# Scan only staged changes (pre-commit)
secrets-staged:
    gitleaks git --pre-commit --staged --redact .

# Optimised release build
build:
    cargo build --release --locked

# Check the declared MSRV builds (library profile; run `rustup toolchain install <msrv>` first)
msrv:
    cargo +{{msrv}} check --all-targets --all-features --locked

# Full gate, identical locally and in CI
ci: fetch fmt-check lint coverage doctest doc deny secrets build

# Build a throwaway Docker Compose stack, run the deployment tests against it before and after a restart, then remove it
docker-test port="18750":
    #!/usr/bin/env bash
    set -euo pipefail
    export COMPOSE_PROJECT_NAME=joedocs-test JOEDOCS_PORT={{port}}
    trap 'docker compose down --volumes --remove-orphans >/dev/null 2>&1' EXIT
    docker compose up --detach --build --wait
    docker compose exec -T joedocs joedocs project add itest --key IT --title "Integration tests"
    token="$(docker compose exec -T joedocs joedocs key create itest --grant itest:write | tail -n 1)"
    export JOEDOCS_TEST_URL="http://127.0.0.1:{{port}}" JOEDOCS_TEST_TOKEN="$token" JOEDOCS_TEST_PROJECT=itest
    JOEDOCS_TEST_PHASE=before-restart cargo nextest run --locked --test deployment --run-ignored only
    docker compose restart
    docker compose up --detach --wait
    JOEDOCS_TEST_PHASE=after-restart cargo nextest run --locked --test deployment --run-ignored only
