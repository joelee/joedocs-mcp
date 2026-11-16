#!/usr/bin/env bash
# The Board showcase (`just board-drive-showcase`): a store of its own in
# .local/showcase (git-ignored) with three projects and four agents, for
# recording the README media.
#
#   reset    wipe .local/showcase and create it again: user dev, carried
#            over with its password and authenticator once it has set one
#            up (else a new dev and its one-time password), projects
#            project-a, project-b and project-c, the API keys
#            laptop-claude, laptop-opencode, mac-claude and mac-codex, and
#            background documents in project-b and project-c, written over
#            MCP; refused while a serve or another reset of the store runs
#            (they hold its lock, .local/showcase.lock)
#   serve   serve it with scripts/scratch-server.sh on HTTPS port 8444
#            (--https-port N changes it; --no-tailscale passes through)
#   (none)   play the feature flow on the served store: an idea becomes a
#            plan, which is built from STEP-01 to STEP-05 and signed off,
#            while another agent's plan is drafted and rejected, each beat
#            written over MCP by the key named for it; DRIVE_PAUSE_MS sets
#            the pause between beats (default 3000; 0 waits for a
#            keystroke, which needs a terminal). Once per reset.
#
# --port N picks the port (default 8770, beside just scratch-server's 8760).
# Tokens stay in this shell: never printed, never written down. Every
# joedocs command uses the showcase config and none of your JOEDOCS_*
# variables. Bash on Linux and macOS: no GNU-only options, no jq or python.
set -euo pipefail

usage() {
    echo "usage: just board-drive-showcase [reset | serve] [--port N] [serve: --https-port N] [serve: --no-tailscale]" >&2
    exit 64
}

fail() {
    echo "board-drive-showcase: $*" >&2
    exit 1
}

command=play
case "${1:-}" in
    reset | serve) command="$1"; shift ;;
esac
port=8770
# 8444, so the showcase can be served over Tailscale beside just
# scratch-server, which takes 8443.
https_port=8444
serve_args=()
while [ $# -gt 0 ]; do
    case "$1" in
        --port) [ $# -ge 2 ] || usage; port="$2"; shift ;;
        --https-port) [ "$command" = serve ] && [ $# -ge 2 ] || usage; https_port="$2"; shift ;;
        --no-tailscale) [ "$command" = serve ] || usage; serve_args+=("$1") ;;
        *) usage ;;
    esac
    shift
