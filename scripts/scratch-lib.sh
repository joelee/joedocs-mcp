# Shared by the scratch tooling in scripts/: sourced, never run. Bash on
# Linux and macOS: no GNU-only options, no jq or python.
#
# The caller sets `scratch` (the store directory), `config` (its
# config.toml) and `bin` (the joedocs binary) before sourcing it, and
# defines `fail MESSAGE`, which prints the message with the script's name
# and exits 1.

# joedocs reads a .env file in its working directory, whose JOEDOCS__*
# values win over the config file, so every joedocs here runs in the store
# directory, where no .env may be.
[ ! -e "$scratch/.env" ] ||
    fail "$scratch/.env exists; joedocs would read it and leave the store's config.toml; remove it"

# joedocs with the store's config, a clean environment and the store
# directory as its working directory: an exported JOEDOCS__STORAGE__HOME,
# or one in a .env where the script was started, would otherwise win over
# the file.
jd() {
    (cd "$scratch" && exec env -i PATH="$PATH" HOME="$HOME" TERM="${TERM:-dumb}" \
        JOEDOCS_CONFIG_FILE="$config" "$bin" "$@")
}

# Whether something listens on 127.0.0.1:PORT.
listening() {
    (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null
}

# token_for KEY GRANT...: a token for the API key KEY on standard output,
# for the caller to capture in a variable (never print it or write it down).
# An existing key is rotated, keeping its grants; a new one is created with
# GRANTs such as demo:write.
token_for() {
    local name="$1" grant args=()
    shift
    if jd key list | awk 'NR > 1 { print $1 }' | grep -qx "$name"; then
        jd key rotate "$name" | tail -n 1
    else
        for grant in "$@"; do args+=(--grant "$grant"); done
        jd key create "$name" ${args[@]+"${args[@]}"} | tail -n 1
    fi
}

# write_config [HOST...]: the store's config.toml, for the server on
# 127.0.0.1:$port with its vault and state in $scratch; each HOST joins
# allowed_hosts. Rewritten on every run.
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

# The port of server.bind in $config, in the form write_config writes;
# empty when there is no config or no such line.
saved_port() {
    [ -f "$config" ] || return 0
    sed -n 's/^bind = "127\.0\.0\.1:\([0-9][0-9]*\)"$/\1/p' "$config" | head -n 1
}

# A server started only to write sample data over MCP, and stopped after.
server_pid=""

# start_server LOG: serves the store in the background, logging to LOG,
# and waits until 127.0.0.1:$port answers /health (at most 30 s). The
# caller's EXIT trap calls stop_server.
start_server() {
    local log="$1" tries=0
    # In the store directory, as jd. The subshell replaces itself with env,
    # and env with joedocs, so $! is the server and killing it stops it.
    (cd "$scratch" && exec env -i PATH="$PATH" HOME="$HOME" TERM="${TERM:-dumb}" \
        JOEDOCS_CONFIG_FILE="$config" "$bin" serve) >"$log" 2>&1 &
    server_pid=$!
    until curl -fsS "http://127.0.0.1:$port/health" >/dev/null 2>&1; do
        tries=$((tries + 1))
        [ "$tries" -le 30 ] || fail "the server did not start; see $log"
        sleep 1
    done
}

stop_server() {
    if [ -n "$server_pid" ]; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
        server_pid=""
    fi
}

# The terminal settings keys_mode_on replaced, for keys_mode_off.
keys_saved=""

# DRIVE_PAUSE_MS=0: each pause waits for one keystroke, read by the drive
# as one byte. Puts the terminal in non-canonical mode without echo (no
# Enter needed), and fails when standard input is not a terminal. Call
# before anything is written, with keys_mode_off in an EXIT trap.
keys_mode_on() {
    [ -t 0 ] || fail "DRIVE_PAUSE_MS=0 waits for keystrokes, so standard input must be a terminal"
    keys_saved="$(stty -g)"
    stty -icanon -echo min 1
}

# Puts the terminal back as keys_mode_on found it.
keys_mode_off() {
    if [ -n "$keys_saved" ]; then
        stty "$keys_saved" 2>/dev/null || true
        keys_saved=""
    fi
}

# One keystroke, after a prompt naming the next step (keys mode on).
key_wait() {
    local key
    echo "press a key: next $1" >&2
    IFS= read -r -n 1 key || fail "standard input ended while waiting for a key"
}
