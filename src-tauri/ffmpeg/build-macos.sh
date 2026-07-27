#!/bin/sh
set -eu
OSXCROSS_IMAGE=${OSXCROSS_IMAGE:-crazymax/osxcross:latest}
target=${1:-aarch64-apple-darwin}

case "$target" in
  x86_64-apple-darwin)
    target_arch=x86_64
    ;;
  aarch64-apple-darwin)
    target_arch=aarch64
    ;;
  *)
    echo "Unsupported macOS target: $target" >&2
    exit 2
    ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
src_tauri_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
output_dir=$(mktemp -d)

cleanup() {
  rm -rf "$output_dir"
}

trap cleanup EXIT

docker buildx build \
  --output "type=local,dest=$output_dir" \
  --build-arg "OSXCROSS_IMAGE=$OSXCROSS_IMAGE" \
  --build-arg "TARGET_ARCH=$target_arch" \
  --build-arg "MACOS_MIN_VERSION=${MACOS_MIN_VERSION:-11.0}" \
  --file "$script_dir/Dockerfile.macos" \
  "$script_dir"
mkdir -p "$src_tauri_dir/binaries"

install -m 755 "$output_dir/ffmpeg" "$src_tauri_dir/binaries/ffmpeg-$target"
install -m 755 "$output_dir/ffprobe" "$src_tauri_dir/binaries/ffprobe-$target"
