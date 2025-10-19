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

fn main() -> Result<(), grafton_ndi::Error> {
    // Initialize NDI
    let ndi = NDI::new()?;
    
    // Find sources on the network
    let finder_options = FinderOptions::builder().show_local_sources(true).build();
    let finder = Finder::new(&ndi, &finder_options)?;
    
    // Wait for sources
    finder.wait_for_sources(5000);
    let sources = finder.get_sources(5000)?;
    
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

## Usage Examples

### Finding NDI Sources

```rust
use grafton_ndi::{NDI, FinderOptions, Finder};

let ndi = NDI::new()?;

// Configure the finder
let finder_options = FinderOptions::builder()
    .show_local_sources(false)
    .groups("Public")
    .extra_ips("192.168.1.100")
    .build();

let finder = Finder::new(&ndi, &finder_options)?;

// Discover sources
if finder.wait_for_sources(5000) {
    let sources = finder.get_sources(0)?;
    for source in &sources {
        println!("Found: {} at {}", source.name, source.address);
    }
}
```

### Receiving Video

```rust
use grafton_ndi::{NDI, ReceiverOptions, Receiver, ReceiverColorFormat, ReceiverBandwidth, FrameType, Finder};

let ndi = NDI::new()?;

// First, find a source
let finder = Finder::new(&ndi, &Default::default())?;
finder.wait_for_sources(5000);
let sources = finder.get_sources(0)?;
let source = sources.first().ok_or("No sources found")?;

// Create receiver
let receiver = ReceiverOptions::builder(source.clone())
    .color(ReceiverColorFormat::RGBX_RGBA)
    .bandwidth(ReceiverBandwidth::Highest)
    .name("My Receiver")
    .build(&ndi)?;

// Capture frames
match receiver.capture(5000)? {
    FrameType::Video(video) => {
        println!("Video: {}x{} @ {}/{} fps", 
            video.width, video.height,
            video.frame_rate_n, video.frame_rate_d
        );
        // Process video data...
    }
    FrameType::Audio(audio) => {
        println!("Audio: {} channels @ {} Hz", 
            audio.num_channels, audio.sample_rate
        );
        // Access audio samples as f32
        let samples: &[f32] = audio.data();
        println!("First sample: {:.3}", samples[0]);
    }
    _ => {}
}
```

### Sending Video

```rust
use grafton_ndi::{NDI, Sender, SenderOptions, VideoFrame, FourCCVideoType};

let ndi = NDI::new()?;

// Configure sender
let options = SenderOptions::builder("My NDI Source")
    .groups("Public")
    .clock_video(true)
    .clock_audio(false)
    .build()?;

let sender = Sender::new(&ndi, &options)?;

// Create frame using builder
let frame = VideoFrame::builder()
    .resolution(1920, 1080)
    .fourcc(FourCCVideoType::BGRA)
    .frame_rate(60, 1)
    .aspect_ratio(16.0 / 9.0)
    .build()?;

// Frame is created with zero-initialized data
// You can access the data to fill it:
// let data = frame.data_mut();
// ... fill data with your video content ...

sender.send_video(&frame);
```

### Async Video Sending

```rust
use grafton_ndi::{NDI, Sender, SenderOptions, BorrowedVideoFrame, FourCCVideoType};
use std::sync::Arc;

let ndi = NDI::new()?;
let sender = Sender::new(&ndi, &SenderOptions::builder("Async Source").build()?)?;

// Register completion callback
let completed = Arc::new(std::sync::atomic::AtomicU32::new(0));
let completed_clone = completed.clone();
sender.on_async_video_done(move |frame_id| {
    completed_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    println!("Frame {} can be reused", frame_id);
});

// Send frame asynchronously
let buffer = vec![0u8; 1920 * 1080 * 4];
let frame = BorrowedVideoFrame::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);

// Token automatically manages frame lifetime
let token = sender.send_video_async(&frame);

// Buffer can be safely reused when token is dropped or completion callback fires
drop(token);

// Flush all pending frames with timeout
sender.flush_async(std::time::Duration::from_secs(5))?;
```

### Working with Audio

```rust
use grafton_ndi::{NDI, ReceiverOptions, ReceiverBandwidth};

// Assuming you already have a source from discovery
let receiver = ReceiverOptions::builder(source)
    .bandwidth(ReceiverBandwidth::AudioOnly)
    .build(&ndi)?;

// Capture audio frame
if let Some(audio) = receiver.capture_audio(5000)? {
    // Audio samples are 32-bit floats
    let samples: &[f32] = audio.data();
    
    // Calculate RMS level
    let rms = (samples.iter()
        .map(|&x| x * x)
        .sum::<f32>() / samples.len() as f32)
        .sqrt();
    
    // Access individual channels (stereo example)
    if let Some(left) = audio.channel_data(0) {
        println!("Left channel: {} samples", left.len());
    }
    if let Some(right) = audio.channel_data(1) {
        println!("Right channel: {} samples", right.len());
    }
}
```

### PTZ Camera Control

```rust
use grafton_ndi::{NDI, ReceiverOptions};

// Assuming you already have a source from discovery
let receiver = ReceiverOptions::builder(source).build(&ndi)?;

// Check PTZ support
if receiver.ptz_is_supported()? {
    // Control camera
    receiver.ptz_zoom(0.5)?;         // Zoom to 50%
    receiver.ptz_pan_tilt(0.0, 0.25)?; // Pan center, tilt up 25%
    receiver.ptz_auto_focus()?;       // Enable auto-focus
}
```

### Source Caching (New in 0.9)

```rust
use grafton_ndi::SourceCache;

// Create a shared cache instance
let cache = SourceCache::new();

// Find sources by hostname or IP - automatically cached
let sources = cache.find_by_host("192.168.1.100", 5000)?;

for source in &sources {
    println!("Found: {} ({})", source.name, source.ip_address().unwrap_or_default());
}

// Cache automatically reuses NDI instances and discovered sources
// Eliminates ~150 lines of manual caching code per application

// Invalidate cache when a source goes offline
cache.invalidate("192.168.1.100");

// Check cache state
println!("Cached hosts: {}", cache.len());
```

### Reliable Frame Capture with Retry Logic (New in 0.9)

```rust
use grafton_ndi::{NDI, ReceiverOptions, ReceiverColorFormat};

let ndi = NDI::new()?;
let receiver = ReceiverOptions::builder(source)
    .color(ReceiverColorFormat::RGBX_RGBA)
    .build(&ndi)?;

// Recommended: Block until frame arrives or timeout (handles NDI SDK timing quirks)
match receiver.capture_video_blocking(5000)? {
    Some(video) => {
        println!("Got video: {}x{}", video.width, video.height);
    }
    None => {
        println!("No video frame within 5 seconds");
    }
}

// Or use fine-grained retry control
match receiver.capture_video_with_retry(100, 50)? {
    Some(video) => println!("Frame captured"),
    None => println!("Timeout after 50 attempts"),
}
```

### Image Encoding (New in 0.9, requires `image-encoding` feature)

```rust
use grafton_ndi::{NDI, ReceiverOptions, ImageFormat};

let receiver = ReceiverOptions::builder(source).build(&ndi)?;

if let Some(video) = receiver.capture_video_blocking(5000)? {
    // One-line PNG encoding
    let png_bytes = video.encode_png()?;
    std::fs::write("frame.png", png_bytes)?;

    // JPEG with quality control
    let jpeg_bytes = video.encode_jpeg(85)?;

    // Base64 data URL for HTML/JSON
    let data_url = video.encode_data_url(ImageFormat::Jpeg(90))?;
    println!("data:image/jpeg;base64,..."); // Ready for HTML <img> tags
}
```

### Receiver Presets (New in 0.9)

```rust
use grafton_ndi::{NDI, ReceiverOptionsBuilder};

let ndi = NDI::new()?;

// Optimized for AI/image processing (low bandwidth, RGBA)
let snapshot_receiver = ReceiverOptionsBuilder::snapshot_preset(source.clone())?
    .build(&ndi)?;

// Full resolution, highest bandwidth for production
let hq_receiver = ReceiverOptionsBuilder::high_quality_preset(source.clone())?
    .build(&ndi)?;

// Metadata-only for tally/status monitoring
let monitoring_receiver = ReceiverOptionsBuilder::monitoring_preset(source)?
    .build(&ndi)?;
```

### Async Runtime Integration (New in 0.9, requires `tokio` or `async-std` feature)

```rust
use grafton_ndi::{AsyncReceiver, ReceiverOptionsBuilder};

#[tokio::main]
async fn main() -> Result<(), grafton_ndi::Error> {
    let source = /* ... discover source ... */;

    // Create async receiver (uses Arc internally for sharing)
    let receiver = AsyncReceiver::new(
        ReceiverOptionsBuilder::snapshot_preset(source)?.build_async()?
    );

    // All capture methods are async
    if let Some(video) = receiver.capture_video_blocking(5000).await? {
        println!("Async frame: {}x{}", video.width, video.height);

        // Image encoding works seamlessly
        #[cfg(feature = "image-encoding")]
        {
            let png = video.encode_png()?;
            tokio::fs::write("async_frame.png", png).await?;
        }
    }

    Ok(())
}
```

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
// Assuming source is from Finder::get_sources()
let receiver = ReceiverOptions::builder(source)
    .color(ReceiverColorFormat::UYVY_BGRA)
    .bandwidth(ReceiverBandwidth::Highest)
    .build(&ndi)?;
```

### `Sender` - Video/Audio Transmission
Sends video, audio, and metadata as an NDI source.

```rust
let sender = Sender::new(&ndi, &SenderOptions::builder("Source Name")
    .clock_video(true)
    .build()?)?);
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
use grafton_ndi::{NDI, ReceiverOptions, RecvStatus};

