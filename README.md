# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi)
[![Documentation](https://docs.rs/grafton-ndi/badge.svg)](https://docs.rs/grafton-ndi)
[![CI](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml/badge.svg)](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE)
[![Minimum Rust Version](https://img.shields.io/badge/rust-1.87%2B-orange.svg)](https://www.rust-lang.org)

`grafton-ndi` is a safe, idiomatic Rust layer over the NDI® 6 SDK for real-time video, audio, and metadata transport over IP networks.

The crate focuses on the core NDI workflows that production applications need: source discovery, receiving, sending, frame synchronization, status/tally monitoring, PTZ control, and optional async-runtime integration. The generated C bindings stay behind a small FFI boundary; the public API exposes validated Rust types with explicit ownership, checked frame layout math, and lifetime-enforced zero-copy access.

## Features

- **NDI runtime management** - Reference-counted `NDI` handles initialize the SDK once, keep it alive while in use, and allow retry after initialization failure.
- **Source discovery** - `Finder`, `Source`, `SourceAddress`, and `SourceCache` cover one-shot discovery, change polling, host/IP matching, and cached lookups.
- **Receive APIs** - Capture video, audio, and metadata as owned frames or zero-copy borrowed refs.
- **Send APIs** - Send owned video/audio/metadata synchronously, or send borrowed video buffers through NDI's async video pipeline.
- **FrameSync** - Pull-based, clock-corrected capture with automatic audio resampling for playback, mixing, and render-loop driven applications.
- **Invariant-preserving frames** - Frame fields that describe SDK layout are private; builders, accessors, and checked mutation APIs keep dimensions, strides, metadata, and buffer sizes valid.
- **Checked pixel format utilities** - `PixelFormat`, `PixelFormatInfo`, and `LineStrideOrSize` provide explicit packed/planar layout handling with overflow-checked size calculations.
- **Async runtime wrappers** - Optional Tokio and async-std receivers run blocking NDI calls on the runtime's blocking pool.
- **Image encoding** - Optional PNG, JPEG, and data URL helpers are enabled by default.
- **Cross-platform CI** - Windows, Linux, and macOS builds, tests, clippy, examples, and semver checks run in CI with NDI SDK setup actions.

## Design Approach

The project is intentionally a focused binding layer, not a media framework. It does not try to own color management, transcoding, scheduling, or rendering. Instead, it makes the NDI SDK's core primitives safe to compose in Rust applications.

- **Validate at the boundary**: raw SDK frames, C strings, dimensions, strides, metadata lengths, and buffer sizes are checked before safe Rust slices or strings are exposed.
- **Use ownership for lifecycle**: `Finder`, `Sender`, and `FrameSync` own the resources they need; `FrameSync` owns its `Receiver`, and `NDI` handles keep the process-global runtime alive.
- **Use lifetimes for buffers**: borrowed receive refs cannot outlive the SDK buffer owner, and async send tokens borrow the application buffer until NDI has released it.
- **Keep blocking semantics explicit**: synchronous receiver methods block with a checked timeout budget; async wrappers use `spawn_blocking` instead of pretending the SDK calls are natively async.
- **Prefer forward-compatible enums**: SDK-facing enums that may grow are `#[non_exhaustive]`, so downstream matches should include a wildcard arm.

## Quick Start

```rust
use grafton_ndi::{Finder, FinderOptions, NDI};
use std::time::Duration;

fn main() -> grafton_ndi::Result<()> {
    let ndi = NDI::new()?;

    let options = FinderOptions::builder()
        .show_local_sources(true)
        .build();
    let finder = Finder::new(&ndi, &options)?;

    let sources = finder.find_sources(Duration::from_secs(5))?;
    for source in sources {
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
# Pick the form that matches your application:

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

## Documentation

- [API documentation](https://docs.rs/grafton-ndi) - Full rustdoc reference.
- [Examples](examples/) - Runnable examples for discovery, receiving, FrameSync, PTZ, monitoring, and sending.
- [CHANGELOG.md](CHANGELOG.md) - Release notes and migration guidance.
- [Migration notes](https://github.com/GrantSparks/grafton-ndi/tree/main/docs/migration) - Older version-to-version migration notes.

## Core Types

### `NDI` - Runtime Lifecycle

`NDI` is the entry point for the SDK. Handles are cheap to clone and keep the process-global runtime initialized until the last handle is dropped.

```rust
let ndi = grafton_ndi::NDI::new()?;
println!("NDI version: {}", grafton_ndi::NDI::version()?);
```

### `Finder`, `Source`, and `SourceCache` - Discovery

Use `Finder` when you want direct control over discovery timing. Use `SourceCache` when an application repeatedly resolves a host, IP fragment, or source name.

```rust
use grafton_ndi::{Finder, FinderOptions, SourceCache};
use std::time::Duration;

let finder_options = FinderOptions::builder()
    .show_local_sources(true)
    .groups("Public,Studio")
    .extra_ips("192.168.1.0/24")
    .build();
let finder = Finder::new(&ndi, &finder_options)?;

if finder.wait_for_sources(Duration::from_secs(2))? {
    for source in finder.current_sources()? {
        println!("{source}");
    }
}

let cache = SourceCache::new()?;
let camera = cache.find_by_host("192.168.1.100", Duration::from_secs(5))?;
```

### `Receiver` - Video, Audio, and Metadata Capture

Receivers can return owned frames for convenience or borrowed frame refs for hot paths. Borrowed refs release the SDK buffer when dropped and cannot outlive the receiver.

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

let frame = receiver.capture_video_ref(Duration::from_secs(5))?;
println!(
    "{}x{} {:?}, {} bytes",
    frame.width(),
    frame.height(),
    frame.pixel_format(),
    frame.data().len()
);

let owned = frame.to_owned()?;
```

### `Sender` - Publishing NDI Sources

Owned frames are sent synchronously. Borrowed video frames can be sent through the SDK's async video path without copying; the returned token keeps the source buffer borrowed until completion.

```rust
use grafton_ndi::{
    BorrowedVideoFrame, PixelFormat, Sender, SenderOptions, VideoFrame,
};

let options = SenderOptions::builder("Rust Program Output")
    .clock_video(true)
    .clock_audio(true)
    .build();
let mut sender = Sender::new(&ndi, &options)?;

let mut frame = VideoFrame::builder()
    .resolution(1920, 1080)
    .pixel_format(PixelFormat::BGRA)
    .frame_rate(60, 1)
    .build()?;
frame.data_mut().fill(0);
sender.send_video(&frame);

let buffer = vec![0u8; PixelFormat::BGRA.try_buffer_size(1920, 1080)?];
let borrowed = BorrowedVideoFrame::try_from_uncompressed(
    &buffer,
    1920,
    1080,
    PixelFormat::BGRA,
    60,
    1,
)?;
let token = sender.send_video_async(&borrowed);
token.wait()?;
```

The `advanced_sdk` feature enables Advanced SDK-specific APIs when the installed headers expose those symbols. Without SDK completion callbacks, async video completion falls back to the SDK's null-frame flush contract.

### `FrameSync` - Clock-Corrected Pull Capture

`FrameSync` owns a `Receiver` and provides immediately-returning capture methods for applications driven by an external clock, such as a display refresh loop or audio device callback.

```rust
use grafton_ndi::{FrameSync, FrameSyncAudioRequest, ScanType};
use std::num::NonZeroI32;

let frame_sync = FrameSync::new(receiver)?;

if let Some(video) = frame_sync.capture_video(ScanType::Progressive)? {
    println!("Video: {}x{}", video.width(), video.height());
}

let audio = frame_sync.capture_audio(FrameSyncAudioRequest::capture(
    NonZeroI32::new(1024).unwrap(),
))?;
if !audio.is_empty() {
    println!(
        "Audio: {} channels at {} Hz",
        audio.num_channels(),
        audio.sample_rate()
    );
}

let receiver = frame_sync.into_receiver();
```

### Frames, Metadata, and Pixel Formats

Frame layout fields are private so that SDK-facing invariants stay intact. Use builders, accessors, `data_mut()`, `replace_data()`, `set_metadata()`, and checked pixel-format helpers.

```rust
use grafton_ndi::{MetadataFrame, PixelFormat};

let stride = PixelFormat::BGRA.try_line_stride(1920)?;
let bytes = PixelFormat::BGRA.try_buffer_size(1920, 1080)?;
let info = PixelFormat::NV12.info();

let metadata = MetadataFrame::with_data("<ndi_product/>", 0)?;
println!("stride={stride}, bytes={bytes}, category={:?}", info.category());
println!("metadata timecode={}", metadata.timecode());
```

With the default `image-encoding` feature, `VideoFrame` can encode common still-image formats:

```rust
use grafton_ndi::ImageFormat;

let png = owned.encode_png()?;
let jpeg = owned.encode_jpeg(85)?;
let data_url = owned.encode_data_url(ImageFormat::Png)?;
```

### Async Receivers

Enable `tokio` or `async-std` to wrap a `Receiver` in an async facade. The wrapper uses the runtime's blocking pool and preserves the timeout budget from the moment the async method is called.

```rust
use grafton_ndi::tokio::AsyncReceiver;
use std::time::Duration;

let async_receiver = AsyncReceiver::new(receiver);
let frame = async_receiver.capture_video(Duration::from_secs(5)).await?;
```

### Status, Tally, and PTZ

Receivers expose connection status, frame drop statistics, tally changes, and PTZ camera commands.

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

## Thread Safety

`Receiver`, `Sender`, and `FrameSync` are `Send + Sync` wrappers around SDK handles. They may be shared across threads when that matches the application's architecture, though keeping hot-path capture/send work on consistent threads is still best for performance.

Borrowed receive refs such as `VideoFrameRef`, `AudioFrameRef`, `MetadataFrameRef`, `FrameSyncVideoRef`, and `FrameSyncAudioRef` are intentionally lifetime-bound to the object that owns the SDK buffer. Async video tokens similarly keep application buffers borrowed until NDI has released them. The compile-time contracts for those lifetimes are covered by `trybuild` tests.

## Performance Notes

- Use `capture_video_ref`, `capture_audio_ref`, and `capture_metadata_ref` when avoiding copies matters.
- Use owned captures when you need to keep frame data after the receiver buffer is released.
- Use `FrameSync` for render-loop or audio-clock driven pull capture; raw `Receiver` capture is better for blocking receive loops.
- Choose receiver bandwidth and color format deliberately. Preview monitors often do not need `ReceiverBandwidth::Highest`.
- Image encoding performs format conversion and compression; keep it off critical real-time paths unless the workload is sized for it.

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

The compile contract tests verify that borrowed receive refs and async send tokens cannot be misused across invalid lifetimes. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup details, CI notes, and contribution guidelines.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

## Disclaimer

This is an unofficial community project and is not affiliated with NewTek or Vizrt.

NDI® is a registered trademark of Vizrt NDI AB.
