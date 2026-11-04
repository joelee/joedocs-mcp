# joedocs

JoeDocs: a central MCP server for multi-project documentation, stored as an Obsidian vault with git history and a SQLite index.

**Status:** scaffold only. The design and phased plan are in [docs/plan.md](docs/plan.md); phase 0 (configuration schema) is done, and the binary has only the `config` commands so far.

## Quick start

```bash
just setup          # once: cargo tools, Cargo.lock, git hooks
cargo run -- config show
just ci             # full quality gate, same as CI
```

Requires Linux or macOS with `rustup`, `just`, `gitleaks` and `pre-commit`. The toolchain version is pinned in `rust-toolchain.toml` and installed automatically by `rustup`.

## Usage

```text
joedocs [--config PATH] [--log-level LEVEL] [COMMAND]

Commands:
  config show    Show the effective configuration and where each value came from
  config paths   List the config file search order
```

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