// Assuming you already have a source from discovery
let receiver = ReceiverOptions::builder(source).build(&ndi)?;

// Get current connection status
let status: RecvStatus = receiver.get_status();
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

This release dramatically improves ergonomics and reduces boilerplate for common NDI workflows. Based on production usage feedback, we've eliminated hundreds of lines of repetitive code that users were implementing in every application.

### Major Features

#### Source Caching & Discovery Helpers
- **`SourceCache`**: Thread-safe caching for NDI instances and discovered sources
  - Eliminates ~150 lines of manual caching code per application
  - Handles expensive NDI initialization and discovery internally
  - Methods: `new()`, `find_by_host()`, `invalidate()`, `clear()`, `len()`, `is_empty()`
- **Source matching helpers**: `matches_host()`, `ip_address()`, `host()` methods on `Source`
- **Address parsing**: `contains_host()`, `port()` methods on `SourceAddress`

#### Image Encoding Support (Feature: `image-encoding`)
- **One-line image export**: `encode_png()`, `encode_jpeg(quality)`, `encode_data_url(format)`
- Automatic BGRA ↔ RGBA color conversion
- Eliminates ~30 lines of encoding logic + 2 dependencies per application
- Ready for HTML/JSON integration with base64 data URLs

#### Reliable Frame Capture with Retry Logic
- **Blocking capture methods**: `capture_video_blocking()`, `capture_audio_blocking()`, `capture_metadata_blocking()`
- **Fine-grained retry**: `capture_video_with_retry()`, `capture_audio_with_retry()`, `capture_metadata_with_retry()`
- Handles NDI SDK timing quirks automatically
- Eliminates ~40 lines of retry loop code per application
- Detailed timeout errors with attempt count and elapsed time

