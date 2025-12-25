//! Unified frame capture abstraction via trait-based generics.
//!
//! This module provides a single generic RAII guard and capture path for all frame types
//! (video, audio, metadata), eliminating duplication while maintaining zero-cost abstraction.
//!
//! # Architecture
//!
//! The `CaptureKind` trait encapsulates frame-type-specific behavior:
//! - Associated types for raw FFI frame, owned frame, and borrowed frame reference
//! - Frame type constant for dispatch
//! - Free function for RAII cleanup
//!
//! The `RecvGuard<'rx, K>` generic struct provides unified RAII management for all frame types.
//!
//! # Example
//!
//! ```ignore
//! // Internal use only - this module is not part of the public API
//! use crate::capture::{RecvGuard, VideoKind};
//!
//! let guard = unsafe { RecvGuard::<VideoKind>::new(instance, frame) };
//! // Guard automatically calls the correct free function when dropped
//! ```

use std::marker::PhantomData;

use crate::ndi_lib::*;

/// Sealed trait module to prevent external implementations of `CaptureKind`.
mod sealed {
    pub trait Sealed {}

    impl Sealed for super::VideoKind {}
    impl Sealed for super::AudioKind {}
    impl Sealed for super::MetadataKind {}
}

/// Trait that encapsulates frame-type-specific behavior for capture operations.
///
/// This is a sealed trait - it cannot be implemented outside this crate.
/// Implementations exist for [`VideoKind`], [`AudioKind`], and [`MetadataKind`].
///
/// # Associated Types
///
/// - `RawFrame`: The FFI frame type from the NDI SDK (e.g., `NDIlib_video_frame_v2_t`)
///
/// # Safety
///
/// Implementors must ensure that `free_frame` correctly frees frames of type `RawFrame`
/// that were populated by `NDIlib_recv_capture_v3`.
pub trait CaptureKind: sealed::Sealed {
    /// The raw FFI frame type from the NDI SDK.
    type RawFrame: Default + Copy;

    /// The expected frame type constant returned by `NDIlib_recv_capture_v3`.
    const FRAME_TYPE: NDIlib_frame_type_e;

    /// Free a captured frame.
    ///
    /// # Safety
    ///
    /// - `instance` must be a valid NDI receiver instance
    /// - `frame` must have been populated by a successful capture that returned `FRAME_TYPE`
    unsafe fn free_frame(instance: NDIlib_recv_instance_t, frame: &Self::RawFrame);
}

/// Marker type for video frame capture operations.
pub struct VideoKind;

impl CaptureKind for VideoKind {
    type RawFrame = NDIlib_video_frame_v2_t;
    const FRAME_TYPE: NDIlib_frame_type_e = NDIlib_frame_type_e_NDIlib_frame_type_video;

    unsafe fn free_frame(instance: NDIlib_recv_instance_t, frame: &Self::RawFrame) {
        NDIlib_recv_free_video_v2(instance, frame);
    }
}

/// Marker type for audio frame capture operations.
pub struct AudioKind;

impl CaptureKind for AudioKind {
    type RawFrame = NDIlib_audio_frame_v3_t;
    const FRAME_TYPE: NDIlib_frame_type_e = NDIlib_frame_type_e_NDIlib_frame_type_audio;

    unsafe fn free_frame(instance: NDIlib_recv_instance_t, frame: &Self::RawFrame) {
        NDIlib_recv_free_audio_v3(instance, frame);
    }
}

/// Marker type for metadata frame capture operations.
pub struct MetadataKind;

impl CaptureKind for MetadataKind {
    type RawFrame = NDIlib_metadata_frame_t;
    const FRAME_TYPE: NDIlib_frame_type_e = NDIlib_frame_type_e_NDIlib_frame_type_metadata;

    unsafe fn free_frame(instance: NDIlib_recv_instance_t, frame: &Self::RawFrame) {
        NDIlib_recv_free_metadata(instance, frame);
    }
}

/// Generic RAII guard for captured NDI frames.
///
/// This guard ensures that captured frames are freed exactly once via the appropriate
/// `NDIlib_recv_free_*` function. The frame type is determined by the `K: CaptureKind`
/// type parameter, enabling compile-time dispatch with zero runtime cost.
///
/// The lifetime parameter `'rx` ties this guard to the `Receiver` that created it,
/// preventing use-after-free by ensuring the receiver cannot be dropped while
/// this guard is alive.
///
/// # Type Parameters
///
/// - `'rx`: Lifetime of the receiver borrow
/// - `K`: The kind of frame (video, audio, or metadata)
///
/// # Safety
///
/// This struct stores raw FFI types and must only be created through the `unsafe fn new()`
/// constructor, which requires the caller to guarantee validity of the instance and frame.
pub struct RecvGuard<'rx, K: CaptureKind> {
    instance: NDIlib_recv_instance_t,
    frame: K::RawFrame,
    _owner: PhantomData<&'rx crate::Receiver>,
}

impl<'rx, K: CaptureKind> RecvGuard<'rx, K> {
    /// Create a new frame guard.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - `instance` is a valid NDI receiver instance
    /// - `frame` was populated by a successful call to `NDIlib_recv_capture_v3`
    ///   that returned the frame type corresponding to `K`
    pub(crate) unsafe fn new(instance: NDIlib_recv_instance_t, frame: K::RawFrame) -> Self {
        Self {
            instance,
            frame,
            _owner: PhantomData,
        }
    }

    /// Get a reference to the underlying raw frame.
    pub(crate) fn frame(&self) -> &K::RawFrame {
        &self.frame
    }
}

impl<'rx, K: CaptureKind> Drop for RecvGuard<'rx, K> {
    fn drop(&mut self) {
        // SAFETY: The constructor guarantees that instance and frame are valid
        unsafe {
            K::free_frame(self.instance, &self.frame);
        }
    }
}

