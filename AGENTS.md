# AGENTS.md — joedocs

JoeDocs: a central MCP server for multi-project documentation, stored as an Obsidian vault with git history and a SQLite index.

This Rust project targets Linux and macOS and was generated from the team's cargo-generate Rust template. Keep it small, explicit, and deterministic. `CLAUDE.md` imports this file (`@AGENTS.md`), so keep instructions here; point other tools that use their own filename at this file too.

## Commands (run these before handing work back)

| Task | Command |
| --- | --- |
| List recipes | `just` |
| One-off setup (tools, lockfile, hooks) | `just setup` |
| Format | `just fmt` |
| Full local gate (same as CI) | `just ci` |
| Lint only | `just lint` |
| Tests only (nextest) | `just test` |
| Coverage (fails under 80% lines) | `just coverage` |
| Secret scan of staged changes | `just secrets-staged` |
| Check the MSRV builds (library profile) | `just msrv` |
| Start the local Docker Compose stack (data in `.local/data`) | `just docker-up` |
| Deployment tests against Docker Compose (needs Docker) | `just docker-test` |

`just ci` runs, in order: `fetch` → `fmt-check` → `lint` → `coverage` → `doctest` → `doc` → `deny` → `secrets` → `build`. Never weaken a gate (threshold, lint level, `--locked`) to make it pass.

## Toolchain And Tools

- The toolchain is pinned in `rust-toolchain.toml` (channel, plus `rustfmt`, `clippy`, `llvm-tools`). Do not change it unless the task is a toolchain bump.
- Edition: Rust 2024. Resolver 3 is implied by the edition; do not set an older resolver.
- `Cargo.lock` is committed. All CI commands use `--locked`.
- Required external tools: `just`, `cargo-nextest`, `cargo-llvm-cov`, `cargo-deny`, `gitleaks`, `pre-commit`, and `git` (the history store runs it, and so do the tests).
- Optional per project: `cargo-hack` (if the crate has Cargo features, to check the feature powerset), `cargo-semver-checks` (for published libraries), `insta` (snapshot tests for CLI output).

### MSRV policy

The MSRV is declared in `Cargo.toml` as `rust-version`. Do not use APIs newer than it, and do not raise it without asking. This project was generated with the **application** profile, recorded in `README.md`:

- **Application profile (default):** `rust-version` equals the pinned toolchain in `rust-toolchain.toml`; bump both together in one change. No separate MSRV CI job is needed.
- **Library profile:** `rust-version` is set below the pinned toolchain (typically latest stable minus 2, never below 1.85, which Edition 2024 requires). CI runs an extra MSRV job (`just msrv`, which runs `cargo check` on the `rust-version` toolchain). Raising the MSRV is recorded in `CHANGELOG.md`.

## Project Standards

- Layout: `src/lib.rs` holds all logic; `src/main.rs` is a thin wrapper that parses args, calls the library, and maps errors to exit codes. Integration tests live in `tests/`. See `docs/directory-structure.md` for the module map.
- CLI parsing: `clap` with the derive API.
- Configuration: `serde` + `toml` for non-secret TOML; config structs use `#[serde(deny_unknown_fields)]`.
- Errors: `thiserror` for library error types; `anyhow` only in `main.rs`. No `unwrap()`/`expect()`/`panic!` in non-test code (enforced by clippy).
- Logging: `tracing` + `tracing-subscriber`; never `println!` for diagnostics. Never log secrets or whole config structs that may contain them.
- `unsafe` is forbidden (`unsafe_code = "forbid"` in `[lints]`). If a task genuinely needs it, stop and ask.
- Lints are configured in the `[lints]` table of `Cargo.toml`, not scattered `#![deny]` attributes. To silence a lint locally, use `#[expect(lint_name, reason = "...")]`, never a bare `#[allow]`.
- Formatting: `rustfmt` defaults (with `rustfmt.toml` if present). Linting: `clippy` with `-D warnings` in CI.

### Rust 2024 edition rules agents commonly get wrong

- `std::env::set_var` and `remove_var` are `unsafe` and racy in multithreaded tests. Do not use them anywhere; inject environment lookups instead (see Configuration and Testing).
- `extern` blocks must be `unsafe extern`; attributes like `no_mangle` must be written `#[unsafe(no_mangle)]`.
- `gen` is a reserved keyword. References to `static mut` are denied.

## Dependencies

- Prefer the standard library and the crates already in use. Adding a new dependency requires a one-line justification in the change summary.
- Use `default-features = false` and enable only what is needed when a crate has heavy defaults.
- `cargo deny check` (config in `deny.toml`) must pass. The licence allow-list, banned crates, and allowed sources are set per project in `deny.toml`; never edit them without asking.

## Security

- Secrets come from environment variables. `.env` is a local-development convenience only, loaded with `dotenvy`; real environment variables always take precedence over `.env`.
- `.env` is git-ignored. `.env.example` lists every supported secret variable with safe placeholder values and is kept in sync.
- Secrets are not part of the `config.toml` schema and cannot be set through config env overrides. Hold them in `secrecy::SecretString` (or equivalent) so they cannot leak via `Debug` or logs.
- Never commit tokens, passwords, API keys, private keys, or secret-bearing connection strings.

## Configuration

Effective configuration is built from four layers, highest precedence first:

1. CLI flags (only for keys that have a flag)
2. Per-key environment overrides: `JOEDOCS__<SECTION>__<KEY>`
3. The selected config file (see below)
4. Built-in defaults (the `Default` impl), so the app runs with no config file

### Config file selection

