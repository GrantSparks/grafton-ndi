# Continuous integration

Two workflows: [`rust.yml`](../.github/workflows/rust.yml) checks every push to
`main` and every pull request, and [`publish.yml`](../.github/workflows/publish.yml)
releases a version tag. Everything here is plain YAML, a few shell scripts and
standard-library Python, tested by `python -m unittest discover -s ci/tests`.

The crate builds without an NDI SDK, so most of CI needs no secrets. Only the
`sdk` job downloads the vendor's SDK packages, to run the tests against real
runtimes.

## Jobs in `rust.yml`

| Job | Runs for | What it checks |
|---|---|---|
| `lint` | every event | CI script tests, shellcheck, actionlint, rustfmt, clippy (`-D warnings`) for the workspace with all features and for the crate with no features and with each of `image-encoding`, `tokio`, `smol`, `advanced`, `file-io`, `advanced,file-io`; the bindings generator's tests; rustdoc (`-D warnings`); `cargo check` on Rust 1.88; `cargo package`; `cargo audit` |
| `test` | every event | `cargo test` with all features and with default features on Linux, macOS and Windows. No NDI runtime is installed, so tests that need one print `skipped: <test> needs …` |
| `cross` | every event | clippy (`-D warnings`, all features, all targets) for every dynamically loaded target: Linux aarch64, armv7 and i686; Android aarch64, armv7, i686 and x86_64; Windows x86_64 and i686; macOS aarch64 and x86_64. Then an Android link of the examples with the runner's NDK ([`scripts/build-android.sh`](../scripts/build-android.sh)) |
| `sdk` | events that can read the secrets | the tests against real runtimes; see below |
| `required-ci` | every event, `if: always()` | passes only if every job the event requires succeeded ([`gate.py`](gate.py)) |

`required-ci` is the one check branch protection should require. For pushes,
manual runs, releases and pull requests from this repository, every job must
succeed, and a skipped `sdk` job is a failure. A pull request from a fork cannot
read the secrets: there the `sdk` job is skipped and the gate passes on `lint`,
`test` and `cross`.

A new push to a pull request cancels its running check. Runs for `main`, manual
runs and releases are never cancelled. Every job has a timeout, the default
token permission is `contents: read`, and every action is pinned to a commit.

## The `sdk` job

The job runs once per row of this matrix. The table is checked against the
workflow by the script tests.

| Row | Runner | Target | Package | Promise, all features | Promise, no default features | Also runs |
|---|---|---|---|---|---|---|
| linux-x64 standard-6.3.2 | ubuntu-24.04 | x86_64-unknown-linux-gnu | 6.3.2-standard-linux | `1` | `1` | semver check |
| linux-x64 standard-6.4 | ubuntu-24.04 | x86_64-unknown-linux-gnu | current-standard-linux | `1,discovery-server` | `1,discovery-server` | vendored bindings check |
| linux-x64 advanced-6.4 | ubuntu-24.04 | x86_64-unknown-linux-gnu | current-advanced-linux | `advanced,file,discovery-server` | `1,discovery-server` | Advanced lint |
| linux-x86 standard-6.4 | ubuntu-24.04 | i686-unknown-linux-gnu | current-standard-linux | `1,discovery-server` | `1,discovery-server` | |
| linux-arm64 standard-6.4 | ubuntu-24.04-arm | aarch64-unknown-linux-gnu | current-standard-linux | `1,discovery-server` | `1,discovery-server` | |
| windows-x64 standard-6.3.2 | windows-2025 | x86_64-pc-windows-msvc | 6.3.2-standard-windows | `1` | `1` | |
| windows-x64 standard-6.4 | windows-2025 | x86_64-pc-windows-msvc | current-standard-windows | `1,file` | `1` | |
| windows-x64 advanced-6.4 | windows-2025 | x86_64-pc-windows-msvc | current-advanced-windows | `advanced,file` | `1` | Advanced lint |
| windows-x86 advanced-6.4 | windows-2025 | i686-pc-windows-msvc | current-advanced-windows | `advanced` | `1` | Advanced lint |
| macos-arm64 standard-6.3.2 | macos-15 | aarch64-apple-darwin | 6.3.2-standard-apple | `1` | `1` | |
| macos-arm64 standard-6.4 | macos-15 | aarch64-apple-darwin | current-standard-apple | `1` | `1` | Apple mobile builds |
| macos-arm64 advanced-6.4 | macos-15 | aarch64-apple-darwin | current-advanced-apple | `advanced,file` | `1` | Advanced lint, Apple mobile builds |
| macos-x64 standard-6.4 | macos-15-intel | x86_64-apple-darwin | current-standard-apple | `1` | `1` | |

