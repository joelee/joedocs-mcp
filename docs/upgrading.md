# Upgrading JoeDocs

An upgrade replaces the `joedocs` binary and restarts the service. On its first start, the new version migrates the database and rebuilds the index if it needs to. This guide covers each way of installing JoeDocs, what to check afterwards, and how to go back.

Read the [version notes](#version-notes) for every version between yours and the new one before you start, and the [CHANGELOG](../CHANGELOG.md) for the full list of changes.

## 0. Know your install

Every step below has a variant for each way JoeDocs runs. Commands that read the store (`config show`, `backup`, `status`, `check`) must see the service's own configuration and paths, so run them as the service does. Define `JOEDOCS` in your shell for your install, and use it for every such command below:

```bash
# systemd user unit: your account, with the unit's environment
JOEDOCS() { env $(systemctl --user show -p Environment --value joedocs) joedocs "$@"; }

# systemd system unit: the unit's account (User=), with the unit's environment
JOEDOCS() {
  sudo -u "$(systemctl show -p User --value joedocs)" \
    env $(systemctl show -p Environment --value joedocs) joedocs "$@"
}

# Homebrew: your account
JOEDOCS() { joedocs "$@"; }

# Docker Compose: in the container (use `docker compose run --rm joedocs joedocs "$@"` while it is stopped)
JOEDOCS() { docker compose exec joedocs joedocs "$@"; }
```

A systemd unit written by `joedocs systemd install` pins its configuration in `Environment=` lines: the config file in use (`JOEDOCS_CONFIG_FILE`), every override given at install time, and the absolute storage paths (`JOEDOCS__STORAGE__HOME`, `JOEDOCS__STORAGE__STATE_DIR`). Overrides take precedence over any config file, so a plain `joedocs` (or one with `--config`) can read a different store than the service does, and back up the wrong one. `systemctl show` works whether the unit is running or stopped. It prints the values separated by spaces, so a value that holds a space makes `env` fail (`No such file or directory`); then copy the `Environment=` lines from `systemctl cat joedocs` (`--user` for a user unit) into the `env` command by hand. Check the result with `JOEDOCS config show`: `storage.home` and `storage.state_dir` must come from the environment and match the unit.

| Install | The service's vault and state directory | Stop | Check it stopped | Start |
| --- | --- | --- | --- | --- |
| systemd user unit | `JOEDOCS config show`, or the `JOEDOCS__STORAGE__*` lines in `systemctl --user cat joedocs` | `systemctl --user stop joedocs` | `systemctl --user is-active joedocs` prints `inactive` | `systemctl --user start joedocs` |
| systemd system unit | `JOEDOCS config show`, or the `JOEDOCS__STORAGE__*` and `ReadWritePaths=` lines in `systemctl cat joedocs` | `sudo systemctl stop joedocs` | `systemctl is-active joedocs` prints `inactive` | `sudo systemctl start joedocs` |
| Homebrew | `joedocs config show` | `brew services stop joedocs` | `brew services info joedocs` shows it is not running | `brew services start joedocs` |
| Docker Compose | on the host, `$JOEDOCS_DATA/vault` and `$JOEDOCS_DATA/state` (default `./.local/data`), mounted at `/data/vault` and `/data/state` | `docker compose stop joedocs` | `docker compose ps joedocs` shows no running container | `docker compose up -d` |

Below, `<storage.home>` and `<storage.state_dir>` are the service's directories from the table.

## 1. Back up first

Some versions add a database migration, and an older JoeDocs refuses a database with a newer schema ([reference](reference.md#database)). The only way back from such an upgrade is the backup you take now.

**From 0.3.0 on**, while the service runs (it is consistent without stopping anything):

```bash
JOEDOCS backup ~/joedocs-before-upgrade-$(date +%F).tgz
```

The file is written by the account the service runs as, so with a system unit or Docker, `~` (your home) is the wrong place. For a system unit, write it into the service's state directory (`JOEDOCS backup <storage.state_dir>/before-upgrade.tgz`) and move it out with `sudo mv`. With Docker, write it under the mounted state directory (`JOEDOCS backup /data/state/before-upgrade.tgz`), then move it out of `$JOEDOCS_DATA/state` on the host.

**On 0.2.x and older**, which have no `backup` command, the service must be stopped first, or the database, history and vault in the archive may come from different moments. Stop it with the command for your install, check that it has stopped, then archive both of the service's directories:

```bash
systemctl --user stop joedocs && systemctl --user is-active joedocs   # prints inactive (see the table for other installs)
tar czf ~/joedocs-before-upgrade-$(date +%F).tgz <storage.home> <storage.state_dir>
```

For a system unit, run `tar` with `sudo` so it can read the service account's files. `tar` stores the paths without their leading `/`, so `tar xzf FILE -C /` puts them back where they were. Keep the archive private (`chmod 600`): it holds API key hashes and, from 0.3.0, admin TOTP secrets.

## 2. Install the new version

| Install | Upgrade |
| --- | --- |
| `cargo install`, systemd user unit | `cargo install joedocs --locked` |
| `cargo install`, systemd system unit | `sudo cargo install joedocs --locked --root /usr/local` (or wherever the unit's `ExecStart` points) |
| Homebrew | `brew update && brew upgrade joedocs` |
| Docker Compose | `docker compose pull` (or `docker compose build` for a local image) |

Then start the service again with the command in the table (or restart it, if you did not stop it for the backup). `cargo install` builds from source and needs the Rust version in the [install guide](deployment.md#1-install). The unit does not need reinstalling: `joedocs systemd install` pins the configuration, and `cargo install` replaces the binary at the same path. Rerun `joedocs systemd install` only if the binary moved.

## 3. Check it

```bash
JOEDOCS --version
JOEDOCS status                     # health ok, the unit active, the index current
JOEDOCS check                      # exit 0, no problems
journalctl --user -u joedocs -n 50 # system unit: journalctl -u joedocs; Homebrew: brew services info joedocs; Docker: docker compose logs joedocs
```

The restart ends the MCP sessions, so reconnect each client (in Claude Code, `/mcp` and reconnect). API keys and URLs do not change. From 0.3.0, the first start also makes the state directory private (`0700`, the database `0600`) and logs it.

## 4. Going back

If the new version misbehaves, install the old one again (`cargo install joedocs --version X.Y.Z --locked`, an older image tag, or the older formula) and start it.

If the upgrade added a migration (see the notes), the old version refuses the database (`database schema version N is newer than this build supports`). Then restore the backup from step 1:

1. Stop the service and check that it has stopped (the table in step 0).
2. Move its directories aside, keeping them until you are sure: `mv <storage.home> <storage.home>.new && mv <storage.state_dir> <storage.state_dir>.new` (with `sudo` for a system unit).
3. Install the version you are going back to.
4. Restore into the now empty places, **with that older binary**: restoring opens the store, which migrates it, so a newer binary would upgrade the restored database again.
   - **an archive from `joedocs backup`** (the older version is 0.3.0 or later): `JOEDOCS backup --restore FILE.tgz`;
   - **an archive from `tar`** (going back to 0.2.x, which has no `backup`): `tar xzf FILE.tgz -C /` (with `sudo` for a system unit).

   For a system unit, the restore runs as the service account. If that account cannot create the directories, create them empty and owned by it first: `sudo install -d -o <User> -m 0700 <storage.state_dir>`, and the same for `<storage.home>` with the mode it had.
5. Start the service and check it (step 3).

Changes made after the backup are lost, so go back soon or not at all.

## Version notes

### 0.4.1 to 0.4.2

- **No migration**, and no configuration change: install the new version and restart, as in steps 2 and 3. Going back to 0.4.1 needs no restore.
- **New commands:** `joedocs list`, `search`, `history`, `diff` and `audit` view and find documents and read the audit log, as tables or with `-j/--json`; `joedocs key ungrant` removes a key's grants; `archive list` takes `-t/--type`. Frequent options have short forms (`-p`, `-t`, `-s`, `-d`, `-k`, `-F`, `-n`, `-P`, `-j`). See the [reference](reference.md#command-line).
- **Remote use needs the server upgraded too.** The new commands use new admin API routes; a 0.4.1 server answers `no such admin API route`, and the client says to upgrade it. `archive list --type` against a 0.4.1 server is not filtered, because that server ignores the unknown `type` parameter; the `TYPE` column still shows each document's type.
- MCP clients see no change: the tools and their results are as in 0.4.1.
- **Skills:** `joedocs-code-review` now updates a review in place for a re-review of the same change. If you copied the skills into `~/.claude/skills`, copy them again (`cp -r docs/skills/joedocs-* ~/.claude/skills/`); links to a checkout pick it up.

### 0.4.0 to 0.4.1

- **No migration**, and no configuration change: install the new version and restart, as in steps 2 and 3. Going back to 0.4.0 needs no restore.
- **Fixed:** `search_documents` hits on multi-file documents carry their real `files` count.
- **Skills:** `joedocs-planner` changed and `joedocs-issue-log` is new. If you copied the skills into `~/.claude/skills`, copy them again (`cp -r docs/skills/joedocs-* ~/.claude/skills/`); if you linked them to a checkout, link the new one too.

### 0.3.x to 0.4.0

- **Database migration 5** adds multi-file documents. 0.3.x cannot open the database afterwards, so take a backup in step 1 first: `JOEDOCS backup ~/joedocs-before-0.4.0.tgz` works while the service runs.
- **The first start rebuilds the index** from the history once, because its layout changed; the log shows it, and `JOEDOCS check` then reports no problems. Existing documents and their files do not change.
- **New for agents:** multi-file documents (`new_document` with `multifile: true`) and the tools `new_document_file`, `list_document_files`, `write_document_files` and `archive_document_file`, nineteen tools in all; reconnect clients to see them. Existing tools take an optional `file`.
- **New for administrators:** `joedocs document convert`, `move-file` and `restore-file`, `get --part` and `get --archive`, and `joedocs login` asking for the server when none is set. New config key `documents.max_files` (default 500).
- **Going back** to 0.3.x means restoring the backup taken before the upgrade with the 0.3.x binary; documents written since are lost.

### 0.2.x to 0.3.0

- **Database migration 4** adds admin tokens and admin sessions. 0.2.x cannot open the database afterwards, so take the `tar` backup in step 1 before upgrading.
- **New commands:** `get`, `status`, `agents`, `backup` (with `--restore`), `admin-token`, `login` and `logout`. Nothing existing changes.
- **The remote admin API is off.** To manage the server from another machine, set `[admin] enabled = true`, restart, create an admin token with `joedocs admin-token create NAME`, enrol its TOTP secret in an authenticator app, and follow [Manage from another machine](deployment.md#6a-manage-from-another-machine). Leave it off if you do not need it.
- **For agents that maintain the server**, `joedocs agents` prints instructions filled in with this installation's paths.

### 0.1.x to 0.2.0

- No migration: 0.1.x can still open the database.
- `serve` writes `INDEX.md`, a table of the live documents, in each project folder when it starts; until then, `joedocs check` reports the files as missing. Turn it off with `[documents] index_file = false`.
