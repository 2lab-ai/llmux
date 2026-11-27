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
        docker exec -w /src/llmux-islands-linux -e LLMUX_KDE_SETTLE="${LLMUX_KDE_SETTLE:-6}" "$name" \
            dbus-run-session -- bash dev/wayland-capture.sh
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
