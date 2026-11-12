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
- `tests/admin_api.rs` drives the admin API over raw HTTP with a `FixedClock` (expiry, lockout) and checks the audit rows; `tests/client.rs` runs the binary as a client of an in-process server on its own thread (the client runs its own runtime), compares remote and local output command by command, and drives the re-login prompt through the library with a scripted `Prompt`. TOTP codes in tests come from the token's secret and the server's clock (`joedocs::totp::code`).
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

## Web interface

The admin web interface lives in `src/web/`, with its Askama templates in `templates/` at the repository root (Askama's default directory; Askama's `config` feature, which would let them live elsewhere, depends on the unmaintained `basic-toml`). The templates and the static files in `src/web/assets/` are compiled into the binary; `Cargo.toml` `include`, the `Dockerfile` and `.dockerignore` list `templates/`. There is no Node or WASM build step.

### Editing pages and styles

- **Templates** in `templates/` are checked and compiled by `cargo build`; a template error is a compile error. Each page module in `src/web/pages/` owns a `#[derive(Template)]` struct for its template. Askama escapes every value; use `|safe` only for HTML the code wrote itself from escaped parts (the Markdown renderer's output, a rendered fragment).
- **No inline script or style.** The Content-Security-Policy allows no inline script, and no inline style except on pages with a Mermaid diagram. Behaviour goes in `src/web/assets/app.js` as event listeners (never `eval`, `hx-on` or `js:` values); styles go in `app.css`, with colours as the design tokens at its top (`light-dark()` pairs, at least 4.5:1 contrast in both themes). `tests/web.rs` checks every page for one `<h1>`, labelled controls, table headers, image `alt`, and no `style`, `on...` or inline `<script>`.
- **Dialogs** open with invoker commands (`commandfor` and `command="show-modal"`) and close with a `formmethod="dialog"` button, so they work without JavaScript.
- **Routes.** A new route is added to `Route` in `src/web/mod.rs` with its audit tool (the matching admin API operation), and to the route table in that module's documentation; a test requests every route in the table and checks its audit row. Every route but the sign-in page needs a session by default.
- **Static files** are served under `/admin/assets/<hash>/<name>`; the hash changes with the content, so browsers never keep an old copy.
- **Trying it.** `JOEDOCS__ADMIN__ENABLED=true JOEDOCS__ADMIN__WEB_UI=true cargo run -- serve` with a scratch `JOEDOCS__STORAGE__HOME` and `JOEDOCS__STORAGE__STATE_DIR`, a user from `cargo run -- user create dev` on the same store (its one-time password is the last line), then `http://127.0.0.1:8750/admin/`, where the first sign-in sets up TOTP.

### Vendored browser files

Third-party browser files sit in `src/web/assets/vendor/`, unmodified, each with its licence text beside it. To update one, download the new version from the npm registry, check the tarball against the registry's `integrity` value, copy the file and its licence, and update this table.

| File | Version | Source | SHA-256 | Licence |
| --- | --- | --- | --- | --- |
| `htmx.min.js` | htmx 4.0.1 | `dist/htmx.min.js` from <https://registry.npmjs.org/htmx.org/-/htmx.org-4.0.1.tgz> (integrity `sha512-oF7JgXhDW9UXCy/k1ZYFUQgzu/rm19ORWPSPNL91vPluB2PB9+MV55tyQdH/w7o2EAbTYUG5SDW1R60mdkdqGg==`) | `a0abb04f2f45a1f89d1d4844023245537e5e6014c87b144553e0607b4cc32130` | 0BSD, `htmx.LICENSE` |
| `pele.global.js` | Pele 1.13.0 (Mermaid diagrams) | `dist/pele.global.js` from <https://registry.npmjs.org/pele/-/pele-1.13.0.tgz> (integrity `sha512-Ie8UKY9x00MsuR3jO6AzUwzK2Z6MIV3Tf4EDdITiuk8Uaz8K7g0R082WqsqNaSU8DdgkrRvZThzhit83mi/uiw==`) | `0b0a46c57a5a6971670c09f5ec4066705bd78505f90fd11adf6260325efa0910` | MIT, with the Lucide ISC notice, `pele.LICENSE` |

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

Pushing a signed `vX.Y.Z` tag runs [`.github/workflows/release.yml`](../.github/workflows/release.yml). It checks the tag, then runs the full CI gate on the tagged commit and, alongside it, builds the release binaries: Linux x86_64 and aarch64 (static, musl) and macOS Apple silicon. Each is checked (`--version`, and on Linux no shared libraries) and kept as a workflow artifact. The workflow then waits for approval in the `release` environment, publishes to crates.io with Trusted Publishing (no stored token), and asks the Homebrew tap to bump the formula. Last, it creates the GitHub release from the CHANGELOG section, with the three `joedocs-X.Y.Z-<target>.tar.gz` archives, their `SHA256SUMS`, and a build-provenance attestation for each archive. Release builds use no cache.

### One-time setup

1. **crates.io:** in the `joedocs` crate's Settings → Trusted Publishing, add a GitHub publisher: owner `joelee`, repository `joedocs-mcp`, workflow `release.yml`, environment `release`. After the first automated release, revoke the old `cargo login` token on crates.io.
2. **GitHub environment:** Settings → Environments → `release`, with yourself as required reviewer and deployments limited to `v*` tags.
3. **Homebrew tap:** finish the tap's setup ([`docs-release.md`](https://github.com/joelee/homebrew-oss/blob/main/docs-release.md), parts 1 and 2), then add to this repository the variable `TAP_BOT_CLIENT_ID` (the App's client ID, shown on its settings page) and, in the `release` environment, the secret `TAP_BOT_PRIVATE_KEY`. Without them the release still publishes, and the tap's daily check picks up the new version.
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
4. Dry-run the release workflow, from `main` or, before merging, from the pushed release branch. It runs the checks, CI, the binary builds and `cargo publish --dry-run`, and never publishes, releases or attests. Do this whenever the workflow changed:

   ```bash
   gh workflow run release.yml -f dry_run=true                 # from main
   gh workflow run release.yml --ref <branch> -f dry_run=true  # from a branch
   ```

