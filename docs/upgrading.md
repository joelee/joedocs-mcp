# Upgrading JoeDocs

An upgrade replaces the `joedocs` binary and restarts the service. On its first start, the new version migrates the database and rebuilds the index if it needs to. This guide covers each way of installing JoeDocs, what to check afterwards, and how to go back.

Read the [version notes](#version-notes) for every version between yours and the new one before you start, and the [CHANGELOG](../CHANGELOG.md) for the full list of changes.

## 1. Back up first

Some versions add a database migration, and an older JoeDocs refuses a database with a newer schema ([reference](reference.md#database)). The only way back from such an upgrade is the backup you take now.

**From 0.3.0 on**, while the service runs:

```bash
joedocs backup ~/joedocs-before-upgrade-$(date +%F).tgz
```

**On 0.2.x and older**, which have no `backup` command, stop the service and archive both directories:

```bash
systemctl --user stop joedocs                               # or: brew services stop joedocs
joedocs config show | grep -E 'storage\.(home|state_dir)'    # the two directories
tar czf ~/joedocs-before-upgrade-$(date +%F).tgz <storage.home> <storage.state_dir>
```

`tar` stores the paths without their leading `/`, so `tar xzf FILE -C /` puts them back. Keep the archive private: it holds API key hashes and, from 0.3.0, admin TOTP secrets.

## 2. Install the new version

The way you installed it decides how you upgrade it.

| Install | Upgrade | Restart |
| --- | --- | --- |
| `cargo install`, systemd user unit | `cargo install joedocs --locked` | `systemctl --user restart joedocs` |
| `cargo install`, systemd system unit | `sudo cargo install joedocs --locked --root /usr/local` (or wherever the unit's `ExecStart` points) | `sudo systemctl restart joedocs` |
| Homebrew | `brew update && brew upgrade joedocs` | `brew services restart joedocs` |
| Docker Compose | `docker compose pull` (or `docker compose build` for a local image) | `docker compose up -d` |

`cargo install` builds from source and needs the Rust version in the [install guide](deployment.md#1-install). The unit does not need reinstalling: `joedocs systemd install` pins the configuration, and `cargo install` replaces the binary at the same path. Rerun `joedocs systemd install` only if the binary moved.

## 3. Check it

```bash
joedocs --version
joedocs status                     # health ok, the unit active, the index current
joedocs check                      # exit 0, no problems
journalctl --user -u joedocs -n 50 # or: brew services info joedocs
```

The restart ends the MCP sessions, so reconnect each client (in Claude Code, `/mcp` and reconnect). API keys and URLs do not change.

## 4. Going back

If the new version misbehaves, install the old one again and restart:

```bash
cargo install joedocs --version X.Y.Z --locked
```

If the upgrade added a migration (see the notes), the old version refuses the database (`database schema version N is newer than this build supports`). Then stop the service, move the current directories aside, restore the backup from step 1 into the configured paths, and start it again:

```bash
systemctl --user stop joedocs
mv <storage.home> <storage.home>.new && mv <storage.state_dir> <storage.state_dir>.new
joedocs backup --restore ~/joedocs-before-upgrade-YYYY-MM-DD.tgz   # with 0.3.0 or later installed
systemctl --user start joedocs
```

An archive made with `tar` in step 1 is restored with `tar xzf ... -C /` instead. Changes made after the backup are lost, so go back soon or not at all.

## Version notes

### 0.2.x to 0.3.0

- **Database migration 4** adds admin tokens and admin sessions. 0.2.x cannot open the database afterwards, so take the `tar` backup in step 1 before upgrading.
- **New commands:** `get`, `status`, `agents`, `backup` (with `--restore`), `admin-token`, `login` and `logout`. Nothing existing changes.
- **The remote admin API is off.** To manage the server from another machine, set `[admin] enabled = true`, restart, create an admin token with `joedocs admin-token create NAME`, enrol its TOTP secret in an authenticator app, and follow [Manage from another machine](deployment.md#6a-manage-from-another-machine). Leave it off if you do not need it.
- **For agents that maintain the server**, `joedocs agents` prints instructions filled in with this installation's paths.

### 0.1.x to 0.2.0

- No migration: 0.1.x can still open the database.
- `serve` writes `INDEX.md`, a table of the live documents, in each project folder when it starts; until then, `joedocs check` reports the files as missing. Turn it off with `[documents] index_file = false`.
