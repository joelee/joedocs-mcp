# Configuration

## Layers

Effective configuration is built from four layers, highest precedence first:

1. CLI flags (only keys that have a flag)
2. Per-key environment overrides: `JOEDOCS__<SECTION>__<KEY>`
3. The selected config file
4. Built-in defaults, so the application runs with no config file

Run `joedocs config show` to see the effective value of every key and which layer supplied it.

## Config file selection

The first source that exists wins; files are never merged.

1. `--config <path>`: error (exit 78) if the file does not exist
2. `JOEDOCS_CONFIG_FILE`: error if the file does not exist; an empty value counts as unset
3. `$XDG_CONFIG_HOME/joedocs/config.toml`: only when `XDG_CONFIG_HOME` is an absolute path
4. `$HOME/.config/joedocs/config.toml`: also on macOS
5. `/etc/joedocs/config.toml`
6. `./config.toml`
7. None found: defaults plus overrides only

Run `joedocs config paths` to see the search order and the selected file.

## Keys

`config.example.toml` lists every key with its default; a test keeps it in sync with the schema.

| Key | Type | Default | Environment override | CLI flag | Description |
| --- | --- | --- | --- | --- | --- |
| `log.level` | string: `error`, `warn`, `info`, `debug`, `trace` | `"info"` | `JOEDOCS__LOG__LEVEL` | `--log-level` | Minimum level written to stderr. |
| `server.bind` | string: socket address | `"127.0.0.1:8750"` | `JOEDOCS__SERVER__BIND` | | Address and port the MCP server listens on. Use a TLS reverse proxy or Tailscale for remote clients. |
| `server.allowed_hosts` | array of strings: `host` or `host:port` (1 or more) | `["localhost", "127.0.0.1", "::1"]` | `JOEDOCS__SERVER__ALLOWED_HOSTS` | | `Host` header values to accept, which blocks DNS rebinding. Behind a reverse proxy, add the public name, e.g. `docs.example.net`. |
| `server.allowed_origins` | array of strings: `http(s)://host[:port]` | `[]` | `JOEDOCS__SERVER__ALLOWED_ORIGINS` | | `Origin` header values to accept, e.g. `["https://docs.example.net"]`. Requests without `Origin` (CLI and desktop clients) are allowed; any other `Origin` gets `403`. |
| `server.max_document_bytes` | integer (1 or more) | `1048576` | `JOEDOCS__SERVER__MAX_DOCUMENT_BYTES` | | Largest accepted document, front matter included, in bytes. |
| `storage.home` | string: absolute path, or empty | `""` | `JOEDOCS__STORAGE__HOME` | | Content directory (`$JOEDOCS_HOME`) holding `<project>/<type>/*.md`. Empty means not set; commands that need it fail until it is. |
| `storage.state_dir` | string: absolute path, or empty | `""` | `JOEDOCS__STORAGE__STATE_DIR` | | Database and history directory. Empty means `$XDG_DATA_HOME/joedocs` (when `XDG_DATA_HOME` is absolute), else `$HOME/.local/share/joedocs` (macOS too). |
| `documents.types` | array of slugs | `["home", "plans", "ideas", "issues", "reviews", "research", "specs", "notes"]` | `JOEDOCS__DOCUMENTS__TYPES` | | Document types; each is a folder under every project. `all` is reserved. |
| `documents.statuses` | array of slugs | `["draft", "review", "approved", "rejected", "superseded", "done"]` | `JOEDOCS__DOCUMENTS__STATUSES` | | Allowed values of the `status` front matter key. |
| `documents.default_type` | string | `"home"` | `JOEDOCS__DOCUMENTS__DEFAULT_TYPE` | | Type for new documents that name none; must be in `documents.types`. |
| `documents.default_status` | string | `"draft"` | `JOEDOCS__DOCUMENTS__DEFAULT_STATUS` | | Status for new documents that name none; must be in `documents.statuses`. |
| `documents.index_file` | boolean | `true` | `JOEDOCS__DOCUMENTS__INDEX_FILE` | | Write `INDEX.md`, a table of the live documents sorted by ID, in each project folder (see [reference](reference.md#documents)). When `false`, JoeDocs neither writes nor checks it, and leaves existing files in place. |
| `documents.max_files` | integer (1 or more) | `500` | `JOEDOCS__DOCUMENTS__MAX_FILES` | `200` | Most live sub-files a multi-file document may hold, besides its main file. Creating, writing, moving in or restoring one more (`restore_document_version`, `archive restore`, `document restore-file`) is refused with `invalid_argument`. |
| `admin.enabled` | boolean | `false` | `JOEDOCS__ADMIN__ENABLED` | `true` | Serve the remote admin API under `/admin/v1/`, so `joedocs` on another machine can run admin commands with an admin token and a TOTP code (see [deployment](deployment.md)). Off by default. |
| `admin.session_hours` | integer | `8` | `JOEDOCS__ADMIN__SESSION_HOURS` | `12` | How long a session opened by `joedocs login` lasts, in hours: 1 to 168. |
| `client.server` | string | `""` | `JOEDOCS__CLIENT__SERVER` | `"https://docs.example.ts.net"` | On a client machine: the server whose admin API `joedocs` uses. When set (or with `--server URL`), `project`, `key`, `archive`, `check`, `reindex`, `status`, `get` and `backup` run on that server after `joedocs login`. Must be `https://`, or `http://` to `localhost`, `127.0.0.1` or `[::1]`. Empty means the local store. |

A slug is 1 to 64 characters of `a-z`, `0-9` and `-`, starting with a letter or digit. Lists of slugs must be non-empty and have no duplicates.

## Environment override rules

- Variables start with `JOEDOCS__`; each further `__` separates a table level. Matching after the prefix is case-insensitive.
- Values are parsed as the key's type: strings are taken literally, booleans and numbers use TOML syntax, arrays use TOML inline arrays such as `["a", "b"]`. An integer is accepted for a float key.
- These are startup errors (exit 78) that name the variable: an unknown key, a key that is a whole table, an unparseable value, a value of the wrong type or out of range, and an empty value for a non-string key.
- An override value must be a single TOML value; extra lines are rejected.
- After all layers are applied, rule checks run (for example "`documents.default_type` must be in `documents.types`"). A broken rule is a startup error (exit 78) naming the key and the layer that set it, e.g. `invalid documents.default_type (from env JOEDOCS__DOCUMENTS__DEFAULT_TYPE): "memo" is not one of documents.types`.

## Secrets

Secrets are read from environment variables only and are never part of the config file or overrides. For local development they can live in `.env` (git-ignored); real environment variables take precedence. `config show` reports only whether each secret is set.

| Variable | Required | Description |
| --- | --- | --- |
| `JOEDOCS_API_TOKEN` | No | Example secret; replace with your own. |
| `JOEDOCS_ADMIN_TOKEN` | For `login` and remote commands | The admin token (`jda_...`) printed once by `joedocs admin-token create` on the server. Read it from a password manager, for example `JOEDOCS_ADMIN_TOKEN="$(pass show joedocs/admin)" joedocs login`. |

Keep `.env.example` listing every supported secret with a safe placeholder value.