5. From an up-to-date `main`, sign, check and push the tag:

   ```bash
   git tag -s vX.Y.Z -m "JoeDocs X.Y.Z"
   git tag -v vX.Y.Z                  # check the signature before pushing
   git push origin vX.Y.Z
   ```

6. In the Actions tab, open the Release run and approve the `release` deployment once the checks and CI pass.
7. Check the result:
   - the version on crates.io;
   - the GitHub release, with the three archives and `SHA256SUMS`, checked with `gh release download vX.Y.Z -p '*' -D /tmp/jd-X.Y.Z && (cd /tmp/jd-X.Y.Z && shasum -a 256 -c SHA256SUMS)` and `gh attestation verify /tmp/jd-X.Y.Z/joedocs-X.Y.Z-<target>.tar.gz -R joelee/joedocs-mcp`;
   - the tap's `joedocs X.Y.Z` pull request, which merges by itself once the tap's tests pass.

### When the release fails

- **Before publishing** (a check, CI, a binary build, or the approval was rejected): nothing was published. Fix the problem on `main`, then move the tag: `git tag -d vX.Y.Z`, `git push origin :refs/tags/vX.Y.Z`, and tag and push again.
- **After publishing:** the version is on crates.io and cannot be replaced, so never rerun the Publish job: `cargo publish` fails for a version that already exists.
  - If the GitHub release failed (including its checksums or attestations), rerun only the GitHub release job: it takes the binaries from the run's artifacts. Creating the release by hand (below) leaves out the binaries.
  - If the Homebrew dispatch failed, the tap's daily check bumps the formula by itself; to bump it now, run `gh workflow run bump-formula.yml -R joelee/homebrew-oss -f formula=joedocs -f version=X.Y.Z`.
  - Fix anything else forward with a new patch version.

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

Pushing the tag also starts the release workflow; reject its `release` deployment, since the crate is already published. A release made by hand has no binaries.
