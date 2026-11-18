#!/usr/bin/env bash
set -euo pipefail

# The Linux runtime loads libavcodec.so.61 for the two native picture tests.
prefix=${1:?provide an absolute installation directory}
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
cd "$work"

archive=ffmpeg-7.1.5.tar.xz
sha256=de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f
curl --fail --silent --show-error --location --retry 2 --max-time 60 \
    --output "$archive" "https://ffmpeg.org/releases/$archive"
printf '%s  %s\n' "$sha256" "$archive" | sha256sum --check
tar -xf "$archive"
cd "${archive%.tar.xz}"

./configure --prefix="$prefix" --disable-autodetect --disable-everything \
    --enable-shared --disable-static --disable-programs --disable-doc \
    --disable-network --disable-x86asm --enable-decoder=h264,hevc \
    --enable-parser=h264,hevc
make -s -j "$(nproc)"
make -s install-libs
test -f "$prefix/lib/libavcodec.so.61"
