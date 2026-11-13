# Set up NDI

## Build requirements

Use Rust 1.88 or later. Windows, macOS, Linux and Android builds need no NDI SDK
with the default features. Two configurations need additional setup:

- To use Advanced native operations, enable `advanced`, install the Advanced SDK
  and `libclang`, and set `NDI_SDK_DIR` to the SDK root. See the [Advanced guide](advanced.md).
- iOS, tvOS and visionOS link a vendor archive at build time. See [Apple mobile targets](#apple-mobile-targets).

To run on a dynamically loaded target, install an [NDI 6 runtime](https://ndi.video/for-developers/ndi-sdk/)
or package it with your application. Point `NDI_RUNTIME_DIR_V6` at its directory,
or put the library beside your executable.

On Linux x86_64, for example:

```sh
export NDI_RUNTIME_DIR_V6="/usr/share/NDI SDK for Linux/lib/x86_64-linux-gnu"
```

`NDI_SDK_DIR` selects build inputs; `NDI_RUNTIME_DIR_V6` selects a runtime search
directory. An explicitly set `NDI_SDK_DIR` must name a valid SDK in builds that
read it. Unset stale values. After installing an SDK in a default location,
set `NDI_SDK_DIR` or run `cargo clean -p grafton-ndi` to trigger a rebuild.

## Finding the runtime

`Ndi::new()` selects and initializes a runtime. On dynamically loaded targets,
the loader tries each location in order:

1. The executable's directory.
2. `NDI_RUNTIME_DIR_V6`.
3. The platform loader's search path (`rpath`, `LD_LIBRARY_PATH`, `PATH` and system directories).

At each location it tries both editions, unless you [request one](#sdk-editions).
Builds with `advanced`, or with `file-io` on Linux, macOS or Android, try the
Advanced name first; other builds try Standard first. On macOS, the platform
search first tries `@rpath/<name>`.

Missing, unloadable or incompatible candidates do not stop the search.

The library file names are:

| Platform | Standard SDK | Advanced SDK |
| --- | --- | --- |
| Windows x64 | `Processing.NDI.Lib.x64.dll` | `Processing.NDI.Lib.Advanced.x64.dll` |
| Windows x86 | `Processing.NDI.Lib.x86.dll` | `Processing.NDI.Lib.Advanced.x86.dll` |
| macOS | `libndi.dylib` | `libndi_advanced.dylib` |
| Linux | `libndi.so.6` | `libndi_advanced.so.6` |
| Android | `libndi.so` | `libndi_advanced.so` |

To load one exact file instead, pass its absolute path. No other location is tried, and a relative path returns `Error::InvalidInput`:

```rust
use grafton_ndi::{RuntimeOptions, Ndi};

let options = RuntimeOptions::default().library_path("/opt/ndi/lib/libndi.so.6");
let ndi = Ndi::with_options(&options)?;
println!("loaded {:?}", ndi.info().library_path());
```

If no candidate works, `RuntimeLoadFailed` lists attempted paths and their errors.
You can retry a failed selection. A successful selection lasts for the process;
requesting a different library or an incompatible edition returns `RuntimeConflict`.

Rejected libraries that loaded also stay resident. The system loader may resolve
later searches to that rejected module again. Remove or fix the named library,
or select an explicit path.

## Selecting a runtime

Use `Runtime::select` to load and inspect a library before starting NDI.
Call `initialize` to obtain an `Ndi` handle. `Ndi::new()` performs both steps with
default options; `Ndi::with_options` uses your options.

```rust
use grafton_ndi::{Runtime, RuntimeOptions};

let runtime = Runtime::select(&RuntimeOptions::default())?;
println!("NDI {} from {:?}", runtime.info().raw_version(), runtime.info().library_path());
if !runtime.is_supported_cpu() {
    eprintln!("this CPU cannot run the selected NDI runtime");
}
let ndi = runtime.initialize()?;
```

A runtime that does not start because of the CPU makes `initialize` return `Error::UnsupportedCpu`. `ndi.runtime()` returns the `Runtime` of an existing handle.

## SDK editions

NDI ships a Standard SDK and an Advanced SDK with different runtime libraries. Without a request, the search accepts the library of either edition, so a program finds whichever runtime the machine has. A request narrows the search to one library name:

- `RuntimeOptions::default().edition(SdkEdition::Advanced)` searches only for the Advanced library and fails unless the loaded library exports the Advanced SDK entry points. Use it when the program needs Advanced SDK operations.
- `RuntimeOptions::default().edition(SdkEdition::Standard)` searches only for the Standard library. An Advanced runtime found under that name, or named by an explicit path, also meets a Standard request.

The search does not depend on how or where the crate was built. `NDI_SDK_DIR` matters only to builds that read an SDK: the `advanced` feature and the Apple mobile targets.

`ndi.info().edition()` reports the edition of the library that was actually loaded, detected from the functions it exports. An edition says nothing about a license entitlement.

## Runtime capabilities

Use `ndi.info()` to inspect the selected library and `ndi.capabilities()` to
check optional operations. Cargo features expose Rust APIs; runtime capabilities
report which operations can run.

```rust
use grafton_ndi::RuntimeFeature;

ndi.require_feature(RuntimeFeature::ReceiveDelay)?;
```

`Error::UnsupportedCapability` identifies the feature, reason and supporting detail.
The [API reference](https://docs.rs/grafton-ndi/latest/grafton_ndi/enum.CapabilityUnavailable.html)
describes each reason. Capability availability does not establish hardware support
or license entitlement.

The File library loads separately. Query `FileApi::status` or `FileApi::supports`
for playback and recording; the core runtime reports `SeparateLibrary` for those
features, even when the File library is available. See the [File guide](file-io.md).

## Deploy an application

Package the native runtime explicitly: dependency scanners cannot discover a
library loaded at run time. Use one of the search locations above or an absolute
`RuntimeOptions::library_path`.

**The default search trusts directory ownership.** `Ndi::new()` tries libraries
beside the executable before environment and platform locations. Loading a
candidate executes its native constructors before symbol or edition checks,
even if that candidate is rejected and the search continues. For services and
elevated processes, make the very first selection use an absolute path in an
application-owner-controlled directory:

```rust
use grafton_ndi::{Ndi, RuntimeOptions};
# fn main() -> grafton_ndi::Result<()> {
let ndi = Ndi::with_options(&RuntimeOptions::new()
    .library_path("/opt/my-service/ndi/libndi.so.6"))?;
println!("Runtime: {:?}", ndi.info().library_path());
# Ok(())
# }
```

An explicit path tries only that file, with no executable, environment or
platform fallback. Select it before any other component calls `Ndi::new()`;
a later selection cannot undo native code already executed. Native dependencies
still follow platform loader rules and need trusted locations too. File API
users must also select an absolute `FileRuntimeOptions::library_path` or trust
all of its search directories, beginning with the core runtime's companion
directory. Inspect that selection with `FileApi::library_path`; see
[File library deployment](file-io.md#requirements).

## Supported NDI versions and platforms

`grafton-ndi` 3.0 works with NDI 6 runtimes from the Standard SDK and the Advanced SDK. It needs Rust 1.88 or later.

### NDI versions

| NDI SDK | Status | Editions tested |
| --- | --- | --- |
| 6.4.0.2 | Tested. The crate's bindings are generated from its headers. | Standard, Advanced |
| 6.3.2 | Tested. | Standard, Advanced |
| 6.1.1 | Tested. | Standard |
| Other NDI 6 releases, including 6.0 | Not tested. | |
| NDI 5 and earlier | Not supported. | |

CI runs the whole test suite against each tested version on the platforms that the [Platforms](#platforms) table lists for it. It also crosses versions: a build made with one version's SDK runs against another version's runtime, and senders and receivers of different versions exchange video, audio and metadata. See [SDK validation](sdk-validation.md).

The crate checks what the selected runtime provides before it calls into it. An operation that the runtime cannot perform returns `Error::UnsupportedCapability`, and the rest of the API keeps working. [Runtime capabilities](#runtime-capabilities) shows how to ask in advance. The Linux x86_64 runtimes report:

| Available | 6.1.1 | 6.3.2 | 6.4.0.2 |
| --- | :---: | :---: | :---: |
| Core APIs: source discovery, receiving, sending, FrameSync, PTZ, tally and status | Yes | Yes | Yes |
| Routing, receiver connection metadata, the web-control URL, metadata from receivers to a sender, interleaved audio, V210/P216 conversion | Yes | Yes | Yes |
| `Receiver::poll_source_name_change` | No | Yes | Yes |
| Receiver video and audio delay, `Ndi::timestamp` | No | No | Yes |
| Planar 4:4:4 video formats, `ReceiverColorFormat::Best444`, the video-only bandwidth modes | No | No | Yes |
| Discovery Server advertisers and listeners | No | No | Yes |
| Advanced SDK operations (`advanced` feature), with an Advanced runtime | Not tested | Yes | Yes |
| File API (`file-io` feature) | No | No | Yes |

Discovery Server APIs require NDI 6.4 or later. The File library requires NDI
6.4 and, on Linux, macOS and Android, the Advanced runtime. See
[SDK limitations](troubleshooting.md) before deploying these features.

### Platforms

| Platform | Rust targets | How NDI is provided | Validation |
| --- | --- | --- | --- |
| Linux | `x86_64-unknown-linux-gnu` | Runtime library, loaded at run time | Test suite run against Standard 6.1.1, 6.3.2 and 6.4 and Advanced 6.3.2 and 6.4 runtimes |
| Linux | `i686-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` | Runtime library, loaded at run time | Test suite run against the Standard 6.4 runtime |
| Linux | `armv7-unknown-linux-gnueabihf` | Runtime library, loaded at run time | Compiled and linted |
| Windows | `x86_64-pc-windows-msvc` | Runtime library, loaded at run time | Test suite run against Standard and Advanced 6.3.2 and 6.4 runtimes |
| Windows | `i686-pc-windows-msvc` | Runtime library, loaded at run time | Test suite run against the Advanced 6.4 runtime |
| macOS | `aarch64-apple-darwin` | Runtime library, loaded at run time | Test suite run against Standard and Advanced 6.3.2 and 6.4 runtimes |
| macOS | `x86_64-apple-darwin` | Runtime library, loaded at run time | Test suite run against the Standard 6.4 runtime |
| Android | `aarch64-linux-android`, `armv7-linux-androideabi`, `i686-linux-android`, `x86_64-linux-android` | Runtime library packaged with the application, loaded at run time | Compiled and linted |
| iOS | `aarch64-apple-ios`, and `x86_64-apple-ios` for the simulator | Static library from the NDI SDK for Apple, linked at build time | Built against the NDI SDK for Apple 6.4 |
| tvOS | `aarch64-apple-tvos`, and `x86_64-apple-tvos` for the simulator | Static library from the NDI SDK for Apple, linked at build time | Built against the NDI SDK for Apple 6.4 |
| visionOS | `aarch64-apple-visionos` | Static library from the NDI SDK for Apple, linked at build time | Built against the NDI SDK for Apple 6.4 |

The test suites run in the repository's [CI](../ci/README.md), on GitHub's hosted runners. Compiled and linted means that `cargo clippy` with every feature and with warnings denied passes for the target; built means that the library builds for it. Nothing runs on those targets, and no NDI library is loaded.

Not supported, or not tested:

- Windows on ARM64. The NDI SDK for Windows ships x64 and x86 libraries only.
- The arm64 simulators: `aarch64-apple-ios-sim`, `aarch64-apple-tvos-sim` and `aarch64-apple-visionos-sim`. The arm64 archives in the NDI SDK for Apple are device builds. visionOS has no simulator target at all: the SDK's visionOS simulator archive is for x86_64, and Rust has no x86_64 visionOS target.
- Mac Catalyst. The NDI SDK for Apple has no Catalyst build.
- Every target not in the table, for example the `windows-gnu`, musl and BSD targets, and the Advanced SDK's builds for particular embedded boards. They have not been compiled or tested.

The minimum Rust version, 1.88, applies to every target in the table except tvOS and visionOS, for which Rust 1.88 distributes no standard library; a later stable release does. `x86_64-apple-tvos` has no distributed standard library at all and needs a nightly toolchain with `-Zbuild-std`. [`ci/apple-mobile.sh`](../ci/apple-mobile.sh) shows the commands.

### Apple mobile targets

iOS, tvOS and visionOS targets link the vendor's static archive into the application instead of loading a library. For those targets:

- `NDI_SDK_DIR` must name an NDI SDK for Apple. The build fails without it.
- The build links the archive for the target from the SDK's `lib/iOS`, `lib/tvOS` or `lib/visionOS` directory, together with the system frameworks it needs. With `file-io`, the File archive is linked in its place when the SDK supplies one.
- The archive must contain a slice for the target's CPU and platform. In the NDI SDK for Apple 6.4 the arm64 slices are device builds and the x86_64 slices are simulator builds.
- The 6.4 archives need iOS 16.0, tvOS 16.0 or visionOS 1.0. If `IPHONEOS_DEPLOYMENT_TARGET`, `TVOS_DEPLOYMENT_TARGET` or `XROS_DEPLOYMENT_TARGET` is set below the minimum the archive was built for, the build fails.
- The edition is that of the SDK: an NDI Advanced SDK for Apple links its Advanced archive, an NDI SDK for Apple its Standard one. `RuntimeOptions::library_path` is rejected with `Error::RuntimeConflict`, and selecting the runtime loads nothing.
