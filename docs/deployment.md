# Deployment guide

How to run JoeDocs as a long-lived server and connect clients to it from other machines. The commands and tools are in [reference.md](reference.md), and every configuration key is in [configuration.md](configuration.md).

A typical setup is one always-on machine (a home server, a VPS, or your desktop) running `joedocs serve`, with every laptop and desktop connecting to it over Tailscale or HTTPS:

```mermaid
flowchart LR
    A["Claude Code<br/>laptop"] -- "HTTPS + Bearer key" --> P
    B["Claude Code<br/>desktop"] -- "HTTPS + Bearer key" --> P
    subgraph S[Server]
        P["tailscale serve<br/>or a TLS proxy"] --> J["joedocs serve<br/>127.0.0.1:8750"]
        J --> V[/"vault<br/>Markdown"/]
        J --> T[("state<br/>SQLite + history.git")]
    end
```

## 1. Install

| Platform | Command |
| --- | --- |
| macOS | `brew trust joelee/oss && brew tap joelee/oss && brew install joelee/oss/joedocs` |
| Linux or macOS | A prebuilt binary from [GitHub Releases](https://github.com/joelee/joedocs-mcp/releases) (0.4.3 or later; below) |
| Linux or macOS | `cargo install joedocs --locked` (Rust 1.99 or newer and a C compiler; installs to `~/.cargo/bin`) |
| Docker | Build from a clone with the `Dockerfile` and `compose.yaml` (see the [developer guide](developer-guide.md#running-with-docker)) |

JoeDocs needs `git` at run time (`sudo pacman -S git`, `sudo apt install git`, or the Xcode Command Line Tools on macOS).

**Prebuilt binaries.** Each release has `joedocs-X.Y.Z-<target>.tar.gz` for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` (static, so they run on any distribution) and `aarch64-apple-darwin` (Apple silicon), with a `SHA256SUMS` file and a build-provenance attestation for each archive. Check both before installing:

```bash
V=0.5.0 T=x86_64-unknown-linux-musl   # or aarch64-unknown-linux-musl, aarch64-apple-darwin
A="joedocs-$V-$T.tar.gz"
gh release download "v$V" -R joelee/joedocs-mcp -p "$A" -p SHA256SUMS --clobber &&
  shasum -a 256 -c --ignore-missing SHA256SUMS &&
  gh attestation verify "$A" -R joelee/joedocs-mcp &&
  tar xzf "$A" joedocs &&
  mkdir -p ~/.local/bin && install -m 0755 joedocs ~/.local/bin/
```

Each step runs only if the one before it succeeded, so a failed download, checksum or attestation stops before anything is installed; `~/.local/bin` is created if it is missing and must be on your `PATH`. Without `gh`, download the archive and `SHA256SUMS` from the release page and run the `shasum` line; if it reports anything but `OK`, do not install the archive. Install it wherever your service expects it: `joedocs systemd install` pins the binary's path in the unit, so put it on `PATH` first (here `~/.local/bin`) and run `systemd install` from there. The macOS binary is not notarised: if you downloaded it in a browser, run `xattr -d com.apple.quarantine joedocs` once (files fetched with `gh` or `curl` are not quarantined).

## 2. Configure

Put the configuration in `~/.config/joedocs/config.toml`, which JoeDocs finds on its own (`joedocs config paths` shows the search order):

```toml
[log]
level = "info"

[server]
bind = "127.0.0.1:8750"
# Host names clients use to reach the server: loopback, plus your public or tailnet name.
allowed_hosts = ["localhost", "127.0.0.1", "::1", "docs.example.ts.net"]

[storage]
home = "/home/you/Documents/JoeDocs"           # the vault: Markdown only
state_dir = "/home/you/.local/state/joedocs"    # database and history; never synced
```

- `storage.home` must be an absolute path, and empty or absent before `joedocs init`. Leave `state_dir` empty to use `~/.local/share/joedocs`.
- Keep `bind` on loopback when a proxy or `tailscale serve` sits in front. Add each name clients use to `allowed_hosts`, or their requests get `403`.
- Check the result with `joedocs config show`: each value says which layer set it.

## 3. Create the store, projects and keys

Run these as the account that will run the server:

```bash
joedocs init
joedocs project add my-app --key MA --title "My app"
joedocs project add notes --key NT --title Notes
joedocs key create laptop --grant my-app:write --grant notes:write
```

`key create` prints the token once, alone on its last line, so a script can capture it with `| tail -n 1`. Create one key per machine or agent: the key name is recorded as the author of every change, and you can revoke one machine without touching the others.

## 4. Run it as a service

### Linux (systemd)

`joedocs systemd install` writes a unit that runs `joedocs serve`, then reloads systemd, enables the unit and (re)starts it. systemd does not see your shell, so the unit pins the configuration in effect: the config file in use (`JOEDOCS_CONFIG_FILE`), every key set by an environment override or flag, and the absolute storage paths. Run `joedocs init` first; install refuses an uninitialised store.

A **user service** runs as you. Run these from a real login session (SSH or a desktop, not `su`), with no `JOEDOCS__` variables set unless you mean to pin them:

```bash
joedocs systemd install --dry-run           # show the unit and the commands first
joedocs systemd install                     # ~/.config/systemd/user/joedocs.service
sudo loginctl enable-linger "$USER"         # start at boot and keep running after logout
systemctl --user status joedocs
journalctl --user -u joedocs -f
curl -s http://127.0.0.1:8750/health        # ok
```

A **system service** runs as a dedicated account and is sandboxed so it can write only the vault and the state directory (`ProtectSystem=strict`, `ReadWritePaths=`, and similar):

```bash
sudo joedocs --config /etc/joedocs/config.toml systemd install --system --run-as joedocs
sudo systemctl status joedocs
```

`sudo` resets your environment, so give the configuration through `--config` (or `/etc/joedocs/config.toml`, which the lookup finds anyway) rather than environment variables, and check the result with `--dry-run`. The account must be able to write the storage paths.

`joedocs systemd remove` (with `--system` for a system unit) stops, disables and deletes the unit. A unit file that `joedocs systemd install` did not write is never overwritten or removed.

### macOS (Homebrew)

The Homebrew formula includes a service that runs `joedocs serve` at login:

```bash
brew services start joedocs
brew services info joedocs
tail -f "$(brew --prefix)/var/log/joedocs.log"
curl -s http://127.0.0.1:8750/health        # ok
```

It reads `~/.config/joedocs/config.toml` like any other run. After changing the configuration, run `brew services restart joedocs`; `brew services stop joedocs` stops it. With a Cargo install instead, run `joedocs serve` from a launchd agent of your own.

### Docker

`compose.yaml` runs the server in a container with the vault and state bind-mounted from `.local/data`. It is meant for local trials and the deployment tests; see the [developer guide](developer-guide.md#running-with-docker).

## 5. Reach it from other machines

JoeDocs speaks plain HTTP and does not handle TLS itself. Put one of these in front of it.

**Tailscale (recommended for personal use).** Every device on your tailnet can reach the server, nothing is exposed to the internet, and Tailscale provides the certificate:

```bash
sudo tailscale serve --bg 8750     # https://<machine>.<tailnet>.ts.net → http://127.0.0.1:8750
tailscale serve status
```

Add `<machine>.<tailnet>.ts.net` to `server.allowed_hosts`. If HTTPS certificates are not enabled for your tailnet yet, the command prints a link to enable them. The setting survives reboots.

Without `tailscale serve`, set `bind` to the machine's Tailscale address (`tailscale ip -4`) and connect with `http://<machine>.<tailnet>.ts.net:8750/mcp`. WireGuard still encrypts the traffic.

**A TLS reverse proxy.** For example, with Caddy, which gets the certificate itself:

```text
docs.example.net {
    reverse_proxy 127.0.0.1:8750
}
```

Add `docs.example.net` to `server.allowed_hosts`. If a browser-based client connects, also add its origin to `server.allowed_origins`.

## 6. Connect clients

**Claude Code**, with user scope so every project on the machine sees it and the token stays outside any repository:

```bash
claude mcp add --transport http --scope user joedocs https://docs.example.ts.net/mcp \
  --header "Authorization: Bearer jd_..."
claude mcp get joedocs        # Connected
```

In a session, `/mcp` should list nineteen JoeDocs tools. Ask *"List my JoeDocs projects"* to check the key's grants.

To create a key from the client machine without copying it by hand, run `key create` over SSH and capture the last line (non-interactive SSH may not have `~/.cargo/bin` on `PATH`, so give the full path):

```bash
TOKEN=$(ssh you@server '~/.cargo/bin/joedocs key create laptop --grant my-app:write' | tail -n 1)
```

**Keep the key out of files you might commit.** Put it in an environment variable, or in a client's user-level config outside any repository. Never put it in a project-level client config, such as an `opencode.json` or `.codex/config.toml` inside a repository: one `git add .` commits it. The examples below read the key from `JOEDOCS_API_KEY`. Fill that variable from a password manager when you start the client, for example with [`pass`](https://www.passwordstore.org/):

```bash
export JOEDOCS_API_KEY="$(pass show joedocs/token)"
```

The same substitution works for Claude Code: `--header "Authorization: Bearer $(pass show joedocs/token)"` stores the key in your user-level `~/.claude.json`, not in the project.

**OpenCode** replaces `{env:NAME}` in its config. In the global config, `~/.config/opencode/opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "joedocs": {
        "type": "remote",
        "url": "https://docs.example.ts.net/mcp",
        "oauth": false,
        "headers": {
          "Authorization": "Bearer {env:JOEDOCS_API_KEY}"
        }
      }
    }
  }
}
```

`"oauth": false` stops OpenCode trying OAuth, which JoeDocs does not offer. If the variable is not set, OpenCode sends an empty value and the server answers `401 Unauthorized`.

**Codex** sends the variable named by `bearer_token_env_var` as the bearer token. In `~/.codex/config.toml`:

```toml
[mcp_servers.joedocs]
url = "https://docs.example.ts.net/mcp"
bearer_token_env_var = "JOEDOCS_API_KEY"
```

**Other MCP clients** need the Streamable HTTP URL (`https://<host>/mcp`) and the header `Authorization: Bearer <token>`. If the client can read the header or token from the environment, use that rather than writing the key into its config.

## 6a. Manage from another machine

The admin commands can also run from another machine, through the server's admin API, signed in as a user with a password and a TOTP code from an authenticator app. The API is off until you turn it on.

1. On the server, turn the API on and restart the service:

   ```toml
   [admin]
   enabled = true
   session_hours = 8    # how long a login lasts
   ```

2. On the server, create your user (with Docker: `docker compose exec joedocs joedocs user create alice`):

   ```bash
   joedocs user create alice
   ```

   It prints a one-time password on the last line, valid for 72 hours. Users are created only by an admin; every user is an admin in 0.5.1.
3. Sign in from the machine you manage it from. The first `joedocs auth login` asks for the server's URL, checks it, and saves it as `client.server` in `~/.config/joedocs/config.toml` (or the config file in use); you can also set it there yourself, with your user name:

   ```toml
   [client]
   server = "https://docs.example.ts.net"
   user = "alice"
   ```

   ```bash
   joedocs auth login --user alice   # the one-time password, then TOTP setup and your own password
   joedocs status
   joedocs key create ci --grant joedocs:read
   joedocs backup ~/joedocs-$(date +%F).tgz
   joedocs auth logout
   ```

   At this first sign-in it shows a TOTP secret, its `otpauth://` URI and a QR code: add it to Google Authenticator (or another TOTP app), type a code from it, then choose your password (12 or more characters). The machine keeps a device credential in `~/.local/state/joedocs/devices.toml` (`0600`), so later sign-ins there need only a code: when a session ends after `admin.session_hours`, the next command asks for one. A device unused for `admin.device_days` (30 by default) signs in with the password again.

`joedocs auth devices` lists your devices and `auth devices revoke NAME` ends one; `joedocs user disable alice` on the server (or as another signed-in user) ends all of a user's sessions and devices at once, and `user reset-totp alice` lets a user who lost their authenticator set up a new one. The server's `/admin/v1/` path must be reachable the same way as `/mcp` (for example through `tailscale serve`), and `server.allowed_hosts` must list the name you use.

**Sign-in attempts.** Five failed sign-ins lock that user name for 15 minutes (open sessions and MCP keys keep working), and anyone who can reach the sign-in can trigger that for a name they know. Keep `/admin/v1/` and `/admin/` off the public internet (a tailnet, a VPN) or behind a proxy that limits request rates. `joedocs user unlock NAME` clears a lock.

**Admin tokens (deprecated).** Before 0.5.1 the admin API took an admin token (`joedocs admin-token create NAME` on the server, `JOEDOCS_ADMIN_TOKEN` and `joedocs login` on the client). They still work and print a warning; once nobody uses them, set `[admin] token_login = false`, restart, and revoke them with `joedocs admin-token revoke NAME`.

## 6b. Admin web interface

The admin API can also serve pages for a browser under `/admin/`: browse, search and read documents, follow history and diffs, manage projects and keys, read the audit log, restore archives, and check, reindex or back up the store (the [reference](reference.md#admin-web-interface) lists every page). It is off until you turn it on, and needs the admin API.

1. On the server, turn both on and restart the service:

   ```toml
   [admin]
   enabled = true
   web_ui = true
   ```

   `joedocs serve` then prints `JoeDocs web interface: http://127.0.0.1:8750/admin/`, and `joedocs status` says it is on.
2. Create a user as in 6a step 2.
3. Open `https://<your server>/admin/` and sign in with your user name and password and a code. At the first sign-in, the one-time password leads to the setup page: scan its QR code (or type the secret) into your authenticator, type a code, and choose your password. The browser keeps a web session for `admin.session_hours`; the password is not stored. The Users page creates and manages users, and Your account (in the session menu) changes your password and revokes your devices.

**HTTPS.** Signing in is refused over plain HTTP except on the server itself (`http://localhost:8750/admin/`, `127.0.0.1` or `[::1]`), so reach it through `tailscale serve` or a TLS proxy (section 5), not through `bind` set to a Tailscale address. Over HTTPS the session cookie is `Secure` and host-only (`__Host-`).

**Proxies.** The interface checks the `Host` header, like `/mcp`, and accepts a form only when the browser's `Origin` names that same host (or an entry of `server.allowed_origins`):

- Add the public name to `server.allowed_hosts` and let the proxy pass the `Host` through. `tailscale serve` and Caddy's `reverse_proxy` do; with nginx, set `proxy_set_header Host $host;`.
- If your proxy rewrites `Host` to the backend address instead, add the public origin to `server.allowed_origins`, for example `allowed_origins = ["https://docs.example.net"]`, or every sign-in is refused as "not sent from this site".

**PlantUML diagrams.** The bundled renderer draws Mermaid diagrams in the browser. PlantUML diagrams are shown as source unless `admin.web_plantuml_url` names a PlantUML server, such as `https://www.plantuml.com/plantuml`. The admin's browser then fetches each diagram from `<url>/svg/<encoded diagram text>`, so **that server receives the text of every diagram you view**; JoeDocs itself sends nothing. For private diagrams run your own server, for example the `plantuml/plantuml-server` container on the same machine, and set its address (`http://127.0.0.1:8080` is allowed only on loopback; use `https://` otherwise).

## 7. Operate

| Task | How |
| --- | --- |
| Upgrade | Back up, install the new version, restart, then check: see [upgrading.md](upgrading.md), with notes for each version. In short, `brew upgrade joedocs && brew services restart joedocs`, or `cargo install joedocs --locked && systemctl --user restart joedocs`. `serve` migrates the database and rebuilds the index on start when needed. |
| Change the configuration | Edit `config.toml`, then restart the service. Rerun `joedocs systemd install` only if you moved the binary or changed what the unit pins (the config file path, overrides, storage paths). |
| Give a machine access | `joedocs key create NAME --grant PROJECT:PERMISSION`, or `joedocs key grant NAME PROJECT:PERMISSION` for an existing key. |
| Replace a leaked token | `joedocs key rotate NAME`, then update the client's header. `joedocs key revoke NAME` cuts a machine off for good. |
| Bring back an archived document | `joedocs archive list`, then `joedocs archive restore JD-42`. |
| Back up | `joedocs backup ~/joedocs-$(date +%F).tgz`, safe while the service runs. Keep the file private: it holds API key hashes. |
| Permissions | The state directory is private (`0700`, the database `0600`): it holds API key hashes and admin TOTP secrets. `serve` tightens an older install's permissions on start and logs it. Run admin commands as the account that owns it. |
| Restore a backup | Stop the service, then `joedocs backup --restore --state-dir NEW_STATE --content-dir NEW_VAULT FILE.tgz` into empty or new directories. It checks the result and prints the `[storage]` settings to point the configuration at; set them (or move the directories into place), then start the service. |
| Check consistency | `joedocs check` (exits `1` if it finds problems, and says how to fix each). `joedocs reindex` rebuilds the index from the history. |
| Back up | Back up `state_dir` (the database holds the projects and keys; `history.git` holds every revision) and the vault. The document index can be rebuilt from `history.git` alone. |
| Browse in Obsidian | Copy the vault one way into an Obsidian vault on a schedule, for example `rsync -a --delete <home>/ <obsidian-vault>/JoeDocs/`. Edit through JoeDocs, not in the vault: JoeDocs refuses to overwrite files changed behind its back. |

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `403 Forbidden` | The `Host` the client used is not in `server.allowed_hosts` (or its `Origin` is not in `server.allowed_origins`). Add it and restart. |
| `401 Unauthorized` | The token is wrong, rotated or revoked. Check with `joedocs key list`. |
| Signing in says "sign-in is locked" | Five failed sign-ins lock the user name (or admin token) for 15 minutes. Wait, or run `joedocs user unlock NAME` on the server. |
| `joedocs auth login` says "sign in once at a terminal" | The first sign-in on a machine asks for the password, which needs a terminal; scripts then renew with `joedocs auth login --code CODE`. |
| A command says "this device's sign-in has ended" | The device was revoked, or unused for `admin.device_days`. Sign in again with `joedocs auth login`. |
| `/admin/` answers `401 Unauthorized` in the browser | `admin.enabled` or `admin.web_ui` is off, so `/admin/` is an unknown path, which the MCP endpoint's key check refuses. Turn both on and restart; `joedocs status` says whether the interface is on. |
| Signing in says "Signing in needs HTTPS" | The page was opened over plain HTTP from another machine. Use `https://` through `tailscale serve` or a TLS proxy (section 6b). |
| Signing in says the form was "not sent from this site" | The proxy changes the `Host` header. Pass it through, or add the public origin to `server.allowed_origins` (section 6b). |
| `Failed to connect to bus` from `systemctl --user` | Not a real login session (for example after `su`). Log in over SSH as that user. |
| The service stops at logout | Enable lingering: `sudo loginctl enable-linger "$USER"`. |
| Exit code `78` on start | A configuration error; the message names the file, variable or key. `joedocs config show` shows every value and its source. |
| `integrity` errors from tools | A vault file was edited outside JoeDocs. `joedocs check` lists it and gives the git command that discards the change. |
