#!/bin/sh
# The JoeDocs image's entrypoint (docs/docker.md).
#
# With no arguments, or `serve ...`, it prepares the store and starts the
# server:
#
# 1. `joedocs init`, which is idempotent: the first start creates the vault,
#    database and history, later starts only check them.
# 2. On a store with no users (`joedocs user list` prints `No users.`), it
#    creates the user named by JOEDOCS_INITIAL_ADMIN (default `admin`; set it
#    empty to create none) and prints how to sign in, once. A store that has
#    users, removed ones included, is left as it is.
# 3. `exec joedocs serve ...`.
#
# Any other arguments run as a command instead of the server, for example
# `docker run --rm IMAGE joedocs --version`.
set -eu

if [ "$#" -gt 0 ] && [ "$1" != serve ]; then
    exec "$@"
fi
[ "$#" -gt 0 ] && shift

joedocs init

name="${JOEDOCS_INITIAL_ADMIN-admin}"
if [ -n "$name" ] && [ "$(joedocs user list)" = "No users." ]; then
    # The one-time password is alone on the last line.
    password="$(joedocs user create "$name" | tail -n 1)"
    version="$(joedocs --version | cut -d ' ' -f 2)"
    cat <<EOF

========================================================================
 JoeDocs is ready. Sign in to the web interface:

   URL:       http://localhost:8750/admin/
              (if you published another host port, use it instead)
   User:      $name
   Password:  $password
              (a one-time password: it works once, within 72 hours)

 At the first sign-in you choose your own password, then scan a QR code
 with an authenticator app (TOTP). Lost this password? Run:
   docker exec <container> joedocs user reset-password $name

 WARNING: without a volume of your own (-v or compose), the data lives
 in an anonymous Docker volume. It is kept when the container restarts,
 but deleted by \`docker rm -v\` or \`docker run --rm\`, and hard to find.
 To keep it in named volumes, start the container like this instead:
   docker run -d --name joedocs -p 127.0.0.1:8750:8750 \\
     -v joedocs-vault:/data/vault -v joedocs-state:/data/state \\
     joeworks/joedocs:$version

 Customising the container: docs/docker.md, at
 https://github.com/joelee/joedocs-mcp/blob/main/docs/docker.md
========================================================================

EOF
fi

exec joedocs serve "$@"
