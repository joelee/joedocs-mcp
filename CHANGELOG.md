# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Project generated from the Rust template: layered configuration, `config show` and `config paths` commands, and the `just ci` quality gate.
- JoeDocs design plan (`docs/plan.md`) and empty module skeleton for the server.
- Configuration sections `[server]` (`bind`, `allowed_origins`, `max_document_bytes`), `[storage]` (`home`, `state_dir`) and `[documents]` (`types`, `statuses`, `default_type`, `default_status`), with rule checks that name the offending key and the layer that set it.
- `LICENSE` file with the Apache License 2.0 text, matching the `license` field in `Cargo.toml`.
- Document store (phase 1): create, get (latest or any revision), update with `expected_revision`, rename (old title kept in `aliases`), change front matter (set and remove, with reserved-key rules), archive and list, over Markdown files with YAML front matter.
- Git history with one commit per revision and `JoeDocs-*` trailers, kept in a git directory outside the vault, and a SQLite index of projects, API keys, documents and revisions.
- Integrity guard: a document whose file was changed outside JoeDocs is never overwritten; reads fall back to the committed revision.
- Commands `init`, `project add|list` and `key create|list|grant|revoke`. API keys are `jd_<id>_<secret>` and only a hash of the secret is stored.
- Exit codes `65` (invalid request) and `70` (store fault).
- MCP server (phase 2): `joedocs serve` speaks MCP Streamable HTTP on `/mcp` with bearer API keys, `Host` and `Origin` checks, and `GET /health`. Tools: `list_projects`, `list_documents`, `get_document`, `new_document`, `update_document`, `rename_document`, `change_document_front_matter`, `archive_document`, with JSON schemas, per-project grant checks and structured errors.
- Audit log of every change and refused change made through MCP.
- `Dockerfile` and `compose.yaml` to run `joedocs serve` locally, with the vault, database and history bind-mounted from `.local/data` (git-ignored) and the server running as your user; `just docker-up` starts it.
- Deployment tests (`tests/deployment.rs`) for a running server, and `just docker-test`, which runs them against a throwaway Compose stack before and after a restart.
- Config key `server.allowed_hosts`; exit code `69` when the server cannot listen.
- Search (phase 3): the `search_documents` MCP tool finds documents by text (words, `"phrases"`, `-exclude`, `prefix*`, `OR`, ignoring word endings and accents) and by front matter filters (`eq`, `ne`, `in`, `contains`, `gt`, `gte`, `lt`, `lte`, `exists`) in one project or every readable one, with relevance scores and snippets. Built on SQLite FTS5; the index is kept current by every write.
- `joedocs reindex` rebuilds the document index and search from the git history, recreates projects missing from the database, and lists vault files that differ from the history. `joedocs serve` runs it on start when the index is behind the history or from an older build.
- History and editing tools (phase 4): `get_document_history`, `diff_document` (unified diff between any two revisions), `restore_document_version`, `move_document` (change type), `edit_document` (exact replacements in the body) and `append_to_document`.
- Command `systemd install|remove`: a user unit (or, with `--system --run-as USER`, a sandboxed system unit) that runs `joedocs serve` with the configuration in effect at install time; `--dry-run` prints the unit and the commands.
- GitHub Actions CI: `just ci` on Linux and macOS and `just docker-test`, with pinned actions and tools, and Dependabot updates.
- Command `key rotate <name>`: prints a new token for the key and invalidates the old one; the name, ID and grants stay.
- Commands `archive list [--project]` and `archive restore <id>`, and `check`, which reports vault files that differ from the history or belong to no document, and exits `1` when it finds problems.
- Dependencies: `rmcp`, `axum`, `tokio`, `tokio-util` (MCP server).
- Dependencies: `rusqlite` (bundled SQLite), `serde-saphyr` (YAML), `serde_json`, `indexmap` (ordered front matter), `jiff` (timestamps), `sha2` and `getrandom` (API keys).

### Changed

- `deny.toml` allows `BSD-3-Clause`, needed by the approved axum and `serde-saphyr` stacks.
- Running `joedocs` with no command now prints help and exits with code 2.
- Renamed the crate and binary from `joedocs-mcp` to `joedocs`. The environment prefix is now `JOEDOCS` (for example `JOEDOCS__LOG__LEVEL` and `JOEDOCS_CONFIG_FILE`), and config files are looked up under `joedocs/` (for example `~/.config/joedocs/config.toml`).

### Fixed

- On case-insensitive file systems (macOS), renaming a document to a title that differs only in case now records the new file name in the history; before, the next change to it failed with "nothing to commit".
- History commits now carry the revision time. Before, every commit made by the store had a 1970 author and committer date.

### Removed

- The template's example `greet` command and `[greeting]` config section.
