# joedocs

JoeDocs: a central MCP server for multi-project documentation, stored as an Obsidian vault with git history and a SQLite index.

**Status:** phases 0 to 2 of [docs/plan.md](docs/plan.md) are done: configuration, the document store with git history and a SQLite index, the admin commands, and the MCP server with eight document tools. Search (`search_documents`) is phase 3.

## Quick start

```bash
just setup          # once: cargo tools, Cargo.lock, git hooks
just ci             # full quality gate, same as CI

export JOEDOCS__STORAGE__HOME=/srv/joedocs/vault
cargo run -- init
cargo run -- project add joedocs --key JD --title JoeDocs
cargo run -- key create laptop --grant joedocs:write   # prints the token once
cargo run -- serve                                     # http://127.0.0.1:8750/mcp
```

Or run it in Docker (see [Running with Docker](docs/developer-guide.md#running-with-docker)):

```bash
docker compose up --detach --build --wait
docker compose exec joedocs joedocs project add joedocs --key JD --title JoeDocs
docker compose exec joedocs joedocs key create laptop --grant joedocs:write
```

Connect Claude Code (through your TLS proxy or Tailscale name; add that host to `server.allowed_hosts`):

```bash
claude mcp add --transport http joedocs https://docs.example.net/mcp \
  --header "Authorization: Bearer jd_..."
```

Requires Linux or macOS with `rustup`, `just`, `git`, `gitleaks` and `pre-commit`. The toolchain version is pinned in `rust-toolchain.toml` and installed automatically by `rustup`.

## Usage

```text
joedocs [--config PATH] [--log-level LEVEL] [COMMAND]

Commands:
  config show                         Show the effective configuration and where each value came from
  config paths                        List the config file search order
  init                                Create the vault, state directory, database and history
  serve                               Run the MCP server on server.bind until Ctrl-C or SIGTERM
  project add NAME --key KEY [--title TITLE]
  project list
  key create NAME [--grant PROJECT:PERMISSION]...
  key list
  key grant NAME PROJECT:PERMISSION...
  key revoke NAME
```

Exit codes: `0` success, `2` usage error, `65` invalid request (bad name, unknown project, not initialised), `69` cannot listen, `70` database, history or vault fault, `74` output error, `78` configuration error.

MCP tools: `list_projects`, `list_documents`, `get_document`, `new_document`, `update_document`, `rename_document`, `change_document_front_matter`, `archive_document`.

Configuration comes from CLI flags, `JOEDOCS__SECTION__KEY` environment variables, a TOML config file, and built-in defaults, in that order of precedence. See [docs/configuration.md](docs/configuration.md).

## Project facts

| Item | Value |
| --- | --- |
| MSRV profile | application (see `AGENTS.md`) |
| CI platform | _decide per project; the pipeline runs `just ci`_ |
| Licence | [Apache-2.0](LICENSE) |

## Documentation

- [Plan and design decisions](docs/plan.md)
- [Architecture](docs/architecture.md)
- [Configuration](docs/configuration.md)
- [Directory structure](docs/directory-structure.md)
- [Developer guide](docs/developer-guide.md)
- [Backlog](docs/backlog.md)
- [Changelog](CHANGELOG.md)
