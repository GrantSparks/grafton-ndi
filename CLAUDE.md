# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

grafton-ndi provides idiomatic Rust bindings for the NDI 6 SDK (Network Device Interface), a protocol for real-time video/audio streaming over IP networks. The library wraps unsafe C FFI calls in safe Rust interfaces.

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

# Run examples (requires NDI SDK installed and in PATH)
cargo run --example NDIlib_Find
cargo run --example NDIlib_Recv_PNG
cargo run --example NDIlib_Recv_PTZ

# Format code (if rustfmt is installed)
cargo fmt

# Run clippy linter (if clippy is installed)
cargo clippy
```

## NDI SDK Setup

The build requires the NDI SDK to be installed:

- **Windows**: Default path is `C:\Program Files\NDI SDK for Windows`
- **Linux**: `/usr/share/NDI SDK for Linux` or `/usr/share/NDI Advanced SDK for Linux`
- **Custom**: Set `NDI_SDK_DIR` environment variable

On Windows, ensure `%NDI_SDK_DIR%\Bin\x64` is in your PATH.

## Architecture

The codebase follows a layered architecture:

1. **build.rs**: Uses bindgen to generate FFI bindings from NDI SDK headers. Handles platform-specific library linking (Processing.NDI.Lib.x64 on Windows, libndi on Linux).

2. **src/ndi_lib.rs**: Auto-generated unsafe FFI bindings (included via `include!` macro).

3. **src/lib.rs**: Safe Rust wrappers around FFI calls. Key types:
   - `NDI`: Main entry point, manages library initialization
   - `Find`/`Finder`: Network discovery for NDI sources
   - `Recv`/`Receiver`: Receive video/audio/metadata
   - `Send`/`Sender`: Transmit as NDI source
   - Frame types: `VideoFrame`, `AudioFrame`, `MetadataFrame`

4. **src/error.rs**: Custom error types using thiserror.

## Key Design Patterns

- **Lifetime Management**: Uses PhantomData to ensure NDI instance outlives dependent objects (Finder, Receiver, Sender)
- **RAII**: All NDI objects implement Drop for automatic cleanup
- **Builder Pattern**: Configuration structs for Find, Recv, Send with defaults
- **Zero-Copy**: Frame data references NDI's internal buffers when possible

## Testing Considerations

- Tests require NDI SDK runtime
- Examples serve as integration tests
- Network tests may require actual NDI sources on the network