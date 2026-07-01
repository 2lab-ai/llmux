#!/usr/bin/env bash
# Record the `llmux server` CLI/TUI demo GIF from demo/llmux.tape (charmbracelet
# VHS). Runs on the developer's Mac at deploy time — it needs a real config (real
# accounts) because the tape drives a REAL Claude Code call through the proxy.
# Emails are masked by LLMUX_DEMO_MODE=1, so the output GIF is public-safe.
#
# Output: screenshots/llmux-demo.gif
# Usage:  demo/record-cli.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DEMO_CONFIG="${LLMUX_DEMO_CONFIG:-/tmp/llmux-demo.json}"
SRC_CONFIG="${LLMUX_CONFIG:-$HOME/.config/llmux.json}"
DEMO_PORT="${LLMUX_DEMO_PORT:-3457}"

fail() { echo "record-cli: $*" >&2; exit 1; }

command -v vhs  >/dev/null || fail "vhs not found — brew install vhs"
command -v tmux >/dev/null || fail "tmux not found — brew install tmux"
command -v llmux >/dev/null || fail "llmux not found on PATH"

# The tape drives a REAL `claude` (Claude Code) binary. The user commonly aliases
# `claude` to `llmux run`; aliases don't reach VHS's non-interactive bash, so we
# must locate a real binary and put it first on PATH.
find_claude() {
  local c
  for c in \
    "$HOME/.claude/local/claude" \
    "$HOME/.local/bin/claude" \
    "/opt/homebrew/bin/claude" \
    "/usr/local/bin/claude"; do
    [ -x "$c" ] && { echo "$c"; return 0; }
  done
  # last resort: a `claude` on PATH that is a real file (not just a function/alias)
  c="$(command -v claude 2>/dev/null || true)"
  [ -n "$c" ] && [ -f "$c" ] && { echo "$c"; return 0; }
  return 1
}

CLAUDE_BIN="$(find_claude || true)"
if [ -z "$CLAUDE_BIN" ]; then
  fail "no real 'claude' (Claude Code) binary found — the CLI tape needs one.
       Install Claude Code, or set it on PATH, then re-run. (App demo is unaffected.)"
fi
CLAUDE_DIR="$(dirname "$CLAUDE_BIN")"

[ -f "$SRC_CONFIG" ] || fail "source config not found: $SRC_CONFIG (set LLMUX_CONFIG)"

# Derive a demo config: a copy of the real config on a private port so it does not
# fight the live :3456 daemon. Real accounts are reused (needed for a real call);
# LLMUX_DEMO_MODE=1 masks their emails in the rendered TUI.
python3 - "$SRC_CONFIG" "$DEMO_CONFIG" "$DEMO_PORT" <<'PY'
import json, sys
src, dst, port = sys.argv[1], sys.argv[2], int(sys.argv[3])
cfg = json.load(open(src))
cfg.setdefault("proxy", {})["port"] = port
json.dump(cfg, open(dst, "w"), indent=2)
print(f"record-cli: wrote {dst} (port {port}, {len(cfg.get('accounts', []))} accounts)")
PY

echo "record-cli: rendering demo/llmux.tape → screenshots/llmux-demo.gif (real Claude call, ~30s)…"
PATH="$CLAUDE_DIR:$REPO_ROOT/target/release:$HOME/.local/bin:$PATH" \
  LLMUX_DEMO_CONFIG="$DEMO_CONFIG" \
  LLMUX_DEMO_MODE=1 \
  vhs demo/llmux.tape

[ -f screenshots/llmux-demo.gif ] || fail "vhs finished but screenshots/llmux-demo.gif was not produced"
echo "record-cli: OK → screenshots/llmux-demo.gif ($(du -h screenshots/llmux-demo.gif | cut -f1))"
