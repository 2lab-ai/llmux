#!/usr/bin/env bash
# Record the llmux-islands macOS notch-app demo GIF. Launches the app in --demo
# mode (island opens + holds itself, emails masked), screen-records the notch
# region, and converts to a GIF.
#
# REQUIRES a one-time macOS grant: System Settings → Privacy & Security → Screen
# Recording → enable the app that runs THIS script (your terminal / herdr).
# (CI runners can't do this — that's why recording is local. On macOS 15 the
# permission is tied to the *responsible* process, so avoid `exec`-wrapping.)
#
# Output: screenshots/llmux-islands-demo.gif
# Usage:  demo/record-islands.sh
#   env:  LLMUX_ISLANDS_APP   (default /Applications/LlmuxIslands.app)
#         LLMUX_DEMO_REGION   "x,y,w,h" in points (default: centered top of main display)
#         LLMUX_DEMO_SECONDS  capture duration (default 9)
#         LLMUX_DEMO_FPS / LLMUX_DEMO_WIDTH  gif fps/width (default 20 / 640)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

APP="${LLMUX_ISLANDS_APP:-/Applications/LlmuxIslands.app}"
DUR="${LLMUX_DEMO_SECONDS:-9}"
FPS="${LLMUX_DEMO_FPS:-20}"
GIF_WIDTH="${LLMUX_DEMO_WIDTH:-640}"
OUT="screenshots/llmux-islands-demo.gif"
MOV="$(mktemp -t llmux-islands).mov"

fail() { echo "record-islands: $*" >&2; exit 1; }
command -v ffmpeg >/dev/null || fail "ffmpeg not found — brew install ffmpeg"
command -v screencapture >/dev/null || fail "screencapture not found (macOS only)"
[ -d "$APP" ] || fail "app not found: $APP (set LLMUX_ISLANDS_APP)"
mkdir -p screenshots

# Capture region: centered near the top of the main display unless overridden.
if [ -n "${LLMUX_DEMO_REGION:-}" ]; then
  REGION="$LLMUX_DEMO_REGION"
else
  BOUNDS="$(osascript -e 'tell application "Finder" to get bounds of window of desktop' 2>/dev/null || echo '0, 0, 1728, 1117')"
  W="$(echo "$BOUNDS" | awk -F', ' '{print $3}')"
  CW="${LLMUX_DEMO_CW:-720}"; CH="${LLMUX_DEMO_CH:-660}"
  X=$(( (W - CW) / 2 )); [ "$X" -lt 0 ] && X=0
  REGION="$X,0,$CW,$CH"
fi
echo "record-islands: region=$REGION  duration=${DUR}s  app=$APP"

# Restart the app in demo mode (island opens + holds; emails masked).
osascript -e 'tell application "LlmuxIslands" to quit' >/dev/null 2>&1 || true
pkill -f 'LlmuxIslands.app/Contents/MacOS/LlmuxIslands' 2>/dev/null || true
sleep 1
open -na "$APP" --args --demo
sleep 3   # let the island open + first status poll land

# Record. screencapture -V records video for a fixed duration; -R limits to region.
rm -f "$MOV"
set +e
screencapture -x -V "$DUR" -R "$REGION" "$MOV" 2>/tmp/screencap.err
rc=$?
set -e

if [ $rc -ne 0 ] || [ ! -s "$MOV" ]; then
  echo "record-islands: screen capture FAILED (rc=$rc)." >&2
  cat /tmp/screencap.err >&2 || true
  cat >&2 <<'EOF'

  This is almost always a missing Screen Recording permission (TCC):
    System Settings → Privacy & Security → Screen Recording
    → enable the app running this script (your terminal / herdr), then re-run.
  On macOS 15+, permission follows the *responsible* process; if you launch this
  via a wrapper that `exec`s, grant the outermost app instead.
EOF
  # Relaunch the normal app before exiting so the desktop isn't left in demo mode.
  osascript -e 'tell application "LlmuxIslands" to quit' >/dev/null 2>&1 || true
  open -na "$APP" >/dev/null 2>&1 || true
  exit 3
fi

echo "record-islands: converting → $OUT"
ffmpeg -y -loglevel error -i "$MOV" \
  -vf "fps=$FPS,scale=$GIF_WIDTH:-1:flags=lanczos,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse" \
  -loop 0 "$OUT"
rm -f "$MOV"

# Restore the normal (non-demo) app.
osascript -e 'tell application "LlmuxIslands" to quit' >/dev/null 2>&1 || true
sleep 1
open -na "$APP" >/dev/null 2>&1 || true

[ -f "$OUT" ] || fail "ffmpeg finished but $OUT was not produced"
echo "record-islands: OK → $OUT ($(du -h "$OUT" | cut -f1))"
