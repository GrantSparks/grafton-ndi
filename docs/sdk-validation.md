# SDK integration validation

This document states what was tested for the current version, what was not, and
how to repeat the tests on your own machine. The compatibility target is NDI 6.x
and Rust 1.88.

## What was tested for 3.0.0

The test suite was run on **Linux x86_64** with `cargo test --locked
--all-features` in these environments. Every run against a real runtime was
confined to a network namespace with loopback networking only and had a
private temporary directory.

| Build | Runtime | Required to be present | Passed | Failed | Ignored |
| --- | --- | --- | --- | --- | --- |
| No NDI SDK | None | Nothing | 612 | 0 | 3 |
| Standard 6.4.0.2 headers | Standard 6.4.0.2 | `1,discovery-server` | 612 | 0 | 3 |
| Advanced SDK 6.3.2 headers | Advanced 6.3.2 | `advanced` | 615 | 0 | 3 |
| Advanced SDK 6.4.0.2 headers | Advanced 6.4.0.2 | `advanced,file,discovery-server` | 615 | 0 | 3 |
| Advanced SDK 6.4.0.2 headers | Standard 6.3.2 | `1` | 614 | 0 | 3 |
| Standard 6.3.2 headers | Advanced 6.4.0.2 | `1` | 612 | 0 | 3 |
| Advanced SDK 6.3.2 headers | Advanced 6.4.0.2 | `advanced,file,discovery-server` | 615 | 0 | 3 |
| Advanced SDK 6.4.0.2 headers | Advanced 6.3.2 | `advanced` | 615 | 0 | 3 |

"Required to be present" is the value of `GRAFTON_NDI_REQUIRE_RUNTIME`: what
the environment promised, so that a test could not pass by skipping. In the
first row the tests that need a runtime reported skips. Other rows visibly skip
Advanced or File operations when that build/runtime combination cannot provide
them. The counts include tests that reported such skips; the requirements
prevent skipping the operations the row promises. The three ignored tests
are two manual benchmarks and one slow stress test.

With the Standard 6.3.2 runtime, `cargo test --locked` passed 425 tests with
default features and 399 with `--no-default-features`, with none failed.

The Advanced row covers the Advanced SDK operations, the File library of the
6.4.0.2 SDK, and the advertisers and listeners against the SDK's own Discovery
Server.

The runtimes report themselves as `6.3.2.0` and `6.4.0.2`. With the Standard
6.3.2 runtime these capabilities are unavailable, and the operations that
depend on them return `Error::UnsupportedCapability`:

- `ReceiveDelay` and `SdkTimestamp`, whose functions that runtime does not
  export;
- `Planar444Video`, `ReceiveBest444`, `ReceiveVideoOnly`, and the four
  Discovery Server capabilities, which need NDI 6.4.0.

On the same machine these static checks passed: formatting; clippy with
warnings denied for the workspace with all features, with default features,
with no default features, with `advanced` alone and with `file-io` alone, each
with and without the Advanced SDK headers; rustdoc with warnings denied; the
examples; a `cargo check` with Rust 1.88; `cargo package`; and the check that
the generated bindings match the SDK headers.

## Compatibility safeguards and additional runtime checks

Advanced availability is derived from the declarations generated from the
installed SDK. A missing API family disables only that family; absent types
and newer receive presets do not make older headers fail to compile. A shared
registry supplies both the build selector and the typed runtime tables. Every
Advanced function declared by the installed headers must be classified by the
coverage test. Synthetic partial headers exercise the actual build, including
capability reporting, with warnings denied. They check fallback behavior and
are not evidence of a run against a historical vendor SDK.

Additional Linux x86_64 tests against 6.4 check a real lossless HEVC 4:4:4 frame,
including every sample of its three full-resolution planes; audio suppression
for both video-only bandwidth settings with an ordinary audio receiver as a
control; AVSync following changes in video size, frame rate, audio sample rate
and channel count while preserving nonzero samples; and both Genlock clocks
following a local source and falling back to paced system clocks. The KVM
boundary test checks failed release, retry on drop, touch coordinates and
clipboard ownership using stand-in functions; it does not qualify a KVM device.

The recorder's explicit `reset` operation replaces its native instance after
abandoning an unconfirmed session. It returns the old report without claiming
finalization, isolates late events from later sessions, and leaves the original
recorder usable if replacement creation fails.

Two File API entry points remain explicit safety exceptions:
`NDIlib_file_recorder_get_source` and
`NDIlib_file_recorder_get_current_file`. They return borrowed strings that the
SDK can invalidate on its own recording thread during stopping or rollover.
Copying immediately does not establish a safe lifetime. The Rust API provides
`last_accepted_source` and `reported_current_file` from owned state and events,
and documents the distinction from an instantaneous native query. Complete C
entry-point coverage is not claimed. These exceptions can be removed when the
vendor provides a stable lifetime or a caller-owned copy operation; a Rust
mutex around the getters alone cannot protect against the SDK's thread.

