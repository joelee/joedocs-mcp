# Developer guide

## Setup

Install `rustup`, `just`, `git`, `gitleaks` and `pre-commit` with your package manager (on macOS: `brew install rustup just git gitleaks pre-commit`), then run:

```bash
just setup
```

This installs `cargo-nextest`, `cargo-llvm-cov` and `cargo-deny`, normalises `Cargo.lock`, and installs the git hooks. The history tests run the real `git` from `PATH`, isolated from your own git configuration. `rustup` installs the toolchain pinned in `rust-toolchain.toml` on first use. Commit `Cargo.lock` if `just setup` changed it.

## Workflow

1. Write or update a failing test (TDD).
2. Implement the change.
3. Update the documentation the change affects (see `docs/AGENTS.md`).
4. Run `just ci` and fix anything it reports.

| Command | Purpose |
| --- | --- |
| `just fmt` | Format code. |
| `just lint` | Clippy with warnings as errors. |
| `just test` | Run tests with nextest. |
| `just coverage` | Tests under coverage; fails below 80% lines. |
| `just coverage-html` | Browse the coverage report. |
| `just doctest` | Run doc-tests (nextest skips them). |
| `just doc` | Build API docs with warnings as errors. |
| `just deny` | Advisories, licences, bans and sources. |
| `just secrets` | Scan git history for secrets: gitleaks' built-in rules plus a `.gitleaks.toml` rule for JoeDocs API keys (checked by `tests/secrets.rs`). |
| `just msrv` | Check the declared MSRV builds (library profile). |
| `just docker-test [port]` | Build a throwaway Compose stack, run the deployment tests against it before and after a restart, then remove it (default port 18750). |
| `just ci` | Everything above, in CI order. |

## Testing

- Unit tests sit beside the code and use `MapEnv` and `MemFs` instead of the real environment and filesystem.
- Integration tests in `tests/cli.rs` run the binary with `env_clear()`, a temporary `HOME` and working directory, so your own config files and `.env` cannot affect results.
- Never call `std::env::set_var` or `remove_var`; they are `unsafe` in Rust 2024 and race with parallel tests.
- `tests/store.rs` and `tests/mcp.rs` drive the store and the MCP server in-process against a temporary vault and real git. `tests/common/` holds the shared MCP client helpers.
- `tests/history.rs` covers the phase 4 operations (history, diff, restore, move, edit, append, archive restore, `check`) and checks that each replays identically through `reindex`.
- `tests/search.rs` searches a fixture vault built through the store, and `tests/reindex.rs` checks that rebuilding the index from the history gives the same tables, recovers a lost write or database, and refuses a history it cannot replay.
- `tests/deployment.rs` is a black-box suite for a running server. It is ignored by `just test` and `just ci`; `just docker-test` runs it against Docker Compose, or set `JOEDOCS_TEST_URL`, `JOEDOCS_TEST_TOKEN` and `JOEDOCS_TEST_PROJECT` and run `cargo nextest run --test deployment --run-ignored only` against any server.

## Running with Docker

`compose.yaml` runs `joedocs serve` in a container. On start it runs `joedocs init` (which does nothing once initialised). The vault and the state are bind-mounted from `.local/data` (git-ignored), so they persist across rebuilds and you can inspect them on the host:

```text
.local/data/
├── vault/                Markdown files, one folder per project and type
└── state/
    ├── joedocs.db        SQLite index, projects and API keys
    ├── history.git/      git history (work tree: vault/)
    └── joedocs.lock
```

```bash
just docker-up                   # creates .local/data/{vault,state}, starts the stack as you
docker compose exec joedocs joedocs project add joedocs --key JD --title JoeDocs
docker compose exec joedocs joedocs key create laptop --grant joedocs:write   # last line is the token
claude mcp add --transport http joedocs http://127.0.0.1:8750/mcp \
  --header "Authorization: Bearer jd_..."
docker compose logs --follow
docker compose down              # stops the stack; the data stays in .local/data
```

