# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/). Versions number the `joedocs` command line, its configuration and its MCP tools; the Rust library inside the crate is internal and may change in any release.

## [Unreleased]

## [0.3.0] - 2026-11-08

Run the admin commands from another machine, protected by an admin token and a TOTP code, and new commands to read a document, see where and how the server runs, brief a maintenance agent, and back up and restore.

### Added

- **Remote administration.** With `[admin] enabled = true` (off by default), `joedocs serve` also serves an admin API under `/admin/v1/`. On another machine, set `client.server` (or pass `--server URL`) and `JOEDOCS_ADMIN_TOKEN`, run `joedocs login` with a 6-digit code from Google Authenticator or another TOTP app, and `project`, `key`, `archive`, `check`, `reindex`, `status`, `get` and `backup` run on the server with the same output and exit codes. Sessions last `admin.session_hours` (8 by default; `JOEDOCS__ADMIN__SESSION_HOURS`). When one expires, a command at a terminal asks for a new code and runs once more; scripts get exit `77`. Five wrong codes lock a token for 15 minutes. Every admin call is audited. `joedocs logout` ends the session. Admin operations are still never MCP tools, and document API keys cannot use the admin API.
- **`joedocs admin-token create|list|revoke`**, on the server only: admin tokens (`jda_...`) with their TOTP secret, shown once as base32 and as an `otpauth://` URI.
- **`joedocs get DOCUMENT_ID`** prints a document, or only its body (`--content-only`) or front matter (`--front-matter-only`), at any revision (`--version N`), to standard output or a file (`-f`).
- **`joedocs status`** shows where the service runs (bind address, `/health`, the systemd unit and its PID, whether the admin API is on), the sizes of the vault, state directory, database and history, and the state of the store.
- **`joedocs agents`** prints an AGENTS.md-style guide for AI agents that maintain the server, filled in with this installation's paths and never with a secret.
- **`joedocs backup FILE.tgz`** writes a consistent backup (database, history and vault) while the server runs, and **`joedocs backup --restore`** restores one into empty directories, refusing damaged archives and unsafe entries.
- **[docs/upgrading.md](docs/upgrading.md)**: backing up, upgrading each kind of install, checking, going back, and notes for each version.
- Exit code `77` (admin credentials refused); `69` now also means the `client.server` cannot be reached.
- The secret scan recognises admin and session tokens (`jda_...`, `jds_...`).

### Security

- **The state directory is private.** `init` creates it `0700`, with the database and its `-wal` and `-shm` files `0600`, because it now holds admin TOTP secrets. Every start tightens an older install's state directory and database and logs each change; backups are staged in a private directory and written `0600`.
- **The admin client** never follows a redirect (a `3xx` exits `70` and names the target), gives up after 60 seconds without data from the server (exit `69`), refuses a reply that is not what the admin API sends (exit `70`, before printing anything or replacing a file), and never logs a `--code`.
- **The admin API** reads request bodies up to 64 KiB (`413` `payload_too_large` beyond), and audits every request, including refused, oversized and unknown ones.
- `get -f` writes through a randomly named temporary file created exclusively, so a file or link planted there is never followed, and keeps the destination's mode (a new file is `0600`).

### Upgrading

- Database migration 4 adds admin tokens and sessions: **JoeDocs 0.2.x cannot open the database afterwards**. Take a backup first; 0.2.x has no `backup` command, so stop the service and `tar` the vault and state directory, as [upgrading.md](docs/upgrading.md) describes.
- On its first start, 0.3.0 makes the state directory private and logs it; run admin commands as the account that owns it. Nothing else changes until you turn the admin API on.
- A remote backup no longer pauses the server while it compresses, and a restore that cannot open the restored store leaves its targets empty again, so it can be retried.

## [0.2.0] - 2026-11-07

Each project folder gets a generated `INDEX.md`, and releases are published from a signed tag by a GitHub Actions workflow.

### Added

- **`INDEX.md` in each project folder** (`<storage.home>/<project>/INDEX.md`): a table of the project's live documents sorted by ID, with each title linked to its file and its type, status and updated date, so a document can be found and opened by ID in Obsidian or any Markdown viewer. JoeDocs rewrites it after every change, never commits it to the history, and `check` treats the path as reserved. Turn it off with `documents.index_file = false` (`JOEDOCS__DOCUMENTS__INDEX_FILE`). After upgrading, `serve` writes the files when it starts (as does `joedocs reindex`); until then `check` reports them as missing.

## [0.1.2] - 2026-11-07

Three MCP tools stop writing empty revisions, and the deployment guide shows how to keep API keys out of client config files.

### Changed

