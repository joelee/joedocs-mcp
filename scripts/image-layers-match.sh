#!/usr/bin/env bash
# Checks that a pushed image's linux/amd64 layers are those of a local
# image (`image-publish` in .github/workflows/release.yml, after the push).
#
# Usage: scripts/image-layers-match.sh LOCAL_IMAGE PUSHED_REF
#
# LOCAL_IMAGE is the amd64 image the job smoke-tested (loaded into Docker);
# PUSHED_REF is the pushed multi-platform image, by digest. It compares the
# layer diff IDs of LOCAL_IMAGE with those in the linux/amd64 image config
# of PUSHED_REF, and fails if they differ. Needs Docker with Buildx, and jq.
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 LOCAL_IMAGE PUSHED_REF" >&2
    exit 2
fi
local_image="$1"
pushed="$2"

tested="$(docker image inspect -f '{{json .RootFS.Layers}}' "$local_image" | jq -c .)"
published="$(docker buildx imagetools inspect "$pushed" --format '{{json .Image}}' \
    | jq -c '.["linux/amd64"].rootfs.diff_ids')"

if [ "$published" = null ] || [ -z "$published" ]; then
    echo "::error::$pushed has no linux/amd64 image config"
    exit 1
fi
if [ "$tested" != "$published" ]; then
    echo "::error::the pushed linux/amd64 layers of $pushed are not those of the smoke-tested $local_image (docs/developer-guide.md, Container images)"
    echo "tested:    $tested"
    echo "published: $published"
    exit 1
fi
echo "image-layers-match: the pushed linux/amd64 layers of $pushed are the smoke-tested ones ($(jq length <<<"$tested") layers)"
