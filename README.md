# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi)
[![Documentation](https://img.shields.io/badge/docs-grantsparks.github.io-blue.svg)](https://grantsparks.github.io/grafton-ndi/grafton_ndi/)
[![CI](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml/badge.svg)](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE)
[![Minimum Rust Version](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org)

`grafton-ndi` is an idiomatic Rust interface to the NDI® 6 SDK. It gives Rust applications a practical way to discover NDI sources, receive and publish video/audio/metadata streams, monitor status and tally, control PTZ devices, and integrate NDI work into synchronous or async application architectures.

The crate keeps generated C bindings behind typed runtime dispatch and Rust ownership wrappers. Applications supply their renderer, mixer, encoder, storage layer, or UI.

## What It Covers

- **Runtime lifecycle** - Initialize and tear down the process-global NDI runtime through cheap, reference-counted `NDI` handles.
- **Network discovery** - Discover sources, wait for source-list changes, search groups or extra IP addresses, and cache host/IP lookups.
- **Receiving** - Capture video, audio, and metadata with configurable bandwidth and color-format choices.
- **Sending** - Publish an NDI source and send video, audio, metadata, connection metadata, and failover information.
- **FrameSync** - Use NDI's clock-corrected pull API for playback, render loops, and audio-device driven workflows.
- **Monitoring** - Query receiver connection status, frame-drop statistics, tally state, and sender connection counts.
- **PTZ control** - Drive supported pan, tilt, zoom, focus, exposure, and white-balance commands.
- **Async integration** - Use optional Tokio or smol wrappers for receive workflows without blocking the async runtime.
- **Image snapshots** - Encode captured video frames as PNG, JPEG, or data URLs with the default `image-encoding` feature.
- **Routing and Discovery Server** - Route sources, manage advertisements, inspect owned listener snapshots/events, and explicitly request remote receiver connections.
- **Media formats and controls** - Planar 4:4:4 formats, receiver color/bandwidth modes, delay controls, SDK timestamps, and checked audio/video conversions.
- **Advanced SDK** - Optional compressed/scatter streams, native completion queues, custom receive allocation, JSON configuration, AVSync, Genlock, and KVM.
- **File API** - Optional source recording and file-to-network playback, with owned events and explicit finalization state.
- **Runtime capabilities** - Inspect the selected SDK implementation and check whether optional operations are available.

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

For the published core API, add the crate to `Cargo.toml`:

```toml
[dependencies]
grafton-ndi = "2.0"
```

For the APIs and optional features in this checkout, use a local path dependency:

```toml
[dependencies]
grafton-ndi = { path = "../grafton-ndi", features = ["advanced", "file-io"] }
```

Feature flags:

| Feature | Effect |
| --- | --- |
| `image-encoding` | PNG, JPEG and data URLs; enabled by default. |
| `tokio`, `smol` | Receiver adapters for the selected async runtime; independently optional. |
| `advanced` | Advanced Rust APIs; native calls require authorized Advanced headers and the selected Advanced runtime. |
| `file-io` | File recording/playback and executor-independent async owners; requires a matching File artifact. |

Use `default-features = false` for a minimal build. Cargo features select Rust
APIs. `NDI_SDK_DIR`, `NDI_SDK_EDITION` and the installed artifacts select the build
SDK; runtime selection is described below. SDK versions are detected without
version-specific Cargo features.

### Prerequisites

1. **NDI SDK 6.x** (see the exact version/target evidence below): Install the [NDI SDK](https://ndi.video/for-developers/ndi-sdk/) for your platform.
   - Windows defaults include `C:\Program Files\NDI\NDI 6 SDK` and `C:\Program Files\NDI\NDI 6 Advanced SDK`.
   - Linux defaults: `/usr/share/NDI Advanced SDK for Linux` or `/usr/share/NDI SDK for Linux`
   - macOS defaults include `/Library/NDI SDK for macOS`, `/Library/NDI SDK for Apple`, and `/Library/NDI 6 SDK`
   - Android: set `NDI_SDK_DIR` to the Android SDK and use the Android NDK for cross-compilation.
   - Set `NDI_SDK_DIR` when the SDK is installed elsewhere. Use `NDI_SDK_PROFILE` for a specific embedded/device artifact and `NDI_SDK_EDITION=standard|advanced` to resolve an installation containing both editions.
   - Apple mobile builds require the corresponding Xcode SDK and an actual vendor device/simulator slice. A newer Rust toolchain is required only for targets unavailable in Rust 1.88.

2. **Rust**: Rust 1.88 or later.

3. **Build dependencies**: bindgen generates the FFI bindings at build time from
   your installed SDK headers, so it needs an LLVM/Clang installation including
   the `libclang` shared library. Set `LIBCLANG_PATH` if bindgen cannot locate it:
   - Windows: Visual Studio 2019+ or Build Tools, plus LLVM/Clang for bindgen
   - Linux: a C toolchain and a libclang development package for bindgen
   - macOS: Xcode Command Line Tools and a discoverable libclang installation

4. **Runtime libraries**: shared-library builds need the vendor runtime and
   its dependencies when NDI is used. Set `NDI_RUNTIME_DIR_V6` to its directory,
   or select an absolute library path through `RuntimeOptions`.
   - Windows also supports the native DLL search path, including `PATH`.
   - Linux also supports the native loader search path, including `LD_LIBRARY_PATH`.
   - macOS tries application `@rpath` and then native basename lookup; `DYLD_LIBRARY_PATH` can be used where the host permits it.
   - Apple mobile profiles link the supplied static archive and required system frameworks into the application.

Builds require a complete SDK installation for header and target-artifact
validation. Shared core libraries load when selected at runtime; an unavailable
library produces a Rust load error.

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

For applications that repeatedly reconnect to known devices, `SourceCache` handles runtime initialization, discovery, exact host/IP matching, and cache invalidation. Pass a complete bare hostname or IP address. Hostname ASCII case, an optional final dot, and equivalent numeric IP spellings share one cache entry. `len()` counts distinct host identities; `invalidate()` accepts any equivalent spelling. For searches by partial IP or source label, filter a `Finder` snapshot explicitly.

```rust
use grafton_ndi::SourceCache;
use std::time::Duration;

let cache = SourceCache::new();
let camera = cache.find_by_host("192.168.1.100", Duration::from_secs(5))?;
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

- `NDI` owns a reference to the process-global NDI runtime.
- `Finder` discovers `Source` values on the network.
- `Receiver` connects to a source and captures video, audio, and metadata.
- `Sender` publishes an NDI source.
- `FrameSync` wraps a receiver for clock-corrected pull capture.
- `VideoFrame`, `AudioFrame`, and `MetadataFrame` represent application-owned frame data.

Most applications can start with owned frame APIs such as `receiver.video().capture()` and `sender.send_video()`. For receive hot paths, borrowed receive refs keep data in SDK buffers without an extra copy. Async `sender.send_video_async(frame)` moves an owned `VideoFrame` into the sender without copying pixels and returns the previous completed frame for reuse. This avoids Rust wrapper pixel copies; SDK encoding and network processing may still copy data. Use two frames and call `finish_video_async()` to recover the final frame. For borrowed buffers, `send_video_async_copy(&frame)` explicitly copies pixels and metadata into sender-owned storage and returns the previous frame as `Result<Option<VideoFrame>>`; the source buffer can immediately be reused.

Frame layout fields that describe SDK-facing memory are private. Builders, accessors, checked mutation methods, and `PixelFormat` helpers keep dimensions, strides, metadata, and buffer sizes consistent before data crosses the FFI boundary.

## Design Approach

`grafton-ndi` aims to be a predictable Rust layer over NDI, not a replacement for the NDI SDK documentation.

- **Typed dispatch**: all core calls use one selected native implementation. Shared modules remain loaded while ordinary `NDI` handles retain initialization separately.
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

## Documentation

- [API documentation](https://grantsparks.github.io/grafton-ndi/grafton_ndi/) - Hosted reference. Generate documentation for your checkout with `cargo doc --no-deps --all-features`; docs.rs lacks the SDK installation required by the build script.
- [Examples](examples/) - Runnable examples for discovery, receiving, FrameSync, PTZ, monitoring, and sending.
- [CHANGELOG.md](CHANGELOG.md) - Release notes and migration guidance.
- [SDK compatibility and validation](docs/sdk-validation.md) - Version coverage, platform qualifications, and cross-build requirements.
- [Advanced SDK guide](docs/advanced.md) - Configuration, packet ownership, callbacks, allocators and synchronization.
- [File API guide](docs/file-io.md) - Recording/playback, finalization, async ownership and current pointer-getter restrictions.
- [SDK upgrade tools](tools/sdk/README.md) - Repeatable API review, capability metadata and independent ABI probes.
- [Development SDK store](ci/sdk-store.md) - Verified local archives and side-by-side SDK installations.
- [Image encoding resources](docs/image-encoding-performance.md) - Conversion scratch bounds, codec/output peak memory, and reproducible timing comparisons.
- [Migration guides](docs/migration/) - Guidance for upgrading between crate versions.

## Examples

Run examples with:

```bash
cargo run --example NDIlib_Find
```

Discovery and monitoring:

- `NDIlib_Find.rs` - Discover NDI sources on the network.
- `status_monitor.rs` - Monitor receiver connection status and frame drops.

Receiving:

- `NDIlib_Recv_Audio.rs` - Receive and inspect audio streams.
- `NDIlib_Recv_Audio_16bpp.rs` - Receive 16-bit audio samples.
- `NDIlib_Recv_FrameSync.rs` - Clock-corrected capture with `FrameSync`.
- `NDIlib_Recv_PNG.rs` - Receive video and save PNG snapshots.
- `NDIlib_Recv_PTZ.rs` - Control PTZ cameras.
- `concurrent_capture.rs` - Capture from multiple sources concurrently.

Sending:

- `NDIlib_Send_Audio.rs` - Send audio.
- `NDIlib_Send_Video.rs` - Send video.
- `async_send.rs` - Reuse two owned frames for async sending and recover completed frames.
- `async_send_copy.rs` - Explicitly copy borrowed pixels into an async-send snapshot.
- `zero_copy_send.rs` - Render into two reusable owned frames without per-send pixel copies.

## Runtime capabilities and optional APIs

`NDI::new()` initializes the selected runtime and releases initialization when its
last handle is dropped. Desktop, Linux and Android builds load a shared library;
Apple mobile profiles link a static provider. To select an absolute runtime path,
use `NDI::with_options(&RuntimeOptions::default().library_path(path))`.
`RuntimeOptions::edition` selects Standard or Advanced independently of Cargo
features.

A successfully selected module stays loaded for the process lifetime, and an
incompatible later selection returns an error. Failed loads are retryable; failed
native initialization retries the selected provider. Version and CPU queries can
select the default module, so apply explicit options first when choosing an
installation. `NDI::try_is_supported_cpu()` reports loading errors;
`NDI::is_supported_cpu()` returns `false` when loading fails.

Use `ndi.info()` for runtime identity and `ndi.capabilities()` for optional
features. Availability depends on the compiled bindings, runtime symbols and
semantics, and any hardware or licensing requirements. Unavailable operations
return capability errors without disabling unrelated operations.
`RuntimeInfo::edition_verified()` reports whether the runtime provides positive
edition evidence; an edition label alone does not establish feature availability
or a license.

Planar `NA48`, `NB48`, `NA4G`, and `NB4G` frames require compatible NDI 6.4 runtime
semantics. Construct them with
`VideoFrame::builder().pixel_format(format).build_for(&ndi)` or the borrowed-frame
equivalent. The frame retains the runtime capability proof, including when captured
or copied. `build()` is runtime-independent for formats that do not require this
proof. `Best444`, video-only receive modes, delay controls, and SDK timestamps also
require compatible runtime capabilities. Delay setters accept at most 2000 ms,
round positive fractional milliseconds upward, and report whether the native value
changed. Standard encoding may convert 4:4:4 inputs to 4:2:2.

The `routing` and `discovery` modules provide routing, Discovery Server monitoring,
owned snapshots, subscriptions, and remote control. Advertisement guards retain
their native resources while registered. `utilities` provides checked interleaved
audio conversions with explicit integer reference levels; `video_conversion`
provides V210/P216 conversions for complete six-pixel V210 groups.

With `advanced`, `QueuedSender` retains submitted frames until native completion.
Custom receive allocators require `ndi.process_lifetime()`, an explicit opt-in
that keeps the initialized runtime alive until process exit. See the
[Advanced SDK guide](docs/advanced.md) for packet ownership, callbacks,
configuration, and synchronization.

With `file-io`, `file::FileApi` loads recording/playback on demand. Shared POSIX
File libraries require the matching verified Advanced core; Windows File is
supplied for x64. Apple static File profiles use the File archive as the sole core
provider. An unavailable File module leaves core operations usable. Recorder
finalization, async ownership, and the restricted `native_source()` and
`native_current_file()` getters are described in the [File API guide](docs/file-io.md).

## Platform Support

The compatibility target is NDI 6.x. The validation matrix covers Standard 6.3.2
and 6.4, and Advanced 6.4, including mixed Standard header/runtime versions.
SDKs 6.0 and 6.1.1 do not have current validation results. See
[SDK validation](docs/sdk-validation.md) for coverage requirements and validation records.
For a specific revision, consult its [CI checks](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
and `required-ci` aggregate. Required CI runs full native suites on Linux x64,
Windows x64 and macOS ARM64 with both SDK editions. Other supported profiles keep
ABI and final link/package checks, with representative Android and tvOS execution.
The [CI guide](ci/README.md) describes the required checks and optional extended
audit. Build/link, emulator/simulator and physical-device evidence are recorded
separately.

| Target family | Integration and required CI coverage |
| --- | --- |
| Linux x86_64 GNU | Standard/Advanced shared runtime; full native suites. |
| Linux x86/ARM64 and RPi GNU profiles | Matching ISA, libc, float ABI and sysroot; ABI and vendor link checks. |
| Windows MSVC x86/x64 | Standard/Advanced DLLs; x64 native suites and x86 ABI/link checks. File is x64 only. |
| macOS Intel/Apple Silicon | Standard/Advanced shared libraries and Xcode; ARM64 native suites and Intel ABI/link checks. |
| Android | Four supplied ABIs with matching NDK and APK packaging; Standard x64 emulator execution. |
| iOS/tvOS/visionOS | Supplied device or usable x86 simulator slice, matching Xcode SDK and static provider; package/link checks plus Standard tvOS x64 simulator execution. |

[Build profiles](build_profiles.tsv) define artifact selection. Support is limited
to profiles whose SDK and toolchain can be obtained and exercised on GitHub-hosted
runners. Nineteen specialized vendor BSP profiles are inventoried candidates
outside supported scope. [Hosted CI](ci/README.md) and the validation record
distinguish native, build/link-only, emulator/simulator, and physical-device results.

The SDK supplies no ARM64 Apple simulator slices, and its x86 visionOS slice has
no built-in Rust target. Those combinations are unavailable. RPi1 artifacts declare
ARMv7/VFPv3 attributes, so the directory name does not establish ARMv6 support.
The four RPi GNU cross profiles use a Debian armhf glibc 2.36 build sysroot;
build/link checks do not certify glibc 2.17 deployment or a particular board.

## Development

Common checks:

```bash
cargo fmt --all -- --check
cargo test
cargo clippy -- -D warnings
cargo build --examples
cargo test --all-features --test compile_contracts --test advanced_contracts
```

Ordinary tests skip the 39 SDK-dependent core tests. Use the
[confined native runner](CONTRIBUTING.md#tests-that-demonstrate-the-change) for runtime validation.

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, CI notes, and contribution guidelines.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
Private native ABI adapters retain their [upstream notices](src/native_abi_LICENSE.txt).
The NDI SDK and runtime are supplied separately under their vendor terms.

## Disclaimer

This is an unofficial community project and is not affiliated with NewTek or Vizrt.

NDI® is a registered trademark of Vizrt NDI AB.
