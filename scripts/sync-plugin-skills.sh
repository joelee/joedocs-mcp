#!/usr/bin/env bash
# Regenerate plugins/joedocs/skills from docs/skills (the source of truth).
# A plugin's MCP tools are named mcp__plugin_<plugin>_<server>__*, so the
# standalone prefix mcp__joedocs__ is rewritten, and the sentence naming the
# server says it comes from the plugin.  Use --check to fail on drift.
set -euo pipefail
cd "$(dirname "$0")/.."

src=docs/skills
dest=plugins/joedocs/skills
mode=${1:-write}
case "$mode" in
  write | --check) ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

out=$dest
if [ "$mode" = "--check" ]; then
  out=$(mktemp -d)
  trap 'rm -rf "$out"' EXIT
fi
rm -rf "${out:?}"/joedocs-* 2>/dev/null || true
mkdir -p "$out"

for dir in "$src"/joedocs-*/; do
  name=$(basename "$dir")
  mkdir -p "$out/$name"
  for f in "$dir"*; do
    sed -e 's/^`joedocs` (tools `mcp__joedocs__\*`)/`joedocs` by the `joedocs` plugin (tools `mcp__joedocs__*`)/' \
      -e 's/mcp__joedocs__/mcp__plugin_joedocs_joedocs__/g' "$f" > "$out/$name/$(basename "$f")"
  done
done

if [ "$mode" = "--check" ]; then
  diff -r "$out" "$dest" >/dev/null || { echo "plugin skills are stale: run scripts/sync-plugin-skills.sh" >&2; exit 1; }
fi
