# Architecture

## Overview

`joedocs` is a command-line application split into a thin binary (`src/main.rs`) and a library (`src/lib.rs`) that holds all logic. The library never reads the real process environment, clock or random source directly, nor runs `systemctl`: they are injected through the `Env`, `FileSystem`, `Clock`, `RandomSource` and `ServiceManager` traits. The document store works on configured directories (temporary ones in tests) and keeps its history behind the `History` trait, implemented with the `git` command line. The design and phases are in [plan.md](plan.md).

```mermaid
flowchart LR
    main[main.rs] -->|parse| cli[cli::Cli]
    main -->|build| ctx[Context: AppId, ProcessEnv, RealFs, SystemClock, OsRandom]
    main -->|load| load[lib::load]
    load --> resolve[config::resolve]
    load --> secrets[secrets::Secrets]
    main -->|init| tracing[tracing-subscriber]
    main -->|execute| exec[lib::execute]
    exec --> show[config::render_show / render_paths]
    exec -->|serve| mcp[mcp: axum + rmcp]
    mcp -->|bearer key| store
    exec -->|init, project, key, reindex, archive, check| admin[admin: reports]
    admin --> store[store::Store]
    store --> db[(db: SQLite)]
    store --> history[history::GitCli]
    store --> vault[/vault files/]
    store --> document[document: IDs, titles, front matter]
    store --> search[search: query grammar, filters]
```

## Modules

