# AGENTS.md - maintaining this JoeDocs server

You are maintaining a JoeDocs server: an MCP server that stores project documentation (plans, ideas, reviews, notes and more) as Markdown files with YAML front matter, keeps every revision in git, and indexes them in SQLite. People and agents on other machines read and change documents through its MCP tools. Treat the stored documents as the team's records: keeping them safe matters more than any single task.

## This installation

| Fact | Value |
| --- | --- |
| JoeDocs version | {{version}} |
| Config file | {{config_file}} |
| Vault (`storage.home`, the Markdown files) | {{home}} |
| State (`storage.state_dir`: `joedocs.db`, `history.git`, `joedocs.lock`) | {{state_dir}} |
| Listen address (`server.bind`) | {{bind}} |
| Service | {{service}} |
| Remote admin API | {{admin}} |
| Admin web interface | {{web}} |

Run `joedocs status` for the current state, sizes and health, and `joedocs config show` for every setting and where it came from.

Run every `joedocs` command below the way the service runs, or it may read a different store. A systemd unit pins its configuration and storage paths in `Environment=` lines (`systemctl show -p User -p Environment joedocs`, with `--user` for a user unit), and they take precedence over any config file. Run commands with that environment (`env <those values> joedocs ...`), and for a system unit as its `User=` account (`sudo -u <User> env ... joedocs ...`). `joedocs config show` must then report the unit's `storage.home` and `storage.state_dir`; if this table shows other paths, it was printed without the unit's environment.

## Safe to run at any time

These only read:

- `joedocs status`: where the service runs, sizes, counts, index and health.
- `joedocs check`: drift between the vault, the history and the index (exits `1` when it finds problems, and says how to fix each).
- `joedocs config show` and `joedocs config paths`.
- `joedocs project list`, `joedocs key list`, `joedocs archive list`.
- `joedocs get DOCUMENT_ID`: print a document.
- `joedocs list -p PROJECT` and `joedocs search [QUERY]`: list a project's documents, or find documents by text and front matter.
- `joedocs history DOCUMENT_ID` and `joedocs diff DOCUMENT_ID --from N`: a document's revisions and what changed between two of them.
- `joedocs audit`: the audit log of MCP writes and admin API calls (filter with `--since`, `-k`, `--tool`, `-d`).
- `joedocs backup FILE.tgz`: a consistent backup, safe while the service runs. The file holds key hashes, so keep it private.
- Service logs: `journalctl --user -u joedocs` (user unit), `journalctl -u joedocs` (system unit), or `brew services info joedocs` on macOS.

## Ask the operator first

These change the store or what clients can do. Explain what you intend and why, and wait for approval:

- `joedocs reindex` (rebuilds the index from the history; safe, but takes the write lock).
- `joedocs archive restore DOCUMENT_ID`.
- `joedocs document convert`, `document move-file` and `document restore-file` (multi-file documents).
- `joedocs project add`, `joedocs key create`, `key grant`, `key ungrant`, `key rotate` and `key revoke`. A rotated or revoked key stops working at once, on every machine that uses it, and an ungranted project is out of reach on the key's next request.
- `joedocs backup --restore`, which needs empty target directories and a stopped service.
- `joedocs user create`, `disable`, `enable`, `unlock`, `reset-password`, `reset-totp` and `remove` (`create` and the resets print a one-time password for a person; never keep it), and turning the remote admin API on or off or `admin.token_login` off.
- `joedocs admin-token create`, `list` and `revoke` (admin tokens are deprecated since 0.5.1).
- Upgrading, restarting or stopping the service, and `joedocs systemd install` or `remove`.
- Any change to the config file.

## Never

- Print, log, copy or store an API token. `key create` and `key rotate` show a token once; hand it to the operator and do not keep it.
- Edit, move or delete anything in the state directory by hand: not `joedocs.db`, not `history.git`, not the lock file.
- Edit, add or delete files in the vault directly. Documents change through the MCP tools, so every change becomes a revision. A hand edit is reported by `joedocs check` as drift.
- Run `joedocs init` over an existing vault, or delete the vault or state directory.
- Turn off checks to make an error go away. Report it instead.

## Upgrading

1. `joedocs backup ~/joedocs-backup-$(date +%F).tgz`.
2. Install the new version the way this one was installed (`cargo install joedocs --locked`, `brew upgrade joedocs`, or a new container image).
3. Restart the service, then check `joedocs status`, `joedocs check` and the health endpoint.
4. If something is wrong, reinstall the old version and restore the backup into empty directories.

The full procedure, with notes for each version: <https://github.com/joelee/joedocs-mcp/blob/main/docs/upgrading.md>.

## Troubleshooting

- **The service does not answer:** `joedocs status` (health and unit state), then the service logs. `serve` refuses to start when it cannot bind `server.bind`.
- **`check` reports drift:** read each problem; `joedocs reindex` fixes an index behind the history and rewrites `INDEX.md` files. Files changed by hand need the operator's decision.
- **A client gets `unauthorized`:** its key may be revoked or rotated; check `joedocs key list` and ask the operator.
- **Disk space:** `joedocs status` shows the vault, database and history sizes.
