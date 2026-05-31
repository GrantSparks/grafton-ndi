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

use std::{marker::PhantomData, rc::Rc};

use crate::{
    frames::{
        AudioFrame, AudioFrameRef, MetadataFrame, MetadataFrameRef, VideoFrame, VideoFrameRef,
    },
    ndi_lib::*,
    Result,
};

/// Sealed trait module to prevent external implementations of `CaptureKind`.
mod sealed {
    pub trait Sealed {}

    impl Sealed for super::VideoKind {}
    impl Sealed for super::AudioKind {}
    impl Sealed for super::MetadataKind {}
}

/// Single source of truth describing one NDI frame kind (video, audio, or
/// metadata).
///
/// Each implementor wires together the FFI frame struct, the borrowed and owned
/// Rust frame types, and the four SDK calls those entail — capture, the
/// frame-type discriminant, freeing, and the borrowed/owned conversions. Every
/// generic capture path (`capture_raw`, [`Receiver`](crate::Receiver)'s
/// [`Capture`](crate::Capture) view, and the async views) is written once
/// against this trait, so adding or changing a kind happens in exactly one
/// place.
///
/// This is a sealed trait — it cannot be implemented outside this crate.
/// Implementations exist for [`VideoKind`], [`AudioKind`], and [`MetadataKind`].
///
/// # Safety
///
/// Implementors must ensure that the four `unsafe` members agree on the same
/// `RawFrame`: `capture_into` populates it via `NDIlib_recv_capture_v3`,
/// `free_frame` frees frames so populated, and `make_ref` wraps one that
/// `capture_into` reported as [`FRAME_TYPE`](Self::FRAME_TYPE).
pub trait CaptureKind: sealed::Sealed + Sized + 'static {
    /// The raw FFI frame type from the NDI SDK.
    type RawFrame: Default + Copy;

    /// The borrowed, zero-copy view of a captured frame, tied to the receiver
    /// that produced it.
    type Ref<'rx>;

    /// The owned, `'static` frame produced by copying a borrowed view.
    type Owned;

    /// The frame-type discriminant `NDIlib_recv_capture_v3` returns for this kind.
    const FRAME_TYPE: NDIlib_frame_type_e;

    /// Run a capture for this kind, routing `frame` into the matching
    /// `NDIlib_recv_capture_v3` slot and ignoring the others.
    ///
    /// # Safety
    ///
    /// - `instance` must be a valid NDI receiver instance.
    /// - `frame` must point to a writable [`RawFrame`](Self::RawFrame).
    unsafe fn capture_into(
        instance: NDIlib_recv_instance_t,
        frame: *mut Self::RawFrame,
        timeout_ms: u32,
    ) -> NDIlib_frame_type_e;

    /// Free a captured frame.
    ///
    /// # Safety
    ///
    /// - `instance` must be a valid NDI receiver instance
    /// - `frame` must have been populated by a successful capture that returned `FRAME_TYPE`
    unsafe fn free_frame(instance: NDIlib_recv_instance_t, frame: &Self::RawFrame);

    /// Wrap a freshly captured frame guard in its borrowed view, validating any
    /// kind-specific invariants (e.g. FourCC) during construction.
    ///
    /// # Safety
    ///
    /// `guard` must own a frame that `capture_into` reported as
    /// [`FRAME_TYPE`](Self::FRAME_TYPE).
    unsafe fn make_ref<'rx>(guard: RecvGuard<'rx, Self>) -> Result<Self::Ref<'rx>>;

    /// Copy a borrowed view into an owned, `'static` frame.
    fn ref_to_owned(frame: &Self::Ref<'_>) -> Result<Self::Owned>;
}

/// Marker type for video frame capture operations.
pub struct VideoKind;

impl CaptureKind for VideoKind {
    type RawFrame = NDIlib_video_frame_v2_t;
    type Ref<'rx> = VideoFrameRef<'rx>;
    type Owned = VideoFrame;
    const FRAME_TYPE: NDIlib_frame_type_e = NDIlib_frame_type_e_NDIlib_frame_type_video;

    unsafe fn capture_into(
        instance: NDIlib_recv_instance_t,
        frame: *mut Self::RawFrame,
        timeout_ms: u32,
    ) -> NDIlib_frame_type_e {
        NDIlib_recv_capture_v3(
            instance,
            frame,
            std::ptr::null_mut(), // no audio
            std::ptr::null_mut(), // no metadata
            timeout_ms,
        )
    }

