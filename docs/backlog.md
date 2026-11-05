# Backlog

High-level features, newest first. Move items between sections as their status changes and link to a feature directory under `docs/features/` when one exists.

Phases and design are in [plan.md](plan.md#15-phases), and review decisions are in [section 17](plan.md#17-decisions-log).

## In progress

- Publish 0.1.0 to crates.io (`cargo install joedocs --locked`). Prepared: package metadata and `include` list, README install section, CHANGELOG, release steps in the developer guide, `cargo publish --dry-run` passes. Left: merge, `cargo publish`, tag `v0.1.0`, GitHub release. Later releases: crates.io Trusted Publishing from a tag-triggered workflow, so no token is stored.
- Confirm a real Claude Code connection to `joedocs serve` on the server (it works against the local Docker Compose stack, confirmed 2026-11-04).

## Planned

- Removing a single grant from a key (`key grant` only adds or changes grants today).
- Phase 5, operations: deployment and backups.
- Publish a Homebrew formula, so users can `brew install joedocs`. Likely a tap (`joelee/homebrew-tap`) with a formula that builds from the release tarball or installs prebuilt binaries from GitHub Releases (a release workflow for Linux and macOS, with checksums), `depends_on "git"`, and a `test do` block. Consider homebrew-core once it has users. On macOS a launchd service would be the counterpart of `joedocs systemd`.

## Future

- Rewrite `[[links]]` to a renamed document in other documents of the same project (plan section 16). Needs a commit that changes several documents, and `reindex` support for it.
- **Approval provenance** (review question 5, deferred). Any `write` key can set `status: approved`, and custom `approved_by` values are client-asserted. Planned fix: server-stamped, read-only `status_changed` and `status_changed_by` front matter (plus matching DB columns), and optionally an `approve` grant required for statuses listed in a new `documents.protected_statuses` key.
- Remaining items from the plan's "Later" row: per-type templates (MCP prompts), documents as MCP resources, backlinks, embedding search, attachments, secret scanning on write.

## Done

- `joedocs systemd install|remove` (user or sandboxed system unit, `--dry-run`) and GitHub Actions CI (`just ci` on Linux and macOS, `just docker-test`).
- `key rotate`: a new token for an existing key, keeping its name and grants.
- Phase 4, history and editing tools: `get_document_history`, `diff_document`, `restore_document_version`, `move_document`, `edit_document`, `append_to_document`, and the `archive list|restore` and `check` commands. Implementation choices are in [plan.md section 17](plan.md#17-decisions-log).
- Phase 3, search: `search_documents` with the query grammar (words, phrases, exclusions, prefixes, `OR`), front matter filters, ranking and snippets, in one project or all; `joedocs reindex`, which `serve` runs on start when the index is behind the history. Also fixed commit dates, which phases 1 and 2 recorded as 1970. Implementation choices are in [plan.md section 17](plan.md#17-decisions-log).
- Phase 2, MCP server: `serve` over Streamable HTTP with bearer keys, `Host`/`Origin` checks, eight tools, grant checks, structured errors and an audit log. Implementation choices are in [plan.md section 17](plan.md#17-decisions-log).
- Phase 1, core store: document IDs, titles and front matter rules; SQLite schema; git history; create, get, update, rename, front matter, archive and list; the integrity guard; `init`, `project` and `key` commands; the `Clock` trait. Implementation choices are in [plan.md section 17](plan.md#17-decisions-log).
- Phase 0, foundations: the `server`, `storage` and `documents` config replaces the example `greet` command, and the dependencies are approved (option A in [plan.md section 13](plan.md#13-crates-audited-and-approved-2026-11-04)).
- Design plan and module skeleton ([plan.md](plan.md)); crate and binary renamed to `joedocs`.
- Project scaffold generated from the Rust template.
