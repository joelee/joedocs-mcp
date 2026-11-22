# Developer guide

## Setup

Install `rustup`, `just` (1.54 or later; CI uses 1.59.0), `git`, `gitleaks` and `actionlint` with your package manager (on macOS: `brew install rustup just git gitleaks actionlint`), then run:

```bash
just setup
```

This installs `cargo-nextest`, `cargo-llvm-cov` and `cargo-deny`, normalises `Cargo.lock`, and turns on the git hook in `.githooks/` (`git config core.hooksPath .githooks`). Before each commit the hook runs `just pre-commit`: gitleaks on the staged changes, `cargo fmt --check` and clippy when Rust files or the Cargo manifests are staged, and `actionlint` when workflows are. A clone set up before 0.1.2 used the `pre-commit` framework; run `pre-commit uninstall` there if it is installed, then `just setup`. The history tests run the real `git` from `PATH`, isolated from your own git configuration. `rustup` installs the toolchain pinned in `rust-toolchain.toml` on first use. Commit `Cargo.lock` if `just setup` changed it.

## Workflow

1. Write or update a failing test (TDD).
2. Implement the change.
3. Update the documentation the change affects (see `docs/AGENTS.md`).
4. Run `just ci` and fix anything it reports.

| Command | Purpose |
| --- | --- |
| `just fmt` | Format code. |
| `just lint` | Clippy with warnings as errors. |
| `just test` | Run tests with nextest. |
| `just coverage` | Tests under coverage; fails below 80% lines. |
| `just coverage-html` | Browse the coverage report. |
| `just doctest` | Run doc-tests (nextest skips them). |
| `just doc` | Build API docs with warnings as errors. |
| `just deny` | Licences, bans and sources. |
| `just advisories` | RustSec advisories: its own CI check, not part of `just ci`; an active advisory blocks a production release (see Releasing). |
| `just actionlint` | Lint the GitHub Actions workflows (shellcheck and pyflakes integrations off). |
| `just pre-commit` | The checks the git hook runs before each commit. |
| `just secrets` | Scan git history for secrets: gitleaks' built-in rules plus a `.gitleaks.toml` rule for JoeDocs API keys (checked by `tests/secrets.rs`). |
| `just msrv` | Check the declared MSRV builds (library profile). |
| `just docker-test [port]` | Build a throwaway Compose stack, run the deployment tests against it before and after a restart, then remove it (default port 18750). |
| `just ci` | Everything above, in CI order. |

## Testing

- Unit tests sit beside the code and use `MapEnv` and `MemFs` instead of the real environment and filesystem.
- Integration tests in `tests/cli.rs` run the binary with `env_clear()`, a temporary `HOME` and working directory, so your own config files and `.env` cannot affect results.
- Never call `std::env::set_var` or `remove_var`; they are `unsafe` in Rust 2024 and race with parallel tests.
- `tests/store.rs` and `tests/mcp.rs` drive the store and the MCP server in-process against a temporary vault and real git. `tests/common/` holds the shared MCP client helpers.
- `tests/history.rs` covers the phase 4 operations (history, diff, restore, move, edit, append, archive restore, `check`) and checks that each replays identically through `reindex`.
- `tests/search.rs` searches a fixture vault built through the store, and `tests/reindex.rs` checks that rebuilding the index from the history gives the same tables, recovers a lost write or database, and refuses a history it cannot replay.
- `tests/admin_api.rs` drives the admin API over raw HTTP with a `FixedClock` (expiry, lockout) and checks the audit rows; `tests/client.rs` runs the binary as a client of an in-process server on its own thread (the client runs its own runtime), compares remote and local output command by command, and drives the re-login prompt through the library with a scripted `Prompt`. TOTP codes in tests come from the token's secret and the server's clock (`joedocs::totp::code`).
- `tests/deployment.rs` is a black-box suite for a running server. It is ignored by `just test` and `just ci`; `just docker-test` runs it against Docker Compose, or set `JOEDOCS_TEST_URL`, `JOEDOCS_TEST_TOKEN` and `JOEDOCS_TEST_PROJECT` and run `cargo nextest run --test deployment --run-ignored only` against any server.

## Running with Docker

`compose.yaml` runs `joedocs serve` in a container. On start it runs `joedocs init` (which does nothing once initialised). The vault and the state are bind-mounted from `.local/data` (git-ignored), so they persist across rebuilds and you can inspect them on the host:

