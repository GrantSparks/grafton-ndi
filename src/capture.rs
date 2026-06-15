//! Unified frame capture abstraction via trait-based generics.
//!
//! This module provides a single generic RAII guard and capture path for all frame types
//! (video, audio, metadata), eliminating duplication while maintaining zero-cost abstraction.
//!
//! # Architecture
//!
//! Two sealed traits split the abstraction along its two real axes:
//!
//! - [`FrameFree`] is the *free strategy*: the SDK instance handle type plus the
//!   `free_*` call that releases one captured frame. The generic RAII guard
//!   [`Guard<'owner, S>`](Guard) is written once against it, so every owning
//!   surface — the [`Receiver`](crate::Receiver) capture path and
//!   [`FrameSync`](crate::FrameSync) — shares one guard rather than hand-rolling
//!   its own.
//! - [`CaptureKind`] builds on `FrameFree` for the receiver capture path only,
//!   adding the `NDIlib_recv_capture_v3` routing, the borrowed/owned frame
//!   types, and the frame-type discriminant.
//!
//! # Example
//!
//! ```ignore
//! // Internal use only - this module is not part of the public API
//! use crate::capture::{Guard, VideoKind};
//!
//! let guard = unsafe { Guard::<VideoKind>::new(instance, frame) };
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

/// Sealed trait module to prevent external implementations of [`FrameFree`] and
/// [`CaptureKind`].
mod sealed {
    pub trait Sealed {}

    impl Sealed for super::VideoKind {}
    impl Sealed for super::AudioKind {}
    impl Sealed for super::MetadataKind {}
    impl Sealed for super::FrameSyncVideoFree {}
    impl Sealed for super::FrameSyncAudioFree {}
}

/// The *free strategy* for one captured-frame family: the SDK instance handle
/// type plus the SDK call that releases a single frame.
///
/// This is the single axis along which the RAII `Guard` varies. The receiver
/// kinds ([`VideoKind`], [`AudioKind`], [`MetadataKind`]) free through
/// `NDIlib_recv_free_*`; the FrameSync strategies ([`FrameSyncVideoFree`],
/// [`FrameSyncAudioFree`]) free through `NDIlib_framesync_free_*`. Factoring the
/// free call out of [`CaptureKind`] lets both families reuse one guard and one
/// borrowed-reference core instead of maintaining parallel copies.
///
/// This is a sealed trait — it cannot be implemented outside this crate.
///
/// # Safety
///
/// Implementors must ensure [`free`](Self::free) releases a frame that was
/// populated by the matching SDK capture call through the same `instance`.
pub trait FrameFree: sealed::Sealed + 'static {
    /// The SDK instance handle that owns the frame buffers (and through which
    /// they must be freed).
    type Instance: Copy;

    /// The raw FFI frame type from the NDI SDK.
    type RawFrame: Default + Copy;

    /// The `Debug` struct name of the borrowed reference that wraps this guard
    /// (e.g. `"VideoFrameRef"` or `"FrameSyncVideoRef"`), so the shared generic
    /// `Debug` impls render the historically-correct type name.
    const REF_DEBUG_NAME: &'static str;

    /// Free a single captured frame through its owning instance.
    ///
    /// # Safety
    ///
    /// - `instance` must be the instance that produced `frame` (the FrameSync
    ///   strategies additionally tolerate a null `instance` by short-circuiting).
    /// - `frame` must have been populated by a successful capture for this
    ///   strategy and not already freed.
    unsafe fn free(instance: Self::Instance, frame: &mut Self::RawFrame);
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
/// Implementors must ensure the members agree on the same
/// [`RawFrame`](FrameFree::RawFrame): `capture_into` populates it via
/// `NDIlib_recv_capture_v3`, [`free`](FrameFree::free) frees frames so populated,
/// and `make_ref` wraps one that `capture_into` reported as
/// [`FRAME_TYPE`](Self::FRAME_TYPE).
pub trait CaptureKind: FrameFree<Instance = NDIlib_recv_instance_t> + Sized {
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
    /// - `frame` must point to a writable [`RawFrame`](FrameFree::RawFrame).
    unsafe fn capture_into(
        instance: NDIlib_recv_instance_t,
        frame: *mut Self::RawFrame,
        timeout_ms: u32,
    ) -> NDIlib_frame_type_e;

