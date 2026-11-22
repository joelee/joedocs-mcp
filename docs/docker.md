# Docker installation

Run a JoeDocs server with Docker alone: no Rust toolchain, `git` or other dependencies on your machine. Each release from 0.5.6-rc.3 on publishes an image for `linux/amd64` and `linux/arm64` to two registries:

- Docker Hub: `joeworks/joedocs` (`docker.io/joeworks/joedocs`)
- GitHub Container Registry: `ghcr.io/joelee/joedocs`

Both hold the same image. It contains the static `joedocs` binary of the GitHub release with the same version, on Debian slim with `git`, and runs as the user `joedocs` (uid 10001). The examples use `joeworks/joedocs:0.5.6-rc.3`; use the newest tag (see [Tags and upgrades](#12-tags-and-upgrades)).

For installing without Docker, running as a service, and remote access in general, see the [deployment guide](deployment.md).

## 1. Quick start

```bash
docker run -d --name joedocs -p 127.0.0.1:8750:8750 joeworks/joedocs:0.5.6-rc.3
docker logs joedocs
```

The first start sets up a standalone server and prints how to sign in to the web interface, once:

```text
========================================================================
 JoeDocs is ready. Sign in to the web interface:

   URL:       http://localhost:8750/admin/
              (if you published another host port, use it instead)
   User:      admin
   Password:  <a one-time password>
              (a one-time password: it works once, within 72 hours)
 ...
```

Then:

1. Open `http://localhost:8750/admin/` and sign in as `admin` with that password.
2. Choose your own password (12 or more characters).
3. Scan the QR code with an authenticator app (Google Authenticator, 1Password or any TOTP app) and type a code from it.

The server's MCP endpoint is `http://127.0.0.1:8750/mcp`. To use it, create a project and an API key ([section 9](#9-projects-keys-and-claude-code)).

`-p 127.0.0.1:8750:8750` publishes the port on this machine's loopback address only, so nothing else on your network can reach the server. Without your own volumes, the data lives in anonymous Docker volumes ([section 3](#3-where-the-data-lives)): for anything you want to keep, use named volumes ([section 4](#4-named-volumes)) or local directories ([section 5](#5-local-directories-for-the-vault-and-state)) from the start.

## 2. The first sign-in and the first user

On every start the image's entrypoint runs `joedocs init`, which creates the vault, database and history on an empty store and only checks them later. Then, **only when the store has no users** (`joedocs user list` prints `No users.`), it creates the first user and prints the banner above. A store that has users, removed ones included, is left as it is, so a restart or a new container on the same volumes prints nothing.

| Variable | Effect |
| --- | --- |
| `JOEDOCS_INITIAL_ADMIN` unset | Creates the user `admin`. |
| `-e JOEDOCS_INITIAL_ADMIN=alice` | Creates the user `alice` instead. |
| `-e JOEDOCS_INITIAL_ADMIN=` (empty) | Creates no user; create one yourself with `docker exec joedocs joedocs user create NAME`, which prints its one-time password on the last line. |

The one-time password stays in `docker logs`, but it works only once and only for 72 hours, and signing in still needs the authenticator you set up. If you lose it before signing in, or it runs out, make a new one:

```bash
docker exec joedocs joedocs user reset-password admin   # the new one-time password is on the last line
```

Every user is an admin in this version. The web interface's Users page creates more.

## 3. Where the data lives

| Path in the container | What it holds |
| --- | --- |
| `/data/vault` | The vault: Markdown files, one folder per project and type |
| `/data/state` | `joedocs.db` (the SQLite index, projects, API keys, users), `history.git` (every revision) and `joedocs.lock` |

Both are Docker volumes. If you do not give them one of your own, Docker creates **anonymous volumes**: they survive `docker restart` and `docker stop`, but `docker rm -v joedocs` deletes them, `docker run --rm` deletes them when the container stops, and a new container gets new, empty ones. They have random names; `docker inspect -f '{{json .Mounts}}' joedocs` shows them.

To move a trial server onto volumes of your own, back it up ([section 11](#11-backups)) and restore the archive into the new volumes.

## 4. Named volumes

Named volumes are kept until you remove them, by name:

```bash
docker run -d --name joedocs -p 127.0.0.1:8750:8750 \
  -v joedocs-vault:/data/vault -v joedocs-state:/data/state \
  --restart unless-stopped \
  joeworks/joedocs:0.5.6-rc.3
```

`docker rm -f joedocs` removes the container and keeps `joedocs-vault` and `joedocs-state`; the next `docker run` with the same `-v` options carries on with the same data. `docker volume ls` lists them, and `docker volume rm joedocs-vault joedocs-state` deletes them.

## 5. Local directories for the vault and state

To keep the vault and state in directories on your machine, where you can read the Markdown, browse the history with `git` and back them up like other files, bind-mount them. Create both directories first (otherwise Docker creates them owned by `root`, and the server cannot write), and run the container as your own user so the files belong to you:

```bash
mkdir -p ~/joedocs/vault ~/joedocs/state
docker run -d --name joedocs -p 127.0.0.1:8750:8750 \
  --user "$(id -u):$(id -g)" \
  -v ~/joedocs/vault:/data/vault -v ~/joedocs/state:/data/state \
  --restart unless-stopped \
  joeworks/joedocs:0.5.6-rc.3
```

- The directories must be empty the first time (or hold a JoeDocs store from earlier).
- Instead of `--user`, you can give the directories to the image's user: `sudo chown -R 10001:10001 ~/joedocs/vault ~/joedocs/state`. Then the files on your machine belong to uid 10001.
- Read the data freely, for example `git --git-dir ~/joedocs/state/history.git log --format='%ad %an | %s'`, but change it only through JoeDocs: a file edited by hand is refused for writes until resolved (`docker exec joedocs joedocs check` explains how).
- `~/joedocs/state` holds API key hashes and TOTP secrets. JoeDocs keeps it private (`0700`, the database `0600`); keep it out of synced and shared folders.
- On Docker Desktop (macOS, Windows), bind mounts belong to your user without `--user`.

## 6. Configuration

Every setting can be given as an environment variable `JOEDOCS__SECTION__KEY`; [configuration.md](configuration.md) lists them all with their defaults. For example:

```bash
docker run ... \
  -e JOEDOCS__LOG__LEVEL=debug \
  -e JOEDOCS__SERVER__ALLOWED_HOSTS='["localhost", "127.0.0.1", "::1", "docs.example.ts.net"]' \
  joeworks/joedocs:0.5.6-rc.3
```

Or mount a configuration file at `/etc/joedocs/config.toml`, which the server finds on its own:

```bash
docker run ... -v ~/joedocs/config.toml:/etc/joedocs/config.toml:ro joeworks/joedocs:0.5.6-rc.3
```

The image sets these variables, and a variable overrides the file, so change them with `-e`, not in the file:

| Variable | Image's value | Why |
| --- | --- | --- |
| `JOEDOCS__STORAGE__HOME` | `/data/vault` | The vault volume |
| `JOEDOCS__STORAGE__STATE_DIR` | `/data/state` | The state volume |
| `JOEDOCS__SERVER__BIND` | `0.0.0.0:8750` | Reachable through the published port |
| `JOEDOCS__ADMIN__ENABLED` | `true` | The admin API, which the web interface needs |
| `JOEDOCS__ADMIN__WEB_UI` | `true` | The web interface at `/admin/` |

To run without the web interface and admin API: `-e JOEDOCS__ADMIN__WEB_UI=false -e JOEDOCS__ADMIN__ENABLED=false`. `docker exec joedocs joedocs config show` prints every value and where it came from.

## 7. Ports, host names and HTTPS

JoeDocs speaks plain HTTP. Inside the container it listens on port 8750; `-p` decides where it appears on your machine:

- `-p 127.0.0.1:8750:8750`: this machine only (recommended). Another host port works too, for example `-p 127.0.0.1:9000:8750`; then use `http://localhost:9000/admin/`.
- `-p 8750:8750` publishes it on every network interface. Don't, unless a firewall or a private network protects it: the sign-in page is then reachable by anyone who can reach the port.

**Host names.** The server answers only requests whose `Host` header is in `server.allowed_hosts` (default `localhost`, `127.0.0.1`, `::1`); others get `403`. Add every name clients use, as in [section 6](#6-configuration).

**Signing in needs HTTPS**, except on loopback: the web interface refuses a sign-in over plain HTTP unless the address is `localhost`, `127.0.0.1` or `[::1]`. To use the web interface or connect clients from other machines, put HTTPS in front of the published loopback port on the host:

- **Tailscale:** `sudo tailscale serve --bg 8750` gives `https://<machine>.<tailnet>.ts.net` on your tailnet. Add `<machine>.<tailnet>.ts.net` to `allowed_hosts`.
- **A TLS reverse proxy** such as Caddy (`reverse_proxy 127.0.0.1:8750`). Add its name to `allowed_hosts`, and let it pass the `Host` header through.

The [deployment guide](deployment.md#5-reach-it-from-other-machines) has the details, including proxies that rewrite `Host` (`server.allowed_origins`).

## 8. Docker Compose

A `compose.yaml` for the published image, with the data in local directories next to it:

```yaml
services:
  joedocs:
    image: joeworks/joedocs:0.5.6-rc.3
    # Forwards SIGTERM, so `docker compose stop` shuts the server down cleanly.
    init: true
    # Your `id -u`:`id -g`, so the files in ./data belong to you.
    user: "1000:1000"
    ports:
      - "127.0.0.1:8750:8750"
    environment:
      JOEDOCS__LOG__LEVEL: info
      # JOEDOCS__SERVER__ALLOWED_HOSTS: '["localhost", "127.0.0.1", "::1", "docs.example.ts.net"]'
    volumes:
      - ./data/vault:/data/vault
      - ./data/state:/data/state
    restart: unless-stopped
```

```bash
mkdir -p data/vault data/state
docker compose up -d --wait        # waits until the health check passes
docker compose logs joedocs        # the sign-in banner, on the first start
docker compose exec joedocs joedocs status
```

The `compose.yaml` in this repository builds the image from source instead; it is for development (see the [developer guide](developer-guide.md#running-with-docker)).

## 9. Projects, keys and Claude Code

An agent reaches JoeDocs through the MCP endpoint with an API key, which grants it projects. Create them in the web interface (Projects, then Keys), or from the command line:

```bash
docker exec joedocs joedocs project add my-app --key MA --title "My app"
docker exec joedocs joedocs key create laptop --grant my-app:write   # the token is alone on the last line
```

`key create` prints the token once. Connect Claude Code with it:

```bash
claude mcp add --transport http --scope user joedocs http://127.0.0.1:8750/mcp \
  --header "Authorization: Bearer jd_..."
claude mcp get joedocs        # Connected
```

From another machine, use the HTTPS address from [section 7](#7-ports-host-names-and-https) instead (`https://<machine>.<tailnet>.ts.net/mcp`). The [deployment guide](deployment.md#6-connect-clients) covers other clients, the JoeDocs plugin for Claude Code, and keeping the key out of files.

## 10. Commands

Run `joedocs` commands in the running container with `docker exec`; they use the same store and settings as the server:

```bash
docker exec joedocs joedocs status
docker exec joedocs joedocs check
docker exec joedocs joedocs user list
docker exec joedocs joedocs key list
```

Arguments given to `docker run` after the image name run as a command instead of the server (only no arguments, or `serve`, start the server):

```bash
docker run --rm joeworks/joedocs:0.5.6-rc.3 joedocs --version
docker run --rm --volumes-from joedocs joeworks/joedocs:0.5.6-rc.3 joedocs check   # on a stopped container's volumes
```

## 11. Backups

`joedocs backup` writes one archive of the vault, database and history, consistent while the server runs. Write it inside the container, then copy it out:

```bash
docker exec joedocs joedocs backup /tmp/joedocs-backup.tgz
docker cp joedocs:/tmp/joedocs-backup.tgz ./joedocs-$(date +%F).tgz
docker exec joedocs rm /tmp/joedocs-backup.tgz
```

Keep the archive private: it holds API key hashes and TOTP secrets.

To restore, start from empty volumes or directories, with the server not running on them, and restore with the same version that made the archive or a newer one. The archive goes in on standard input, because the container's user cannot read your private copy through a mount:

```bash
docker run --rm -i -v joedocs-vault:/data/vault -v joedocs-state:/data/state \
  joeworks/joedocs:0.5.6-rc.3 \
  sh -c 'cat > /tmp/joedocs-backup.tgz && joedocs backup --restore /tmp/joedocs-backup.tgz' \
  < joedocs-2026-11-22.tgz
```

Then start the server on those volumes as in [section 4](#4-named-volumes); it prints no banner, since the restored store has its users. With local directories, mount them instead, and add `--user` as in [section 5](#5-local-directories-for-the-vault-and-state).

## 12. Tags and upgrades

| Tag | What it is |
| --- | --- |
| `X.Y.Z-<pre>`, such as `0.5.6-rc.3` | A pre-release: not for production use. Pre-releases get only their exact version tag. |
| `X.Y.Z`, such as `0.5.6` | A release. |
| `X.Y`, such as `0.5` | The newest `X.Y.Z` release. |
| `latest` | The newest release. Never a pre-release. |

Use an exact version, so that an upgrade happens only when you choose it. To upgrade:

1. Read the [upgrading guide](upgrading.md)'s version notes, and back up ([section 11](#11-backups)).
2. Pull the new tag, remove the old container, and start the new one with the **same volumes or directories and options**:

   ```bash
   docker pull joeworks/joedocs:NEW
   docker rm -f joedocs
   docker run -d --name joedocs ... joeworks/joedocs:NEW     # the same -p, -v, --user and -e as before
   docker logs joedocs
   ```

   With Compose, change `image:` and run `docker compose up -d`.

On its first start the new version migrates the database and rebuilds the index if it needs to. To go back, start the older tag on the same data; if the upgrade added a migration, restore the backup instead (see [upgrading.md](upgrading.md#4-going-back)). A container with anonymous volumes cannot be upgraded this way, since a new container gets new volumes: back it up and restore the archive into named volumes first.

## 13. Verifying an image

Each image carries an SBOM and build provenance (BuildKit attestations, in both registries), and a GitHub build-provenance attestation that it was built by this repository's release workflow:

```bash
gh attestation verify oci://ghcr.io/joelee/joedocs:0.5.6-rc.3 --repo joelee/joedocs-mcp
docker buildx imagetools inspect joeworks/joedocs:0.5.6-rc.3      # the platforms and attestation manifests
docker buildx imagetools inspect joeworks/joedocs:0.5.6-rc.3 --format '{{json .SBOM}}' | head -c 300
```

Docker Hub and GHCR hold the same image, with the same digest: compare the `Digest:` line of `docker buildx imagetools inspect` for both names. The binary inside is the one in the GitHub release's `joedocs-X.Y.Z-<arch>-unknown-linux-musl.tar.gz`, which has its own attestation ([deployment guide](deployment.md#1-install)).
