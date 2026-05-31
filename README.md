# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi)
[![Documentation](https://docs.rs/grafton-ndi/badge.svg)](https://docs.rs/grafton-ndi)
[![CI](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml/badge.svg)](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE)
[![Minimum Rust Version](https://img.shields.io/badge/rust-1.87%2B-orange.svg)](https://www.rust-lang.org)

`grafton-ndi` is an idiomatic Rust interface to the NDI® 6 SDK. It gives Rust applications a practical way to discover NDI sources, receive and publish video/audio/metadata streams, monitor status and tally, control PTZ devices, and integrate NDI work into synchronous or async application architectures.

The crate is intentionally a binding layer rather than a media framework. It keeps the generated C bindings behind a narrow FFI boundary and exposes Rust types that are safe to compose with the rest of an application. You bring the renderer, mixer, encoder, storage layer, or UI; `grafton-ndi` handles the NDI-facing parts.

## What It Covers

- **Runtime lifecycle** - Initialize and tear down the process-global NDI runtime through cheap, reference-counted `NDI` handles.
- **Network discovery** - Discover sources, wait for source-list changes, search groups or extra IP ranges, and cache host/IP lookups.
- **Receiving** - Capture video, audio, and metadata with configurable bandwidth and color-format choices.
- **Sending** - Publish an NDI source and send video, audio, metadata, connection metadata, and failover information.
- **FrameSync** - Use NDI's clock-corrected pull API for playback, render loops, and audio-device driven workflows.
- **Monitoring** - Query receiver connection status, frame-drop statistics, tally state, and sender connection counts.
- **PTZ control** - Drive supported pan, tilt, zoom, focus, exposure, and white-balance commands.
- **Async integration** - Use optional Tokio or async-std wrappers for receive workflows without blocking the async runtime.
- **Image snapshots** - Encode captured video frames as PNG, JPEG, or data URLs with the default `image-encoding` feature.
- **Advanced SDK hooks** - Enable Advanced SDK-specific functionality when your installed SDK exposes those symbols.

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
grafton-ndi = "0.12"
```

Feature flags:

```toml
# Minimal build without PNG/JPEG/data URL helpers
# grafton-ndi = { version = "0.12", default-features = false }

# Image encoding support is enabled by default
# grafton-ndi = { version = "0.12", features = ["image-encoding"] }

# Async receiver wrappers
# grafton-ndi = { version = "0.12", features = ["tokio"] }
# grafton-ndi = { version = "0.12", features = ["async-std"] }

# Advanced SDK symbols, when available from the installed SDK
# grafton-ndi = { version = "0.12", features = ["advanced_sdk"] }
```

### Prerequisites

1. **NDI SDK 6.x**: Install the [NDI SDK](https://ndi.video/type/developer/) for your platform.
   - Windows default: `C:\Program Files\NDI\NDI 6 SDK`
   - Linux defaults: `/usr/share/NDI Advanced SDK for Linux` or `/usr/share/NDI SDK for Linux`
   - macOS defaults include `/Library/NDI SDK for macOS`, `/Library/NDI SDK for Apple`, and `/Library/NDI 6 SDK`
   - Set `NDI_SDK_DIR` when the SDK is installed elsewhere.

2. **Rust**: Rust 1.87 or later.

3. **Build dependencies**:
   - Windows: Visual Studio 2019+ or Build Tools, plus LLVM/Clang for bindgen
   - Linux: a C toolchain and LLVM/Clang headers for bindgen
   - macOS: Xcode Command Line Tools

4. **Runtime libraries**:
   - Windows: ensure the NDI runtime DLL directory is on `PATH`
   - Linux: install the NDI runtime/tools or configure `LD_LIBRARY_PATH`
   - macOS: install the NDI runtime/tools or configure `DYLD_LIBRARY_PATH` as needed

## Core Workflows

### Discover Sources

`Finder` wraps the NDI discovery API. It can return a current source snapshot, wait for source-list changes, or perform a bounded discovery pass.

```rust
use grafton_ndi::{Finder, FinderOptions};
use std::time::Duration;

let options = FinderOptions::builder()
    .show_local_sources(true)
    .groups("Public,Studio")
    .extra_ips("192.168.1.0/24")
    .build();
let finder = Finder::new(&ndi, &options)?;

if finder.wait_for_sources(Duration::from_secs(2))? {
    for source in finder.current_sources()? {
        println!("{source}");
    }
}
```

For applications that repeatedly reconnect to known devices, `SourceCache` handles runtime initialization, discovery, host/IP matching, and cache invalidation.

```rust
use grafton_ndi::SourceCache;
use std::time::Duration;

let cache = SourceCache::new()?;
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
use std::num::NonZeroI32;

let frame_sync = FrameSync::new(receiver)?;

if let Some(video) = frame_sync.capture_video(ScanType::Progressive)? {
    println!("video: {}x{}", video.width(), video.height());
}

let audio = frame_sync.capture_audio(FrameSyncAudioRequest::capture(
    NonZeroI32::new(1024).unwrap(),
))?;
if !audio.is_empty() {
    println!("audio: {} channels at {} Hz", audio.num_channels(), audio.sample_rate());
}
```

### Monitor and Control

Receivers can report connection health, frame-drop statistics, tally changes, and PTZ support.

```rust
use std::time::Duration;

if receiver.is_connected() {
    let stats = receiver.connection_stats();
    println!("video drop rate: {:.2}%", stats.video_drop_percentage());
}

if let Some(status) = receiver.poll_status_change(Duration::from_millis(100))? {
    if let Some(tally) = status.tally {
        println!(
            "tally: program={}, preview={}",
            tally.on_program,
            tally.on_preview
        );
    }
}

if receiver.ptz_is_supported() {
    receiver.ptz_zoom(0.25)?;
}
```

### Integrate With Async Runtimes

NDI receive calls are fundamentally blocking SDK calls. The optional async wrappers make that explicit by running receive work on the runtime's blocking pool while preserving the timeout budget from the moment the async method is called.

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

Most applications can start with owned frame APIs such as `receiver.video().capture()` and `sender.send_video()`. For hot paths, the crate also exposes borrowed receive refs and borrowed async-send video frames so data can stay in SDK or application buffers without an extra copy. Those zero-copy APIs use Rust lifetimes to make buffer reuse explicit.

Frame layout fields that describe SDK-facing memory are private. Builders, accessors, checked mutation methods, and `PixelFormat` helpers keep dimensions, strides, metadata, and buffer sizes consistent before data crosses the FFI boundary.

## Design Approach

`grafton-ndi` aims to be a predictable Rust layer over NDI, not a replacement for the NDI SDK documentation.

- **Small FFI boundary**: generated bindings live behind safe wrappers.
- **RAII lifecycle**: NDI handles, senders, receivers, frames, and async send tokens clean up through ownership.
- **Checked frame descriptions**: frame dimensions, line strides, channel strides, metadata strings, and buffer sizes are validated before slices or strings are exposed.
- **Explicit blocking behavior**: synchronous methods block with checked timeouts; async adapters use `spawn_blocking`.
- **Forward-compatible SDK enums**: public SDK-mode enums that may grow are `#[non_exhaustive]`.

## Performance and Safety

- Use owned frame APIs when clarity matters or when frame data must outlive the SDK capture buffer.
- Use borrowed receive refs when you need direct access to SDK-owned buffers during a tight capture loop.
- Use borrowed async-send video frames when you want NDI to send from an application buffer without copying.
- Use `FrameSync` when capture timing is driven by an external output clock.
- Use lower bandwidth modes for previews and monitoring surfaces.
- Keep image encoding off real-time paths unless the workload is sized for compression.

The compile-contract tests in `tests/ui` verify the important lifetime rules around borrowed receive refs and async send tokens.

## Image Encoding

With the default `image-encoding` feature, captured `VideoFrame` values can be encoded directly:

```rust
use grafton_ndi::ImageFormat;

let png = video.encode_png()?;
let jpeg = video.encode_jpeg(85)?;
let data_url = video.encode_data_url(ImageFormat::Png)?;
```

## Documentation

- [API documentation](https://docs.rs/grafton-ndi) - Full rustdoc reference.
- [Examples](examples/) - Runnable examples for discovery, receiving, FrameSync, PTZ, monitoring, and sending.
- [CHANGELOG.md](CHANGELOG.md) - Release notes and migration guidance.
- [Migration notes](https://github.com/GrantSparks/grafton-ndi/tree/main/docs/migration) - Older version-to-version migration notes.

## Examples

Run examples with:

```bash
cargo run --example NDIlib_Find
```

Discovery and monitoring:

- `NDIlib_Find.rs` - Discover NDI sources on the network.
- `status_monitor.rs` - Monitor receiver connection status, tally, and frame drops.

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
- `async_send.rs` - Demonstrate async video send tokens and completion callbacks.
- `zero_copy_send.rs` - Send borrowed video buffers without copying.

## Platform Support

| Platform | Status | Notes |
|----------|--------|-------|
| Windows | CI-tested | Uses the NDI SDK import library at build time and NDI runtime DLLs at runtime. |
| Linux | CI-tested | Supports standard and Advanced SDK install directories; runtime libraries must be discoverable by the dynamic linker. |
| macOS | CI-tested | Supports current NDI SDK package layouts used by the CI setup action and common local install paths. |

## Development

Common checks:

```bash
cargo fmt --all -- --check
cargo test
cargo clippy -- -D warnings
cargo build --examples
cargo test --test compile_contracts
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, CI notes, and contribution guidelines.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

## Disclaimer

This is an unofficial community project and is not affiliated with NewTek or Vizrt.

NDI® is a registered trademark of Vizrt NDI AB.