    /// Wrap a freshly captured frame guard in its borrowed view, validating any
    /// kind-specific invariants (e.g. FourCC) during construction.
    ///
    /// # Safety
    ///
    /// `guard` must own a frame that `capture_into` reported as
    /// [`FRAME_TYPE`](Self::FRAME_TYPE).
    unsafe fn make_ref<'rx>(guard: Guard<'rx, Self>) -> Result<Self::Ref<'rx>>;

    /// Copy a borrowed view into an owned, `'static` frame.
    fn ref_to_owned(frame: &Self::Ref<'_>) -> Result<Self::Owned>;
}

/// Marker type for video frame capture operations.
pub struct VideoKind;

impl FrameFree for VideoKind {
    type Instance = NDIlib_recv_instance_t;
    type RawFrame = NDIlib_video_frame_v2_t;
    const REF_DEBUG_NAME: &'static str = "VideoFrameRef";

    unsafe fn free(instance: Self::Instance, frame: &mut Self::RawFrame) {
        NDIlib_recv_free_video_v2(instance, frame);
    }
}

impl CaptureKind for VideoKind {
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

    unsafe fn make_ref<'rx>(guard: Guard<'rx, Self>) -> Result<Self::Ref<'rx>> {
        VideoFrameRef::new(guard)
    }

    fn ref_to_owned(frame: &Self::Ref<'_>) -> Result<Self::Owned> {
        frame.to_owned()
    }
}

/// Marker type for audio frame capture operations.
pub struct AudioKind;

impl FrameFree for AudioKind {
    type Instance = NDIlib_recv_instance_t;
    type RawFrame = NDIlib_audio_frame_v3_t;
    const REF_DEBUG_NAME: &'static str = "AudioFrameRef";

    unsafe fn free(instance: Self::Instance, frame: &mut Self::RawFrame) {
        NDIlib_recv_free_audio_v3(instance, frame);
    }
}

impl CaptureKind for AudioKind {
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

    unsafe fn make_ref<'rx>(guard: Guard<'rx, Self>) -> Result<Self::Ref<'rx>> {
        AudioFrameRef::new(guard)
    }

    fn ref_to_owned(frame: &Self::Ref<'_>) -> Result<Self::Owned> {
        frame.to_owned()
    }
}

/// Marker type for metadata frame capture operations.
pub struct MetadataKind;

impl FrameFree for MetadataKind {
    type Instance = NDIlib_recv_instance_t;
    type RawFrame = NDIlib_metadata_frame_t;
    const REF_DEBUG_NAME: &'static str = "MetadataFrameRef";

    unsafe fn free(instance: Self::Instance, frame: &mut Self::RawFrame) {
        NDIlib_recv_free_metadata(instance, frame);
    }
}

impl CaptureKind for MetadataKind {
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

    unsafe fn make_ref<'rx>(guard: Guard<'rx, Self>) -> Result<Self::Ref<'rx>> {
        MetadataFrameRef::new(guard)
    }

    fn ref_to_owned(frame: &Self::Ref<'_>) -> Result<Self::Owned> {
        // Metadata `to_owned` is infallible; lift it into the shared `Result` shape.
        Ok(frame.to_owned())
    }
}

/// Free strategy for video frames captured through [`FrameSync`](crate::FrameSync).
///
/// Frees via `NDIlib_framesync_free_video`. Unlike the receiver strategies this
/// tolerates a null instance handle (the borrowed-frame tests construct guards
/// with a null instance), short-circuiting the free in that case.
pub struct FrameSyncVideoFree;

impl FrameFree for FrameSyncVideoFree {
    type Instance = NDIlib_framesync_instance_t;
    type RawFrame = NDIlib_video_frame_v2_t;
    const REF_DEBUG_NAME: &'static str = "FrameSyncVideoRef";

    unsafe fn free(instance: Self::Instance, frame: &mut Self::RawFrame) {
        if instance.is_null() {
            return;
        }
        NDIlib_framesync_free_video(instance, frame);
    }
}

/// Free strategy for audio frames captured through [`FrameSync`](crate::FrameSync).
///
/// Frees via `NDIlib_framesync_free_audio_v2`, tolerating a null instance handle
/// the same way [`FrameSyncVideoFree`] does.
pub struct FrameSyncAudioFree;

impl FrameFree for FrameSyncAudioFree {
    type Instance = NDIlib_framesync_instance_t;
    type RawFrame = NDIlib_audio_frame_v3_t;
    const REF_DEBUG_NAME: &'static str = "FrameSyncAudioRef";

    unsafe fn free(instance: Self::Instance, frame: &mut Self::RawFrame) {
        if instance.is_null() {
            return;
        }
        NDIlib_framesync_free_audio_v2(instance, frame);
    }
}

