#!/usr/bin/env bash
# Build and link every example; device/runtime testing is a separate step.
set -euo pipefail

: "${NDI_SDK_DIR:?Set NDI_SDK_DIR to the extracted NDI SDK for Android}"
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
bindgen_var="BINDGEN_EXTRA_CLANG_ARGS_${target//-/_}"
export "$linker_var=$linker"
# Target-specific args override host/global bindgen args. Keep paths quoted for
# bindgen's shell-word parser when the SDK/NDK directory contains spaces.
export "$bindgen_var=--sysroot=\"$toolchain/sysroot\" -isystem \"$toolchain/sysroot/usr/include/$header_target\" -D__ANDROID_API__=$api"
cargo build --examples --all-features --target "$target"