    unsafe fn free_frame(instance: NDIlib_recv_instance_t, frame: &Self::RawFrame) {
        NDIlib_recv_free_video_v2(instance, frame);
    }

    unsafe fn make_ref<'rx>(guard: RecvGuard<'rx, Self>) -> Result<Self::Ref<'rx>> {
        VideoFrameRef::new(guard)
    }

    fn ref_to_owned(frame: &Self::Ref<'_>) -> Result<Self::Owned> {
        frame.to_owned()
    }
}

/// Marker type for audio frame capture operations.
pub struct AudioKind;

impl CaptureKind for AudioKind {
    type RawFrame = NDIlib_audio_frame_v3_t;
    type Ref<'rx> = AudioFrameRef<'rx>;
    type Owned = AudioFrame;
    const FRAME_TYPE: NDIlib_frame_type_e = NDIlib_frame_type_e_NDIlib_frame_type_audio;

    unsafe fn capture_into(
        instance: NDIlib_recv_instance_t,
        frame: *mut Self::RawFrame,
        timeout_ms: u32,
    ) -> NDIlib_frame_type_e {
        NDIlib_recv_capture_v3(
            instance,
            std::ptr::null_mut(), // no video
            frame,
            std::ptr::null_mut(), // no metadata
            timeout_ms,
        )
    }

    unsafe fn free_frame(instance: NDIlib_recv_instance_t, frame: &Self::RawFrame) {
        NDIlib_recv_free_audio_v3(instance, frame);
    }

    unsafe fn make_ref<'rx>(guard: RecvGuard<'rx, Self>) -> Result<Self::Ref<'rx>> {
        AudioFrameRef::new(guard)
    }

    fn ref_to_owned(frame: &Self::Ref<'_>) -> Result<Self::Owned> {
        frame.to_owned()
    }
}

/// Marker type for metadata frame capture operations.
pub struct MetadataKind;

impl CaptureKind for MetadataKind {
    type RawFrame = NDIlib_metadata_frame_t;
    type Ref<'rx> = MetadataFrameRef<'rx>;
    type Owned = MetadataFrame;
    const FRAME_TYPE: NDIlib_frame_type_e = NDIlib_frame_type_e_NDIlib_frame_type_metadata;

    unsafe fn capture_into(
        instance: NDIlib_recv_instance_t,
        frame: *mut Self::RawFrame,
        timeout_ms: u32,
    ) -> NDIlib_frame_type_e {
        NDIlib_recv_capture_v3(
            instance,
            std::ptr::null_mut(), // no video
            std::ptr::null_mut(), // no audio
            frame,
            timeout_ms,
        )
    }

    unsafe fn free_frame(instance: NDIlib_recv_instance_t, frame: &Self::RawFrame) {
        NDIlib_recv_free_metadata(instance, frame);
    }

    unsafe fn make_ref<'rx>(guard: RecvGuard<'rx, Self>) -> Result<Self::Ref<'rx>> {
        MetadataFrameRef::new(guard)
    }

    fn ref_to_owned(frame: &Self::Ref<'_>) -> Result<Self::Owned> {
        // Metadata `to_owned` is infallible; lift it into the shared `Result` shape.
        Ok(frame.to_owned())
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
    // SDK-owned receive buffers must be freed through the originating receiver.
    // Keep the borrowed frame refs deliberately !Send/!Sync rather than relying
    // on raw-pointer auto-traits from generated bindings.
    _thread_affine: PhantomData<Rc<()>>,
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
            _thread_affine: PhantomData,
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

/// Capture a frame of kind `K` from an NDI receiver.
///
/// Routes the frame into the matching `NDIlib_recv_capture_v3` slot via
/// [`CaptureKind::capture_into`] and classifies the result. Other frame types
/// (e.g. status-change) are treated as [`CaptureResult::None`].
///
/// # Safety
///
/// `instance` must be a valid NDI receiver instance.
pub(crate) unsafe fn capture_raw<'rx, K: CaptureKind>(
    instance: NDIlib_recv_instance_t,
    timeout_ms: u32,
) -> CaptureResult<'rx, K> {
    let mut frame = K::RawFrame::default();
    let frame_type = K::capture_into(instance, &mut frame, timeout_ms);

    match frame_type {
        t if t == K::FRAME_TYPE => CaptureResult::Frame(RecvGuard::<K>::new(instance, frame)),
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