- **No empty revisions.** `edit_document`, `update_document` and `change_document_front_matter` now refuse a call that would leave the document unchanged, with `invalid_argument` (field `edits`, `content` or `front_matter`), instead of writing a revision and a git commit that change nothing. This matches how `rename_document` and `move_document` refuse the current title or type. A stale `expected_revision` is still reported as `conflict` first. The skills in `docs/skills` say to treat this refusal as already done.
- The deployment guide shows how to connect OpenCode and Codex with the API key read from an environment variable (`JOEDOCS_API_KEY`), how to fill it from a password manager such as `pass`, and warns against keys in project-level client config.
- The `joedocs-planner` skill can rename a draft plan with `rename_document` when the user asks, then updates the plan's heading and change log to match.

## [0.1.1] - 2026-11-07

A documentation release: no change to the command line, the configuration, the MCP tools or what is stored.

### Added

- **Deployment guide** (`docs/deployment.md`): installing with Homebrew or Cargo, configuring a server, running it as a systemd user or system service on Linux or under `brew services` on macOS, reaching it over Tailscale or a TLS proxy, connecting Claude Code, upgrades, backups and troubleshooting.
- **Reference** (`docs/reference.md`): every command and exit code, every MCP tool with its parameters, error codes, the search syntax and filters, front matter rules and history trailers.
- **Agent skills** (`docs/skills`): four Claude Code skills that keep their documents in JoeDocs and make a delivery workflow: `joedocs-idea-architect` (pressure-tests ideas), `joedocs-planner` (traceable delivery plans), `joedocs-builder` (executes an approved plan step by step, with evidence in the plan's work log) and `joedocs-code-review` (evidence-backed reviews against the plan), plus the conventions they share and a list of skills to add.
- Homebrew installation: `brew install joelee/oss/joedocs`, with a `brew services` service.

### Changed

- The README now explains what JoeDocs is for, its features and use cases, and how to install it with Homebrew or Cargo, with a banner and diagrams of how the server and the skills fit together.

## [0.1.0] - 2026-11-05

First release: a central MCP server for multi-project documentation, stored as Markdown with YAML front matter, with git history and a SQLite index.

### Added

- **Documents.** Markdown files with server-managed YAML front matter, one folder per project and type, named after their titles, with stable IDs such as `JD-42`. Server keys (`id`, `title`, `type`, `revision`, `created*`, `updated*`, `client`) are set by JoeDocs; `status`, `tags` and `aliases` are validated; other keys are free.
- **History.** One git commit per revision, in a git directory outside the vault, authored by the API key's name, with `JoeDocs-*` trailers. Every revision can be read, compared and restored.
- **Index and search.** A SQLite index of projects, API keys, documents and revisions, with SQLite FTS5 full-text search and front matter filters. `joedocs reindex` rebuilds it from the history, and `joedocs serve` does so on start when it is behind.
- **Integrity guard.** A document whose file was changed outside JoeDocs is never overwritten; reads fall back to the committed revision, and `joedocs check` reports the drift.
- **MCP server.** `joedocs serve` speaks MCP Streamable HTTP on `/mcp`, with bearer API keys scoped per project (`read` or `write`), `Host` and `Origin` checks, `GET /health`, and an audit log of every change and refused change.
- **MCP tools.** Reading: `list_projects`, `list_documents`, `search_documents` (words, `"phrases"`, `-exclude`, `prefix*`, `OR`, and filters such as `{"tags": {"contains": "security"}}`, in one project or all), `get_document`, `get_document_history`, `diff_document`. Writing: `new_document`, `update_document` (with `expected_revision`), `edit_document` (exact replacements), `append_to_document`, `rename_document` (old title kept in `aliases`), `move_document`, `change_document_front_matter`, `restore_document_version`, `archive_document`. Tools have JSON schemas and return structured errors.
- **Admin commands.** `init`, `project add|list`, `key create|list|grant|rotate|revoke` (tokens are shown once and stored hashed), `reindex`, `archive list|restore`, `check`, `config show|paths`, and `systemd install|remove` (a user unit, or a sandboxed system unit, that runs `serve` with the configuration in effect).
- **Configuration.** Layered: flags, `JOEDOCS__SECTION__KEY` environment overrides, a TOML file (`~/.config/joedocs/config.toml` and others), and defaults. Sections `[log]`, `[server]`, `[storage]` and `[documents]`; see `config.example.toml`.
- **Exit codes.** `1` when `check` finds problems, `65` invalid request, `69` cannot listen or run systemd, `70` store fault, `74` output error, `78` configuration error.
- **Packaging.** A `Dockerfile` and `compose.yaml` for a local server, and GitHub Actions CI on Linux and macOS.

[Unreleased]: https://github.com/joelee/joedocs-mcp/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/joelee/joedocs-mcp/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/joelee/joedocs-mcp/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/joelee/joedocs-mcp/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/joelee/joedocs-mcp/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/joelee/joedocs-mcp/releases/tag/v0.1.0
