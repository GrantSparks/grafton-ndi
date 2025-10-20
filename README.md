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
grafton-ndi = "0.9"

# For NDI Advanced SDK features (optional)
# grafton-ndi = { version = "0.9", features = ["advanced_sdk"] }

# For image encoding support (PNG/JPEG)
# grafton-ndi = { version = "0.9", features = ["image-encoding"] }

# For async runtime integration
# grafton-ndi = { version = "0.9", features = ["tokio"] }
# grafton-ndi = { version = "0.9", features = ["async-std"] }
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
let mut sender = grafton_ndi::Sender::new(&ndi, &options)?;  // Must be mut for async send

// Synchronous send
sender.send_video(&video_frame);

// Or async zero-copy send
let token = sender.send_video_async(&borrowed_frame);
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

## Thread Safety

All primary types (`Finder`, `Receiver`, `Sender`) are `Send + Sync` as the underlying NDI SDK is thread-safe. You can safely share instances across threads, though performance is best when keeping instances thread-local.

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

## What's New in 0.9

Version 0.9.0 is a **major milestone** toward 1.0, with comprehensive API stabilization and significant improvements:

### 🎯 API Stabilization for 1.0
- **Duration-based timeouts** - All timeout parameters now use `std::time::Duration` instead of `u32` milliseconds
- **Consistent builders** - Both `Receiver` and `Sender` have symmetric, infallible builders
- **Simplified capture** - 2 clear variants instead of 3 confusing ones (`capture_*` and `capture_*_timeout`)
- **Type renames** - `FourCCVideoType` → `PixelFormat`, `FrameFormatType` → `ScanType`, `AudioType` → `AudioFormat`
- **Forward compatibility** - All enums marked `#[non_exhaustive]` for future SDK versions
- **Cleaner naming** - Removed `get_` prefixes per Rust API guidelines

### 🚀 Zero-Copy Performance
- **Zero-copy receive** - New `VideoFrameRef`, `AudioFrameRef`, `MetadataFrameRef` types
- **Performance gain** - Eliminates ~475 MB/s of memcpy at 1080p@60fps
- **Lifetime-safe** - Frame refs bound to `Receiver` lifetime, preventing use-after-free at compile-time

### 🔒 Memory Safety & Correctness
- **Sound async send** - Fixed critical use-after-free in `send_video_async`
- **Typed stride/size** - Eliminated UB from union field access with typed `LineStrideOrSize` enum
- **Non-null FFI** - All source pointers validated at FFI boundary
- **Safe callbacks** - Fixed memory leaks and races in async completion callbacks

### 🛠️ Ergonomics & Features
- **Source caching** - Thread-safe `SourceCache` eliminates ~150 lines of boilerplate
- **Image encoding** - One-line PNG/JPEG export (optional `image-encoding` feature)
- **Async runtimes** - Native Tokio and async-std integration (optional features)
- **Audio fixed** - Audio sending now actually works with proper `AudioLayout` support

### ⚠️ Breaking Changes
This release contains extensive breaking changes necessary for API stabilization. See [CHANGELOG.md](CHANGELOG.md) for the comprehensive migration guide with before/after examples.

**Quick migration:**
```rust
// 0.8.1
finder.wait_for_sources(5000);
let sources = finder.get_sources(0)?;
let frame = receiver.capture_video_blocking(5000)?;

// 0.9.0
use std::time::Duration;
finder.wait_for_sources(Duration::from_secs(5))?;
let sources = finder.sources(Duration::ZERO)?;
let frame = receiver.capture_video(Duration::from_secs(5))?;
```

See [CHANGELOG.md](CHANGELOG.md) for complete details and migration guide.
