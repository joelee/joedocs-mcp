# Developer guide

## Setup

Install `rustup`, `just`, `git`, `gitleaks` and `actionlint` with your package manager (on macOS: `brew install rustup just git gitleaks actionlint`), then run:

```bash
just setup
```

This installs `cargo-nextest`, `cargo-llvm-cov` and `cargo-deny`, normalises `Cargo.lock`, and turns on the git hook in `.githooks/` (`git config core.hooksPath .githooks`). Before each commit the hook runs `just pre-commit`: gitleaks on the staged changes, `cargo fmt --check` and clippy when Rust files or the Cargo manifests are staged, and `actionlint` when workflows are. A clone set up before 0.1.2 used the `pre-commit` framework; run `pre-commit uninstall` there if it is installed, then `just setup`. The history tests run the real `git` from `PATH`, isolated from your own git configuration. `rustup` installs the toolchain pinned in `rust-toolchain.toml` on first use. Commit `Cargo.lock` if `just setup` changed it.

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
| `just actionlint` | Lint the GitHub Actions workflows (shellcheck and pyflakes integrations off). |
| `just pre-commit` | The checks the git hook runs before each commit. |
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

The same workflow runs as a reusable workflow (`workflow_call`) from [`.github/workflows/release.yml`](../.github/workflows/release.yml) on every release tag, before anything is published (see Releasing).

Actions are pinned to commit SHAs and tools to exact versions (the `TOOLS`, `GITLEAKS_VERSION` and `ACTIONLINT_VERSION` variables, with gitleaks and actionlint checksums per platform). Keep the tool versions in step with what `just setup` installs locally. Dependabot proposes weekly updates for actions and crates; every proposal runs CI, including `cargo deny`.

## Releasing

Versions number the command line, the configuration and the MCP tools; the Rust library in the crate is internal. A published version can never be replaced (only yanked), so check before publishing.

Pushing a signed `vX.Y.Z` tag runs [`.github/workflows/release.yml`](../.github/workflows/release.yml). It checks the tag, runs the full CI gate on the tagged commit, waits for approval in the `release` environment, publishes to crates.io with Trusted Publishing (no stored token), asks the Homebrew tap to bump the formula, and creates the GitHub release from the CHANGELOG section.

### One-time setup

1. **crates.io:** in the `joedocs` crate's Settings → Trusted Publishing, add a GitHub publisher: owner `joelee`, repository `joedocs-mcp`, workflow `release.yml`, environment `release`. After the first automated release, revoke the old `cargo login` token on crates.io.
2. **GitHub environment:** Settings → Environments → `release`, with yourself as required reviewer and deployments limited to `v*` tags.
3. **Homebrew tap:** finish the tap's setup ([`docs-release.md`](https://github.com/joelee/homebrew-oss/blob/main/docs-release.md), parts 1 and 2), then add to this repository the variable `TAP_BOT_APP_ID` and, in the `release` environment, the secret `TAP_BOT_PRIVATE_KEY`. Without them the release still publishes, and the tap's daily check picks up the new version.
4. **Signing key:** release tags are signed (`git tag -s`), so git needs a signing key (`user.signingkey`, GPG or SSH with `gpg.format`), and the public key must be on your GitHub account, because the workflow refuses a tag that GitHub does not report as verified.

### Each release

1. On a release branch, move the `Unreleased` entries in `CHANGELOG.md` under `## [X.Y.Z] - YYYY-MM-DD`, add the compare links at the bottom, and set `version` in `Cargo.toml`.
2. Check what would be published and that it builds on its own:

   ```bash
   cargo package --list --locked          # only src/, Cargo.*, README, LICENSE, CHANGELOG, config.example.toml
   cargo publish --dry-run --locked       # builds the packaged sources
   cargo install --path . --locked --root /tmp/joedocs-release && /tmp/joedocs-release/bin/joedocs --version
   ```

3. Run `just ci`, open a pull request, and merge it once CI passes.
4. Optionally, dry-run the release workflow from `main`. It runs the checks, CI and `cargo publish --dry-run`, and never publishes:

   ```bash
   gh workflow run release.yml -f dry_run=true
   ```

5. From an up-to-date `main`, sign, check and push the tag:

   ```bash
   git tag -s vX.Y.Z -m "JoeDocs X.Y.Z"
   git tag -v vX.Y.Z                  # check the signature before pushing
   git push origin vX.Y.Z
   ```

6. In the Actions tab, open the Release run and approve the `release` deployment once the checks and CI pass.
7. Check the result: the version on crates.io, the GitHub release, and the tap's `joedocs X.Y.Z` pull request, which merges by itself once the tap's tests pass.

### When the release fails

- **Before publishing** (a check, CI, or the approval was rejected): nothing was published. Fix the problem on `main`, then move the tag: `git tag -d vX.Y.Z`, `git push origin :refs/tags/vX.Y.Z`, and tag and push again.
- **After publishing:** the version is on crates.io and cannot be replaced. If only the GitHub release or the Homebrew dispatch failed, rerun the failed job, or finish it by hand (below). Fix anything else forward with a new patch version.

### Releasing by hand

If the workflow cannot be used, publish from an up-to-date `main` with a crates.io token from `cargo login` (scope `publish-update`, limited to the `joedocs` crate, with an expiry date; never commit it):

```bash
cargo publish --locked
git tag -s vX.Y.Z -m "JoeDocs X.Y.Z"
git tag -v vX.Y.Z
git push origin vX.Y.Z
gh release create vX.Y.Z --title "JoeDocs X.Y.Z" \
  --notes-file <(awk -v heading="## [X.Y.Z]" 'index($0, heading) == 1 { found = 1; next } /^## \[/ { found = 0 } found' CHANGELOG.md)
gh workflow run bump-formula.yml -R joelee/homebrew-oss -f formula=joedocs -f version=X.Y.Z
```

Pushing the tag also starts the release workflow; reject its `release` deployment, since the crate is already published.
