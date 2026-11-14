#!/usr/bin/env bash
# Drives an open Board on a running `just scratch-server` (`just board-drive`):
# a scripted run of MCP writes, then a restore by a local joedocs command (a
# write from another process). The demo script of the Board (0.5.5).
#
# The run uses its own API key, board-drive, created on the first run and
# rotated on each later one; its token stays in this shell, never printed and
# never written down. SCRATCH_DIR points at another scratch store (default
# .local/scratch); DRIVE_PAUSE_MS sets the pause between steps (default 3000).
# Bash on Linux and macOS: no GNU-only options, no jq or python.
set -euo pipefail

usage() {
    echo "usage: just board-drive [--port N]" >&2
    exit 64
}

fail() {
    echo "board-drive: $*" >&2
    exit 1
}

port=8760
while [ $# -gt 0 ]; do
    case "$1" in
        --port) [ $# -ge 2 ] || usage; port="$2"; shift ;;
        *) usage ;;
    esac
    shift
done
case "$port" in '' | *[!0-9]*) fail "--port must be a number, not \"$port\"" ;; esac
pause_ms="${DRIVE_PAUSE_MS:-3000}"
case "$pause_ms" in '' | *[!0-9]*) fail "DRIVE_PAUSE_MS must be a number of milliseconds" ;; esac

root="$(cd "$(dirname "$0")/.." && pwd -P)"
scratch="${SCRATCH_DIR:-$root/.local/scratch}"
config="$scratch/config.toml"
bin="$root/target/debug/joedocs"
[ -f "$config" ] && [ -e "$scratch/seeded" ] ||
    fail "no seeded scratch store at $scratch; start one with: just scratch-server"
[ -x "$bin" ] || fail "no $bin; just scratch-server builds it"
(exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null ||
    fail "nothing listens on 127.0.0.1:$port; start just scratch-server (or pass --port)"

# joedocs with the scratch config and a clean environment, as
# scripts/scratch-server.sh runs it.
jd() {
    env -i PATH="$PATH" HOME="$HOME" TERM="${TERM:-dumb}" \
        JOEDOCS_CONFIG_FILE="$config" "$bin" "$@"
}

if jd key list | awk 'NR > 1 { print $1 }' | grep -qx board-drive; then
    token="$(jd key rotate board-drive | tail -n 1)"
else
    token="$(jd key create board-drive --grant demo:write --grant sandbox:write | tail -n 1)"
fi
issue="$(cd "$root" && SCRATCH_MCP_URL="http://127.0.0.1:$port/mcp" SCRATCH_TOKEN="$token" \
    DRIVE_PAUSE_MS="$pause_ms" cargo run --locked --quiet --example board_drive)"
unset token
sleep "$(( (pause_ms + 999) / 1000 ))"
jd archive restore "$issue" >/dev/null
echo "board-drive: $issue restored by a local joedocs command (another process)" >&2