/// Generic RAII guard for captured NDI frames.
///
/// This guard ensures that captured frames are freed exactly once via the free
/// strategy `S`'s [`FrameFree::free`] call. The strategy is determined by the
/// `S: FrameFree` type parameter, enabling compile-time dispatch with zero
/// runtime cost — receiver captures use `NDIlib_recv_free_*`, FrameSync captures
/// use `NDIlib_framesync_free_*`, all through the same guard.
///
/// The lifetime parameter `'owner` ties this guard to the owner that created it
/// (a `Receiver` or a `FrameSync`), preventing use-after-free by ensuring the
/// owner cannot be dropped while this guard is alive.
///
/// # Type Parameters
///
/// - `'owner`: Lifetime of the owning instance's borrow
/// - `S`: The free strategy (video/audio/metadata, receiver or FrameSync)
///
/// # Safety
///
/// This struct stores raw FFI types and must only be created through the `unsafe fn new()`
/// constructor, which requires the caller to guarantee validity of the instance and frame.
pub struct Guard<'owner, S: FrameFree> {
    instance: S::Instance,
    frame: S::RawFrame,
    // Ties the guard (and the borrowed refs built on it) to the owning instance's
    // borrow. The owner type is irrelevant to the borrow checker, so a unit
    // reference is enough to carry `'owner` covariantly.
    _owner: PhantomData<&'owner ()>,
    // SDK-owned buffers must be freed through the originating instance, on the
    // originating thread. Keep the guard (and borrowed frame refs) deliberately
    // !Send/!Sync rather than relying on raw-pointer auto-traits from generated
    // bindings.
    _thread_affine: PhantomData<Rc<()>>,
}

impl<'owner, S: FrameFree> Guard<'owner, S> {
    /// Create a new frame guard.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - `instance` is a valid instance for the free strategy `S` (or null only
    ///   for the FrameSync strategies, which short-circuit their free)
    /// - `frame` was populated by a successful capture for `S` and not yet freed
    pub(crate) unsafe fn new(instance: S::Instance, frame: S::RawFrame) -> Self {
        Self {
            instance,
            frame,
            _owner: PhantomData,
            _thread_affine: PhantomData,
        }
    }

    /// Get a reference to the underlying raw frame.
    pub(crate) fn frame(&self) -> &S::RawFrame {
        &self.frame
    }
}

impl<'owner, S: FrameFree> Drop for Guard<'owner, S> {
    fn drop(&mut self) {
        // SAFETY: The constructor guarantees that instance and frame are valid
        unsafe {
            S::free(self.instance, &mut self.frame);
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
    Frame(Guard<'rx, K>),
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
        t if t == K::FRAME_TYPE => CaptureResult::Frame(Guard::<K>::new(instance, frame)),
        NDIlib_frame_type_e_NDIlib_frame_type_none => CaptureResult::None,
        NDIlib_frame_type_e_NDIlib_frame_type_error => CaptureResult::Error,
        _ => CaptureResult::None, // Other frame types are ignored
    }
}

// Convenience aliases for the receiver guard kinds that have a kind-specific
// constructor (audio validates a concrete layout; metadata wraps its own ref).
// Video and the FrameSync families name `Guard<'_, Strategy>` directly.
/// RAII guard for a captured audio frame.
///
/// Automatically calls `NDIlib_recv_free_audio_v3` when dropped.
pub type RecvAudioGuard<'rx> = Guard<'rx, AudioKind>;

/// RAII guard for a captured metadata frame.
///
/// Automatically calls `NDIlib_recv_free_metadata` when dropped.
pub type RecvMetadataGuard<'rx> = Guard<'rx, MetadataKind>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guard_sizes() {
        use std::mem::size_of;

        // Guards should be compact - instance pointer + frame struct + zero-sized PhantomData
        assert!(size_of::<RecvAudioGuard>() > 0);
        assert!(size_of::<RecvMetadataGuard>() > 0);

        // Generic guard with different kinds should have same overhead per kind
        // (sizes differ only due to RawFrame size differences)
        assert!(size_of::<Guard<VideoKind>>() > 0);
        assert!(size_of::<Guard<AudioKind>>() > 0);
        assert!(size_of::<Guard<MetadataKind>>() > 0);
        assert!(size_of::<Guard<FrameSyncVideoFree>>() > 0);
        assert!(size_of::<Guard<FrameSyncAudioFree>>() > 0);
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
