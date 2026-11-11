# SDK integration validation

Use the revision’s [CI checks](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
and their `required-ci` aggregate to determine qualification. The required scope
contains 52 individually recorded checks in 16 SDK runner jobs across 20 supported
profiles. Aggregate acceptance requires every check, all 12 package availability
receipts, and matching tested commit/tree, dependency, SDK and toolchain identities.
Results from different revisions or scopes cannot be combined into a passing
aggregate. The optional 82-check extended audit is separate from release
qualification.

GitHub Actions can check out a generated PR merge commit. Record the PR head and
the actual checked-out commit/tree separately; the head SHA alone does not identify
the tested source. Native execution, ABI/link, application packaging,
emulator/simulator and physical-device qualification are separate evidence levels.

The compatibility contract targets NDI 6.x and Rust 1.88. SDK archives, restricted
generated bindings, private source locators and credentials stay private; hosted
CI shares bounded validation records. Build and runtime selection are documented
in the [README](../README.md), and reproducible acquisition and execution in the
[CI guide](../ci/README.md). The historical section records its named implementation
and does not qualify the current source.

Retained local assessment evidence includes 6.3.2 and 6.4 Linux x64 headers/runtimes, exact
loaded-module identity, old-header/new-runtime feature tests, deterministic
callback/ownership tests, and native File playback/recording tests in a network
namespace with host D-Bus sockets hidden. Platform selection was checked against
the actual extracted artifacts. Independent C/Rust ABI probes also run without
executing target code. The existing native integration suites passed
70 test executions across five SDK/header/runtime combinations, including both
directions of Standard 6.3.2/6.4 runtime substitution. These local assessment
results apply to Linux x64. Hosted qualification comes from the exact revision's
required aggregate and the individual coverage levels in its result records.

The maintained definitions are `build_profiles.tsv`, `api/ndi-api.toml`,
`tools/sdk/`, and `ci/`. The profile file inventories vendor artifacts; it is not
a blanket supported-platform claim. The supported set is explicitly declared in
the CI policy and requires obtainable toolchains on GitHub-hosted runners. The
19 specialized vendor profiles without those inputs remain unsupported candidates.
The required plan runs full native suites on Linux x64, Windows x64 and macOS
ARM64 with both current SDK editions. Every other supported profile retains
current Standard/Advanced ABI and final link/package checks. Standard Android x64
and tvOS x64 provide the required emulator and simulator runtime representatives;
other mobile checks establish packaging only. Grouping checks in one job does not
combine their individual evidence or turn build coverage into runtime coverage.
A prepared workflow or local unit test is not proof that its hosted jobs have run.
Required SDK packages or vendor toolchains that are absent make the gate fail.
Mobile runtime representatives use a fixed sender and a generated 16×16 frame on
disposable hosted virtual devices. Their emulator/simulator results do not
establish desktop-style confinement or prove that off-host networking is blocked.

Explicit native SDK execution has separate confinement evidence. Linux uses a
network namespace and Windows uses process firewall rules. On disposable hosted
macOS runners, `macos-pf-offline` combines the process sandbox with a bounded
whole-VM loopback-only PF window. The guardian verifies the boundary before
starting SDK code and reaps its processes before restoring the exact initial
filter and confirming connectivity recovery. The aggregate requires a receipt
for each test, ignored-test listing and runtime-identity probe, including
Advanced/File suites and the compatibility probes. The 39 registered core tests
run in native and compatibility cells and under each of the nine feature
selections in the feature cells, with exact feature bindings in their reports.
Missing or incomplete cleanup proof fails qualification. Builds, pure tests and
pure runnable doctests keep their ordinary execution environment; SDK examples
are compile-only doctests and resource contracts remain uncalled in trybuild.
Ordinary CI tests select a fresh empty default-runtime directory, while explicit
fake-library loader fixtures retain their own module selection. This loader guard
does not replace native network confinement.
These controls do not establish physical-device behavior;
the [CI guide](../ci/README.md) defines the exact checks and limits.

The required compatibility baseline is Standard 6.3.2, tested against current
6.4 in both header/runtime directions on Linux x64, Windows x64 and macOS ARM64.
Linux x64 checks minimum Rust and optional-feature combinations with both versions,
and Android ARM64 checks minimum Rust with the baseline SDK. Public API
compatibility is checked on Linux x64. Retention covers the four complete 6.3.2
Standard platform bundles and all eight 6.4 Standard/Advanced platform bundles;
future SDK releases are added explicitly. SDKs 6.0, 6.1.1 and Advanced versions
other than 6.4 do not have current validation results.

Raspberry Pi profile checks use a pinned public GNU cross-toolchain;
their compiler/sysroot version defines the tested build environment, not proof
of the vendor's oldest libc floor or of physical board compatibility. Apple
profiles needing `build-std` use an explicitly pinned compiler. Build/link or
emulator results do not imply device qualification.
The development store passed isolated read-only mount and selected-runtime
checks; a Docker image/container has not been rebuilt in this environment. Do not
infer ARMv6 support from the RPi1 directory name: both inspected 6.3.2 and 6.4
artifacts declare ARMv7/VFPv3 attributes, and the historical QEMU run below did not
establish an ARMv6-only CPU contract. Absent SDK simulator slices and unsupported
Rust-target intersections remain explicitly unavailable.

V210/P216 conversions use checked, aligned storage and complete six-pixel V210
groups. Independent pixel and memory checks passed with both tested Standard
runtimes. File getters returning pointers invalidated by native background work
are not exposed as borrowed Rust strings; the safe APIs use owned observations
and explicit unavailable diagnostics where the SDK supplies no lifetime proof.

## Reproducing current validation

Use the exact cataloged archives through [CI provisioning](../ci/README.md) or
the [local SDK store](../ci/sdk-store.md). The store selects header and runtime
versions independently and verifies their contents before running a command.
Keep SDKs and build outputs outside the source checkout. Both paths use the same
package identities and hashes; the container's default SDK is Standard 6.3.2.

`ci/plan.py --scope required` lists the required checks and runner groups.
`ci/batch.py` executes a group while keeping individual results; `ci/run.py` runs
a selected check. Both plan generation and individual execution accept
`--scope extended` for the optional broader audit. `ci/aggregate.py` checks complete
hosted evidence for the exact declared scope, commit, dependency resolution, SDK
packages and target toolchains. A locally prepared workflow or a
passing policy suite does not replace those runs. Publication requires the
separate [publication gates](../ci/README.md#publication-gates).

---

# Historical record: NDI SDK 6.3.2 validation for 2.0.0

The remainder of this document records the 2.0.0 implementation and its tooling
at the time. References to unsupported mobile targets, legacy setup actions,
URL secrets, and future Apple work below describe that revision. For the current
implementation and workflow, use the guides linked above.

Validated on 2026-10-16 using Rust 1.99.0 on an x86_64 Debian 12 host. NDI's
[developer page](https://ndi.video/for-developers/) and
[release notes](https://docs.ndi.video/all/developing-with-ndi/sdk/release-notes)
identified 6.3.2 as the current SDK at that validation date. The downloaded Linux, Android, and Apple
`Version.txt` files identify `v6.3.2.0`, dated 2026-04-13; the Linux x86_64
runtime also reports `6.3.2.0` through `NDIlib_version()`. The Windows x86/x64
DLL version resources identify 6.3.2.

This validates the wrapper's existing APIs against the new SDK. It does not add
wrappers for every new NDI 6.3 discovery/advertiser API, or require 6.3.2 as a new
minimum version. Bindings continue to be generated from the installed headers.
The standard SDK was used; an actual Advanced SDK installation was not tested.

## SDK baseline coverage

Every successful build below linked **all examples with all features**, rather
than stopping at `cargo check` or an unlinked Rust library.

| Target | NDI library directory | Result on this host |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | `lib/x86_64-linux-gnu` | Full native test suite, SDK regressions, and examples passed |
| `i686-unknown-linux-gnu` | `lib/i686-linux-gnu` | Examples linked; 262 library tests passed using the 32-bit glibc loader |
| `aarch64-unknown-linux-gnu` | `lib/aarch64-rpi4-linux-gnueabi` | Examples linked; 262 library tests passed under QEMU |
| `armv7-unknown-linux-gnueabihf` | `lib/arm-rpi4-linux-gnueabihf` | Examples linked; 262 library tests passed under QEMU |
| `arm-unknown-linux-gnueabihf` | `lib/arm-rpi1-linux-gnueabihf` | Examples linked; 262 library tests passed under QEMU |
| `x86_64-pc-windows-msvc` | `Lib/x64` | Examples linked using cargo-xwin; no Windows runtime tests here |
| `i686-pc-windows-msvc` | `Lib/x86` | Examples linked using cargo-xwin; no Windows runtime tests here |
| `aarch64-linux-android` | `lib/arm64-v8a` | Examples linked using NDK r27d, API 24 |
| `armv7-linux-androideabi` | `lib/armeabi-v7a` | Examples linked using NDK r27d, API 24 |
| `i686-linux-android` | `lib/x86` | Examples linked using NDK r27d, API 24 |
| `x86_64-linux-android` | `lib/x86_64` | Examples linked using NDK r27d, API 24 |
| `aarch64-apple-darwin`, `x86_64-apple-darwin` | `lib/macOS` | Installer/hash inspected; no Apple platform SDK or runtime on this host |
| iOS and other Apple mobile targets | Apple static libraries | Not currently supported by the build script; no local build/runtime validation |

Each Linux library run also had one intentionally ignored test. QEMU coverage
does not replace device testing. Android still needs device/emulator runtime
testing, including discovery and application packaging. The Windows-hosted
Android installer was downloaded and hashed; its four `libndi.so` files match
the Linux-hosted installer byte for byte. Builds used the Linux-hosted one.

Before the dependency/runtime migration in #114, native Linux checks passed: `cargo test --all-features`, all six build-selection
tests, Clippy with warnings denied, examples with all features, Rust 1.87 library
compatibility, and the normally ignored sender lifecycle, receiver status,
FrameSync audio, and video-padding SDK regression suites. The full native run
included 262 library tests, compile-contract fixtures, and 73 doctests.

CI retains native Linux, Windows, and macOS testing and adds cross-build jobs for
all four Android ABIs. Linux ARM/x86 and Windows x86 cross-build results above
were validated locally; they are not additional CI runtime jobs. The macOS CI
job covers the architecture of its hosted runner, not both Mac architectures.

## Dependency/runtime migration validation

Additional validation on 2026-10-16 uses the same standard NDI SDK 6.3.2.0 on
Debian 12 x86_64, with the new Rust 1.88 minimum and optional smol adapter. These
results validate the implementation of #114 separately from the SDK baseline
above and its historical Rust 1.87 check.

- Rust 1.88: all-target/all-feature check and 266 library tests passed; one test
  remains intentionally ignored. Compiler-diagnostic fixtures use stable Rust.
- Stable Rust 1.99: all-feature tests passed, including 266 library tests,
  39 compile-contract cases, and 73 doctests. Clippy and rustdoc passed with
  warnings denied. Default/all-feature examples built and example tests passed.
- All 10 normally ignored SDK regressions passed across async-send lifecycle,
  sender stress, receiver status, FrameSync audio, and padded video. Padded-video
  coverage exercises both Tokio and smol owned capture.
- The resolved graph passed `cargo audit --deny warnings` with zero
  vulnerabilities and zero warnings. It contains event-listener 5.4.2 and no
  async-std or once_cell. Direct dependency floors are recorded in `Cargo.toml`;
  consuming applications still need to refresh affected lockfiles.

The [release preparation PR](https://github.com/GrantSparks/grafton-ndi/pull/115)
provides native Linux/Windows/macOS CI and four Android example-link checks.
Those checks and the main-branch Pages deployment establish hosted coverage;
local cross-linking from the SDK baseline is not native execution of this
migration. Android device execution and an actual Advanced SDK installation
remain untested.

## Final cache, snapshot, and pixel-view validation

The final 2.0.0 fixes for #117–#119 add canonical cache identity, row-bounded
image conversion, and scoped transfer of immutable video views. Their checks
use stable Rust 1.99 on Debian 12 x86_64 with standard NDI SDK 6.3.2.0;
library and compile-contract coverage also ran against standard SDK 6.1.1.

- All-feature library tests: 269 passed, with two manual throughput comparisons
  intentionally ignored in ordinary runs. The public compile suite contains 45
  pass/fail cases, including owner lifetimes and both FrameSync auto traits.
- All-feature rustdoc examples: 74 passed. Strict Clippy and rustdoc builds pass.
- All 13 examples build with default and all features. Example tests include the
  two PCM conversion regressions. Contributor checks also build and test each
  independent optional-runtime configuration; PNG requires `image-encoding`.
- The live padded-video regression resolves the advertised machine through
  hostname case/final-dot variants, invalidates and rediscovers it, connects a
  receiver, transfers pixel views to scoped workers, and encodes both receiver
  and FrameSync frames before freeing them on their permitted thread.
- The same live regression passes under Valgrind Memcheck with zero errors.
  It observes a 2592-byte SDK stride for 642-pixel BGRA, versus 2568 active bytes.
  The synthetic uninitialized-padding library tests also pass Memcheck with
  zero errors, including scoped reads of packed and multiplane formats.
- Release-mode encoding comparisons cover 1080p/4K/8K, padding, pixel entropy,
  PNG, and JPEG quality 1/85/100, with scalar and unified JPEG SIMD builds. See
  [image encoding resources](image-encoding-performance.md) for the measured
  conversion scratch, codec/output peak allocations, timings, and commands.

The existing native desktop, Android example-link, MSRV, audit, and semver CI
checks apply to these fixes. See the
[final release PR](https://github.com/GrantSparks/grafton-ndi/pull/120) for their
results. An `--all-features` standard-SDK build still does not validate an actual
Advanced SDK installation. Android device execution and physical PTZ/camera
behavior remain outside the local coverage.

To repeat the live padding check on a configured Linux SDK installation:

```bash
cargo test --all-features --test video_padding_sdk -- --ignored --nocapture
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="valgrind --tool=memcheck --track-origins=yes --error-exitcode=99" \
  cargo test --all-features --test video_padding_sdk -- --ignored --nocapture
```

## Installing and selecting the SDK

Download installers outside the repository. The
[CI maintenance guide](../.github/actions/README.md) records their hashes and
explains URL secrets. Personalized links must not appear in tracked files or
public change descriptions. CI needs URL secrets only when overriding the
vendor fallback URLs; it does not need SDK binaries uploaded to public releases.

Set `NDI_SDK_DIR` to the **target** SDK root containing headers and libraries.
Android requires its own SDK, not the Linux libraries. Windows installers use
`Include` and `Lib`; both their casing and the existing lowercase layout work.
Cross-compilation also requires target Rust standard libraries, a linker, and
target C headers for bindgen. The build script never substitutes host NDI
binaries when target libraries are absent.

The Linux tarball contains an installer script with an embedded SDK archive.
The setup action extracts it and creates `libndi.so` and `libndi.so.6` symlinks
beside every `libndi.so.6.3.2` binary. If extracting manually, create those links
in **each target library directory** before building. For example:

```bash
export NDI_SDK_DIR='/path/to/NDI SDK for Linux'
for library in "$NDI_SDK_DIR"/lib/*/libndi.so.6.3.2; do
    directory="$(dirname "$library")"
    ln -sfn libndi.so.6.3.2 "$directory/libndi.so"
    ln -sfn libndi.so.6.3.2 "$directory/libndi.so.6"
done
export LD_LIBRARY_PATH="$NDI_SDK_DIR/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export NDI_RUNTIME_DIR_V6="$NDI_SDK_DIR/lib/x86_64-linux-gnu"
cargo test --all-features
cargo build --examples --all-features
```

## Linux cross-builds

Install the appropriate cross GCC/glibc packages and Rust targets:

| Rust target | Linker | C sysroot on the Debian validation host |
| --- | --- | --- |
| `aarch64-unknown-linux-gnu` | `aarch64-linux-gnu-gcc` | `/usr/aarch64-linux-gnu` |
| `armv7-unknown-linux-gnueabihf` | `arm-linux-gnueabihf-gcc` | `/usr/arm-linux-gnueabihf` |
| `arm-unknown-linux-gnueabihf` | `arm-linux-gnueabihf-gcc` | `/usr/arm-linux-gnueabihf` |
| `i686-unknown-linux-gnu` | `i686-linux-gnu-gcc` | `/usr/i686-linux-gnu` |

The target runtime needs Avahi and its dependencies (including D-Bus). Install
target-architecture packages in the sysroot or extract them into a separate
directory. If the linker cannot locate these indirect dependencies, pass
`-C link-arg=-Wl,-rpath-link,/path/to/target/libraries` in `RUSTFLAGS`, once per
library directory. Do not use host x86_64 libraries or suppress unresolved
shared-library errors to make a cross-link appear successful.

For example, with ARM64 runtime dependencies installed in the target sysroot:

```bash
rustup target add aarch64-unknown-linux-gnu
export NDI_SDK_DIR='/path/to/NDI SDK for Linux'
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu='--sysroot=/usr/aarch64-linux-gnu'
cargo build --examples --all-features --target aarch64-unknown-linux-gnu
```

To run library tests, configure Cargo's target runner to invoke `qemu-aarch64`
or `qemu-arm` with `-L` pointing at the target glibc sysroot. The emulated
process's `LD_LIBRARY_PATH` must include both the target NDI library directory
and target Avahi/D-Bus dependencies. Then run `cargo test --lib --all-features
--target <target>`. On this host i686 tests ran directly through the 32-bit
glibc loader; the QEMU i386 attempt stalled and was stopped.

## Android cross-builds

Use the NDI SDK for Android and an installed Android NDK. CI pins NDK r27d
(`27.3.13750724`); the helper defaults to API 24. This is the configuration tested
here, not a claim that every older Android API level is supported.

```bash
export NDI_SDK_DIR='/path/to/NDI SDK for Android'
export ANDROID_NDK_HOME='/path/to/android-ndk-r27d'
rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android
for target in aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android; do
    scripts/build-android.sh "$target"
done
```

The helper configures the target-specific linker and bindgen sysroot, including
the ARMv7 NDK compiler/header naming difference. `ANDROID_API_LEVEL` overrides
24. Applications must package the corresponding `libndi.so`, its required
runtime dependencies, and configure Android network access; linking examples
alone does not test those application responsibilities.

## Windows cross-builds from Linux

Install [cargo-xwin](https://github.com/rust-cross/cargo-xwin), Clang/LLD, and the
Rust targets. Extract the Windows installer outside the repository (the
validation used `innoextract`); select the extracted `app` directory containing
`Include` and `Lib`. cargo-xwin provisions the Microsoft CRT and Windows SDK.

```bash
rustup target add x86_64-pc-windows-msvc i686-pc-windows-msvc
export NDI_SDK_DIR='/path/to/extracted/windows/app'
export XWIN_ARCH=x86,x86_64
cargo xwin build --examples --all-features --target x86_64-pc-windows-msvc
cargo xwin build --examples --all-features --target i686-pc-windows-msvc
```

Native Windows tests still require the corresponding NDI DLL on `PATH`.
Windows GNU and Windows ARM64 targets are rejected because the standard SDK
does not supply the import-library layout this crate supports for those ABIs.

## Apple follow-up

The Apple package contains macOS dynamic libraries and iOS/tvOS/visionOS static
libraries. Possessing that package does not provide Apple's platform headers,
frameworks, or a runnable Apple environment. Run the existing native tests on
macOS with Xcode's tools before release. Adding Apple mobile target selection,
static-link dependencies, and device/simulator coverage remains separate work.
