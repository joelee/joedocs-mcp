# Shared by the scratch tooling in scripts/: sourced, never run. Bash on
# Linux and macOS: no GNU-only options, no jq or python.
#
# The caller sets `config` (the store's config.toml) and `bin` (the joedocs
# binary), and defines `fail MESSAGE`, which prints the message with the
# script's name and exits 1.

# joedocs with the store's config and a clean environment: an exported
# JOEDOCS__STORAGE__HOME, for example, would otherwise win over the file.
jd() {
    env -i PATH="$PATH" HOME="$HOME" TERM="${TERM:-dumb}" \
        JOEDOCS_CONFIG_FILE="$config" "$bin" "$@"
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
