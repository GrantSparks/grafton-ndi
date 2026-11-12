# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi)
[![Documentation](https://docs.rs/grafton-ndi/badge.svg)](https://docs.rs/grafton-ndi)
[![CI](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml/badge.svg)](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE)
[![Minimum Rust Version](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org)

`grafton-ndi` is a Rust interface to the NDI® 6 SDK. It lets Rust applications discover NDI sources, receive and publish video, audio and metadata streams, monitor status and tally, control PTZ devices, and use NDI from synchronous or async code.

The crate loads the NDI runtime library when the program selects a runtime and wraps the SDK's objects in Rust ownership types. Building it needs no NDI SDK. Applications supply their renderer, mixer, encoder, storage layer, or UI.

## What It Covers

- **Runtime lifecycle** - Select the NDI runtime library, inspect it, then start and stop NDI through cheap, reference-counted `NDI` handles.
- **Network discovery** - Discover sources, wait for source-list changes, search groups or extra IP addresses, and cache host/IP lookups.
- **Receiving** - Capture video, audio, and metadata with configurable bandwidth and color-format choices.
- **Sending** - Publish an NDI source and send video, audio, metadata, connection metadata, and failover information.
- **FrameSync** - Use NDI's clock-corrected pull API for playback, render loops, and audio-device driven workflows.
- **Monitoring** - Query receiver connection status, frame-drop statistics, tally state, and sender connection counts.
- **PTZ control** - Drive supported pan, tilt, zoom, focus, exposure, and white-balance commands.
- **Async integration** - Use optional Tokio or smol wrappers for receive workflows without blocking the async runtime.
- **Image snapshots** - Encode captured video frames as PNG, JPEG, or data URLs with the default `image-encoding` feature.
- **Routing and Discovery Server** - Redirect receivers with a routing source, register senders and receivers with a Discovery Server, list what it reports, and request receiver connections.
- **Media formats and controls** - Planar 4:4:4 formats, receiver color and bandwidth modes, delay controls, SDK timestamps, and checked audio and video conversions.
- **Advanced SDK** - Optional compressed and scatter streams, completion queues, custom receive allocation, JSON configuration, AVSync, Genlock, and KVM.
- **File API** - Optional recording of sources to files and playback of files as NDI sources.
- **Runtime capabilities** - Inspect the selected runtime and check whether each optional operation is available before using it.

## Quick Start

```rust
use grafton_ndi::{Finder, FinderOptions, NDI};
use std::time::Duration;

fn main() -> grafton_ndi::Result<()> {
    let ndi = NDI::new()?;

    let finder = Finder::new(
        &ndi,
        &FinderOptions::builder()
            .show_local_sources(true)
            .build(),
    )?;

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
| `image-encoding` | PNG, JPEG and data-URL encoding of video frames. Enabled by default. |
| `tokio`, `smol` | Receiver adapters for the named async runtime. Each is independent. |
| `advanced` | The `advanced` module: NDI Advanced SDK operations. See [Advanced SDK](#advanced-sdk). |
| `file-io` | The `file` module: recording and playback through the NDI File API. See the [File API guide](docs/file-io.md). |

Use `default-features = false` for a build without image encoding. Cargo features select Rust APIs only. They do not choose an SDK edition or a runtime library; see [SDK editions](#sdk-editions).

### Requirements

To build:

- Rust 1.88 or later.
- Nothing else for Windows, macOS, Linux and Android targets. The crate contains bindings generated from the SDK headers, so a build needs no NDI SDK and no `libclang`.

To run:

- An NDI 6 runtime library that the program can find; see [Finding the runtime](#finding-the-runtime). The [NDI SDK](https://ndi.video/for-developers/ndi-sdk/) contains the runtime library for each platform.

Two cases read an SDK at build time:

- The `advanced` feature generates declarations from the Advanced SDK headers when it finds an Advanced SDK. That step runs bindgen and needs a `libclang` shared library. See [Advanced SDK](#advanced-sdk).
- iOS, tvOS and visionOS targets link the SDK into the application. See [Apple mobile targets](#apple-mobile-targets).

### Finding the runtime

The runtime library is loaded when the program selects a runtime, not when the program starts. `NDI::new()` selects the default runtime and starts NDI; [Selecting a runtime](#selecting-a-runtime) shows the two steps separately. The first usable library in this order is selected:

1. The directory that contains the running executable.
2. The directory named by the `NDI_RUNTIME_DIR_V6` environment variable. This is the variable the NDI SDK itself uses for the runtime directory.
3. The platform loader's own search for the library name: the executable's run path, `LD_LIBRARY_PATH` or `PATH`, and the system library directories. On macOS the name is first tried as `@rpath/<name>`.

A directory that does not contain the library is skipped. A library that cannot be loaded, that lacks a function every NDI 6 runtime exports, or that is not the requested edition is rejected, and the search continues.

The library file names are:

| Platform | Standard SDK | Advanced SDK |
| --- | --- | --- |
| Windows x64 | `Processing.NDI.Lib.x64.dll` | `Processing.NDI.Lib.Advanced.x64.dll` |
| Windows x86 | `Processing.NDI.Lib.x86.dll` | `Processing.NDI.Lib.Advanced.x86.dll` |
| macOS | `libndi.dylib` | `libndi_advanced.dylib` |
| Linux | `libndi.so.6` | `libndi_advanced.so.6` |
| Android | `libndi.so` | `libndi_advanced.so` |

To load one exact file instead, pass its absolute path. No other location is tried:

```rust
use grafton_ndi::{RuntimeOptions, NDI};

let options = RuntimeOptions::default().library_path("/opt/ndi/lib/libndi.so.6");
let ndi = NDI::with_options(&options)?;
println!("loaded {:?}", ndi.info().library_path());
```

When no usable library is found, `NDI::new()` returns `Error::RuntimeLoadFailed`. Its message names every location tried and the operating system's reason for each:

```text
Failed to load the NDI runtime: no usable Standard runtime; tried /opt/app/libndi.so.6 (next to the executable): not found; /opt/ndi/libndi.so.6 (NDI_RUNTIME_DIR_V6): not found; libndi.so.6 (system library search): dlopen failed: libndi.so.6: cannot open shared object file: No such file or directory
```

A failed selection can be retried. A successful one is permanent: the selected library stays loaded until the process exits, and a later request for a different library returns `Error::RuntimeConflict`.

One limit applies to the search. A library that loads but is rejected stays loaded, because a native library cannot be unloaded safely. If the platform loader's own search then matches the library name to that module, it returns the module again instead of looking further. The error names the rejected file. Remove or fix that file, or pass an explicit library path.

### Selecting a runtime

Using NDI takes two steps. `Runtime::select` loads the library and pins it for the rest of the process without starting NDI. `Runtime::initialize` starts NDI and returns the `NDI` handle from which finders, senders and receivers are created. `NDI::new()` does both with the default options, and `NDI::with_options` with the options it is given. Nothing else selects a runtime.

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

NDI ships a Standard SDK and an Advanced SDK with different runtime libraries. An edition request decides which library name the search uses:

- `RuntimeOptions::default().edition(SdkEdition::Advanced)` searches for the Advanced library and fails unless the loaded library exports the Advanced SDK entry points.
- `RuntimeOptions::default().edition(SdkEdition::Standard)` searches for the Standard library. An Advanced runtime also meets a Standard request.
- Without a request, the default chosen when the crate was built applies: the value of `NDI_SDK_EDITION` (`standard` or `advanced`) if it was set, otherwise the edition of an NDI SDK found on the build machine, otherwise Standard.

The build looks for an SDK in the directory named by `NDI_SDK_DIR`, or else in the vendor installers' default locations, and uses the first one it finds. On Linux the Advanced SDK's location is checked before the Standard SDK's. A build that finds an Advanced SDK therefore searches for the Advanced runtime by default. Set `NDI_SDK_EDITION=standard` for that build, or request `SdkEdition::Standard` at run time, to use the Standard runtime instead. `NDI_SDK_DIR` must name an SDK installation when it is set; any other value fails the build.

`ndi.info().edition()` reports the edition of the library that was actually loaded, detected from the functions it exports. An edition says nothing about a license entitlement.

## Core Workflows

### Discover Sources

`Finder` wraps the NDI discovery API. It can return a current source snapshot, wait for source-list changes, or perform a bounded discovery pass.

```rust
use grafton_ndi::{Finder, FinderOptions};
use std::time::Duration;

let options = FinderOptions::builder()
    .show_local_sources(true)
    .groups("Public,Studio")
    .extra_ips("192.168.1.100")
    .build();
let finder = Finder::new(&ndi, &options)?;

if finder.wait_for_sources(Duration::from_secs(2))? {
    for source in finder.current_sources()? {
        println!("{source}");
    }
}
```

For applications that repeatedly reconnect to known devices, `SourceCache` handles discovery, exact host/IP matching, and cache invalidation. Its lookups take the `NDI` handle. Pass a complete bare hostname or IP address. Hostname ASCII case, an optional final dot, and equivalent numeric IP spellings share one cache entry. `len()` counts distinct host identities; `invalidate()` accepts any equivalent spelling. For searches by partial IP or source label, filter a `Finder` snapshot explicitly.

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

let options = ReceiverOptions::builder(camera)
    .color(ReceiverColorFormat::RGBX_RGBA)
    .bandwidth(ReceiverBandwidth::Highest)
    .build();
let receiver = Receiver::new(&ndi, &options)?;

let video = receiver.video().capture(Duration::from_secs(5))?;
println!(
    "{}x{} {:?}",
    video.width(),
    video.height(),
    video.pixel_format()
);

let audio = receiver.audio().try_capture(Duration::from_millis(100))?;
let metadata = receiver.metadata().try_capture(Duration::from_millis(100))?;
```

Borrowed receiver and FrameSync video expose only initialized active pixels.
`frame.data()` returns `Some(&[u8])` when all active pixels are contiguous, and
`None` when row or plane padding separates them. Traverse padded frames without
allocation or pixel copies:

```rust
if let Some(frame) = receiver.video().try_capture_ref(Duration::from_secs(1))? {
    for plane in frame.planes() {
        for row in plane.rows() {
            // Each row contains active bytes only, with no SDK padding.
            println!("{} active bytes", row.len());
        }
    }
    let packed = frame.to_owned()?; // one pixel allocation; active rows only
}
```

Owned capture and copied video sends pack planes tightly and can reduce the
source stride. Format, plane order, metadata, timing, aspect ratio, and scan type
are preserved. Borrowed PNG/JPEG encoding reads active rows directly.

`VideoPlane`, `VideoRows`, and `VideoPlanes` implement `Send + Sync`. You can
move preconstructed views into scoped workers or share them for immutable reads,
including views from FrameSync. The frame stays borrowed until the workers
finish; FrameSync frame references keep their capture-thread affinity.

PNG/JPEG conversion uses active rows for owned and captured frames. PNG borrows
RGBA rows directly and uses at most one `width * 4` conversion row for other
supported formats. JPEG writes components into the codec's row buffers without
an RGB staging frame. Codec storage and encoded output require additional memory.

### Publish Sources

`Sender` publishes a named NDI source and sends video, audio, and metadata. It also exposes connection count, tally, failover, and connection metadata APIs.

```rust
use grafton_ndi::{PixelFormat, Sender, SenderOptions, VideoFrame};
use std::time::Duration;

let options = SenderOptions::builder("Rust Program Output")
    .clock_video(true)
    .clock_audio(true)
    .build();
let sender = Sender::new(&ndi, &options)?;

let mut frame = VideoFrame::builder()
    .resolution(1920, 1080)
    .pixel_format(PixelFormat::BGRA)
    .frame_rate(60, 1)
    .build()?;
frame.data_mut().fill(0);

sender.send_video(&frame);

let connections = sender.connection_count(Duration::from_millis(500))?;
println!("connected receivers: {connections}");
```

### Use FrameSync

`FrameSync` is for pull-based capture when your application has its own clock: a GPU vsync loop, audio callback, timeline, or mixer. Video capture returns the frame appropriate for the requested time base, and audio capture can resample to the requested output shape.

```rust
use grafton_ndi::{FrameSync, FrameSyncAudioRequest, ScanType};

let frame_sync = FrameSync::new(receiver)?;

if let Some(video) = frame_sync.capture_video(ScanType::Progressive)? {
    println!("video: {}x{}", video.width(), video.height());
}

let request = FrameSyncAudioRequest::capture_with(Some(48_000), 2, 1024)?;
let audio = frame_sync.capture_audio(request)?;
if !audio.is_empty() {
    println!("audio: {} channels at {} Hz", audio.num_channels(), audio.sample_rate());
}
```

`FrameSync` owns its receiver. `frame_sync.receiver()` provides controls, audio,
metadata, and status through `Receiver<FrameSyncAttached>`. Use
`frame_sync.into_receiver()` to recover an ordinary `Receiver` with raw video
capture. The type system prevents raw-video capture while FrameSync owns it.

### Monitor and Control

Receivers report connection health, frame-drop statistics, and PTZ support. Use `set_tally` to send program/preview state upstream; sender tally reports receiver state.

```rust
use std::time::Duration;

if receiver.is_connected() {
    let stats = receiver.connection_stats();
    println!("video drop rate: {:.2}%", stats.video_drop_percentage());
}

if let Some(status) = receiver.poll_status_change(Duration::from_millis(100))? {
    println!("Active connections: {}", status.connections);
}

if receiver.ptz_is_supported() {
    receiver.ptz_zoom(0.25)?;
}
```

### Integrate With Async Runtimes

NDI receive calls are fundamentally blocking SDK calls. The optional async wrappers make that explicit by running receive work on the runtime's blocking pool while preserving the timeout budget from the future's first poll. Both `capture` and `try_capture` include blocking-pool queue delay and reconnect-lock contention in the same deadline. Dropping a future suppresses work before its cancellation gate; a task that has passed that gate cannot be interrupted and may still consume a frame, even if it is waiting for the reconnect lock. Queued work can still finish after the budget expires; these wrappers do not impose a wall-clock completion deadline.

```rust
use grafton_ndi::tokio::AsyncReceiver;
use std::time::Duration;

let async_receiver = AsyncReceiver::new(receiver);
let frame = async_receiver.video().capture(Duration::from_secs(5)).await?;
```

## API Model

The crate's public API is organized around a small set of resource types:

- `Runtime` is the selected NDI runtime library; `NDI` is a handle that keeps NDI started.
- `Finder` discovers `Source` values on the network.
- `Receiver` connects to a source and captures video, audio, and metadata.
- `Sender` publishes an NDI source.
- `FrameSync` wraps a receiver for clock-corrected pull capture.
- `VideoFrame`, `AudioFrame`, and `MetadataFrame` represent application-owned frame data.

Most applications can start with owned frame APIs such as `receiver.video().capture()` and `sender.send_video()`. For receive hot paths, borrowed receive refs keep data in SDK buffers without an extra copy. Async `sender.send_video_async(frame)` moves an owned `VideoFrame` into the sender without copying pixels and returns the previous completed frame for reuse. This avoids Rust wrapper pixel copies; SDK encoding and network processing may still copy data. Use two frames and call `finish_video_async()` to recover the final frame. For borrowed buffers, `send_video_async_copy(&frame)` explicitly copies pixels and metadata into sender-owned storage and returns the previous frame as `Result<Option<VideoFrame>>`; the source buffer can immediately be reused.

Frame layout fields that describe SDK-facing memory are private. Builders, accessors, checked mutation methods, and `PixelFormat` helpers keep dimensions, strides, metadata, and buffer sizes consistent before data crosses the FFI boundary.

## Design Approach

`grafton-ndi` aims to be a predictable Rust layer over NDI, not a replacement for the NDI SDK documentation.

- **Typed function tables**: every native call goes through a table of functions resolved once, when the runtime is selected. Each table's signatures are checked at compile time against bindings generated from the SDK headers.
- **One runtime per process**: the selected library stays loaded until the process exits, while `NDI` handles start and stop the SDK by reference count.
- **RAII lifecycle**: NDI handles, senders, receivers, frames, and pending async sends clean up through ownership.
- **Initialized receive views**: borrowed video rows and audio channels expose active payload only; `data()` is optional for padded layouts. Owned capture packs active payload without reading SDK padding.
- **Checked frame descriptions**: frame dimensions, line strides, channel strides, metadata strings, and buffer sizes are validated before slices or strings are exposed.
- **Explicit blocking behavior**: timeout-bearing calls validate their wait budgets; SDK sends and drains have no deadline. Async adapters run SDK work on a blocking pool.
- **Forward-compatible SDK enums**: public SDK-mode enums that may grow are `#[non_exhaustive]`.

## Performance and Safety

- Use owned frame APIs when clarity matters or when frame data must outlive the SDK capture buffer.
- Use borrowed receive refs when you need direct access to SDK-owned buffers during a tight capture loop.
- Use two owned video frames with `send_video_async` to reuse pixel allocations without per-send copies; use `send_video_async_copy` when borrowing external buffers is more convenient.
- Use `FrameSync` when capture timing is driven by an external output clock.
- Use lower bandwidth modes for previews and monitoring surfaces.
- Keep image encoding off real-time paths unless the workload is sized for compression.

The compile-contract tests in `tests/ui` verify the important lifetime rules around borrowed receive refs and owned async submissions and immediate source reuse after copied sends.

## Image Encoding

With the default `image-encoding` feature, captured `VideoFrame` values can be encoded directly:

```rust
use grafton_ndi::ImageFormat;

let png = video.encode_png()?;
let jpeg = video.encode_jpeg(85)?;
let data_url = video.encode_data_url(ImageFormat::Png)?;
```

## Runtime capabilities and optional APIs

`runtime.info()`, or `ndi.info()` on a handle, gives the identity of the selected library: its path, edition, version and whether it is shared or statically linked.

Runtimes differ in what they provide. `capabilities()` reports each optional capability, and `require_feature` turns an unavailable one into an error before any native call is made. Both are available on `Runtime` and on `NDI`:

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

An operation whose capability is unavailable returns `Error::UnsupportedCapability` with the capability's name and the reason. Other operations keep working. A capability is unavailable for one of these reasons:

| Reason | Meaning |
| --- | --- |
| `SymbolUnavailable` | The runtime does not export a function the capability needs. |
| `RuntimeTooOld` | The runtime's version is below the capability's minimum. |
| `RuntimeVersionUnknown` | The capability has a minimum version and the runtime's version text was not recognized. |
| `WrongEdition` | The capability belongs to the Advanced SDK and the runtime is a Standard one. |
| `BuildAbiUnavailable` | The capability belongs to the Advanced SDK and the crate was built without the Advanced SDK headers. |

The capabilities with a minimum version need NDI 6.4.0:

- `Planar444Video`, `ReceiveBest444` and `ReceiveVideoOnly`, because an older runtime does not know those values.
- `SenderAdvertising`, `ReceiverAdvertising`, `SenderDiscovery` and `ReceiverDiscovery`, because older runtimes can crash when those objects are destroyed.

The other capabilities depend only on what the runtime exports. The Standard 6.3.2 runtime does not export the functions behind `ReceiveDelay` and `SdkTimestamp`. The Standard 6.4.0.2 runtime provides every capability except the two Advanced SDK ones. Availability says nothing about hardware or license requirements.

Planar `NA48`, `NB48`, `NA4G`, and `NB4G` frames need a runtime with `Planar444Video`. Construct them with
`VideoFrame::builder().pixel_format(format).build_for(&ndi)` or the borrowed-frame
equivalent. The frame keeps the runtime alive, including when captured or copied. `build()` works without a runtime for every other format, and returns `Error::RuntimeRequired` for these four. Delay setters accept at most 2000 ms,
round positive fractional milliseconds upward, and report whether the native value
changed. Standard encoding may convert 4:4:4 inputs to 4:2:2.

`utilities` provides checked interleaved audio conversions with explicit integer reference levels. `video_conversion`
provides V210/P216 conversions for complete six-pixel V210 groups.

### Routing

A `Router` is an NDI source that carries no media. Receivers connect to it by name and are redirected to whichever source it currently routes to:

```rust
use grafton_ndi::routing::{Router, RouterOptions};

let mut router = Router::new(&ndi, &RouterOptions::new("Program"))?;
router.change(&camera)?;
// Later: stop routing.
router.clear()?;
```

### Discovery Server

The `discovery` module works with an NDI Discovery Server, which keeps a registry of senders and receivers. `SendAdvertiser` and `RecvAdvertiser` register this process's senders and receivers. `SendListener` and `RecvListener` list what is registered, subscribe to events and, for receivers that allow it, request a source change. All four need an NDI 6.4.0 or later runtime.

```rust
use grafton_ndi::discovery::{ListenerOptions, RecvListener};
use std::time::Duration;

let options = ListenerOptions::default().server("127.0.0.1")?;
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

Each object is created for a server address: the one in its options, or else the one in the NDI configuration. Creation fails only when there is no address or the SDK cannot parse it. A server that cannot be reached is not an error. The object keeps trying in the background and starts working a few seconds after the server appears. A listener reports its connection through `is_connected`, and its snapshots are empty until it connects. An advertiser reports nothing.

An advertiser owns its registrations. `register` returns a token, and the registered sender or receiver stays alive until `unregister` is called with that token or the advertiser is dropped, even if the application drops its own handle first. A token given to an advertiser that did not issue it comes back inside `Rejected`.

Names and metadata from the network are decoded lossily, so one malformed peer cannot fail a snapshot. Identifiers are the exception: a `SenderId`, `ReceiverId` or `InputGroupId` keeps the server's exact bytes, so passing one back always names the same peer, and each kind is its own type. One snapshot or event batch is limited to 16 MiB, and one string to 4 MiB. `Router`, the advertisers and the listeners are `Send` and not `Sync`.

### Advanced SDK

The `advanced` feature adds the `advanced` module. Using it needs three things:

1. The `advanced` Cargo feature.
2. An NDI Advanced SDK on the build machine, in `NDI_SDK_DIR` or a default installation directory. The Advanced SDK headers may not be redistributed, so the build generates their declarations locally with bindgen, which needs `libclang`.
3. An Advanced SDK runtime selected when the program runs.

The module's API is the same whether or not the build found the Advanced SDK. Without it, or with a Standard runtime, each native operation returns `Error::UnsupportedCapability` with the reason. The data types in the module, such as packets, envelopes and options, work in every build.

An operation that takes ownership of a receiver, a sender or a frame hands it back inside `grafton_ndi::Rejected<T>` when it fails. Custom receive allocators require `ndi.process_lifetime()`, an explicit opt-in that keeps NDI started until the process exits. See the [Advanced SDK guide](docs/advanced.md).

### File API

The `file-io` feature adds the `file` module, which records NDI sources to disk and plays recordings back as NDI sources through the NDI File library. The File library is a separate native library that ships with the NDI SDK beside the runtime library. `FileApi::open` finds it when it is first needed, searching the selected runtime's directory, then the executable's directory, then `NDI_RUNTIME_DIR_V6`, then the platform loader's path. Nothing else in the crate depends on it.

```rust
use grafton_ndi::file::{FileApi, FileRuntimeOptions, PlayerOptions};

let file = FileApi::open(&ndi, &FileRuntimeOptions::default())?;
let info = file.probe("take_001.mkv")?;
println!("{} track(s)", info.track_count());

let mut player = file.create_player(PlayerOptions::new("Replay")?)?;
player.start("take_001.mkv")?;
```

On Linux, macOS and Android the File library works only with the Advanced SDK runtime, and `FileApi::open` refuses it beside a Standard runtime before loading anything. A recorder stores a source's compressed data as it arrives and takes no frames from Rust. Treat a recording as complete only when `FileRecorder::finish` returns `FinishStatus::Closed`. See the [File API guide](docs/file-io.md).

### Apple mobile targets

iOS, tvOS and visionOS targets link the vendor's static archive into the application instead of loading a library. For those targets:

- `NDI_SDK_DIR` must name an NDI SDK for Apple. The build fails without it.
- The build links the archive for the target from the SDK's `lib/iOS`, `lib/tvOS` or `lib/visionOS` directory, together with the system frameworks it needs. With `file-io`, the File archive is linked in its place when the SDK supplies one.
- The archive must contain a slice for the target's CPU and platform. The SDK does not supply every device and simulator combination.
- If `IPHONEOS_DEPLOYMENT_TARGET`, `TVOS_DEPLOYMENT_TARGET` or `XROS_DEPLOYMENT_TARGET` is set below the minimum the archive was built for, the build fails.
- The edition is fixed by the archive. `RuntimeOptions::library_path` is rejected with `Error::RuntimeConflict`, and selecting the runtime loads nothing.

## Platform Support

The compatibility target is NDI 6.x and Rust 1.88.

| Target family | How NDI is provided |
| --- | --- |
| Linux | Shared library, loaded at run time. |
| Windows MSVC x86/x64 | Shared library, loaded at run time. |
| macOS Intel/Apple Silicon | Shared library, loaded at run time. |
| Android | Shared library packaged with the application, loaded at run time. |
| iOS/tvOS/visionOS | Static archive from the NDI SDK for Apple, linked at build time. |

For this release the test suite was run on Linux x86_64 against Standard 6.1.1 and Standard/Advanced 6.3.2 and 6.4.0.2 runtimes. Cross-version peers were also tested between 6.1.1/6.3.2 and 6.4. Windows, macOS, Android and iOS targets were compiled and linted but their tests were not run. NDI 6.0 remains unqualified. The CI workflows in this repository have not yet run on a hosted runner. See [SDK validation](docs/sdk-validation.md) for the results and their limits.

## Documentation

- [API documentation](https://docs.rs/grafton-ndi) - Reference for every module and feature. Generate it for a checkout with `cargo doc --no-deps --all-features`.
- [Examples](examples/) - Runnable programs, listed below.
- [CHANGELOG.md](CHANGELOG.md) - Release notes.
- [Migration guides](docs/migration/) - Guidance for upgrading between crate versions, including [2.0 to 3.0](docs/migration/2.0-to-3.0.md).
- [Advanced SDK guide](docs/advanced.md) - Requirements, ownership, packets, queues, allocators and synchronization.
- [File API guide](docs/file-io.md) - Recording, playback and how a recording is finished.
- [SDK validation](docs/sdk-validation.md) - What was tested, and how to reproduce it.
- [CI guide](ci/README.md) - The workflows, the SDK test matrix and the release procedure.
- [Image encoding resources](docs/image-encoding-performance.md) - Conversion scratch bounds, codec/output peak memory, and reproducible timing comparisons.

## Examples

Run an example with `cargo run --example <name>`. Most accept the IP addresses to search as arguments; each file's header lists its options.

Discovery and monitoring:

| Example | Command | What it shows |
| --- | --- | --- |
| `runtime_selection.rs` | `cargo run --example runtime_selection` | Select a runtime, inspect it, then start NDI. |
| `NDIlib_Find.rs` | `cargo run --example NDIlib_Find` | Discover NDI sources on the network. |
| `status_monitor.rs` | `cargo run --example status_monitor` | Monitor receiver connection status and frame drops. |
| `routing.rs` | `cargo run --example routing` | Route receivers to each discovered source in turn. |
| `discovery_server.rs` | `cargo run --example discovery_server -- 127.0.0.1:5959` | Register a sender with a Discovery Server and list what the server reports. Needs a Discovery Server and an NDI 6.4.0 or later runtime. |

Receiving:

| Example | Command | What it shows |
| --- | --- | --- |
| `NDIlib_Recv_Audio.rs` | `cargo run --example NDIlib_Recv_Audio` | Receive and inspect audio. |
| `NDIlib_Recv_Audio_16bpp.rs` | `cargo run --example NDIlib_Recv_Audio_16bpp` | Receive audio and convert it to 16-bit samples. |
| `NDIlib_Recv_FrameSync.rs` | `cargo run --example NDIlib_Recv_FrameSync` | Clock-corrected capture with `FrameSync`. |
| `NDIlib_Recv_PNG.rs` | `cargo run --example NDIlib_Recv_PNG` | Receive video and save a PNG. Needs the `image-encoding` feature, which is on by default. |
| `NDIlib_Recv_PTZ.rs` | `cargo run --example NDIlib_Recv_PTZ` | Control PTZ cameras. |
| `concurrent_capture.rs` | `cargo run --example concurrent_capture` | Capture video, audio and metadata from one receiver on separate threads. |

Sending:

| Example | Command | What it shows |
| --- | --- | --- |
| `NDIlib_Send_Audio.rs` | `cargo run --example NDIlib_Send_Audio` | Send audio. |
| `NDIlib_Send_Video.rs` | `cargo run --example NDIlib_Send_Video` | Send video. |
| `async_send.rs` | `cargo run --example async_send` | Reuse two owned frames for async sending and recover completed frames. |
| `async_send_copy.rs` | `cargo run --example async_send_copy` | Copy borrowed pixels into an async send. |
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

File API. These need the `file-io` feature and the NDI File library; on Linux and macOS that means the Advanced SDK runtime:

| Example | Command | What it shows |
| --- | --- | --- |
| `file_record.rs` | `cargo run --example file_record --features file-io -- "MACHINE (Source)" take 10` | Record a source to disk and wait for the file to be finalized. |
| `file_play.rs` | `cargo run --example file_play --features file-io -- take_001.mkv "Replay"` | Play a recording back as an NDI source. |

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
skipped: frames::tests::planar_444_rows_copy_without_padding_and_preserve_runtime_proof needs an NDI runtime
```

Those tests create senders, receivers and finders, which announce themselves on the local network like any NDI program.

Set `GRAFTON_NDI_REQUIRE_RUNTIME` to turn skips into failures. Its value is a comma-separated list of what the environment provides:

| Entry | The environment provides |
| --- | --- |
| `1` | An NDI runtime library. Every other entry implies it. |
| `advanced` | An Advanced SDK runtime, and a build made with the Advanced SDK headers. |
| `file` | An NDI File library that the selected runtime can use. |
| `discovery-server` | The SDK's `ndi-discovery-server` program beside the selected runtime, and an NDI 6.4.0 or later runtime. |

An entry that no test in the build can check fails the run too, for example `advanced` without the `advanced` feature.

For example, with an NDI SDK installed on Linux x86_64:

```bash
export NDI_RUNTIME_DIR_V6="/usr/share/NDI SDK for Linux/lib/x86_64-linux-gnu"
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo test --all-features
```

Adding `--config ci/offline.toml` runs every test executable with loopback networking only, so nothing is announced on the network the machine is attached to. The [CI guide](ci/README.md#loopback-only-test-runs) describes what that needs on each operating system.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development setup, the bindings generator and contribution guidelines.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

The bindings in `src/sys/bindings.rs` and `src/sys/declarations.rs` are generated from NDI SDK headers that the vendor distributes under the MIT license. Each file starts with that copyright and permission notice.

The NDI SDK and runtime are supplied separately under their vendor terms.

## Disclaimer

This is an unofficial community project and is not affiliated with NewTek or Vizrt.

NDI® is a registered trademark of Vizrt NDI AB.
