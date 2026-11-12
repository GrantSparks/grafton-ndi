#!/usr/bin/env bash
# Build the library for the statically linked Apple mobile targets.
#
#   NDI_SDK_DIR="/path/to/NDI SDK for Apple" ci/apple-mobile.sh
#
# These targets link the vendor archive, so they are the one place where the
# crate needs an SDK at build time. Requires macOS with Xcode and rustup.
set -euo pipefail

: "${NDI_SDK_DIR:?Set NDI_SDK_DIR to the NDI SDK for Apple}"

# build <toolchain> <target> <deployment variable> <minimum OS> [cargo arguments]
#
# A library build links nothing, so it needs no Xcode SDK.
build() {
  local toolchain=$1 target=$2 variable=$3 minimum=$4
  shift 4
  echo "== $target ($toolchain)"
  env "$variable=$minimum" \
    cargo "+$toolchain" build --locked --lib --all-features --target "$target" "$@"
}

rustup target add --toolchain stable \
  aarch64-apple-ios x86_64-apple-ios aarch64-apple-tvos aarch64-apple-visionos
build stable aarch64-apple-ios IPHONEOS_DEPLOYMENT_TARGET 16.0
build stable x86_64-apple-ios IPHONEOS_DEPLOYMENT_TARGET 16.0
build stable aarch64-apple-tvos TVOS_DEPLOYMENT_TARGET 16.0
build stable aarch64-apple-visionos XROS_DEPLOYMENT_TARGET 1.0

# rustup ships no standard library for the tvOS simulator target, so build one.
nightly='nightly-2026-11-10'
rustup toolchain install "$nightly" --profile minimal --component rust-src
build "$nightly" x86_64-apple-tvos TVOS_DEPLOYMENT_TARGET 16.0 \
  -Zbuild-std=std,panic_abort
