# Reference

The command line, the MCP tools, the search syntax, the front matter and the error codes. These are the stable interfaces that the version number covers (see `AGENTS.md`). Configuration keys are in [configuration.md](configuration.md), and setting up a server is in [deployment.md](deployment.md).

## Command line

```text
joedocs [--config PATH] [--log-level LEVEL] <COMMAND>
```

| Command | What it does |
| --- | --- |
| `init` | Create the vault, the state directory, the database and the history. Safe to rerun. |
| `serve` | Run the MCP server on `server.bind` until Ctrl-C or SIGTERM. Rebuilds the index first if it is behind the history, and rewrites each project's `INDEX.md`. |
| `project add NAME --key KEY [--title TITLE]` | Register a project. `NAME` is a slug (its folder); `KEY` is 1 to 10 upper-case letters and digits, the prefix of its document IDs (`JD` gives `JD-42`). Neither can change later. |
| `project list` | List projects. |
| `key create NAME [--grant PROJECT:PERMISSION]...` | Create an API key and print its token once, alone on the last line. `PERMISSION` is `read` or `write`. `NAME` is recorded as the author of every change made with the key. |
| `key list` | List keys and their grants (never the tokens). |
| `key grant NAME PROJECT:PERMISSION...` | Add grants to a key, or change their permission. |
| `key rotate NAME` | Print a new token for a key, alone on the last line. The old token stops working at once; the key keeps its name and grants. |
| `key revoke NAME` | Revoke a key immediately and for good. |
| `archive list [--project NAME]` | List archived documents, most recently archived first. |
| `archive restore DOCUMENT_ID` | Bring an archived document back as it was before it was archived. |
| `backup FILE.tgz` | Write a backup of the vault, the database and the history to `FILE.tgz` (see [Backups](#backups)). Safe while `serve` runs: writes wait for the snapshot, which takes the write lock only while copying. Refuses an existing `FILE`. The file is created with mode `0600` because it holds API key hashes. |
| `agents` | Print an AGENTS.md-style guide for AI agents that maintain this server: what is safe to run, what needs the operator's approval, what never to do, how to upgrade and troubleshoot. It is filled in with this installation's version, config file, vault and state paths, listen address and systemd unit, and never contains a secret. For example, `joedocs agents > AGENTS.md` in the directory an agent works from. |
| `status` | Show where the service runs and what the store holds, without secrets: the version, host name and config file; `server.bind` and whether `GET /health` answers there; the systemd user and system unit state with its `MainPID`; the vault and state directory with their sizes and file counts, and the database and history sizes; the history head and last change; whether the index and `INDEX.md` files are current; API key counts; and live and archived documents per project. Changes nothing, and works with the service stopped. |
| `get DOCUMENT_ID [--version N] [--content-only \| --front-matter-only] [-f PATH]` | Print a document as `get_document` returns it: the front matter block and the body. `--content-only` prints only the body (what `update_document` takes as `content`); `--front-matter-only` prints only the front matter YAML, without the `---` lines, so it can be piped to a YAML tool. `--version N` prints an earlier revision. `-f`/`--file PATH` writes the same text to `PATH`, replacing it, instead of printing it. An archived document fails with exit `65`; `archive restore` brings it back. |
| `reindex` | Rebuild the document index and search from the history, and rewrite each project's `INDEX.md`. Safe to rerun; projects and keys are kept. |
| `check` | Report drift between the vault, the history and the index, and `INDEX.md` files that are missing or out of date, and say how to resolve it. Changes no document, index or `INDEX.md` (opening the store may add the `INDEX.md` rule to the history's `info/exclude` once). |
| `systemd install [--system --run-as USER] [--dry-run]` | Write a systemd unit that runs `serve` with the configuration in effect, then enable and (re)start it. Linux only; see [deployment.md](deployment.md#linux-systemd). |
| `systemd remove [--system] [--dry-run]` | Stop, disable and delete the unit. |
| `config show` | Show the effective configuration and where each value came from. |
| `config paths` | List the config file search order and which file was selected. |

`key create`, `key rotate` and the admin commands change the store directly, so run them on the server, as the account that runs `serve`.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | `check` found problems. |
| `2` | Usage error (unknown command or flag). |
| `65` | Invalid request: a bad name, an unknown project or key, or a store that is not initialised. |
| `69` | `serve` cannot listen, or `systemctl` cannot be run. |
| `70` | Database, history or vault fault. |
| `74` | Output error. |
| `78` | Configuration error, naming the file, variable or key. |

## MCP tools

The server speaks MCP Streamable HTTP on `/mcp`, with `Authorization: Bearer jd_...`. `GET /health` answers `ok` without authentication.

Every tool returns structured JSON. A tool takes only the parameters listed; an unknown one is an error, so typos fail instead of being ignored. Documents are named by ID (`JD-42`, any case); projects by name.

### Reading

| Tool | Parameters | Returns |
| --- | --- | --- |
| `list_projects` | none | The projects this key can use, with `permission` and document count, plus the allowed `types`, `statuses`, `default_type` and `default_status`. Call it first. |
| `list_documents` | `project`, `type` (default `all`), `status`, `limit` (1 to 1000, default 200), `start` (default 1), `sort` (`updated_desc` (default), `updated_asc`, `created_desc`, `created_asc`, `title`) | Metadata only: `items`, `total`, `next_start`. |
| `search_documents` | `project` (a name, or `"*"` for every project the key can read), `type` (default `all`), `query`, `filters`, `limit` (1 to 200, default 20), `start` | `items` with metadata, `score` and `snippet`, best first; `total`, `next_start`. See [Search](#search). |
| `get_document` | `document_id`, `version` (omit for the latest) | `markdown` (front matter and body), `document` (metadata) and `revision`. |
| `get_document_history` | `document_id`, `limit` (1 to 1000, default 50), `start` | Revisions newest first: number, action, author key, client, summary and time. |
| `diff_document` | `document_id`, `from`, `to` (omit for the latest), `context` (0 to 100, default 3) | A unified diff of the whole file, front matter included, and `changed`. |

### Writing

Every change is one new revision and one git commit. `update_document`, `edit_document` and `change_document_front_matter` refuse a call that would change nothing with `invalid_argument`, and write nothing. `rename_document` and `move_document` likewise refuse the current title or type, and `restore_document_version` refuses the latest revision. Restoring an earlier revision whose content equals the latest still writes a revision. `summary`, where accepted, becomes the commit message. `expected_revision` is the revision you last read: the change is refused with `conflict` if the document has moved on since. It is required by `update_document` and optional elsewhere.

| Tool | Parameters | Effect |
| --- | --- | --- |
| `new_document` | `project`, `title`, `content` (the body), `type`, `status`, `front_matter` (custom keys, `tags`, `aliases`), `summary` | Creates revision 1 and returns the new ID and path. |
| `update_document` | `document_id`, `content`, `expected_revision`, `front_matter`, `summary` | Replaces the body; `front_matter` keys are set as in `change_document_front_matter`. Refused (`field: content`) if neither the body nor the front matter would change. |
| `edit_document` | `document_id`, `edits` (1 to 64 of `{old, new}`), `expected_revision`, `summary` | Exact replacements in the body, in order. Each `old` must occur exactly once; if any edit fails, nothing changes and the error names it. Refused (`field: edits`) if the edits leave the body as it was. |
| `append_to_document` | `document_id`, `content`, `expected_revision`, `summary` | Adds Markdown to the end of the body, after a blank line. |
| `change_document_front_matter` | `document_id`, `set` (object), `remove` (list of keys), `expected_revision` | Sets and removes front matter keys without touching the body. Refused (`field: front_matter`) if the keys already have these values, or a removed key is not set. |
| `rename_document` | `document_id`, `title`, `expected_revision` | Changes the title and the file name; the old title is added to `aliases`. |
| `move_document` | `document_id`, `type`, `expected_revision` | Changes the type (folder). The ID, title and content stay the same. |
| `restore_document_version` | `document_id`, `version`, `expected_revision`, `summary` | Makes an earlier revision's body, status, tags and custom keys the new latest revision. Title, type and aliases stay current. |
| `archive_document` | `document_id`, `reason` | Removes the document from the vault, lists and search. An administrator can bring it back with `joedocs archive restore`. |

### Errors

A failed tool call is a result with `isError: true` and `structuredContent.error = {code, message, ...}`:

| Code | Meaning |
| --- | --- |
| `invalid_argument` | A parameter is wrong; `field` names it. |
| `not_found` | No such project or document, or the key has no grant on it (the two look the same, so nothing leaks). |
| `archived` | The document is archived. |
| `already_exists` | The name or title is taken. |
| `conflict` | `expected_revision` is stale; `current_revision` says what to read and merge. |
| `integrity` | The file was changed outside JoeDocs, so JoeDocs refuses to overwrite it. `joedocs check` explains how to resolve it. |
| `too_large` | The document is over `server.max_document_bytes`. |
| `permission_denied` | The key can only read this project. |
| `internal` | A server fault; details are in the server log, not the result. |

## Search

`search_documents` takes a text `query`, front matter `filters`, or both.

**Query.** Words match titles, aliases and bodies, ignoring case, accents and word endings (`plans` matches `plan`). Title matches rank above body matches.

| Syntax | Matches |
| --- | --- |
| `rate limit` | documents with both words |
| `"rate limit"` | the words in that order |
| `-redis` | excludes documents with the word |
| `auth*` | words starting with `auth` |
| `redis OR valkey` | either word |

No query is a syntax error: an unclosed quote runs to the end, and terms with no letter or digit are ignored. A query of exclusions only is rejected. Snippets mark matches between `«` and `»`.

**Filters** are an object keyed by front matter key, all of which must hold. A plain value means equals. Otherwise give an object of operators:

| Operator | Example |
| --- | --- |
| `eq`, `ne` | `{"status": {"ne": "done"}}` |
| `in` | `{"status": {"in": ["draft", "review"]}}` |
| `contains` | `{"tags": {"contains": "security"}}` (for lists) |
| `gt`, `gte`, `lt`, `lte` | `{"updated": {"gte": "2026-10-01"}}`, `{"priority": {"lte": 2}}` |
| `exists` | `{"approved_by": {"exists": true}}` |

Every key works the same way, server keys and custom keys alike. Text compares ignoring case, numbers compare as numbers, and dates compare as text, so RFC 3339 prefixes such as `2026-10-01` work. Without a `query`, results are newest first.

## Documents

Each document is `<storage.home>/<project>/<type>/<Title>.md`. Characters that Obsidian rejects in file names are left out of the file name, and a clash in one folder gets ` (JD-42)` appended.

### INDEX.md

With `documents.index_file` on (the default), each project folder also holds `<storage.home>/<project>/INDEX.md`, a table of the project's live documents for finding and opening one by ID:

```markdown
# JoeDocs (JD)

Generated by JoeDocs after every change. Do not edit: changes are overwritten.

| ID | Title | Type | Status | Updated |
| --- | --- | --- | --- | --- |
| JD-3 | [Delivery plan - JoeDocs 0.1.2 release](plans/Delivery%20plan%20-%20JoeDocs%200.1.2%20release.md) | plans | approved | 2026-11-07 |
```

- Rows are sorted by the number in the ID (`JD-2` before `JD-10`); archived documents are left out. A project with no documents shows "No documents yet."
- Each title links to its file with a path relative to `INDEX.md`, percent-encoded, so the link works in Obsidian, on GitHub and in any editor, wherever the folder is copied. Updated is the UTC date of the latest revision.
- JoeDocs rewrites the file after every change to the project's documents and when the project is added, and leaves it alone when its content would not change. A failure to write it is logged as a warning and does not fail the change.
- It is not a document and never enters the history: `init` and every later start add `/*/INDEX.md` to the history's `info/exclude`. `<project>/INDEX.md` of a registered project is a reserved path: `check` never reports it as an unknown file, but reports it when it is missing or out of date.
- `serve` rewrites every project's `INDEX.md` when it starts, and so does `reindex`.
- With `documents.index_file` off, JoeDocs neither writes nor checks it, and leaves an existing one in place.

### Front matter

```yaml
---
id: JD-42
title: Search query language
project: joedocs
type: specs
status: approved
revision: 4
created: 2026-11-04T09:12:03Z
created_by: laptop
updated: 2026-11-05T14:40:10Z
updated_by: desktop
client: claude-code
aliases: [Search syntax]
tags: [search, fts]
priority: 2
---
```

| Class | Keys | Rules |
| --- | --- | --- |
| Server-owned | `id`, `title`, `project`, `type`, `revision`, `created`, `created_by`, `updated`, `updated_by`, `client` | Written by JoeDocs. `title` changes only through `rename_document` and `type` only through `move_document`. `created_by` and `updated_by` are API key names; `client` is the MCP client's name. |
| Validated | `status`, `tags`, `aliases` | Can be set, never removed. `status` must be one of `documents.statuses`. Tags are letters, digits, `_`, `-` or `/`. |
| Custom | anything else | Names of up to 64 letters, digits, `_` or `-`. Values are strings, numbers, booleans or lists of those; `null` and nested objects are rejected (use `remove` to delete a key). |

Server keys come first in a fixed order, then custom keys in the order they were added. Comments in client-supplied YAML are not kept.

### Backups

`joedocs backup FILE.tgz` writes a gzip-compressed tar archive, format 1:

| Entry | What it is |
| --- | --- |
| `manifest.json` | `format` (1), `joedocs_version`, `schema_version` (the database's `PRAGMA user_version`), `created_at` (UTC), `history_head` (the history's latest commit), `documents` (live documents), and `vault` and `state_dir`, where they were (for information only). |
| `state/joedocs.db` | A copy of the database made with SQLite `VACUUM INTO`. |
| `state/history.git/` | The history repository. |
| `content/` | Every regular file and directory of the vault, including `INDEX.md` files and dot folders such as `.obsidian`. Symlinks are left out and reported. |

The lock file is not included. The snapshot is taken under the write lock, so it never holds half of a change.

### History

The history is a git repository at `<storage.state_dir>/history.git` whose work tree is the vault. Each revision is one commit, authored by the API key name, with trailers:

```text
Update JD-42: tighten FTS grammar

JoeDocs-Document: JD-42
JoeDocs-Revision: 4
JoeDocs-Action: update
JoeDocs-Client: claude-code
```

Actions are `create`, `update`, `edit`, `append`, `front_matter`, `rename`, `move`, `restore`, `archive` and `unarchive`. Inspect it with plain git, for example `git --git-dir <state_dir>/history.git log --format='%ad %an | %s'`.
