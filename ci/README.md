# Continuous integration

[`rust.yml`](../.github/workflows/rust.yml) checks pushes and pull requests to
`main`, manual runs and calls from the release workflow.
[`publish.yml`](../.github/workflows/publish.yml) handles `v*` tags. These files
state configured checks; a successful run must be established from its logs.
[`docs.yml`](../.github/workflows/docs.yml) builds and publishes the API reference
on pushes to `main` and manual runs on `main`.
For user platform selection, see [setup](../docs/setup.md#platforms).

## Jobs in `rust.yml`

| Job | Configured checks |
| --- | --- |
| `lint` | Python workflow/script tests, shellcheck, actionlint, formatting, clippy for the workspace and feature combinations, bindings-generator tests, rustdoc, Rust 1.88, packaged build, semver and dependency advisories |
| `test` | All features, default features and `file-io` alone on Linux, macOS and Windows, without provisioning an SDK |
| `cross` | Clippy for the dynamic targets named in the workflow, plus linking Android examples |
| `sdk` | Pinned native runtimes, required prerequisites, Advanced declarations and compatibility checks below |
| `required-ci` | [`gate.py`](gate.py) requires the appropriate jobs to succeed |

Require `required-ci` in branch protection. Fork pull requests and Dependabot
runs receive no SDK secrets: their `sdk` job must skip. Other events require it
to succeed, even if `NDI_CI_RELEASED` is unset and the job skips. New pull-request
pushes cancel older checks; main, manual and release runs are not cancelled.
The workflows use read-only default token permissions and pinned actions.

[`semver.py`](semver.py) compares against the newest crates.io release, skipping
while this crate's major version is ahead of all published versions.

## The `sdk` job

This seven-column matrix is checked against the workflow by
[`tests/test_workflows.py`](tests/test_workflows.py). A row defines the package,
not proof of a completed qualification run.

| Row | Runner | Target | Package | Promise, all features | Promise, no default features | Also runs |
|---|---|---|---|---|---|---|
| linux-x64 standard-6.1.1 | ubuntu-24.04 | x86_64-unknown-linux-gnu | 6.1.1-standard-linux | `1` | `1` | |
| linux-x64 standard-6.3.2 | ubuntu-24.04 | x86_64-unknown-linux-gnu | 6.3.2-standard-linux | `1` | `1` | |
| linux-x64 advanced-6.3.2 | ubuntu-24.04 | x86_64-unknown-linux-gnu | 6.3.2-advanced-linux | `advanced` | `1` | Advanced lint |
| linux-x64 standard-6.4 | ubuntu-24.04 | x86_64-unknown-linux-gnu | current-standard-linux | `1,discovery-server` | `1,discovery-server` | vendored bindings check |
| linux-x64 advanced-6.4 | ubuntu-24.04 | x86_64-unknown-linux-gnu | current-advanced-linux | `advanced,file,discovery-server,h264-decoder,hevc-444-decoder` | `1,discovery-server` | Advanced lint |
| linux-x86 standard-6.4 | ubuntu-24.04 | i686-unknown-linux-gnu | current-standard-linux | `1,discovery-server` | `1,discovery-server` | |
| linux-arm64 standard-6.4 | ubuntu-24.04-arm | aarch64-unknown-linux-gnu | current-standard-linux | `1,discovery-server` | `1,discovery-server` | |
| windows-x64 standard-6.3.2 | windows-2025 | x86_64-pc-windows-msvc | 6.3.2-standard-windows | `1` | `1` | |
| windows-x64 advanced-6.3.2 | windows-2025 | x86_64-pc-windows-msvc | 6.3.2-advanced-windows | `advanced,h264-decoder` | `1` | Advanced lint |
| windows-x64 standard-6.4 | windows-2025 | x86_64-pc-windows-msvc | current-standard-windows | `1,file` | `1` | |
| windows-x64 advanced-6.4 | windows-2025 | x86_64-pc-windows-msvc | current-advanced-windows | `advanced,file,h264-decoder` | `1` | Advanced lint |
| windows-x86 advanced-6.4 | windows-2025 | i686-pc-windows-msvc | current-advanced-windows | `advanced,h264-decoder` | `1` | Advanced lint |
| macos-arm64 standard-6.3.2 | macos-15 | aarch64-apple-darwin | 6.3.2-standard-apple | `1` | `1` | |
| macos-arm64 advanced-6.3.2 | macos-15 | aarch64-apple-darwin | 6.3.2-advanced-apple | `advanced,h264-decoder` | `1` | Advanced lint |
| macos-arm64 standard-6.4 | macos-15 | aarch64-apple-darwin | current-standard-apple | `1` | `1` | Apple mobile builds |
| macos-arm64 advanced-6.4 | macos-15 | aarch64-apple-darwin | current-advanced-apple | `advanced,file,h264-decoder,hevc-444-decoder` | `1` | Advanced lint, Apple mobile builds |
| macos-x64 standard-6.4 | macos-15-intel | x86_64-apple-darwin | current-standard-apple | `1` | `1` | |

Each row provisions its SDK, checks the test runner, then runs all-feature and
no-default-feature suites with `--locked --no-fail-fast`. Test steps have their
own deadlines and continue after earlier test failures once isolation passed.
Advanced rows also lint/document the implementation compiled from installed
headers; without those headers, the feature's unavailable implementations would
be checked instead. The extra steps check vendored bindings or build the static
Apple targets through [`apple-mobile.sh`](apple-mobile.sh).

The current Advanced Linux x64, Windows x64 and macOS ARM64 rows also cross
current headers with baseline runtimes and baseline headers with the current
runtime. They use Standard 6.1.1 on Linux, Standard 6.3.2 elsewhere, and Advanced
6.3.2. [`tests/sdk_peers.rs`](../tests/sdk_peers.rs) starts a peer on the other
runtime and asserts video, audio, timecodes and metadata. Compatibility runs do
not promise decoders; decode tests can skip there. The job has no dependency
cache or artifact uploads: SDK files and generated Advanced bindings stay on
the runner.

### What a row promises

`GRAFTON_NDI_REQUIRE_RUNTIME` is a comma-separated list. A missing prerequisite
prints a skip unless promised, in which case it fails. Unknown tokens and promises
that the build cannot check fail too. See
[`policy.rs`](../src/test_support/policy.rs) and
[`test_support.rs`](../src/test_support.rs).

| Token | Required prerequisite |
| --- | --- |
| `1` | NDI runtime; implied by every other token |
| `advanced` | Advanced runtime and compiled Advanced declarations |
| `file` | Usable separate File library, with `file-io` enabled |
| `discovery-server` | SDK Discovery Server program and compatible runtime, on Linux |
| `h264-decoder` | Runtime can decode the H.264 fixture, with `advanced` enabled |
| `hevc-444-decoder` | Runtime can decode the HEVC 4:4:4 fixture, with `advanced` enabled |

Promise only prerequisites the build and installation can exercise. The workflow
guards compare these promises with package pins and features. Decoder detection
and placeholder rejection are test policy, not a guarantee for hardware codecs
or third-party sources.

## SDK packages

[`sdk-packages.json`](sdk-packages.json) pins size and SHA-256;
[`sdk.py`](sdk.py) stages and verifies each archive before extraction:

```sh
python ci/sdk.py list
python ci/sdk.py install current-standard-linux --dest ~/ndi/standard-6.4 \
    --archive ~/Downloads/Install_NDI_SDK_v6_Linux.tar.gz --target x86_64-unknown-linux-gnu
```

The destination must not exist. Linux/Android payloads are extracted without
running their installer script; macOS uses `pkgutil --expand-full`, and Windows
runs the vendor installer silently. Apple extraction needs macOS; Windows
extraction needs Windows. Tar members and resulting links are checked against
destination escape.

The script prints `NDI_SDK_DIR`; with `--target`, it also prints
`NDI_RUNTIME_DIR_V6` and `GRAFTON_NDI_EXPECTED_RUNTIME`,
`GRAFTON_NDI_EXPECTED_VERSION`, `GRAFTON_NDI_EXPECTED_EDITION`. The test helpers
compare those identity values against the loaded module. `--github-env FILE`
and `--github-output FILE` write exports and step outputs. Unset stale expected
identity values before switching runtimes.

Without `--archive`, acquisition uses `NDI_SDK_SOURCES_JSON`:

```json
{"schema": 1, "packages": {"current-standard-linux": {"github_release_asset": {"repo": "owner/private-store", "asset_id": 1}}}}
```

Each entry chooses `github_release_asset`, `url_env` (an environment variable
containing an HTTPS URL) or `archive` (an absolute local path). Key it by package
ID or content-qualified name `ndi-<version>-<edition>-<platform>-<sha256>`,
exactly one of the two. Drivers use `hx-driver` in place of edition. Include
baseline and driver packages referenced by the matrix. GitHub assets use
`NDI_SDK_ARCHIVE_TOKEN`, or the entry's `token_env`; authorization is sent only
to `api.github.com`. Error handling omits source-map fields and tool arguments;
vendor-tool environments exclude acquisition credentials. Review these paths
when changing provisioning.

### The NDI|HX driver

Linux rows can install the separately pinned driver beside a matching runtime:

```sh
python ci/sdk.py install-driver current-hx-driver-linux --sdk ~/ndi/advanced-6.4 \
    --archive ~/Downloads/NDIHXDriverForLinux.tar.gz --target x86_64-unknown-linux-gnu
```

The script verifies the archive, requires the SDK release and vendor build to
match, and copies only `libndihx.so` and `lib*-ndi.so.*` for the target. It
refuses existing destination files and removes its copies on failure. Keep
vendor archives and headers outside the repository; consult their bundled terms
for redistribution and codec licensing.

### Who can reach the secrets

The entire SDK job is the trust boundary. Although only provisioning receives
the secrets as environment variables, subsequent repository code, dependencies
and tools execute on the same machine. Anyone able to push repository branches
can change the workflow run by those branches. Limit the archive token to
read-only access, give it an expiry, and rotate it. Restricting secrets to an
environment changes which events can run the job; update its condition and the
gate expectation together.

## Loopback-only test runs

[`offline.toml`](offline.toml) selects wrappers for Linux and macOS. Use it for
native tests, which create network senders and finders. It does not confine
Cargo's build processes or the filesystem.

| Platform | Wrapper and limits |
| --- | --- |
| Linux | [`linux.sh`](offline/linux.sh) uses `bubblewrap` with a private network namespace containing only loopback, hides host `/run` sockets, maps the hostname to loopback and gives tests a private `TMPDIR`. Requires bubblewrap and unprivileged user namespaces. |
| macOS | [`macos.sh`](offline/macos.sh) uses Seatbelt to restrict outbound traffic to loopback, denies daemon sockets, configures loopback discovery and supplies private temporary state. A wildcard listener can still receive external traffic: this is not a packet filter. |
| Windows | No wrapper; tests run on the machine's network. |

Both wrappers set `CARGO_NET_OFFLINE` for nested Cargo calls. The SDK job runs
[`offline/check.py`](offline/check.py) outside and inside them to check loopback
TCP/UDP and refused external traffic. Its explicit macOS multicast-control
exception is reported as a limitation; the check does not prove isolation beyond
the operations it exercises.

### Windows

Runtime tests are unconfined. Connection diagnostics in relevant regressions
include the Windows TCP table through `tests/common/socket.rs`. Review skipped
tests and qualify native behavior on the intended network and hardware rather
than assuming another platform's results transfer.

<a id="checks-before-review"></a>

## Running the checks locally

Run the standard checks below. CI scripts need Python 3.11 or later (`tomllib`):

```sh
python -m unittest discover -s ci/tests
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
python ci/docs.py --compile --rustdoc target/doc
cargo build --locked --examples --all-features
cargo test --locked --all-features
```

[Development](../docs/development.md#checks-before-review) describes additional
checks for changes to bindings, public contracts and platform support.

For a Linux native run, install a pinned package above, export its printed
variables, then require the prerequisites being tested:

```sh
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo test --locked --config ci/offline.toml --all-features --no-fail-fast
```

An Advanced build also needs `NDI_SDK_DIR` pointing at its headers and
`libclang`. Replace `1` with only the promises your installation provides, for
example `advanced,file,discovery-server`. Without promises, absent prerequisites
can skip; a green run alone does not establish native coverage.

To require cross-version transport, set `GRAFTON_NDI_PEER_RUNTIME` to a second
runtime's exact library path, `GRAFTON_NDI_PEER_VERSION` to its four-component
version and `GRAFTON_NDI_PEER_EDITION` to `standard` or `advanced`. The peer must
have a different version. Keep `NDI_SDK_DIR` as build-header selection and
`NDI_RUNTIME_DIR_V6`/expected identity values as runtime selection; the workflow
shows both crossed directions. Record results using the
[SDK evidence checklist](../docs/sdk-validation.md#recording-evidence).

## Releasing

Before publishing, configure the controls below. Then merge the version bump
to `main`, date its changelog entry, and verify versioned guide/API links and
the packaged README. Tag that commit `v<version>` and push the tag.

| Job in `publish.yml` | Configured action |
| --- | --- |
| `release` | Check tag equals `v` plus the manifest version and commit belongs to `main` |
| `ci` | Call the complete `rust.yml` workflow |
| `verify` | Build the locked package and report its SHA-256 and toolchain |
| `publish` | In `crates-io`, recreate the package with `--no-verify`, compare its digest, authenticate through trusted publishing and publish |

The publishing job starts Cargo outside the checkout, uses the verified
toolchain, runs no repository scripts and compiles no code. It validates the
digest/toolchain strings before use. Review changes to this boundary against
[`tests/test_workflows.py`](tests/test_workflows.py). Package contents follow
`Cargo.toml`'s `include` list; `docs/` and `ci/` are not included. docs.rs builds
the published crate as a supplementary service; release publication does not
depend on it. The authoritative API reference is hosted on
[GitHub Pages](https://grantsparks.github.io/grafton-ndi/grafton_ndi/).

### API documentation publication

`docs.yml` builds all Cargo features with the lockfile and rustdoc warnings as
errors, then checks guide links, generated API pages/anchors and standalone
Rust examples. The build needs no NDI SDK and receives only read access. A
separate deployment job receives Pages/OIDC permissions and deploys the checked
HTML through the `github-pages` environment. It runs no repository code.

The site root redirects to `grafton_ndi/`, preserving the existing public URL.
The site tracks `main`, including unreleased changes, and rustdoc displays its
crate version. It is not an archive of release-specific API references. For an
older version, check out its tag and generate rustdoc with that version's build
prerequisites. docs.rs remains configured as a secondary host, but failures
there do not prevent repository-owned documentation publication.

In repository **Settings → Pages**, select **GitHub Actions** as the build
source. Configure the `github-pages` environment to allow deployments only
from `main`. The workflow also checks the repository and branch before building;
manual dispatches on other branches cannot publish. This workflow publishes
rustdoc, while the guides remain in the repository.

### Repository settings, in order

The files cannot establish live repository settings. Before the first release,
the owner must verify:

1. **`crates-io` environment:** required reviewers, appropriate self-review rule,
   and deployment restricted to `v*` tags.
2. **Tag ruleset:** restrict creating, moving and deleting `v*` tags.
3. **`main` protection:** require pull requests, Code Owner review and
   `required-ci`. [`CODEOWNERS`](../.github/CODEOWNERS) covers workflows, CI,
   scripts, Cargo configuration/manifests and toolchain selection.
4. **crates.io trusted publisher:** bind this repository, `publish.yml` and the
   `crates-io` environment.
5. **SDK access:** provide `NDI_SDK_SOURCES_JSON`, a scoped
   `NDI_SDK_ARCHIVE_TOKEN`, and `NDI_CI_RELEASED=true` when SDK CI is enabled.

A workflow cannot protect itself from a person able to replace it on a tag or
branch. Repository rules and review enforce these boundaries; the tag-on-main
check catches mistakes.

## Maintained artifacts

Keep `Cargo.lock` for locked checks, SDK compatibility pins for the matrix,
checked-in bindings/declarations for SDK-free builds, compiler-error `.stderr`
fixtures for trybuild, and the small media fixtures with their generator.
Their build/test roles make regeneration alone insufficient reason to remove
them. See [development](../docs/development.md) for implementation contracts.
