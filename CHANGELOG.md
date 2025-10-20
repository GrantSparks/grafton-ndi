# Changelog

## [0.9.0] - (Unreleased)

### Major Features

This release dramatically improves ergonomics and reduces boilerplate for common NDI workflows. Based on production usage feedback (issue #11), we've eliminated ~240 lines of repetitive code that users were implementing in every application.

### Documentation
- **Clarified `capture_video_blocking` behavior** - Documented why retry logic exists and its performance characteristics
  - The NDI SDK returns immediately (0ms) during initial connection while the stream synchronizes
  - Early returns only occur for the first 2-3 calls after connecting to a source
  - After synchronization (warm-up), the retry loop has **zero overhead** - frames are captured on first attempt
  - Empirical testing with NDI SDK 6.1.1 confirms: 300/300 frames captured with no retries in steady-state
  - First call: ~200-400ms (includes synchronization), subsequent calls: ~3-4ms per frame
  - This method is safe and recommended for continuous capture loops with no performance penalty

### Fixed
- **Audio frame sending now works correctly** ([#10](https://github.com/GrantSparks/grafton-ndi/issues/10))
  - `AudioFrameBuilder` now properly calculates `channel_stride_in_bytes` based on audio layout
  - Default layout changed to **Planar** (matching FLTP format semantics)
  - Previously hardcoded to 0 (interleaved), causing NDI SDK to reject audio samples entirely
  - `NDIlib_Send_Audio` example now functional
- **Async video send API is now sound and prevents use-after-free** ([#16](https://github.com/GrantSparks/grafton-ndi/issues/16))
  - Eliminates FFI use-after-free hazard where buffers could be dropped while NDI still reads them
  - `AsyncVideoToken` now holds real borrows of the buffer (not `PhantomData`)
  - Compile-time enforcement prevents buffer from being dropped while token exists
  - Single-flight semantics enforced: only one async send can be in-flight at a time

### Added
- **`AudioLayout` enum**: Explicit control over audio data layout
  - `AudioLayout::Planar` - All samples for channel 0, then channel 1, etc. (new default)
  - `AudioLayout::Interleaved` - Samples from all channels interleaved
- **`AudioFrameBuilder::layout()`**: Method to specify planar or interleaved audio format
- Comprehensive documentation with memory layout diagrams for both formats
- Test coverage for both planar and interleaved audio formats (4 new tests)

### Changed
- **BREAKING**: `send_video_async` now requires `&mut self` and enforces single-flight semantics ([#16](https://github.com/GrantSparks/grafton-ndi/issues/16))
  - API signature changed from `&self` to `&mut self` to prevent multiple concurrent async sends
  - `AsyncVideoToken` structure redesigned: holds `&'a Arc<Inner>` and `&'buf [u8]` (real borrows)
  - Removed `in_flight: AtomicUsize` counter - no longer needed with single-flight enforcement
  - Token drop now always flushes (simplified from conditional logic)
  - Migration: declare `Sender` as `mut` and send frames sequentially
  - Example: `let mut sender = Sender::new(...)` and `let token = sender.send_video_async(&frame)`
  - Breaking but mechanical change that eliminates soundness bugs
- **BREAKING**: `LineStrideOrSize` is now a typed enum instead of a union ([#15](https://github.com/GrantSparks/grafton-ndi/issues/15))
  - Eliminates undefined behavior from reading inactive union fields
  - Two variants: `LineStrideBytes(i32)` for uncompressed formats, `DataSizeBytes(i32)` for compressed
  - Direct field access replaced with pattern matching: `match line_stride_or_size { LineStrideBytes(s) => ... }`
  - All union reads in `VideoFrame::from_raw` and `video_done_cb` now discriminate based on format
  - Added comprehensive test coverage (8 new tests)
- **BREAKING**: `AudioFrameBuilder` now defaults to planar layout
  - `channel_stride_in_bytes` now correctly set to `num_samples * 4` (was 0)
  - Users requiring interleaved format must explicitly call `.layout(AudioLayout::Interleaved)`
  - This fixes completely broken audio sending functionality
  - Planar is the correct default as FLTP means "Float Planar"

### API Stabilization ([#24](https://github.com/GrantSparks/grafton-ndi/issues/24))

Major API cleanup for 1.0 readiness with focus on consistency, type safety, and forward compatibility.

#### **BREAKING**: Duration-based Timeouts (Step 1)
All timeout parameters now use `std::time::Duration` instead of `u32` milliseconds:

**Before:**
```rust
finder.wait_for_sources(5000);  // u32 milliseconds
let sources = finder.get_sources(0)?;
receiver.capture_video_blocking(5000)?;
```

**After:**
```rust
use std::time::Duration;

finder.wait_for_sources(Duration::from_secs(5));
let sources = finder.sources(Duration::ZERO)?;
receiver.capture_video(Duration::from_secs(5))?;
```

- Introduced `MAX_TIMEOUT` constant (~49.7 days, the u32::MAX milliseconds limit)
- Added internal `to_ms_checked()` helper that returns `Error::InvalidConfiguration` on overflow
- No silent truncation or saturation - errors are explicit
- Updated all public APIs: Finder, Receiver, Sender, and async adapters

#### **BREAKING**: Builder Pattern Consistency (Step 2)
Both builders are now infallible with validation moved to constructors:

**Before:**
```rust
// Asymmetric and confusing
let receiver = ReceiverOptions::builder(source).build(&ndi)?;  // Takes NDI, returns Receiver
let options = SenderOptions::builder("Name").build()?;         // Returns Result<Options>
let sender = Sender::new(&ndi, &options)?;
```

**After:**
```rust
// Symmetric and clear
let options = ReceiverOptions::builder(source).build();  // Infallible
let receiver = Receiver::new(&ndi, &options)?;           // Validation here

let options = SenderOptions::builder("Name").build();    // Infallible
let sender = Sender::new(&ndi, &options)?;              // Validation here
```

- `ReceiverOptionsBuilder::build()` and `SenderOptionsBuilder::build()` are now infallible
- All validation errors occur in `::new()` constructors
- Builders are just data containers - no I/O or validation
- Clear separation of concerns

#### **BREAKING**: Simplified Capture API (Step 3)
Reduced from 3 confusing variants to 2 clear variants per frame type:

**Before:**
```rust
// Three variants - which one to use?
receiver.capture_video(timeout)?;                    // Returns Option, may timeout
receiver.capture_video_with_retry(timeout, max)?;    // Returns Option, custom retry
receiver.capture_video_blocking(total_timeout)?;     // Returns Result, blocks until success
```

**After:**
```rust
// Two clear variants
receiver.capture_video(timeout)?;              // Result<VideoFrame> - primary, reliable
receiver.capture_video_timeout(timeout)?;      // Result<Option<VideoFrame>> - polling
```

- **Primary method** `capture_*()`: Blocks and retries internally until success or timeout
- **Polling variant** `capture_*_timeout()`: Returns `Ok(None)` on timeout for manual polling
- Applies to all frame types: video, audio, metadata
- Removed deprecated `Receiver::capture()` composite API
- Async wrappers updated to match (tokio, async-std features)

#### **BREAKING**: Frame Type Renames & Forward Compatibility (Step 4)
Renamed enums for clarity and made them forward-compatible with `#[non_exhaustive]`:

**Type Renames:**
```rust
FourCCVideoType → PixelFormat
FrameFormatType → ScanType
AudioType       → AudioFormat
```

**Field Renames:**
```rust
// VideoFrame and BorrowedVideoFrame
frame.fourcc              → frame.pixel_format
frame.frame_format_type   → frame.scan_type

// AudioFrame
frame.fourcc              → frame.format
```

**Builder Method Updates:**
```rust
// VideoFrameBuilder
.fourcc(...)       → .pixel_format(...)
.format(...)       → .scan_type(...)

// AudioFrameBuilder
.format(...)       // Still .format(), but now sets the .format field
```

**Forward Compatibility:**
- All three enums marked `#[non_exhaustive]`
- Removed `Max` sentinel variants
- Unknown format codes from NDI SDK now return proper errors
- FFI conversions use `try_from().map_err()` instead of `unwrap_or(Max)`
- Match expressions now require wildcard arms: `_ => /* handle unknown */`

#### **BREAKING**: Sender API Polish (Step 6)
Eliminated magic bool/negative integer returns in favor of explicit types:

**1. `get_tally()` → `tally()`**

**Before:**
```rust
let mut tally = Tally::new(false, false);
if sender.get_tally(&mut tally, Duration::from_secs(1))? {
    println!("On program: {}", tally.on_program);
}
```

**After:**
```rust
if let Some(tally) = sender.tally(Duration::from_secs(1))? {
    println!("On program: {}", tally.on_program);
}
```

- No mutable reference parameter
- Returns `Result<Option<Tally>>` - timeout is explicit as `Ok(None)`

**2. `get_no_connections()` → `connection_count()`**

**Before:**
```rust
let count = sender.get_no_connections(Duration::from_secs(1))?;  // i32, negative on error
if count >= 0 {
    println!("Connections: {}", count);
}
```

**After:**
```rust
let count = sender.connection_count(Duration::from_secs(1))?;  // Result<u32>
println!("Connections: {}", count);
```

- Returns `Result<u32>` with proper error instead of negative sentinel
- Type system enforces error handling

**3. `get_source_name()` → `source()`**

**Before:**
```rust
let source = sender.get_source_name()?;
```

**After:**
```rust
let source = sender.source()?;
```

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

- Makes async video completion explicit instead of relying solely on `Drop`

#### **BREAKING**: Single Runtime Constructor (Step 7)
Removed `NDI::acquire()` from public API:

**Before:**
```rust
let ndi = NDI::acquire()?;  // Public method
let ndi = NDI::new()?;      // Just forwarded to acquire()
```

**After:**
```rust
let ndi = NDI::new()?;      // Single entry point
// NDI::acquire() is now private
```

- Single obvious way to initialize NDI
- Clearer API with one constructor
- Internal implementation details hidden

#### **BREAKING**: Receiver Status Changes
Receiver status polling now uses `Duration`:

**Before:**
```rust
receiver.poll_status_change(1000)  // u32 milliseconds
```

**After:**
```rust
receiver.poll_status_change(Duration::from_secs(1))
```

### Migration Summary

Quick reference for common patterns:

| Old API | New API |
|---------|---------|
| `finder.get_sources(5000)` | `finder.sources(Duration::from_secs(5))` |
| `finder.get_current_sources()` | `finder.current_sources()` |
| `finder.wait_for_sources(1000)` | `finder.wait_for_sources(Duration::from_secs(1))` |
| `receiver.capture_video_blocking(5000)` | `receiver.capture_video(Duration::from_secs(5))` |
| `receiver.capture_video(100)` (polling) | `receiver.capture_video_timeout(Duration::from_millis(100))` |
| `FourCCVideoType::BGRA` | `PixelFormat::BGRA` |
| `FrameFormatType::Progressive` | `ScanType::Progressive` |
| `frame.fourcc` | `frame.pixel_format` |
| `frame.frame_format_type` | `frame.scan_type` |
| `ReceiverOptions::builder(src).build(&ndi)` | `ReceiverOptions::builder(src).build()` + `Receiver::new(&ndi, &opts)` |
| `sender.get_no_connections(1000)` | `sender.connection_count(Duration::from_secs(1))` |
| `sender.get_tally(&mut t, 1000)` | `sender.tally(Duration::from_secs(1))` |
| `sender.get_source_name()` | `sender.source()` |
| `NDI::acquire()` | `NDI::new()` |

**Important Notes:**
- Matching on `PixelFormat`, `ScanType`, `AudioFormat` now requires wildcard arms due to `#[non_exhaustive]`
- Large `Duration` values (>49.7 days) now error instead of silently saturating
- All builders are infallible - error handling moved to `::new()` constructors

#### 🎯 Source Discovery & Caching
- **`SourceCache`**: Thread-safe caching for NDI instances and discovered sources
  - `new()` - Create a new cache instance
  - `find_by_host(host, timeout_ms)` - Find sources by IP/hostname with automatic caching
  - `invalidate(host)` - Remove specific cache entry when sources go offline
  - `clear()` - Clear all cached sources
  - `len()` / `is_empty()` - Cache introspection helpers
- Eliminates ~150 lines of manual caching code per application
- Handles expensive NDI initialization and discovery internally
- Thread-safe with interior mutability

#### 🔍 Source Matching Helpers
- **`Source` methods**: Simplified source discovery and matching
  - `matches_host(&str)` - Check if source matches hostname/IP
  - `ip_address()` - Extract IP address from source
  - `host()` - Extract hostname/IP without port
- **`SourceAddress` methods**: Network address parsing utilities
  - `contains_host(&str)` - Check if address contains host/IP
  - `port()` - Parse port number from address
- Handles both IP and URL address types intelligently
- Eliminates ~20 lines of verbose pattern matching per lookup

#### 🖼️ Image Encoding Support (Feature: `image-encoding`)
- **`VideoFrame` encoding methods**: One-line image export
  - `encode_png()` - Encode frame as PNG bytes
  - `encode_jpeg(quality)` - Encode frame as JPEG with quality control
  - `encode_data_url(ImageFormat)` - Encode as base64 data URL for HTML/JSON
- **`ImageFormat` enum**: PNG or JPEG(quality) selection
- Automatic color format conversion (BGRA ↔ RGBA)
- Stride validation prevents corrupted images
- Eliminates ~30 lines of encoding logic + 2 dependencies per application
- Optional feature flag keeps core library lean

#### ⏱️ Frame Capture with Retry Logic
- **Reliable frame capture**: Handles NDI SDK timing quirks automatically
  - `capture_video_with_retry(timeout_ms, max_attempts)` - Fine-grained retry control
  - `capture_video_blocking(total_timeout_ms)` - Recommended: blocks until frame or timeout
  - `capture_audio_with_retry()` / `capture_audio_blocking()` - Audio variants
  - `capture_metadata_with_retry()` / `capture_metadata_blocking()` - Metadata variants
- 6 new methods total (2 per frame type)
- 100ms per-attempt timeout with 10ms sleep between retries
- Detailed timeout errors with attempt count and elapsed time
- Eliminates ~40 lines of retry loop code per application
- Fixes common mistake of trusting `capture_video()` timeout behavior

#### 🎛️ Receiver Configuration Presets
- **`ReceiverOptionsBuilder` presets**: Optimized configurations for common use cases
  - `snapshot_preset(source)` - Low bandwidth, RGBA, optimized for AI/image processing
  - `high_quality_preset(source)` - Full resolution, highest bandwidth for production
  - `monitoring_preset(source)` - Metadata-only for tally/status monitoring
- Self-documenting API guides users to optimal settings
- Easy to extend with more presets as patterns emerge

### Added

#### 📡 Source Discovery Enhancement
- **`Finder::get_current_sources()`**: Get immediate snapshot of discovered sources
  - Uses `NDIlib_find_get_current_sources` for instant source list retrieval
  - No additional network discovery unlike `get_sources(timeout)`
  - Available since NDI SDK 6.0
  - Useful for polling current state without blocking

#### 🚀 Async Runtime Integration (Features: `tokio`, `async-std`)
- **`AsyncReceiver`**: Full async/await support for Tokio and async-std runtimes
  - All 9 capture methods (video/audio/metadata × 3 retry variants)
  - Proper `spawn_blocking` usage prevents runtime blocking
  - Arc-based sharing for async contexts
  - Clone support for multi-task usage
- **Feature flags**: `tokio` and `async-std` for optional runtime support
- Eliminates boilerplate `spawn_blocking` wrappers in every async application
- Production-ready with comprehensive documentation and examples

#### 🎯 Enhanced Error Types
- **Specific error variants** for common failure scenarios:
  - `Error::FrameTimeout { attempts, elapsed }` - Detailed frame timeout with retry info
  - `Error::NoSourcesFound { criteria }` - Source discovery failure with search criteria
  - `Error::SourceUnavailable { source_name }` - Source went offline during operation
  - `Error::Disconnected { reason }` - Receiver disconnected with context
- Rich error context enables better debugging and targeted error recovery
- Pattern matching friendly for handling specific failure modes
- Doc examples show proper error handling patterns

#### 📚 Documentation & Testing
- Comprehensive rustdoc examples for all new APIs
- 28 tests passing (up from 13), including:
  - Source cache validation (4 tests)
  - Source matching helpers (3 tests)
  - Receiver presets (4 tests)
  - Retry logic validation (2 tests)
- Real-world usage examples in doc comments
- Feature flag documentation

### Changed
- `png` dependency moved from dev-dependencies to optional dependency
- Added optional dependencies: `base64`, `jpeg-encoder`, `tokio`, `async-std`

### Fixed
- Example `NDIlib_Recv_PNG` simplified using new retry logic (189 → 165 lines)

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

## [0.8.0] - 2025-05-28

### Added
- **Async video sending**: Non-blocking video transmission with completion callbacks
  - `send_video_async()` method returns `AsyncVideoToken` for safe buffer management
  - `on_async_video_done()` callback for buffer reuse notification
  - `flush_async()` and `flush_async_blocking()` for flushing pending frames
- **BorrowedVideoFrame**: Zero-copy frame type for optimal async performance
  - Enables true zero-copy workflows with external buffers
  - Designed specifically for async send operations
- **AsyncVideoToken**: RAII token for safe async frame lifetime management
  - Prevents use-after-free in async operations
  - Automatically manages frame reference counting
- **Receiver Status API**: Monitor connection health and performance
  - `get_status()` returns detailed `RecvStatus` struct
  - Track frame counts, dropped frames, and connection state
  - Monitor metadata, audio, and video frame statistics
- **Advanced SDK Support**: Optional `advanced_sdk` feature flag
  - Enables `NDIlib_send_set_video_async_completion` for true async callbacks
  - Falls back to simulated completion via `Drop` for standard SDK
- New examples:
  - `async_send.rs`: Demonstrates async video sending
  - `concurrent_capture.rs`: Shows multi-threaded capture
  - `status_monitor.rs`: Receiver status monitoring
  - `zero_copy_send.rs`: Zero-copy async transmission

### Changed
- Enhanced Windows compatibility with proper enum conversions
- Improved error messages with more context
- Better documentation with more examples
- CI/CD improvements:
  - Separated runtime-dependent tests from unit tests
  - Added Windows CI support
  - Improved caching and performance

### Fixed
- Enum conversion issues on Windows platforms
- Potential race conditions in async operations
- CI test failures due to missing NDI runtime
- Documentation inconsistencies

## [0.7.0] - 2025-05-23

### Breaking Changes
- **Removed `AsyncAudioToken` and `send_audio_async()`**: Audio send is always synchronous
  - The NDI SDK function `NDIlib_send_send_audio_v3` performs a synchronous copy
  - Migration: Remove any `AsyncAudioToken` usage and use `send_audio()` directly
  - Audio buffers can be reused immediately after `send_audio()` returns
- **Audio data type change**: `AudioFrame::data` now returns `&[f32]` instead of `&[u8]`
  - Audio samples are now properly typed as 32-bit floats
  - This matches the NDI v3 audio format (FLTP - 32-bit float planar)
  - Migration: Update code that accesses audio data to work with `f32` values

### Added
- `AudioFrame::data()` method returns audio samples as `&[f32]`
- `AudioFrame::channel_data(channel)` method extracts samples for a specific channel
  - Handles both interleaved and planar audio formats automatically
  - Returns `Option<Vec<f32>>` with the channel's samples
- `AudioFrameBuilder::data()` now accepts `Vec<f32>` for setting audio samples
- Comprehensive tests for 32-bit float audio handling
- New example: `NDIlib_Recv_Audio` demonstrating float audio capture
- Test demonstrating audio buffer reusability after synchronous send
- **Receiver status monitoring**: New `poll_status_change()` method and `RecvStatus` type
  - Monitor tally state changes (program/preview)
  - Track connection count changes
  - Detect other status changes (latency, PTZ, etc.)
- **Async send completion callback**: New `on_async_video_done()` method
  - Register a callback to be notified when NDI releases async send buffers
  - Enables single-buffer zero-copy workflows
  - Callback receives a mutable slice for buffer reuse
- New example: `status_monitor` demonstrating receiver status monitoring
- Updated example: `zero_copy_send` now uses completion callback instead of double-buffering

### Changed
- `AudioFrame` internal storage changed from `Cow<'rx, [u8]>` to `Cow<'rx, [f32]>`
- Audio frame building now properly initializes with float samples
- Default `channel_stride_in_bytes` is now 0 (indicating interleaved format)
- Improved NDI initialization spin-loop with exponential backoff after ~200 iterations
  - Prevents CPU burn on slow systems or VMs
- `FrameType::StatusChange` now contains a `RecvStatus` struct instead of being empty
- `AsyncVideoToken` is now `#[repr(transparent)]` and explicitly `!Send`

### Fixed
- Audio data is now correctly interpreted as 32-bit floats instead of raw bytes
- Channel stride calculation for planar audio formats
- CPU waste in initialization when NDI takes time to start

## [0.6.0] - 2025-05-23

### Added
- **Lifetime-bound frames**: Video and audio frames are now lifetime-bound to their originating `Recv` instance, preventing use-after-free bugs at compile time
- **Zero-copy async send**: New `VideoFrameBorrowed` type enables true zero-copy async send operations
- **Concurrent capture API**: New thread-safe methods `capture_video()`, `capture_audio()`, and `capture_metadata()` that take `&self` instead of `&mut self`
- Examples for concurrent capture and zero-copy send demonstrating the new APIs

### Changed
- `VideoFrame` and `AudioFrame` now have a lifetime parameter `'rx` tied to their receiver
- `FrameType` enum now has a lifetime parameter `'rx`
- `Send::send_video_async()` now accepts `VideoFrameBorrowed` for zero-copy operation
- `AsyncVideoToken` and `AsyncAudioToken` now have proper lifetime bounds
- Frame structs now include `recv_instance` field and implement `Drop` to properly free NDI resources

### Deprecated
- `Recv::capture(&mut self)` - use the new type-specific capture methods for concurrent access

### Fixed
- Use-after-free vulnerability when frames outlived their `Recv` instance
- Heavy memory copies on every async send operation
- Artificial single-threading limitation on capture operations

## [0.5.0] - Previous release
[Previous changelog content...]
