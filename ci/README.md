# Build and test from source

For library installation, SDK requirements and supported platforms, start with
[setup](../docs/setup.md). The [examples](../examples/README.md) exercise sending,
receiving, image encoding, async adapters and recording.

## Running the checks locally

Use the selected stable Rust toolchain with Clippy and rustfmt:

```sh
cargo build --locked --examples --all-features
cargo test --locked --all-features
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
```

Tests that require an unavailable NDI runtime report a skip. Runtime
initialization errors still fail. A successful SDK-free run therefore does
not establish native transport coverage. The configured platform/runtime
matrix is in [the CI workflow](../.github/workflows/rust.yml); execution results
are available in [GitHub Actions](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml).

## Test against an installed runtime

Set `NDI_RUNTIME_DIR_V6` to the native library directory. An Advanced build also
needs `NDI_SDK_DIR` pointing at its SDK and `libclang`; see
[build requirements](../docs/setup.md#build-requirements).

Require the prerequisites you intend to test so missing libraries fail rather
than skip:

```sh
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo test --locked --all-features --config ci/offline.toml
```

`GRAFTON_NDI_REQUIRE_RUNTIME` accepts comma-separated tokens:

| Token | Prerequisite |
| --- | --- |
| `1` | NDI runtime; implied by every other token |
| `advanced` | Advanced runtime and compiled Advanced declarations |
| `file` | Usable separate File library, with `file-io` enabled |
| `discovery-server` | Compatible runtime and SDK Discovery Server, on Linux |
| `h264-decoder` | Runtime can decode the H.264 fixture, with `advanced` enabled |
| `hevc-444-decoder` | Runtime can decode the HEVC 4:4:4 fixture, with `advanced` enabled |

For example, use `advanced,file` when both are installed. Unknown tokens and
requirements that the build cannot exercise are errors. Decoder fixtures do
not establish compatibility with every third-party stream or hardware codec.

### Loopback-only test runs

Native tests create network-visible NDI objects.
[`offline.toml`](offline.toml) selects a runner on Linux and macOS. Linux uses
bubblewrap with a private loopback-only network namespace and requires
unprivileged user namespaces. macOS uses Seatbelt to restrict outbound traffic
to loopback; it permits SDK multicast-control socket options, and wildcard
listeners can still receive external traffic. Windows tests use the machine's
network. The runners do not isolate Cargo's build processes or the filesystem.

### Runtime identity and compatibility

To verify selection, set `GRAFTON_NDI_EXPECTED_RUNTIME` to the exact library
path, `GRAFTON_NDI_EXPECTED_VERSION` to its four-component version and
`GRAFTON_NDI_EXPECTED_EDITION` to `standard` or `advanced`. Clear those values
when changing runtimes. `NDI_SDK_DIR` selects build headers;
`NDI_RUNTIME_DIR_V6` selects the runtime loaded by tests.

Cross-version transport tests can use `GRAFTON_NDI_PEER_RUNTIME`,
`GRAFTON_NDI_PEER_VERSION` and `GRAFTON_NDI_PEER_EDITION` for a second runtime
with a different version. Inspect actual skips and results for your platform.

When reporting a problem, include the crate version, platform, Rust version,
Cargo features, runtime version/edition, command and error output. Do not
include SDK archives, credentials or private recordings.
