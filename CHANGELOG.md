# Changelog

## [Unreleased]

### Fixed
- **Audio frame sending now works correctly** ([#10](https://github.com/GrantSparks/grafton-ndi/issues/10))
  - `AudioFrameBuilder` now properly calculates `channel_stride_in_bytes` based on audio layout
  - Default layout changed to **Planar** (matching FLTP format semantics)
  - Previously hardcoded to 0 (interleaved), causing NDI SDK to reject audio samples entirely
  - `NDIlib_Send_Audio` example now functional

### Added
- **`AudioLayout` enum**: Explicit control over audio data layout
  - `AudioLayout::Planar` - All samples for channel 0, then channel 1, etc. (new default)
  - `AudioLayout::Interleaved` - Samples from all channels interleaved
- **`AudioFrameBuilder::layout()`**: Method to specify planar or interleaved audio format
- Comprehensive documentation with memory layout diagrams for both formats
- Test coverage for both planar and interleaved audio formats (4 new tests)

### Changed
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

## [0.9.0] - 2025-10-19

### Major Features

This release dramatically improves ergonomics and reduces boilerplate for common NDI workflows. Based on production usage feedback (issue #11), we've eliminated ~240 lines of repetitive code that users were implementing in every application.

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