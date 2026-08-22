#!/bin/sh
set -ex

target=${1:-x86_64-unknown-linux-gnu}

case "$target" in
  x86_64-unknown-linux-gnu | x86_64-unknown-linux-musl) platform=linux/amd64 ;;
  aarch64-unknown-linux-gnu) platform=linux/arm64 ;;
  *)
    echo "Unsupported Linux target: $target" >&2
    exit 2
    ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
src_tauri_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
if [ -z "${FFMPEG_DIR:-}" ]; then
  FFMPEG_DIR=$(CDPATH= cd -- "$src_tauri_dir/.." && mkdir -p ffmpeg-Linux && cd ffmpeg-Linux && pwd)
else
  mkdir -p "$FFMPEG_DIR"
  FFMPEG_DIR=$(CDPATH= cd -- "$FFMPEG_DIR" && pwd)
fi

if [ "${SKIP_DOCKER_BUILD:-}" = "1" ]; then
  echo "已请求跳过 Docker FFmpeg 构建：$FFMPEG_DIR"
elif [ -z "$(find "$FFMPEG_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  docker buildx build --progress=plain\
    --output="type=local,dest=$FFMPEG_DIR" \
    --file="$script_dir/Dockerfile.linux" \
    "$script_dir"
else
  echo "FFmpeg 输出目录非空，跳过构建：$FFMPEG_DIR"
fi

mkdir -p "$src_tauri_dir/binaries"
install -m 755 "$FFMPEG_DIR/bin/ffmpeg" "$src_tauri_dir/binaries/ffm-$target"
install -m 755 "$FFMPEG_DIR/bin/ffprobe" "$src_tauri_dir/binaries/ffp-$target"
