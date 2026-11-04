# Directory structure

```text
.
├── AGENTS.md               Instructions for coding agents
├── CLAUDE.md               Imports AGENTS.md for Claude Code
├── Dockerfile              Two-stage image: build, then debian-slim with git
├── compose.yaml            Local server with vault and state volumes
├── .dockerignore           Sends only Cargo.toml, Cargo.lock and src/ to the build
├── CHANGELOG.md            Keep a Changelog
├── Cargo.toml              Manifest, MSRV, lint levels
├── Cargo.lock              Committed; CI uses --locked
├── LICENSE                 Apache License 2.0
├── README.md
├── clippy.toml             Test allowances for restriction lints
├── config.example.toml     Every config key with its default (checked by a test)
├── deny.toml               cargo-deny: advisories, licences, bans, sources
├── justfile                Local and CI entry points
├── rust-toolchain.toml     Pinned toolchain and components
├── .config/nextest.toml    Test runner profiles (default, ci)
├── .env.example            Supported secret variables with placeholders
├── .gitleaks.toml          Secret-scan rules and allowlists
├── .pre-commit-config.yaml Local hooks: gitleaks, fmt, clippy
├── docs/                   Maintained documentation (rules in docs/AGENTS.md)
│   └── plan.md             JoeDocs design decisions and phased plan
├── src/
│   ├── main.rs             Thin binary wrapper
│   ├── lib.rs              load, execute, Error, exit codes
│   ├── app_id.rs           App name and environment prefix
│   ├── cli.rs              clap definitions
│   ├── env.rs              Env trait, ProcessEnv, MapEnv
│   ├── fs.rs               FileSystem trait, RealFs, MemFs
│   ├── secrets.rs          Secrets from the environment
│   ├── config/
│   │   ├── mod.rs          Schema, defaults, Source, ConfigError
│   │   ├── resolve.rs      File selection, layering, override parsing
│   │   ├── show.rs         config show / config paths output
│   │   └── validate.rs     Rule checks beyond types (slugs, membership, paths)
│   ├── admin.rs            Reports for init, project, key and reindex commands
│   ├── auth.rs             API key format, hashing, grants, RandomSource
│   ├── clock.rs            Clock trait, SystemClock, FixedClock
│   ├── db/
│   │   ├── mod.rs          Connection setup, migrations, history head, index format
│   │   ├── projects.rs     Project queries
│   │   ├── keys.rs         API key and grant queries
│   │   ├── documents.rs    Document, listing and revision queries
│   │   ├── search.rs       Search index upkeep and the search query
│   │   └── migrations/     Embedded .sql files (0001_core, 0002_audit, 0003_search)
│   ├── document/
│   │   ├── mod.rs          Slugs and re-exports
│   │   ├── id.rs           Project keys and DocumentId (JD-42)
│   │   ├── title.rs        Title rules and file stems
│   │   └── front_matter.rs Key classes, parse, render, change rules
│   ├── history/mod.rs      History trait and GitCli (commit, show, log)
│   ├── mcp/
│   │   ├── mod.rs          Router, ServerOptions, serve, run, shutdown signal
│   │   ├── auth.rs         Bearer key middleware and Caller
│   │   ├── tools.rs        Tool parameters, results and handlers
│   │   └── errors.rs       ToolError: codes and structured results
│   ├── search/
│   │   ├── mod.rs          Re-exports and the Invalid error
│   │   ├── query.rs        Query grammar and FTS5 expressions
│   │   └── filter.rs       Front matter filters, SQL, value flattening
│   └── store/
│       ├── mod.rs          Store, Paths, Settings, Actor, Error, write lock
│       ├── documents.rs    Document use cases and the write path
│       ├── registry.rs     Projects and API keys
│       ├── search.rs       Store::search
│       ├── reindex.rs      Rebuilding the index from the history
│       ├── journal.rs      Undoable vault file writes
│       └── tests.rs        Store unit tests (temp vault, real git)
└── tests/
    ├── admin.rs            init, project, key and reindex through the binary
    ├── common/mod.rs       Shared MCP client and raw HTTP helpers
    ├── deployment.rs       Ignored black-box suite for a running server (just docker-test)
    ├── cli.rs              End-to-end tests of the binary
    ├── config_example.rs   config.example.toml matches the schema
    ├── mcp.rs              Every tool over HTTP, grants, Host/Origin, serve
    ├── reindex.rs          Rebuilds: identical, after a lost write or database
    ├── search.rs           Search over a fixture vault
    └── store.rs            A document's lifecycle across files, git and index
```

Unit tests live next to the code in `#[cfg(test)] mod tests`. Integration tests in `tests/` run the compiled binary with a cleared environment in a temporary directory, or drive the library's store against a temporary vault. Tests that touch history need `git` on `PATH`.
