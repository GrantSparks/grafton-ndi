# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

grafton-ndi provides high-performance, idiomatic Rust bindings for the NDI® 6 SDK (Network Device Interface), a protocol for real-time video/audio streaming over IP networks. The library wraps unsafe C FFI calls in safe Rust interfaces.

## Current Version

Version 0.8.0 - Async video sending, receiver status monitoring, and improved Windows compatibility.

## Build Commands

```bash
# Build the library
cargo build

# Build with optimizations
cargo build --release

# Run tests
cargo test

# Check code without building
cargo check

# Run examples (requires NDI SDK installed and runtime in PATH)
cargo run --example NDIlib_Find
cargo run --example NDIlib_Recv_PNG
cargo run --example NDIlib_Recv_PTZ
cargo run --example async_send
cargo run --example status_monitor

# Format code (if rustfmt is installed)
cargo fmt

# Run clippy linter (if clippy is installed)
cargo clippy
```

## NDI SDK Setup

The build requires the NDI SDK to be installed:

- **Windows**: Default path is `C:\Program Files\NDI\NDI 6 SDK`
- **Linux**: `/usr/share/NDI SDK for Linux` or `/usr/share/NDI Advanced SDK for Linux`
- **Custom**: Set `NDI_SDK_DIR` environment variable

On Windows, ensure `%NDI_SDK_DIR%\Bin\x64` is in your PATH.

## NDI SDK Versions

There are two variants of the NDI SDK:

### Standard SDK
- Free to download and use
- Includes all core NDI functionality
- Sufficient for most applications
- Functions available: send, receive, discovery, routing, etc.
- Default for this library (no feature flags needed)

### Advanced SDK
- Requires license from NDI
- Includes additional performance features
- Notable additions:
  - `NDIlib_send_set_video_async_completion`: Callback when async video frame can be reused
  - Additional performance monitoring APIs
  - Advanced routing capabilities
- Enable with `advanced_sdk` feature flag in Cargo.toml

### Version History
- **NDI SDK 6**: Current major version, significant performance improvements
- **NDI SDK 6.1.1**: Added async completion callbacks (Advanced SDK only)
- **NDI SDK 5**: Previous stable version, still widely deployed

### Feature Differences in grafton-ndi
When using standard SDK (default):
- Async video completion is simulated via `AsyncVideoToken::drop()`
- All core functionality works correctly
- Slightly less optimal buffer management

When using Advanced SDK (`advanced_sdk` feature):
- True async video completion callbacks from SDK
- Optimal zero-copy buffer management
- Access to additional advanced APIs

## Architecture

The codebase follows a layered architecture:

1. **build.rs**: Uses bindgen to generate FFI bindings from NDI SDK headers. Handles platform-specific library linking (Processing.NDI.Lib.x64 on Windows, libndi on Linux).

2. **src/ndi_lib.rs**: Auto-generated unsafe FFI bindings (included via `include!` macro).

3. **src/lib.rs**: Safe Rust wrappers around FFI calls. Key types:
   - `NDI`: Main entry point, manages library initialization (reference-counted)
   - `Finder`/`FinderOptions`/`FinderOptionsBuilder`: Network discovery for NDI sources
   - `Receiver`/`ReceiverOptions`/`ReceiverOptionsBuilder`: Receive video/audio/metadata
   - `Sender`/`SenderOptions`/`SenderOptionsBuilder`: Transmit as NDI source
   - Frame types: `VideoFrame`, `AudioFrame`, `MetadataFrame`, `BorrowedVideoFrame` (all with builders)
   - Async types: `AsyncVideoToken`, `AsyncCompletionHandler`
   - Status types: `RecvStatus`, `TallyState`
   - Enums: `FourCCVideoType`, `ReceiverColorFormat`, `ReceiverBandwidth`, `FrameType`, etc.

4. **src/error.rs**: Custom error types using thiserror with detailed messages.

## Key Design Patterns

- **Lifetime Management**: Uses PhantomData to ensure NDI instance outlives dependent objects (Finder, Receiver, Sender)
- **RAII**: All NDI objects implement Drop for automatic cleanup
- **Builder Pattern**: Configuration structs for FinderOptions, ReceiverOptions, SenderOptions with defaults
- **Zero-Copy**: Frame data references NDI's internal buffers when possible (especially with BorrowedVideoFrame)
- **Async Safety**: AsyncVideoToken uses Arc<Inner> to prevent use-after-free in async operations
- **Thread Safety**: Arc + Mutex for shared state, all main types are Send + Sync

## Testing Considerations

- Tests require NDI SDK runtime
- Examples serve as integration tests
- Network tests may require actual NDI sources on the network
- Run `cargo test` and `cargo clippy` before commits
- Ensure examples compile with `cargo build --examples`

## Documentation Standards

- All public APIs must have rustdoc comments
- Include examples in documentation where helpful
- Document safety considerations for unsafe code
- Keep README focused on current usage, not migration
- Migration guides go in `docs/migration/`

## API Stability Goals (1.0.0)

- No breaking changes after 1.0.0 release
- Builder patterns allow extending APIs without breaks
- Remove rather than deprecate when possible until 1.0.0
- Use semantic versioning (MAJOR.MINOR.PATCH)