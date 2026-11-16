# Build and test from source

For library installation, SDK requirements and supported platforms, start with
[setup](../docs/setup.md). The [examples](../examples/README.md) exercise sending,
receiving, image encoding, async adapters and recording.

## Running the checks locally

CI builds the API reference and checks documentation links and examples once
in its cached lint job. Pushes to `main` upload that HTML for GitHub Pages;
the deployment job publishes it only after `required-ci` succeeds. Pull
requests, manual CI runs and release-tag runs validate docs without publishing.
Pages write permissions are confined to the deployment job, which runs no
repository scripts. The release caller forwards those permissions for reusable
workflow validation; its tag ref keeps the Pages job disabled.

Use the pinned Rust toolchain with Clippy and rustfmt:

```sh
python -m unittest discover -s ci/tests
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
for features in "" image-encoding tokio smol advanced file-io advanced,file-io; do
    cargo clippy --locked --all-targets --no-default-features ${features:+--features "$features"} -- -D warnings
done
cargo test --locked --package xtask
for features in "" image-encoding tokio smol advanced file-io advanced,file-io; do
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --no-default-features ${features:+--features "$features"}
done
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
python ci/docs.py --compile --rustdoc target/doc
cargo build --locked --examples --all-features
cargo test --locked --all-features
cargo test --locked
cargo test --locked --no-default-features --features file-io
```

Tests that require an unavailable NDI runtime report a skip. Runtime
initialization errors still fail. A successful SDK-free run therefore does
not establish native transport coverage. The configured platform/runtime
matrix is in [the CI workflow](../.github/workflows/rust.yml); execution results
are available in [GitHub Actions](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml).

### Updating the Rust toolchain

`rust-toolchain.toml` pins the release used by contributors and every stable CI
job. Change the toolchain file and every workflow toolchain step together in one
PR. Install that release, then re-bless the compile-contract snapshots:

```sh
TRYBUILD=overwrite cargo test --locked --all-features --test compile_contracts
# With Advanced SDK headers installed:
TRYBUILD=overwrite cargo test --locked --all-features --test advanced_contracts
```

Review every `.stderr` change and fix new Clippy findings under `-D warnings`
before merging the bump. The minimum supported version and the Miri and Apple
mobile nightlies have their own pins.

### Memory checkers

The required `miri` job runs synthetic frame, metadata, guard and send-path
aliasing tests with `nightly-2026-11-10`, using both Stacked Borrows and Tree
Borrows. Native tests report visible skips under Miri; source-parsing and
subprocess tests are outside its filters. The long descriptor churn test is
explicitly skipped. Both memory-checker steps list each filter before running
it and fail if it selects no tests; Miri also checks every skip name. This keeps
fixture renames from silently removing coverage. The pinned nightly detects a
`Box`-owned `AliasableBox` regression; it does not yet support the Miri option
needed to enforce `AliasableVec`'s unique-pointer contract. Miri uses one padded
layout and one PNG encoding per format; native tests retain their full matrices.
Run a focused case locally with:

```sh
cargo +nightly-2026-11-10 miri test --locked --lib receiver::mixed_tests
MIRIFLAGS="-Zmiri-tree-borrows" cargo +nightly-2026-11-10 miri test --locked --lib receiver::mixed_tests
```

The Ubuntu `test` job runs the scripted capture fixtures and full encoder padding
matrix under Valgrind Memcheck. It traces child test processes, skips compiler
children and fails on errors. The Standard 6.3.2 Linux SDK row also checks live
padding through the loopback runner. A narrowly scoped suppression covers only
glibc loader token comparisons reported by Valgrind 3.19:

```sh
cargo test --locked --all-features --config 'target.x86_64-unknown-linux-gnu.runner=["valgrind","--tool=memcheck","--error-exitcode=99","--trace-children=yes","--trace-children-skip=*/rustc,*/rustc-*"]' --test mixed_capture_boundary --test frame_metadata_boundary
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo test --locked --features image-encoding --config ci/offline.toml --config 'target.x86_64-unknown-linux-gnu.runner=["ci/offline/linux.sh","valgrind","--tool=memcheck","--error-exitcode=99","--suppressions=ci/valgrind.supp"]' --test video_padding_sdk -- --test-threads=1
```

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
| `h264-decoder` | Implies `advanced`; runtime can decode the H.264 fixture |
| `hevc-444-decoder` | Implies `advanced`; runtime can decode the HEVC 4:4:4 fixture with the 6.4 Best444 receiver |

For example, use `advanced,file` when both are installed. Unknown tokens and
requirements that the build cannot exercise are errors. Decoder fixtures do
not establish compatibility with every third-party stream or hardware codec.

Native NDI|HX tests need Python 3 and FFmpeg with the `libx264` and `libx265`
encoders. They generate their synthetic H.264/HEVC pictures once per test
process in a temporary directory and remove the files after reading them.
SDK-free tests do not need these tools. To inspect the pictures separately:

```sh
python3 tests/support/generate_hx.py --output-dir target/test-data/ndi-hx
```

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

## Release controls

Workflows cannot establish repository settings. Before publishing, verify these
controls in order:

1. The `crates-io` environment requires reviewers, an appropriate self-review
   rule, and deployment restricted to `v*` tags.
2. A tag ruleset restricts creating, moving and deleting `v*` tags.
3. Protection of `main` requires pull requests, Code Owner review and
   `required-ci`. [CODEOWNERS](../.github/CODEOWNERS) covers files that control CI
   and publishing.
4. The crates.io trusted publisher binds this repository, `publish.yml` and the
   `crates-io` environment.
5. SDK CI has `NDI_SDK_SOURCES_JSON`, a scoped `NDI_SDK_ARCHIVE_TOKEN`, and
   `NDI_CI_RELEASED=true` when enabled.

Anyone who can replace a workflow on a tag or branch can replace its checks.
Repository rules and review enforce these boundaries; the tag-on-main check
catches mistakes.
