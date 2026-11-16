#!/usr/bin/env bash
# Lint the library for the statically linked Apple mobile targets.
#
#   NDI_SDK_DIR="/path/to/NDI SDK for Apple" ci/apple-mobile.sh
#
# These targets link the vendor archive, so they are the one place where the
# crate needs an SDK at build time. Requires rustup; these Clippy checks do
# not link an application.
set -euo pipefail

: "${NDI_SDK_DIR:?Set NDI_SDK_DIR to the NDI SDK for Apple}"

# Use the contributor release for every target that does not need the nightly.
repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
release=$(python3 - "$repository_root/rust-toolchain.toml" <<'PYTHON'
import sys
import tomllib
with open(sys.argv[1], "rb") as source:
    print(tomllib.load(source)["toolchain"]["channel"])
PYTHON
)

# lint <toolchain> <target> <deployment variable> <minimum OS> [cargo arguments]
#
# A library check links nothing, so it needs no Xcode SDK.
lint() {
  local toolchain=$1 target=$2 variable=$3 minimum=$4
  shift 4
  echo "== $target ($toolchain)"
  env "$variable=$minimum" \
    cargo "+$toolchain" clippy --locked --lib --all-features --target "$target" "$@" -- -D warnings
  env "$variable=$minimum" \
    cargo "+$toolchain" clippy --locked --lib --target "$target" "$@" -- -D warnings
}

rustup component add --toolchain "$release" clippy
rustup target add --toolchain "$release" \
  aarch64-apple-ios x86_64-apple-ios aarch64-apple-tvos aarch64-apple-visionos \
  aarch64-apple-ios-sim
# The ARM64 simulator CPU also occurs in device archives. Refuse the target
# in the build script, before selecting a device slice or reaching the linker.
unsupported='aarch64-apple-ios-sim'
if output=$(cargo "+$release" check --locked --lib --target "$unsupported" 2>&1); then
  printf 'Expected the unsupported Apple target %s to fail\n' "$unsupported" >&2
  exit 1
fi
if [[ "$output" != *"Unsupported Apple static target $unsupported;"* ]]; then
  printf '%s\n' "$output" >&2
  exit 1
fi
lint "$release" aarch64-apple-ios IPHONEOS_DEPLOYMENT_TARGET 16.0
lint "$release" x86_64-apple-ios IPHONEOS_DEPLOYMENT_TARGET 16.0
lint "$release" aarch64-apple-tvos TVOS_DEPLOYMENT_TARGET 16.0
lint "$release" aarch64-apple-visionos XROS_DEPLOYMENT_TARGET 1.0

# rustup ships no standard library for the tvOS simulator target, so build one.
nightly='nightly-2026-11-10'
rustup toolchain install "$nightly" --profile minimal --component rust-src --component clippy
lint "$nightly" x86_64-apple-tvos TVOS_DEPLOYMENT_TARGET 16.0 \
  -Zbuild-std=std,panic_abort
