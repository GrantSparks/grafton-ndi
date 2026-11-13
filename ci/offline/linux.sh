#!/bin/sh
# Cargo test runner for Linux: run the test executable in its own network
# namespace, where only loopback exists.
#
#   cargo test --config ci/offline.toml ...
#
# Three things besides the namespace are needed for the SDK to work in it:
#
# * /run is replaced by an empty tmpfs, so the process cannot reach the host's
#   D-Bus or Avahi sockets and ask a host daemon to announce on its behalf.
# * /etc/hosts is replaced by one that maps the machine's own name to loopback.
#   A sender advertises itself under that name, so a lookup by host name finds
#   it only if the name resolves to an address that exists in the namespace.
# * TMPDIR moves into the private tmpfs. The SDK reserves every sender and
#   receiver name machine-wide with a lock file, $TMPDIR/NTK_SEND_<name>, so in
#   a shared directory unrelated processes fail each other's sender creation.
set -eu

host=$(hostname)
exec 9<<EOF
127.0.0.1 localhost $host $host.
::1 localhost
EOF

# Nested cargo invocations (trybuild) must not wait for a registry they cannot reach.
export CARGO_NET_OFFLINE=true
exec bwrap --unshare-user --unshare-net --dev-bind / / --tmpfs /run \
    --dir /run/tmp --setenv TMPDIR /run/tmp --ro-bind-data 9 /etc/hosts -- "$@"