The first source that exists wins; files are never merged.

1. `--config <path>` (error if the file does not exist)
2. `JOEDOCS_CONFIG_FILE` (error if the file does not exist; empty counts as unset)
3. `$XDG_CONFIG_HOME/joedocs/config.toml` (only if `XDG_CONFIG_HOME` is set to an absolute path)
4. `$HOME/.config/joedocs/config.toml` (used on macOS too; do not use `~/Library`)
5. `/etc/joedocs/config.toml`
6. `./config.toml`
7. None found: defaults plus overrides only

### Environment override rules

- Prefix is `JOEDOCS__` (double underscore). Each further `__` separates a table level. Names are upper case; snake_case keys keep their single underscores. Example: `[server] max_document_bytes` → `JOEDOCS__SERVER__MAX_DOCUMENT_BYTES`.
- `JOEDOCS_CONFIG_FILE` and secrets such as `JOEDOCS_API_TOKEN` (single underscore) are not overrides.
- Values are parsed as the target field's type: booleans, integers and floats use TOML scalar syntax, strings are taken literally, arrays use TOML inline-array syntax (`["a", "b"]`).
- An unknown override path, an unparseable value, or an empty value for a non-string field is a startup error that names the variable.
- Every config key's override variable name is listed in `docs/configuration.md` alongside its type, default, and example.

### Implementation

Resolution is implemented in-house in `src/config/resolve.rs` as a pure function of the CLI values, an injected `Env` (`src/env.rs`) and an injected `FileSystem` (`src/fs.rs`). Read `HOME` and `XDG_CONFIG_HOME` through `Env`, never directly. Do not adopt config crates whose test helpers mutate the real process environment. `joedocs config show` prints the effective config with the layer that supplied each key; `joedocs config paths` prints the search order.

To add a config key: add the field with a default to the schema in `src/config/mod.rs` (no `Option` fields, so every key has a type for overrides), add it to `config.example.toml` (a test fails if the two drift), add a CLI flag in `src/cli.rs` only if needed, and document it in `docs/configuration.md`.

## Engineering Workflow

- Follow TDD: write or update a failing test before changing behaviour.
- Keep changes scoped to the active request. Do not refactor unrelated code or reformat untouched files.
- Do not overwrite user work. If the tree has unrelated changes, leave them alone.
- Update documentation in the same change that makes it true.
- Run `just ci` before handing work back and report any gate you could not run.

### Testing

- Tests run under `cargo-nextest` (config in `.config/nextest.toml`). Each test runs in its own process, so tests must not rely on shared in-process state or execution order.
- nextest does not run doc-tests; `just doctest` runs them with `cargo test --doc`. Doc-tests are not counted toward coverage on stable.
- Unit tests sit next to the code in `#[cfg(test)] mod tests`. External interfaces (filesystem, network, clock, process environment, subprocesses) sit behind traits or injected parameters; unit tests use in-memory fakes. Prefer hand-written fakes; `mockall` is acceptable when a fake would be unwieldy.
- Integration tests in `tests/` drive the binary with `assert_cmd` + `predicates`, pass environment explicitly with `Command::env`/`env_clear`, use `tempfile` for filesystem state, and never hit the real network. After `env_clear`, re-add `LLVM_PROFILE_FILE` (see the `cmd` helper in `tests/cli.rs`) or coverage loses the child process.
- Line coverage stays at or above 80% (`cargo llvm-cov nextest`).
- `unwrap()`/`expect()` are allowed inside `#[test]` functions (`clippy.toml`). Helper functions in `tests/` are not covered by that allowance, so they return `Result` and the test unwraps.

## Ask Before

- Adding `unsafe`, a build script (`build.rs`), or a dependency that needs a `deny.toml` change.
- Raising the MSRV or changing `rust-toolchain.toml`.
- Changing CI thresholds, lint levels, `deny.toml`, `.config/nextest.toml`, or `.gitleaks.toml` allowlists.
- Deleting files, or a breaking change to a stable interface (see Stable Interfaces).

## Stable Interfaces

`joedocs` is published on crates.io, and its version (SemVer) covers these stable interfaces:

- the command line: commands, flags, exit codes, and output that scripts read (such as the token alone on the last line of `key create` and `key rotate`);
- the configuration: keys, defaults, `JOEDOCS__SECTION__KEY` overrides, and the config file lookup;
- the MCP tools: names, parameters, results and error codes;
- what is stored: the vault layout, front matter keys, history commit trailers, and the database (changed only by new migrations).

The Rust library in the crate is internal; its API may change in any release. Record every user-visible change in `CHANGELOG.md`.

## CI/CD

The CI platform is GitHub Actions (`.github/workflows/ci.yml`, with action updates from `.github/dependabot.yml`). Every pipeline is a thin wrapper that installs the required tools and runs `just ci` on the pinned toolchain with `NEXTEST_PROFILE=ci`. Library-profile projects add the `just msrv` job. The secret scan uses `gitleaks git` against full history, so CI must check out with full depth. Pre-commit runs `gitleaks git --pre-commit --staged --redact`.

## Documentation

Follow `docs/AGENTS.md` for documentation rules. The maintained set is:

- `README.md` (including the chosen MSRV profile and CI platform)
- `CHANGELOG.md` (Keep a Changelog format)
- `docs/deployment.md`
- `docs/reference.md`
- `docs/skills/` (agent skills that use JoeDocs)
- `docs/architecture.md`
- `docs/configuration.md`
- `docs/directory-structure.md`
- `docs/developer-guide.md`
- `docs/backlog.md`
