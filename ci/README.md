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
repository scripts. A run skips deployment when its commit is no longer the
tip of `main`. The release caller forwards those permissions for reusable
workflow validation. Its tag ref keeps the Pages job disabled.

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
cargo test --locked --no-fail-fast --all-features
scope_dir="${RUNNER_TEMP:-target/ci}"
mkdir -p "$scope_dir"
cargo test --locked --no-fail-fast -- --list > "$scope_dir/default-tests.txt"
python ci/sdk_test_scope.py --scope fixed-graph --inventory "$scope_dir/default-tests.txt" --output "$scope_dir/default-skips.txt"
scope_args=()
while IFS= read -r argument; do scope_args+=("$argument"); done < "$scope_dir/default-skips.txt"
cargo test --locked --no-fail-fast -- "${scope_args[@]}"
scope_dir="${RUNNER_TEMP:-target/ci}"
mkdir -p "$scope_dir"
cargo test --locked --no-fail-fast --no-default-features --features file-io -- --list > "$scope_dir/file-tests.txt"
python ci/sdk_test_scope.py --scope fixed-graph --inventory "$scope_dir/file-tests.txt" --output "$scope_dir/file-skips.txt"
scope_args=()
while IFS= read -r argument; do scope_args+=("$argument"); done < "$scope_dir/file-skips.txt"
cargo test --locked --no-fail-fast --no-default-features --features file-io -- "${scope_args[@]}"
```

Tests that require an unavailable NDI runtime report a skip. Runtime
initialization errors still fail. A successful SDK-free run therefore does
not establish native transport coverage. The configured platform/runtime
matrix is in [the CI workflow](../.github/workflows/rust.yml); execution results
are available in [GitHub Actions](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml).

The all-feature pass owns three internal compiler fixtures whose nested Cargo
commands explicitly select the default or Advanced-only graph. The later default
and File-only passes still compile their full configurations and run their
feature-specific tests and doctests. Their inventories omit only those three
repeated fixture executions, with exact-name guards in `sdk_test_scope.py`.
Unknown tests remain included.

Native SDK CI has 17 primary configurations and six compatibility roles. Each
compatibility direction runs independently of the primary SDK checks, retaining
both baseline editions, runtime identity checks, peer settings and isolation.
Each header/feature/target build has a primary owner for its compiler and pure
value checks. Compatibility roles use the helper's `runtime` scope to omit those
audited repeated cases while retaining native tests, doctests and unknown tests.
The synthetic-header matrix has one current Advanced compiler-host owner per
platform; older Advanced and Windows x86 rows do not repeat that same host build.
SDK files, generated bindings and SDK-derived binaries stay on their runner.

When adding or changing a test, leave it included unless its inputs prove that
an existing owner covers it. Add exact names and the source rationale to
[`sdk_test_scope.py`](sdk_test_scope.py) only after that audit. The helper
rejects missing, duplicate, colliding or mismatched inventory entries. Its
`select` mode validates named Miri, Memcheck and measurement groups against an
inventory before returning exact test names. Workflow guards require the owner.
SDK-private caches and artifacts are excluded.

The Linux test job also runs the manual measurement fixtures with `--ignored`
in release mode. Its configurable image sweep uses one 64×64 BGRA input and
JPEG output; the fixed workspace and allocator measurements keep their full
inputs. Their printed measurements describe that runner, not a performance
threshold. This SDK-free measurement step is optional locally:

```sh
GRAFTON_NDI_SIZES=64x64 GRAFTON_NDI_FORMATS=Bgra GRAFTON_NDI_CONTENTS=photographic GRAFTON_NDI_OUTPUTS=jpeg85 cargo test --locked --release --all-features --lib -- --ignored --test-threads=1
```

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

The required `miri` job checks synthetic frames, metadata, bounded SDK reads,
descriptor retention, guards and send-path aliasing with `nightly-2026-11-10`,
using both Stacked Borrows and Tree Borrows. Native tests report visible skips
under Miri; source-parsing and subprocess tests are outside its filters. The
long descriptor churn, exhaustive UTF-8 pairs and multi-megabyte copy-budget
fixtures are explicitly skipped. Smaller synthetic tests retain their exact
charges, malformed-entry handling and scan bounds. The
[`sdk_test_scope.py` selector](sdk_test_scope.py) applies the audited skips and
filters to one interpreted inventory per model. The job runs three batches
with `--exact` for bounded reads, media frames and ownership. Serial test cases
preserve the audited local and thread-local state boundaries; explicit worker
threads in the tests still run. Both borrowing models execute the same selected
cases, with a fresh process per batch. This avoids repeating interpreter startup
for every filter while keeping fixture renames from silently removing coverage.
The pinned nightly detects a
`Box`-owned `AliasableBox` regression; it does not yet support the Miri option
needed to enforce `AliasableVec`'s unique-pointer contract. Miri uses one padded
layout and one PNG encoding per format; native tests retain their full matrices.

The sender metadata lease test uses an expired contended deadline under Miri
because the pinned parking_lot_core futex arguments are rejected by its syscall
shim ([upstream issue](https://github.com/Amanieu/parking_lot/issues/542)). It
still checks cross-thread lease release, contended refusal and allocation-free
reacquisition through the real gate. Native runs check the positive wait budget.

Run a focused case locally with:

```sh
cargo +nightly-2026-11-10 miri test --locked --lib receiver::mixed_tests
MIRIFLAGS="-Zmiri-tree-borrows" cargo +nightly-2026-11-10 miri test --locked --lib receiver::mixed_tests
```

The Ubuntu `test` job runs the scripted capture fixtures and full encoder padding
matrix under Valgrind Memcheck. It traces child test processes, skips compiler
children and fails on errors. The Standard 6.3.2 Linux SDK row also checks live
padding through the loopback runner. Install Valgrind before running either recipe.

SDK-free capture and encoder padding:

```sh
runner='target.x86_64-unknown-linux-gnu.runner=["valgrind","--tool=memcheck","--error-exitcode=99","--trace-children=yes","--trace-children-skip=*/rustc,*/rustc-*"]'
cargo test --locked --no-fail-fast --all-features --config "$runner" --test mixed_capture_boundary --test frame_metadata_boundary -- --test-threads=1
memcheck_dir="${RUNNER_TEMP:-target/ci}"
mkdir -p "$memcheck_dir"
inventory="$memcheck_dir/ndi-encoder-padding-tests.txt"
selection="$memcheck_dir/ndi-encoder-padding-selected.txt"
cargo test --locked --no-fail-fast --all-features --lib -- --list > "$inventory"
python3 ci/sdk_test_scope.py select --group encoder-padding --inventory "$inventory" --output "$selection"
mapfile -t tests < "$selection"
cargo test --locked --no-fail-fast --all-features --config "$runner" --lib -- --exact "${tests[@]}" --test-threads=1
```

Live SDK padding on Linux x64:

```sh
memcheck_dir="${RUNNER_TEMP:-target/ci}"
mkdir -p "$memcheck_dir"
inventory="$memcheck_dir/ndi-sdk-padding-tests.txt"
selection="$memcheck_dir/ndi-sdk-padding-selected.txt"
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo test --locked --config ci/offline.toml --target x86_64-unknown-linux-gnu --no-fail-fast --features image-encoding --test video_padding_sdk -- --list > "$inventory"
python3 ci/sdk_test_scope.py select --group sdk-padding --inventory "$inventory" --output "$selection"
mapfile -t tests < "$selection"
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo test --locked --config ci/offline.toml --config 'target.x86_64-unknown-linux-gnu.runner=["ci/offline/linux.sh","valgrind","--tool=memcheck","--track-origins=yes","--error-exitcode=99"]' --target x86_64-unknown-linux-gnu --no-fail-fast --features image-encoding --test video_padding_sdk -- --exact "${tests[@]}" --test-threads=1
```

On Debian 12 with Valgrind 3.19 and glibc 2.36, the live SDK recipe may report
reads from glibc's loader token comparisons. Add
`"--suppressions=ci/valgrind.supp"` to its runner array on that host.

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

CI generates the synthetic H.264 and HEVC pictures in a separate job and
passes the downloaded directory to the Advanced rows as
`GRAFTON_NDI_HX_PICTURES`. Each row logs the SHA-256 of both files.
The Windows Advanced rows also log the version of the hash-verified libclang
DLL from the pinned LLVM release.

For local native NDI|HX tests, leave `GRAFTON_NDI_HX_PICTURES` unset to
generate the pictures on demand with Python 3 and FFmpeg's `libx264` and
`libx265` encoders. To generate the pictures once for reuse:

```sh
python3 tests/support/generate_hx.py --output-dir target/test-data/ndi-hx
export GRAFTON_NDI_HX_PICTURES="$PWD/target/test-data/ndi-hx"
```

When you set the variable, both files must already be in that directory.

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

Configure the repository before publishing. The release job verifies the first
three controls through read-only GitHub API requests. Verify items 4 and 5
before pushing a release tag. [Release check](release.py)

1. The `crates-io` environment requires reviewers and has exactly one custom
   deployment policy for `v*` tags.
2. An active tag ruleset includes `refs/tags/v*` without exclusions and
   restricts creating, updating and deleting those tags.
3. Active rules on `main` require pull requests, Code Owner review and
   the `required-ci` status check. [CODEOWNERS](../.github/CODEOWNERS)
4. The crates.io trusted publisher binds this repository, `publish.yml` and the
   `crates-io` environment. The GitHub API cannot read crates.io settings.
5. SDK CI has `NDI_SDK_SOURCES_JSON`, a scoped `NDI_SDK_ARCHIVE_TOKEN`, and
   `NDI_CI_RELEASED=true` when enabled. The release check has no access to
   secret values.

Anyone who can replace a workflow on a tag or branch can replace its checks.
Repository rules and review enforce these boundaries. The release job catches
configuration mistakes before the jobs that build or publish the release.
