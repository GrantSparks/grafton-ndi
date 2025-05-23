# Changelog

## [0.7.0] - 2025-05-23

### Breaking Changes
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

### Changed
- `AudioFrame` internal storage changed from `Cow<'rx, [u8]>` to `Cow<'rx, [f32]>`
- Audio frame building now properly initializes with float samples
- Default `channel_stride_in_bytes` is now 0 (indicating interleaved format)

### Fixed
- Audio data is now correctly interpreted as 32-bit floats instead of raw bytes
- Channel stride calculation for planar audio formats

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