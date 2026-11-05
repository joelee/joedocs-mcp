# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/). Versions number the `joedocs` command line, its configuration and its MCP tools; the Rust library inside the crate is internal and may change in any release.

## [Unreleased]

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

[Unreleased]: https://github.com/joelee/joedocs-mcp/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/joelee/joedocs-mcp/releases/tag/v0.1.0