- The port is published on loopback only: `JOEDOCS_PORT` (default `8750`). `JOEDOCS_LOG_LEVEL` sets the log level.
- Other settings go under `environment:` as `JOEDOCS__SECTION__KEY`, e.g. `JOEDOCS__SERVER__ALLOWED_HOSTS: '["localhost", "127.0.0.1", "docs.example.net"]'`, or mount a file at `/etc/joedocs/config.toml`.
- The server runs as `JOEDOCS_UID:JOEDOCS_GID` (default `1000:1000`; `just docker-up` sets your own), so the files in `.local/data` belong to you. If you run `docker compose up` yourself, create `.local/data/vault` and `.local/data/state` first, or Docker creates them owned by root and the server cannot write.
- `JOEDOCS_DATA` moves the data elsewhere (default `./.local/data`). `just docker-test` uses a temporary directory, so it never touches yours.
- Inspect the data on the host, for example `git --git-dir .local/data/state/history.git log --format='%ad %an | %s'` or `sqlite3 .local/data/state/joedocs.db`. Change it only through JoeDocs while the server runs: files edited by hand are refused for writes until resolved (`joedocs check` explains how).
- To start afresh: `docker compose down && rm -rf .local/data`.
- After an upgrade, `serve` rebuilds the index from the history if the database is from an older build; `docker compose logs` shows the outcome. To rebuild by hand: `docker compose exec joedocs joedocs reindex`.

## Running as a service

Running JoeDocs as a systemd or `brew services` service, reaching it from other machines, and operating it are in the [deployment guide](deployment.md). When you change `src/systemd.rs`, check the rendered unit with `cargo run -- systemd install --dry-run` (and `--system --run-as USER`).

## Debugging

- `cargo run -- --log-level debug` writes debug logs to stderr.
- `cargo run -- config show` explains where every config value came from.
- `cargo run -- config paths` shows which config file was picked.
- `cargo run -- reindex` rebuilds the database index from the history and lists vault files that differ from it.
- `cargo run -- check` reports drift between the vault, the history and the index without changing anything, and says how to resolve it.

## CI

CI runs on GitHub Actions ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) for pushes to `main` and for pull requests:

- `just ci` on Ubuntu and macOS, with full git history (for the gitleaks scan) and `NEXTEST_PROFILE=ci`; the JUnit report is uploaded as an artifact.
- `just docker-test`, which builds the image and runs the deployment tests against it.

Actions are pinned to commit SHAs and tools to exact versions (the `TOOLS` and `GITLEAKS_VERSION` variables, with gitleaks checksums per platform). Keep the tool versions in step with what `just setup` installs locally. Dependabot proposes weekly updates for actions and crates; every proposal runs CI, including `cargo deny`.

## Releasing

Versions number the command line, the configuration and the MCP tools; the Rust library in the crate is internal. A published version can never be replaced (only yanked), so check before publishing.

1. On a release branch, move the `Unreleased` entries in `CHANGELOG.md` under `## [X.Y.Z] - YYYY-MM-DD`, add the compare links at the bottom, and set `version` in `Cargo.toml`.
2. Check what would be published and that it builds on its own:

   ```bash
   cargo package --list --locked          # only src/, Cargo.*, README, LICENSE, CHANGELOG, config.example.toml
   cargo publish --dry-run --locked       # builds the packaged sources
   cargo install --path . --locked --root /tmp/joedocs-release && /tmp/joedocs-release/bin/joedocs --version
   ```

3. Run `just ci`, open a pull request, and merge it once CI passes.
4. From an up-to-date `main`, publish and tag:

   ```bash
   cargo publish --locked
   git tag -a vX.Y.Z -m "JoeDocs X.Y.Z"
   git push origin vX.Y.Z
   gh release create vX.Y.Z --title "JoeDocs X.Y.Z" --notes "<the CHANGELOG section>"
   ```

5. Update the Homebrew formula in the tap ([`joelee/homebrew-oss`](https://github.com/joelee/homebrew-oss)): set `url` in `Formula/joedocs.rb` to the new crate and `sha256` to its crates.io checksum, then open a pull request there; its macOS workflow runs `brew install --build-from-source`, `brew test` and `brew audit --strict`.

   ```bash
   curl -sL https://static.crates.io/crates/joedocs/joedocs-X.Y.Z.crate | shasum -a 256
   ```

`cargo publish` uses the token from `cargo login`: create it on crates.io with only the `publish-update` scope (and `publish-new` for the first release), limited to the `joedocs` crate, with an expiry date. Never commit it.