Each row:

1. installs what the row needs beyond the runner image: `bubblewrap`,
   `libavahi-client3` and `libclang-dev` on Linux (plus the i386 linker and
   libraries for the x86 row), LLVM on Windows;
2. provisions the package with [`sdk.py`](sdk.py), which exports `NDI_SDK_DIR`
   and `NDI_RUNTIME_DIR_V6` to the following steps. This is the only step that
   sees the secrets;
3. proves that the test runner isolates ([`offline/check.py`](offline/check.py)
   outside and inside it);
4. runs `cargo test --locked --config ci/offline.toml --target <target>` twice,
   with `--all-features` and with `--no-default-features`;
5. runs the row's extra steps:
   - **vendored bindings check**: `cargo xtask bindgen --check --sdk "$NDI_SDK_DIR"`;
   - **semver check**: `cargo semver-checks` against the last published release,
     with no release-type override and no lint configuration. It lives in an
     SDK row because the published baseline needs an installed SDK to build;
   - **Advanced lint**: clippy (`-D warnings`) with all features, with
     `advanced` and with `advanced,file-io`, and rustdoc (`-D warnings`).
     Without the Advanced headers the `advanced` feature compiles to stubs, so
     these rows are the only place the real implementation is linted;
   - **Apple mobile builds**: [`apple-mobile.sh`](apple-mobile.sh) builds the
     library for iOS, tvOS and visionOS, the statically linked targets that
     need the SDK at build time.

The job uses no dependency cache and uploads nothing, so SDK files and the
Advanced bindings generated into the target directory never leave the runner.

### What a row promises

A test that needs native code runs where that code is available and prints a
skip elsewhere. `GRAFTON_NDI_REQUIRE_RUNTIME` lists what the environment
provides, which turns the matching skips into failures, so a provisioning
mistake cannot pass as a skip:

| Entry | The row provides | A build can check it when |
|---|---|---|
| `1` | an NDI runtime (implied by every other entry) | always |
| `advanced` | an Advanced runtime, compiled with its headers | the `advanced` feature is on |
| `file` | a File library the selected runtime can drive: an Advanced runtime on Linux and macOS, any 6.4 runtime on 64-bit Windows | the `file-io` feature is on |
| `discovery-server` | the SDK's Discovery Server program and a 6.4 runtime, whose advertisers and listeners can use it | the target is Linux |

A promise that no test in a build can check is itself a failure, and a missing
one lets a test skip, so each step promises exactly what its build can check:
the all-features step everything the row's package provides, the
no-default-features step only `1` and, on Linux with a 6.4 package,
`discovery-server`. The script tests hold every row to this rule.

## SDK packages

[`sdk-packages.json`](sdk-packages.json) pins each SDK package by size and
SHA-256. [`sdk.py`](sdk.py) installs one:

```sh
python ci/sdk.py list
python ci/sdk.py install current-standard-linux --dest ~/ndi/standard-6.4 \
    --archive ~/Downloads/Install_NDI_SDK_v6_Linux.tar.gz --target x86_64-unknown-linux-gnu
```

It checks the archive against the pin before opening it, unpacks it without
running the vendor's installer script (the Windows installer is the exception:
it is the package), and prints the `NDI_SDK_DIR` and `NDI_RUNTIME_DIR_V6` values
that select the installation. Apple packages unpack on macOS only and Windows
packages on Windows only.

