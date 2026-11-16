#!/usr/bin/env bash
# Drives an open Board on a running `just scratch-server` (`just board-drive`):
# a scripted run of MCP writes, then a restore by a local joedocs command (a
# write from another process). The demo script of the Board (0.5.5).
#
# The run uses its own API key, board-drive, created on the first run and
# rotated on each later one; its token stays in this shell, never printed and
# never written down. SCRATCH_DIR points at another scratch store (default
# .local/scratch); DRIVE_PAUSE_MS sets the pause between steps (default 3000;
# 0 waits for a keystroke instead, which needs a terminal).
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
# One spelling for every value (00 is 0), in base 10 so 010 is not octal.
pause_ms=$((10#$pause_ms))

root="$(cd "$(dirname "$0")/.." && pwd -P)"
scratch="${SCRATCH_DIR:-$root/.local/scratch}"
# A relative store is taken from the directory the script runs in (the
# repository root under just), as scratch-server takes it.
case "$scratch" in /*) ;; *) scratch="$PWD/$scratch" ;; esac
while [ "$scratch" != / ] && [ "${scratch%/}" != "$scratch" ]; do scratch="${scratch%/}"; done
config="$scratch/config.toml"
bin="$root/target/debug/joedocs"
[ -f "$config" ] && [ -e "$scratch/seeded" ] ||
    fail "no seeded scratch store at $scratch; start one with: just scratch-server"
[ -x "$bin" ] || fail "no $bin; just scratch-server builds it"
# shellcheck source=scripts/scratch-lib.sh
. "$root/scripts/scratch-lib.sh"
listening "$port" ||
    fail "nothing listens on 127.0.0.1:$port; start just scratch-server (or pass --port)"

# With keys driving the run, the terminal goes back as it was however the
# script ends (Ctrl-C included).
trap keys_mode_off EXIT
trap 'exit 130' INT TERM
if [ "$pause_ms" = 0 ]; then
    keys_mode_on
fi

token="$(token_for board-drive demo:write sandbox:write)"
issue="$(cd "$root" && SCRATCH_MCP_URL="http://127.0.0.1:$port/mcp" SCRATCH_TOKEN="$token" \
    DRIVE_PAUSE_MS="$pause_ms" cargo run --locked --quiet --example board_drive)"
unset token
if [ "$pause_ms" = 0 ]; then
    key_wait "the restore by a local joedocs command"
else
    sleep "$(( (pause_ms + 999) / 1000 ))"
fi
jd archive restore "$issue" >/dev/null
echo "board-drive: $issue restored by a local joedocs command (another process)" >&2
