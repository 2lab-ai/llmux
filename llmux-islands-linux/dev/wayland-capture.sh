#!/usr/bin/env bash
# Runs INSIDE the preview container (see kde-preview.sh wayland): a headless
# wlroots desktop (sway, pixman renderer, no GPU) with a swaybar tray, the
# shell on it, then a screenshot. The D-Bus session (dbus-run-session) and
# the tray matter: without a StatusNotifier tray the shell stays open after
# its boot animation, with one it closes like on Plasma.
set -euo pipefail
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
# comm is cut to 15 characters; `pkill -f` would match this script itself.
pkill -x sway-nocap || true
pkill -x llmux-islands-l || true
printf 'output HEADLESS-1 resolution 1512x982 bg #202124 solid_color\nbar {\n position bottom\n tray_output *\n}\n' > /tmp/sway.conf
# Arch ships sway with a file capability (cap_sys_nice) a container may not
# grant; a plain copy runs without it.
cp /usr/sbin/sway /tmp/sway-nocap
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
    /tmp/sway-nocap --unsupported-gpu -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
for _ in $(seq 1 50); do
    ls "$XDG_RUNTIME_DIR"/wayland-* >/dev/null 2>&1 && break
    sleep 0.2
done
WAYLAND_DISPLAY="$(basename "$(ls "$XDG_RUNTIME_DIR"/wayland-* | grep -v lock | head -1)")"
export WAYLAND_DISPLAY
cargo build --locked
QT_QPA_PLATFORM=wayland XDG_CURRENT_DESKTOP=KDE XDG_SESSION_TYPE=wayland \
    /target/debug/llmux-islands-linux >/tmp/islands.log 2>&1 &
sleep "${LLMUX_KDE_SETTLE:-6}"
grim /target/screen.png
