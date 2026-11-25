#!/usr/bin/env bash
# The release workflow's smoke test of a built image (`image` and
# `image-publish` in .github/workflows/release.yml).
#
# Usage: scripts/image-smoke-test.sh IMAGE VERSION
#
# Checks that the image's `joedocs --version` prints VERSION, then starts it
# with no volumes of its own and checks that it becomes healthy within 60 s
# and prints the first-run banner for `admin`. Everything it starts is
# removed on exit, with its anonymous volumes. Needs Docker.
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 IMAGE VERSION" >&2
    exit 2
fi
image="$1"
version="$2"
name="joedocs-smoke-$$"

cleanup() {
    docker rm -f -v "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

fail() {
    echo "::error::$*"
    docker logs "$name" 2>&1 || true
    exit 1
}

printed="$(docker run --rm "$image" joedocs --version)"
[ "$printed" = "joedocs $version" ] || fail "the image's joedocs --version printed '$printed', not 'joedocs $version'"

docker run -d --name "$name" "$image" >/dev/null
status=""
for _ in $(seq 60); do
    status="$(docker inspect -f '{{.State.Health.Status}}' "$name")"
    [ "$status" = healthy ] && break
    sleep 1
done
[ "$status" = healthy ] || fail "the container is $status after 60 s, not healthy"

# Read the logs whole: `docker logs | grep -q` fails under pipefail when
# grep exits before docker logs finishes (SIGPIPE).
logs="$(docker logs "$name" 2>/dev/null)"
grep -q 'JoeDocs is ready' <<<"$logs" || fail "no first-run banner"
grep -q '^   User:      admin$' <<<"$logs" || fail "the banner names no user admin"

echo "image-smoke-test: $image ($printed) is healthy and printed the banner for admin"
