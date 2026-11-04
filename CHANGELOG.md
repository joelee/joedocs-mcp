# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Project generated from the Rust template: layered configuration, `config show` and `config paths` commands, and the `just ci` quality gate.
- JoeDocs design plan (`docs/plan.md`) and empty module skeleton for the server.
- Configuration sections `[server]` (`bind`, `allowed_origins`, `max_document_bytes`), `[storage]` (`home`, `state_dir`) and `[documents]` (`types`, `statuses`, `default_type`, `default_status`), with rule checks that name the offending key and the layer that set it.
- `LICENSE` file with the Apache License 2.0 text, matching the `license` field in `Cargo.toml`.

### Changed

- `deny.toml` allows `BSD-3-Clause`, needed by the approved axum and `serde-saphyr` stacks.
- Running `joedocs` with no command now prints help and exits with code 2.
- Renamed the crate and binary from `joedocs-mcp` to `joedocs`. The environment prefix is now `JOEDOCS` (for example `JOEDOCS__LOG__LEVEL` and `JOEDOCS_CONFIG_FILE`), and config files are looked up under `joedocs/` (for example `~/.config/joedocs/config.toml`).

### Removed

- The template's example `greet` command and `[greeting]` config section.
