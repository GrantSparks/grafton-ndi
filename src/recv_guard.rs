//! RAII guards for NDI receive frames.
//!
//! This private module imports the unified generic guards from [`crate::capture`]
//! for internal frame-reference constructors. These guards and their raw SDK
//! strategies are not part of the public capture API.
//!
//! ## Architecture
//!
//! The actual implementation lives in `capture.rs`, which provides:
//! - Public `CaptureKind` with borrowed/owned frame types and conversion
//! - Private `FrameFree` and `CaptureStrategy` SDK implementations
//! - `Guard<'owner, S>` generic RAII guard
//!
//! This module imports aliases for the receiver guard kinds that have a
//! kind-specific constructor:
//! - `RecvAudioGuard<'rx>` = `Guard<'rx, AudioKind>`
//! - `RecvMetadataGuard<'rx>` = `Guard<'rx, MetadataKind>`
//!
//! Video and the FrameSync families name `Guard<'_, Strategy>` directly.

pub(crate) use crate::capture::{RecvAudioGuard, RecvMetadataGuard};