/// Result of a generic capture operation.
///
/// This enum represents the possible outcomes of calling `NDIlib_recv_capture_v3`:
/// - `Frame`: Successfully captured a frame of the expected type
/// - `None`: No frame available (timeout)
/// - `Error`: The SDK returned an error frame
pub(crate) enum CaptureResult<'rx, K: CaptureKind> {
    /// Successfully captured a frame of the expected type.
    Frame(RecvGuard<'rx, K>),
    /// No frame available within timeout, or a different frame type was returned.
    None,
    /// The SDK returned an error frame.
    Error,
}

/// Capture a video frame from an NDI receiver.
///
/// # Safety
///
/// `instance` must be a valid NDI receiver instance.
pub(crate) unsafe fn capture_video_raw<'rx>(
    instance: NDIlib_recv_instance_t,
    timeout_ms: u32,
) -> CaptureResult<'rx, VideoKind> {
    use std::ptr;

    let mut frame = NDIlib_video_frame_v2_t::default();

    let frame_type = NDIlib_recv_capture_v3(
        instance,
        &mut frame,
        ptr::null_mut(), // no audio
        ptr::null_mut(), // no metadata
        timeout_ms,
    );

    match frame_type {
        t if t == VideoKind::FRAME_TYPE => {
            CaptureResult::Frame(RecvGuard::<VideoKind>::new(instance, frame))
        }
        NDIlib_frame_type_e_NDIlib_frame_type_none => CaptureResult::None,
        NDIlib_frame_type_e_NDIlib_frame_type_error => CaptureResult::Error,
        _ => CaptureResult::None, // Other frame types are ignored
    }
}

/// Capture an audio frame from an NDI receiver.
///
/// # Safety
///
/// `instance` must be a valid NDI receiver instance.
pub(crate) unsafe fn capture_audio_raw<'rx>(
    instance: NDIlib_recv_instance_t,
    timeout_ms: u32,
) -> CaptureResult<'rx, AudioKind> {
    use std::ptr;

    let mut frame = NDIlib_audio_frame_v3_t::default();

    let frame_type = NDIlib_recv_capture_v3(
        instance,
        ptr::null_mut(), // no video
        &mut frame,
        ptr::null_mut(), // no metadata
        timeout_ms,
    );

    match frame_type {
        t if t == AudioKind::FRAME_TYPE => {
            CaptureResult::Frame(RecvGuard::<AudioKind>::new(instance, frame))
        }
        NDIlib_frame_type_e_NDIlib_frame_type_none => CaptureResult::None,
        NDIlib_frame_type_e_NDIlib_frame_type_error => CaptureResult::Error,
        _ => CaptureResult::None, // Other frame types are ignored
    }
}

/// Capture a metadata frame from an NDI receiver.
///
/// # Safety
///
/// `instance` must be a valid NDI receiver instance.
pub(crate) unsafe fn capture_metadata_raw<'rx>(
    instance: NDIlib_recv_instance_t,
    timeout_ms: u32,
) -> CaptureResult<'rx, MetadataKind> {
    use std::ptr;

    let mut frame = NDIlib_metadata_frame_t::default();

    let frame_type = NDIlib_recv_capture_v3(
        instance,
        ptr::null_mut(), // no video
        ptr::null_mut(), // no audio
        &mut frame,
        timeout_ms,
    );

    match frame_type {
        t if t == MetadataKind::FRAME_TYPE => {
            CaptureResult::Frame(RecvGuard::<MetadataKind>::new(instance, frame))
        }
        NDIlib_frame_type_e_NDIlib_frame_type_none => CaptureResult::None,
        NDIlib_frame_type_e_NDIlib_frame_type_error => CaptureResult::Error,
        _ => CaptureResult::None, // Other frame types are ignored
    }
}

// Type aliases for backwards compatibility with existing code
/// RAII guard for a captured video frame.
///
/// Automatically calls `NDIlib_recv_free_video_v2` when dropped.
pub type RecvVideoGuard<'rx> = RecvGuard<'rx, VideoKind>;

/// RAII guard for a captured audio frame.
///
/// Automatically calls `NDIlib_recv_free_audio_v3` when dropped.
pub type RecvAudioGuard<'rx> = RecvGuard<'rx, AudioKind>;

/// RAII guard for a captured metadata frame.
///
/// Automatically calls `NDIlib_recv_free_metadata` when dropped.
pub type RecvMetadataGuard<'rx> = RecvGuard<'rx, MetadataKind>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guard_sizes() {
        use std::mem::size_of;

        // Guards should be compact - instance pointer + frame struct + zero-sized PhantomData
        assert!(size_of::<RecvVideoGuard>() > 0);
        assert!(size_of::<RecvAudioGuard>() > 0);
        assert!(size_of::<RecvMetadataGuard>() > 0);

        // Generic guard with different kinds should have same overhead per kind
        // (sizes differ only due to RawFrame size differences)
        assert!(size_of::<RecvGuard<VideoKind>>() > 0);
        assert!(size_of::<RecvGuard<AudioKind>>() > 0);
        assert!(size_of::<RecvGuard<MetadataKind>>() > 0);
    }

    #[test]
    fn test_frame_type_constants() {
        // Verify frame type constants match expected values
        assert_eq!(
            VideoKind::FRAME_TYPE,
            NDIlib_frame_type_e_NDIlib_frame_type_video
        );
        assert_eq!(
            AudioKind::FRAME_TYPE,
            NDIlib_frame_type_e_NDIlib_frame_type_audio
        );
        assert_eq!(
            MetadataKind::FRAME_TYPE,
            NDIlib_frame_type_e_NDIlib_frame_type_metadata
        );
    }
}
