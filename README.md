# grafton-ndi

Unofficial idiomatic Rust bindings for the [NDI 6 SDK](https://ndi.video/for-developers/ndi-sdk/).

## New in 0.6.0

### 🚀 Major Performance & Safety Improvements

1. **Lifetime-bound Frames**: Video and audio frames are now lifetime-bound to their originating `Recv` instance, preventing use-after-free bugs at compile time.

2. **Zero-Copy Async Send**: New `VideoFrameBorrowed` type enables true zero-copy async send operations, eliminating frame cloning overhead.

3. **Concurrent Capture**: New thread-safe capture methods (`capture_video`, `capture_audio`, `capture_metadata`) allow concurrent frame capture from multiple threads without locking.

### Example: Zero-Copy Send
```rust
let mut buffer = vec![0u8; 1920 * 1080 * 4];
let frame = VideoFrameBorrowed::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);
let _token = send.send_video_async(frame); // No copy!
```

### Example: Concurrent Capture
```rust
let recv = Arc::new(recv);

// Video thread
let recv_video = Arc::clone(&recv);
thread::spawn(move || {
    while let Ok(Some(frame)) = recv_video.capture_video(5000) {
        // Process video frame
    }
});

// Audio thread - runs concurrently!
let recv_audio = Arc::clone(&recv);
thread::spawn(move || {
    while let Ok(Some(frame)) = recv_audio.capture_audio(5000) {
        // Process audio frame
    }
});
```

## Upgrading from 0.5.x to 0.6.0

### Breaking Changes

1. **Frame Lifetimes**: Frames are now lifetime-bound to their receiver:
   ```rust
   // Before: frames could outlive receiver (unsafe!)
   let frame: VideoFrame<'static> = recv.capture(timeout)?;
   
   // After: frames tied to receiver lifetime (safe!)
   let frame: VideoFrame<'_> = recv.capture(timeout)?;
   ```

2. **Async Send API**: Now accepts `VideoFrameBorrowed` for zero-copy:
   ```rust
   // Before: accepts reference
   send.send_video_async(&frame)
   
   // After: accepts borrowed frame (can be created from &VideoFrame)
   send.send_video_async((&frame).into())
   ```

## Upgrading from 0.4.x to 0.5.0

Version 0.5.0 includes significant improvements for memory safety, ergonomics, and API consistency. While these are breaking changes, the migration is straightforward.

### 🔧 Required Changes

#### 1. Frame Data Access (Zero-Copy Support)
**Before (0.4.x):**
```rust
let frame: VideoFrame = /* ... */;
let data: &Vec<u8> = &frame.data;
```

**After (0.5.0):**
```rust
let frame: VideoFrame = /* ... */;
let data: &[u8] = &frame.data; // Now Cow<[u8]> - works with both owned and borrowed data
```

#### 2. Receiver Creation (New Builder Pattern)
**Before (0.4.x):**
```rust
let receiver = Receiver::new(
    source,
    RecvColorFormat::RGBX_RGBA,
    RecvBandwidth::Highest,
    false,
    Some("My Receiver".to_string()),
);
let mut ndi_recv = Recv::new(&ndi, receiver)?;
```

**After (0.5.0):**
```rust
let mut ndi_recv = Receiver::builder(source)
    .color(RecvColorFormat::RGBX_RGBA)
    .bandwidth(RecvBandwidth::Highest)
    .allow_video_fields(false)
    .name("My Receiver")
    .build(&ndi)?;
```

#### 3. NDI Initialization (Singleton Pattern)
**Before (0.4.x):**
```rust
let ndi = NDI::new()?; // Could panic or fail inconsistently
```

**After (0.5.0):**
```rust
let ndi = NDI::new()?; // Safe singleton pattern - multiple calls return the same instance
// OR use the more explicit:
let ndi = NDI::acquire()?;
```

#### 4. Error Handling (IO Errors)
**Before (0.4.x):**
```rust
let file = File::create(path)
    .map_err(|e| Error::InitializationFailed(format!("Failed: {}", e)))?;
```

**After (0.5.0):**
```rust
let file = File::create(path)?; // IO errors now bubble up automatically
```

#### 5. MetadataFrame (Owned Data)
**Before (0.4.x):**
```rust
// MetadataFrame held raw pointers - unsafe!
let metadata = MetadataFrame { /* raw pointer fields */ };
```

**After (0.5.0):**
```rust
// MetadataFrame now owns its data - safe!
let metadata = MetadataFrame::with_data("<metadata>content</metadata>".to_string(), timecode);
```

#### 6. Runtime Initialization Reset
Version 0.5.0 now allows the NDI runtime to be safely torn down and re-initialized. The global singleton flag resets when the last `NDI` handle is dropped.
```rust
let ndi1 = NDI::new()?;
// ... use ndi1
drop(ndi1);                   // destroys runtime
assert!(!NDI::is_running());  // runtime no longer active

let ndi2 = NDI::acquire()?;   // re-initializes runtime
assert!(NDI::is_running());   // runtime active again
```

#### 7. Async Send API
The async send API has been redesigned with a safer token-based approach to prevent use-after-free errors:
**Before (0.4.x):**
```rust
unsafe { send.send_video_async(&frame); }
// Developer must manually ensure frame outlives the send operation
```

**After (0.5.0):**
```rust
// Safe token-based API - frame remains valid while token exists
let _token = send.send_video_async(&frame);
// Frame is automatically protected from being dropped

// Also available for audio
let _audio_token = send.send_audio_async(&audio_frame);
```

#### 8. Frame Creation Error Handling
**Before (0.4.x):**
```rust
let video_frame = VideoFrame::from_raw(raw_frame);
let audio_frame = AudioFrame::from_raw(raw_frame);
```

**After (0.5.0):**
```rust
let video_frame = VideoFrame::from_raw(raw_frame)?; // Now returns Result
let audio_frame = AudioFrame::from_raw(raw_frame)?; // Now returns Result
```

#### 9. Metadata Send Methods Return Results
**Before (0.4.x):**
```rust
send.send_metadata(&metadata);
send.add_connection_metadata(&metadata);
```

**After (0.5.0):**
```rust
send.send_metadata(&metadata)?; // Now returns Result<(), Error>
send.add_connection_metadata(&metadata)?; // Now returns Result<(), Error>
```

#### 10. Removed APIs
- `Send::free_metadata()` method has been removed (no longer needed with owned data)
- `VideoFrame::from_raw_borrowed` constructor is now `pub(crate)` to prevent accidental misuse

#### 11. FrameType Lifetime Parameter
**Before (0.4.x):**
```rust
let frame_type: FrameType = recv.capture(timeout);
```

**After (0.5.0):**
```rust
let frame_type: FrameType<'_> = recv.capture(timeout); // Now has lifetime parameter
```

#### 12. Source Address Changes
**Before (0.4.x):**
```rust
let source = Source {
    name: "My Source".to_string(),
    url_address: Some("ndi://192.168.1.100:5960".to_string()),
    ip_address: Some("192.168.1.100".to_string()),
};
```

**After (0.5.0):**
```rust
let source = Source {
    name: "My Source".to_string(),
    address: SourceAddress::Url("ndi://192.168.1.100:5960".to_string()),
    // OR
    // address: SourceAddress::Ip("192.168.1.100".to_string()),
    // address: SourceAddress::None,
};
```

#### 13. PTZ Methods Return Results
**Before (0.4.x):**
```rust
if recv.ptz_recall_preset(3, 1.0) {
    println!("Preset recalled");
}
```

**After (0.5.0):**
```rust
if let Err(e) = recv.ptz_recall_preset(3, 1.0) {
    eprintln!("Failed to recall preset: {}", e);
}
// All PTZ methods now return Result<(), Error>
```

#### 14. Async Send API with Tokens
**Before (0.4.x):**
```rust
unsafe { send.send_video_async(&frame); }
// Must manually ensure frame outlives send
```

**After (0.5.0):**
```rust
// Safe token-based API
let _token = send.send_video_async(&frame);
// Frame automatically remains valid while token exists

// Also available for audio
let _audio_token = send.send_audio_async(&audio_frame);
```

### ✨ New Features in 0.5.0

- **Thread Safety**: `Recv`, `Send`, and `Find` are now `Send + Sync`
- **Zero-Copy Access**: Frame data uses `Cow<[u8]>` for optional zero-copy processing
- **Builder Patterns**: Ergonomic `.builder()` API for complex structures
- **Memory Safety**: Eliminated all use-after-free and double-free vulnerabilities
- **Better Error Handling**: Automatic IO error bubbling with `thiserror`
- **FFI Safety**: All FFI structs use `#[repr(C)]` for guaranteed layout
- **New Error Types**: Added `Error::InvalidFrame` and `Error::PtzCommandFailed` for better error handling
- **Default Implementations**: `RecvColorFormat` and `RecvBandwidth` now have sensible defaults
- **Safer Async Send**: New token-based API for async send operations prevents use-after-free
- **Improved Source Management**: New `SourceAddress` enum provides type-safe address handling
- **Better PTZ Error Handling**: All PTZ methods now return `Result` for proper error propagation
- **Robust Initialization**: Improved NDI runtime initialization with better failure tracking

### 🔍 Migration Checklist for 0.6.0

- [ ] Update `Cargo.toml` to version `0.6.0`
- [ ] Replace `capture(&mut self)` with type-specific methods for concurrent access
- [ ] Update async send calls to use `VideoFrameBorrowed` for zero-copy
- [ ] Add lifetime annotations to frame types where stored
- [ ] Test concurrent capture if using multiple threads

### 🔍 Migration Checklist for 0.5.0

- [ ] Update `Cargo.toml` to version `0.5.0`
- [ ] Replace `Receiver::new()` calls with `Receiver::builder()` pattern
- [ ] Update frame data access to work with `&[u8]` instead of `&Vec<u8>`
- [ ] Add `?` to `VideoFrame::from_raw()` and `AudioFrame::from_raw()` calls
- [ ] Add `?` to `send_metadata()` and `add_connection_metadata()` calls
- [ ] Remove any calls to `Send::free_metadata()` (no longer needed)
- [ ] Update `FrameType` usage to include lifetime parameter
- [ ] Remove manual IO error wrapping (use `?` operator instead)
- [ ] Update `Source` structs to use new `SourceAddress` enum instead of separate `url_address`/`ip_address` fields
- [ ] Add error handling for PTZ methods (they now return `Result` instead of `bool`)
- [ ] Update async send calls to use the new token-based API if safety is desired
- [ ] Test thread safety improvements if using across threads
- [ ] Verify that memory-intensive operations now use less memory (zero-copy)

Most code will continue to work with minimal changes due to Rust's automatic dereferencing and the backward-compatible nature of `Cow<[u8]>`.

## Usage

See our blog article on [how to use the NDI SDK with Rust](https://blog.grafton.ai/configuration-management-for-rust-applications-15b2a0346b80).

## Requirements

This library has been developed and tested on Windows 10, but it should work on other platforms easily enough (please contribute!). You need to have the [NDI 6 SDK](https://ndi.video/for-developers/ndi-sdk/) installed for your platform. After installation, make sure your library path (or system PATH on Windows) includes the NDI library binaries location, (e.g., `%NDI_SDK_DIR%\Bin\x64` for Windows PATH).

You also need to install Rust bindgen [according to the instructions here](https://rust-lang.github.io/rust-bindgen/requirements.html).

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
grafton-ndi = "*"
```

Ensure that you have set up the environment variables correctly for your NDI SDK installation.

## Examples

Examples inspired by the official NDI 6 SDK examples can be found in the `examples` directory. To run them, you will need to have the NDI SDK installed and in your PATH.

To run an example, use the following command:

```sh
cargo run --example NDIlib_Find
cargo run --example concurrent_capture
cargo run --example zero_copy_send
```

### Async Send Example
Demonstrates the safe token-based asynchronous send API. The token ensures the frame remains valid while NDI is using it.
```rust,no_run
use grafton_ndi::{NDI, Sender, Send, VideoFrame};

fn main() -> Result<(), grafton_ndi::Error> {
    // Initialize NDI runtime
    let ndi = NDI::new()?;
    // Create sender settings
    let settings = Sender {
        name: "MySend".into(),
        groups: None,
        clock_video: true,
        clock_audio: true,
    };
    let send = Send::new(&ndi, settings)?;
    // Obtain or generate a video frame
    let frame: VideoFrame = get_frame();
    // Safe async send: token keeps frame alive
    let _token = send.send_video_async(&frame);
    // Frame remains valid while token exists
    // Token is automatically dropped when no longer needed
    Ok(())
}
```  

## Contributing

Contributions are welcome! Please submit a pull request or open an issue to discuss what you would like to change.

## License

This project is licensed under the Apache License, Version 2.0. See the [LICENSE](LICENSE) file for more details.
