#!/bin/sh
set -eu
FFMPEG_ARCH="win64"
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
src_tauri_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)

FFMPEG_REF="8.1"
# 修改构建参数
FF_CONFIGURE="--toolchain=msvc --enable-static --disable-shared  --disable-everything --disable-programs --enable-ffmpeg --enable-ffprobe --disable-avdevice --enable-avfilter --disable-swresample --disable-swscale  --disable-network --disable-doc --disable-debug --enable-protocol=file,pipe --enable-demuxer=matroska,ass,srt --enable-muxer=matroska,ass,srt --enable-decoder=ass,ssa,subrip --enable-encoder=ass,ssa,subrip"
cat <<EOF > Dockerfile.tmp
    FROM ghcr.io/btbn/ffmpeg-builds/$FFMPEG_ARCH-lgpl-$FFMPEG_REF
    ENV FF_CONFIGURE="$FF_CONFIGURE"
EOF
curr=$(pwd)
cleanup(){
	rm -rf Dockerfile.tmp
	cd "$curr/FFmpeg-Builds"
	git checkout build.sh
	
}
trap cleanup EXIT
docker build . -t ghcr.io/btbn/ffmpeg-builds/$FFMPEG_ARCH-lgpl-$FFMPEG_REF -f Dockerfile.tmp


ls FFmpeg-Builds || git clone https://github.com/BtbN/FFmpeg-Builds
cd FFmpeg-Builds
sed -i 's@rm -rf ffbuild@# rm -rf ffbuild@' build.sh
unset GITHUB_REPOSITORY
./build.sh $FFMPEG_ARCH lgpl $FFMPEG_REF
tree ffbuild/prefix
cp -r ffbuild/prefix "$src_tauri_dir/../ffmpeg-Windows"
ls "$src_tauri_dir/../ffmpeg-Windows"
mkdir -p "src_tauri_dir/binaries"
install -m 755 "$src_tauri_dir/../ffmpeg-Windows/bin/ffmpeg.exe" "$src_tauri_dir/binaries/ffm-x86_64-pc-windows-msvc.exe"
install -m 755 "$src_tauri_dir/../ffmpeg-Windows/bin/ffprobe.exe" "$src_tauri_dir/binaries/ffp-x86_64-pc-windows-msvc.exe"
install -m 755 "$src_tauri_dir/../ffmpeg-Windows/bin/ffmpeg.exe" "$src_tauri_dir/binaries/ffm-x86_64-pc-windows-gnu.exe"
install -m 755 "$src_tauri_dir/../ffmpeg-Windows/bin/ffprobe.exe" "$src_tauri_dir/binaries/ffp-x86_64-pc-windows-gnu.exe"
