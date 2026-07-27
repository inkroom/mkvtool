#!/bin/sh
set -eu

target=${1:-x86_64-unknown-linux-gnu}

case "$target" in
  x86_64-unknown-linux-gnu) platform=linux/amd64 ;;
  aarch64-unknown-linux-gnu) platform=linux/arm64 ;;
  *)
    echo "Unsupported Linux target: $target" >&2
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
  --platform "$platform" \
  --output "type=local,dest=$output_dir" \
  --file "$script_dir/Dockerfile.linux" \
  "$script_dir"
mkdir -p "$src_tauri_dir/binaries"
install -m 755 "$output_dir/ffmpeg" "$src_tauri_dir/binaries/ffm-$target"
install -m 755 "$output_dir/ffprobe" "$src_tauri_dir/binaries/ffp-$target"
