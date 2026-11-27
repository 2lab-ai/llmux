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
    # Recreate when the container is stopped, mounts another checkout, or
    # runs another image (LLMUX_KDE_IMAGE changed or the image was rebuilt).
    local want have
    want="$repo true $(docker image inspect -f '{{.Id}}' "$image")"
    have="$(docker inspect -f '{{range .Mounts}}{{if eq .Destination "/src"}}{{.Source}}{{end}}{{end}} {{.State.Running}} {{.Image}}' "$name" 2>/dev/null || true)"
    if [ "$have" != "$want" ]; then
        docker rm -f "$name" >/dev/null 2>&1 || true
        docker run -d --name "$name" --platform linux/amd64 \
            -v "$repo":/src -v llmux-kde-target:/target -v llmux-kde-cargo:/root/.cargo \
            "$image" sleep infinity >/dev/null
    fi
}

run() { docker exec -w /src/llmux-islands-linux "$name" bash -lc "set -euo pipefail; $1"; }

# Containers share the /target volume (cargo cache); captures are per
# container so two previews never overwrite each other.
snap="/target/snap-$name"
screen="/target/screen-$name.png"

# Every checkout mounts at /src and builds into the shared /target, and cargo
# decides what to rebuild by mtime: after switching checkouts it would keep
# the other checkout's build script, QML and code. Touch this crate's files
# whenever the checkout differs from the last one built.
sync_checkout() {
    run "if [ \"\$(cat /target/.preview-checkout 2>/dev/null)\" != '$repo' ]; then
             find . -path ./target -prune -o -type f -print0 | xargs -0 touch
             echo '$repo' > /target/.preview-checkout
         fi"
}

ensure_image
ensure_container
mkdir -p "$out"

case "$mode" in
    snapshot)
        sync_checkout
        run "rm -rf $snap && cargo run --locked -- --snapshot-dir $snap"
        docker cp "$name:$snap/." "$out/"
        ;;
    wayland)
        sync_checkout
        docker exec -w /src/llmux-islands-linux -e LLMUX_KDE_SETTLE="${LLMUX_KDE_SETTLE:-6}" \
            -e LLMUX_KDE_SCREEN="$screen" "$name" dbus-run-session -- bash dev/wayland-capture.sh
        docker cp "$name:$screen" "$out/screen.png"
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