done
case "$port" in '' | *[!0-9]*) fail "--port must be a number, not \"$port\"" ;; esac
[ "$port" != 8750 ] || fail "port 8750 is the default server.bind (a real server or the Docker stack); pick another --port"
pause_ms="${DRIVE_PAUSE_MS:-3000}"
case "$pause_ms" in '' | *[!0-9]*) fail "DRIVE_PAUSE_MS must be a number of milliseconds" ;; esac
# One spelling for every value (00 is 0), in base 10 so 010 is not octal.
pause_ms=$((10#$pause_ms))

root="$(cd "$(dirname "$0")/.." && pwd -P)"
scratch="$root/.local/showcase"
config="$scratch/config.toml"
bin="$root/target/debug/joedocs"
# shellcheck source=scripts/scratch-lib.sh
. "$root/scripts/scratch-lib.sh"

# The store a reset is replacing, kept until its user dev is copied over.
old="$root/.local/showcase.old"

# Only .local/showcase and .local/showcase.old inside this repository are
# ever written or removed.
for dir in "$scratch" "$old"; do
    if [ -L "$root/.local" ] || [ -L "$dir" ]; then
        fail "$dir is a symbolic link; refusing to use it"
    fi
    if [ -d "$dir" ] && [ "$(cd "$dir" && pwd -P)" != "$dir" ]; then
        fail "$dir resolves outside the repository; refusing to use it"
    fi
done

if [ "$command" = serve ]; then
    exec env SCRATCH_DIR="$scratch" "$root/scripts/scratch-server.sh" --port "$port" \
        --https-port "$https_port" ${serve_args[@]+"${serve_args[@]}"}
fi

if [ "$command" = play ]; then
    [ -f "$config" ] && [ -e "$scratch/seeded" ] ||
        fail "no showcase store at $scratch; create it with: just board-drive-showcase reset"
    [ -x "$bin" ] || fail "no $bin; just board-drive-showcase reset builds it"
    listening "$port" ||
        fail "nothing listens on 127.0.0.1:$port; start: just board-drive-showcase serve (or pass --port)"
    # By the count in the JSON reply, not the wording of the table.
    listed="$(jd list -p project-a --json)"
    printf '%s\n' "$listed" | grep -Eq '^[[:space:]]*"total":[[:space:]]*0[[:space:]]*,?[[:space:]]*$' ||
        fail "project-a already holds the flow; run it again after: just board-drive-showcase reset"

    # With keys driving the run, the terminal goes back as it was however
    # the script ends (Ctrl-C included).
    trap keys_mode_off EXIT
    trap 'exit 130' INT TERM
    if [ "$pause_ms" = 0 ]; then
        keys_mode_on
    fi

    grants=(project-a:write project-b:write project-c:write)
    laptop_claude="$(token_for laptop-claude "${grants[@]}")"
    laptop_opencode="$(token_for laptop-opencode "${grants[@]}")"
    mac_claude="$(token_for mac-claude "${grants[@]}")"
    mac_codex="$(token_for mac-codex "${grants[@]}")"
    logs="$scratch/agents"
    mkdir -p "$logs"

    statuses="status=draft&status=review&status=approved&status=parked&status=done&status=rejected"
    echo "The Board, a row per project, closed cards kept in view:"
    echo "  http://127.0.0.1:$port/admin/board?$statuses&group=project"
    echo "Each agent's log, for a terminal of its own:"
    for key in laptop-claude laptop-opencode mac-claude mac-codex; do
        echo "  tail -F $logs/$key.log"
    done
    echo
    # The logs stamp the local time of day: the example has no time zone
    # database, so it gets this machine's offset from UTC (+1100, -0700).
    (cd "$root" && SCRATCH_MCP_URL="http://127.0.0.1:$port/mcp" SHOWCASE_LOG_DIR="$logs" \
        SHOWCASE_UTC_OFFSET="$(date +%z)" \
        SHOWCASE_TOKEN_LAPTOP_CLAUDE="$laptop_claude" SHOWCASE_TOKEN_LAPTOP_OPENCODE="$laptop_opencode" \
        SHOWCASE_TOKEN_MAC_CLAUDE="$mac_claude" SHOWCASE_TOKEN_MAC_CODEX="$mac_codex" \
        DRIVE_PAUSE_MS="$pause_ms" cargo run --locked --quiet --example board_drive_showcase -- play)
    unset laptop_claude laptop_opencode mac_claude mac_codex
    echo "board-drive-showcase: done; run it again after: just board-drive-showcase reset" >&2
    exit 0
fi

# reset. Wiping a store under a running server would spoil the next take.
#
# Whatever ends the script stops the server started for writing the
# background and releases the store's lock; Ctrl-C (or SIGTERM, or SIGHUP)
# is an interruption (130), and leaves no seeded marker. The old store is
# never removed on the way out: only once the new one is complete.
trap 'stop_server; store_unlock' EXIT
trap 'exit 130' INT TERM HUP
# One command at a time per store: a serve holds the lock while it runs, so
# taking it here, before anything is checked or built, and keeping it to the
# end means no serve or other reset of the store can start meanwhile.
store_lock
# A joedocs serve of the store started by hand takes no lock: something
# listening on the port, or on the one serve last wrote into its
# config.toml, may be one.
! listening "$port" ||
    fail "something is listening on 127.0.0.1:$port (a showcase server?); stop it before a reset"
saved="$(saved_port)"
[ -z "$saved" ] || [ "$saved" = "$port" ] || ! listening "$saved" ||
    fail "the showcase store is being served on 127.0.0.1:$saved; stop that server before a reset"

(cd "$root" && cargo build --locked --quiet)
# The old store goes aside. One already aside was left by an interrupted
# reset: unless the store beside it was completed (its user already
# copied), it is still the one to copy dev from.
if [ -d "$old" ] && [ -e "$scratch/seeded" ]; then
    rm -rf "$old"
fi
if [ -d "$old" ]; then
    echo "board-drive-showcase: an earlier reset was interrupted; taking dev from $old"
    rm -rf "$scratch"
elif [ -d "$scratch" ]; then
    echo "board-drive-showcase: moving $scratch aside"
    mv "$scratch" "$old"
fi
mkdir -p "$scratch"
write_config
jd init >/dev/null
# dev comes over with its password and authenticator when it has set one
# up (copy-user says kept); otherwise a new dev gets a one-time password.
kept=""
if [ -f "$old/state/joedocs.db" ]; then
    kept="$(cd "$root" && cargo run --locked --quiet --example board_drive_showcase -- \
        copy-user "$old/state/joedocs.db" "$scratch/state/joedocs.db")"
fi
password=""
if [ "$kept" != kept ]; then
    [ -z "$kept" ] || echo "board-drive-showcase: dev not kept ($kept); a new dev gets a one-time password"
    password="$(jd user create dev | tail -n 1)"
fi
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
rm -rf "$old"

echo
echo "Board showcase store ready (store: $scratch)"
if [ -z "$password" ]; then
    echo "  User: dev kept (same password and authenticator; sign in again)"
else
    echo "  User: dev   one-time password: $password"
    echo "  The first sign-in sets a password and an authenticator (scan the QR code); later resets keep them."
fi
echo "  Serve it with:  just board-drive-showcase serve$( [ "$port" = 8770 ] || echo " --port $port")"
echo "  Then play it, in another terminal:  just board-drive-showcase$( [ "$port" = 8770 ] || echo " --port $port")"
echo