```text
.local/data/
├── vault/                Markdown files, one folder per project and type
└── state/
    ├── joedocs.db        SQLite index, projects and API keys
    ├── history.git/      git history (work tree: vault/)
    └── joedocs.lock
```

```bash
just docker-up                   # creates .local/data/{vault,state}, starts the stack as you
docker compose exec joedocs joedocs project add joedocs --key JD --title JoeDocs
docker compose exec joedocs joedocs key create laptop --grant joedocs:write   # last line is the token
claude mcp add --transport http joedocs http://127.0.0.1:8750/mcp \
  --header "Authorization: Bearer jd_..."
docker compose logs --follow
docker compose down              # stops the stack; the data stays in .local/data
```

- The port is published on loopback only: `JOEDOCS_PORT` (default `8750`). `JOEDOCS_LOG_LEVEL` sets the log level.
- Other settings go under `environment:` as `JOEDOCS__SECTION__KEY`, e.g. `JOEDOCS__SERVER__ALLOWED_HOSTS: '["localhost", "127.0.0.1", "docs.example.net"]'`, or mount a file at `/etc/joedocs/config.toml`.
- The server runs as `JOEDOCS_UID:JOEDOCS_GID` (default `1000:1000`; `just docker-up` sets your own), so the files in `.local/data` belong to you. If you run `docker compose up` yourself, create `.local/data/vault` and `.local/data/state` first, or Docker creates them owned by root and the server cannot write.
- `JOEDOCS_DATA` moves the data elsewhere (default `./.local/data`). `just docker-test` uses a temporary directory, so it never touches yours.
- Inspect the data on the host, for example `git --git-dir .local/data/state/history.git log --format='%ad %an | %s'` or `sqlite3 .local/data/state/joedocs.db`. Change it only through JoeDocs while the server runs: files edited by hand are refused for writes until resolved (`joedocs check` explains how).
- To start afresh: `docker compose down && rm -rf .local/data`.
- After an upgrade, `serve` rebuilds the index from the history if the database is from an older build; `docker compose logs` shows the outcome. To rebuild by hand: `docker compose exec joedocs joedocs reindex`.

## Web interface

The admin web interface lives in `src/web/`, with its Askama templates in `templates/` at the repository root (Askama's default directory; Askama's `config` feature, which would let them live elsewhere, depends on the unmaintained `basic-toml`). The templates and the static files in `src/web/assets/` are compiled into the binary; `Cargo.toml` `include`, the `Dockerfile` and `.dockerignore` list `templates/`. There is no Node or WASM build step.

### Editing pages and styles

