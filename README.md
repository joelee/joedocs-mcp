# joedocs

[![CI](https://github.com/joelee/joedocs-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/joelee/joedocs-mcp/actions/workflows/ci.yml)

JoeDocs: a central MCP server for multi-project documentation, stored as an Obsidian vault with git history and a SQLite index.

**Status:** phases 0 to 4 of [docs/plan.md](https://github.com/joelee/joedocs-mcp/blob/main/docs/plan.md) are done: configuration, the document store with git history and a SQLite index, the admin commands, the MCP server with fifteen document tools, full-text and front matter search, and history, diff, restore and in-place editing. Operations (deployment and backups) are phase 5.

## Install

```bash
cargo install joedocs --locked
```

This needs Rust 1.99 or newer and a C compiler (SQLite is built in). `joedocs` runs `git` at run time, so install it too. Linux and macOS are supported.

## Quick start

```bash
export JOEDOCS__STORAGE__HOME=/srv/joedocs/vault      # or set storage.home in config.toml
joedocs init
joedocs project add joedocs --key JD --title JoeDocs
joedocs key create laptop --grant joedocs:write       # prints the token once
joedocs serve                                          # http://127.0.0.1:8750/mcp
```

Run it as a systemd service instead, as you (add `--dry-run` to see the unit first; see [Running as a systemd service](https://github.com/joelee/joedocs-mcp/blob/main/docs/developer-guide.md#running-as-a-systemd-service)):

```bash
joedocs systemd install
```

Connect Claude Code (on the same machine as above; from elsewhere, through your TLS proxy or Tailscale name, which you add to `server.allowed_hosts`):

```bash
claude mcp add --transport http joedocs http://127.0.0.1:8750/mcp \
  --header "Authorization: Bearer jd_..."
```

## Development

From a clone of the repository, on Linux or macOS with `rustup`, `just`, `git`, `gitleaks` and `pre-commit` (the toolchain is pinned in `rust-toolchain.toml` and installed by `rustup`):

```bash
just setup          # once: cargo tools, Cargo.lock, git hooks
just ci             # full quality gate, same as CI
cargo run -- serve
just docker-up      # or a local Docker Compose stack; data persists in .local/data
```

See the [developer guide](https://github.com/joelee/joedocs-mcp/blob/main/docs/developer-guide.md) for Docker, testing and releasing.

## Usage

```text
joedocs [--config PATH] [--log-level LEVEL] [COMMAND]

Commands:
  config show                         Show the effective configuration and where each value came from
  config paths                        List the config file search order
  init                                Create the vault, state directory, database and history
  serve                               Run the MCP server on server.bind until Ctrl-C or SIGTERM
                                      (rebuilds the index first if it is behind the history)
  reindex                             Rebuild the document index and search from the history
  systemd install [--system --run-as USER] [--dry-run]
                                      Run serve as a systemd service with the current configuration
  systemd remove [--system] [--dry-run]
  archive list [--project NAME]       List archived documents
  archive restore DOCUMENT_ID         Bring an archived document back
  check                               Report drift between the vault, the history and the index
  project add NAME --key KEY [--title TITLE]
  project list
  key create NAME [--grant PROJECT:PERMISSION]...
  key list
  key grant NAME PROJECT:PERMISSION...
  key rotate NAME                     Print a new token; the old one stops working
  key revoke NAME
```

Exit codes: `0` success, `1` `check` found problems, `2` usage error, `65` invalid request (bad name, unknown project, not initialised), `69` cannot listen, `70` database, history or vault fault, `74` output error, `78` configuration error.

MCP tools:

- Reading: `list_projects`, `list_documents`, `search_documents`, `get_document`, `get_document_history`, `diff_document`.
- Writing: `new_document`, `update_document`, `edit_document` (exact replacements), `append_to_document`, `rename_document`, `move_document` (change type), `change_document_front_matter`, `restore_document_version`, `archive_document`.

`search_documents` takes a `query` (words, `"phrases"`, `-exclude`, `prefix*`, `a OR b`; word endings are ignored) and `filters` on any front matter key, for example `{"tags": {"contains": "security"}, "updated": {"gte": "2026-10-01"}}`, in one project or all of them (`"project": "*"`). See [plan.md section 7](https://github.com/joelee/joedocs-mcp/blob/main/docs/plan.md#7-search-decision-2).

Configuration comes from CLI flags, `JOEDOCS__SECTION__KEY` environment variables, a TOML config file, and built-in defaults, in that order of precedence. See [docs/configuration.md](https://github.com/joelee/joedocs-mcp/blob/main/docs/configuration.md).

## Project facts

| Item | Value |
| --- | --- |
| MSRV profile | application (see `AGENTS.md`) |
| CI platform | GitHub Actions ([`ci.yml`](https://github.com/joelee/joedocs-mcp/blob/main/.github/workflows/ci.yml): `just ci` on Linux and macOS, and `just docker-test`) |
| Licence | [Apache-2.0](https://github.com/joelee/joedocs-mcp/blob/main/LICENSE) |

## Documentation

- [Plan and design decisions](https://github.com/joelee/joedocs-mcp/blob/main/docs/plan.md)
- [Architecture](https://github.com/joelee/joedocs-mcp/blob/main/docs/architecture.md)
- [Configuration](https://github.com/joelee/joedocs-mcp/blob/main/docs/configuration.md)
- [Directory structure](https://github.com/joelee/joedocs-mcp/blob/main/docs/directory-structure.md)
- [Developer guide](https://github.com/joelee/joedocs-mcp/blob/main/docs/developer-guide.md)
- [Backlog](https://github.com/joelee/joedocs-mcp/blob/main/docs/backlog.md)
- [Changelog](https://github.com/joelee/joedocs-mcp/blob/main/CHANGELOG.md)