Without `--archive`, the package comes from the source map in
`NDI_SDK_SOURCES_JSON`:

```json
{"schema": 1, "packages": {"current-standard-linux": {"github_release_asset": {"repo": "owner/private-store", "asset_id": 1}}}}
```

An entry is one of `github_release_asset` (read with the token in
`NDI_SDK_ARCHIVE_TOKEN`, or in the variable named by `token_env`), `url_env`
(the name of a variable holding an HTTPS URL) or `archive` (an absolute local
path). The token is sent to `api.github.com` only, a transient HTTP failure is
retried twice, and script output never includes a source location.

## Loopback-only test runs

Tests against a real runtime create NDI senders and finders. `ci/offline.toml`
makes Cargo start every test executable through a wrapper in
[`offline/`](offline) that leaves it loopback networking only, so nothing is
announced on, or reached over, the network the machine is attached to.

| | Linux ([`linux.sh`](offline/linux.sh)) | macOS ([`macos.sh`](offline/macos.sh)) | Windows ([`windows.ps1`](offline/windows.ps1)) |
|---|---|---|---|
| Network | own network namespace (`bwrap --unshare-net`) holding only `lo` | Seatbelt profile allowing only binding, loopback connections and traffic arriving on a local address | Defender Firewall block rules for the test executable, both directions; the firewall never filters loopback |
| Host daemons | `/run` replaced by an empty tmpfs: no D-Bus, no Avahi | mDNSResponder socket denied; the SDK is given a loopback discovery address through `NDI_CONFIG_DIR` | untouched |
| Machine's own name | resolves to `127.0.0.1` through a private `/etc/hosts` | cannot be relied on to resolve | resolves as usual |
| `TMPDIR` | private tmpfs (`/run/tmp`) | private directory, removed afterwards | untouched |
| Needs | `bubblewrap`, unprivileged user namespaces | nothing | an elevated shell |

Two of these exist because of how the SDK behaves:

