#!/bin/sh
set -eu

# Build the Rust C ABI for exactly the architectures Xcode is linking. All
# artifacts live in DerivedData; the source tree remains clean.
project_dir=${PROJECT_DIR:?PROJECT_DIR is required}
derived_dir=${DERIVED_FILE_DIR:?DERIVED_FILE_DIR is required}
crate_dir="$project_dir/../llmux-islands-macos-bridge"
manifest="$crate_dir/Cargo.toml"
target_dir="$derived_dir/rust-target"
output="$derived_dir/libllmux_islands_macos_bridge.a"

if [ ! -f "$manifest" ]; then
    echo "error: missing Rust macOS bridge manifest: $manifest" >&2
    exit 1
fi

mkdir -p "$target_dir"
export CARGO_TARGET_DIR="$target_dir"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
# Keep Cargo and rustc on one rustup toolchain. A Homebrew Cargo paired with a
# different rustc can stamp objects for a newer macOS than the application.
cargo_bin=$(rustup which --toolchain "$RUSTUP_TOOLCHAIN" cargo)
rustc_bin=$(rustup which --toolchain "$RUSTUP_TOOLCHAIN" rustc)
rustdoc_bin=$(rustup which --toolchain "$RUSTUP_TOOLCHAIN" rustdoc)
export RUSTC="$rustc_bin"
export RUSTDOC="$rustdoc_bin"

archives=""
for arch in ${ARCHS:-${CURRENT_ARCH:-arm64}}; do
    case "$arch" in
        arm64) rust_target=aarch64-apple-darwin ;;
        x86_64) rust_target=x86_64-apple-darwin ;;
        *)
            echo "error: unsupported macOS Rust architecture: $arch" >&2
            exit 1
            ;;
    esac

    # Xcode Release may request a universal archive even on a single-arch
    # host. Installing an already-present target is idempotent.
    rustup target add --toolchain "$RUSTUP_TOOLCHAIN" "$rust_target"

    "$cargo_bin" build \
        --manifest-path "$manifest" \
        --locked \
        --release \
        --target "$rust_target"
    archive="$target_dir/$rust_target/release/libllmux_islands_macos_bridge.a"
    if [ ! -f "$archive" ]; then
        echo "error: Rust bridge archive was not produced: $archive" >&2
        exit 1
    fi
    archives="$archives $archive"
done

set -- $archives
if [ "$#" -eq 1 ]; then
    cp "$1" "$output"
else
    xcrun lipo -create "$@" -output "$output"
fi

echo "Built Rust shared UI core: $output"
