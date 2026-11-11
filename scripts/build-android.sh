#!/usr/bin/env bash
# Build and link every example for an Android target with the NDK.
#
#   ANDROID_NDK_HOME=/path/to/ndk scripts/build-android.sh aarch64-linux-android
#
# The crate loads the NDI runtime when the application starts, so no NDI SDK is
# needed to build. Set NDI_SDK_DIR to an NDI Advanced SDK for Android to compile
# the Advanced implementation from its headers; device and runtime testing is a
# separate step either way.
set -euo pipefail

: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to the Android NDK installation}"
target="${1:?Usage: scripts/build-android.sh <Rust Android target>}"
case "$target" in
  aarch64-linux-android|i686-linux-android|x86_64-linux-android)
    clang_target="$target"
    header_target="$target"
    ;;
  armv7-linux-androideabi)
    clang_target=armv7a-linux-androideabi
    header_target=arm-linux-androideabi
    ;;
  *) echo "Unsupported Android target: $target" >&2; exit 1 ;;
esac
case "$(uname -s)" in
  Linux) host_tag=linux-x86_64 ;;
  Darwin) host_tag=darwin-x86_64 ;;
  *) echo 'This script requires a Linux or macOS host' >&2; exit 1 ;;
esac
toolchain="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$host_tag"
api="${ANDROID_API_LEVEL:-24}"
linker="$toolchain/bin/${clang_target}${api}-clang"
if [ ! -x "$linker" ]; then
  echo "Android linker not found: $linker" >&2
  exit 1
fi
linker_var="CARGO_TARGET_$(printf '%s' "$target" | tr '[:lower:]-' '[:upper:]_')_LINKER"
export "$linker_var=$linker"
# Only the Advanced headers are parsed at build time. The API level is selected
# through Clang's target, because redefining __ANDROID_API__ conflicts with the
# Android driver's built-in macro. The paths stay quoted for bindgen's shell-word
# parser: SDK and NDK directories may contain spaces.
bindgen_var="BINDGEN_EXTRA_CLANG_ARGS_${target//-/_}"
export "$bindgen_var=--target=${clang_target}${api} --sysroot=\"$toolchain/sysroot\" -isystem \"$toolchain/sysroot/usr/include/$header_target\""
rustup target add "$target"
cargo build --locked --examples --all-features --target "$target"
