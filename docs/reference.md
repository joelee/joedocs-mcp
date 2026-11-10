# Reference

The command line, the MCP tools, the search syntax, the front matter and the error codes. These are the stable interfaces that the version number covers (see `AGENTS.md`). Configuration keys are in [configuration.md](configuration.md), and setting up a server is in [deployment.md](deployment.md).

## Command line

```text
joedocs [--config PATH] [--log-level LEVEL] [--server URL] <COMMAND>
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
| `key ungrant NAME PROJECT...` | Remove a key's grants on these projects, all or nothing, and print its remaining grants. Refuses (exit `65`, changing nothing) an unknown or revoked key, an unknown project, and a project the key has no grant on. The key stays active, even with no grants left; its next MCP request no longer reaches those projects. |
| `key rotate NAME` | Print a new token for a key, alone on the last line. The old token stops working at once; the key keeps its name and grants. |
| `key revoke NAME` | Revoke a key immediately and for good. |
| `archive list [-p\|--project NAME] [-t\|--type TYPE]` | List archived documents, most recently archived first, of one project or type (an unknown type exits `65`). |
| `archive restore DOCUMENT_ID` | Bring an archived document back as it was before it was archived. |
| `document convert DOCUMENT_ID` | Make a single-file document multi-file: its file moves from `<type>/<Title>.md` to `<type>/<Title>/<Title>.md` as one revision (action `convert`), keeping its ID, history (`git log --follow` follows it), front matter and body. Its file list, `get --archive --version N`, the main file's history and folder diffs cover the revisions before the conversion too. Refuses a document that is already multi-file. |
| `document move-file DOCUMENT_ID FILE --to TARGET_ID [--as NEW_FILE]` | Move a file of a multi-file document to another multi-file document, under the same path or `NEW_FILE`, in one commit that raises both documents' revisions (action `move_file`). The file's front matter then names the target. Refuses a single-file target, the same document, the main file, and a path taken in the target. |
| `document restore-file DOCUMENT_ID FILE --version N` | Bring back one file of a multi-file document as it was at document revision `N` (`FILE` is its path then), as one revision (action `restore_file`): its content then, at its path then, whether it has since changed, been renamed or been archived. Refuses a file already as it was, a path another file now holds, and a path where a file the index does not know is. |
| `backup FILE.tgz` | Write a backup of the vault, the database and the history to `FILE.tgz` (see [Backups](#backups)). Safe while `serve` runs: writes wait for the snapshot, which takes the write lock only while copying. Refuses an existing `FILE`. The file is created with mode `0600` because it holds API key hashes. |
| `backup --restore [--state-dir PATH] [--content-dir PATH] FILE.tgz` | Restore a backup into a state directory and a vault that are empty or do not exist yet (by default the configured `storage.state_dir` and `storage.home`). Refuses an unknown archive format, a database schema newer than this build, and any entry that is not a regular file or directory under `state/` or `content/` (absolute paths, `..`, links, devices), and a damaged or truncated archive (its gzip checksum or length does not match). Then it opens the restored store, which migrates an older database; if extracting, opening or migrating fails, it removes what it extracted, so the targets are empty (or absent) again and the restore can be retried. Then it runs `check` (a restore whose `check` finds problems keeps its data and exits `1`), and prints the `[storage]` settings to use when it restored elsewhere. Run it with the service stopped. |
| `agents` | Print an AGENTS.md-style guide for AI agents that maintain this server: what is safe to run, what needs the operator's approval, what never to do, how to upgrade and troubleshoot. It is filled in with this installation's version, config file, vault and state paths, listen address and systemd unit, and never contains a secret. For example, `joedocs agents > AGENTS.md` in the directory an agent works from. |
| `status` | Show where the service runs and what the store holds, without secrets: the version, host name and config file; `server.bind` and whether `GET /health` answers there; the systemd user and system unit state with its `MainPID`; the vault and state directory with their sizes and file counts, and the database and history sizes; the history head and last change; whether the index and `INDEX.md` files are current; API key counts; and live and archived documents per project. Changes nothing, and works with the service stopped. |
| `admin-token create NAME` | Create an admin token for the remote admin API, with a new TOTP secret. Prints the secret in base32 (type it into Google Authenticator or another TOTP app) and the `otpauth://totp/JoeDocs:NAME?...` URI (pipe it to `qrencode -t ansiutf8` for a QR code), then the token alone on the last line, `jda_<12 hex id>_<64 hex secret>`. Both are shown once. Run it on the server only: no remote call can create, list or revoke admin tokens. |
| `admin-token list` | List admin tokens: name, creation, last use, and whether each is active, locked after failed logins, or revoked. Never the secrets. |
| `admin-token revoke NAME` | Revoke an admin token and end every session it opened, immediately and for good. |
| `get DOCUMENT_ID [--version N] [--content-only \| --front-matter-only] [-f PATH] [--part FILE \| --archive OUT.tgz]` | Print a document as `get_document` returns it: the front matter block and the body. `--content-only` prints only the body (what `update_document` takes as `content`); `--front-matter-only` prints only the front matter YAML, without the `---` lines, so it can be piped to a YAML tool. `--version N` prints an earlier revision. `-f`/`--file PATH` writes the same text to `PATH`, replacing it, instead of printing it: through a new, randomly named temporary file in the same directory, so a file or link already there is never followed, and keeping `PATH`'s mode (a new file is `0600`). An archived document fails with exit `65`; `archive restore` brings it back. For a multi-file document, `get` prints the main file and then, under `--- Other files of JD-42 ---`, a table of its other files (path, revision, updated); `--content-only`, `--front-matter-only` and `-f` act on the main file alone. `--part FILE` prints one of its files instead (by its path in the folder, with the same modifiers, `--version` and `-f`). `--archive OUT.tgz` writes every file of the document (at `--version N`, or the latest) to a gzip tar with paths relative to its folder, replacing `OUT.tgz` the way `-f` replaces a file; it cannot be combined with `-f`, `--part` or the modifiers. `--part` and `--archive` on a single-file document exit `65`. |
| `list -p\|--project NAME [-t\|--type TYPE] [-s\|--status STATUS] [--sort ORDER] [--start N] [-n\|--limit N] [-j\|--json]` | List a project's live documents, without content, as `list_documents` does: a table of `ID`, `TYPE`, `STATUS`, `REV`, `FILES` (the other files of a multi-file document, `-` for a single file), `UPDATED` and `TITLE`. `ORDER` is `updated-desc` (the default), `updated-asc`, `created-desc`, `created-asc` or `title`. `--limit` is 1 to 1000 (default 200); when more documents match, a last line reads `Showing 1-200 of 512; next page: --start 201`. No match prints `No documents.` `-j`/`--json` prints the `list_documents` result as pretty JSON instead, the same locally and remotely. Control characters in the table's text are printed as escapes (`\n`, `\t`, `\u{1b}`). An unknown project, type or status, or a bad start or limit, exits `65`. |
| `search [QUERY...] [-p\|--project NAME] [-t\|--type TYPE] [-s\|--status STATUS] [-F\|--filter KEY=VALUE]... [--filters JSON] [-d\|--document DOCUMENT_ID] [--start N] [-n\|--limit N] [-j\|--json]` | Search live documents and their files as `search_documents` does, in every project (or only `--project`). `QUERY` uses the [search syntax](#search) (the words are joined with spaces); leave it out to search by front matter only. `--status S` is short for `--filter status=S`; each `--filter KEY=VALUE` requires that key to equal that text, and `--filters` takes a JSON object with the operators of `search_documents` (use it for numbers, booleans and ranges); they are merged, and a key given twice, `--filter` without `=`, or `--filters` that is not a JSON object exits `65`. Filters test the matched file's own front matter. `--document` searches one document's files. Prints a table of `ID`, `FILE` (the matched file of a multi-file document, `-` for a main file), `TYPE`, `STATUS`, `UPDATED` and `TITLE`, each hit followed by its snippet, indented, with matches between `«` and `»`; then the paging line as for `list`, or `No matches.` `--limit` is 1 to 200 (default 20). `-j`/`--json` prints the `search_documents` result. |
| `history DOCUMENT_ID [-P\|--part FILE] [--start N] [-n\|--limit N] [-j\|--json]` | List a document's revisions, newest first, as `get_document_history` does: a table of `REV`, `TIME`, `ACTION`, `AUTHOR`, `CLIENT` and `SUMMARY`, then the paging line as for `list`. `--part FILE` lists only the revisions that changed that file of a multi-file document, following its renames. `--limit` is 1 to 1000 (default 50). `-j`/`--json` prints the `get_document_history` result. An unknown or archived document exits `65`. |
| `diff DOCUMENT_ID --from N [--to M] [--context N] [-P\|--part FILE] [-j\|--json]` | Print the unified diff from revision `N` to `M` (default: the latest), as `diff_document` does: the whole file, front matter included, or for a multi-file document every file that changed, each headed by its path; `--part FILE` diffs one file (its path at `M`). `--context` is 0 to 100 lines (default 3). When nothing changed it prints `No changes between revisions N and M.` `-j`/`--json` prints the `diff_document` result. A revision that does not exist, or an unknown or archived document, exits `65`. |
| `audit [--since TIME] [--until TIME] [-k\|--key NAME] [--tool TOOL] [-d\|--document DOCUMENT_ID] [--outcome OUTCOME] [--start N] [-n\|--limit N] [-j\|--json]` | Read the audit log, newest first: one row for every MCP call that changes (or tries to change) a document, and for every admin API call. MCP reads are not audited, and neither are commands run on the server's command line. A table of `TIME`, `KEY` (the API key or admin token), `CLIENT`, `TOOL` (`admin:<operation>` for the admin API), `DOCUMENT` (`-` for none) and `OUTCOME` (`ok` or an error code), then the paging line as for `list`, or `No audit entries.` `TIME` is a UTC date (`2026-11-10`, its midnight) or time (`2026-11-10T14:00:00Z`); `--since` is inclusive and `--until` exclusive. `--tool` matches one tool, or with a trailing `*` every tool that starts with the rest (`admin:*`), matching case. `--limit` is 1 to 1000 (default 50). `-j`/`--json` prints `{"items": [{"at", "key_id", "key_name", "client", "tool", "document_id"?, "outcome"}], "total", "next_start"}`. A bad time, document ID, start or limit exits `65`. |
| `reindex` | Rebuild the document index and search from the history, and rewrite each project's `INDEX.md`. Safe to rerun; projects and keys are kept. |
| `check` | Report drift between the vault, the history and the index, and `INDEX.md` files that are missing or out of date, and say how to resolve it. Changes no document, index or `INDEX.md` (opening the store may add the `INDEX.md` rule to the history's `info/exclude` once). |
| `systemd install [--system --run-as USER] [--dry-run]` | Write a systemd unit that runs `serve` with the configuration in effect, then enable and (re)start it. Linux only; see [deployment.md](deployment.md#linux-systemd). |
| `systemd remove [--system] [--dry-run]` | Stop, disable and delete the unit. |
| `config show` | Show the effective configuration and where each value came from. |
| `config paths` | List the config file search order and which file was selected. |
| `login [--code CODE]` | With a server configured (`client.server` or `--server URL`): log in to its admin API with `JOEDOCS_ADMIN_TOKEN` and a 6-digit code from your authenticator (asked for when `--code` is left out), and keep the session for later commands (see [Client mode](#client-mode)). With no server configured, at a terminal, it asks for the server's URL first and saves it as `client.server`. |
| `logout` | End the admin session with the server and forget it. |

Without a server configured, every command works on the local store, so run admin commands on the server, as the account that runs `serve`. With one, they run on that server (see [Client mode](#client-mode)).

### Client mode

When `client.server` is set, or `--server URL` is given, `project`, `key`, `archive`, `document`, `check`, `reindex`, `status`, `get`, `list`, `search`, `history`, `diff`, `audit` and `backup FILE` run on that server through its [admin API](#admin-api). They print the same output and exit with the same codes as on the server; `status` adds the server URL and the session's expiry, and `backup` downloads the archive to `FILE` (mode `0600`). `init`, `serve`, `systemd`, `admin-token`, `agents` and `backup --restore` run only on the server and exit `65` with a server configured; `config` always describes this machine.

- `joedocs login` with no server configured, when standard input and standard error are terminals, asks for the server's URL (`Server URL (for example https://docs.example.ts.net): `). It checks the URL as below and that `GET <URL>/health` answers `ok` without a redirect, asks again up to three times in all, and otherwise exits `65` without saving. The URL is saved as `client.server` in the config file in use, or else `$XDG_CONFIG_HOME/joedocs/config.toml` (when `XDG_CONFIG_HOME` is absolute) or `~/.config/joedocs/config.toml`; comments and other settings in that file are kept, a new file is `0600` and missing directories `0700`. It prints `Saved client.server in <file>.` and goes on to log in. Without a terminal it exits `65`, and `--server URL` is used for that run only, never saved. The admin token is never asked for or saved.
- `joedocs login` opens a session with the admin token from `JOEDOCS_ADMIN_TOKEN` and a TOTP code. The session lasts `admin.session_hours` (8 by default) and is kept in `$XDG_STATE_HOME/joedocs/sessions.toml`, or `~/.local/state/joedocs/sessions.toml` (file `0600`, directory `0700`), one entry per server URL. Every call sends both the token and the session, so the file alone is not enough to act.
- When a session has expired, or there is none, and standard input and standard error are terminals, a command asks for a new code (`Session expired. Code for <server>: `), logs in, and runs once more. The server checks the session before it runs anything, so a change is never applied twice. A wrong code is asked for again, up to three times in all. Without a terminal (scripts, agents, CI) the command exits `77` and says to run `joedocs login`.
- A missing `JOEDOCS_ADMIN_TOKEN`, refused codes, or a token locked after five failed logins exit `77` without running the command; a server that cannot be reached exits `69`.
- `client.server` must be `https://` (rustls with the operating system's root certificates), or `http://` to `localhost`, `127.0.0.1` or `[::1]`.
- The client never follows a redirect: a `3xx` reply exits `70` and names its `Location`, so set `client.server` to the final URL. It gives up when the server sends nothing for 60 seconds (exit `69`), however long a download that keeps arriving takes, and removes a partial download.
- A command whose route the server does not have (a server running an older JoeDocs answers `404` with `no such admin API route`) exits `65` and says to upgrade the server.
- A reply that is not what the admin API sends (an HTML page from a proxy, or JSON of the wrong shape) exits `70` before anything is printed or a file is replaced. A backup download that is not `application/gzip`, or has no readable manifest, is removed.
- Logs name the command only, never its arguments, so a `--code` never appears in them.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | `check` found problems. |
| `2` | Usage error (unknown command or flag). |
| `65` | Invalid request: a bad name, an unknown project or key, or a store that is not initialised. |
| `69` | `serve` cannot listen, `systemctl` cannot be run, or the `client.server` cannot be reached. |
| `70` | Database, history or vault fault, or an admin API reply the client does not understand (including a redirect). |
| `74` | Output error. |
| `77` | The admin API refused the credentials: no `JOEDOCS_ADMIN_TOKEN`, a wrong code, a locked token, or no valid session where no code can be asked for (run `joedocs login`). |
| `78` | Configuration error, naming the file, variable or key. |

## MCP tools

The server speaks MCP Streamable HTTP on `/mcp`, with `Authorization: Bearer jd_...`. `GET /health` answers `ok` without authentication.

Every tool returns structured JSON. A tool takes only the parameters listed; an unknown one is an error, so typos fail instead of being ignored. Documents are named by ID (`JD-42`, any case); projects by name.

### Reading

| Tool | Parameters | Returns |
| --- | --- | --- |
| `list_projects` | none | The projects this key can use, with `permission` and document count, plus the allowed `types`, `statuses`, `default_type` and `default_status`. Call it first. |
| `list_documents` | `project`, `type` (default `all`), `status`, `limit` (1 to 1000, default 200), `start` (default 1), `sort` (`updated_desc` (default), `updated_asc`, `created_desc`, `created_asc`, `title`) | Metadata only: `items`, `total`, `next_start`. |
| `search_documents` | `project` (a name, or `"*"` for every project the key can read), `type` (default `all`), `query`, `filters`, `limit` (1 to 200, default 20), `start`, `document_id` (only that document's files) | `items` with metadata, `score` and `snippet`, and `file` for a hit on a file of a multi-file document, best first; `total`, `next_start`. See [Search](#search). |
| `get_document` | `document_id`, `version` (omit for the latest), `file` (a file of a multi-file document; omit for the main file) | `markdown` (front matter and body), `document` (metadata) and `revision`. For a multi-file document also `files` (its other files at that revision) and, when `file` was given, `file`. |
| `list_document_files` | `document_id`, `version` (omit for the latest) | `document_id`, `revision` and `files`: the main file first, then the other files by path, each with `file`, `main`, `revision` (the document revision at which it last changed) and `updated`. |
| `get_document_history` | `document_id`, `limit` (1 to 1000, default 50), `start`, `file` (a file of a multi-file document) | Revisions newest first: number, action, author key, client, summary and time. With `file`, only the revisions that changed that file, following it back through its renames to its creation, each with `file`, its path then; an archived file's history can still be read. Naming the main file lists the revisions that changed it. |
| `diff_document` | `document_id`, `from`, `to` (omit for the latest), `context` (0 to 100, default 3), `file` (a file of a multi-file document, by its path at `to`) | A unified diff of the whole file, front matter included, and `changed`. For a multi-file document, `files` lists the files covered and `diff` holds one diff per changed file, each headed `--- JD-1 revision N: <file>`; a file that exists at only one of the revisions shows as wholly added or removed. With `file`, only that file, followed through its renames between the two revisions. |

### Writing

Every change is one new revision and one git commit. `update_document`, `edit_document` and `change_document_front_matter` refuse a call that would change nothing with `invalid_argument`, and write nothing. `rename_document` and `move_document` likewise refuse the current title or type, and `restore_document_version` refuses the latest revision. Restoring an earlier revision whose content equals the latest still writes a revision. `summary`, where accepted, becomes the commit message. `expected_revision` is the revision you last read: the change is refused with `conflict` if the document has moved on since. It is required by `update_document` and optional elsewhere.

For a multi-file document, `update_document`, `edit_document`, `append_to_document` and `change_document_front_matter` take `file` to change one file instead of the main file, and `rename_document` takes `file` and `new_file` (instead of `title`) to rename or move a file within the document. A change to one file, the main file's content and front matter included, is refused with `conflict` only when that file changed after `expected_revision`, or `expected_revision` is ahead of the document, so agents changing different files of a document do not get in each other's way; the error then also gives `file` and `file_revision`. Renaming, moving, restoring and archiving the whole document still need the current revision.

| Tool | Parameters | Effect |
| --- | --- | --- |
| `new_document` | `project`, `title`, `content` (the body), `type`, `status`, `front_matter` (custom keys, `tags`, `aliases`), `summary`, `multifile` (default `false`) | Creates revision 1 and returns the new ID and path. With `multifile: true` the document is a folder (see [Multi-file documents](#multi-file-documents)). |
| `new_document_file` | `document_id` (a multi-file document), `file` (its path in the folder, e.g. `backlog/item-1`, with or without `.md`), `content`, `front_matter`, `expected_revision`, `summary` | Adds a file as one new revision and returns the revision, `path` and `file`. Refused with `conflict` only when `expected_revision` is ahead of the document, since no existing file changes. |
| `update_document` | `document_id`, `content`, `expected_revision`, `front_matter`, `summary` | Replaces the body; `front_matter` keys are set as in `change_document_front_matter`. Refused (`field: content`) if neither the body nor the front matter would change. |
| `edit_document` | `document_id`, `edits` (1 to 64 of `{old, new}`), `expected_revision`, `summary` | Exact replacements in the body, in order. Each `old` must occur exactly once; if any edit fails, nothing changes and the error names it. Refused (`field: edits`) if the edits leave the body as it was. |
| `append_to_document` | `document_id`, `content`, `expected_revision`, `summary` | Adds Markdown to the end of the body, after a blank line. |
| `change_document_front_matter` | `document_id`, `set` (object), `remove` (list of keys), `expected_revision` | Sets and removes front matter keys without touching the body. Refused (`field: front_matter`) if the keys already have these values, or a removed key is not set. |
| `rename_document` | `document_id`, `title`, `expected_revision`; or `document_id`, `file`, `new_file`, `expected_revision` | Changes the title and the file name; the old title is added to `aliases`. With `file` and `new_file`, renames or moves that file within its multi-file document. |
| `write_document_files` | `document_id` (a multi-file document), `files` (each `{file, content, front_matter}`), `expected_revision`, `summary` | Creates or replaces several files in one revision and one commit. All or nothing: every path, limit, size and conflict is checked first. An existing file's `front_matter` keys are merged as in `update_document`; a file given with the content and front matter it already has is left alone, and a call that changes nothing is refused (`field: files`). Returns the revision and the files written, each with `file`, `path` and `created`. The main file is changed with the other tools. |
| `move_document` | `document_id`, `type`, `expected_revision` | Changes the type (folder). The ID, title and content stay the same. |
| `restore_document_version` | `document_id`, `version`, `expected_revision`, `summary` | Makes an earlier revision's body, status, tags and custom keys the new latest revision. Title, type and aliases stay current. |
| `archive_document` | `document_id`, `reason` | Removes the document from the vault, lists and search. An administrator can bring it back with `joedocs archive restore`. |
| `archive_document_file` | `document_id` (a multi-file document), `file`, `expected_revision`, `reason` | Removes one file (not the main file) from the vault, the file list and search, as one new revision. Its history stays readable. Refused with `conflict` if the file changed after `expected_revision`. |

### Errors

A failed tool call is a result with `isError: true` and `structuredContent.error = {code, message, ...}`:

| Code | Meaning |
| --- | --- |
| `invalid_argument` | A parameter is wrong; `field` names it. |
| `not_found` | No such project or document, or the key has no grant on it (the two look the same, so nothing leaks). |
| `archived` | The document is archived. |
| `already_exists` | The name or title is taken. |
| `conflict` | `expected_revision` is stale; `current_revision` says what to read and merge. For a file of a multi-file document, `file` and `file_revision` say which file changed and when. |
| `integrity` | The file was changed outside JoeDocs, so JoeDocs refuses to overwrite it. `joedocs check` explains how to resolve it. |
| `too_large` | The document is over `server.max_document_bytes`. |
| `permission_denied` | The key can only read this project. |
| `internal` | A server fault; details are in the server log, not the result. |

## Admin API

With `[admin] enabled = true`, `joedocs serve` also serves a JSON API under `/admin/v1/`, the one client mode uses. It is never part of the MCP endpoint, and when `admin.enabled` is off every `/admin/v1/` path answers `404`. Requests must pass the `server.allowed_hosts` check, and requests with an `Origin` outside `server.allowed_origins` get `403`.

Every request carries `Authorization: Bearer jda_...` (an admin token from `joedocs admin-token create`). Every route but login also carries `JoeDocs-Session: jds_...`, a session of that same token. Each request, refused or not, writes one row to the audit log with the tool `admin:<operation>` (`admin:unknown` for a path or method that is not a route), and never a token, session or code. Request bodies are limited to 64 KiB.

| Route | Operation | Body or query | Reply |
| --- | --- | --- | --- |
| `POST /admin/v1/session` | `session.create` | `{"code": "123456"}` | `{"session", "expires_at", "name"}` |
| `DELETE /admin/v1/session` | `session.delete` | | `204` |
| `GET /admin/v1/status` | `status` | | the `joedocs status` report as JSON |
| `GET /admin/v1/projects` | `projects.list` | | `{"output"}` |
| `POST /admin/v1/projects` | `projects.add` | `{"name", "key", "title"?}` | `{"output"}` |
| `GET /admin/v1/keys` | `keys.list` | | `{"output"}` |
| `POST /admin/v1/keys` | `keys.create` | `{"name", "grants": ["project:read"]}` | `{"output"}`, ending with the token |
| `POST /admin/v1/keys/{name}/grants` | `keys.grant` | `{"grants": [...]}` | `{"output"}` |
| `POST /admin/v1/keys/{name}/ungrant` | `keys.ungrant` | `{"projects": ["project"]}` | `{"output"}` |
| `POST /admin/v1/keys/{name}/rotate` | `keys.rotate` | | `{"output"}`, ending with the token |
| `POST /admin/v1/keys/{name}/revoke` | `keys.revoke` | | `{"output"}` |
| `GET /admin/v1/archive` | `archive.list` | `?project=NAME&type=TYPE` | `{"output"}` |
| `POST /admin/v1/archive/{id}/restore` | `archive.restore` | | `{"output"}` |
| `GET /admin/v1/audit` | `audit.list` | `?since=TIME`, `until`, `key`, `tool`, `document`, `outcome`, `start`, `limit` | `{"items", "total", "next_start"}`, as `joedocs audit --json` prints (the call itself is audited after it answers) |
| `GET /admin/v1/check` | `check` | | `{"output", "problems"}` |
| `POST /admin/v1/reindex` | `reindex` | | `{"output"}` |
| `GET /admin/v1/documents` | `documents.list` | `?project=NAME` (required), `type`, `status`, `sort` (`updated_desc`, `updated_asc`, `created_desc`, `created_asc`, `title`), `start`, `limit` | the `list_documents` result: `{"items", "total", "next_start"}` |
| `POST /admin/v1/search` | `documents.search` | `{"project"?, "type"?, "query"?, "filters"?, "document_id"?, "start"?, "limit"?}`: no `project`, or `"*"`, searches every project; other fields are refused | the `search_documents` result: `{"items", "total", "next_start"}` |
| `GET /admin/v1/documents/{id}` | `documents.get` | `?version=N`, `?part=FILE` (percent-encoded), `?archive=true` | `{"markdown"}`, with `files` (each `{file, revision, updated}`) for a multi-file document; with `part`, that file's `{"markdown"}`; with `archive=true`, the document's files as a gzip tar (`application/gzip`) |
| `GET /admin/v1/documents/{id}/history` | `documents.history` | `?file=FILE`, `start`, `limit` | the `get_document_history` result: `{"document", "revisions", "total", "next_start"}` |
| `GET /admin/v1/documents/{id}/diff` | `documents.diff` | `?from=N` (required), `to`, `context`, `file` | the `diff_document` result: `{"document_id", "from", "to", "changed", "diff", "files"?}` |
| `POST /admin/v1/documents/{id}/convert` | `document.convert` | | `{"output"}` |
| `POST /admin/v1/documents/{id}/files/move` | `document.move_file` | `{"file", "to", "as"}` (`as` optional) | `{"output"}` |
| `POST /admin/v1/documents/{id}/files/restore` | `document.restore_file` | `{"file", "version"}` | `{"output"}` |
| `GET /admin/v1/backup` | `backup` | | the backup archive (`application/gzip`), with `JoeDocs-Documents` and `JoeDocs-Skipped-Links` headers |

Query values are percent-encoded. `output` is the text the same command prints on the server, and `problems` is the number of problems `check` found (it exits `1` when that is not `0`).

Logins are TOTP codes (RFC 6238: SHA-1, 6 digits, 30 seconds). A code is accepted for its own step or one step either side, and each step only once per token. Five wrong codes in a row lock the token for 15 minutes. A session lasts `admin.session_hours`. Refusals are `{"error": {"code", "message"}}`:

| Status | `code` | Meaning |
| --- | --- | --- |
| `401` | `unauthorized` | The admin token, or the code, is not valid, or the token is revoked. |
| `401` | `no_session` | No session, or one that was ended or belongs to another token. |
| `401` | `session_expired` | The session has expired: log in again. |
| `429` | `locked` | Too many wrong codes; `Retry-After` gives the seconds until logins are accepted again. |
| `403` | `forbidden` | The `Host` or `Origin` is not allowed (checked before anything else). |
| `413` | `payload_too_large` | The request body is over 64 KiB. |
| `400` | `invalid_argument` | A bad request, such as a bad name. |
| `404` | `not_found` | No such project, key or document, or no such route. |
| `409` | `already_exists`, `conflict`, `archived` | The request conflicts with what is stored. |
| `500` | `internal` | A server fault (details are only in the server log). |

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

**Multi-file documents.** Each file of a multi-file document is searched on its own, ranked in the same list as main files: its path (and `aliases`) counts as its title. A hit on such a file carries `file`, and its snippet comes from that file; `filters` apply to the front matter of the file that matched, so `{"status": "planned"}` finds backlog items by their own status. `document_id` restricts the search to one document's files, its main file included. Archived files and archived documents are left out.

## Documents

Each document is `<storage.home>/<project>/<type>/<Title>.md`. Characters that Obsidian rejects in file names are left out of the file name, and a clash in one folder gets ` (JD-42)` appended.

JoeDocs never writes over something in the vault that the index does not know: a file, a directory or a symlink (dangling or not; symlinks are never followed). A document whose new name is taken that way gets ` (JD-42)` appended; a file write, rename, move or restore onto it is refused with `already_exists` and changes nothing. A rename that changes only the case of a name (`Notes` to `notes`) renames in place only when both spellings are the same file, as on a case-insensitive file system (macOS); on a case-sensitive one (Linux), another file at the new spelling counts as taken.

### Multi-file documents

A document created with `multifile: true` is a folder, `<project>/<type>/<Title>/`, holding its main file `<Title>/<Title>.md` (an Obsidian folder note, so `[[Title]]` still links to it) and any other files, such as `<Title>/backlog/item-1.md`, under the same ID. A folder named `INDEX` in any case gets ` (JD-42)` appended, since `INDEX.md` is the folder's file list.

- The main file holds the document's front matter. Every other file has its own: `document` (the ID), `revision` and `updated` first, written by JoeDocs, then any keys of yours. In those files every other key is custom, `status` included (it is not checked against `documents.statuses`); `tags` and `aliases` must still be lists of strings.
- A file is named by its path inside the folder, with or without `.md`: up to 4 directories deep, each part following the title rules (none of `` * " \ < > : | ? # ^ [ ] `` or control characters, not starting with `.`, not ending with a space or `.`, at most 100 bytes). `INDEX` (in any case) and the main file's own name at the top level are reserved, and two files whose paths differ only in case cannot both exist. Nor can two folders: a file cannot go into `Backlog/` while the document has a file in `backlog/`, and a rename cannot change only the case of a file's folder (`backlog/item-1` to `Backlog/item-1`), since a case-insensitive file system (macOS) holds both spellings as one folder. Such a change is refused with `invalid_argument` naming both spellings, and changes nothing. A document holds at most `documents.max_files` files besides its main file (500 by default); a restore that would bring back more is refused with `invalid_argument` and changes nothing.
- Every change to any file raises the document's one revision number and is one history commit. Each file's `revision` is the document revision at which it last changed; changing another file leaves the main file as it is.
- Renaming or moving the document moves the whole folder in one commit; the other files keep their paths inside it and their content. A folder name equal, ignoring case, to one of the document's top-level files is treated as taken, so renaming `Roadmap` (holding `notes`) to `notes` gives the folder `notes (JD-42)`; `archive restore` picks the folder the same way. A restore that would bring back a top-level file named like the current main file is refused with `already_exists`, and so is one that would bring a file back onto a path where a file the index does not know is (nothing is overwritten). Archiving removes the folder, and `joedocs archive restore` brings back every file that was live when it was archived, as new files at the restored revision. `restore_document_version` restores the folder as it was at that revision: the main file's body, status, tags and custom keys (its title, type and aliases stay current), every file that existed then with its content and path, and archives the files made since. Each is one revision. Before any of these, every file of the document is checked against the index, and a file changed by hand stops the change with `integrity`.
- `<Title>/INDEX.md`, generated like the project `INDEX.md`, lists the ID, title and revision and a table of the files with an Obsidian link to each, the revision at which each last changed and its date. The project `INDEX.md` lists the document once, linking its main file, with `(+N files)`.

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
- It is not a document and never enters the history: `init` and every later start add `/*/INDEX.md` and `/*/*/*/INDEX.md` (the folder lists of multi-file documents) to the history's `info/exclude`. `<project>/INDEX.md` of a registered project, and `INDEX.md` in a live multi-file document's folder, are reserved paths: `check` never reports them as unknown files, but reports them when missing or out of date.
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

### Database

`<storage.state_dir>/joedocs.db` is SQLite. Its schema changes only through numbered migrations, applied when the store opens; `PRAGMA user_version` records how many have run. A build refuses a database with a newer schema than it knows, so going back to an older JoeDocs after an upgrade that added a migration means restoring a backup taken before it.

| Version | Migration | Adds |
| --- | --- | --- |
| 1 | `0001_core` | projects, API keys and grants, documents and revisions |
| 2 | `0002_audit` | the audit log |
| 3 | `0003_search` | the search index |
| 4 | `0004_admin` (0.3.0) | admin tokens, with their TOTP secrets, and admin sessions |
| 5 | `0005_files` (0.4.0) | multi-file documents: the sub-files of each document, the files each revision changed, and sub-file search entries |

The index (documents, files, revisions and search) is rebuilt from the history whenever it was built by a build with another index layout: 0.4.0 changes the layout, so its first `serve` runs `reindex` once.

Only SHA-256 hashes of API keys, admin tokens and session tokens are stored. TOTP secrets are stored as they are, because checking a code needs them, so the state directory is kept private: `init` creates it with mode `0700`, and the database and its `-wal` and `-shm` files are `0600`. Every start (and `init`) tightens a state directory or database that group or other can read, logging each change; if it cannot (for example, the files belong to another account), it logs the `chmod` to run and carries on. The vault keeps its own permissions.

### Backups

`joedocs backup FILE.tgz` writes a gzip-compressed tar archive, format 1:

| Entry | What it is |
| --- | --- |
| `manifest.json` | `format` (1), `joedocs_version`, `schema_version` (the database's `PRAGMA user_version`), `created_at` (UTC), `history_head` (the history's latest commit), `documents` (live documents), and `vault` and `state_dir`, where they were (for information only). |
| `state/joedocs.db` | A copy of the database made with SQLite `VACUUM INTO`. |
| `state/history.git/` | The history repository. |
| `content/` | Every regular file and directory of the vault, including `INDEX.md` files and dot folders such as `.obsidian`. Symlinks are left out and reported. |

The lock file is not included. The snapshot is taken under the write lock, so it never holds half of a change, and is staged in a `0700` directory inside the state directory before it is compressed. `joedocs backup --restore` reads the same format and creates the state directory `0700`.

### History

The history is a git repository at `<storage.state_dir>/history.git` whose work tree is the vault. Each revision is one commit, authored by the API key name, with trailers:

```text
Update JD-42: tighten FTS grammar

JoeDocs-Document: JD-42
JoeDocs-Revision: 4
JoeDocs-Action: update
JoeDocs-Client: claude-code
```

Actions are `create`, `update`, `edit`, `append`, `front_matter`, `rename`, `move`, `restore`, `archive`, `unarchive` and, for multi-file documents, `create_file`, `write_files`, `archive_file`, and the administrator's `convert`, `move_file` and `restore_file`. A `move_file` commit names the file's old path in `JoeDocs-File` and adds `JoeDocs-Target-Document`, `JoeDocs-Target-Revision` and `JoeDocs-Target-File` for the document it moved to; it is a revision of both documents. A change to one file of a multi-file document keeps the action of the tool that made it (`update`, `edit`, `append`, `front_matter`, `rename`). A commit of a multi-file document also names every file it adds, changes or removes in a `JoeDocs-File` trailer: its path inside the folder with `.md`, the main file under its own name. A renamed file's `JoeDocs-File` is followed by `JoeDocs-File-From` with its old path. When the document is renamed or moved, only the main file is named (with `JoeDocs-File-From`): the other files move with the folder unchanged. Commits of single-file documents have none. Inspect it with plain git, for example `git --git-dir <state_dir>/history.git log --format='%ad %an | %s'`.
