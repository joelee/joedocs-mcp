set shell := ["bash", "-euo", "pipefail", "-c"]

# MSRV read from Cargo.toml, used by the `msrv` recipe
msrv := `sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml`

# List available recipes
default:
    @just --list

# One-off setup: cargo tools, canonical Cargo.lock, the git hook in .githooks (gitleaks and actionlint come from your package manager)
setup:
    rustup show active-toolchain
    cargo install --locked cargo-nextest cargo-llvm-cov cargo-deny
    cargo fetch
    git config core.hooksPath .githooks

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

# shellcheck and pyflakes are off, so the result does not depend on what else
# is installed.
# Lint the GitHub Actions workflows
actionlint:
    actionlint -shellcheck= -pyflakes=

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

# The checks are the recipes `just ci` uses, so the hook has no versions of its
# own to pin. gitleaks always scans the staged changes; fmt and clippy run when
# a staged change (including a deletion or rename) touches a Rust file or the
# Cargo manifests, and actionlint when one touches the workflows. Git matches
# the pathspecs itself, so unusual file names need no parsing. fmt and clippy
# see the working tree, not only the index; CI checks the commit itself.
# Checks before each commit, run by .githooks/pre-commit (enabled by `just setup`)
pre-commit:
    #!/usr/bin/env bash
    set -euo pipefail
    # Exit status 0 means no staged change matches the pathspecs.
    staged() { ! git diff --cached --quiet -- "$@"; }
    check() {
        echo "pre-commit: $1"
        if ! "{{just_executable()}}" "$1"; then
            echo "pre-commit: '$1' failed; the commit was stopped" >&2
            exit 1
        fi
    }
    check secrets-staged
    if staged '*.rs' Cargo.toml Cargo.lock; then
        check fmt-check
        check lint
    fi
    if staged .github/workflows; then
        check actionlint
    fi

# Optimised release build
build:
    cargo build --release --locked

# Check the declared MSRV builds (library profile; run `rustup toolchain install <msrv>` first)
msrv:
    cargo +{{msrv}} check --all-targets --all-features --locked

# Full gate, identical locally and in CI
ci: fetch fmt-check lint actionlint coverage doctest doc deny secrets build

# Start the local Docker Compose stack as you, with its data in .local/data (or $JOEDOCS_DATA)
docker-up:
    #!/usr/bin/env bash
    set -euo pipefail
    data="${JOEDOCS_DATA:-./.local/data}"
    mkdir -p "$data/vault" "$data/state"
    export JOEDOCS_UID="$(id -u)" JOEDOCS_GID="$(id -g)"
    docker compose up --detach --build --wait
    echo "JoeDocs MCP endpoint: http://127.0.0.1:${JOEDOCS_PORT:-8750}/mcp (data in $data)"

# Build a throwaway Docker Compose stack, run the deployment tests against it before and after a restart, then remove it
docker-test port="18750":
    #!/usr/bin/env bash
    set -euo pipefail
    data="$(mktemp -d)"
    mkdir -p "$data/vault" "$data/state"
    export COMPOSE_PROJECT_NAME=joedocs-test JOEDOCS_PORT={{port}} JOEDOCS_DATA="$data" \
        JOEDOCS_UID="$(id -u)" JOEDOCS_GID="$(id -g)"
    trap 'docker compose down --remove-orphans >/dev/null 2>&1; rm -rf "$data"' EXIT
    docker compose up --detach --build --wait
    docker compose exec -T joedocs joedocs project add itest --key IT --title "Integration tests"
    token="$(docker compose exec -T joedocs joedocs key create itest --grant itest:write | tail -n 1)"
    export JOEDOCS_TEST_URL="http://127.0.0.1:{{port}}" JOEDOCS_TEST_TOKEN="$token" JOEDOCS_TEST_PROJECT=itest
    JOEDOCS_TEST_PHASE=before-restart cargo nextest run --locked --test deployment --run-ignored only
    docker compose restart
    docker compose up --detach --wait
    JOEDOCS_TEST_PHASE=after-restart cargo nextest run --locked --test deployment --run-ignored only

# A throwaway server from the working tree with sample data, kept in .local/scratch, for trying the web interface on other devices (args: fresh, --port N, --https-port N, --no-tailscale)
scratch-server *args:
    scripts/scratch-server.sh {{args}}
