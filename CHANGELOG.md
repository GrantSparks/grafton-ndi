# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.0] - (Unreleased)

### Overview

Version 0.9.0 represents a **major milestone** toward our 1.0 release. This version stabilizes the public API with comprehensive breaking changes designed to improve safety, performance, ergonomics, and forward compatibility. We recommend all users migrate to 0.9.0 as it establishes the API that will be locked in for 1.0.

**Key Improvements:**
- 🎯 **API Stabilization**: Consistent, idiomatic Rust API ready for 1.0
- 🚀 **Zero-Copy Receive**: Eliminates ~475 MB/s of unnecessary memcpy at 1080p@60fps
- 🔒 **Memory Safety**: Eliminated 5+ classes of undefined behavior through the type system
- 🛠️ **Ergonomics**: Simplified capture API, source caching, and image encoding
- ⚡ **Performance**: Multiple performance improvements and zero-cost abstractions

**Migration Effort**: Medium to High. Most changes are mechanical (find/replace), but the API surface has changed significantly. See the [Migration Guide](#migration-guide) below for detailed instructions.

---

## Breaking Changes

### 1. Duration-Based Timeouts (API Stabilization Step 1)

**Why**: Using `std::time::Duration` instead of raw `u32` milliseconds is more idiomatic Rust and prevents accidental bugs from mixing up time units. It also makes overflow explicit instead of silently truncating.

All timeout parameters now use `Duration` instead of `u32` milliseconds:

**Before (0.8.1):**
```rust
// Raw milliseconds everywhere
finder.wait_for_sources(5000);
let sources = finder.get_sources(0)?;
let frame = receiver.capture_video_blocking(5000)?;
sender.get_tally(&mut tally, 1000)?;
```

**After (0.9.0):**
```rust
use std::time::Duration;

// Type-safe Duration values
finder.wait_for_sources(Duration::from_secs(5))?;
let sources = finder.sources(Duration::ZERO)?;
let frame = receiver.capture_video(Duration::from_secs(5))?;
let tally = sender.tally(Duration::from_secs(1))?;
```

**Changes:**
- Added `MAX_TIMEOUT` constant (~49.7 days, the `u32::MAX` milliseconds limit)
- Added internal `to_ms_checked()` helper that returns `Error::InvalidConfiguration` on overflow
- No silent truncation or saturation—errors are explicit
- Updated all public APIs: `Finder`, `Receiver`, `Sender`, and async adapters

**Affected Methods:**
- `Finder::wait_for_sources(timeout: Duration)`
- `Finder::sources(timeout: Duration)`
- `Finder::find_sources(timeout: Duration)`
- `Receiver::capture_video(timeout: Duration)`
- `Receiver::capture_video_timeout(timeout: Duration)`
- `Receiver::capture_audio(timeout: Duration)`
- `Receiver::capture_audio_timeout(timeout: Duration)`
- `Receiver::capture_metadata(timeout: Duration)`
- `Receiver::capture_metadata_timeout(timeout: Duration)`
- `Receiver::poll_status_change(timeout: Duration)`
- `Receiver::capture_video_ref(timeout: Duration)` (new)
- `Receiver::capture_audio_ref(timeout: Duration)` (new)
- `Receiver::capture_metadata_ref(timeout: Duration)` (new)
- `Sender::tally(timeout: Duration)`
- `Sender::connection_count(timeout: Duration)`
- `Sender::flush_async(timeout: Duration)`
- All async wrapper methods

---

### 2. Builder Pattern Consistency (API Stabilization Step 2)

**Why**: Having builders be infallible and moving validation to constructors creates a clear separation of concerns and makes the API more predictable. It also makes both `Receiver` and `Sender` construction symmetric.

Both builders are now infallible, with validation moved to `::new()` constructors:

**Before (0.8.1):**
```rust
// Asymmetric and confusing
let receiver = ReceiverOptions::builder(source)
    .color(ReceiverColorFormat::RGBX_RGBA)
    .build(&ndi)?;  // Takes NDI, returns Receiver directly

let options = SenderOptions::builder("My Sender")
    .clock_video(true)
    .build()?;  // Returns Result<SenderOptions>
let sender = Sender::new(&ndi, &options)?;
```

**After (0.9.0):**
```rust
// Symmetric and clear
let options = ReceiverOptions::builder(source)
    .color(ReceiverColorFormat::RGBX_RGBA)
    .build();  // Infallible
let receiver = Receiver::new(&ndi, &options)?;  // Validation here

let options = SenderOptions::builder("My Sender")
    .clock_video(true)
    .build();  // Infallible
let sender = Sender::new(&ndi, &options)?;  // Validation here
```

**Changes:**
- `ReceiverOptionsBuilder::build()` is now infallible (no longer takes `&NDI`, no longer returns `Result`)
- `SenderOptionsBuilder::build()` is now infallible
- All validation errors occur in `Receiver::new()` and `Sender::new()` constructors
- Builders are just data containers—no I/O or validation
- Clear separation of concerns

---

### 3. Simplified Capture API (API Stabilization Step 3)

**Why**: The previous API had 3 confusing variants per frame type. The new API has 2 clear variants: one for reliable capture (with internal retry), and one for manual polling. This makes the intent clearer and reduces API surface area.

Reduced from 3 confusing variants to 2 clear variants per frame type:

**Before (0.8.1):**
```rust
// Three variants - which one to use?
let frame = receiver.capture_video(timeout)?;  // Returns Option, may timeout early
let frame = receiver.capture_video_with_retry(timeout, max_attempts)?;  // Custom retry
let frame = receiver.capture_video_blocking(total_timeout)?;  // Blocks until success
```

**After (0.9.0):**
```rust
// Two clear variants
let frame = receiver.capture_video(timeout)?;  // Result<VideoFrame> - primary, reliable
let frame = receiver.capture_video_timeout(timeout)?;  // Result<Option<VideoFrame>> - polling
```

**Changes:**
- **Primary method** `capture_*()`: Blocks and retries internally until success or timeout. Returns `Result<Frame>`.
- **Polling variant** `capture_*_timeout()`: Returns `Ok(None)` on timeout for manual polling. Returns `Result<Option<Frame>>`.
- Applies to all frame types: video, audio, metadata
- **Removed**: `capture_*_with_retry()` (replaced by primary `capture_*`)
- **Removed**: `capture_*_blocking()` (replaced by primary `capture_*`)
- **Removed**: Deprecated `Receiver::capture()` composite API
- Async wrappers updated to match

**Rationale**: The primary `capture_*()` method handles NDI SDK timing quirks automatically with internal retry logic. During initial connection, the NDI SDK may return immediately (0ms) while the stream synchronizes. After warm-up, frames are captured on the first attempt with zero retry overhead. Empirical testing with NDI SDK 6.1.1 confirms 300/300 frames captured with no retries in steady-state operation.

---

### 4. Frame Type Renames & Forward Compatibility (API Stabilization Step 4)

**Why**: The new names are clearer and more idiomatic. Making enums `#[non_exhaustive]` allows future NDI SDK versions to add new formats without breaking existing code. Removing `Max` sentinels prevents using invalid placeholder values.

Renamed enums for clarity and made them forward-compatible with `#[non_exhaustive]`:

**Type Renames:**
```rust
FourCCVideoType → PixelFormat
FrameFormatType → ScanType
AudioType       → AudioFormat
```

**Field Renames:**
```rust
// VideoFrame and VideoFrameRef
frame.fourcc              → frame.pixel_format
frame.frame_format_type   → frame.scan_type

// AudioFrame and AudioFrameRef
frame.fourcc              → frame.format

// BorrowedVideoFrame
frame.fourcc              → frame.pixel_format
frame.frame_format_type   → frame.scan_type
```

**Builder Method Updates:**
```rust
// VideoFrameBuilder
.fourcc(...)       → .pixel_format(...)
.format(...)       → .scan_type(...)

// AudioFrameBuilder
// .format() remains but now sets the .format field
```

**Forward Compatibility Changes:**
- All three enums marked `#[non_exhaustive]`
- Removed `Max` sentinel variants from all enums
- Unknown format codes from NDI SDK now return proper errors instead of falling back to `Max`
- FFI conversions use `try_from().map_err()` instead of `unwrap_or(Max)`
- Match expressions now require wildcard arms to handle future formats

**Before (0.8.1):**
```rust
use grafton_ndi::FourCCVideoType;

let format = FourCCVideoType::BGRA;

match frame.fourcc {
    FourCCVideoType::BGRA => { /* ... */ },
    FourCCVideoType::UYVY => { /* ... */ },
    FourCCVideoType::Max => { /* invalid/unknown */ },
}
```

**After (0.9.0):**
```rust
use grafton_ndi::PixelFormat;

let format = PixelFormat::BGRA;

// Must include wildcard arm due to #[non_exhaustive]
match frame.pixel_format {
    PixelFormat::BGRA => { /* ... */ },
    PixelFormat::UYVY => { /* ... */ },
    _ => { /* handle unknown formats from future SDKs */ },
}
```

---

### 5. Finder API Polish (API Stabilization Step 5)

**Why**: Removing `get_` prefixes aligns with Rust API guidelines. The new names are more concise and idiomatic.

Removed `get_` prefixes and added convenience methods:

**Before (0.8.1):**
```rust
finder.wait_for_sources(5000);
let sources = finder.get_sources(0)?;
let current = finder.get_current_sources()?;
```

**After (0.9.0):**
```rust
finder.wait_for_sources(Duration::from_secs(5))?;
let sources = finder.sources(Duration::ZERO)?;
let current = finder.current_sources()?;

// New convenience method
let sources = finder.find_sources(Duration::from_secs(5))?;  // wait + get
```

**Changes:**
- `get_sources(timeout)` → `sources(timeout: Duration)`
- `get_current_sources()` → `current_sources()`
- Added `find_sources(timeout: Duration)`: convenience method that waits then returns current sources

---

### 6. Sender API Polish (API Stabilization Step 6)

**Why**: Eliminates magic bool/negative integer returns in favor of explicit Result types. Makes timeout handling clear with `Option`. Removes `get_` prefixes per Rust API guidelines.

Eliminated magic bool/negative integer returns in favor of explicit types:

**1. `get_tally()` → `tally()`**

**Before (0.8.1):**
```rust
let mut tally = Tally::new(false, false);
if sender.get_tally(&mut tally, 1000)? {
    println!("On program: {}", tally.on_program);
}
```

**After (0.9.0):**
```rust
if let Some(tally) = sender.tally(Duration::from_secs(1))? {
    println!("On program: {}", tally.on_program);
}
```

**Changes:**
- No mutable reference parameter
- Returns `Result<Option<Tally>>` - timeout is explicit as `Ok(None)`

**2. `get_no_connections()` → `connection_count()`**

**Before (0.8.1):**
```rust
let count = sender.get_no_connections(1000)?;  // i32, negative on error
if count >= 0 {
    println!("Connections: {}", count);
}
```

**After (0.9.0):**
```rust
let count = sender.connection_count(Duration::from_secs(1))?;  // Result<u32>
println!("Connections: {}", count);
```

**Changes:**
- Returns `Result<u32>` with proper error instead of negative sentinel
- Type system enforces error handling

**3. `get_source_name()` → `source()`**

**Before (0.8.1):**
```rust
let source = sender.get_source_name()?;
```

**After (0.9.0):**
```rust
let source = sender.source()?;
```

**Changes:**
- Removed `get_` prefix per Rust API guidelines
- Still returns owned `Source` (no lifetime complexity)

**4. Added `AsyncVideoToken` explicit methods**

```rust
/// Explicitly wait for completion (consumes token)
pub fn wait(self) -> Result<()>

/// Check completion status (advanced_sdk only)
#[cfg(feature = "advanced_sdk")]
pub fn is_complete(&self) -> bool
```

**Changes:**
- Makes async video completion explicit instead of relying solely on `Drop`

---

### 7. Single Runtime Constructor (API Stabilization Step 7)

**Why**: Having a single constructor makes the API clearer and removes a redundant public method.

Removed `NDI::acquire()` from public API:

**Before (0.8.1):**
```rust
let ndi = NDI::acquire()?;  // Public method
let ndi = NDI::new()?;      // Just forwarded to acquire()
```

**After (0.9.0):**
```rust
let ndi = NDI::new()?;  // Single entry point
```

**Changes:**
- Single obvious way to initialize NDI
- `NDI::acquire()` is now private
- Clearer API with one constructor

---

### 8. Audio Layout and Sending Fixes

**Why**: Audio sending was completely broken in 0.8.1 because `channel_stride_in_bytes` was hardcoded to 0. The NDI SDK rejected all audio samples. This fix makes audio sending actually work.

Audio frame sending now works correctly with proper layout support:

**Before (0.8.1):**
```rust
// Audio sending was broken - SDK rejected all samples
let frame = AudioFrameBuilder::new()
    .sample_rate(48000)
    .num_channels(2)
    .num_samples(1920)
    .data(samples)
    .build();
// This would fail silently - no audio transmitted
```

**After (0.9.0):**
```rust
use grafton_ndi::AudioLayout;

// Audio sending now works - explicit layout control
let frame = AudioFrameBuilder::new()
    .sample_rate(48000)
    .num_channels(2)
    .num_samples(1920)
    .layout(AudioLayout::Planar)  // Explicit control
    .data(samples)
    .build();
```

**Changes:**
- **Added `AudioLayout` enum**: Explicit control over audio data layout
  - `AudioLayout::Planar` - All samples for channel 0, then channel 1, etc. (new default)
  - `AudioLayout::Interleaved` - Samples from all channels interleaved
- **Added `AudioFrameBuilder::layout()`**: Method to specify planar or interleaved audio format
- **BREAKING**: `AudioFrameBuilder` now defaults to planar layout (matching FLTP format semantics)
  - `channel_stride_in_bytes` now correctly set to `num_samples * 4` for planar (was 0)
  - Users requiring interleaved format must explicitly call `.layout(AudioLayout::Interleaved)`
- Comprehensive documentation with memory layout diagrams for both formats
- Test coverage for both planar and interleaved audio formats

**Migration**: If you need interleaved audio (previous behavior), add `.layout(AudioLayout::Interleaved)` to your builder chain. However, note that planar is the correct default for FLTP format.

---

### 9. Async Video Send Safety

**Why**: The previous async send API had a critical soundness bug where buffers could be dropped while NDI was still reading them, causing use-after-free. The new API enforces buffer lifetime at compile-time.

Async video send API is now sound and prevents use-after-free:

**Before (0.8.1):**
```rust
// Unsound - buffer could be dropped while in-flight
let frame = BorrowedVideoFrame::from_buffer(&buffer, width, height, fourcc, 30, 1);
let token = sender.send_video_async(&frame);
// Buffer could be dropped here while NDI still reads it - UB!
```

**After (0.9.0):**
```rust
// Sound - buffer lifetime tied to token
let frame = BorrowedVideoFrame::from_buffer(&buffer, width, height, pixel_format, 30, 1);
let token = sender.send_video_async(&frame);
// Buffer cannot be dropped while token exists - compile error!
// Token must be held or explicitly waited on
drop(token);  // or: token.wait()?;
```

**Changes:**
- **BREAKING**: `send_video_async` now requires `&mut self` and enforces single-flight semantics
  - API signature changed from `&self` to `&mut self` to prevent multiple concurrent async sends
  - `AsyncVideoToken` structure redesigned: holds `&'a Arc<Inner>` and `&'buf [u8]` (real borrows)
  - Removed `in_flight: AtomicUsize` counter - no longer needed with single-flight enforcement
  - Token drop now always flushes (simplified from conditional logic)
- Migration: declare `Sender` as `mut` and send frames sequentially
- Example: `let mut sender = Sender::new(...)` and `let token = sender.send_video_async(&frame)`
- Breaking but mechanical change that eliminates soundness bugs

---

### 10. Video Frame Stride/Size Union Elimination

**Why**: Reading inactive union fields is undefined behavior in Rust. The typed enum approach is sound and makes the discriminant explicit based on the format.

`LineStrideOrSize` is now a typed enum instead of a union:

**Before (0.8.1):**
```rust
// Union with implicit discriminant based on format
let stride = frame.line_stride_or_size.line_stride_in_bytes;  // Maybe UB!
```

**After (0.9.0):**
```rust
// Typed enum with pattern matching
match frame.line_stride_or_size {
    LineStrideOrSize::LineStrideBytes(stride) => {
        println!("Stride: {} bytes", stride);
    },
    LineStrideOrSize::DataSizeBytes(size) => {
        println!("Data size: {} bytes", size);
    },
}
```

**Changes:**
- **BREAKING**: `LineStrideOrSize` is now a typed enum instead of a union
  - Eliminates undefined behavior from reading inactive union fields
  - Two variants: `LineStrideBytes(i32)` for uncompressed formats, `DataSizeBytes(i32)` for compressed
  - Direct field access replaced with pattern matching
  - All union reads in `VideoFrame::from_raw` and `video_done_cb` now discriminate based on format
- Added comprehensive test coverage

---

## Zero-Copy Performance Improvements

### Zero-Copy Receive Path (New in 0.9.0)

**Why**: The previous receive API copied every frame into a `Vec`, incurring massive CPU and memory bandwidth costs. For 1920×1080 BGRA at 60fps, this was ~475 MB/s of unnecessary memcpy. The new borrowed frame types eliminate this overhead.

Added zero-copy borrowed receive frames that eliminate per-frame memory copies:

**Performance Impact:**
- At 1920×1080 BGRA (4 bytes/pixel), one frame is **8.3 MB**
- At 60 fps, the old API performed **~475 MB/s** of unnecessary memcpy
- New API: **0 bytes copied** - direct reference to NDI SDK buffers

**New Types:**
- **`VideoFrameRef<'rx>`** - Zero-copy borrowed video frame
  - Wraps `RecvVideoGuard` internally with RAII cleanup
  - `data()` returns `&[u8]` directly from NDI SDK buffer (no memcpy)
  - Provides accessor methods for all frame properties
  - Lifetime-bound to `Receiver` to prevent use-after-free
  - `to_owned()` converts to owned `VideoFrame` when needed

- **`AudioFrameRef<'rx>`** - Zero-copy borrowed audio frame
  - Wraps `RecvAudioGuard` internally
  - `data()` returns `&[f32]` directly from NDI SDK buffer
  - Similar accessor pattern to `VideoFrameRef`

- **`MetadataFrameRef<'rx>`** - Zero-copy borrowed metadata frame
  - Wraps `RecvMetadataGuard` internally
  - `data()` returns `&CStr` directly from NDI SDK buffer

**New Receiver Methods:**
```rust
// Zero-copy capture (fastest)
let frame_ref = receiver.capture_video_ref(timeout)?;
let data: &[u8] = frame_ref.data();  // No copy!

// Convert to owned if needed
let owned_frame = frame_ref.to_owned()?;
```

**All three ref types:**
- Are **not `Send`** (inherit from guards) - prevents cross-thread use of FFI buffers
- Implement RAII - exactly one `NDIlib_recv_free_*` per frame via `Drop`
- Are lifetime-bound to `Receiver<'rx>` to prevent use-after-free at compile time

**Existing `capture_*` methods now delegate to `capture_*_ref` internally:**
The existing owned capture methods now use the zero-copy path internally and only copy when creating the owned frame. This centralizes the frame capture logic and ensures correctness.

**Migration**: For maximum performance, use `capture_*_ref()` methods. The existing `capture_*()` methods still work but perform a copy.

---

## Memory Safety & Correctness Improvements

### 1. Frame Reference Lifetime Bounds (New in 0.9.0)

**Why**: Without lifetime bounds, `*FrameRef` types could outlive their `Receiver`, causing use-after-free when their `Drop` implementations tried to free NDI resources. The lifetime bounds enforce correct ordering at compile-time with zero runtime cost.

`VideoFrameRef`, `AudioFrameRef`, and `MetadataFrameRef` are now lifetime-bound to `Receiver`:

**What This Prevents:**
```rust
let frame_ref = {
    let receiver = Receiver::new(&ndi, &options)?;
    receiver.capture_video_ref(Duration::from_secs(1))?  // ERROR: won't compile!
};
// Without lifetimes, receiver drops here while frame_ref still holds NDI buffer
// This would be use-after-free - now caught at compile time
```

**Changes:**
- All `*FrameRef` types are lifetime-parameterized: `VideoFrameRef<'rx>`, etc.
- Internal guards also lifetime-parameterized: `RecvVideoGuard<'rx>`, etc.
- Capture methods bind the lifetime: `capture_video_ref<'rx>(&'rx self, ...) -> Result<VideoFrameRef<'rx>>`
- **Compiler enforces**: Cannot drop `Receiver` while any `*FrameRef` is alive
- **Zero runtime cost**: Only uses `PhantomData<&'rx Receiver>`

This eliminates an entire class of use-after-free bugs at compile-time with no performance penalty.

---

### 2. Non-Null FFI Source Pointers

**Why**: The NDI SDK can return NULL pointers for source names and URLs. Previously, we didn't check for these, leading to potential crashes when dereferencing.

All FFI source pointers are now validated as non-null at the boundary:

**Changes:**
- Added `Source::try_from_raw()` that checks all source pointer fields for NULL
- Returns `Error::InvalidFrame` if any required field is NULL
- `Finder::current_sources()` and `Finder::sources()` skip invalid sources gracefully
- Added debug assertions to catch SDK issues in development

This prevents potential crashes from dereferencing NULL pointers passed by the NDI SDK.

---

### 3. Memory-Safe Async Completion Callback

**Why**: The async completion callback had memory leaks and potential use-after-free issues. The NDI SDK can call the callback after we've started destroying the sender, or the callback could fire on a different thread while we're in the middle of cleanup.

Fixed memory leaks and use-after-free in async completion callback:

**Changes:**
- Properly track buffer ownership in callback
- Use atomic flags to detect when `Inner` is being destroyed
- Synchronize callback execution with sender destruction
- Callback no longer fires on already-destroyed instances
- All tests pass, including stress tests with concurrent async sends

This eliminates memory leaks and potential crashes in async video sending.

---

## New Features

### 1. Source Caching (`SourceCache`)

**Why**: NDI source discovery is expensive (network I/O, SDK initialization). Users were implementing ~150 lines of manual caching code in every application. `SourceCache` eliminates this boilerplate.

Thread-safe caching for NDI instances and discovered sources:

```rust
use grafton_ndi::SourceCache;

let cache = SourceCache::new();

// Find sources by host with automatic caching
let source = cache.find_by_host("192.168.1.100", Duration::from_secs(5))?;

// Invalidate cache when sources go offline
cache.invalidate("192.168.1.100");

// Clear all cached sources
cache.clear();
```

**Features:**
- `new()` - Create a new cache instance
- `find_by_host(host, timeout)` - Find sources by IP/hostname with automatic caching
- `invalidate(host)` - Remove specific cache entry when sources go offline
- `clear()` - Clear all cached sources
- `len()` / `is_empty()` - Cache introspection helpers

**Benefits:**
- Eliminates ~150 lines of manual caching code per application
- Handles expensive NDI initialization and discovery internally
- Thread-safe with interior mutability

---

### 2. Image Encoding Support (Optional `image-encoding` Feature)

**Why**: Encoding video frames to PNG/JPEG is a common requirement. Users were implementing ~30 lines of encoding logic plus adding 2 dependencies to every application. This feature makes it one line of code.

One-line image export for video frames (requires `image-encoding` feature):

```rust
use grafton_ndi::ImageFormat;

// Encode as PNG
let png_bytes = frame.encode_png()?;

// Encode as JPEG with quality control
let jpeg_bytes = frame.encode_jpeg(85)?;

// Encode as base64 data URL for HTML/JSON
let data_url = frame.encode_data_url(ImageFormat::Jpeg(85))?;
```

**Features:**
- `encode_png()` - Encode frame as PNG bytes
- `encode_jpeg(quality)` - Encode frame as JPEG with quality control (0-100)
- `encode_data_url(ImageFormat)` - Encode as base64 data URL for HTML/JSON
- Automatic color format conversion (BGRA ↔ RGBA)
- Stride validation prevents corrupted images

**Enable in `Cargo.toml`:**
```toml
[dependencies]
grafton-ndi = { version = "0.9", features = ["image-encoding"] }
```

**Benefits:**
- Eliminates ~30 lines of encoding logic + 2 dependencies per application
- Optional feature flag keeps core library lean

---

### 3. Source Discovery & Matching Helpers

Simplified source discovery and matching:

**Source Methods:**
```rust
// Check if source matches hostname/IP
if source.matches_host("192.168.1.100") { /* ... */ }

// Extract IP address from source
let ip = source.ip_address();

// Extract hostname/IP without port
let host = source.host();
```

**SourceAddress Methods:**
```rust
// Check if address contains host/IP
if address.contains_host("192.168.1.100") { /* ... */ }

// Parse port number from address
let port = address.port();
```

**Benefits:**
- Handles both IP and URL address types intelligently
- Eliminates ~20 lines of verbose pattern matching per lookup

---

### 4. Async Runtime Integration (Optional Features)

**Why**: Users in async applications were writing boilerplate `spawn_blocking` wrappers. These adapters provide native integration with Tokio and async-std.

Native integration with Tokio and async-std runtimes (requires feature flags):

```rust
// Tokio
use grafton_ndi::tokio::AsyncReceiver;

let async_receiver = AsyncReceiver::new(receiver);
let frame = async_receiver.capture_video(Duration::from_secs(5)).await?;
```

```rust
// async-std
use grafton_ndi::async_std::AsyncReceiver;

let async_receiver = AsyncReceiver::new(receiver);
let frame = async_receiver.capture_video(Duration::from_secs(5)).await?;
```

**Features:**
- All 6 capture methods (video/audio/metadata × 2 variants)
- Proper `spawn_blocking` usage prevents runtime blocking
- Arc-based sharing for async contexts
- Clone support for multi-task usage

**Enable in `Cargo.toml`:**
```toml
[dependencies]
grafton-ndi = { version = "0.9", features = ["tokio"] }
# or
grafton-ndi = { version = "0.9", features = ["async-std"] }
```

**Benefits:**
- Eliminates boilerplate `spawn_blocking` wrappers
- Production-ready with comprehensive documentation

---

### 5. Enhanced Error Types

Specific error variants for common failure scenarios:

```rust
// Detailed frame timeout with retry info
Error::FrameTimeout { attempts, elapsed }

// Source discovery failure with search criteria
Error::NoSourcesFound { criteria }

// Source went offline during operation
Error::SourceUnavailable { source_name }

// Receiver disconnected with context
Error::Disconnected { reason }

// Invalid configuration (e.g., timeout overflow)
Error::InvalidConfiguration(String)
```

**Benefits:**
- Rich error context enables better debugging
- Pattern matching friendly for handling specific failure modes
- Doc examples show proper error handling patterns

---

### 6. Receiver Connection Statistics

Monitor receiver connection state and performance:

```rust
if let Some(status) = receiver.poll_status_change(Duration::from_millis(100))? {
    println!("Connected: {}", status.is_connected);

    // Connection statistics
    if let Some(stats) = &status.connection {
        println!("Video frames: {}", stats.video_frames);
        println!("Audio frames: {}", stats.audio_frames);
        println!("Metadata frames: {}", stats.metadata_frames);
    }

    // Tally state
    if let Some(tally) = &status.tally {
        println!("On program: {}", tally.on_program);
        println!("On preview: {}", tally.on_preview);
    }

    // Connection count
    if let Some(connections) = status.connections {
        println!("Active connections: {}", connections);
    }
}
```

**Benefits:**
- Monitor tally state changes (program/preview)
- Track connection count changes
- Detect other status changes (latency, PTZ, etc.)

---

## Bug Fixes

### Audio Sending Now Works ([#10](https://github.com/GrantSparks/grafton-ndi/issues/10))

- `AudioFrameBuilder` now properly calculates `channel_stride_in_bytes` based on audio layout
- Default layout changed to **Planar** (matching FLTP format semantics)
- Previously hardcoded to 0 (interleaved), causing NDI SDK to reject audio samples entirely
- `NDIlib_Send_Audio` example now functional

### Platform & Build Fixes

- Fixed architecture detection for Linux NDI SDK library paths
- Improved error messages and configuration validation
- Fixed clippy warnings and applied formatting improvements
- Code quality improvements and modernization

---

## Migration Guide

### Quick Reference Table

| 0.8.1 API | 0.9.0 API | Notes |
|-----------|-----------|-------|
| `finder.wait_for_sources(5000)` | `finder.wait_for_sources(Duration::from_secs(5))?` | Now returns `Result` |
| `finder.get_sources(0)` | `finder.sources(Duration::ZERO)?` | Renamed, Duration param |
| `finder.get_current_sources()` | `finder.current_sources()?` | Removed `get_` |
| `receiver.capture_video_blocking(5000)` | `receiver.capture_video(Duration::from_secs(5))?` | Primary capture method |
| `receiver.capture_video(100)` (polling) | `receiver.capture_video_timeout(Duration::from_millis(100))?` | Explicit polling |
| `receiver.capture_video_with_retry(...)` | `receiver.capture_video(...)` | Use primary method |
| `FourCCVideoType::BGRA` | `PixelFormat::BGRA` | Type renamed |
| `FrameFormatType::Progressive` | `ScanType::Progressive` | Type renamed |
| `AudioType::FLTP` | `AudioFormat::FloatPlanar` | Type renamed |
| `frame.fourcc` | `frame.pixel_format` | Field renamed |
| `frame.frame_format_type` | `frame.scan_type` | Field renamed |
| `builder.fourcc(...)` | `builder.pixel_format(...)` | Builder method |
| `builder.format(...)` (video) | `builder.scan_type(...)` | Builder method |
| `ReceiverOptions::builder(src).build(&ndi)?` | `ReceiverOptions::builder(src).build()` + `Receiver::new(&ndi, &opts)?` | Infallible builder |
| `SenderOptions::builder("Name").build()?` | `SenderOptions::builder("Name").build()` + `Sender::new(&ndi, &opts)?` | Infallible builder |
| `sender.get_no_connections(1000)` | `sender.connection_count(Duration::from_secs(1))?` | Returns `Result<u32>` |
| `sender.get_tally(&mut t, 1000)` | `sender.tally(Duration::from_secs(1))?` | Returns `Result<Option<Tally>>` |
| `sender.get_source_name()` | `sender.source()?` | Removed `get_` |
| `NDI::acquire()` | `NDI::new()?` | Single constructor |
| `let sender = Sender::new(...)` | `let mut sender = Sender::new(...)` | Mut required for async send |

### Search & Replace Patterns

These patterns can help automate parts of the migration:

```regex
# Basic renames (review carefully!)
get_sources\( → sources(
get_current_sources\( → current_sources(
get_source_name\( → source(
get_tally\( → tally(
get_no_connections\( → connection_count(

# Type renames
FourCCVideoType → PixelFormat
FrameFormatType → ScanType
AudioType → AudioFormat

# Field renames
\.fourcc\b → .pixel_format
\.frame_format_type\b → .scan_type

# Capture method renames
capture_video_blocking → capture_video
capture_audio_blocking → capture_audio
capture_metadata_blocking → capture_metadata
```

**⚠️ Manual Review Required:**
- Timeout parameters: Convert `u32` to `Duration`
- Builder usage: Separate `.build()` from `::new()`
- Capture polling: Rename to `*_timeout()` and handle `Option`
- Enum matching: Add wildcard `_ =>` arms
- Async send: Add `mut` to sender declaration

### Detailed Migration Steps

#### 1. Update Timeout Parameters

**Find all numeric timeout literals:**
```rust
// Before
5000
1000
100
```

**Replace with Duration:**
```rust
// After
Duration::from_secs(5)
Duration::from_secs(1)
Duration::from_millis(100)
```

**Common pattern:**
```rust
// Before
let frame = receiver.capture_video_blocking(5000)?;

// After
use std::time::Duration;
let frame = receiver.capture_video(Duration::from_secs(5))?;
```

#### 2. Update Builder Usage

**Before:**
```rust
// Receiver (old API)
let receiver = ReceiverOptions::builder(source)
    .color(ReceiverColorFormat::RGBX_RGBA)
    .build(&ndi)?;

// Sender (old API)
let options = SenderOptions::builder("My Sender")
    .clock_video(true)
    .build()?;
let sender = Sender::new(&ndi, &options)?;
```

**After:**
```rust
// Receiver (new API)
let options = ReceiverOptions::builder(source)
    .color(ReceiverColorFormat::RGBX_RGBA)
    .build();  // No longer takes &ndi or returns Result
let receiver = Receiver::new(&ndi, &options)?;

// Sender (new API) - symmetric
let options = SenderOptions::builder("My Sender")
    .clock_video(true)
    .build();  // No longer returns Result
let sender = Sender::new(&ndi, &options)?;
```

#### 3. Update Capture Methods

**Reliable capture (blocking until success):**
```rust
// Before
let frame = receiver.capture_video_blocking(5000)?;

// After
let frame = receiver.capture_video(Duration::from_secs(5))?;
```

**Polling/manual retry:**
```rust
// Before
let frame = receiver.capture_video(100)?;  // Returns Option

// After
let frame = receiver.capture_video_timeout(Duration::from_millis(100))?;  // Returns Result<Option<...>>
```

**Custom retry (removed):**
```rust
// Before
let frame = receiver.capture_video_with_retry(timeout, max_attempts)?;

// After - use primary method (has built-in retry)
let frame = receiver.capture_video(Duration::from_secs(total_timeout))?;
```

#### 4. Update Type Names

```rust
// Before
use grafton_ndi::{FourCCVideoType, FrameFormatType, AudioType};

// After
use grafton_ndi::{PixelFormat, ScanType, AudioFormat};
```

#### 5. Update Field Names

```rust
// Before
let format = frame.fourcc;
let scan = frame.frame_format_type;

// After
let format = frame.pixel_format;
let scan = frame.scan_type;
```

#### 6. Update Enum Matching (Add Wildcard)

```rust
// Before
match frame.fourcc {
    FourCCVideoType::BGRA => { /* ... */ },
    FourCCVideoType::UYVY => { /* ... */ },
}

// After - must include wildcard for forward compatibility
match frame.pixel_format {
    PixelFormat::BGRA => { /* ... */ },
    PixelFormat::UYVY => { /* ... */ },
    _ => { /* handle unknown future formats */ },
}
```

#### 7. Update Finder Usage

```rust
// Before
finder.wait_for_sources(5000);
let sources = finder.get_sources(0)?;
let current = finder.get_current_sources()?;

// After
finder.wait_for_sources(Duration::from_secs(5))?;
let sources = finder.sources(Duration::ZERO)?;
let current = finder.current_sources()?;

// Or use convenience method
let sources = finder.find_sources(Duration::from_secs(5))?;
```

#### 8. Update Sender Methods

```rust
// Before
let mut tally = Tally::new(false, false);
if sender.get_tally(&mut tally, 1000)? {
    println!("On program: {}", tally.on_program);
}

let count = sender.get_no_connections(1000)?;
if count >= 0 {
    println!("Connections: {}", count);
}

let source = sender.get_source_name()?;

// After
if let Some(tally) = sender.tally(Duration::from_secs(1))? {
    println!("On program: {}", tally.on_program);
}

let count = sender.connection_count(Duration::from_secs(1))?;
println!("Connections: {}", count);

let source = sender.source()?;
```

#### 9. Update Async Send Usage

```rust
// Before
let sender = Sender::new(&ndi, &options)?;
let token = sender.send_video_async(&frame);

// After - sender must be mutable
let mut sender = Sender::new(&ndi, &options)?;
let token = sender.send_video_async(&frame);
```

#### 10. Update Audio Building (if using audio)

```rust
// Before - audio sending was broken anyway
let frame = AudioFrameBuilder::new()
    .sample_rate(48000)
    .num_channels(2)
    .num_samples(1920)
    .data(samples)
    .build();

// After - now works, defaults to planar
let frame = AudioFrameBuilder::new()
    .sample_rate(48000)
    .num_channels(2)
    .num_samples(1920)
    .data(samples)
    .build();

// If you need interleaved (old default):
let frame = AudioFrameBuilder::new()
    .sample_rate(48000)
    .num_channels(2)
    .num_samples(1920)
    .layout(AudioLayout::Interleaved)  // Explicit
    .data(samples)
    .build();
```

#### 11. Use Zero-Copy Receive (Optional, for Performance)

For maximum performance, use the new zero-copy `*_ref()` methods:

```rust
// Before - always copies
let frame = receiver.capture_video(Duration::from_secs(5))?;
let data = &frame.data;  // This was copied from NDI buffer

// After - zero-copy
let frame_ref = receiver.capture_video_ref(Duration::from_secs(5))?;
let data = frame_ref.data();  // Direct reference, no copy!

// Convert to owned only if you need to store it
let owned_frame = frame_ref.to_owned()?;
```

**Performance gain**: Eliminates ~8.3 MB memcpy per 1080p BGRA frame (~475 MB/s @ 60fps).

---

### Important Notes

- **Matching on enums now requires wildcard arms** due to `#[non_exhaustive]`:
  ```rust
  match pixel_format {
      PixelFormat::BGRA => { /* ... */ },
      _ => { /* handle unknown */ },  // Required!
  }
  ```

- **Large `Duration` values may now error** instead of silently saturating:
  ```rust
  // This will error
  let timeout = Duration::from_secs(u64::MAX);
  let result = receiver.capture_video(timeout);  // Error: exceeds MAX_TIMEOUT

  // Use MAX_TIMEOUT constant if needed
  use grafton_ndi::MAX_TIMEOUT;
  let result = receiver.capture_video(MAX_TIMEOUT)?;
  ```

- **All builders are infallible** - error handling moved to `::new()` constructors:
  ```rust
  // Builder cannot fail
  let options = ReceiverOptions::builder(source).build();

  // Constructor can fail
  let receiver = Receiver::new(&ndi, &options)?;
  ```

- **Async send requires mutable sender**:
  ```rust
  let mut sender = Sender::new(&ndi, &options)?;  // Note: mut
  let token = sender.send_video_async(&frame);
  ```

---

### Testing Your Migration

After migrating, verify everything works:

```bash
# Format code
cargo fmt

# Check for errors
cargo check --all-targets --all-features

# Run clippy
cargo clippy --all-targets --all-features

# Run tests
cargo test --lib
cargo test --doc

# Build examples
cargo build --examples

# Try running an example
cargo run --example NDIlib_Find
```

---

## Upgrading

Update your `Cargo.toml`:

```toml
[dependencies]
grafton-ndi = "0.9"

# With optional features
grafton-ndi = { version = "0.9", features = ["image-encoding", "tokio"] }
```

Available features:
- `image-encoding` - PNG/JPEG encoding support
- `tokio` - Tokio async runtime integration
- `async-std` - async-std runtime integration
- `advanced_sdk` - NDI Advanced SDK features (requires Advanced SDK)

---

## [0.8.1] - 2025-01-06

### Added
- **Full macOS Support**: Complete CI/CD pipeline for macOS platform
  - Automated NDI SDK installation for macOS in GitHub Actions
  - Support for NDI SDK installed at `/Library/NDI SDK for Apple`
  - Platform-specific library paths (`lib/macOS`)
  - Comprehensive testing on macOS runners

### Fixed
- macOS CI workflow to properly detect and use NDI SDK
- Build script to handle macOS-specific SDK locations
- Library path detection for macOS platform
- Windows-specific deadlocks in async token stress tests
- Windows deadlock in concurrent async flush operations
- Clippy warning about redundant pattern matching
- Windows DLL check test compilation errors
- Code formatting issues

---

## [0.8.0] - 2025-05-28

### Added
- **Async video sending**: Non-blocking video transmission with completion callbacks
- **BorrowedVideoFrame**: Zero-copy frame type for optimal async performance
- **AsyncVideoToken**: RAII token for safe async frame lifetime management
- **Receiver Status API**: Monitor connection health and performance
- **Advanced SDK Support**: Optional `advanced_sdk` feature flag

### Changed
- Enhanced Windows compatibility with proper enum conversions
- Improved error messages with more context
- Better documentation with more examples
- CI/CD improvements

### Fixed
- Enum conversion issues on Windows platforms
- Potential race conditions in async operations
- CI test failures due to missing NDI runtime

---

For older changelog entries, see the Git history.
