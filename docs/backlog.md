# Backlog

High-level features, newest first. Move items between sections as their status changes and link to a feature directory under `docs/features/` when one exists.

Phases and design are in [plan.md](plan.md#15-phases), and review decisions are in [section 17](plan.md#17-decisions-log).

## In progress

- **0.1.1, a documentation release.** A new README (motivation, features, use cases, Homebrew install), the [deployment guide](deployment.md) (Linux systemd, macOS `brew services`, Tailscale and TLS proxies, operations), the [reference](reference.md) (commands, MCP tools, search, front matter, errors) and the first [agent skills](skills/README.md): `joedocs-idea-architect`, `joedocs-planner`, `joedocs-builder` and `joedocs-code-review`. Left: try the four skills in Claude Code, then merge, publish 0.1.1 to crates.io, tag, GitHub release, and update the Homebrew formula.

## Planned

- **Plain logs when not on a terminal.** `serve` writes ANSI colour codes even when stderr is a file or the journal, so `brew services` logs and `journalctl` show escape codes. Turn colour on only when stderr is a terminal.
- **More agent skills** in `docs/skills`: an issue log, research notes, decision records, a session journal and a gardener, listed in [skills/README.md](skills/README.md#more-skills-to-add).
- **Phase 5, operations, the remaining parts:** a database backup command, and an optional `git push` of the history to a private remote after each commit (the `[history] remote` key in plan section 9). The deployment guide, the systemd unit and the Homebrew service are done.
- **Release automation:** publish to crates.io from a tag-triggered workflow with crates.io Trusted Publishing, so no token is stored, and update the Homebrew formula from the same workflow.
- Removing a single grant from a key (`key grant` only adds or changes grants today).
- Remove the template's example secret `JOEDOCS_API_TOKEN` (plan section 9): the server needs no secrets of its own, but `config show` still reports it.

## Future

- Rewrite `[[links]]` to a renamed document in other documents of the same project (plan section 16). Needs a commit that changes several documents, and `reindex` support for it.
- **Approval provenance** (review question 5, deferred). Any `write` key can set `status: approved`, and custom `approved_by` values are client-asserted. Planned fix: server-stamped, read-only `status_changed` and `status_changed_by` front matter (plus matching DB columns), and optionally an `approve` grant required for statuses listed in a new `documents.protected_statuses` key.
- Prebuilt binaries for Linux and macOS on GitHub Releases, with checksums, so Homebrew and others need not compile; and a homebrew-core formula once JoeDocs has users.
- Remaining items from the plan's "Later" row: per-type templates (MCP prompts), documents as MCP resources, backlinks, embedding search, attachments, secret scanning on write.

## Done

- **A real Claude Code connection** to a server (2026-11-05): `joedocs serve` as a systemd user service on an Arch Linux machine, published on a tailnet with `tailscale serve`, used from Claude Code on another machine. This closes phase 2's last check.
- **Homebrew** (2026-11-05): the `joedocs` formula in the [`joelee/oss`](https://github.com/joelee/homebrew-oss) tap builds the crates.io release, runs `joedocs serve` under `brew services`, and is tested by the tap's macOS workflow (`brew install --build-from-source`, `brew test`, `brew audit --strict`). Install with `brew install joelee/oss/joedocs`.
- **0.1.0 on crates.io** (2026-11-05): `cargo install joedocs --locked`, tag `v0.1.0` and a GitHub release. The package `include` list ships only the sources, the lockfile, the README, the licence, the changelog and `config.example.toml`.
- `joedocs systemd install|remove` (user or sandboxed system unit, `--dry-run`) and GitHub Actions CI (`just ci` on Linux and macOS, `just docker-test`).
- `key rotate`: a new token for an existing key, keeping its name and grants.
- Phase 4, history and editing tools: `get_document_history`, `diff_document`, `restore_document_version`, `move_document`, `edit_document`, `append_to_document`, and the `archive list|restore` and `check` commands. Implementation choices are in [plan.md section 17](plan.md#17-decisions-log).
- Phase 3, search: `search_documents` with the query grammar (words, phrases, exclusions, prefixes, `OR`), front matter filters, ranking and snippets, in one project or all; `joedocs reindex`, which `serve` runs on start when the index is behind the history. Also fixed commit dates, which phases 1 and 2 recorded as 1970. Implementation choices are in [plan.md section 17](plan.md#17-decisions-log).
- Phase 2, MCP server: `serve` over Streamable HTTP with bearer keys, `Host`/`Origin` checks, eight tools, grant checks, structured errors and an audit log. Implementation choices are in [plan.md section 17](plan.md#17-decisions-log).
- Phase 1, core store: document IDs, titles and front matter rules; SQLite schema; git history; create, get, update, rename, front matter, archive and list; the integrity guard; `init`, `project` and `key` commands; the `Clock` trait. Implementation choices are in [plan.md section 17](plan.md#17-decisions-log).
- Phase 0, foundations: the `server`, `storage` and `documents` config replaces the example `greet` command, and the dependencies are approved (option A in [plan.md section 13](plan.md#13-crates-audited-and-approved-2026-11-04)).
- Design plan and module skeleton ([plan.md](plan.md)); crate and binary renamed to `joedocs`.
- Project scaffold generated from the Rust template.
