#!/usr/bin/env bash
# Regenerate plugins/joedocs/skills from docs/skills (the source of truth).
# A plugin's MCP tools are named mcp__plugin_<plugin>_<server>__*, so the
# standalone prefix mcp__joedocs__ is rewritten.  Use --check to fail on drift.
set -euo pipefail
cd "$(dirname "$0")/.."

src=docs/skills
dest=plugins/joedocs/skills
mode=${1:-write}

out=$dest
[ "$mode" = "--check" ] && out=$(mktemp -d)
rm -rf "${out:?}"/joedocs-* 2>/dev/null || true
mkdir -p "$out"

for dir in "$src"/joedocs-*/; do
  name=$(basename "$dir")
  mkdir -p "$out/$name"
  for f in "$dir"*; do
    sed 's/mcp__joedocs__/mcp__plugin_joedocs_joedocs__/g' "$f" > "$out/$name/$(basename "$f")"
  done
done

if [ "$mode" = "--check" ]; then
  diff -r "$out" "$dest" >/dev/null || { echo "plugin skills are stale: run scripts/sync-plugin-skills.sh" >&2; rm -rf "$out"; exit 1; }
  rm -rf "$out"
fi
