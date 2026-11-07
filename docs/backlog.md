# Backlog

High-level features, newest first. Move items between sections as their status changes and link to a feature directory under `docs/features/` when one exists.

Phases and design are in [plan.md](plan.md#15-phases), and review decisions are in [section 17](plan.md#17-decisions-log).

## In progress

- **0.2.0: `INDEX.md` per project.** `<home>/<project>/INDEX.md`, a generated table of the project's live documents sorted by ID (`JD-2` before `JD-10`): ID, title (a relative Markdown link to the file), type, status and updated date. Rewritten by the store after each change (skipped when unchanged), and by `serve` and `reindex`; never committed to the history (excluded in `history.git`); a reserved path that `check` expects and reports when missing or stale. On by default, `[documents] index_file`.
- **0.2.0: release automation.** Publish to crates.io from a tag-triggered workflow with crates.io Trusted Publishing, so no token is stored; create the GitHub release; and send the Homebrew tap a `formula-release` dispatch so its Bump Formula workflow updates the formula.

## Planned

- **Plain logs when not on a terminal.** `serve` writes ANSI colour codes even when stderr is a file or the journal, so `brew services` logs and `journalctl` show escape codes. Turn colour on only when stderr is a terminal.
- **More agent skills** in `docs/skills`: an issue log, research notes, decision records, a session journal and a gardener, listed in [skills/README.md](skills/README.md#more-skills-to-add).
- **Phase 5, operations, the remaining parts:** a database backup command, and an optional `git push` of the history to a private remote after each commit (the `[history] remote` key in plan section 9). The deployment guide, the systemd unit and the Homebrew service are done.
- Removing a single grant from a key (`key grant` only adds or changes grants today).
- Remove the template's example secret `JOEDOCS_API_TOKEN` (plan section 9): the server needs no secrets of its own, but `config show` still reports it.

## Future

- **Client admin CLI.** Run the server's administrative tasks (projects, keys and grants, archive restore, `check`, `reindex`) from a client machine, without SSH. This reverses a recorded decision: [plan.md section 3](plan.md#3-requests-mcp-tools) keeps admin operations off the network so a leaked client key cannot escalate. It therefore needs an admin credential separate from document API keys (never an MCP tool), an admin endpoint that is off by default, an audit log entry per call, and its own threat model. To be refined as an idea first.
- **Idea: a document graph with Graphify.** Map the documents on the server (links, sources such as `source_ideas` and `related_plan`, shared tags, mentions of IDs) into a knowledge graph, and give agents a tool to query it, for example "what depends on JD-12" or "the path from this idea to its reviews". To explore with `joedocs-idea-architect`: what the graph adds over `search_documents` and front matter filters, how it stays current with each revision, and whether it runs inside JoeDocs or beside it.
- Rewrite `[[links]]` to a renamed document in other documents of the same project (plan section 16). Needs a commit that changes several documents, and `reindex` support for it.
- **Approval provenance** (review question 5, deferred). Any `write` key can set `status: approved`, and custom `approved_by` values are client-asserted. Planned fix: server-stamped, read-only `status_changed` and `status_changed_by` front matter (plus matching DB columns), and optionally an `approve` grant required for statuses listed in a new `documents.protected_statuses` key.
- Prebuilt binaries for Linux and macOS on GitHub Releases, with checksums, so Homebrew and others need not compile; and a homebrew-core formula once JoeDocs has users.
- Remaining items from the plan's "Later" row: per-type templates (MCP prompts), documents as MCP resources, backlinks, embedding search, attachments, secret scanning on write.

## Done

- **0.1.2** (2026-11-07): `edit_document`, `update_document` and `change_document_front_matter` refuse no-op changes; a gitleaks rule for JoeDocs API keys; a committed pre-commit hook (`.githooks/pre-commit`) with `actionlint` in the hook and CI; client setup with keys from the environment; signed release tags. Plan JD-3, review JD-4.
- **0.1.1, a documentation release** (2026-11-07): a new README, the [deployment guide](deployment.md), the [reference](reference.md) and the first [agent skills](skills/README.md).
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
