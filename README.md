# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi)
[![Documentation](https://docs.rs/grafton-ndi/badge.svg)](https://docs.rs/grafton-ndi)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE)
[![Minimum Rust Version](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

High-performance, idiomatic Rust bindings for the [NDI® 6 SDK](https://ndi.video/), enabling real-time, low-latency IP video streaming. Built for production use with zero-copy performance and comprehensive async support.

## Features

- **Zero-copy frame handling** - Minimal overhead for high-performance video processing
- **Async video sending** - Non-blocking video transmission with completion callbacks
- **Thread-safe by design** - Safe concurrent access with Rust's ownership model  
- **Ergonomic API** - Builder patterns and idiomatic Rust interfaces
- **Comprehensive type safety** - Strongly-typed color formats and frame types
- **Cross-platform** - Windows, Linux, and macOS support
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
grafton-ndi = "0.8"

# For NDI Advanced SDK features (optional)
# grafton-ndi = { version = "0.8", features = ["advanced_sdk"] }
```

### Prerequisites

1. **NDI SDK**: Download and install the [NDI SDK](https://ndi.video/type/developer/) for your platform.
   - Windows: Installs to `C:\Program Files\NDI\NDI 6 SDK` by default
   - Linux: Extract to `/usr/share/NDI SDK for Linux` or set `NDI_SDK_DIR`
   - macOS: Install and set `NDI_SDK_DIR` environment variable

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

### Async Video Sending (New in 0.8)

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

## What's New in 0.8

### Major Features
- **Async Video Sending**: Non-blocking video transmission with completion callbacks
- **Receiver Status API**: Monitor connection health and performance metrics
- **BorrowedVideoFrame**: Zero-copy frame type for optimal performance
- **AsyncVideoToken**: RAII tokens for safe async frame lifetime management
- **Advanced SDK Support**: Optional features for NDI Advanced SDK users

### Improvements
- Enhanced Windows compatibility with proper enum conversions
- Better error messages and documentation
- Improved CI/CD pipeline with automated testing
- Fixed potential race conditions in async operations

## Migration Guides

For upgrading from previous versions:
- [0.7.x to 0.8.x](docs/migration/0.7-to-0.8.md) - Async API additions
- [0.6.x to 0.7.x](docs/migration/0.6-to-0.7.md) - Major API improvements
- [0.5.x to 0.6.x](docs/migration/0.5-to-0.6.md) - Builder patterns and zero-copy operations
- [0.4.x to 0.5.x](docs/migration/0.4-to-0.5.md) - Critical safety fixes and error handling
- [0.3.x to 0.4.x](docs/migration/0.3-to-0.4.md) - Memory management improvements
- [0.2.x to 0.3.x](docs/migration/0.2-to-0.3.md) - Lifetime changes and better validation