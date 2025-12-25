//! RAII guards for NDI receive frames.
//!
//! This module re-exports the unified generic guards from [`crate::capture`],
//! providing backwards-compatible type aliases for the frame-specific guard types.
//!
//! ## Architecture
//!
//! The actual implementation lives in `capture.rs`, which provides:
//! - `CaptureKind` trait for frame-type-specific behavior
//! - `RecvGuard<'rx, K>` generic RAII guard
//!
//! This module re-exports type aliases for backwards compatibility:
//! - `RecvVideoGuard<'rx>` = `RecvGuard<'rx, VideoKind>`
//! - `RecvAudioGuard<'rx>` = `RecvGuard<'rx, AudioKind>`
//! - `RecvMetadataGuard<'rx>` = `RecvGuard<'rx, MetadataKind>`

pub(crate) use crate::capture::{RecvAudioGuard, RecvMetadataGuard, RecvVideoGuard};
