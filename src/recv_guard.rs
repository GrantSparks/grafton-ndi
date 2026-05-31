//! RAII guards for NDI receive frames.
//!
//! This module re-exports the unified generic guards from [`crate::capture`],
//! providing backwards-compatible type aliases for the frame-specific guard types.
//!
//! ## Architecture
//!
//! The actual implementation lives in `capture.rs`, which provides:
//! - `FrameFree` free-strategy trait and `CaptureKind` capture trait
//! - `Guard<'owner, S>` generic RAII guard
//!
//! This module re-exports type aliases for the receiver guard kinds that have a
//! kind-specific constructor:
//! - `RecvAudioGuard<'rx>` = `Guard<'rx, AudioKind>`
//! - `RecvMetadataGuard<'rx>` = `Guard<'rx, MetadataKind>`
//!
//! Video and the FrameSync families name `Guard<'_, Strategy>` directly.

pub(crate) use crate::capture::{RecvAudioGuard, RecvMetadataGuard};
