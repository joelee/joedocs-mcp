# Directory structure

```text
.
├── .github/
│   ├── workflows/ci.yml    just ci on Linux and macOS, and just docker-test
│   ├── workflows/release.yml  Signed vX.Y.Z tag: checks, CI, approval, crates.io, tap, GitHub release
│   └── dependabot.yml      Weekly action and crate update proposals
├── AGENTS.md               Instructions for coding agents
├── CLAUDE.md               Imports AGENTS.md for Claude Code
├── Dockerfile              Two-stage image: build, then debian-slim with git
├── compose.yaml            Local server; data bind-mounted from .local/data (git-ignored)
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
├── .githooks/pre-commit    Git hook running `just pre-commit` (enabled by just setup)
├── docs/                   Maintained documentation (rules in docs/AGENTS.md)
│   ├── deployment.md       Running a server: systemd, brew services, Tailscale, operations
│   ├── upgrading.md        Upgrading a server, going back, notes for each version
│   ├── reference.md        Commands, MCP tools, search, front matter, errors
│   ├── plan.md             JoeDocs design decisions and phased plan
│   ├── media/              Images for the README (the banner)
│   └── skills/             Agent skills that use JoeDocs (one folder per skill, with SKILL.md)
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
│   │   ├── save.rs         Saving client.server for joedocs login (toml_edit)
│   │   ├── show.rs         config show / config paths output
│   │   └── validate.rs     Rule checks beyond types (slugs, membership, paths)
│   ├── admin.rs            Reports for init, project, key, admin-token, reindex, archive, document, check, get, backup
│   ├── agents.rs           joedocs agents: the maintenance guide filled in with this installation's facts
│   ├── agents.md           The guide's template (shipped in the crate)
│   ├── backup.rs           joedocs backup: archive format, writing, safe restore
│   ├── browse.rs           joedocs list and search: store requests, the admin API's JSON replies, tables
│   ├── client.rs           Client of a server's admin API: login, sessions, prompts, remote calls
│   ├── status.rs           joedocs status: StatusReport, the /health probe, unit state, sizes
│   ├── totp.rs             TOTP (RFC 6238), base32, otpauth URIs
│   ├── auth.rs             API key format, hashing, grants, RandomSource
│   ├── clock.rs            Clock trait, SystemClock, FixedClock
│   ├── db/
│   │   ├── mod.rs          Connection setup, migrations, history head, index format
│   │   ├── projects.rs     Project queries
│   │   ├── keys.rs         API key and grant queries
│   │   ├── admin.rs        Admin token and session queries
│   │   ├── documents.rs    Document, listing and revision queries
│   │   ├── files.rs        Files of multi-file documents and the files each revision changed
│   │   ├── search.rs       Search index upkeep and the search query
│   │   └── migrations/     Embedded .sql files (0001_core, 0002_audit, 0003_search, 0004_admin, 0005_files)
│   ├── document/
│   │   ├── mod.rs          Slugs and re-exports
│   │   ├── id.rs           Project keys and DocumentId (JD-42)
│   │   ├── title.rs        Title rules and file stems
│   │   ├── file.rs         FilePath: paths of files inside a multi-file document
│   │   └── front_matter.rs Key classes, parse, render, change rules (documents and their files)
│   ├── history/mod.rs      History trait and GitCli (commit, show, log, diff)
│   ├── mcp/
│   │   ├── mod.rs          Router, ServerOptions, serve, run, shutdown signal
│   │   ├── admin_api.rs    The /admin/v1 API: login, sessions, checks, audit, routes
│   │   ├── auth.rs         Bearer key middleware and Caller
│   │   ├── tools.rs        Tool parameters, results and handlers
│   │   └── errors.rs       ToolError: codes and structured results
│   ├── systemd.rs          joedocs systemd install|remove, ServiceManager
│   ├── search/
│   │   ├── mod.rs          Re-exports and the Invalid error
│   │   ├── query.rs        Query grammar and FTS5 expressions
│   │   └── filter.rs       Front matter filters, SQL, value flattening
│   └── store/
│       ├── mod.rs          Store, Paths, Settings, Actor, Error, write lock
│       ├── documents.rs    Document use cases and the write path
│       ├── editing.rs      Move, exact edits, append, restore a revision
│       ├── files.rs        Multi-file documents: create, read and list files
│       ├── file_changes.rs Changing files: update, edit, append, front matter, rename, write several
│       ├── folders.rs      Folder moves, archive and unarchive, folder restore, archive one file
│       ├── admin_files.rs  convert, move-file and restore-file (admin only)
│       ├── revisions.rs    History pages and diffs
│       ├── admin_tokens.rs Admin tokens, TOTP logins, lockout, sessions
│       ├── archive.rs      Archive list and restore
│       ├── backup.rs       The consistent snapshot behind joedocs backup
│       ├── summary.rs      Counts and state for joedocs status
│       ├── check.rs        Drift report for joedocs check
│       ├── index_file.rs   The generated INDEX.md of each project and multi-file document
│       ├── registry.rs     Projects and API keys
│       ├── search.rs       Store::search
│       ├── reindex.rs      Rebuilding the index from the history
│       ├── journal.rs      Undoable vault file writes
│       └── tests.rs        Store unit tests (temp vault, real git)
└── tests/
    ├── admin.rs            Admin commands through the binary
    ├── admin_api.rs        The admin API: off by default, login, lockout, sessions, audit
    ├── client.rs           Client mode: login, sessions, prompts, parity with local commands
    ├── common/mod.rs       Shared MCP client and raw HTTP helpers
    ├── deployment.rs       Ignored black-box suite for a running server (just docker-test)
    ├── files.rs            Multi-file documents: files, conflicts, folders, history, search, reindex
    ├── cli.rs              End-to-end tests of the binary
    ├── config_example.rs   config.example.toml matches the schema
    ├── history.rs          History, diff, restore, move, edit, append, archive, check
    ├── mcp.rs              Every tool over HTTP, grants, Host/Origin, serve
    ├── reindex.rs          Rebuilds: identical, after a lost write or database
    ├── search.rs           Search over a fixture vault
    └── store.rs            A document's lifecycle across files, git and index
```

Unit tests live next to the code in `#[cfg(test)] mod tests`. Integration tests in `tests/` run the compiled binary with a cleared environment in a temporary directory, or drive the library's store against a temporary vault. Tests that touch history need `git` on `PATH`.