- **JavaScript is required.** The web interface needs a browser with JavaScript turned on, and new pages and features may depend on `app.js` and htmx with no fallback. The fallbacks some pages still have (a filter form's button, dialogs opened by invoker commands) may stay, but nothing needs to keep working without scripts, and tests need not check it.
- **Templates** in `templates/` are checked and compiled by `cargo build`; a template error is a compile error. Each page module in `src/web/pages/` owns a `#[derive(Template)]` struct for its template. Askama escapes every value; use `|safe` only for HTML the code wrote itself from escaped parts (the Markdown renderer's output, a rendered fragment).
- **No inline script or style.** The Content-Security-Policy allows no inline script, and no inline style except on pages with a Mermaid diagram. Behaviour goes in `src/web/assets/app.js` as event listeners (never `eval`, `hx-on` or `js:` values); styles go in `app.css`, with colours as the design tokens at its top (`light-dark()` pairs, at least 4.5:1 contrast in both themes). `tests/web.rs` checks every page for one `<h1>`, labelled controls, table headers, image `alt`, and no `style`, `on...` or inline `<script>`.
- **Live filters.** A filter form that applies as it changes carries `hx-get` (its own path), `hx-trigger` (`change, submit` for selects, `input changed delay:300ms, submit` for text), `hx-target="#results"` and `hx-push-url="true"`; its results sit in `<section id="results" data-focus="keep" data-announce="...">`, rendered by their own template (`*_results.html`), which the handler sends alone when `WebRequest::wants_fragment`. The submit button sits in `<div class="filters-apply">`: `app.js` puts `js` on `<html>`, and `.js .filters-apply` hides the button from view but keeps it in the form, so Enter still submits and the button shows again when the keyboard reaches it.
- **Sortable tables.** A table that shows every row is marked `<table data-sortable>`, and each header it sorts by `data-sort` (leave it off action columns). `app.js` turns those headers into buttons that cycle ascending, descending and the server's order, and sets `aria-sort`. A cell sorts by its `data-sort-value` when it has one (give every time cell its RFC 3339 value, empty for "never"), a `.num` column as numbers, and any other cell by its text, with numbers inside compared by value. A paged table sorts on the server instead, with header links (Documents, `src/web/pages/documents.rs`).
- **The Board's script** is `src/web/assets/board.js`, loaded only by the Board (through the `scripts` block of `templates/base.html`, as `pele.global.js` is for documents); `app.js` does not know the Board. Styles that move with the content (`view-transition-name`, popover positions) are set through the CSSOM (`element.style`), which `style-src` does not restrict, never as `style` attributes. Only the cards that moved, changed, came or went are named for a view transition: naming every card is too slow on a large board. `?debug=1` on the Board shows the script's counters.
- **Filter parts** (JD-71) are `src/web/assets/filters.js`, loaded by the Board before `board.js`, and the "Filter parts" section of `app.css`: pills, a searchable picker, a switch, segmented controls, and active-filter chips. They work on any `<form data-filter-form>`: a group of checkboxes sits in `[data-filter-group="<name>"]` with `data-filter-label` (its name in a chip) and `data-filter-default` (the values lit by default, comma-separated, or empty when nothing chosen means all; `data-filter-default-label` names that default in words); a picker is `[data-filter-picker]` and pills `[data-filter-pills]` (with `data-pills-all`, and `data-pills-default` and `data-pills-every` in a `data-pills-tools` holder, each hidden while it would change nothing, as the server also draws them); `[data-filter-chips]`, `[data-filter-count]` and `[data-filter-clear]` show the filters in force (Clear all from two chips); any other control inside an element with `data-filter-default` is left out of the query at that value; a checkbox with `role="switch"` shows as a chip when on; a `[data-view-menu]` is a compact button (`data-view-open`, with its text in `data-view-summary`) that opens a popover (`data-view-pop`) of controls, each `[data-view-part]` adding the `data-short` text of its chosen value to the summary (after " · ", or its `data-view-join`). Every part is a native, labelled control (give each an `id` and a `for`, which `tests/web.rs` looks for). A page reads the address to write with `form.joedocsFilters.query()` (one key per name, values comma-separated and each URL-encoded, defaults left out) and listens for the form's `change` and `filters:changed`; the server reads such lists with `WebRequest::query_list`, which splits each raw value on commas before decoding and also accepts repeated keys. The script knows nothing of the Board.
- **The document page's script** is `src/web/assets/document.js`, loaded only on a document's latest revision, whose page wrapper (`data-doc-page`) carries `data-live` and `data-revision`. After it swaps a new revision in, it fires `joedocs:swapped` on the wrapper, and `app.js` draws the diagrams, sets up sorting and the Files panel's folds there, as after an htmx swap.
- **What the browser keeps.** `localStorage`: the theme (`joedocs-theme`) and the remembered Board and Documents views (`joedocs-board-view`, `joedocs-documents-view`, the view's own keys of the address, copied as written so comma-separated lists keep their commas), kept by `app.js`; the Board's script reports a change with a `joedocs:view-changed` event. `sessionStorage`: the Files panel's folds per document (`joedocs-files-<ID>`). Every access is wrapped, so a browser that refuses storage still works. Nothing else is stored: no content, tokens or session data.
- **Dialogs** open with invoker commands (`commandfor` and `command="show-modal"`) and close with a `formmethod="dialog"` button, so they work without JavaScript.
- **Routes.** A new route is added to `Route` in `src/web/mod.rs` with its audit tool (the matching admin API operation), and to the route table in that module's documentation; a test requests every route in the table and checks its audit row. Every route but the sign-in page needs a session by default.
- **Static files** are served under `/admin/assets/<hash>/<name>`; the hash changes with the content, so browsers never keep an old copy.
- **Icons.** `src/web/assets/favicon.svg` is the source; `favicon.ico` (16, 32 and 48 pixels) and `apple-touch-icon.png` (180 pixels, square corners, because phones round them) are drawn from it with `rsvg-convert` (librsvg) and ImageMagick, which are needed only to redraw them, not to build. After changing the SVG:

  ```bash
  cd src/web/assets && t=$(mktemp -d)
  for n in 16 32 48; do rsvg-convert -w $n -h $n favicon.svg -o $t/$n.png; done
  magick $t/16.png $t/32.png $t/48.png favicon.ico
  sed 's/ rx="14"//' favicon.svg > $t/square.svg
  rsvg-convert -w 180 -h 180 $t/square.svg -o $t/touch.png && magick $t/touch.png -strip apple-touch-icon.png
  ```
- **Trying it.** `just scratch-server` (see [Scratch server](#scratch-server)). By hand: `JOEDOCS__ADMIN__ENABLED=true JOEDOCS__ADMIN__WEB_UI=true cargo run -- serve` with a scratch `JOEDOCS__STORAGE__HOME` and `JOEDOCS__STORAGE__STATE_DIR`, a user from `cargo run -- user create dev` on the same store (its one-time password is the last line), then `http://127.0.0.1:8750/admin/`, where the first sign-in sets up TOTP.

### Scratch server

`just scratch-server` runs a throwaway server from the working tree, for trying the web interface in other browsers and on phones before a release:

```bash
just scratch-server                  # build, then serve until Ctrl-C
just scratch-server fresh            # wipe the scratch store first
just scratch-server --port 8761 --https-port 8444 --no-tailscale
```

- **The store** is `.local/scratch` (git-ignored), kept between runs. The first run fills it with sample data over MCP (`examples/scratch_seed.rs`): projects `demo`, with more than one page of documents in every type and status, diagrams, revisions, a multi-file backlog and an archived document, and `sandbox`. It also creates user `dev` and prints its one-time password once. The first sign-in sets a password and an authenticator; later runs keep both, and print how to reset them. `fresh` deletes `.local/scratch` and starts over.
- **On this machine:** `http://127.0.0.1:8760/admin/` (`--port` changes it; `8750`, the default `server.bind`, is refused, as is a port in use).
- **On a phone:** signing in needs HTTPS off loopback, so, unless `--no-tailscale` is given, the script adds a `tailscale serve` entry on port `8443` (`--https-port`) and prints `https://<machine>.<tailnet>.ts.net:8443/admin/`. The phone needs the Tailscale app on the same tailnet. The entry is removed when the server stops, also after Ctrl-C or an error; if one is ever left, `tailscale serve --https=8443 off` removes it. On Linux, if `tailscale serve` is refused, allow your user once with `sudo tailscale set --operator=$USER`; the script never uses `sudo`. A port that Tailscale already serves is refused.
- **A proxy of your own** (Caddy, for example) can point at the loopback address. List the names it passes as `Host` in `SCRATCH_ALLOWED_HOSTS` (comma-separated), or the server answers `403`: `SCRATCH_ALLOWED_HOSTS=scratch.example.net just scratch-server`.
- **Another store:** `SCRATCH_DIR=.local/<name> just scratch-server` serves a store that another script created directly under `.local/`, with the same addresses and options. The script only serves such a store: it refuses one that has not been seeded, `fresh`, a symbolic link, and any path outside `.local/`. Only `.local/scratch` is ever wiped by it.
- **It never touches real data:** every `joedocs` command it runs uses `.local/scratch/config.toml` (rewritten on each run) and none of your `JOEDOCS_*` variables. It also runs in the store directory, so a `.env` where you start the script is not read (one inside the store directory is refused). The live server, the Docker stack's `.local/data` and your own configuration are left alone. The seeding key's token is never printed or written down.
- **One run per store:** each run locks its store for as long as it runs, with `.local/<store>.lock` (`.local/scratch.lock` here), a symbolic link naming the script's process ID. A second run on the same store, `fresh` included, stops before it checks, wipes or builds anything: `... is in use by process N (a serve or a reset of it); stop it first`. A lock whose process has ended (after `kill -9`, a crash or a reboot) is taken over, with a notice. If the refusal names a process that is not a serve or a reset (a reboot can give its number to another program), remove the lock file it names. Anything else at the lock path, a directory included, is refused with the path to remove.
- **Stopping it** with Ctrl-C is the normal end: the script and `just` exit with status 0 (the recipe's `[continue]` attribute, `just` 1.54 or later). Closing its terminal stops it the same way. Ctrl-C during the build or the seeding exits with 130. Every way of stopping releases the lock.
- The script is bash, for Linux and macOS (`tailscale` from `PATH`, or the macOS app), and needs `curl`.

### Driving the Board

`just board-drive` plays the part of busy agents against a running scratch server, so the Board (0.5.5) can be watched, demonstrated and checked on other devices:

```bash
just scratch-server                  # in one terminal
just board-drive                     # in another, with /admin/board open
DRIVE_PAUSE_MS=5000 just board-drive # slower, for a demo
DRIVE_PAUSE_MS=0 just board-drive    # wait for a keystroke before each step
just board-drive --port 8761         # a scratch server on another port
```

- **What it does,** over MCP as an agent would (`examples/board_drive.rs`), in about 40 seconds: a plan is drafted, lit for approval (`attention`), reviewed and approved; a builder picks it up (`implementation_status: in-progress`, the 🏗️ mark with its `current_step`) and works through STEP-01 to STEP-03; ten sample documents move from draft to review in about two seconds, and back; an issue arrives in `sandbox`, lit; the build completes and the plan is closed; the issue is archived, then restored by a local `joedocs archive restore`, a write from another process. Each step prints one line.
- **The pause** between steps is `DRIVE_PAUSE_MS` milliseconds (default 3000). `DRIVE_PAUSE_MS=0` (or `00`: the value is read as a base-10 number) waits for a keystroke instead, after a prompt naming the next step, so you can talk over a demo or hold a frame; any key will do, with no Enter. It needs a terminal, and stops before writing anything without one. The script puts the terminal back as it was when it ends, Ctrl-C included.
- **Its key** is `board-drive`, with write on `demo` and `sandbox`: created on the first run and rotated on each later one. The token stays in the script's shell; it is never printed or written down.
- **Another store:** `SCRATCH_DIR=/path/to/store just board-drive` drives a server using that scratch store (its `config.toml` and a seeded store), for example a copy of `.local/scratch`. A relative path is taken from the directory the command runs in, which is the repository root under `just`.
- **`?debug=1`** on the Board shows the page script's counters (events, refreshes, transitions and skipped ones, latency, frames over 50 ms): developer use only.

#### The showcase

`just board-drive-showcase` stages the Board for recording the README media: a store of its own in `.local/showcase` (git-ignored), with three projects and four agents on two machines, kept apart from `.local/scratch`.

```bash
just board-drive-showcase reset   # wipe .local/showcase and create it again
just board-drive-showcase serve   # serve it on port 8770, and over Tailscale on HTTPS port 8444 (--no-tailscale, --https-port N)
just board-drive-showcase         # in another terminal: play the feature flow, once per reset
DRIVE_PAUSE_MS=0 just board-drive-showcase   # wait for a keystroke before each beat
```

- **`reset`** creates the projects `project-a`, `project-b` and `project-c` (card IDs `PA`, `PB`, `PC`), and four API keys, each with write on the three projects: `laptop-claude`, `laptop-opencode`, `mac-claude` and `mac-codex`. It then writes three background documents in `project-b` (by `laptop-opencode`, as the MCP client `opencode`) and three in `project-c` (by `mac-codex`, as `codex`), always with the same content, so a fresh store always gets the same IDs. It refuses to run while a serve or another reset of the store runs (each holds the store's lock, `.local/showcase.lock`, from before it checks or builds anything until it ends), or while something answers on the port given or on the one `serve` last used (a `joedocs serve` of the store started by hand). An interrupted reset leaves a store that `serve` refuses until the next reset (which picks up where it stopped).
- **`dev` is kept across resets** once it has set up an authenticator: reset moves the old store to `.local/showcase.old`, copies `dev`'s row of the users table (password hash and authenticator; not its sessions or devices) into the new store's database, and removes the old store once the new one is complete. After a reset, sign in again with the same password and a code: no new setup. The row goes from database to database and is never printed. The first reset, a `dev` that has not finished its first sign-in, or a users table of another shape than the copy knows (after a database migration, until the copy is updated), gives a new `dev` and prints its one-time password, with the reason.
- **`serve`** runs `just scratch-server` on this store (`SCRATCH_DIR=.local/showcase`), on port 8770 and Tailscale HTTPS port 8444, so it can run beside the scratch server (8760 and 8443); `--port N` changes the first for every command, `--https-port N` the second. One server per store: `serve` refuses while another serve or a reset of the store runs (it holds the store's lock, `.local/showcase.lock`, as `just scratch-server` does), and while the port it last used answers, which catches a `joedocs serve` of the store started by hand.
- **Playing** (no argument) runs one feature from idea to done, one beat at a time, each written over MCP by the key named for it, so every card shows its agent as the last writer:
  1. `mac-claude` drafts the idea PA-1 "Offline sync for field notes", puts it to you (`review`, lit "Waiting for your decision") and records your acceptance (`approved`).
  2. `laptop-claude` drafts the plan PA-2 from it and puts it to you (lit "Waiting for your approval"), while `mac-codex` drafts another plan, PB-4, in `project-b`.
  3. PA-2 is approved, and PB-4 goes to review, lit.
  4. `mac-claude` builds PA-2: 🏗️ with STEP-01, then STEP-02 to STEP-05. PB-4 stays lit beside it, which is the frame for a still of both kinds of card.
  5. The build completes and PA-2 lights up for your sign-off. `laptop-claude` closes PA-2 and PA-1 (`done`), and `mac-codex`'s PB-4 is rejected.

  Meanwhile `laptop-opencode` changes three documents in `project-b`. At the default pause (3000 ms) the flow takes about 45 seconds, almost all of it pauses, so a shorter `DRIVE_PAUSE_MS` shortens it in proportion. `DRIVE_PAUSE_MS=0` waits for a keystroke before each beat, after a prompt naming it, as for `just board-drive`. A second play refuses to start until the next reset.
- **The Board address** it prints at the start selects every status the flow uses, closed ones included, and a row per project, so done and rejected cards stay in view: `http://127.0.0.1:8770/admin/board?status=draft&status=review&status=approved&status=parked&status=done&status=rejected&group=project`.
- **The agents' logs:** each beat line also goes to `.local/showcase/agents/<key>.log`, with the local time of day (this machine's offset from UTC, from `date +%z`). The four `tail -F` commands it prints give a terminal per agent beside the Board; start them before the play if you like, as `-F` waits for the files.
- **Kept apart from real data** as `just scratch-server` is: every `joedocs` command uses the showcase's config, none of your `JOEDOCS_*` variables and no `.env`.
- **The keys' tokens** stay in the script's shell; they are never printed or written down.

### Vendored browser files

Third-party browser files sit in `src/web/assets/vendor/`, unmodified, each with its licence text beside it. To update one, download the new version from the npm registry, check the tarball against the registry's `integrity` value, copy the file and its licence, and update this table.

| File | Version | Source | SHA-256 | Licence |
| --- | --- | --- | --- | --- |
| `htmx.min.js` | htmx 4.0.1 | `dist/htmx.min.js` from <https://registry.npmjs.org/htmx.org/-/htmx.org-4.0.1.tgz> (integrity `sha512-oF7JgXhDW9UXCy/k1ZYFUQgzu/rm19ORWPSPNL91vPluB2PB9+MV55tyQdH/w7o2EAbTYUG5SDW1R60mdkdqGg==`) | `a0abb04f2f45a1f89d1d4844023245537e5e6014c87b144553e0607b4cc32130` | 0BSD, `htmx.LICENSE` |
| `pele.global.js` | Pele 1.13.0 (Mermaid diagrams) | `dist/pele.global.js` from <https://registry.npmjs.org/pele/-/pele-1.13.0.tgz> (integrity `sha512-Ie8UKY9x00MsuR3jO6AzUwzK2Z6MIV3Tf4EDdITiuk8Uaz8K7g0R082WqsqNaSU8DdgkrRvZThzhit83mi/uiw==`) | `0b0a46c57a5a6971670c09f5ec4066705bd78505f90fd11adf6260325efa0910` | MIT, with the Lucide ISC notice, `pele.LICENSE` |

## Running as a service

Running JoeDocs as a systemd or `brew services` service, reaching it from other machines, and operating it are in the [deployment guide](deployment.md). When you change `src/systemd.rs`, check the rendered unit with `cargo run -- systemd install --dry-run` (and `--system --run-as USER`).

## Debugging

- `cargo run -- --log-level debug` writes debug logs to stderr.
- `cargo run -- config show` explains where every config value came from.
- `cargo run -- config paths` shows which config file was picked.
- `cargo run -- reindex` rebuilds the database index from the history and lists vault files that differ from it.
- `cargo run -- check` reports drift between the vault, the history and the index without changing anything, and says how to resolve it.

## CI

CI runs on GitHub Actions ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) for pushes to `main` and for pull requests:

- `just ci` on Ubuntu and macOS, with full git history (for the gitleaks scan) and `NEXTEST_PROFILE=ci`; the JUnit report is uploaded as an artifact.
- `just docker-test`, which builds the image and runs the deployment tests against it.

The RustSec advisory check runs on its own, in [`.github/workflows/advisories.yml`](../.github/workflows/advisories.yml) (job `cargo deny advisories`, `just advisories`), on pushes to `main`, pull requests, daily and by hand. It is not one of the checks `main` requires, so an advisory does not block a merge; it shows red while one is active, and blocks a production release.

The same workflow runs as a reusable workflow (`workflow_call`) from [`.github/workflows/release.yml`](../.github/workflows/release.yml) on every release tag, before anything is published (see Releasing).

Actions are pinned to commit SHAs and tools to exact versions (the `TOOLS`, `GITLEAKS_VERSION` and `ACTIONLINT_VERSION` variables, with gitleaks and actionlint checksums per platform). Keep the tool versions in step with what `just setup` installs locally. Dependabot proposes weekly updates for actions and crates; every proposal runs CI, including `cargo deny`.

## Releasing

Versions number the command line, the configuration and the MCP tools; the Rust library in the crate is internal. A published version can never be replaced (only yanked), so check before publishing.

Pushing a signed tag runs [`.github/workflows/release.yml`](../.github/workflows/release.yml): a `vX.Y.Z` tag makes a production release, a `vX.Y.Z-<pre>` tag (such as `v0.5.6-dev`) a GitHub pre-release (see Pre-releases). It checks the tag, then runs the full CI gate and the advisory check on the tagged commit and, alongside them, builds the release binaries: Linux x86_64 and aarch64 (static, musl) and macOS Apple silicon. Each is checked (`--version`, and on Linux no shared libraries) and kept as a workflow artifact. For a production release, the workflow then waits for approval in the `release` environment, publishes to crates.io with Trusted Publishing (no stored token), and asks the Homebrew tap to bump the formula. Last, it creates the GitHub release from the CHANGELOG section, with the three `joedocs-X.Y.Z-<target>.tar.gz` archives, their `SHA256SUMS`, and a build-provenance attestation for each archive. Release builds use no cache.

### One-time setup

1. **crates.io:** in the `joedocs` crate's Settings → Trusted Publishing, add a GitHub publisher: owner `joelee`, repository `joedocs-mcp`, workflow `release.yml`, environment `release`. After the first automated release, revoke the old `cargo login` token on crates.io.
2. **GitHub environment:** Settings → Environments → `release`, with yourself as required reviewer and deployments limited to `v*` tags.
3. **Homebrew tap:** finish the tap's setup ([`docs-release.md`](https://github.com/joelee/homebrew-oss/blob/main/docs-release.md), parts 1 and 2), then add to this repository the variable `TAP_BOT_CLIENT_ID` (the App's client ID, shown on its settings page) and, in the `release` environment, the secret `TAP_BOT_PRIVATE_KEY`. Without them the release still publishes, and the tap's daily check picks up the new version.
4. **Signing key:** release tags are signed (`git tag -s`), so git needs a signing key (`user.signingkey`, GPG or SSH with `gpg.format`), and the public key must be on your GitHub account, because the workflow refuses a tag that GitHub does not report as verified.

### Each release

No production release while a RustSec advisory is active: `just advisories` (`cargo deny check advisories`) must pass with `ignore = []` in `deny.toml`. If it reports an advisory, of any kind (`unmaintained` included), do not release, and do not ignore the advisory or narrow the check: file an issue, and wait for or make a fixed dependency (see `AGENTS.md`, Security). Meanwhile the work can go out as a pre-release (below). The release workflow enforces this: for a `vX.Y.Z` tag its advisory check fails before anything is published.

1. On a release branch, move the `Unreleased` entries in `CHANGELOG.md` under `## [X.Y.Z] - YYYY-MM-DD`, add the compare links at the bottom, and set `version` in `Cargo.toml`.
2. Check what would be published and that it builds on its own:

   ```bash
   cargo package --list --locked          # only src/, Cargo.*, README, LICENSE, CHANGELOG, config.example.toml
   cargo publish --dry-run --locked       # builds the packaged sources
   cargo install --path . --locked --root /tmp/joedocs-release && /tmp/joedocs-release/bin/joedocs --version
   ```

3. Run `just ci`, open a pull request, and merge it once CI passes.
4. Dry-run the release workflow, from `main` or, before merging, from the pushed release branch. It runs the checks, CI, the binary builds and `cargo publish --dry-run`, and never publishes, releases or attests. Do this whenever the workflow changed:

   ```bash
   gh workflow run release.yml -f dry_run=true                 # from main
   gh workflow run release.yml --ref <branch> -f dry_run=true  # from a branch
   ```

5. From an up-to-date `main`, sign, check and push the tag:

   ```bash
   git tag -s vX.Y.Z -m "JoeDocs X.Y.Z"
   git tag -v vX.Y.Z                  # check the signature before pushing
   git push origin vX.Y.Z
   ```

6. In the Actions tab, open the Release run and approve the `release` deployment once the checks and CI pass.
7. Check the result:
   - the version on crates.io;
   - the GitHub release, with the three archives and `SHA256SUMS`, checked with `gh release download vX.Y.Z -p '*' -D /tmp/jd-X.Y.Z && (cd /tmp/jd-X.Y.Z && shasum -a 256 -c SHA256SUMS)` and `gh attestation verify /tmp/jd-X.Y.Z/joedocs-X.Y.Z-<target>.tar.gz -R joelee/joedocs-mcp`;
   - the tap's `joedocs X.Y.Z` pull request, which merges by itself once the tap's tests pass.

### When the release fails

- **Before publishing** (a check, CI, a binary build, or the approval was rejected): nothing was published. Fix the problem on `main`, then move the tag: `git tag -d vX.Y.Z`, `git push origin :refs/tags/vX.Y.Z`, and tag and push again.
- **After publishing:** the version is on crates.io and cannot be replaced, so never rerun the Publish job: `cargo publish` fails for a version that already exists.
  - If the GitHub release failed (including its checksums or attestations), rerun only the GitHub release job: it takes the binaries from the run's artifacts. Creating the release by hand (below) leaves out the binaries.
  - If the Homebrew dispatch failed, the tap's daily check bumps the formula by itself; to bump it now, run `gh workflow run bump-formula.yml -R joelee/homebrew-oss -f formula=joedocs -f version=X.Y.Z`.
  - Fix anything else forward with a new patch version.

### Pre-releases

While a release is held back, for example by an advisory, its work can be shared as a pre-release: a version with a SemVer pre-release part in `Cargo.toml` (`X.Y.Z-dev`, `X.Y.Z-rc.1`), and a `## [X.Y.Z-dev] - YYYY-MM-DD` changelog section that says why it is not a release. A pre-release is not for production use, and goes to GitHub only, never crates.io or Homebrew.

Merge it to `main` and tag it as in Each release (`git tag -s vX.Y.Z-dev -m "JoeDocs X.Y.Z-dev"`, then `git tag -v` and push). The release workflow runs the checks, CI and the binary builds, and its advisory check reports any active advisory without failing. After approval in the `release` environment it creates a GitHub release marked **Pre-release**, never Latest, with the three archives, `SHA256SUMS` and the attestations. Its notes are the changelog section, then an "Active RustSec advisories" list with a link to each advisory, and "Not for production use." When the release goes ahead, set the version to `X.Y.Z` and rename and date the section.

### Releasing by hand

The same rule applies: no production release by hand while `cargo deny check advisories` fails. If the workflow cannot be used, publish from an up-to-date `main` with a crates.io token from `cargo login` (scope `publish-update`, limited to the `joedocs` crate, with an expiry date; never commit it):

```bash
cargo publish --locked
git tag -s vX.Y.Z -m "JoeDocs X.Y.Z"
git tag -v vX.Y.Z
git push origin vX.Y.Z
gh release create vX.Y.Z --title "JoeDocs X.Y.Z" \
  --notes-file <(awk -v heading="## [X.Y.Z]" 'index($0, heading) == 1 { found = 1; next } /^## \[/ { found = 0 } found' CHANGELOG.md)
gh workflow run bump-formula.yml -R joelee/homebrew-oss -f formula=joedocs -f version=X.Y.Z
```

Pushing the tag also starts the release workflow; reject its `release` deployment, since the crate is already published. A release made by hand has no binaries.
