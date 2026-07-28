#!/bin/sh
set -eu
FFMPEG_ARCH="win64"
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
src_tauri_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
output_dir="artifacts/"

FFMPEG_REF="7.1"
# 修改构建参数
FF_CONFIGURE="--enable-static --disable-shared  --disable-everything --disable-programs --enable-ffmpeg --enable-ffprobe --disable-avdevice --enable-avfilter --disable-swresample --disable-swscale  --disable-network --disable-doc --disable-debug --enable-protocol=file,pipe --enable-demuxer=matroska,ass,srt --enable-muxer=matroska,ass,srt --enable-decoder=ass,ssa,subrip --enable-encoder=ass,ssa,subrip"
cat <<EOF > Dockerfile.tmp
    FROM ghcr.io/btbn/ffmpeg-builds/$FFMPEG_ARCH-lgpl-$FFMPEG_REF
    ENV FF_CONFIGURE="$FF_CONFIGURE"
EOF

docker build . -t ghcr.io/btbn/ffmpeg-builds/$FFMPEG_ARCH-lgpl-$FFMPEG_REF -f Dockerfile.tmp


git clone https://github.com/BtbN/FFmpeg-Builds
cd FFmpeg-Builds
unset GITHUB_REPOSITORY
./build.sh $FFMPEG_ARCH lgpl $FFMPEG_REF

zip_file=$(find $output_dir -name *.zip | head -n 1)
unzip $zip_file -d $output_dir
ls -all -h $output_dir
ff=$(find -r $output_dir -name ffmpeg.exe | head -n 1)
fp=$(find -r $output_dir -name ffprobe.exe | head -n 1 )



install -m 755 "$ff" "$src_tauri_dir/binaries/ffm-x86_64-pc-windows-msvc.exe"
install -m 755 "$fp" "$src_tauri_dir/binaries/ffp-x86_64-pc-windows-msvc.exe"
