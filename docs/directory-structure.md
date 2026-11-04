# Directory structure

```text
.
├── AGENTS.md               Instructions for coding agents
├── CLAUDE.md               Imports AGENTS.md for Claude Code
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
│   ├── admin.rs            (skeleton) Admin CLI: init, project, key, archive, reindex
│   ├── auth.rs             (skeleton) API keys and project grants
│   ├── db/                 (skeleton) SQLite connection, migrations, queries
│   ├── document/           Domain rules (slug so far); later types, front matter, file names
│   ├── history/            (skeleton) History trait, git implementation
│   ├── mcp/                (skeleton) MCP tools, HTTP transport, auth middleware
│   ├── search/             (skeleton) Query grammar, FTS5, filters
│   └── store/              (skeleton) Document use cases, single writer
└── tests/
    ├── cli.rs              End-to-end tests of the binary
    └── config_example.rs   config.example.toml matches the schema
```

Unit tests live next to the code in `#[cfg(test)] mod tests`. Integration tests in `tests/` run the compiled binary with a cleared environment in a temporary directory.
