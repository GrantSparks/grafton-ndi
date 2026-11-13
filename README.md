# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi)
[![Documentation](https://docs.rs/grafton-ndi/badge.svg)](https://docs.rs/grafton-ndi)
[![CI](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml/badge.svg)](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE)
[![Minimum Rust Version](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org)

`grafton-ndi` is a Rust interface to the NDI® 6 SDK. It lets Rust applications discover NDI sources, receive and publish video, audio and metadata streams, monitor status and tally, control PTZ devices, and use NDI from synchronous or async code.

The crate loads the NDI runtime library when the program selects a runtime and wraps the SDK's objects in Rust ownership types. Building it needs no NDI SDK. Applications supply their renderer, mixer, encoder, storage layer, or UI.

## Documentation

**Evaluating the crate**

- [What it covers](#what-it-covers) - The SDK functionality the crate wraps, area by area.
- [Supported NDI versions and platforms](#supported-ndi-versions-and-platforms) - Which runtimes and targets were tested, which were only compiled, and which are unsupported.
- [Known issues](#known-issues) - Defects and limits of the NDI SDK, and what the crate does about each.
- [Design approach](#design-approach) - How the crate wraps the SDK: checked function tables, one runtime per process, cleanup through ownership and validated frames.
- [Performance and safety](#performance-and-safety) - Which frame APIs to use for which job, and the lifetime rules the compile-contract tests check.
- [SDK validation](docs/sdk-validation.md) - What was run against which runtime, the measurements behind the version requirements, and how to repeat them.
- [Changelog](CHANGELOG.md) - What 3.0 changes: compatibility, breaking changes, additions and fixes.

**Using the crate**

- [Quick start](#quick-start), [installation](#installation) and [core workflows](#core-workflows) - Finding and selecting the NDI runtime, then discovering, receiving and publishing.
- [API documentation](https://docs.rs/grafton-ndi) - Reference for every module and feature. Generate it for a checkout with `cargo doc --no-deps --all-features`.
- [Examples](#examples) - Runnable programs for each workflow.
- [Runtime capabilities and optional APIs](#runtime-capabilities-and-optional-apis) - What each runtime provides, and the routing, Discovery Server, Advanced and File APIs.
- [Advanced SDK guide](docs/advanced.md) - Requirements, ownership, compressed packets, queues, allocators, synchronization and KVM.
- [File API guide](docs/file-io.md) - Recording, playback, how a recording is finished, and the File library's limits.
- [Migrating from 2.0 to 3.0](docs/migration/2.0-to-3.0.md) - Every change existing code must make. [Guides for earlier versions](docs/migration/) are kept too.
- [Image encoding guide](docs/image-encoding.md) - Still images of any frame: formats, the SDK's color conversion and how the encoder follows it, HDR, reduction, cost and errors.
- [Image encoding performance](docs/image-encoding-performance.md) - Time and peak memory of every codec by source format and size, and the conversion scratch bounds.

**Contributing**

- [Contributing guide](CONTRIBUTING.md) - Setup, where changes belong, the SDK contracts the code relies on, and the checks before review.
- [CI guide](ci/README.md) - The workflows, the SDK test matrix, the loopback-only test runners and the release procedure.

## Supported NDI versions and platforms

`grafton-ndi` 3.0 works with NDI 6 runtimes from the Standard SDK and the Advanced SDK. It needs Rust 1.88 or later.

### NDI versions

| NDI SDK | Status for 3.0 | Editions tested |
| --- | --- | --- |
| 6.4.0.2 | Tested. The crate's bindings are generated from its headers. | Standard, Advanced |
| 6.3.2 | Tested. | Standard, Advanced |
| 6.1.1 | Tested. | Standard |
| Other NDI 6 releases, including 6.0 | Not tested. | |
| NDI 5 and earlier | Not supported. | |

A tested version passed the whole test suite on Linux x86_64. The runs also crossed versions: a build made with one version's SDK ran against another version's runtime, and senders and receivers of different versions exchanged video, audio and metadata. [SDK validation](docs/sdk-validation.md) lists every run.

The crate checks what the selected runtime provides before it calls into it. An operation that the runtime cannot perform returns `Error::UnsupportedCapability`, and the rest of the API keeps working. [Runtime capabilities](#runtime-capabilities-and-optional-apis) shows how to ask in advance. The Linux x86_64 runtimes report:

| Available | 6.1.1 | 6.3.2 | 6.4.0.2 |
| --- | :---: | :---: | :---: |
| Every API that 2.0 had: source discovery, receiving, sending, FrameSync, PTZ, tally and status | Yes | Yes | Yes |
| Routing, receiver connection metadata, the web-control URL, metadata from receivers to a sender, interleaved audio, V210/P216 conversion | Yes | Yes | Yes |
| `Receiver::poll_source_name_change` | No | Yes | Yes |
| Receiver video and audio delay, `Ndi::timestamp` | No | No | Yes |
| Planar 4:4:4 video formats, `ReceiverColorFormat::Best444`, the video-only bandwidth modes | No | No | Yes |
| Discovery Server advertisers and listeners | No | No | Yes |
| Advanced SDK operations (`advanced` feature), with an Advanced runtime | Not tested | Yes | Yes |
| File API (`file-io` feature) | No | No | Yes |

The 6.3.2 runtime exports the Discovery Server functions, but destroying an advertiser or listener there while it is connected to a server aborts the process, measured in every object type and with no call order that avoids it, so the crate refuses them before 6.4.0. [Known issues](#known-issues) lists this and the defects that 6.4 still has. The File library first ships with the 6.4 SDK. On Linux, macOS and Android it works only beside the Advanced runtime.

### Platforms

| Platform | Rust targets | How NDI is provided | Checked for 3.0 |
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

The test suites run in the repository's [CI](ci/README.md), on GitHub's hosted runners. Compiled and linted means that `cargo clippy` with every feature and with warnings denied passes for the target; built means that the library builds for it. Nothing runs on those targets, and no NDI library is loaded.

Not supported, or not tested:

- Windows on ARM64. The NDI SDK for Windows ships x64 and x86 libraries only.
- The arm64 simulators: `aarch64-apple-ios-sim`, `aarch64-apple-tvos-sim` and `aarch64-apple-visionos-sim`. The arm64 archives in the NDI SDK for Apple are device builds. visionOS has no simulator target at all: the SDK's visionOS simulator archive is for x86_64, and Rust has no x86_64 visionOS target.
- Mac Catalyst. The NDI SDK for Apple has no Catalyst build.
- Every target not in the table, for example the `windows-gnu`, musl and BSD targets, and the Advanced SDK's builds for particular embedded boards. They have not been compiled or tested.

The minimum Rust version, 1.88, applies to every target in the table except tvOS and visionOS, for which Rust 1.88 distributes no standard library; a later stable release does. `x86_64-apple-tvos` has no distributed standard library at all and needs a nightly toolchain with `-Zbuild-std`. [`ci/apple-mobile.sh`](ci/apple-mobile.sh) shows the commands.

### Apple mobile targets

iOS, tvOS and visionOS targets link the vendor's static archive into the application instead of loading a library. For those targets:

- `NDI_SDK_DIR` must name an NDI SDK for Apple. The build fails without it.
- The build links the archive for the target from the SDK's `lib/iOS`, `lib/tvOS` or `lib/visionOS` directory, together with the system frameworks it needs. With `file-io`, the File archive is linked in its place when the SDK supplies one.
- The archive must contain a slice for the target's CPU and platform. In the NDI SDK for Apple 6.4 the arm64 slices are device builds and the x86_64 slices are simulator builds.
- The 6.4 archives need iOS 16.0, tvOS 16.0 or visionOS 1.0. If `IPHONEOS_DEPLOYMENT_TARGET`, `TVOS_DEPLOYMENT_TARGET` or `XROS_DEPLOYMENT_TARGET` is set below the minimum the archive was built for, the build fails.
- The edition is that of the SDK: an NDI Advanced SDK for Apple links its Advanced archive, an NDI SDK for Apple its Standard one. `RuntimeOptions::library_path` is rejected with `Error::RuntimeConflict`, and selecting the runtime loads nothing.

## Known issues

These are defects and limits of the NDI SDK, not of this crate: measured on Linux x86_64 where a version is named, otherwise stated by the SDK's own documentation or API. None can be corrected from outside the SDK. Where a type or a check can keep an application clear of one, the crate does so; otherwise this list and the documentation of the affected methods say how to stay clear of it. [SDK validation](docs/sdk-validation.md#discovery-server-objects) and the [File API guide](docs/file-io.md#limits-of-the-ndi-64-file-library) record the measurements.

### Discovery Server

- **Destroying an object aborts the process on 6.3.x.** On a 6.3.2 runtime, destroying a `SendAdvertiser`, `RecvAdvertiser`, `SendListener` or `RecvListener` while it is connected to a server aborts the process ("pure virtual method called"). No order of calls avoids it, so the crate refuses all four before NDI 6.4.0 with `Error::UnsupportedCapability`. 6.4.0.2 never aborted in the same tests.
- **A quick re-registration crashes listeners (6.3.2 and 6.4.0.2).** Moving one sender or receiver from one advertiser to another on the same server within a few tens of milliseconds can crash a listener on that server, in any process. Wait about a second between unregistering and registering again, or drop the first advertiser before registering with the second.
- **An unsubscribe can block a receiver's connect for good (6.3.2 and 6.4.0.2).** When a listener, in this process or another, unsubscribes from a receiver that a `RecvAdvertiser` registers while that receiver connects, its connect can block forever. Drop the listener to stop receiving a receiver's events; this crate's listeners never unsubscribe on their own.
- **A send advertiser's destroy can hang (6.4.0.2).** Dropping a `SendAdvertiser` never returned once in about a million destroys under heavy load.
- **A listener that connects late misses earlier registrations (6.4.0.2).** An advertiser or listener that finds no server tries again every 10 seconds. A listener that connects on such a later attempt lists only the senders and receivers registered after it connected. Create listeners once their server is up, or replace a listener that took ten seconds or more to report `is_connected`.

### Receiving and sending

- **NDI|HX video needs a decoder the runtime can find (6.4).** Without one, a receiver of an NDI|HX source gets black 1920×1080 frames instead of the picture, and no error. On Linux the runtime decodes NDI|HX (H.264 and HEVC) only with the NDI|HX Driver for Linux, `libndihx.so` and its bundled FFmpeg libraries copied beside the runtime library, or with FFmpeg 7's `libavcodec` on the loader's path; NDI grants no H.264 or AAC licence with the driver. The Windows and macOS runtimes decode H.264 themselves. H.265 the SDK documents only in 4:2:0 and where decoding hardware supports it: macOS decodes HEVC 4:4:4, and Windows without a GPU delivers the black frames for it.
- **A FrameSync takes its receiver's audio.** Raw audio capture beside a FrameSync (measured with 6.3.2 and 6.4) takes the frames the FrameSync needs, and the FrameSync returns silence in their place. A FrameSync's receiver therefore has no audio capture.
- **The 32-bit x86 Linux runtime returns 3840×2160 video with wrong chroma in places (6.4).** A UYVY picture of that size, sent and received over loopback, comes back with the chroma inside flat areas off by up to 41 code values, and two receivers of one stream are not given the same picture. Its luma is intact, and so are 720×576 and 1920×1080 frames.
- **Standard encoding may convert 4:4:4 video to 4:2:2.** Sending a planar 4:4:4 frame does not guarantee that a receiver gets 4:4:4.
- **Sender and receiver names are reserved machine-wide** through lock files in the temporary directory (`NTK_SEND_<name>`, `NTK_RECV_<name>`). Two processes that share `TMPDIR` cannot each create a sender, or a receiver, of the same name at the same time.
- **Some waits cannot be cut short.** A `Genlock` clock's wait cannot be cancelled or given a deadline. Dropping a `QueuedSender` waits, without a timeout, for the SDK to finish with every queued frame. An async call whose SDK work has started runs to the end of that SDK call even if its future is dropped.
- **On Windows a receiver may never connect while a firewall blocks the program (6.3.2 and 6.4).** Even a receiver given an explicit loopback address: its connection count stays 0 and it opens no connection. With multicast DNS blocked this happens to receivers of any source, and giving the SDK a discovery address instead (`{"ndi": {"networks": {"discovery": "…"}}}` in its configuration) avoids it. With the program's other non-loopback traffic blocked it still happens to about half of the receivers of a source the process connected to before. Allow the program through the firewall; `Receiver::reconnect` revives most receivers that have not connected.
- **On Windows the 6.4 runtime's genlock does not acquire a reference** (Advanced). A `Genlock` connects to a reference sent from the same machine, whose sender counts the connection, but `is_active` stays false and its clocks keep to the system clock. The 6.3.2 runtime on Windows, and 6.4 on Linux and macOS, acquire the same reference.
- **Stream tier and stream selection need a licensed vendor ID** (Advanced). Without one, the SDK ignores `set_stream_tier` and `set_streams` and reports no change, which the crate cannot tell from "already in effect".

### File library (NDI 6.4)

- It works only beside the Advanced runtime on Linux, macOS and Android.
- A file reported closed is not proof that its data was written: when the disk fills while a session stops, the library still reports the file closed without an error.
- A write that fails while recording, for example on a full disk, crashes the process inside the library.
- A file can be closed with nothing in it, after a stop or a source switch before any frame arrived. `RecordingReport::empty_files` counts these.
- A rollover the library cannot carry out ends the session. `rollover` refuses the cases it can check first.
- The recorder's own getters for its source and its current file return strings that the library's recording thread can invalidate, so the crate does not call them, and `last_accepted_source` and `reported_current_file` follow the recorder's events instead.

## What It Covers

- **Runtime lifecycle** - Select the NDI runtime library, inspect it, then start and stop NDI through cheap, reference-counted `Ndi` handles.
- **Network discovery** - Discover sources, wait for source-list changes, search groups or extra IP addresses, and cache host/IP lookups.
- **Receiving** - Capture video, audio, and metadata with configurable bandwidth and color-format choices.
- **Sending** - Publish an NDI source and send video, audio, metadata, connection metadata, and failover information.
- **FrameSync** - Use NDI's clock-corrected pull API for playback, render loops, and audio-device driven workflows.
- **Monitoring** - Query receiver connection status, frame-drop statistics, tally state, and sender connection counts.
- **PTZ control** - Drive supported pan, tilt, zoom, focus, exposure, and white-balance commands.
- **Async integration** - Use optional Tokio or smol wrappers for receive workflows without blocking the async runtime.
- **Image snapshots** - Encode any video frame, in every uncompressed format, as PNG (8 or 16 bits), JPEG, lossless WebP or a data URL, with the SDK's color conversion, HDR tagging and reduction, with the `image-encoding` feature.
- **Routing and Discovery Server** - Redirect receivers with a routing source, register senders and receivers with a Discovery Server, list what it reports, and request receiver connections.
- **Media formats and controls** - Planar 4:4:4 formats, receiver color and bandwidth modes, delay controls, SDK timestamps, and checked audio and video conversions.
- **Advanced SDK** - Optional compressed and scatter streams, completion queues, custom receive allocation, JSON configuration, source metadata, encoder and keyframe controls, AVSync, Genlock, and KVM.
- **File API** - Optional recording of sources to files and playback of files as NDI sources.
- **Runtime capabilities** - Inspect the selected runtime and check whether each optional operation is available before using it.

## Quick Start

```rust
use grafton_ndi::{Finder, FinderOptions, Ndi};
use std::time::Duration;

fn main() -> grafton_ndi::Result<()> {
    let ndi = Ndi::new()?;

    let finder = Finder::new(&ndi, &FinderOptions::new())?;

    for source in finder.find_sources(Duration::from_secs(5))? {
        println!("Found source: {source}");
    }

    Ok(())
}
```

## Installation

Add the crate to `Cargo.toml`:

```toml
[dependencies]
grafton-ndi = "3.0"
```

Feature flags:

| Feature | Effect |
| --- | --- |
| `image-encoding` | PNG, JPEG, lossless WebP and data-URL encoding of video frames in every uncompressed format. Off by default. See the [image encoding guide](docs/image-encoding.md). |
| `tokio`, `smol` | Receiver adapters for the named async runtime. Each is independent. |
| `advanced` | The `advanced` module: NDI Advanced SDK operations. See [Advanced SDK](#advanced-sdk). |
| `file-io` | The `file` module: recording and playback through the NDI File API. See the [File API guide](docs/file-io.md). |

No feature is enabled by default; enable each one you need, for example `features = ["image-encoding"]`. Cargo features select Rust APIs only. They do not choose an SDK edition or a runtime library; see [SDK editions](#sdk-editions).

### Requirements

To build:

- Rust 1.88 or later. tvOS and visionOS need a later release; see [Platforms](#platforms).
- Nothing else for Windows, macOS, Linux and Android targets. The crate contains bindings generated from the SDK headers, so a build needs no NDI SDK and no `libclang`.

To run:

- An NDI 6 runtime library that the program can find; see [NDI versions](#ndi-versions) and [Finding the runtime](#finding-the-runtime). The [NDI SDK](https://ndi.video/for-developers/ndi-sdk/) contains the runtime library for each platform.

Two cases read an SDK at build time:

- The `advanced` feature generates declarations from the Advanced SDK headers when it finds an Advanced SDK. That step runs bindgen and needs a `libclang` shared library. See [Advanced SDK](#advanced-sdk).
- iOS, tvOS and visionOS targets link the SDK into the application. See [Apple mobile targets](#apple-mobile-targets).

### Finding the runtime

The runtime library is loaded when the program selects a runtime, not when the program starts. `Ndi::new()` selects the default runtime and starts NDI; [Selecting a runtime](#selecting-a-runtime) shows the two steps separately. The first usable library in this order is selected:

1. The directory that contains the running executable.
2. The directory named by the `NDI_RUNTIME_DIR_V6` environment variable. This is the variable the NDI SDK itself uses for the runtime directory.
3. The platform loader's own search for the library name: the executable's run path, `LD_LIBRARY_PATH` or `PATH`, and the system library directories. On macOS the name is first tried as `@rpath/<name>`.

Each location is tried for the library of either SDK edition, under the file names below, before the search moves to the next location. A build with the `advanced` feature, or with `file-io` on Linux, macOS or Android, where the File library needs the Advanced runtime, tries the Advanced name first at each location; other builds try the Standard name first. A program that needs one edition can ask for it; see [SDK editions](#sdk-editions). What is installed on the build machine plays no part.

A file that is not there is skipped. A library that cannot be loaded, that lacks a function every NDI 6 runtime exports, or that is not the requested edition is rejected, and the search continues.

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

When no usable library is found, `Ndi::new()` returns `Error::RuntimeLoadFailed`. Its message names every location tried and the operating system's reason for each:

```text
Failed to load an NDI library: no usable NDI runtime; tried /opt/app/libndi.so.6 (next to the executable): not found; /opt/app/libndi_advanced.so.6 (next to the executable): not found; libndi.so.6 (system library search): dlopen failed: libndi.so.6: cannot open shared object file: No such file or directory; libndi_advanced.so.6 (system library search): dlopen failed: libndi_advanced.so.6: cannot open shared object file: No such file or directory
```

A failed selection can be retried. A successful one is permanent: the selected library stays loaded until the process exits. A later request that it cannot meet returns `Error::RuntimeConflict`: one that names a different library, or one that requires the Advanced edition when the selected library is a Standard runtime.

One limit applies to the search. A library that loads but is rejected stays loaded, because a native library cannot be unloaded safely. If the platform loader's own search then matches the library name to that module, it returns the module again instead of looking further. The error names the rejected file. Remove or fix that file, or pass an explicit library path.

### Selecting a runtime

Using NDI takes two steps. `Runtime::select` loads the library and pins it for the rest of the process without starting NDI. `Runtime::initialize` starts NDI and returns the `Ndi` handle from which finders, senders and receivers are created. `Ndi::new()` does both with the default options, and `Ndi::with_options` with the options it is given. Nothing else selects a runtime.

Selecting first lets a program inspect the library before it starts it:

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

### SDK editions

NDI ships a Standard SDK and an Advanced SDK with different runtime libraries. Without a request, the search accepts the library of either edition, so a program finds whichever runtime the machine has. A request narrows the search to one library name:

- `RuntimeOptions::default().edition(SdkEdition::Advanced)` searches only for the Advanced library and fails unless the loaded library exports the Advanced SDK entry points. Use it when the program needs Advanced SDK operations.
- `RuntimeOptions::default().edition(SdkEdition::Standard)` searches only for the Standard library. An Advanced runtime found under that name, or named by an explicit path, also meets a Standard request.

The search does not depend on how or where the crate was built. `NDI_SDK_DIR` matters only to builds that read an SDK: the `advanced` feature and the Apple mobile targets.

`ndi.info().edition()` reports the edition of the library that was actually loaded, detected from the functions it exports. An edition says nothing about a license entitlement.

## Core Workflows

### Discover Sources

`Finder` wraps the NDI discovery API. It can return a current source snapshot, wait for source-list changes, or perform a bounded discovery pass.

```rust
use grafton_ndi::{Finder, FinderOptions};
use std::time::Duration;

let options = FinderOptions::new()
    .groups("Public,Studio")
    .extra_ips("192.168.1.100");
let finder = Finder::new(&ndi, &options)?;

if finder.wait_for_sources(Duration::from_secs(2))? {
    for source in finder.current_sources()? {
        println!("{source}");
    }
}
```

For applications that repeatedly reconnect to known devices, `SourceCache` handles discovery and exact host/IP matching, and keeps each source it finds until `invalidate()` or `clear()` removes it. Its lookups take the `Ndi` handle. Pass a complete bare hostname or IP address. Hostname ASCII case, an optional final dot, and equivalent numeric IP spellings share one cache entry. `len()` counts distinct host identities; `invalidate()` accepts any equivalent spelling. For searches by partial IP or source label, filter a `Finder` snapshot explicitly.

```rust
use grafton_ndi::SourceCache;
use std::time::Duration;

let cache = SourceCache::new();
let camera = cache.find_by_host(&ndi, "192.168.1.100", Duration::from_secs(5))?;
```

### Receive Streams

`Receiver` captures video, audio, and metadata from a selected `Source`. Use bandwidth and color-format options to match the role of the receiver, from low-bandwidth monitors to full-quality capture.

```rust
use grafton_ndi::{
    Receiver, ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions,
};
use std::time::Duration;

let options = ReceiverOptions::new(camera.clone())
    .color_format(ReceiverColorFormat::RgbxRgba)
    .bandwidth(ReceiverBandwidth::Highest);
let receiver = Receiver::new(&ndi, &options)?;

// Each capture waits up to its timeout, and returns `None` if nothing came.
if let Some(video) = receiver.video().capture(Duration::from_secs(5))? {
    println!(
        "{}x{} {:?}",
        video.width(),
        video.height(),
        video.pixel_format()
    );
}

let audio = receiver.audio().capture(Duration::from_millis(100))?;
let metadata = receiver.metadata().capture(Duration::from_millis(100))?;
```

Borrowed receiver and FrameSync video expose only initialized active pixels.
`frame.data()` returns `Some(&[u8])` when all active pixels are contiguous, and
`None` when row or plane padding separates them. Traverse padded frames without
allocation or pixel copies:

```rust
if let Some(frame) = receiver.video().capture_ref(Duration::from_secs(1))? {
    for plane in frame.planes() {
        for row in plane.rows() {
            // Each row contains active bytes only, with no SDK padding.
            println!("{} active bytes", row.len());
        }
    }
    let packed = frame.to_owned(); // one pixel allocation; active rows only
}
```

Owned capture and copied video sends pack planes tightly and can reduce the
source stride. Format, plane order, metadata, timing, aspect ratio, and scan type
are preserved. Owned frames expose the same `planes()` views, without padding.
So does a `BorrowedVideoFrame` built for sending: its views read the active
rows of a padded buffer in place, while its `data()` includes the padding.
Image encoding reads the active rows of every frame type in place.

`copy_into(&mut owned)` makes the same copy into an existing `VideoFrame` or
`AudioFrame`, on receiver and FrameSync references and on `BorrowedVideoFrame`.
The destination ends up exactly as `to_owned` would have made it, whatever
it held before, but keeps its pixel or sample buffer when that is large enough
and its metadata string when the text has the same length. A loop that copies
frames of one shape into the same destination allocates nothing after the
first copy.

`VideoPlane`, `VideoRows`, and `VideoPlanes` implement `Send + Sync`. You can
move preconstructed views into scoped workers or share them for immutable reads,
including views from FrameSync. The frame stays borrowed until the workers
finish; FrameSync frame references keep their capture-thread affinity.

Image encoding converts one row at a time from the active rows of owned,
captured and borrowed frames. PNG reads RGBA rows in place and uses at most one
converted row for anything else; JPEG converts straight into the codec's row
buffers; lossless WebP alone holds the whole converted image. Codec storage
and encoded output require additional memory.

### Publish Sources

`Sender` publishes a named NDI source and sends video, audio, and metadata. It also exposes connection count, tally (`tally()` for the current state, `wait_for_tally_change(timeout)` to wait for a change), failover, and connection metadata APIs. The Advanced `CompressedSender` and `QueuedSender` lend the same shared operations through `sender()`, as a `Sender` in another mode that has no uncompressed video sends and cannot be registered with an advertiser.

```rust
use grafton_ndi::{FrameRate, PixelFormat, Sender, SenderOptions, VideoFrame};
use std::time::Duration;

let options = SenderOptions::new("Rust Program Output")
    .clock_video(true)
    .clock_audio(true);
let sender = Sender::new(&ndi, &options)?;

let mut frame = VideoFrame::builder()
    .resolution(1920, 1080)
    .pixel_format(PixelFormat::Bgra)
    .frame_rate(FrameRate::new(60, 1)?)
    .build()?;
frame.data_mut().fill(0);

sender.send_video(&frame)?;

let connections = sender.connection_count(Duration::from_millis(500))?;
println!("connected receivers: {connections}");
```

To send pixels owned by another subsystem asynchronously without allocating a
frame each time, copy them into a frame the sender has returned.
`send_video_async` keeps the frame it is given until the next send, which
returns the previous frame once the SDK has finished with it; that frame is the
next copy's destination. Once the loop has made its two frames, the wrapper
allocates nothing more. `send_video_async_copy` makes the same copy into a new
frame on every call.

```rust
use grafton_ndi::{BorrowedVideoFrame, PixelFormat, Sender, SenderOptions, VideoFrame};

let mut sender = Sender::new(&ndi, &SenderOptions::new("Copied Output"))?;
let mut spare: Option<VideoFrame> = None;
// BGRA rows of 1920 pixels, padded to an 8192-byte stride.
while let Some(pixels) = renderer.next_frame() {
    let frame = BorrowedVideoFrame::builder(pixels, 1920, 1080, PixelFormat::Bgra)
        .line_stride(8192)
        .build()?;
    let owned = match spare.take() {
        Some(mut reused) => {
            frame.copy_into(&mut reused); // reuses its pixels and metadata
            reused
        }
        None => frame.to_owned(), // only the first two frames allocate
    };
    spare = sender.send_video_async(owned)?;
}
sender.finish_video_async();
```

### Use FrameSync

`FrameSync` is for pull-based capture when your application has its own clock: a GPU vsync loop, audio callback, timeline, or mixer. Video capture returns the frame appropriate for the requested time base, and audio capture can resample to the requested output shape.

```rust
use grafton_ndi::{FrameSync, FrameSyncAudioRequest, ScanType};

let frame_sync = FrameSync::new(receiver)?;

// Borrow the SDK's frame; `capture_video` returns an owned copy instead.
if let Some(video) = frame_sync.capture_video_ref(ScanType::Progressive)? {
    println!("video: {}x{}", video.width(), video.height());
}

// 1024 stereo samples at 48 kHz, converted from the source's audio.
let request = FrameSyncAudioRequest::capture_at(48_000, 2, 1024)?;
let audio = frame_sync.capture_audio_ref(request)?;
println!("audio: {} channels at {} Hz", audio.num_channels(), audio.sample_rate());
```

An audio capture always returns samples in the requested shape, silence until
the source's audio arrives. `frame_sync.audio_input_format()` reports the
source's sample rate and channel count, or `None` before any audio arrives; to
follow the source's format, wait for `Some` and build the request with
`FrameSyncAudioRequest::for_input(format, 1024)`; query again and rebuild to
follow later changes.

`FrameSync::new` hands the receiver back inside `Rejected` when the SDK creates no
synchronizer. `FrameSync` owns its receiver. `frame_sync.receiver()` provides controls,
metadata capture, events, and status through `Receiver<FrameSyncAttached>`. The
synchronizer takes the receiver's video and audio, so that receiver has no raw
video or audio capture: a raw audio capture beside a FrameSync would take audio
frames that the FrameSync then replaces with silence. Use
`frame_sync.into_receiver()` to recover an ordinary `Receiver` with raw video
and audio capture.

### Monitor and Control

Receivers report connection health and frame-drop statistics, and control a PTZ camera through `receiver.ptz()`. Use `set_tally` to send program/preview state upstream, where the SDK keeps it for every connection; sender tally reports receiver state. `send_metadata` sends metadata upstream and returns `Error::NotConnected` when there is no source to send it to.

```rust
use grafton_ndi::ReceiverEvent;
use std::time::Duration;

if receiver.is_connected() {
    let stats = receiver.connection_stats();
    println!("video drop rate: {:.2}%", stats.video_drop_percentage());
}

if let Some(ReceiverEvent::StatusChanged(status)) = receiver.poll_event(Duration::from_millis(100))? {
    println!("Active connections: {}", status.connections());
}

let ptz = receiver.ptz();
if ptz.is_supported() {
    ptz.zoom(0.25)?;
}
```

`poll_event` spends its timeout as the capture verbs do: in attempts of at most 100 ms, waiting across the SDK's early returns, and returning `None` only once the whole timeout is spent. Every capture holds the receiver's shared capture guard for one such attempt, so `reconnect`, `connect_to` and `disconnect` wait about 100 ms at most for captures in flight, however long their timeouts. The controls work in every receiver mode, including a FrameSync's receiver and, with the `advanced` feature, the `Receiver<PacketAttached>` that `PacketReceiver::receiver()` returns and the `Receiver<AvSyncAttached>` that `AvSync::receiver()` returns, which also keeps its own video and metadata capture and `poll_event`.

### Integrate With Async Runtimes

NDI receive calls are fundamentally blocking SDK calls. The optional async wrappers make that explicit by running receive work on the runtime's blocking pool while preserving the timeout budget from the future's first poll. `capture`, `poll_event` and `poll_source_name_change` include blocking-pool queue delay and reconnect-lock contention in the same deadline, and return `None` only once all of it is spent. Dropping a future suppresses work before its cancellation gate; a task that has passed that gate cannot be interrupted and may still consume a frame or an event, even if it is waiting for the reconnect lock. Queued work can still finish after the budget expires; these wrappers do not impose a wall-clock completion deadline.

`grafton_ndi::asynchronous::AsyncReceiver<R>` and its `AsyncCapture` views are generic over the runtime, `Tokio` or `Smol` (one per Cargo feature); `asynchronous::tokio` and `asynchronous::smol` name them for one runtime.

A status or source change that an async capture passes over is kept for `poll_event`, as for the synchronous receiver. `connect_to`, `disconnect` and `reconnect` wait for the capture attempts in flight, so they run on the blocking pool too. `receiver()` lends the wrapped `Receiver` for the controls that do not wait, such as tally, PTZ and upstream metadata; its waiting calls block the calling thread, so use the async methods for those.

```rust
use grafton_ndi::{asynchronous::tokio::AsyncReceiver, ReceiverEvent, Tally};
use std::time::Duration;

let async_receiver = AsyncReceiver::new(receiver);
async_receiver.receiver().set_tally(Tally::new(true, false));
if let Some(frame) = async_receiver.video().capture(Duration::from_secs(5)).await? {
    println!("{}x{}", frame.width(), frame.height());
}
if let Some(ReceiverEvent::StatusChanged(status)) = async_receiver.poll_event(Duration::ZERO).await? {
    println!("Active connections: {}", status.connections());
}
```

## API Model

The crate's public API is organized around a small set of resource types:

- `Runtime` is the selected NDI runtime library; `Ndi` is a handle that keeps NDI started.
- `Finder` discovers `Source` values on the network.
- `Receiver` connects to a source and captures video, audio, and metadata.
- `Sender` publishes an NDI source.
- `FrameSync` wraps a receiver for clock-corrected pull capture.
- `VideoFrame`, `AudioFrame`, and `MetadataFrame` represent application-owned frame data.

Most applications can start with owned frame APIs such as `receiver.video().capture()` and `sender.send_video()`. For receive hot paths, borrowed receive refs keep data in SDK buffers without an extra copy. `sender.send_video_async(frame)`, the SDK's asynchronous video send, takes `&mut Sender` and moves an owned `VideoFrame` into the sender without copying pixels. It returns the previous completed frame for reuse, and `Ok(None)` on the first call; a frame the sender refuses comes back inside `Rejected`. This avoids Rust wrapper pixel copies; SDK encoding and network processing may still copy data. Use two frames and call `finish_video_async()` to recover the final frame. For borrowed or padded-stride buffers, build a `BorrowedVideoFrame` and call `send_video(frame)`: the synchronous send reads the pixels and metadata in place without allocating or copying, and the SDK has released the buffer when the call returns, so it can be reused immediately. The one exception is a UYVY or UYVA buffer that ends with the frame: the x86 builds of the SDK read up to 28 bytes past the last pixel of many frames of those formats, so the sender copies such a frame into a buffer it keeps, which costs about 15% of a 1080p send. A buffer `PixelFormat::send_tail_room()` bytes longer than the frame is sent in place, as is one whose caller vouches for readable memory after it with `BorrowedVideoFrameBuilder::readable_after`. `send_video_async_copy(&frame)` is for the asynchronous pipeline: it copies pixels and metadata into sender-owned storage and returns the previous frame as `Result<Option<VideoFrame>>`. To copy without allocating, call `frame.copy_into(&mut previous)` on the frame the last send returned and submit it with `send_video_async`. Owned frames are `Clone`; cloning copies their pixels.

Frames carry time as typed values in 100-nanosecond ticks: `Timecode` is the editorial timecode, which starts at `Timecode::SYNTHESIZE` (the SDK generates one) on every frame built for sending, and `SdkTimestamp` is the time the sender submitted a frame, which received frames report as `Option<SdkTimestamp>`. Frame rates are `FrameRate` values, and a pixel format converts to and from its `FourCc`.

Frame layout fields that describe SDK-facing memory are private. Builders, accessors, checked mutation methods, and `PixelFormat` helpers keep dimensions, strides, metadata, and buffer sizes consistent before data crosses the FFI boundary.

## Design Approach

`grafton-ndi` aims to be a predictable Rust layer over NDI, not a replacement for the NDI SDK documentation.

- **Typed function tables**: every native call goes through a table of functions resolved once: when the runtime is selected, or when `FileApi::open` loads the File library. Each table's signatures are checked at compile time against bindings generated from the SDK headers.
- **One runtime per process**: the selected library stays loaded until the process exits, while `Ndi` handles start and stop the SDK by reference count.
- **RAII lifecycle**: `Ndi` handles, senders, receivers, frames, and pending async sends clean up through ownership.
- **Initialized receive views**: borrowed video rows and audio channels expose active payload only; `data()` is optional for padded layouts. Owned capture packs active payload without reading SDK padding.
- **Checked frame descriptions**: frame dimensions, line strides, channel strides, metadata strings, and buffer sizes are validated before slices or strings are exposed.
- **Explicit blocking behavior**: timeout-bearing calls validate their wait budgets; SDK sends and drains have no deadline. Async adapters run SDK work on a blocking pool.
- **Forward-compatible SDK enums**: public SDK-mode enums that may grow are `#[non_exhaustive]`.

## Performance and Safety

- Use owned frame APIs when clarity matters or when frame data must outlive the SDK capture buffer.
- Use borrowed receive refs when you need direct access to SDK-owned buffers during a tight capture loop.
- Use two owned video frames with `send_video_async` to reuse pixel allocations without per-send copies; send a `BorrowedVideoFrame` over an external or padded buffer with the synchronous `send_video`, which copies nothing, and use `send_video_async_copy` only when the asynchronous pipeline is wanted for a borrowed buffer, accepting the copy. `copy_into` copies a borrowed, received or FrameSync frame into an existing owned frame, reusing its storage, so copy loops into the frames `send_video_async` returns allocate nothing once two frames exist.
- Use `FrameSync` when capture timing is driven by an external output clock.
- Use lower bandwidth modes for previews and monitoring surfaces.
- Keep image encoding off real-time paths unless the workload is sized for compression.

The compile-contract tests in `tests/ui` verify the important lifetime rules around borrowed receive refs and owned async submissions and immediate source reuse after copied sends.

## Image Encoding

With the `image-encoding` feature, every video frame type encodes itself as a
PNG, JPEG or lossless WebP image, or a data URL of one. Every uncompressed
pixel format is supported:

```rust
use grafton_ndi::{Downscale, ImageFormat, ImageOptions, PngCompression};

let png = video.encode_png()?;
let jpeg = video.encode_jpeg(85)?;

// Fast lossless snapshots of camera video, and a quarter-size thumbnail.
let snapshot = video.encode_image(&ImageOptions::new(ImageFormat::WebpLossless))?;
let thumbnail = ImageOptions::new(ImageFormat::Jpeg(80)).downscale(Downscale::Quarter);
let data_url = video.encode_image_data_url(&thumbnail)?;
```

YUV frames are converted the way the NDI SDK was measured to convert: BT.709,
limited range and repeated chroma, unless the frame's `ndi_color_info`
metadata names another matrix, which the encoder then follows. `ColorPolicy::Override` replaces the matrix
and range. A frame tagged PQ or HLG is refused unless `HdrPolicy::CodeValues`
asks for its code values, which a PNG carries with a `cICP` tag;
`SampleDepth::Sixteen` keeps a high-bit-depth frame's precision. PNG's default
compression is slow on camera video, about half a second for a 1080p frame;
`PngCompression::Fast` and lossless WebP take about 20 ms. See the
[image encoding guide](docs/image-encoding.md).

## Runtime capabilities and optional APIs

`runtime.info()`, or `ndi.info()` on a handle, gives the identity of the selected library: its path, edition, version and whether it is shared or statically linked.

Runtimes differ in what they provide. `capabilities()` reports each optional capability, and `require_feature` turns an unavailable one into an error before any native call is made. Both are available on `Runtime` and on `Ndi`:

```rust
use grafton_ndi::capabilities::{CapabilityStatus, RuntimeFeature};

match ndi.capabilities().status(RuntimeFeature::ReceiveDelay) {
    CapabilityStatus::Available => println!("receiver delay controls can be used"),
    CapabilityStatus::Unavailable(reason) => println!("no receiver delay: {reason}"),
    _ => {}
}

// The same check as an error.
ndi.require_feature(RuntimeFeature::SenderDiscovery)?;
```

An operation whose capability is unavailable returns `Error::UnsupportedCapability` with the capability as a `RuntimeFeature` in `feature`, the reason, which is the `CapabilityUnavailable` that `capabilities().status(feature)` reports, and the evidence in `detail`: the function the runtime does not export, the version it reports against the one required, or the declaration the build lacks. Every capability an operation can be refused for is a `RuntimeFeature`, so `capabilities()` reports all of them in advance, and an operation is refused exactly when `require_feature` refuses its feature. Other operations keep working. A capability is unavailable for one of these reasons:

| Reason | Meaning |
| --- | --- |
| `SymbolUnavailable` | The runtime does not export a function the capability needs. |
| `RuntimeTooOld` | The runtime's version is below the capability's minimum. |
| `RuntimeVersionUnknown` | The capability has a minimum version and the runtime's version text was not recognized. |
| `WrongEdition` | The capability belongs to the Advanced SDK and the runtime is a Standard one. |
| `BuildAbiUnavailable` | The capability belongs to the Advanced SDK, and the crate was built without the `advanced` feature or without Advanced SDK headers that declare it. |

The capabilities with a minimum version need NDI 6.4.0:

- `Planar444Video`, `ReceiveBest444` and `ReceiveVideoOnly`, because an older runtime does not know those values.
- `SenderAdvertising`, `ReceiverAdvertising`, `SenderDiscovery` and `ReceiverDiscovery`, and the Advanced `AdvancedSenderAdvertiserConfiguration`, `AdvancedReceiverAdvertiserConfiguration`, `AdvancedSenderListenerConfiguration` and `AdvancedReceiverListenerConfiguration`, because destroying one of those objects aborts a 6.3.2 process (see [Known issues](#known-issues)).

The Advanced SDK capabilities, which the [Advanced SDK guide](docs/advanced.md#capabilities) lists, also need an Advanced SDK runtime and a build that declares them. The others depend only on what the runtime exports. The [table of NDI versions](#ndi-versions) shows what each tested runtime provides. Availability says nothing about hardware or license requirements.

Planar 4:4:4 frames (`PixelFormat::Na48`, `Nb48`, `Na4g` and `Nb4g`) are built, captured and copied like any other format, without a runtime. Sending one needs a runtime with `Planar444Video`: `send_video`, `send_video_async`, `send_video_async_copy` and `QueuedSender::try_submit` check the sender's runtime first and return `Error::UnsupportedCapability`, sending nothing and keeping any pending frame, when it lacks the capability. Standard encoding may convert 4:4:4 inputs to 4:2:2.

`Receiver::set_video_delay` and `set_audio_delay` need `ReceiveDelay`. They accept at most 2000 ms, round positive fractional milliseconds upward, and report whether the native value changed as a `SettingChange`.

`audio_conversion` provides checked conversions between planar and interleaved audio, with explicit reference levels for integer samples, and `Sender::send_audio_interleaved_i16`, `_i32` and `_f32`. `video_conversion` provides V210/P216 conversions for complete six-pixel V210 groups.

### Routing

A `Router` is an NDI source that carries no media. Receivers connect to it by name and are redirected to whichever source it currently routes to:

```rust
use grafton_ndi::routing::{Router, RouterOptions};

let mut router = Router::new(&ndi, &RouterOptions::new("Program"))?;
router.change(&camera)?;
// Later: stop routing.
router.clear()?;
```

`Router::new` returns `Error::UnsupportedCapability` when the runtime does not export the routing functions.

### Discovery Server

The `discovery` module works with an NDI Discovery Server, which keeps a registry of senders and receivers. `SendAdvertiser` and `RecvAdvertiser` register this process's senders and receivers. `SendListener` and `RecvListener` list what is registered, subscribe to events and, for receivers that allow it, request a source change. All four need an NDI 6.4.0 or later runtime.

```rust
use grafton_ndi::discovery::{ListenerOptions, RecvListener};
use std::time::Duration;

let options = ListenerOptions::default().server("127.0.0.1");
let mut listener = RecvListener::new(&ndi, &options)?;
// The listener connects in the background.
listener.wait_for_changes(Duration::from_secs(2))?;
if !listener.is_connected() {
    println!("no answer from the Discovery Server yet");
}
for receiver in listener.snapshot()? {
    println!("{:?} at {:?}", receiver.name(), receiver.address());
}
```

Each object is created for a server address: the one in its options, or else the one in the NDI configuration. Apart from an older runtime and invalid option text, creation fails only when there is no address or the SDK cannot parse it. A server that cannot be reached is not an error. The object keeps trying in the background and starts working a few seconds after the server appears. A listener reports its connection through `is_connected`, and its snapshots are empty until it connects. An advertiser reports nothing. `RecvListener::request_connect` and `request_disconnect` send a request only to a receiver the listener lists as accepting it, and otherwise return `Error::ReceiverRequestNotAccepted`. Its `advertised` field is `false` when no listed receiver has the identifier, which a later request can change, for example once the listener has connected.

An advertiser owns its registrations. `register` returns a token, and the registered sender or receiver stays alive until `unregister` is called with that token or the advertiser is dropped, even if the application drops its own handle first. A token given to an advertiser that did not issue it comes back inside `Rejected`, as does one whose object the SDK does not remove, which fails with `Error::NativeOperationFailed` and changes nothing.

Names and metadata from the network are decoded lossily, so one malformed peer cannot fail a snapshot. Identifiers are the exception: a `SenderId`, `ReceiverId` or `InputGroupId` keeps the server's exact bytes, so passing one back always names the same peer, and each kind is its own type. One snapshot or event batch is limited to 16 MiB, and one string to less than 4 MiB. A larger one returns `Error::UnexpectedSdkResult`, and the events of a refused batch are lost. `Router`, the advertisers and the listeners are `Send` and not `Sync`.

### Advanced SDK

The `advanced` feature adds the `advanced` module. Using it needs three things:

1. The `advanced` Cargo feature.
2. An NDI Advanced SDK on the build machine, in `NDI_SDK_DIR` or a default installation directory. The Advanced SDK headers may not be redistributed, so the build generates their declarations, types and constants locally with bindgen, which needs `libclang`; the crate itself carries only the signatures of the Advanced functions it calls. A build that finds an Advanced SDK and no `libclang` fails.
3. An Advanced SDK runtime selected when the program runs.

The module's API is the same whether or not the build found the Advanced SDK. Without it, or with a Standard runtime, each native operation returns `Error::UnsupportedCapability` with the reason. Older Advanced SDK headers disable only the operations they do not declare. The data types in the module, such as packets, envelopes and options, work in every build. The Advanced SDK 6.3.2 and 6.4.0.2 were tested; see [NDI versions](#ndi-versions).

An operation that takes ownership of a receiver, a sender or a frame hands it back inside `grafton_ndi::Rejected<T>` when it fails. Custom receive allocators require `ndi.process_lifetime()`, an explicit opt-in that keeps NDI started until the process exits. See the [Advanced SDK guide](docs/advanced.md).

### File API

The `file-io` feature adds the `file` module, which records NDI sources to disk and plays recordings back as NDI sources through the NDI File library. The File library is a separate native library that ships with the NDI SDK beside the runtime library. `FileApi::open` finds it when it is first needed, searching the selected runtime's directory, then the executable's directory, then `NDI_RUNTIME_DIR_V6`, then the platform loader's path. Nothing else in the crate depends on it.

```rust
use grafton_ndi::file::{FileApi, FileRuntimeOptions, PlayerOptions};

let file = FileApi::open(&ndi, &FileRuntimeOptions::default())?;
let info = file.probe("take_001.mkv")?;
println!("{} track(s)", info.track_count());

let mut player = file.create_player(&PlayerOptions::new("Replay"))?;
player.start("take_001.mkv")?;
```

The File library ships with the NDI 6.4 SDK. On Linux, macOS and Android it works only with the Advanced SDK runtime, and `FileApi::open` refuses it beside a Standard runtime before loading anything. On Windows it ships for x64 only and works with either edition. A library that provides only one of the two families of File entry points, playback and recording, is selected for that one: `FileApi::supports(FileFamily::Recording)` says whether it records, and an operation of a family it lacks returns `Error::UnsupportedCapability`. A recorder stores a source's compressed data as it arrives and takes no frames from Rust. Treat a recording as complete only when `FileRecorder::finish` returns `FinishStatus::Closed`. See the [File API guide](docs/file-io.md).

## Examples

Run an example with `cargo run --example <name>`. Most accept extra IP addresses to search as arguments. The comment at the top of each file says what it does and lists its options.

Discovery and monitoring:

| Example | Command | What it shows |
| --- | --- | --- |
| `runtime_selection.rs` | `cargo run --example runtime_selection` | Select a runtime, inspect it, then start NDI. |
| `NDIlib_Find.rs` | `cargo run --example NDIlib_Find` | Discover NDI sources on the network. |
| `status_monitor.rs` | `cargo run --example status_monitor` | Print a receiver's connection status changes. |
| `routing.rs` | `cargo run --example routing` | Route receivers to each discovered source in turn. |
| `discovery_server.rs` | `cargo run --example discovery_server -- 127.0.0.1:5959` | Register a sender with a Discovery Server and list what the server reports. Needs a Discovery Server and an NDI 6.4.0 or later runtime. |

Receiving:

| Example | Command | What it shows |
| --- | --- | --- |
| `NDIlib_Recv_Audio.rs` | `cargo run --example NDIlib_Recv_Audio` | Receive and inspect audio. |
| `NDIlib_Recv_Audio_16bpp.rs` | `cargo run --example NDIlib_Recv_Audio_16bpp` | Receive audio and convert it to 16-bit samples. |
| `NDIlib_Recv_FrameSync.rs` | `cargo run --example NDIlib_Recv_FrameSync` | Clock-corrected capture with `FrameSync`. |
| `NDIlib_Recv_PNG.rs` | `cargo run --features image-encoding --example NDIlib_Recv_PNG` | Receive video and save a PNG. Needs the `image-encoding` feature. |
| `NDIlib_Recv_PTZ.rs` | `cargo run --example NDIlib_Recv_PTZ` | Control PTZ cameras. |
| `concurrent_capture.rs` | `cargo run --example concurrent_capture` | Capture video, audio and metadata from one receiver on separate threads. |

Sending:

| Example | Command | What it shows |
| --- | --- | --- |
| `NDIlib_Send_Audio.rs` | `cargo run --example NDIlib_Send_Audio` | Send audio. |
| `NDIlib_Send_Video.rs` | `cargo run --example NDIlib_Send_Video` | Send video. |
| `async_send.rs` | `cargo run --example async_send` | Reuse two owned frames for async sending and recover completed frames. |
| `async_send_copy.rs` | `cargo run --example async_send_copy` | Copy borrowed pixels into async sends, then into the frames they return. |
| `zero_copy_send.rs` | `cargo run --example zero_copy_send` | Render into two reusable owned frames without per-send pixel copies. |

Advanced SDK. These need the `advanced` feature, an Advanced SDK at build time and an Advanced runtime:

| Example | Command | What it shows |
| --- | --- | --- |
| `advanced_source_metadata.rs` | `cargo run --features advanced --example advanced_source_metadata` | List sources with the metadata they advertise. |
| `advanced_passthrough.rs` | `cargo run --features advanced --example advanced_passthrough` | Relay compressed video and audio without decoding. |
| `advanced_queued_send.rs` | `cargo run --features advanced --example advanced_queued_send` | Queue video frames and reuse each once it has been sent. |
| `advanced_allocator.rs` | `cargo run --features advanced --example advanced_allocator` | Receive frames into memory the application supplies. |
| `advanced_avsync.rs` | `cargo run --features advanced --example advanced_avsync` | Fetch the audio that belongs to each video frame. |
| `advanced_genlock.rs` | `cargo run --features advanced --example advanced_genlock` | Pace a sender from another source's clock. |
| `advanced_kvm.rs` | `cargo run --features advanced --example advanced_kvm` | Remote keyboard and mouse control of a source that offers it. |

File API. These need the `file-io` feature and the NDI File library of the 6.4 SDK; on Linux and macOS that means the Advanced SDK runtime:

| Example | Command | What it shows |
| --- | --- | --- |
| `file_record.rs` | `cargo run --example file_record --features file-io -- "MACHINE (Source)" take 10` | Record a source to disk and wait for the file to be finalized. |
| `file_play.rs` | `cargo run --example file_play --features file-io -- take_001.mkv "Replay"` | Play a recording back as an NDI source. |
| `file_thumbnails.rs` | `cargo run --example file_thumbnails --features file-io,image-encoding -- take_001.mkv 8` | Seek through a recording and write a quarter-size JPEG thumbnail at each position. |

## Development

Building and the ordinary checks need no NDI SDK:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --examples --all-features
cargo test --all-features
```

`cargo test` runs the tests that need a real NDI runtime when one can be loaded. When none can, each of those tests is skipped and says so on standard error:

```text
skipped: frames::tests::planar_444_formats_are_sendable_only_where_the_runtime_supports_them needs an NDI runtime
```

Those tests create senders, receivers and finders, which announce themselves on the local network like any NDI program.

Set `GRAFTON_NDI_REQUIRE_RUNTIME` to turn skips into failures. Its value is a comma-separated list of what the environment provides:

| Entry | The environment provides |
| --- | --- |
| `1` | An NDI runtime library. Every other entry implies it. |
| `advanced` | An Advanced SDK runtime, and a build made with the Advanced SDK headers. |
| `file` | An NDI File library that the selected runtime can use. |
| `discovery-server` | The SDK's `ndi-discovery-server` program beside the selected runtime, and an NDI 6.4.0 or later runtime. |
| `h264-decoder` | A decoder for NDI\|HX H.264 video: on macOS and Windows, the runtime's own; on Linux, the NDI\|HX Driver for Linux or FFmpeg 7's `libavcodec` beside the runtime or on the loader's path. A runtime that delivers only its black 1920×1080 placeholder frames counts as having none. |
| `hevc-444-decoder` | A decoder for NDI\|HX HEVC 4:4:4 video, as for `h264-decoder`, and an NDI 6.4.0 or later runtime. On Windows the runtime's own HEVC decoding depends on the machine, and GitHub's hosted Windows Server 2025 runners lacked it. |

An entry that is not in this table fails the run. So does an entry that no test in the build can check, for example `advanced` without the `advanced` feature, or `discovery-server` on a target other than Linux.

For example, with an NDI SDK installed on Linux x86_64:

```bash
export NDI_RUNTIME_DIR_V6="/usr/share/NDI SDK for Linux/lib/x86_64-linux-gnu"
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo test --all-features
```

Adding `--config ci/offline.toml` starts every test executable through a wrapper that confines its networking. On Linux, where it needs `bubblewrap`, the tests get loopback networking only, so nothing is announced on the network the machine is attached to. The macOS and Windows wrappers confine less; the [CI guide](ci/README.md#loopback-only-test-runs) describes each one.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development setup, the bindings generator and contribution guidelines.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

The bindings in `src/sys/bindings.rs` and `src/sys/declarations.rs` are generated from NDI SDK headers that the vendor distributes under the MIT license. Each file starts with that copyright and permission notice, and the package's license expression is therefore `Apache-2.0 AND MIT`.

The NDI SDK and runtime are supplied separately under their vendor terms.

## Disclaimer

This is an unofficial community project and is not affiliated with NewTek or Vizrt.

NDI® is a registered trademark of Vizrt NDI AB.
