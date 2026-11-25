#!/usr/bin/env bash
# The image's first run, without Compose (`just docker-first-run-test`).
#
# For two images, the source build (the Dockerfile's default `local`
# target) and the `release` target built from a GitHub release's musl
# archive, it checks:
#
# - a fresh `docker run` with no volumes of its own prints the sign-in
#   banner once (user `admin`, a one-time password), becomes healthy, and
#   serves /admin/login;
# - a restart prints no second banner and keeps the one user;
# - JOEDOCS_INITIAL_ADMIN= (empty) creates no user and prints no banner,
#   and JOEDOCS_INITIAL_ADMIN=alice creates `alice`;
# - a failed first-user setup stops the container with no banner: a bad
#   name (JOEDOCS_INITIAL_ADMIN=Alice), and a failing `joedocs user list`
#   (a fake `joedocs` earlier on the image's PATH);
# - other arguments run as a command: `joedocs --version`.
#
# Usage: scripts/docker-first-run-test.sh [RELEASE], where RELEASE is the
# GitHub release whose archive the `release` target uses (default below).
# Needs Docker, curl and shasum; downloads the archive and its SHA256SUMS from
# GitHub. Everything it creates is labelled and removed on exit.
set -euo pipefail

release="${1:-0.5.6-rc.2}"
repo="joelee/joedocs-mcp"
run="jd-first-run-$$"
work="$(mktemp -d)"
local_image="joedocs-first-run-test:local-$$"
release_image="joedocs-first-run-test:release-$$"

cleanup() {
    docker ps -aq --filter "label=$run" | xargs -r docker rm -f -v >/dev/null
    docker image rm -f "$local_image" "$release_image" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# Waits until container $1 is healthy, or fails with its logs.
wait_healthy() {
    local status=""
    for _ in $(seq 60); do
        status="$(docker inspect -f '{{.State.Health.Status}}' "$1")"
        [ "$status" = healthy ] && return 0
        sleep 1
    done
    docker logs "$1" >&2 || true
    fail "$1 is $status after 60 s, not healthy"
}

# Waits until container $1 has exited, or fails with its logs.
wait_exited() {
    local status=""
    for _ in $(seq 60); do
        status="$(docker inspect -f '{{.State.Status}}' "$1")"
        [ "$status" = exited ] && return 0
        sleep 1
    done
    docker logs "$1" >&2 || true
    fail "$1 is $status after 60 s, not exited"
}

# Checks container $1 stopped with a non-zero exit and printed no banner.
stopped_without_banner() {
    wait_exited "$1"
    [ "$(docker inspect -f '{{.State.ExitCode}}' "$1")" != 0 ] || fail "$1 exited 0"
    [ "$(banners "$1")" = 0 ] || fail "$1 printed a banner"
}

banners() {
    docker logs "$1" 2>/dev/null | grep -c 'JoeDocs is ready' || true
}

# The user names `joedocs user list` prints, one per line.
users() {
    docker exec "$1" joedocs user list | awk 'NR > 1 && $1 != "No" { print $1 }'
}

check_image() {
    local image="$1" version="$2" name="$run-$3"
    echo "== $image"

    local printed
    printed="$(docker run --rm --label "$run" "$image" joedocs --version)"
    [ "$printed" = "joedocs $version" ] || fail "joedocs --version printed '$printed', not 'joedocs $version'"
    echo "ok: a passed-through command runs: $printed"

    docker run -d --label "$run" --name "$name" -p 127.0.0.1::8750 "$image" >/dev/null
    wait_healthy "$name"
    local logs
    logs="$(docker logs "$name" 2>/dev/null)"
    [ "$(banners "$name")" = 1 ] || fail "expected one banner on the first start:"$'\n'"$logs"
    grep -q '^   User:      admin$' <<<"$logs" || fail "the banner names no user admin"
    grep -Eq '^   Password:  [^ ]{8,}$' <<<"$logs" || fail "the banner shows no one-time password"
    grep -q '72 hours' <<<"$logs" || fail "the banner does not give the 72-hour limit"
    grep -q 'anonymous Docker volume' <<<"$logs" || fail "the banner has no data warning"
    grep -q 'docs/docker.md' <<<"$logs" || fail "the banner does not name docs/docker.md"
    [ "$(users "$name")" = admin ] || fail "expected the one user admin"
    local port code
    port="$(docker port "$name" 8750/tcp | head -n 1 | sed 's/.*://')"
    code="$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/admin/login")"
    [ "$code" = 200 ] || fail "/admin/login answered $code, not 200"
    echo "ok: the first start prints one banner for admin, is healthy, and serves /admin/login"

    docker restart "$name" >/dev/null
    wait_healthy "$name"
    [ "$(banners "$name")" = 1 ] || fail "a restart printed another banner"
    [ "$(users "$name")" = admin ] || fail "a restart changed the users"
    echo "ok: a restart prints no banner and keeps the one user"

    docker run -d --label "$run" --name "$name-off" -e JOEDOCS_INITIAL_ADMIN= "$image" >/dev/null
    wait_healthy "$name-off"
    [ "$(banners "$name-off")" = 0 ] || fail "JOEDOCS_INITIAL_ADMIN= printed a banner"
    [ "$(docker exec "$name-off" joedocs user list)" = "No users." ] || fail "JOEDOCS_INITIAL_ADMIN= created a user"
    echo "ok: JOEDOCS_INITIAL_ADMIN= creates no user and prints no banner"

    docker run -d --label "$run" --name "$name-alice" -e JOEDOCS_INITIAL_ADMIN=alice "$image" >/dev/null
    wait_healthy "$name-alice"
    [ "$(users "$name-alice")" = alice ] || fail "JOEDOCS_INITIAL_ADMIN=alice did not create alice alone"
    # Read the logs whole first: `docker logs | grep -q` fails under
    # pipefail when grep exits before docker logs finishes (SIGPIPE, 141).
    logs="$(docker logs "$name-alice" 2>/dev/null)"
    grep -q '^   User:      alice$' <<<"$logs" || fail "the banner does not name alice"
    echo "ok: JOEDOCS_INITIAL_ADMIN=alice creates alice"

    docker run -d --label "$run" --name "$name-bad" -e JOEDOCS_INITIAL_ADMIN=Alice "$image" >/dev/null
    stopped_without_banner "$name-bad"
    logs="$(docker logs "$name-bad" 2>&1)"
    grep -q 'must be lower-case' <<<"$logs" || fail "the logs do not give the name error:"$'\n'"$logs"
    echo "ok: JOEDOCS_INITIAL_ADMIN=Alice stops the container with the name error and no banner"

    docker run -d --label "$run" --name "$name-nolist" \
        -v "$work/fake-joedocs:/usr/local/sbin/joedocs:ro" "$image" >/dev/null
    stopped_without_banner "$name-nolist"
    logs="$(docker logs "$name-nolist" 2>&1)"
    grep -q 'fake: user list failed' <<<"$logs" || fail "the fake joedocs did not run:"$'\n'"$logs"
    echo "ok: a failing joedocs user list stops the container with no banner"

    docker rm -f -v "$name" "$name-off" "$name-alice" "$name-bad" "$name-nolist" >/dev/null
}

