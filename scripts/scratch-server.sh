#!/usr/bin/env bash
# A throwaway JoeDocs server from the working tree, for trying the web
# interface in other browsers and on phones (`just scratch-server`).
#
# The store lives in .local/scratch (git-ignored) and is kept between runs;
# `fresh` wipes it first. SCRATCH_DIR names another store directly under
# .local/, which the script only serves: another script creates it. Every
# joedocs command runs with the store's config and none of your JOEDOCS_*
# variables, so it never touches a real store.
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
default_store="$root/.local/scratch"
scratch="$default_store"
# Another store: an absolute or relative path whose parent is .local in this
# repository; it is served as it is, never created or wiped here.
if [ -n "${SCRATCH_DIR:-}" ]; then
    case "$SCRATCH_DIR" in
        /*) scratch="$SCRATCH_DIR" ;;
        *) scratch="$PWD/$SCRATCH_DIR" ;;
    esac
    while [ "$scratch" != / ] && [ "${scratch%/}" != "$scratch" ]; do scratch="${scratch%/}"; done
    store_name="${scratch##*/}"
    store_parent="${scratch%/*}"
    case "$store_name" in '' | . | ..) fail "SCRATCH_DIR=$SCRATCH_DIR does not name a store directory" ;; esac
    if [ ! -d "$store_parent" ] || [ "$(cd "$store_parent" && pwd -P)" != "$root/.local" ]; then
        fail "SCRATCH_DIR=$SCRATCH_DIR is not a directory directly under $root/.local; refusing to use it"
    fi
    scratch="$root/.local/$store_name"
fi
config="$scratch/config.toml"
bin="$root/target/debug/joedocs"
# shellcheck source=scripts/scratch-lib.sh
. "$root/scripts/scratch-lib.sh"

# Only the store inside this repository's .local is ever written, and only
# .local/scratch is ever removed.
if [ -L "$root/.local" ] || [ -L "$scratch" ]; then
    fail "$scratch is a symbolic link; refusing to use it"
fi
if [ -d "$scratch" ] && [ "$(cd "$scratch" && pwd -P)" != "$scratch" ]; then
    fail "$scratch resolves outside the repository; refusing to use it"
fi
if [ "$scratch" != "$default_store" ]; then
    ! $fresh || fail "fresh starts over only in $default_store; $scratch is wiped by the script that creates it"
    [ -d "$scratch/state" ] && [ -e "$scratch/seeded" ] ||
        fail "no seeded store at $scratch; scratch-server serves it once another script has created it"
fi

[ "$port" != 8750 ] || fail "port 8750 is the default server.bind (a real server or the Docker stack); pick another --port"
! listening "$port" || fail "something is already listening on 127.0.0.1:$port; pick another --port"

# Whatever ends the script stops the server started for seeding and
# removes the one `tailscale serve` entry this run added.
served=false
cleanup() {
    stop_server
    if $served; then
        "$tailscale" serve --https="$https_port" off >/dev/null 2>&1 ||
            echo "scratch-server: could not remove the serve entry; run: tailscale serve --https=$https_port off" >&2
        served=false
    fi
}
trap cleanup EXIT
# Ctrl-C (or SIGTERM) while the server runs is how it is meant to stop, so
# that ends the script cleanly (status 0, which `just` does not report as a
# failure); during the build or the seeding it is an interruption (130).
serving=false
stopped() {
    if $serving; then exit 0; fi
    exit 130
}
trap stopped INT TERM

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
# Tailscale, unless turned off or not installed: the tailnet name joins
# allowed_hosts, and the HTTPS port must not already be served.
tailscale=""
ts_name=""
if $use_tailscale; then
    if command -v tailscale >/dev/null 2>&1; then
        tailscale="$(command -v tailscale)"
    elif [ -x /Applications/Tailscale.app/Contents/MacOS/Tailscale ]; then
        tailscale=/Applications/Tailscale.app/Contents/MacOS/Tailscale
    else
        echo "scratch-server: tailscale not found; serving on loopback only"
    fi
fi
if [ -n "$tailscale" ]; then
    ts_name="$("$tailscale" status --json 2>/dev/null |
        sed -n '/"Self": {/,/"DNSName"/p' |
        sed -n 's/.*"DNSName": *"\([^"]*\)".*/\1/p' | head -n 1)"
    ts_name="${ts_name%.}"
    [ -n "$ts_name" ] || fail "tailscale is not connected (no name in tailscale status); use --no-tailscale"
    if "$tailscale" serve status --json 2>/dev/null | grep -q "\"$https_port\""; then
        fail "tailscale already serves HTTPS port $https_port; pick another --https-port"
    fi
fi

# Extra names for a proxy of your own (comma-separated), checked so they
# cannot break the TOML they are written into.
extra_hosts=()
if [ -n "${SCRATCH_ALLOWED_HOSTS:-}" ]; then
    IFS=, read -r -a extra_hosts <<<"$SCRATCH_ALLOWED_HOSTS"
    for name in "${extra_hosts[@]}"; do
        case "$name" in
            '' | *[!A-Za-z0-9.:-]*) fail "SCRATCH_ALLOWED_HOSTS: \"$name\" is not a host name" ;;
        esac
    done
fi

mkdir -p "$scratch"
write_config "$ts_name" ${extra_hosts[@]+"${extra_hosts[@]}"}

# The sample data goes in over MCP, as an agent would write it, with a key
# whose token stays in this shell: never printed, never written to disk.
seed() {
    local token
    jd project add demo --key DEMO --title "Demo" >/dev/null
    jd project add sandbox --key SBX --title "Sandbox" >/dev/null
    token="$(token_for scratch-seed demo:write sandbox:write)"
    start_server "$scratch/seed-server.log"
    (cd "$root" && SCRATCH_MCP_URL="http://127.0.0.1:$port/mcp" SCRATCH_TOKEN="$token" \
        cargo run --locked --quiet --example scratch_seed)
    stop_server
    touch "$scratch/seeded"
}

if $first_run; then
    jd init >/dev/null
    password="$(jd user create dev | tail -n 1)"
    echo "scratch-server: writing sample data (first run)"
    seed
fi

if [ -n "$tailscale" ]; then
    if ! out="$("$tailscale" serve --bg --https="$https_port" "http://127.0.0.1:$port" 2>&1)"; then
        echo "$out" >&2
        fail "tailscale serve was refused. If it needs root, allow your user once with: sudo tailscale set --operator=\$USER (or use --no-tailscale)"
    fi
    served=true
fi

echo
echo "JoeDocs scratch server (store: $scratch)"
echo "  On this machine:  http://127.0.0.1:$port/admin/"
if $served; then
    echo "  Over HTTPS:       https://$ts_name:$https_port/admin/  (tailnet only; phones need the Tailscale app)"
fi
if $first_run; then
    echo "  User: dev   one-time password: $password"
    echo "  The first sign-in sets a password and an authenticator (scan the QR code)."
else
    echo "  User: dev (set up earlier). To start its sign-in again:"
    echo "    JOEDOCS_CONFIG_FILE=$config $bin user reset-password dev   (or reset-totp dev)"
fi
echo "  Stop with Ctrl-C; start over with: just scratch-server fresh"
echo

serving=true
jd serve
