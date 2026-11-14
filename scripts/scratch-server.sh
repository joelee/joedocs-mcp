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

if $fresh && [ -d "$scratch" ]; then
    echo "scratch-server: removing $scratch"
    rm -rf "$scratch"
fi

(cd "$root" && cargo build --locked --quiet)

first_run=false
[ -d "$scratch/state" ] || first_run=true
mkdir -p "$scratch"
write_config

if $first_run; then
    jd init >/dev/null
    password="$(jd user create dev | tail -n 1)"
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