# A `joedocs` that fails `user list` and runs every other command. Mounted
# at /usr/local/sbin/joedocs, it comes before /usr/local/bin on the
# image's PATH.
cat >"$work/fake-joedocs" <<'EOF'
#!/bin/sh
if [ "${1-}" = user ] && [ "${2-}" = list ]; then
    echo "fake: user list failed" >&2
    exit 1
fi
exec /usr/local/bin/joedocs "$@"
EOF
chmod 0755 "$work/fake-joedocs"

# The source build, as `docker build .` gives it.
docker build -q -t "$local_image" . >/dev/null
check_image "$local_image" "$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)" local

# The release target, from the release's archive for this machine's
# architecture, checked against its SHA256SUMS. Its build context holds
# only what the target copies.
arch="$(docker version -f '{{.Server.Arch}}')"
case "$arch" in
    amd64) target=x86_64-unknown-linux-musl ;;
    arm64) target=aarch64-unknown-linux-musl ;;
    *) fail "no release archive for $arch" ;;
esac
archive="joedocs-$release-$target.tar.gz"
base="https://github.com/$repo/releases/download/v$release"
curl -fsSL -o "$work/$archive" "$base/$archive"
curl -fsSL -o "$work/SHA256SUMS" "$base/SHA256SUMS"
(cd "$work" && grep " $archive\$" SHA256SUMS | shasum -a 256 --check --quiet) || fail "$archive does not match SHA256SUMS"
mkdir -p "$work/context/dist/linux/$arch" "$work/context/docker"
tar -xzf "$work/$archive" -C "$work/context/dist/linux/$arch" joedocs
cp docker/entrypoint.sh "$work/context/docker/"
docker build -q --target release -f Dockerfile -t "$release_image" "$work/context" >/dev/null
check_image "$release_image" "$release" release

echo "docker-first-run-test: all checks passed"
