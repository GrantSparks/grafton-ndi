# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi)
[![Documentation](https://docs.rs/grafton-ndi/badge.svg)](https://docs.rs/grafton-ndi)
[![CI](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml/badge.svg)](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE)
[![Minimum Rust Version](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

High-performance, idiomatic Rust bindings for the [NDI® 6 SDK](https://ndi.video/), enabling real-time, low-latency IP video streaming. Built for production use with zero-copy performance and comprehensive async support.

## Features

- **Zero-copy receive** - Eliminates ~475 MB/s of memcpy at 1080p@60fps with borrowed frames
- **Zero-copy send** - Async video transmission with completion callbacks
- **Memory safe** - Eliminated 5+ classes of UB through compile-time enforcement
- **Source caching** - Thread-safe `SourceCache` eliminates ~150 lines of boilerplate
- **Image encoding** - One-line PNG/JPEG encoding and base64 data URLs (optional feature)
- **Async runtime support** - Native integration with Tokio and async-std (optional features)
- **Thread-safe by design** - Safe concurrent access with Rust's ownership model
- **Frame synchronization** - Clock-corrected capture with automatic audio resampling via `FrameSync`
- **Ergonomic API** - Consistent, idiomatic Rust interface ready for 1.0
- **Comprehensive type safety** - Strongly-typed with forward-compatible `#[non_exhaustive]` enums
- **Cross-platform** - Full support for Windows, Linux, and macOS
- **Battle-tested** - Used in production video streaming applications
- **Advanced SDK support** - Optional features for NDI Advanced SDK users

## Quick Start

```rust
use grafton_ndi::{NDI, FinderOptions, Finder};
use std::time::Duration;

fn main() -> Result<(), grafton_ndi::Error> {
    // Initialize NDI
    let ndi = NDI::new()?;

    // Find sources on the network
    let finder_options = FinderOptions::builder().show_local_sources(true).build();
    let finder = Finder::new(&ndi, &finder_options)?;

    // Discover sources
    let sources = finder.find_sources(Duration::from_secs(5))?;

    for source in sources {
        println!("Found source: {}", source);
    }

    Ok(())
}
```

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
grafton-ndi = "0.11"

# For NDI Advanced SDK features (optional)
# grafton-ndi = { version = "0.11", features = ["advanced_sdk"] }

# For image encoding support (PNG/JPEG)
# grafton-ndi = { version = "0.11", features = ["image-encoding"] }

# For async runtime integration
# grafton-ndi = { version = "0.11", features = ["tokio"] }
# grafton-ndi = { version = "0.11", features = ["async-std"] }
```

### Prerequisites

1. **NDI SDK**: Download and install the [NDI SDK](https://ndi.video/type/developer/) for your platform.
   - Windows: Installs to `C:\Program Files\NDI\NDI 6 SDK` by default
   - Linux: Extract to `/usr/share/NDI SDK for Linux` or set `NDI_SDK_DIR`
   - macOS: Installs to `/Library/NDI SDK for Apple` by default

2. **Rust**: Requires Rust 1.75 or later

3. **Build Dependencies**:
   - Windows: Visual Studio 2019+ or Build Tools, LLVM/Clang for bindgen
   - Linux: GCC/Clang, pkg-config, LLVM
   - macOS: Xcode Command Line Tools

4. **Runtime**: NDI runtime libraries must be available:
   - Windows: Ensure `%NDI_SDK_DIR%\Bin\x64` is in your PATH
   - Linux: Install NDI Tools or add library path to LD_LIBRARY_PATH
   - macOS: Install NDI Tools or configure DYLD_LIBRARY_PATH

## Documentation

For complete API documentation and detailed examples:

- **[API Documentation](https://docs.rs/grafton-ndi)** - Full API reference with examples
- **[Examples Directory](examples/)** - Runnable examples for common use cases
- **[CHANGELOG.md](CHANGELOG.md)** - Version history and migration guides

## Core Types

### `NDI` - Runtime Management
The main entry point that manages NDI library initialization and lifecycle.

```rust
let ndi = NDI::new()?; // Reference-counted, thread-safe
```

### `Finder` - Source Discovery
Discovers NDI sources on the network.

```rust
let finder_options = FinderOptions::builder()
    .show_local_sources(true)
    .groups("Public,Private")
    .build();
let finder = Finder::new(&ndi, &finder_options)?;
```

### `Receiver` - Video/Audio Reception
Receives video, audio, and metadata from NDI sources.

```rust
use std::time::Duration;

// Assuming source is from finder.find_sources() or finder.sources()
let options = ReceiverOptions::builder(source)
    .color(ReceiverColorFormat::RGBX_RGBA)
    .bandwidth(ReceiverBandwidth::Highest)
    .build();  // Infallible
let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;

// Capture a video frame (blocks until success or timeout)
let frame = receiver.capture_video(Duration::from_secs(5))?;

// Or use zero-copy for maximum performance
let frame_ref = receiver.capture_video_ref(Duration::from_secs(5))?;
let data = frame_ref.data();  // Direct reference, no copy!
```

### `Sender` - Video/Audio Transmission
Sends video, audio, and metadata as an NDI source.

```rust
let options = SenderOptions::builder("Source Name")
    .clock_video(true)
    .build();  // Infallible
let sender = grafton_ndi::Sender::new(&ndi, &options)?;

// Synchronous send
sender.send_video(&video_frame);

// Or async zero-copy send (requires &mut self)
let mut sender = grafton_ndi::Sender::new(&ndi, &options)?;
let token = sender.send_video_async(&borrowed_frame);
```

### `FrameSync` - Clock-Corrected Capture
Wraps a `Receiver` to provide pull-based capture with automatic time-base correction and dynamic audio resampling. Captures always return immediately.

```rust
use grafton_ndi::FrameSync;

// FrameSync takes ownership of the receiver
let frame_sync = FrameSync::new(receiver)?;

// Capture clock-corrected video (returns immediately)
if let Some(video) = frame_sync.capture_video(ScanType::Progressive) {
    println!("Video: {}x{}", video.width(), video.height());
}

// Capture resampled audio at requested rate/channels/samples
let audio = frame_sync.capture_audio(48000, 2, 1024);

// Access the underlying receiver for tally, PTZ, or status
let recv = frame_sync.receiver();

// Recover the receiver when done
let receiver = frame_sync.into_receiver();
```

### `PixelFormat` - Format Utilities
Pixel format information with compile-time computation for stride and buffer sizes.

```rust
use grafton_ndi::PixelFormat;

let stride = PixelFormat::BGRA.line_stride(1920);
let size = PixelFormat::BGRA.buffer_size(1920, 1080);
let info = PixelFormat::BGRA.info();  // PixelFormatInfo with bytes_per_pixel, category
```

### Frame Types
- **Owned Frames:**
  - `VideoFrame` - Owned video frame data with resolution, pixel format, and timing
  - `AudioFrame` - Owned 32-bit float audio samples with channel configuration
  - `MetadataFrame` - Owned XML metadata for tally, PTZ, and custom data

- **Borrowed Frames (Zero-Copy):**
  - `VideoFrameRef<'rx>` - Zero-copy video frame reference (eliminates ~475 MB/s memcpy @ 1080p60)
  - `AudioFrameRef<'rx>` - Zero-copy audio frame reference
  - `MetadataFrameRef<'rx>` - Zero-copy metadata frame reference
  - `BorrowedVideoFrame<'buf>` - Zero-copy send frame (for async transmission)
  - `FrameSyncVideoRef<'fs>` / `FrameSyncAudioRef<'fs>` - Zero-copy frames from `FrameSync`

## Thread Safety

All primary types (`Finder`, `Receiver`, `Sender`, `FrameSync`) are `Send + Sync` as the underlying NDI SDK is thread-safe. You can safely share instances across threads, though performance is best when keeping instances thread-local. Note that borrowed frame references (`*FrameRef`, `FrameSyncVideoRef`, `FrameSyncAudioRef`) are not `Send`, as they hold references to SDK-internal buffers.

## Performance Considerations

- **Zero-copy**: Frame data directly references NDI's internal buffers when possible
- **Bandwidth modes**: Use `ReceiverBandwidth::Lowest` for preview quality
- **Frame recycling**: Reuse frame allocations in tight loops
- **Thread affinity**: Keep NDI operations on consistent threads for best performance

### Receiver Status Monitoring

```rust
use grafton_ndi::{NDI, ReceiverOptions, Receiver};
use std::time::Duration;

// Assuming you already have a source from discovery
let options = ReceiverOptions::builder(source).build();
let receiver = Receiver::new(&ndi, &options)?;

// Check connection status
if receiver.is_connected() {
    // Get performance statistics
    let stats = receiver.connection_stats();
    println!("Connections: {}", stats.connections);
    println!("Video frames received: {}", stats.video_frames_received);
    println!("Video frames dropped: {}", stats.video_frames_dropped);

    // Monitor receiver performance using built-in helper
    let drop_rate = stats.video_drop_percentage();
    if drop_rate > 1.0 {
        eprintln!("High drop rate: {:.1}%", drop_rate);
    }
}

// Poll for status changes (tally, connections, etc.)
if let Some(status) = receiver.poll_status_change(Duration::from_millis(100))? {
    if let Some(connections) = status.connections {
        println!("Connection count changed: {}", connections);
    }
    if let Some(tally) = status.tally {
        println!("Tally: program={}, preview={}", tally.on_program, tally.on_preview);
    }
}
```

## Examples

See the `examples/` directory for complete applications:

### Discovery & Monitoring
- `NDIlib_Find.rs` - Discover NDI sources on the network
- `status_monitor.rs` - Monitor receiver status and performance

### Receiving
- `NDIlib_Recv_Audio.rs` - Receive and process audio streams
- `NDIlib_Recv_Audio_16bpp.rs` - Receive 16-bit audio samples
- `NDIlib_Recv_FrameSync.rs` - Clock-corrected capture with FrameSync
- `NDIlib_Recv_PNG.rs` - Receive video and save as PNG images
- `NDIlib_Recv_PTZ.rs` - Control PTZ cameras
- `concurrent_capture.rs` - Capture from multiple sources simultaneously

### Sending
- `NDIlib_Send_Audio.rs` - Send audio streams
- `NDIlib_Send_Video.rs` - Send video streams
- `async_send.rs` - Async video sending with completion callbacks
- `zero_copy_send.rs` - Zero-copy video transmission

Run examples with:
```bash
cargo run --example NDIlib_Find
```

## Platform Support

| Platform | Status | Notes |
|----------|--------|-------|
| Windows | ✅ Fully supported | Tested on Windows 10/11 |
| Linux | ✅ Fully supported | Tested on Ubuntu 20.04+ |
| macOS | ⚠️ Experimental | Limited testing |

## Contributing

Contributions are welcome! Please see our [Contributing Guidelines](CONTRIBUTING.md).

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

## Disclaimer

This is an unofficial community project and is not affiliated with NewTek or Vizrt.

NDI® is a registered trademark of Vizrt NDI AB.
