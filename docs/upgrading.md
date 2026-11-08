# Upgrading JoeDocs

An upgrade replaces the `joedocs` binary and restarts the service. On its first start, the new version migrates the database and rebuilds the index if it needs to. This guide covers each way of installing JoeDocs, what to check afterwards, and how to go back.

Read the [version notes](#version-notes) for every version between yours and the new one before you start, and the [CHANGELOG](../CHANGELOG.md) for the full list of changes.

## 0. Know your install

Every step below has a variant for each way JoeDocs runs. Commands that read the store (`config show`, `backup`, `status`, `check`) must see the service's own configuration and paths, so run them as the service does:

| Install | Run a joedocs command as the service | The service's vault and state directory | Stop | Check it stopped | Start |
| --- | --- | --- | --- | --- | --- |
| systemd user unit | `joedocs ...` as yourself (the unit pins its config with `Environment=` lines) | `systemctl --user cat joedocs`: the `JOEDOCS_CONFIG_FILE` and `JOEDOCS__STORAGE__*` lines; or `joedocs config show` | `systemctl --user stop joedocs` | `systemctl --user is-active joedocs` prints `inactive` | `systemctl --user start joedocs` |
| systemd system unit | `sudo -u joedocs joedocs --config /etc/joedocs/config.toml ...` (the `User=` and config from the unit) | `systemctl cat joedocs`: the `Environment=` and `ReadWritePaths=` lines | `sudo systemctl stop joedocs` | `systemctl is-active joedocs` prints `inactive` | `sudo systemctl start joedocs` |
| Homebrew | `joedocs ...` as yourself | `joedocs config show` | `brew services stop joedocs` | `brew services info joedocs` shows it is not running | `brew services start joedocs` |
| Docker Compose | `docker compose exec joedocs joedocs ...` while it runs, `docker compose run --rm joedocs joedocs ...` while it is stopped | on the host, `$JOEDOCS_DATA/vault` and `$JOEDOCS_DATA/state` (default `./.local/data`), mounted at `/data/vault` and `/data/state` | `docker compose stop joedocs` | `docker compose ps joedocs` shows no running container | `docker compose up -d` |

Below, `<storage.home>` and `<storage.state_dir>` are the service's directories from the table, and `JOEDOCS` stands for the way to run a command as the service.

## 1. Back up first

Some versions add a database migration, and an older JoeDocs refuses a database with a newer schema ([reference](reference.md#database)). The only way back from such an upgrade is the backup you take now.

**From 0.3.0 on**, while the service runs (it is consistent without stopping anything):

```bash
JOEDOCS backup ~/joedocs-before-upgrade-$(date +%F).tgz
```

With Docker, write it under the mounted state directory (`docker compose exec joedocs joedocs backup /data/state/before-upgrade.tgz`), then move it out of `$JOEDOCS_DATA/state` on the host.

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
5. Start the service and check it (step 3).

Changes made after the backup are lost, so go back soon or not at all.

## Version notes

### 0.2.x to 0.3.0

- **Database migration 4** adds admin tokens and admin sessions. 0.2.x cannot open the database afterwards, so take the `tar` backup in step 1 before upgrading.
- **New commands:** `get`, `status`, `agents`, `backup` (with `--restore`), `admin-token`, `login` and `logout`. Nothing existing changes.
- **The remote admin API is off.** To manage the server from another machine, set `[admin] enabled = true`, restart, create an admin token with `joedocs admin-token create NAME`, enrol its TOTP secret in an authenticator app, and follow [Manage from another machine](deployment.md#6a-manage-from-another-machine). Leave it off if you do not need it.
- **For agents that maintain the server**, `joedocs agents` prints instructions filled in with this installation's paths.

### 0.1.x to 0.2.0

- No migration: 0.1.x can still open the database.
- `serve` writes `INDEX.md`, a table of the live documents, in each project folder when it starts; until then, `joedocs check` reports the files as missing. Turn it off with `[documents] index_file = false`.
