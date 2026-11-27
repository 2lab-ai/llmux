#!/usr/bin/env bash
# See llmux Islands for Linux from a Mac (or any Docker host).
#
#   dev/kde-preview.sh snapshot [OUT]   fixture-backed PNGs of every surface (offscreen Qt)
#   dev/kde-preview.sh wayland  [OUT]   the live shell on a headless Wayland desktop -> screen.png
#   dev/kde-preview.sh shell            a shell inside the dev container
#
# The container (default name llmux-kde) mounts this checkout at /src and keeps
# cargo caches in named volumes, so only the first build is slow. OUT defaults
# to target/kde-preview in this checkout.
set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
image="${LLMUX_KDE_IMAGE:-llmux-islands-preview:dev}"
name="${LLMUX_KDE_CONTAINER:-llmux-kde}"
mode="${1:-snapshot}"
out="${2:-$repo/target/kde-preview}"

ensure_image() {
    docker image inspect "$image" >/dev/null 2>&1 \
        || docker build --platform linux/amd64 -f "$repo/llmux-islands-linux/dev/Dockerfile.preview" \
            -t "$image" "$repo/llmux-islands-linux/dev"
}

ensure_container() {
    local mounted
    mounted="$(docker inspect -f '{{range .Mounts}}{{if eq .Destination "/src"}}{{.Source}}{{end}}{{end}}' "$name" 2>/dev/null || true)"
    if [ "$mounted" != "$repo" ] || [ "$(docker inspect -f '{{.State.Running}}' "$name" 2>/dev/null)" != "true" ]; then
        docker rm -f "$name" >/dev/null 2>&1 || true
        docker run -d --name "$name" --platform linux/amd64 \
            -v "$repo":/src -v llmux-kde-target:/target -v llmux-kde-cargo:/root/.cargo \
            "$image" sleep infinity >/dev/null
    fi
}

run() { docker exec -w /src/llmux-islands-linux "$name" bash -lc "$1"; }

ensure_image
ensure_container
mkdir -p "$out"

case "$mode" in
    snapshot)
        run 'rm -rf /target/snap && cargo run --locked -- --snapshot-dir /target/snap'
        docker cp "$name:/target/snap/." "$out/"
        ;;
    wayland)
        # Headless wlroots desktop (no GPU): pixman renderer, one 1512x982 output.
        run 'mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
             pkill -x sway-nocap || true; pkill -x llmux-islands-l || true  # comm is cut to 15 chars; -f would match this shell
             printf "output HEADLESS-1 resolution 1512x982 bg #202124 solid_color\n" > /tmp/sway.conf
             # Arch ships sway with a file capability (cap_sys_nice) that a
             # container may not grant; a plain copy runs without it.
             cp /usr/sbin/sway /tmp/sway-nocap
             WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
                 /tmp/sway-nocap --unsupported-gpu -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
             for i in $(seq 1 50); do ls "$XDG_RUNTIME_DIR"/wayland-* >/dev/null 2>&1 && break; sleep 0.2; done
             export WAYLAND_DISPLAY=$(basename "$(ls "$XDG_RUNTIME_DIR"/wayland-* | grep -v lock | head -1)")
             cargo build --locked
             QT_QPA_PLATFORM=wayland XDG_CURRENT_DESKTOP=KDE XDG_SESSION_TYPE=wayland \
                 /target/debug/llmux-islands-linux >/tmp/islands.log 2>&1 &
             sleep "${LLMUX_KDE_SETTLE:-6}"
             grim /target/screen.png'
        docker cp "$name:/target/screen.png" "$out/screen.png"
        ;;
    shell)
        docker exec -it -w /src/llmux-islands-linux "$name" bash
        ;;
    *)
        echo "usage: $0 snapshot|wayland|shell [OUT]" >&2
        exit 2
        ;;
esac
echo "$out"
