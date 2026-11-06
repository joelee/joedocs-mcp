# JoeDocs

[![CI](https://github.com/joelee/joedocs-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/joelee/joedocs-mcp/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/joedocs.svg)](https://crates.io/crates/joedocs)
[![Licence](https://img.shields.io/crates/l/joedocs.svg)](https://github.com/joelee/joedocs-mcp/blob/main/LICENSE)

**One home for every project's plans, ideas, issues, reviews and specs, shared by all your coding agents, on every machine.**

JoeDocs is a small, self-hosted [MCP](https://modelcontextprotocol.io) server. Claude Code (or any MCP client) on your laptop, desktop and servers reads and writes the same documents through it. Every document is a plain Markdown file with YAML front matter, so you can open the whole collection as an [Obsidian](https://obsidian.md) vault. Every change is a git commit, so nothing is ever lost.

```text
You:    Plan the rate-limit work from the idea we captured yesterday.
Agent:  search_documents("rate limit", type: ideas)  →  JD-12 "Rate limiting for the API"
        get_document(JD-12)  …reads the repository…
        new_document(type: plans, title: "Delivery plan - Rate limiting")  →  JD-31
Agent:  Draft plan JD-31 is ready for review: 6 steps, no open decisions.
```

## Why

Coding agents write a lot of useful documents: plans, design notes, review findings, research. Usually they end up in one of three places:

- **in the repository**, mixed in with the code, cluttering pull requests, and tied to one checkout on one machine;
- **in a chat transcript**, gone when the session ends;
- **in a local notes folder**, which the agent on your other machine cannot see.

JoeDocs gives them one place instead. It is the documentation layer behind your agents: central, versioned, searchable, and readable by both people and models.

## Features

- **Plain Markdown you own.** One folder per project and type (`my-app/plans/Rate limiting.md`), with front matter for status, tags and your own keys. No database lock-in: the files are the content.
- **Stable IDs.** Every document gets a short ID such as `JD-42`. It survives renames and moves, and is cheap for agents to quote.
- **Full history.** One git commit per change, authored by the API key that made it. Agents can read any earlier revision, diff two of them, or restore one.
- **Safe concurrent editing.** Updates carry the revision they were based on, so an agent on one machine never silently overwrites an agent on another. Small changes use exact search-and-replace edits instead of resending the whole document.
- **Search.** Full-text search with phrases, exclusions, prefixes and `OR`, ranked with titles first, plus filters on any front matter key (`{"tags": {"contains": "security"}, "status": "approved"}`), in one project or across all of them.
- **Scoped API keys.** Each key gets `read` or `write` access to specific projects. Tokens are shown once and stored hashed, and can be rotated or revoked at once.
- **Obsidian-friendly.** Titles are file names, so `[[wiki links]]` work; renamed documents keep their old titles as `aliases`; Mermaid and PlantUML blocks are stored as written.
- **Easy to run.** One binary with SQLite built in. It runs as a systemd service on Linux, under `brew services` on macOS, or in Docker. Reach it from anywhere over Tailscale or behind your own TLS proxy.

## Use cases

- **Plans that outlive the session.** An agent drafts a delivery plan, you review and approve it, and a builder agent on another machine works through it and logs progress in the same document. See the [skills](https://github.com/joelee/joedocs-mcp/blob/main/docs/skills/README.md).
- **An idea inbox.** Capture ideas from any machine as you think of them, then search and promote the good ones to plans with `move_document`.
- **Code review records.** Keep review findings per project, filter the open ones by status and severity, and link them from the plans that fix them.
- **Research and decisions.** Keep research notes and architecture decisions where every agent can find them, instead of repeating the same investigation.
- **Issue logs and journals.** Append comments, status updates or session notes to a running document without rewriting it.
- **One knowledge base across projects.** Search everything you are allowed to read at once with `project: "*"`, and browse it all as one Obsidian vault.

## Install

**macOS** (Homebrew):

```bash
brew trust joelee/oss && brew tap joelee/oss
brew install joelee/oss/joedocs
```

**Linux or macOS** (Cargo, Rust 1.99 or newer and a C compiler):

```bash
cargo install joedocs --locked
```

JoeDocs needs `git` at run time. On macOS the Xcode Command Line Tools provide it.

## Quick start

1. Tell JoeDocs where to keep documents, in `~/.config/joedocs/config.toml`:

   ```toml
   [storage]
   home = "/home/you/Documents/JoeDocs"   # an absolute path; empty or new
   ```

2. Create the store, a project and an API key:

   ```bash
   joedocs init
   joedocs project add notes --key NT --title "My notes"
   joedocs key create laptop --grant notes:write   # prints the token once, on the last line
   ```

3. Start the server, by hand or as a background service:

   ```bash
   joedocs serve                     # http://127.0.0.1:8750/mcp
   joedocs systemd install           # Linux: a systemd user service
   brew services start joedocs       # macOS, if installed with Homebrew
   ```

4. Connect Claude Code:

   ```bash
   claude mcp add --transport http --scope user joedocs http://127.0.0.1:8750/mcp \
     --header "Authorization: Bearer jd_..."
   ```

   Then ask it: *"List my JoeDocs projects and create a note about this repository."*

To use one server from several machines, run it on a server and publish it over Tailscale or a TLS proxy. The [deployment guide](https://github.com/joelee/joedocs-mcp/blob/main/docs/deployment.md) walks through it step by step.

## What agents can do

| | Tools |
| --- | --- |
| Discover | `list_projects`, `list_documents`, `search_documents` |
| Read | `get_document` (any revision), `get_document_history`, `diff_document` |
| Write | `new_document`, `update_document`, `edit_document`, `append_to_document`, `change_document_front_matter` |
| Organise | `rename_document`, `move_document`, `restore_document_version`, `archive_document` |

Administration stays on the server's command line, so a leaked client key cannot create keys or delete history: `joedocs project`, `key create|grant|rotate|revoke`, `archive list|restore`, `reindex` and `check`. The [reference](https://github.com/joelee/joedocs-mcp/blob/main/docs/reference.md) lists every command, tool, parameter and error code.

## Skills for agents

[`docs/skills`](https://github.com/joelee/joedocs-mcp/blob/main/docs/skills/README.md) has ready-made Claude Code skills that use JoeDocs as their document store, and together make a delivery workflow:

- **joedocs-idea-architect** pressure-tests a raw idea and records it for you to accept, park or reject.
- **joedocs-planner** turns an accepted idea, an open review or a request into a traceable, step-by-step delivery plan.
- **joedocs-builder** executes an approved plan one reviewed, committed step at a time, logging evidence in the plan.
- **joedocs-code-review** reviews the result against the plan and records findings that feed the next plan.

## Documentation

- [Deployment guide](https://github.com/joelee/joedocs-mcp/blob/main/docs/deployment.md): Linux, macOS, Docker, remote access, upgrades
- [Reference](https://github.com/joelee/joedocs-mcp/blob/main/docs/reference.md): commands, MCP tools, search syntax, front matter, errors
- [Configuration](https://github.com/joelee/joedocs-mcp/blob/main/docs/configuration.md): every key, its default and its environment override
- [Skills](https://github.com/joelee/joedocs-mcp/blob/main/docs/skills/README.md): agent skills built on JoeDocs
- [Architecture](https://github.com/joelee/joedocs-mcp/blob/main/docs/architecture.md) and [design plan](https://github.com/joelee/joedocs-mcp/blob/main/docs/plan.md)
- [Developer guide](https://github.com/joelee/joedocs-mcp/blob/main/docs/developer-guide.md), [directory structure](https://github.com/joelee/joedocs-mcp/blob/main/docs/directory-structure.md) and [backlog](https://github.com/joelee/joedocs-mcp/blob/main/docs/backlog.md)
- [Changelog](https://github.com/joelee/joedocs-mcp/blob/main/CHANGELOG.md)

## Development

From a clone, on Linux or macOS with `rustup`, `just`, `git`, `gitleaks` and `pre-commit`:

```bash
just setup          # once: cargo tools, Cargo.lock, git hooks
just ci             # the full quality gate, same as CI
just docker-up      # a local server in Docker; data persists in .local/data
```

See the [developer guide](https://github.com/joelee/joedocs-mcp/blob/main/docs/developer-guide.md) for testing, Docker and releasing.

| Item | Value |
| --- | --- |
| MSRV profile | application (see `AGENTS.md`) |
| CI platform | GitHub Actions ([`ci.yml`](https://github.com/joelee/joedocs-mcp/blob/main/.github/workflows/ci.yml): `just ci` on Linux and macOS, and `just docker-test`) |
| Licence | [Apache-2.0](https://github.com/joelee/joedocs-mcp/blob/main/LICENSE) |
