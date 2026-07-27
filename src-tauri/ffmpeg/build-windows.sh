#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
src_tauri_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
output_dir=$(mktemp -d)

cleanup() {
  rm -rf "$output_dir"
}

trap cleanup EXIT

docker buildx build \
  --output "type=local,dest=$output_dir" \
  --file "$script_dir/Dockerfile.windows" \
  "$script_dir"
mkdir -p "$src_tauri_dir/binaries"

install -m 755 "$output_dir/ffmpeg.exe" "$src_tauri_dir/binaries/ffm-x86_64-pc-windows-msvc.exe"
install -m 755 "$output_dir/ffprobe.exe" "$src_tauri_dir/binaries/ffp-x86_64-pc-windows-msvc.exe"
