# grafton-ndi

Unofficial idiomatic Rust bindings for the [NDI 6 SDK](https://ndi.video/for-developers/ndi-sdk/).

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

### ✨ New Features in 0.5.0

- **Thread Safety**: `Recv`, `Send`, and `Find` are now `Send + Sync`
- **Zero-Copy Access**: Frame data uses `Cow<[u8]>` for optional zero-copy processing
- **Builder Patterns**: Ergonomic `.builder()` API for complex structures
- **Memory Safety**: Eliminated all use-after-free and double-free vulnerabilities
- **Better Error Handling**: Automatic IO error bubbling with `thiserror`
- **FFI Safety**: All FFI structs use `#[repr(C)]` for guaranteed layout

### 🔍 Migration Checklist

- [ ] Update `Cargo.toml` to version `0.5.0`
- [ ] Replace `Receiver::new()` calls with `Receiver::builder()` pattern
- [ ] Update frame data access to work with `&[u8]` instead of `&Vec<u8>`
- [ ] Remove manual IO error wrapping (use `?` operator instead)
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
```

## Contributing

Contributions are welcome! Please submit a pull request or open an issue to discuss what you would like to change.

## License

This project is licensed under the Apache License, Version 2.0. See the [LICENSE](LICENSE) file for more details.
