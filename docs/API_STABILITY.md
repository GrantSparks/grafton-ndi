# API Stability Guidelines

This document outlines the API stability guarantees for grafton-ndi, particularly for the upcoming 1.0.0 release.

## Version 1.0.0 Commitments

Once 1.0.0 is released, we commit to:

### No Breaking Changes

- Public API signatures will remain stable
- Types will not be renamed or removed
- Required parameters will not be added to existing functions
- Trait implementations will remain consistent

### Allowed Changes

- Adding new methods to structs (non-breaking)
- Adding new optional builder methods
- Adding new enum variants (with `#[non_exhaustive]`)
- Performance improvements
- Bug fixes that don't change API behavior
- Documentation improvements

### Deprecation Policy

When API changes are necessary:

1. Mark old API as `#[deprecated]` with migration guide
2. Maintain deprecated API for at least 2 minor versions
3. Remove only in next major version (2.0.0)

## Current API Surface (0.7.0 → 1.0.0)

### Core Types (Stable)

- `NDI` - Runtime management
- `Find` / `Finder` / `FinderBuilder` - Source discovery
- `Receiver` / `ReceiverBuilder` - Video/audio reception
- `SendInstance` / `SendOptions` / `SendOptionsBuilder` - Transmission
- `Source` / `SourceAddress` - Source representation

### Frame Types (Stable)

- `VideoFrame` / `VideoFrameBuilder`
- `AudioFrame` / `AudioFrameBuilder`
- `MetadataFrame`
- `CapturedFrame` enum

### Enumerations (Stable)

- `FourCCVideoType` - Video pixel formats
- `RecvColorFormat` - Receiver color formats
- `RecvBandwidth` - Bandwidth modes
- `FrameFormatType` - Progressive/interlaced
- `AudioFormat` - Audio sample formats

### Error Types (Stable)

- `Error` enum with all variants
- Error conversion traits

## Pre-1.0 TODO

Before releasing 1.0.0, we need to:

1. ✅ Comprehensive rustdoc for all public items
2. ✅ Examples for common use cases
3. ✅ Migration guides from previous versions
4. ⬜ Audit entire public API surface
5. ⬜ Mark appropriate enums as `#[non_exhaustive]`
6. ⬜ Final review of type names and method signatures
7. ⬜ Performance benchmarks documented
8. ⬜ Platform support clearly documented

## Builder Pattern Stability

All builders follow this pattern:
- Builders are created via `Type::builder()` or `TypeBuilder::new()`
- All builder methods return `Self` for chaining
- `build()` returns `Result<Type, Error>` for validation
- New optional fields can be added without breaking changes

## FFI Safety

The underlying NDI SDK may change, but we commit to:
- Maintaining the safe Rust API even if FFI changes
- Documenting any behavioral changes from SDK updates
- Testing against multiple SDK versions when possible