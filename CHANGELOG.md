# Changelog

## [Unreleased]

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