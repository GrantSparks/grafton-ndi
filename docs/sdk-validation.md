# SDK integration validation

This document states what was tested for the current version, what was not, and
how to repeat the tests on your own machine. The
[README](../README.md#supported-ndi-versions-and-platforms) summarizes it. The
crate supports NDI 6 runtimes and Rust 1.88 or later.

## What was tested for 3.0.0

The test suite was run on **Linux x86_64** with `cargo test --locked`. Every run
against a real runtime used `--config ci/offline.toml`, which confines each test
executable to loopback networking and a private temporary directory. Every such
run also set `GRAFTON_NDI_EXPECTED_RUNTIME`, `GRAFTON_NDI_EXPECTED_VERSION` and
`GRAFTON_NDI_EXPECTED_EDITION`, so each test that loaded a runtime checked that
it had loaded exactly the intended file, version and edition. The counts are the
totals of every test binary and the doctests.

### Runs with one SDK

| Advanced headers | Runtime | Features | Required to be present | Passed | Failed | Ignored |
| --- | --- | --- | --- | --- | --- | --- |
| None | None | all | Nothing | 636 | 0 | 3 |
| None | Standard 6.1.1 | all | `1` | 636 | 0 | 3 |
| None | Standard 6.1.1 | none | `1` | 398 | 0 | 2 |
| None | Standard 6.3.2 | all | `1` | 636 | 0 | 3 |
| None | Standard 6.3.2 | `image-encoding` | `1` | 424 | 0 | 3 |
| None | Standard 6.3.2 | none | `1` | 398 | 0 | 2 |
| Advanced 6.3.2 | Advanced 6.3.2 | all | `advanced` | 639 | 0 | 3 |
| None | Advanced 6.3.2 | none | `1` | 398 | 0 | 2 |
| None | Standard 6.4.0.2 | all | `1,discovery-server` | 636 | 0 | 3 |
| None | Standard 6.4.0.2 | none | `1,discovery-server` | 398 | 0 | 2 |
| Advanced 6.4.0.2 | Advanced 6.4.0.2 | all | `advanced,file,discovery-server` | 639 | 0 | 3 |
| None | Advanced 6.4.0.2 | none | `1,discovery-server` | 398 | 0 | 2 |
| None | Advanced 6.4.0.2 | `file-io` | `1,file,discovery-server` | 508 | 0 | 2 |

"Advanced headers" is the Advanced SDK that `NDI_SDK_DIR` named for a build
with the `advanced` feature, whose Advanced-only headers that build reads. No
other build reads an SDK, so "None" also covers a build made where a Standard
SDK, or an Advanced SDK without the feature, was installed. The core bindings
are always the vendored 6.4.0.2 ones. "Required to be present" is the value of
`GRAFTON_NDI_REQUIRE_RUNTIME`: what the environment promised, so that a test
could not pass by skipping. In the first row the system's runtime was hidden
and the tests that need a runtime reported skips. The other rows visibly skip
the Advanced or File operations that their build and runtime cannot provide;
the requirements make sure that the operations a row promises were not
skipped. The ignored tests are a manual benchmark, a slow stress test and,
with all features or `image-encoding`, a second manual benchmark.

`tests/receiver_video_only.rs` used to send without a clock, so it sent video
and audio as fast as its loop turned: about 17,000 frames of each a second,
some 500 times real time. With every core busy, the runtime dropped the control
receiver's connection within the first second and had not restored it 30
seconds later, even after the load and the sending had stopped, and the test
failed. It now sends in real time, clocked 30 fps video with 1/30 s of audio per
frame. With twice as many busy processes as the machine has cores, the old test
failed 3 of 3 runs against Standard 6.4.0.2; the new one passed 10 of 10 against
each of Standard and Advanced 6.4.0.2. The rows above were run with the new
test, one at a time.

### Runs that cross versions

These are the runs the `sdk` CI job makes on its Linux x64 Advanced 6.4.0.2 row,
all with every feature enabled. In each, a second process on the "peer" runtime
sends video, audio and metadata to a receiver on the run's runtime, and
`tests/sdk_peers.rs` checks the decoded pixels, nonzero audio samples, frame
timecodes and metadata. Both processes are confined to loopback, and each checks
the identity of the runtime it loaded.

| Advanced headers | Runtime | Peer sending | Required to be present | Passed | Failed | Ignored |
| --- | --- | --- | --- | --- | --- | --- |
| Advanced 6.4.0.2 | Standard 6.1.1 | Advanced 6.4.0.2 | `1` | 639 | 0 | 3 |
| Advanced 6.4.0.2 | Advanced 6.3.2 | Advanced 6.4.0.2 | `advanced` | 639 | 0 | 3 |
| None | Advanced 6.4.0.2 | Standard 6.1.1 | `1,file,discovery-server` | 636 | 0 | 3 |
| Advanced 6.3.2 | Advanced 6.4.0.2 | Advanced 6.3.2 | `advanced,file,discovery-server` | 639 | 0 | 3 |

So 6.1.1 and 6.4 exchanged media in both directions, as did Advanced 6.3.2 and
6.4, and Advanced builds ran against runtimes of another version in both
directions.

To require the peer test locally, set `GRAFTON_NDI_PEER_RUNTIME` to the second
SDK's exact library path, `GRAFTON_NDI_PEER_VERSION` to its four-component
version, and `GRAFTON_NDI_PEER_EDITION` to `standard` or `advanced`; the peer
must be a different version from the runtime the tests load. With these set, a
missing peer, an identity mismatch, a delivery failure or a failed shutdown of
the child process fails the test. Without them it reports a skip.

### What each runtime provides

The runtimes report their versions as `6.1.1` (for 6.1.1), `6.3.2.0` and
`6.4.0.2` at the end of their version text. A build with the Advanced SDK
6.4.0.2 headers reported these capabilities for them:

| Capability | Standard 6.1.1 | Standard 6.3.2 | Advanced 6.3.2 | Standard 6.4.0.2 | Advanced 6.4.0.2 |
| --- | --- | --- | --- | --- | --- |
| `Planar444Video`, `ReceiveBest444`, `ReceiveVideoOnly` | `RuntimeTooOld` | `RuntimeTooOld` | `RuntimeTooOld` | Available | Available |
| `ReceiveDelay`, `SdkTimestamp` | `SymbolUnavailable` | `SymbolUnavailable` | `SymbolUnavailable` | Available | Available |
| `ConnectedSourceName` | `SymbolUnavailable` | Available | Available | Available | Available |
| `SenderAdvertising`, `ReceiverAdvertising`, `SenderDiscovery`, `ReceiverDiscovery` | `SymbolUnavailable` | `RuntimeTooOld` | `RuntimeTooOld` | Available | Available |
| `AdvancedSenderAdvertiserConfiguration`, `AdvancedReceiverAdvertiserConfiguration`, `AdvancedSenderListenerConfiguration`, `AdvancedReceiverListenerConfiguration` | `WrongEdition` | `WrongEdition` | `RuntimeTooOld` | `WrongEdition` | Available |
| Every other Advanced capability, including `Passthrough` with each of the 15 presets | `WrongEdition` | `WrongEdition` | Available | `WrongEdition` | Available |
| The other seven capabilities | Available | Available | Available | Available | Available |
| `FileApi::open` | Refused: Standard runtime | Refused: Standard runtime | No File library | Refused: Standard runtime | Available |

An operation whose capability is unavailable returns
`Error::UnsupportedCapability` before any native call is made. The 6.1.1 and
6.3.2 SDKs include no File library; the File library of the 6.4.0.2 SDK was
tested only beside the Advanced 6.4.0.2 runtime.

### Static checks

On the same machine these checks passed: formatting; clippy with warnings
denied for the workspace with all features, and for the crate with no features
and with each of `image-encoding`, `tokio`, `smol`, `advanced`, `file-io` and
`advanced,file-io`; the same `advanced` lints with the Advanced SDK 6.4.0.2
headers; rustdoc with warnings denied, with and without those headers; a
`cargo check` with Rust 1.88; `cargo package`; and the check that the vendored
bindings match the SDK 6.4.0.2 headers.

## Compatibility safeguards and additional runtime checks

Which Advanced operations a build can perform follows from the declarations
generated from the installed headers. A family of functions that the headers
lack disables only that family, and types or receive presets that older headers
lack do not stop them compiling. The build script and the runtime tables take
the families from one registry, `src/sys/advanced_tables.rs`. The coverage test
requires every Advanced function the installed headers declare to belong to a
family or to be listed as deliberately not wrapped. `tests/advanced_headers.rs`
builds the crate, with warnings denied, against synthetic partial headers and
checks the capabilities that build reports. That tests the fallback; it is not
a run against a historical vendor SDK.

Further Linux x86_64 tests against 6.4 check a real lossless HEVC 4:4:4 frame,
including every sample of its three full-resolution planes; audio suppression
by both video-only bandwidth settings, with an ordinary receiver as a control;
AVSync following changes in video size, frame rate, audio sample rate and
channel count while preserving nonzero samples; and both Genlock clocks
following a local source and falling back to paced system clocks. The KVM
boundary test checks failed release, retry on drop, touch coordinates and
clipboard ownership using stand-in functions; it does not qualify a KVM device.

A FrameSync and raw audio capture on its receiver compete for the receiver's
audio, which is why `Receiver<FrameSyncAttached>` has no audio capture. This
was measured by hand, on Linux x86_64 over loopback with Standard 6.3.2 and
6.4, and no test repeats it. A clocked sender sent 30 fps video, each frame
with 1600 sequence-coded samples of 48 kHz stereo audio, while a FrameSync
pulled 480 samples every 10 ms for 8 s. Without raw capture, every frame in the
window reached the FrameSync and it returned no silence. With raw audio capture
on the attached receiver, interleaved with the pulls or on a second thread,
each audio frame reached exactly one of the two: raw capture took 131 to 216 of
the 196 to 239 frames in the window, and the FrameSync returned silence for 65
to 90 per cent of the samples it pulled. Every metadata frame still reached the
attached receiver's own metadata capture.

Tests of `FileRecorder::reset` check that it returns the old session's report
without claiming that the files were finalized, that late events of the old
session do not reach later ones, and that the original recorder stays usable
when its replacement cannot be created.

Two File API functions are deliberately not called:
`NDIlib_file_recorder_get_source` and `NDIlib_file_recorder_get_current_file`.
They return borrowed strings that the SDK can invalidate on its own recording
thread while it stops or rolls over, so copying them at once does not make them
safe to read, and a Rust mutex around the calls cannot exclude the SDK's thread.
The Rust API provides `last_accepted_source` and `reported_current_file` from
its own state and the recorder's events instead, and documents how they differ
from an instantaneous native query. Complete coverage of the C entry points is
therefore not claimed. These exceptions can go when the vendor provides a
stable lifetime or a copying variant.

`ci/sdk.py` exports the expected version, edition and exact library path of the
package it provisions, and the tests compare them with the runtime loaded in
their own process. `tests/runtime_identity.rs` checks that a mismatch of each
fails the run, so knowing which archive was unpacked is not enough to pass.

## Retained SDK packages

[`ci/sdk-packages.json`](../ci/sdk-packages.json) pins the packages the tests
use: Standard 6.1.1 for Linux, and Standard and Advanced 6.3.2 and 6.4.0.2 for
Linux, Windows, Apple and Android. The CI matrix runs Standard 6.1.1 on Linux
and Standard and Advanced 6.3.2 on Linux, Windows and macOS as baselines, and
crosses the oldest retained Standard and Advanced packages with the current
Advanced one in both directions: Standard 6.1.1 on Linux, 6.3.2 elsewhere.

The Linux 6.1.1 installer was recovered from RPM Fusion's source cache and
matches the SHA-512 in its
[historical source manifest](https://github.com/rpmfusion/ndi-sdk/blob/d0147e01a716c8e27d03894d79c296054e8caf63/sources).
Its `Version.txt` reads `NDI 2024-12-20 r143744 v6.1.1`. The unmodified vendor
installer has its own SHA-256 pin in `ci/sdk-packages.json`.

No other SDK version has been tested: not NDI 6.0 or 6.2, not Advanced SDKs
before 6.3.2, and not the older versions for other platforms. The crate is
written for every NDI 6 runtime, and its capability checks keep a runtime from
being asked for what it lacks, but only the versions above are tested. The
Windows and Apple 6.3.2 packages were checked for version and archive identity,
and their headers were compiled, but nothing was run from them.

## What was compiled but not run

The crate was linted with clippy, all features, all targets and warnings
denied, for these targets with Rust 1.99.1. No test was run on them, and no NDI
library was loaded on them:

- `i686-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
  `armv7-unknown-linux-gnueabihf`
- `x86_64-pc-windows-msvc`, `i686-pc-windows-msvc`
- `x86_64-apple-darwin`, `aarch64-apple-darwin`
- `aarch64-linux-android`, `armv7-linux-androideabi`, `i686-linux-android`,
  `x86_64-linux-android`

The library was linted the same way for `aarch64-apple-ios` and
`x86_64-apple-ios` against the NDI SDK for Apple 6.4. That selects and checks
the static archive, but the static link was not performed and nothing was run.

`aarch64-apple-tvos`, `x86_64-apple-tvos` and `aarch64-apple-visionos` were not
compiled: the CI job that builds them has not yet run.

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
- NDI runtimes other than Standard 6.1.1 and Standard and Advanced 6.3.2 and
  6.4.0.2, all on Linux x86_64. NDI 6.0 and 6.2 were not tested.
- The File library on Windows, macOS and Android, and beside an Advanced 6.3.2
  runtime.
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

[`ci/sdk.py`](../ci/README.md#sdk-packages) installs an SDK package into a
directory of your choice and prints the variables above, together with the
`GRAFTON_NDI_EXPECTED_*` values that make the tests check the runtime they
load. The crossed runs set `NDI_SDK_DIR` to one SDK, `NDI_RUNTIME_DIR_V6` and
the expected values to another, and the `GRAFTON_NDI_PEER_*` values to the SDK
that is not loaded. The
[CI workflow](../.github/workflows/rust.yml) shows each step.

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

## Recording a validation

When you test a new SDK version or platform, add it to the tables above with
the runtime's own version text (`ndi.info().raw_version()`), the build inputs,
and what `GRAFTON_NDI_REQUIRE_RUNTIME` promised. Record what was run, not what
was configured: a platform that was only compiled belongs under "compiled but
not run".