#### Async Runtime Integration (Features: `tokio`, `async-std`)
- **`AsyncReceiver`**: Full async/await support for Tokio and async-std
- All 9 capture methods (video/audio/metadata × 3 variants)
- Proper `spawn_blocking` usage prevents runtime blocking
- Arc-based sharing for async contexts

#### Receiver Configuration Presets
- **Optimized presets**: `snapshot_preset()`, `high_quality_preset()`, `monitoring_preset()`
- Self-documenting API guides users to optimal settings
- Reduces configuration boilerplate

#### Enhanced Error Handling
- **Specific error variants**: `FrameTimeout`, `NoSourcesFound`, `SourceUnavailable`, `Disconnected`
- Rich error context for better debugging
- Pattern matching friendly

### Fixed
- **Audio sending now works correctly** (was completely broken in 0.8)
  - New `AudioLayout` enum for explicit planar/interleaved control
  - `channel_stride_in_bytes` now properly calculated
  - Default changed to planar layout (matching FLTP semantics)

### Added
- **`Finder::get_current_sources()`**: Instant source list without blocking
- Comprehensive documentation with real-world examples
- 28 tests (up from 13 in v0.8)

## Migration Guides

For upgrading from previous versions:
- [0.8.x to 0.9.x](docs/migration/0.8-to-0.9.md) - Ergonomic improvements, source caching, and audio fixes
- [0.7.x to 0.8.x](docs/migration/0.7-to-0.8.md) - Async API additions
- [0.6.x to 0.7.x](docs/migration/0.6-to-0.7.md) - Major API improvements
- [0.5.x to 0.6.x](docs/migration/0.5-to-0.6.md) - Builder patterns and zero-copy operations
- [0.4.x to 0.5.x](docs/migration/0.4-to-0.5.md) - Critical safety fixes and error handling
- [0.3.x to 0.4.x](docs/migration/0.3-to-0.4.md) - Memory management improvements
- [0.2.x to 0.3.x](docs/migration/0.2-to-0.3.md) - Lifetime changes and better validation