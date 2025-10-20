# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi)
[![Documentation](https://docs.rs/grafton-ndi/badge.svg)](https://docs.rs/grafton-ndi)
[![CI](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml/badge.svg)](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE)
[![Minimum Rust Version](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

High-performance, idiomatic Rust bindings for the [NDI® 6 SDK](https://ndi.video/), enabling real-time, low-latency IP video streaming. Built for production use with zero-copy performance and comprehensive async support.

## Features

- **Zero-copy frame handling** - Minimal overhead for high-performance video processing
- **Source caching & discovery** - Thread-safe caching eliminates repetitive discovery code
- **Image encoding** - One-line PNG/JPEG encoding and base64 data URLs (optional feature)
- **Retry logic** - Reliable frame capture with automatic retry and blocking variants
- **Async runtime support** - Native integration with Tokio and async-std (optional features)
- **Async video sending** - Non-blocking video transmission with completion callbacks
- **Thread-safe by design** - Safe concurrent access with Rust's ownership model
- **Ergonomic API** - Builder patterns, presets, and idiomatic Rust interfaces
- **Comprehensive type safety** - Strongly-typed color formats and frame types
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

// Assuming source is from finder.find_sources()
let options = ReceiverOptions::builder(source)
    .color(ReceiverColorFormat::UYVY_BGRA)
    .bandwidth(ReceiverBandwidth::Highest)
    .build();
let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;

// Capture a video frame (blocks until success or timeout)
let frame = receiver.capture_video(Duration::from_secs(5))?;
```

### `Sender` - Video/Audio Transmission
Sends video, audio, and metadata as an NDI source.

```rust
let options = SenderOptions::builder("Source Name")
    .clock_video(true)
    .build();
let sender = grafton_ndi::Sender::new(&ndi, &options)?;
```

### Frame Types
- `VideoFrame` - Video frame data with resolution, format, and timing
- `AudioFrame` - 32-bit float audio samples with channel configuration
- `MetadataFrame` - XML metadata for tally, PTZ, and custom data

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

// Poll for status changes
if let Some(status) = receiver.poll_status_change(Duration::from_millis(100)) {
    println!("Connected: {}", status.is_connected);
    println!("Video frames: {}", status.video_frames);
    println!("Audio frames: {}", status.audio_frames);

    // Monitor receiver performance
    if status.total_frames > 0 {
        let drop_rate = status.dropped_frames as f32 / status.total_frames as f32;
        if drop_rate > 0.01 {
            eprintln!("High drop rate: {:.1}%", drop_rate * 100.0);
        }
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

Major ergonomic improvements based on production usage:

- **Source Caching** - Thread-safe `SourceCache` eliminates ~150 lines of boilerplate per app
- **Image Encoding** - One-line PNG/JPEG export with `encode_png()` and `encode_data_url()` (optional `image-encoding` feature)
- **Reliable Frame Capture** - Built-in retry logic with `capture_video_blocking()` and friends
- **Async Runtime Support** - Native Tokio and async-std integration (optional features)
- **Audio Fix** - Audio sending now works correctly with new `AudioLayout` enum

See [CHANGELOG.md](CHANGELOG.md) for complete details and migration guide.

## Migration Guides

For upgrading from previous versions:
- [0.8.x to 0.9.x](docs/migration/0.8-to-0.9.md) - Ergonomic improvements, source caching, and audio fixes
- [0.7.x to 0.8.x](docs/migration/0.7-to-0.8.md) - Async API additions
- [0.6.x to 0.7.x](docs/migration/0.6-to-0.7.md) - Major API improvements
- [0.5.x to 0.6.x](docs/migration/0.5-to-0.6.md) - Builder patterns and zero-copy operations
- [0.4.x to 0.5.x](docs/migration/0.4-to-0.5.md) - Critical safety fixes and error handling
- [0.3.x to 0.4.x](docs/migration/0.3-to-0.4.md) - Memory management improvements
- [0.2.x to 0.3.x](docs/migration/0.2-to-0.3.md) - Lifetime changes and better validation
