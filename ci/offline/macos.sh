#!/bin/sh
# Cargo test runner for macOS: run the test executable under a Seatbelt profile
# that denies every network operation except binding, loopback connections and
# traffic arriving on a local address.
#
#   cargo test --config ci/offline.toml ...
#
# The profile also denies the mDNSResponder socket, so:
#
# * the SDK is pointed at a loopback discovery address instead of multicast DNS;
# * host names, including the machine's own, cannot be relied on to resolve.
#   Tests therefore address their senders as 127.0.0.1:<port>; the one test
#   that looks a source up by host name is compiled for Linux only.
#
# TMPDIR is private to the run, as in the Linux runner, where the SDK keeps its
# sender and receiver name locks in that directory.
#
# Limits: Seatbelt filters this process only. A listener bound to a wildcard
# address stays reachable from the machine's own network, because the profile
# language cannot restrict the peer of inbound traffic.
set -eu

scratch=$(mktemp -d)
trap 'rm -rf -- "${scratch:?}"' EXIT
mkdir "$scratch/config" "$scratch/tmp"
printf '{"ndi":{"networks":{"discovery":"127.0.0.1"}}}\n' > "$scratch/config/ndi-config.v1.json"

export NDI_CONFIG_DIR="$scratch/config"
export TMPDIR="$scratch/tmp"
export CARGO_NET_OFFLINE=true
/usr/bin/sandbox-exec -p '(version 1)(allow default)(deny network*)(allow network-bind)(allow network-outbound (remote ip "localhost:*"))(allow network-inbound (local ip "localhost:*"))' "$@"
