#!/usr/bin/env bash
# A throwaway JoeDocs server from the working tree, for trying the web
# interface in other browsers and on phones (`just scratch-server`).
#
# The store lives in .local/scratch (git-ignored) and is kept between runs;
# `fresh` wipes it first. Every joedocs command runs with the scratch config
# and none of your JOEDOCS_* variables, so it never touches a real store.
# Bash on Linux and macOS: no GNU-only options, no jq or python.
set -euo pipefail

usage() {
    echo "usage: just scratch-server [fresh] [--port N] [--https-port N] [--no-tailscale]" >&2
    exit 64
}

fail() {
    echo "scratch-server: $*" >&2
    exit 1
}

fresh=false
port=8760
https_port=8443
use_tailscale=true
while [ $# -gt 0 ]; do
    case "$1" in
        fresh) fresh=true ;;
        --port) [ $# -ge 2 ] || usage; port="$2"; shift ;;
        --https-port) [ $# -ge 2 ] || usage; https_port="$2"; shift ;;
        --no-tailscale) use_tailscale=false ;;
        *) usage ;;
    esac
    shift
done
case "$port" in '' | *[!0-9]*) fail "--port must be a number, not \"$port\"" ;; esac
case "$https_port" in '' | *[!0-9]*) fail "--https-port must be a number, not \"$https_port\"" ;; esac

root="$(cd "$(dirname "$0")/.." && pwd -P)"
scratch="$root/.local/scratch"
config="$scratch/config.toml"
bin="$root/target/debug/joedocs"

# Only .local/scratch inside this repository is ever written or removed.
if [ -L "$root/.local" ] || [ -L "$scratch" ]; then
    fail "$scratch is a symbolic link; refusing to use it"
fi
if [ -d "$scratch" ] && [ "$(cd "$scratch" && pwd -P)" != "$scratch" ]; then
    fail "$scratch resolves outside the repository; refusing to use it"
fi

listening() {
    (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null
}
[ "$port" != 8750 ] || fail "port 8750 is the default server.bind (a real server or the Docker stack); pick another --port"
! listening "$port" || fail "something is already listening on 127.0.0.1:$port; pick another --port"

# joedocs with the scratch config and a clean environment: an exported
# JOEDOCS__STORAGE__HOME, for example, would otherwise win over the file.
jd() {
    env -i PATH="$PATH" HOME="$HOME" TERM="${TERM:-dumb}" \
        JOEDOCS_CONFIG_FILE="$config" "$bin" "$@"
}

write_config() {
    local hosts='"localhost", "127.0.0.1", "::1"' name
    for name in "$@"; do
        if [ -n "$name" ]; then hosts="$hosts, \"$name\""; fi
    done
    cat >"$config" <<TOML
# Written by scripts/scratch-server.sh on every run; changes here are lost.
[server]
bind = "127.0.0.1:$port"
allowed_hosts = [$hosts]

[storage]
home = "$scratch/vault"
state_dir = "$scratch/state"

[admin]
enabled = true
web_ui = true
TOML
}

# Stops the server started for seeding, whatever ends the script.
seed_pid=""
cleanup() {
    if [ -n "$seed_pid" ]; then
        kill "$seed_pid" 2>/dev/null || true
        wait "$seed_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

if $fresh && [ -d "$scratch" ]; then
    echo "scratch-server: removing $scratch"
    rm -rf "$scratch"
fi

(cd "$root" && cargo build --locked --quiet)

first_run=false
if [ ! -d "$scratch/state" ]; then
    first_run=true
elif [ ! -e "$scratch/seeded" ]; then
    fail "an earlier first run did not finish; start over with: just scratch-server fresh"
fi
mkdir -p "$scratch"
write_config

# The sample data goes in over MCP, as an agent would write it, with a key
# whose token stays in this shell: never printed, never written to disk.
seed() {
    local token
    jd project add demo --key DEMO --title "Demo" >/dev/null
    jd project add sandbox --key SBX --title "Sandbox" >/dev/null
    token="$(jd key create scratch-seed --grant demo:write --grant sandbox:write | tail -n 1)"
    # Not through jd: in a function, $! would be a subshell, and killing it
    # would leave the server running. env replaces itself with joedocs.
    env -i PATH="$PATH" HOME="$HOME" TERM="${TERM:-dumb}" \
        JOEDOCS_CONFIG_FILE="$config" "$bin" serve >"$scratch/seed-server.log" 2>&1 &
    seed_pid=$!
    local tries=0
    until curl -fsS "http://127.0.0.1:$port/health" >/dev/null 2>&1; do
        tries=$((tries + 1))
        [ "$tries" -le 30 ] || fail "the server did not start; see $scratch/seed-server.log"
        sleep 1
    done
    (cd "$root" && SCRATCH_MCP_URL="http://127.0.0.1:$port/mcp" SCRATCH_TOKEN="$token" \
        cargo run --locked --quiet --example scratch_seed)
    cleanup
    seed_pid=""
    touch "$scratch/seeded"
}

if $first_run; then
    jd init >/dev/null
    password="$(jd user create dev | tail -n 1)"
    echo "scratch-server: writing sample data (first run)"
    seed
fi

echo
echo "JoeDocs scratch server (store: $scratch)"
echo "  On this machine:  http://127.0.0.1:$port/admin/"
if $first_run; then
    echo "  User: dev   one-time password: $password"
    echo "  The first sign-in sets a password and an authenticator (scan the QR code)."
else
    echo "  User: dev (set up earlier). To start its sign-in again:"
    echo "    JOEDOCS_CONFIG_FILE=$config $bin user reset-password dev   (or reset-totp dev)"
fi
echo "  Stop with Ctrl-C; start over with: just scratch-server fresh"
echo

jd serve
