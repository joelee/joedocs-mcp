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