| Module | Responsibility |
| --- | --- |
| `main.rs` | Parse args, build the real `Context`, load config, start logging, run, map errors to exit codes. |
| `lib.rs` | `load`, `execute`, `open_store` and `catch_up` (the rebuild `serve` runs on start, followed by rewriting each project's `INDEX.md`), the `Error` type, exit codes. |
| `cli` | `clap` definitions and conversion of flags to config values. |
| `config` | Schema and defaults (`mod.rs`), resolution (`resolve.rs`), rule checks (`validate.rs`), diagnostics (`show.rs`). |
| `env` | `Env` trait, `ProcessEnv` (process environment over `.env`), `MapEnv` (tests). |
| `fs` | `FileSystem` trait, `RealFs`, `MemFs` (tests). |
| `secrets` | Secrets from the environment, held in `SecretString`. |
| `app_id` | Application name and the environment prefix derived from it. |
| `clock` | `Clock` trait, `SystemClock`, `FixedClock` (tests), RFC 3339 formatting. |
| `document` | Pure domain rules: slugs, project keys, `DocumentId`, titles and file stems, front matter classes, parsing, serialising and change rules. |
| `store` | Document use cases (create, get, update, edit, append, rename, move, change front matter, restore a revision, archive and unarchive, list, search, history, diff), projects and API keys, the write lock, the integrity guard, the undo journal for vault files, the file → history → database ordering, each project's generated `INDEX.md` (`index_file.rs`), `reindex` and `check`, the backup snapshot (taken under the write lock), and admin tokens with their TOTP logins, lockout and sessions. |
| `db` | SQLite connection setup, embedded migrations, and queries as free functions over a connection, including the search index (`doc_fields`, `doc_fts`). |
| `history` | `History` trait and `GitCli`, with the git directory outside the vault: commit, show, log and diff. |
| `auth` | API key generation, parsing, hashing and verification; `Permission` and `Grant`; `RandomSource`. |
| `systemd` | `joedocs systemd install|remove`: renders a user or sandboxed system unit that pins the effective configuration, and runs `systemctl` through `ServiceManager`. |
| `admin` | Text reports for `init`, `project`, `key`, `reindex`, `archive` and `check`. |
| `mcp` | `joedocs serve`: the axum app (`/mcp` behind bearer authentication, `/health`), rmcp's Streamable HTTP service with `Host`/`Origin` checks, the fifteen tools with JSON schemas, grant checks, audit logging, and error mapping. |
| `mcp::admin_api` | The `/admin/v1/` API, merged into the router only when `admin.enabled`: `Host`/`Origin` checks, admin token and session authentication, TOTP login, one audit row per call, and routes that run the same `admin` command functions and return their text (`status`, `documents` and `backup` return data and the archive). One handler reads every request itself (a 64 KiB body limit), so nothing is rejected before it is checked and audited. A remote backup stages its snapshot under the store mutex and write lock, then compresses without them, so other calls carry on. |
| `client` | `joedocs` as a client of a server's admin API (`client.server`, `--server`): reqwest with rustls (ring) and the OS root store, the session file, the `Prompt` trait for codes, and the re-login retry. |
| `status` | `joedocs status`: `StatusReport` (shared with the admin API), the `/health` probe, systemd unit state through `ServiceManager`, directory sizes. |
| `agents` | `joedocs agents`: renders `src/agents.md` with this installation's facts. |
| `backup` | `joedocs backup`: the archive format (manifest, `state/`, `content/`), writing it from a store snapshot, and the restore that refuses unsafe entries. |
| `totp` | Pure TOTP (RFC 6238) and HOTP (RFC 4226), base32 (RFC 4648) and `otpauth://` URIs. |
| `search` | Pure: the forgiving query grammar and its FTS5 translation (`query.rs`), and front matter filters, their SQL translation and value flattening (`filter.rs`). |

## Request path

An MCP request first passes the bearer-key middleware, which looks the token up (SHA-256 of the secret) and attaches the key's name and grants to the request. rmcp then checks `Host` and `Origin` and dispatches the tool. Each tool checks the grant for the target project, runs the store operation under the store mutex on a blocking thread, writes an audit entry for changes, and returns structured JSON or a structured error.

## Admin request path

The admin API serves the admin commands to other machines. It is off unless `admin.enabled` is set, and it is a separate router, never MCP tools, so a document API key never reaches it. An admin token (`jda_...`, created only on the server) plus a TOTP code opens a session (`jds_...`) that lasts `admin.session_hours`. Every later call sends both the token and the session. The server checks the session before it runs anything, so a client that logs in again after an expired session can retry without repeating a change.

```mermaid
sequenceDiagram
    participant C as joedocs client
    participant A as admin API
    participant S as Store
    C->>A: POST /admin/v1/session with token and code
    A->>S: admin_login checks the token, the code step and the lock
    S-->>A: session and expiry
    A-->>C: 200 with the session
    C->>C: save sessions.toml with mode 0600
    C->>A: POST /admin/v1/keys with token and session
    A->>S: admin_session checks the session
    A->>S: run the key command and write the audit row
    A-->>C: 200 with the command output
    Note over C,A: an expired session gets 401 session_expired, then the client asks for a new code and retries once
```

## Write path

Every document change takes the write lock (a file lock in the state directory), checks the index's recorded history head against git `HEAD`, and then runs inside one database transaction: validate, check the file on disk still has the hash the index recorded, write the vault file (undoably), commit to git, record the revision and new head, commit the transaction. Any failure before the git commit undoes the file writes and rolls the transaction back. The same transaction replaces the document's search entries: every front matter value in `doc_fields`, and the title, aliases and body in the FTS5 table `doc_fts`. See [plan.md section 11](plan.md#11-write-path-example-update_document).

A multi-file document's main file keeps the `documents` row; each other file has a `document_files` row (path in the folder, the revision at which it last changed, its hash) and its own search entries (`doc_fields` rows carrying its `file_id`, and a `doc_fts` row with rowid `-id`). Every change to any of its files raises the document's revision, takes the same lock and transaction, checks every file of the document against its recorded hash, and records the files it changed in `revision_files` and in `JoeDocs-File` commit trailers. Adding a file (`Store::create_file`, in `store/files.rs`) leaves the main file untouched.

Every change other than create and archive goes through one path (`Store::revise`): it reads the verified file, lets the operation change a draft of the title, type, status, other front matter and body, then writes the result, moving the file if the title or type changed. Rename, move, update, edit, append, front matter changes and restore differ only in how they change the draft.

After the database commit, still holding the write lock, the change rewrites its project's `INDEX.md` (when `documents.index_file` is on), and so do `project add`, `reindex` and the start of `serve`. The file is generated from the database, written to a temporary file and renamed into place, and skipped when its content would not change. A failure only logs a warning, since the document change is already committed. A change to a multi-file document also rewrites the `INDEX.md` in its folder the same way. `INDEX.md` never enters the history: the store adds `/*/INDEX.md` and `/*/*/*/INDEX.md` to the history's `info/exclude`.

## Search

`search_documents` parses `query` into a quoted FTS5 expression (so no input is a syntax error) and `filters` into `EXISTS` conditions over `doc_fields`, then runs one SQL query: driven by the FTS5 match when there is text (ranked by `bm25`, title ten times body, with a snippet), otherwise by the documents table, newest first. `"*"` searches every project the key holds a grant on.

## Rebuilding the index

The documents, revisions and search tables are an index of the history: every revision is a commit whose trailers name the document, revision, action and client, and the latest content of each document carries the rest in front matter. A multi-file document's commits also name each file they change in `JoeDocs-File` trailers; the main file is the one at `<project>/<type>/<name>/<name>.md`, so a rebuild tells it from the other files by path, and a commit that changes a path no trailer names cannot be replayed. Rebuilt files are inserted in the order they were created, so they get the same row IDs. `joedocs reindex` replays `git log` into fresh tables in one transaction, recreates projects missing from the database from front matter, and reports vault files that differ from the history. `serve` runs it on start when the index records another history commit than `HEAD` (a write interrupted between the git commit and the database commit) or another index format (a database from an older build), so writes are never left refused. Then it rewrites every project's `INDEX.md`, so a file deleted or edited by hand is restored before the server listens.

## Configuration resolution

```mermaid
flowchart TD
    D[Built-in defaults] --> F{Config file found?}
    F -->|"--config / _CONFIG_FILE / XDG / ~/.config / /etc / ./"| M[Merge file values]
    F -->|none| E
    M --> E[Apply __SECTION__KEY env overrides]
    E --> C[Apply CLI flags]
    C --> V{Deserialize into Config}
    V -->|ok| K{Config::validate rules}
    K -->|ok| R[Resolved: config + source per key]
    V -->|error| X[ConfigError naming the file, variable or key]
    K -->|violation| X
```

## Key decisions

- **In-house config resolution.** Precedence, override parsing and provenance are explicit and testable without mutating the process environment, which is `unsafe` in Rust 2024.
- **Every config key has a default.** Defaults define each key's type, which is how environment override strings are parsed.
- **Secrets are separate from config.** They never appear in `config.toml`, overrides or `config show` output.
- **Exit codes follow `sysexits.h`.** `1` when `check` finds problems, `65` for requests the user can fix, `69` when the server cannot listen, `70` for store faults, `74` for output errors, `78` for configuration errors.
- **Git through its command line.** No new crates, and administrators can inspect the history with normal git. The user's git configuration is ignored so hooks and signing cannot interfere.
- **The history is the source of truth.** The database index can always be rebuilt from it; only projects and API keys need a database backup.
