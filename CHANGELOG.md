# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/). Versions number the `joedocs` command line, its configuration and its MCP tools; the Rust library inside the crate is internal and may change in any release.

## [Unreleased]

## [0.5.1] - 2026-11-12

User accounts: administrators sign in to the web interface and the admin command line with a name, a password and a TOTP code, instead of an admin token. Admin tokens are deprecated. MCP clients, keys and tools are unchanged.

### Added

- **Users.** `joedocs user create`, `list`, `disable`, `enable`, `unlock`, `reset-password`, `reset-totp` and `remove` on the server. All but `remove` also run remotely for a signed-in user. `create` and the resets print a one-time password, valid for 72 hours, alone on the last line. Every user is an admin in 0.5.1.
- **TOTP required, set up at the first sign-in.** A new user, or one whose authenticator was reset, signs in with the one-time password, adds the shown secret (a QR code in the terminal and on the web page) to an authenticator, confirms it with a code, and chooses their own password. No session or device is given before that, and nothing turns TOTP off.
- **`joedocs auth login`** (`--user NAME`, or the new `client.user`). The first time on a machine it asks for the password (without echo) and a code, and stores a device credential in `$XDG_STATE_HOME/joedocs/devices.toml` (`0600`). After that a session needs only a code; `--code CODE` renews without a terminal. Also `auth logout [--forget-device]`, `auth status`, `auth password` and `auth devices [revoke NAME]`.
- **Web interface:** a user sign-in form (name, password and code), the TOTP setup page with a QR code drawn on the server, an Account page (change your password, revoke your devices) and a Users page.
- **Admin API routes:** `POST /admin/v1/auth/login` and `auth/setup`, `GET /admin/v1/me`, `POST me/password`, `GET me/devices`, `POST me/devices/{name}/revoke`, `GET`/`POST /admin/v1/users` and `POST users/{name}/disable|enable|unlock|reset-password|reset-totp`. `POST /admin/v1/session` also renews a session with a device credential (`Authorization: Bearer jdd_...`). New error codes: `429 busy`, `403 user_required`, `401 device_invalid`.
- **Configuration keys:** `admin.device_days` (default 30: a device not used that long signs in with the password again), `admin.token_login` (default `true`; `false` refuses every admin-token request) and `client.user`, with the global flag `--user`.
- **Passwords** are hashed with Argon2id at OWASP's minimum parameters (19 MiB, 2 iterations, 1 lane), stored as PHC strings and rehashed when the parameters rise. A new password is 12 to 256 characters (counted after NFC normalisation), not the user's name or a common password. Hashing never runs while the store is locked, at most two at once, so sign-in attempts cannot slow MCP requests; beyond that a sign-in is refused `429 busy`.
- **Lockout per name.** Five failed sign-ins lock that name, real or not, for 15 minutes; a count is forgotten 15 minutes after the name's last failure, the same way for every name. The lock stops new sign-ins only: open sessions, device renewals and MCP keys keep working. Every refusal is the same `401`, so a reply never says whether a name exists. A typed name is never stored: real users' counts are kept by user ID, and other names are counted in memory only, under a keyed hash (at most 100,000, so a restart or a sustained flood can lift an unknown name's lock early). A device's renewals keep their own five-code lock, which ordinary use does not lift. Audit rows of a user's calls name the user (`user_id`, `user_name`, and `"user"` in `audit --json`), and a refused sign-in records the name only when it is an existing user's.

### Deprecated

- **Admin tokens:** `joedocs admin-token`, `joedocs login` and `logout` with `JOEDOCS_ADMIN_TOKEN`, and admin-token sign-in on the web page. They keep working in 0.5.1. `login` and `admin-token` print a warning on standard error (standard output is unchanged), and admin API replies to admin-token requests carry `Deprecation: @1794441600` and a `Link` with `rel="deprecation"` (RFC 9745). Removal comes in a later release. Admin tokens can no longer reach the new user routes (`403 user_required`).

### Changed

- The database is version 8 (migrations `0007_users` and `0008_sign_in_lock`): users, devices, user sessions and the sign-in lock by user ID, plus nullable `user_id` and `user_name` columns on the audit log. Existing admin tokens, sessions, keys and grants keep working. Going back to 0.5.0 needs the backup taken before upgrading.
- Exit code `77` also covers a needed TOTP setup without a terminal and an admin token on a user-only route; `69` also covers a server too busy checking sign-ins (`429 busy`).
- `auth logout --forget-device` forgets this device only once the server has revoked it (or says it is no longer valid). It renews an expired session with a code first; without a terminal, or when the server refuses or fails, it exits non-zero and keeps the device. `auth status`, `auth password` and `auth devices revoke` exit `70` when the server's reply is malformed.

### Fixed

- The release workflow no longer cancels the other binary builds when one fails, and retries the `musl-tools` install on a transient network failure (issue JD-35).

## [0.5.0] - 2026-11-11

An admin web interface: the server's administrators can browse, search and read documents, and do what the admin commands do, in a browser.

### Added

- The admin web interface under `/admin/`, served by `joedocs serve` when `admin.enabled` and the new `admin.web_ui` are on: an Overview of the server; the document library; a document view that renders Obsidian-style Markdown on the server (callouts, wiki links, tags, highlights, task lists, footnotes, an "On this page" list, Mermaid diagrams drawn by the bundled Pele, PlantUML diagrams from a configured server) with its properties, files, revisions, History, diffs and `.md` and `.tgz` downloads; Search with every filter of `joedocs search`, reloading as you type; Projects; API keys, with a new or rotated token shown once; the Audit log with its filters and quick filters; the Archive; Maintenance (check, reindex, backup download); and `document convert`, `move-file` and `restore-file`. Every change asks for confirmation and is written with the admin token's name and client `joedocs-web`; document content stays read-only in the browser.
- Signing in with an admin token and a TOTP code opens a web-only session (`jdw_...`) in an `HttpOnly`, `SameSite=Strict` cookie, `__Host-` and `Secure` over HTTPS; plain HTTP is accepted only on `localhost`. Forms need the session's CSRF token and the same origin, pages carry a strict Content-Security-Policy, and every page and form request writes one audit row.
- The keys `admin.web_ui` (default `false`) and `admin.web_plantuml_url` (default empty; a PlantUML server the admin's browser fetches diagrams from, which then receives their text).
- `joedocs serve` prints `JoeDocs web interface: http://<bind>/admin/` when it is on; `joedocs status` and `joedocs agents` say whether it is on; the `status` JSON gains `web_ui_enabled`.

### Changed

- The database is version 6 (migration `0006_web_sessions`): admin sessions have a kind, `api` or `web`, and neither is accepted in place of the other. Existing `joedocs login` sessions keep working. Going back to 0.4.3 needs the backup taken before upgrading.

## [0.4.3] - 2026-11-10

Prebuilt, checksummed and attested binaries on GitHub Releases, plain logs off a terminal, clearer `get -f` errors, and the unused example secret removed.

### Added

- Prebuilt binaries on each GitHub release: Linux x86_64 and aarch64 (static, musl) and macOS Apple silicon, as `joedocs-X.Y.Z-<target>.tar.gz`, with `SHA256SUMS` and a build-provenance attestation for each (`gh attestation verify`). The release workflow builds and checks them before the crates.io publish, so a failed build stops the release.

### Changed

- Logs are plain (no ANSI colour codes) when stderr is not a terminal, such as the journal or `brew services` logs, or when `NO_COLOR` is set.
- `joedocs get --help` describes `-f`/`--file` as the output file and points to `-P`/`--part` for a file of a multi-file document (issue JD-27).

### Fixed

- `joedocs get -f PATH` and `get --archive OUT.tgz`, locally and remotely, name the path they could not write: `cannot write missing/out.md: No such file or directory`, where they said `cannot write output`. The exit code is still `74` (issue JD-27).

### Removed

- `JOEDOCS_API_TOKEN`, the template's example secret, which JoeDocs never used; `config show` no longer lists it.

## [0.4.2] - 2026-11-10

Admin commands to view and find documents and read the audit log, locally and through JSON admin API routes that the 0.5.0 admin web interface will build on.

### Added

- Short options for the frequent flags, with one meaning each: `-p` project, `-t` type, `-s` status, `-d` document, `-k` key, `-F` filter, `-n` limit, `-P` part, `-j` JSON.

- **`joedocs list -p PROJECT`** lists a project's live documents as a table (`ID`, `TYPE`, `STATUS`, `REV`, `FILES`, `UPDATED`, `TITLE`), filtered with `-t/--type` and `-s/--status`, ordered with `--sort` (`updated-desc`, `updated-asc`, `created-desc`, `created-asc`, `title`), and paged with `--start` and `-n/--limit` (a last line says how to get the next page). `-j/--json` prints the `list_documents` result instead. It also runs remotely, through the new route `GET /admin/v1/documents`, which returns that JSON.
- **`joedocs search [QUERY...]`** finds documents and their files by text and front matter in every project, or one (`-p`): `-t/--type`, `-s/--status`, `-F/--filter KEY=VALUE` (repeatable), `--filters JSON` (the operators of `search_documents`) and `-d/--document ID`. Words that start with `-` (exclusions) follow `--`, after the options: `joedocs search -p joedocs -- cache -redis`. It prints each hit with its snippet, pages with `--start` and `-n/--limit`, and `-j/--json` prints the `search_documents` result. Remotely it uses the new route `POST /admin/v1/search`.
- **`joedocs history ID`** lists a document's revisions (`REV`, `TIME`, `ACTION`, `AUTHOR`, `CLIENT`, `SUMMARY`), newest first and paged, or with `-P/--part FILE` one file's; **`joedocs diff ID --from N [--to M]`** prints the unified diff between two revisions, with `--context` and `-P/--part FILE`. Both take `-j/--json` (the `get_document_history` and `diff_document` results) and run remotely through `GET /admin/v1/documents/{id}/history` and `.../diff`.
- **`joedocs audit`** reads the audit log, newest first (`TIME`, `KEY`, `CLIENT`, `TOOL`, `DOCUMENT`, `OUTCOME`), filtered by `--since` and `--until` (UTC dates or times), `-k/--key`, `--tool` (a trailing `*` matches a prefix, e.g. `admin:*`), `-d/--document` and `--outcome`, paged, with `-j/--json`. Remotely it uses the new route `GET /admin/v1/audit`.
- **`joedocs key ungrant NAME PROJECT...`** removes a key's grants on those projects, all or nothing; the key's next request no longer reaches them. Remotely it uses the new route `POST /admin/v1/keys/{name}/ungrant`.
- **`joedocs archive list -t/--type TYPE`** lists only archived documents of that type; `-p` is now short for `--project`. Remotely, `GET /admin/v1/archive` takes `type`.
- `joedocs get -P` is short for `--part`.
- Tables of the new commands print control characters in stored text as escapes (`\n`, `\u{1b}`), so text written through MCP cannot change the terminal.
- A command whose admin API route the server does not have says the server may run an older JoeDocs and should be upgraded.

### Changed

- **The `joedocs-code-review` skill re-reviews in place.** A re-review of the same change (the same pull request, or a branch built on the reviewed head) updates the earlier review document as a new round instead of publishing another document. Each earlier finding keeps its text and gains a disposition (`resolved`, `still-open`, ...); new findings are numbered `R2-MED-01`; a `Re-review R2` section records the round; the verdict, open counts and status follow the latest round (`review_round` counts them). A different change, or a request for one, still gets a new review that supersedes the earlier one.

### Fixed

- `joedocs get --part ""` in client mode reports the missing file, as it does on the server, instead of printing the whole document. More generally, the admin API no longer drops an option given as an empty value, so remote runs refuse or match it as local runs do.

## [0.4.1] - 2026-11-10

An issue log for agents, release plans that pick up their issues, and a search fix.

### Fixed

- `search_documents` hits on a multi-file document report its real `files` count, as `get_document` and `list_documents` do; they said `0` (issue JD-21).

### Added

- **The `joedocs-issue-log` skill** (`docs/skills/joedocs-issue-log`) files bugs, regressions and incidents as `issues` documents. Required front matter: `status`, `kind`, `severity`, `repository`, `found_in` and `reported_by`; optional keys include `target_version`, `fixed_in` and `resolution`. It uses the server's statuses (`draft` and `review` are open, `done` is fixed, `rejected` is not fixed, `superseded` is a duplicate), appends dated investigation notes, closes the issues a release fixed, and lists what is open.
- **`joedocs-planner` plans from issues.** A plan for a version (`target_version`) includes the open issues targeted at it, then lists the open issues with no version and asks which to include, setting `target_version` only on those you pick. Plans record their issues in `source_issues`, and unplanned queries cover issues.

### Changed

- The backlog moved from `docs/backlog.md` into JoeDocs, as a multi-file `notes` document with one file per item; the README has a Roadmap section with the planned and future headlines.

## [0.4.0] - 2026-11-09

Multi-file documents: a document can be a folder of Markdown files under one ID and one revision number, such as a roadmap with a backlog of items, each file with its own front matter and searchable on its own. And `joedocs login` sets up the server for you.

### Added

- **Multi-file documents.** `new_document` with `multifile: true` makes the document a folder, `<project>/<type>/<Title>/<Title>.md` (an Obsidian folder note, so `[[Title]]` still links to it). Its other files, such as `<Title>/backlog/item-1.md`, have their own front matter (`document`, `revision` and `updated` first, then any keys of yours, `status` included). Every change to any file raises the document's revision; each file records the revision at which it last changed, and a change conflicts only when that file changed after the revision you read. Renaming, moving, archiving and restoring the document act on the whole folder. A generated `<Title>/INDEX.md` lists the files.
- **MCP tools:** `new_document_file`, `list_document_files`, `write_document_files` (several files in one revision, all or nothing) and `archive_document_file`, for nineteen in all. `get_document`, `update_document`, `edit_document`, `append_to_document`, `change_document_front_matter`, `get_document_history` and `diff_document` take `file`, and `rename_document` takes `file` and `new_file`. `search_documents` finds a document's files on their own (hits carry `file`; filters test the matched file's front matter) and takes `document_id` to search one document. Documents carry `multifile` and `files` (a count).
- **`joedocs document convert`, `move-file` and `restore-file`**, administrator commands (never MCP tools) that make a single-file document multi-file, move a file to another multi-file document in one commit, and bring back one file as it was at an earlier revision. They also run remotely, through `POST /admin/v1/documents/{id}/convert`, `files/move` and `files/restore`.
- **`joedocs get --part FILE`** prints one file of a multi-file document, and **`--archive OUT.tgz`** writes all of them to a gzip tar; `get` on a multi-file document lists its other files after the main file. Both work remotely too.
- **`joedocs login` asks for the server.** At a terminal with no `client.server` and no `--server`, it asks for the URL, checks it (`https`, or `http` to this machine, and `/health` answering `ok` without a redirect), asks again up to three times, and saves `client.server` in the config file in use, or `~/.config/joedocs/config.toml`, keeping that file's comments.
- `documents.max_files` (default 500; `JOEDOCS__DOCUMENTS__MAX_FILES`): the most files a multi-file document may hold besides its main file.
- History commits of multi-file documents name each file they change in `JoeDocs-File` trailers (`JoeDocs-File-From` for a rename; `JoeDocs-Target-Document`, `-Target-Revision` and `-Target-File` for a move between documents), and `reindex` rebuilds every file from them. New actions: `create_file`, `write_files`, `archive_file`, `convert`, `move_file` and `restore_file`.

### Changed

- A missing admin token now says to *export* `JOEDOCS_ADMIN_TOKEN`, and so does `joedocs admin-token create`: a shell variable that is set but not exported is not seen by `joedocs`.
- `rename_document` takes `title`, or `file` and `new_file`; `title` is no longer required on its own. Calls that worked before work the same.
- `conflict` errors for a file of a multi-file document also give `file` and `file_revision`.

### Fixed

- A case-only rename (of a document, or of a file of a multi-file document) on a case-sensitive file system no longer replaces another file whose name differs only in case.

### Security

- `rmcp` 3.9.0 (RUSTSEC-2026-0462). JoeDocs uses only rmcp's server, which the advisory does not affect; the update clears it for builds from source.

### Upgrading

- Database migration 5 adds multi-file documents: **JoeDocs 0.3.x cannot open the database afterwards**. Take a backup first with `joedocs backup FILE.tgz` (see [upgrading.md](docs/upgrading.md)).
- The index layout changes, so the first `serve` rebuilds the index from the history once; existing documents are unchanged.

## [0.3.0] - 2026-11-08

Run the admin commands from another machine, protected by an admin token and a TOTP code, and new commands to read a document, see where and how the server runs, brief a maintenance agent, and back up and restore.

### Added

- **Remote administration.** With `[admin] enabled = true` (off by default), `joedocs serve` also serves an admin API under `/admin/v1/`. On another machine, set `client.server` (or pass `--server URL`) and `JOEDOCS_ADMIN_TOKEN`, run `joedocs login` with a 6-digit code from Google Authenticator or another TOTP app, and `project`, `key`, `archive`, `check`, `reindex`, `status`, `get` and `backup` run on the server with the same output and exit codes. Sessions last `admin.session_hours` (8 by default; `JOEDOCS__ADMIN__SESSION_HOURS`). When one expires, a command at a terminal asks for a new code and runs once more; scripts get exit `77`. Five wrong codes lock a token for 15 minutes. Every admin call is audited. `joedocs logout` ends the session. Admin operations are still never MCP tools, and document API keys cannot use the admin API.
- **`joedocs admin-token create|list|revoke`**, on the server only: admin tokens (`jda_...`) with their TOTP secret, shown once as base32 and as an `otpauth://` URI.
- **`joedocs get DOCUMENT_ID`** prints a document, or only its body (`--content-only`) or front matter (`--front-matter-only`), at any revision (`--version N`), to standard output or a file (`-f`).
- **`joedocs status`** shows where the service runs (bind address, `/health`, the systemd unit and its PID, whether the admin API is on), the sizes of the vault, state directory, database and history, and the state of the store.
- **`joedocs agents`** prints an AGENTS.md-style guide for AI agents that maintain the server, filled in with this installation's paths and never with a secret.
- **`joedocs backup FILE.tgz`** writes a consistent backup (database, history and vault) while the server runs, and **`joedocs backup --restore`** restores one into empty directories, refusing damaged archives and unsafe entries.
- **[docs/upgrading.md](docs/upgrading.md)**: backing up, upgrading each kind of install, checking, going back, and notes for each version. Commands that read the store run with the systemd unit's own `User=` and `Environment=`, so they see the same store as the service.
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

[Unreleased]: https://github.com/joelee/joedocs-mcp/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/joelee/joedocs-mcp/compare/v0.4.3...v0.5.0
[0.4.3]: https://github.com/joelee/joedocs-mcp/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/joelee/joedocs-mcp/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/joelee/joedocs-mcp/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/joelee/joedocs-mcp/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/joelee/joedocs-mcp/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/joelee/joedocs-mcp/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/joelee/joedocs-mcp/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/joelee/joedocs-mcp/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/joelee/joedocs-mcp/releases/tag/v0.1.0
