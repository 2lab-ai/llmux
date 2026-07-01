#!/usr/bin/env bash
# Record BOTH demo GIFs (CLI + islands app) and attach them to a GitHub release.
# Meant to run at deploy time, on the developer's Mac (see record-cli.sh /
# record-islands.sh for their local prerequisites — real Claude Code binary, and
# a one-time Screen Recording grant for the app capture).
#
# Each half is independent: if one fails (e.g. no Screen Recording permission yet)
# the other still records and uploads.
#
# Usage:  demo/record-all.sh [vX.Y.Z]
#   tag defaults to the latest git tag. Assets are uploaded with stable names so
#   README can reference them via releases/latest/download/<name>.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

TAG="${1:-$(git describe --tags --abbrev=0 2>/dev/null || true)}"
REPO_SLUG="${LLMUX_REPO_SLUG:-2lab-ai/llmux}"
cli_ok=0; app_ok=0

echo "==> recording CLI demo"
if bash demo/record-cli.sh; then cli_ok=1; else echo "   (CLI demo skipped/failed — see above)"; fi

echo "==> recording islands app demo"
if bash demo/record-islands.sh; then app_ok=1; else echo "   (islands demo skipped/failed — see above)"; fi

upload() { # <file>
  local f="$1"
  [ -f "$f" ] || return 0
  if [ -z "$TAG" ]; then echo "   no tag — skipping upload of $f"; return 0; fi
  if command -v gh >/dev/null; then
    echo "==> uploading $(basename "$f") to $REPO_SLUG@$TAG"
    gh release upload "$TAG" "$f" --repo "$REPO_SLUG" --clobber \
      && echo "   https://github.com/$REPO_SLUG/releases/latest/download/$(basename "$f")"
  else
    echo "   gh not found — commit $f or upload manually"
  fi
}

[ "$cli_ok" = 1 ] && upload screenshots/llmux-demo.gif
[ "$app_ok" = 1 ] && upload screenshots/llmux-islands-demo.gif

echo
echo "==> summary: CLI=$([ $cli_ok = 1 ] && echo ok || echo FAIL)  islands=$([ $app_ok = 1 ] && echo ok || echo FAIL)  tag=${TAG:-none}"
# Non-zero only if BOTH failed, so a deploy hook surfaces total failure but
# tolerates the app half being gated on a one-time Screen Recording grant.
[ $cli_ok = 1 ] || [ $app_ok = 1 ]
