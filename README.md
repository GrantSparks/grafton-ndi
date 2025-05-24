# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi)
[![Documentation](https://docs.rs/grafton-ndi/badge.svg)](https://docs.rs/grafton-ndi)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE)
[![Minimum Rust Version](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

High-performance, idiomatic Rust bindings for the [NDI® 6 SDK](https://ndi.video/), enabling real-time, low-latency IP video streaming. Requires NDI SDK 6.1.1 or later.

## Features

- **Zero-copy frame handling** - Minimal overhead for high-performance video processing
- **Thread-safe by design** - Safe concurrent access with Rust's ownership model  
- **Ergonomic API** - Builder patterns and idiomatic Rust interfaces
- **Comprehensive type safety** - Strongly-typed color formats and frame types
- **Cross-platform** - Windows, Linux, and macOS support
- **Battle-tested** - Used in production video streaming applications

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
grafton-ndi = "0.7"
```

### Prerequisites

1. **NDI SDK**: Download and install the [NDI SDK](https://ndi.video/type/developer/) for your platform.
   - Windows: Installs to `C:\Program Files\NDI\SDK` by default
   - Linux: Extract to `/usr/share/NDI SDK for Linux` or set `NDI_SDK_DIR`
   - macOS: Install and set `NDI_SDK_DIR` environment variable

2. **Rust**: Requires Rust 1.75 or later

3. **Platform Requirements**:
   - Windows: Visual Studio 2019+ or Build Tools
   - Linux: GCC/Clang, pkg-config
   - macOS: Xcode Command Line Tools

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
use grafton_ndi::{NDI, ReceiverOptions, Receiver, ReceiverColorFormat, ReceiverBandwidth};

let ndi = NDI::new()?;

// Create receiver
let receiver = ReceiverOptions::builder(source)
    .color(ReceiverColorFormat::RGBX_RGBA)
    .bandwidth(ReceiverBandwidth::Highest)
    .name("My Receiver")
    .build(&ndi)?;

// Capture frames
match receiver.capture(5000)? {
    Some(CapturedFrame::Video(video)) => {
        println!("Video: {}x{} @ {}/{} fps", 
            video.width, video.height,
            video.frame_rate_n, video.frame_rate_d
        );
        // Process video data...
    }
    Some(CapturedFrame::Audio(audio)) => {
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

// Create and send a frame
let frame = VideoFrame::builder()
    .resolution(1920, 1080)
    .fourcc(FourCCVideoType::BGRA)
    .frame_rate(60, 1)
    .aspect_ratio(16.0 / 9.0)
    .build()?;

// Allocate and fill frame data
let mut data = vec![0u8; frame.size()];
// ... fill data with your video content ...

frame.set_data(&data);
sender.send_video(&frame);
```

### Working with Audio

```rust
use grafton_ndi::{NDI, ReceiverOptions, ReceiverBandwidth};

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

## Examples

See the `examples/` directory for complete applications:

- `NDIlib_Find.rs` - Discover NDI sources on the network
- `NDIlib_Recv_PNG.rs` - Receive video and save as PNG images
- `NDIlib_Recv_PTZ.rs` - Control PTZ cameras

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

## Migration Guides

For upgrading from previous versions:
- [0.6.x to 0.7.x](docs/migration/0.6-to-0.7.md)
- [0.5.x to 0.6.x](docs/migration/0.5-to-0.6.md)
- [0.4.x to 0.5.x](docs/migration/0.4-to-0.5.md)