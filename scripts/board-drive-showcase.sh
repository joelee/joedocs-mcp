#!/usr/bin/env bash
# The Board showcase (`just board-drive-showcase`): a store of its own in
# .local/showcase (git-ignored) with three projects and four agents, for
# recording the README media.
#
#   reset    wipe .local/showcase and create it again: user dev (its one-time
#            password is printed), projects project-a, project-b and
#            project-c, the API keys laptop-claude, laptop-opencode,
#            mac-claude and mac-codex, and background documents in
#            project-b and project-c, written over MCP
#   serve    serve it with scripts/scratch-server.sh (its --https-port and
#            --no-tailscale options pass through)
#
# --port N picks the port (default 8770, beside just scratch-server's 8760).
# Tokens stay in this shell: never printed, never written down. Every
# joedocs command uses the showcase config and none of your JOEDOCS_*
# variables. Bash on Linux and macOS: no GNU-only options, no jq or python.
set -euo pipefail

usage() {
    echo "usage: just board-drive-showcase reset|serve [--port N] [serve: --https-port N] [serve: --no-tailscale]" >&2
    exit 64
}

fail() {
    echo "board-drive-showcase: $*" >&2
    exit 1
}

command=""
case "${1:-}" in
    reset | serve) command="$1"; shift ;;
    *) usage ;;
esac
port=8770
serve_args=()
while [ $# -gt 0 ]; do
    case "$1" in
        --port) [ $# -ge 2 ] || usage; port="$2"; shift ;;
        --https-port) [ "$command" = serve ] && [ $# -ge 2 ] || usage; serve_args+=("$1" "$2"); shift ;;
        --no-tailscale) [ "$command" = serve ] || usage; serve_args+=("$1") ;;
        *) usage ;;
    esac
    shift
done
case "$port" in '' | *[!0-9]*) fail "--port must be a number, not \"$port\"" ;; esac
[ "$port" != 8750 ] || fail "port 8750 is the default server.bind (a real server or the Docker stack); pick another --port"

root="$(cd "$(dirname "$0")/.." && pwd -P)"
scratch="$root/.local/showcase"
config="$scratch/config.toml"
bin="$root/target/debug/joedocs"
# shellcheck source=scripts/scratch-lib.sh
. "$root/scripts/scratch-lib.sh"

# Only .local/showcase inside this repository is ever written or removed.
if [ -L "$root/.local" ] || [ -L "$scratch" ]; then
    fail "$scratch is a symbolic link; refusing to use it"
fi
if [ -d "$scratch" ] && [ "$(cd "$scratch" && pwd -P)" != "$scratch" ]; then
    fail "$scratch resolves outside the repository; refusing to use it"
fi

if [ "$command" = serve ]; then
    exec env SCRATCH_DIR="$scratch" "$root/scripts/scratch-server.sh" --port "$port" \
        ${serve_args[@]+"${serve_args[@]}"}
fi

# reset. A server on the port may be serving this store, and wiping a store
# under a running server would spoil the next take.
! listening "$port" ||
    fail "something is listening on 127.0.0.1:$port (a showcase server?); stop it before a reset"

# Whatever ends the script stops the server started for writing the
# background; Ctrl-C is an interruption (130), and leaves no seeded marker.
trap stop_server EXIT
trap 'exit 130' INT TERM

(cd "$root" && cargo build --locked --quiet)
if [ -d "$scratch" ]; then
    echo "board-drive-showcase: removing $scratch"
    rm -rf "$scratch"
fi
mkdir -p "$scratch"
write_config
jd init >/dev/null
password="$(jd user create dev | tail -n 1)"
jd project add project-a --key PA --title "Project A" >/dev/null
jd project add project-b --key PB --title "Project B" >/dev/null
jd project add project-c --key PC --title "Project C" >/dev/null
grants=(project-a:write project-b:write project-c:write)
token_for laptop-claude "${grants[@]}" >/dev/null
token_for mac-claude "${grants[@]}" >/dev/null
opencode_token="$(token_for laptop-opencode "${grants[@]}")"
codex_token="$(token_for mac-codex "${grants[@]}")"

echo "board-drive-showcase: writing the background documents"
start_server "$scratch/seed-server.log"
(cd "$root" && SCRATCH_MCP_URL="http://127.0.0.1:$port/mcp" \
    SHOWCASE_TOKEN_LAPTOP_OPENCODE="$opencode_token" SHOWCASE_TOKEN_MAC_CODEX="$codex_token" \
    cargo run --locked --quiet --example board_drive_showcase -- seed)
unset opencode_token codex_token
stop_server
touch "$scratch/seeded"

echo
echo "Board showcase store ready (store: $scratch)"
echo "  User: dev   one-time password: $password"
echo "  The first sign-in sets a password and an authenticator (scan the QR code); each reset needs a new one."
echo "  Serve it with:  just board-drive-showcase serve$( [ "$port" = 8770 ] || echo " --port $port")"
echo