- **The machine's own name.** A sender advertises itself as `<host name>
  (<sender name>)`, and looking a source up by host name asks the SDK to resolve
  that name. Only loopback exists in the namespace, so the name has to resolve
  to `127.0.0.1`: with the machine's usual address, or with no answer, the
  lookup fails with `NoSourcesFound { criteria: "host: …" }`. The wrapper
  therefore binds a private `/etc/hosts` that maps the name, and the name with
  a trailing dot, to loopback. One test depends on this,
  `tests/video_padding_sdk.rs`, and it is compiled for Linux only; every other
  test addresses its sender as `127.0.0.1:<port>`. The macOS and Windows
  wrappers therefore do nothing about name resolution. A test that looks
  sources up by host name on macOS would need more than that wrapper offers.
- **`TMPDIR`.** The SDK reserves every sender and receiver name machine-wide
  with a lock file, `$TMPDIR/NTK_SEND_<sender name>`. In a shared directory two
  unrelated processes that use the same sender name fail each other with
  "Failed to create NDI send instance", and the files are left behind. This was
  observed on Linux; the macOS wrapper isolates `TMPDIR` as a precaution and
  the Windows wrapper leaves it alone.

What the macOS wrapper does not do: Seatbelt filters the test process only. A
listener bound to a wildcard address stays reachable from the runner's own
network, because the profile language cannot restrict the peer of inbound
traffic.

On Linux the same run works locally:

```sh
export NDI_RUNTIME_DIR_V6="$HOME/ndi/standard-6.4/NDI SDK for Linux/lib/x86_64-linux-gnu"
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo test --config ci/offline.toml --all-features
```

## Releasing

1. Merge the version bump to `main`.
2. Tag that commit `v<version>` and push the tag.

`publish.yml` runs for `v*` tags only, in four jobs that each need the one
before:

| Job | What it does |
|---|---|
| `release` | the tag is `v` plus the `Cargo.toml` version ([`release.py`](release.py)) and its commit is on `main` |
| `ci` | the whole of `rust.yml`, including `required-ci` |
| `verify` | `cargo package --locked`, which builds the packaged crate, then uploads the `.crate`, its SHA-256 and the toolchain version. No registry access |
| `publish` | in the `crates-io` environment with `id-token: write`: re-creates the package with `--no-verify` on the same toolchain, requires its SHA-256 to equal the verified one, and runs `cargo publish --locked --no-verify` through crates.io trusted publishing |

No crate or dependency is compiled in the job that can publish. docs.rs builds
the documentation from the published crate; nothing is deployed to GitHub Pages.

## Secrets, variables and repository settings

Used as they exist today:

| Setting | Purpose |
|---|---|
| Secret `NDI_SDK_SOURCES_JSON` | source map for the private SDK archives |
| Secret `NDI_SDK_ARCHIVE_TOKEN` | read-only token for the archive repository |
| Variable `NDI_CI_RELEASED` = `true` | switches the `sdk` job on. With any other value the job is skipped, which fails `required-ci` for everything except fork pull requests |

To be created by the repository owner:

| Setting | Purpose |
|---|---|
| Environment `crates-io`, with required reviewers | approval gate for the `publish` job |
| crates.io trusted publisher for the crate | repository `GrantSparks/grafton-ndi`, workflow `publish.yml`, environment `crates-io` |
| Branch protection for `main` | require the `required-ci` check |

No longer read by the workflows: the per-package `NDI_SDK_*_URL` secrets and
`NDI_SDK_*_SHA256` variables. Nothing deploys to the `github-pages` environment
any more.

## Running the checks locally

```sh
python -m unittest discover -s ci/tests        # CI scripts and workflow guards
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
cargo test --locked --all-features             # runtime tests skip without an NDI runtime
```

To run the runtime tests, install a package with `ci/sdk.py`, export the printed
`NDI_RUNTIME_DIR_V6`, and use the loopback-only command above. For the Advanced
implementation also export the printed `NDI_SDK_DIR` of an Advanced package and
have libclang installed.

## Files

| File | Purpose |
|---|---|
| `sdk.py`, `sdk-packages.json` | verify and install a pinned SDK package |
| `offline.toml`, `offline/` | loopback-only test runner per operating system, and the check that it isolates |
| `gate.py` | the `required-ci` decision |
| `release.py` | the release tag check |
| `apple-mobile.sh` | builds for the statically linked Apple targets |
| `tests/` | tests for the scripts, and guards on the workflows that actionlint does not cover |

## Not yet run on a hosted runner

These workflows have not run on GitHub. On Linux x86_64 the commands of `lint`,
`test` and `cross`, the three Linux x64 `sdk` rows and SDK provisioning from
local archives have been run by hand; the rest is unproven until the first
hosted run. Remove an item from this section once a hosted run has exercised it.

Carried over from the previous CI, which ran on hosted runners:

- the source-map schema, the GitHub asset request and its headers (the code
  around them is rewritten);
- the installer invocations: `pkgutil --expand-full` on macOS and the silent
  Windows installer flags;
- the Seatbelt policy and the loopback discovery configuration;
- the Windows firewall cmdlets;
- the Linux network namespace with a private `/etc/hosts` (its other `bwrap`
  options differ), the user-namespace `sysctl` and the i386 packages;
- the Windows LLVM installation, the Apple mobile deployment targets and SDK
  roots, and the nightly pin for the tvOS simulator;
- the crates.io authentication action.

New, and so unverified anywhere but Linux x86_64:

- every macOS and Windows step as a whole, including the wrappers used as Cargo
  runners (`pwsh -File` on Windows, `sandbox-exec` without a packet filter on
  macOS) and the build against Xcode's libclang;
- every promise of the Windows and macOS rows, in particular `file` beside a
  Standard runtime in the Windows x64 Standard 6.4 row;
- the Advanced lint on Windows and macOS;
- the Linux x86 and Linux arm64 rows;
- building tvOS and visionOS on the stable toolchain;
- the Android example link in `cross`;
- access to the secrets and the download from the private store;
- the release path: the `crates-io` environment, trusted publishing and the
  package reproduction across the `verify` and `publish` jobs.