SDK provisioning exports the expected normalized version, edition and exact
library path. Tests compare these with the runtime loaded in their own process;
version, edition and path mismatch fixtures must fail. Archive provenance alone
cannot satisfy that check. Hosted CI includes native Standard and Advanced 6.3.2 baselines, and both
directions of each 6.3.2/current Advanced header/runtime combination on Linux,
Windows and macOS.

The historical compatibility target remains NDI 6.x, including 6.0 and the
previously measured 6.1.1; these are qualification gaps, not a raised minimum.
The retained vendor packages cover Standard and Advanced 6.3.2 and 6.4.0.2 on
Linux, Windows, Apple and Android. The recovered Advanced 6.3.2 Linux headers
and runtime passed the full suite; the other recovered packages were checked
for version and archive identity, not executed on this machine.
A historical download link recovered during this review served the exact pinned
6.3.2 archive, demonstrating why a link's age cannot establish a version. Older
vendor archives must be recovered, identified from their contents, pinned in
`ci/sdk-packages.json` and added to the native CI matrix before those versions
can be called qualified. Advanced runtimes older than 6.3.2 remain unqualified.

## What was compiled but not run

The crate was compiled and linted, with warnings denied, for these targets. No
test was run on them, and no NDI library was loaded on them:

- `x86_64-pc-windows-msvc`, `i686-pc-windows-msvc`
- `x86_64-apple-darwin`, `aarch64-apple-darwin`
- `aarch64-linux-android`
- `i686-unknown-linux-gnu`

`aarch64-apple-ios` and `x86_64-apple-ios` were compiled and linted against the
NDI SDK for Apple 6.4. Their static link was not performed and nothing was run.

## Hosted CI

The workflows in `.github/workflows/` have not yet run on a hosted runner.
Everything above was measured on one Linux x86_64 machine. The macOS and
Windows jobs, the other Linux architectures and the release path are unproven
until the first hosted run. The [CI guide](../ci/README.md) describes the
workflows and keeps the
[list of what is unproven](../ci/README.md#not-yet-run-on-a-hosted-runner).

## What was not tested

- Any behavior on Windows, macOS, Android, iOS, tvOS or visionOS. This includes
  the Windows and macOS branches of the runtime search and the static linking
  of Apple mobile targets, which are covered by compilation only.
- NDI runtimes other than 6.3.2 and 6.4.0.2. Versions 6.0 and 6.1.1 in
  particular were not tested.
- The File library on Windows and macOS.
- Linux architectures other than x86_64.
- Hardware-dependent behavior: PTZ devices, KVM devices, hardware codecs, and
  interoperability of compressed streams with other products.
- License entitlements. A capability reported as available says only that the
  runtime exports the functions and has the required version.

## Repeating the tests

The commands below are for Linux x86_64. Replace the paths with those of your
installation. The fake-runtime tests need `rustc` and `readelf` on the `PATH`.

Without any NDI installation:

```bash
cargo test --locked --all-features
```

With a Standard SDK installed:

```bash
export NDI_RUNTIME_DIR_V6="/path/to/NDI SDK for Linux/lib/x86_64-linux-gnu"
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo test --locked --config ci/offline.toml --all-features
```

With an Advanced SDK installed:

```bash
export NDI_SDK_DIR="/path/to/NDI Advanced SDK for Linux"
export NDI_RUNTIME_DIR_V6="$NDI_SDK_DIR/lib/x86_64-linux-gnu"
GRAFTON_NDI_REQUIRE_RUNTIME=advanced,file,discovery-server cargo test --locked --config ci/offline.toml --all-features
```

`--config ci/offline.toml` starts every test executable with loopback
networking only and a private temporary directory, which on Linux needs
`bubblewrap`. The tests that use a real runtime create NDI senders, receivers
and finders. Without that option they announce themselves on the local
network. The [CI guide](../ci/README.md#loopback-only-test-runs) describes the
runner and why the tests need a private temporary directory.

To check that the crate still compiles for a target you cannot run:

```bash
rustup target add x86_64-pc-windows-msvc
cargo clippy --all-features --all-targets --target x86_64-pc-windows-msvc -- -D warnings
```

iOS, tvOS and visionOS targets need the NDI SDK for Apple at build time:

```bash
export NDI_SDK_DIR="/path/to/NDI SDK for Apple"
cargo check --all-features --target aarch64-apple-ios
```

The static checks, and the check that the generated bindings match an SDK, are
listed in [CONTRIBUTING.md](../CONTRIBUTING.md#checks-before-review).
[`ci/sdk.py`](../ci/README.md#sdk-packages) installs an SDK package into a
directory of your choice.

## Recording a validation

When you test a new SDK version or platform, add it to the tables above with
the runtime's own version text (`ndi.info().raw_version()`), the build inputs,
and what `GRAFTON_NDI_REQUIRE_RUNTIME` promised. Record what was run, not what
was configured: a platform that was only compiled belongs under "compiled but
not run".
