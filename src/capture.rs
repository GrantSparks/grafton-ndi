//! Unified frame capture abstraction via trait-based generics.
//!
//! This module provides a single generic RAII guard and capture path for all frame types
//! (video, audio, metadata), eliminating duplication while maintaining zero-cost abstraction.
//!
//! # Architecture
//!
//! The public capture kind describes Rust frame types; private strategies
//! describe SDK routing and freeing:
//!
//! - [`FrameFree`] is the *free strategy*: the SDK instance handle type plus the
//!   `free_*` call that releases one captured frame. The generic RAII guard
//!   [`Guard<'owner, S>`](Guard) is written once against it, so every owning
//!   surface — receiver capture, FrameSync and Advanced AV sync — shares one
//!   guard rather than hand-rolling its own.
//! - [`CaptureKind`] selects the borrowed frame type.
//! - `CaptureStrategy` privately connects each receiver kind to its slot of
//!   the SDK capture call and its frame-type discriminant, wrapping and
//!   freeing the matching raw frame family. The capture itself is made only through
//!   a [`CaptureLock`], which fixes the receiver, runtime and remaining wait
//!   after the mode's permissions have been checked. [`capture_raw`]
//!   (one kind) and `capture_any` (a sealed set of all media slots or none)
//!   both go through `Captured`. A delivered frame becomes a guard through
//!   `Captured::take`, the one place a receiver guard is made. Mixed and
//!   notification-only capture share a receiver kernel that handles every
//!   notification before the caller can convert media.
//!
//! # Example
//!
//! ```text
//! // Internal schematic; this module is not part of the public API
//! use crate::capture::{Guard, VideoKind};
//!
//! let guard = unsafe { Guard::<VideoKind>::new(runtime, instance, frame) };
//! // Guard automatically calls the correct free function when dropped
//! ```

use std::{marker::PhantomData, rc::Rc};

use crate::{
    frames::{AudioFrameRef, MetadataFrameRef, VideoFrameRef},
    receiver::CaptureLock,
    sys::{self, bindings::*},
    Error, Ndi, Result,
};

/// Sealed trait module to prevent external implementations of [`FrameFree`] and
/// [`CaptureKind`].
mod sealed {
    pub trait Sealed {}

    impl Sealed for super::VideoKind {}
    impl Sealed for super::AudioKind {}
    impl Sealed for super::MetadataKind {}
    impl Sealed for super::SenderMetadataFree {}
    impl Sealed for super::FrameSyncVideoFree {}
    impl Sealed for super::FrameSyncAudioFree {}
    #[cfg(feature = "advanced")]
    impl Sealed for super::AvSyncAudioFree {}
    impl Sealed for super::AllSlots {}
    impl Sealed for super::NoSlots {}
}

/// The *free strategy* for one captured-frame family: the SDK instance handle
/// type plus the SDK call that releases a single frame.
///
/// This is the single axis along which the RAII `Guard` varies. The receiver
/// kinds ([`VideoKind`], [`AudioKind`], [`MetadataKind`]) free through
/// `NDIlib_recv_free_*`; the FrameSync strategies ([`FrameSyncVideoFree`],
/// [`FrameSyncAudioFree`]) free through `NDIlib_framesync_free_*`. Factoring the
/// free call out of [`CaptureKind`] lets every family, including Advanced
/// AV sync, reuse one guard and borrowed-reference core.
///
/// This is a sealed trait — it cannot be implemented outside this crate.
///
/// # Safety
///
/// Implementors must ensure [`free`](Self::free) releases a frame that was
/// populated by the matching SDK capture call through the same `instance`.
/// Its metadata scanner must use precisely that family's readable text extent.
pub(crate) unsafe trait FrameFree: sealed::Sealed + 'static {
    /// The SDK instance handle that owns the frame buffers (and through which
    /// they must be freed).
    type Instance: Copy;

    /// The raw FFI frame type from the NDI SDK.
    type RawFrame: Default + Copy;

    /// The `Debug` struct name of the borrowed reference that wraps this guard
    /// (e.g. `"VideoFrameRef"` or `"FrameSyncVideoRef"`), used by the shared
    /// generic `Debug` implementations.
    const REF_DEBUG_NAME: &'static str;

    /// Additional unknown-timestamp marker for audio from this capture family.
    /// Receiver capture uses the header marker; FrameSync also reports zero.
    const AUDIO_TIMESTAMP_UNDEFINED: i64 = NDIlib_recv_timestamp_undefined;

    /// The descriptor's natural metadata rule, overridden by capture families
    /// with a stronger text guarantee (notably upstream sender metadata).
    ///
    /// # Safety
    /// The descriptor must retain this family's readable metadata extent,
    /// initialized and unchanged until matching release.
    unsafe fn scan_metadata(frame: &Self::RawFrame) -> Result<crate::frames::FrameMetadataState>
    where
        Self::RawFrame: crate::frames::SdkDescriptor,
    {
        // SAFETY: the caller retains the natural descriptor extent.
        unsafe { crate::frames::SdkDescriptor::scan_metadata(frame) }
    }

    /// Free a single captured frame through its owning instance.
    ///
    /// # Safety
    ///
    /// - `instance` must be the instance that produced `frame`, created through
    ///   the runtime `core` belongs to.
    /// - `frame` must belong to this strategy's capture/free family and not
    ///   already have been freed. Empty request/result descriptors are allowed
    ///   when that strategy's release explicitly accepts null storage.
    unsafe fn free(core: &sys::Core, instance: Self::Instance, frame: &mut Self::RawFrame);
}

#[cfg(test)]
type TestFrameFree<S> = unsafe fn(<S as FrameFree>::Instance, &mut <S as FrameFree>::RawFrame);

/// A sealed receiver capture kind with its borrowed frame type.
///
/// Implemented for [`VideoKind`], [`AudioKind`], and [`MetadataKind`]. Raw SDK
/// routing, wrapping and freeing are private implementation details. The receiver kernels
/// use this trait to select the borrowed result type.
///
/// Receiver frame references implement `Send + Sync` and can move to scoped
/// worker threads while borrowing their receiver. FrameSync references use
/// separate private strategies and remain thread-affine.
#[allow(private_bounds)]
pub(crate) trait CaptureKind: CaptureStrategy + Sized {
    /// A zero-copy captured frame borrowing the receiver.
    type Ref<'rx>;
}

/// Private SDK routing paired with the corresponding free strategy.
/// Implementations must route, wrap and free the same raw frame family.
pub(crate) trait CaptureStrategy:
    FrameFree<Instance = NDIlib_recv_instance_t> + Sized
{
    /// The frame-type discriminant `NDIlib_recv_capture_v3` returns for this kind.
    const FRAME_TYPE: NDIlib_frame_type_e;

    /// This kind's media slot of an `NDIlib_recv_capture_v3` call.
    fn slot(slots: &mut Slots) -> &mut Option<Self::RawFrame>;

    /// Wrap a freshly captured frame guard in its borrowed view, validating any
    /// kind-specific invariants (e.g. FourCC) during construction.
    /// The guard carries the delivered storage contract for this frame kind.
    fn make_ref<'rx>(guard: Guard<'rx, Self>) -> Result<<Self as CaptureKind>::Ref<'rx>>
    where
        Self: CaptureKind;
}

/// Marker type for video frame capture operations.
#[derive(Debug)]
pub(crate) struct VideoKind;

// SAFETY: This strategy pairs its raw frame and instance types with the
// matching NDIlib_recv_free_video_v2 release through the owning runtime. Its
// embedded metadata retains the bounded first-NUL extent until that release.
unsafe impl FrameFree for VideoKind {
    type Instance = NDIlib_recv_instance_t;
    type RawFrame = NDIlib_video_frame_v2_t;
    const REF_DEBUG_NAME: &'static str = "VideoFrameRef";

    unsafe fn free(core: &sys::Core, instance: Self::Instance, frame: &mut Self::RawFrame) {
        core.NDIlib_recv_free_video_v2(instance, frame);
    }
}

impl CaptureKind for VideoKind {
    type Ref<'rx> = VideoFrameRef<'rx>;
}

impl CaptureStrategy for VideoKind {
    const FRAME_TYPE: NDIlib_frame_type_e = NDIlib_frame_type_e_NDIlib_frame_type_video;

    fn slot(slots: &mut Slots) -> &mut Option<Self::RawFrame> {
        &mut slots.video
    }

    fn make_ref<'rx>(guard: Guard<'rx, Self>) -> Result<<Self as CaptureKind>::Ref<'rx>> {
        VideoFrameRef::new(guard)
    }
}

/// Marker type for audio frame capture operations.
#[derive(Debug)]
pub(crate) struct AudioKind;

// SAFETY: This strategy pairs its raw frame and instance types with the
// matching NDIlib_recv_free_audio_v3 release through the owning runtime. Its
// embedded metadata retains the bounded first-NUL extent until that release.
unsafe impl FrameFree for AudioKind {
    type Instance = NDIlib_recv_instance_t;
    type RawFrame = NDIlib_audio_frame_v3_t;
    const REF_DEBUG_NAME: &'static str = "AudioFrameRef";

    unsafe fn free(core: &sys::Core, instance: Self::Instance, frame: &mut Self::RawFrame) {
        core.NDIlib_recv_free_audio_v3(instance, frame);
    }
}

impl CaptureKind for AudioKind {
    type Ref<'rx> = AudioFrameRef<'rx>;
}

impl CaptureStrategy for AudioKind {
    const FRAME_TYPE: NDIlib_frame_type_e = NDIlib_frame_type_e_NDIlib_frame_type_audio;

    fn slot(slots: &mut Slots) -> &mut Option<Self::RawFrame> {
        &mut slots.audio
    }

    fn make_ref<'rx>(guard: Guard<'rx, Self>) -> Result<<Self as CaptureKind>::Ref<'rx>> {
        AudioFrameRef::new(guard)
    }
}

/// Marker type for metadata frame capture operations.
#[derive(Debug)]
pub(crate) struct MetadataKind;

// SAFETY: This strategy pairs its raw frame and instance types with the
// matching NDIlib_recv_free_metadata release through the owning runtime. Its
// text retains the declared extent, or a bounded C string when length is zero.
unsafe impl FrameFree for MetadataKind {
    type Instance = NDIlib_recv_instance_t;
    type RawFrame = NDIlib_metadata_frame_t;
    const REF_DEBUG_NAME: &'static str = "MetadataFrameRef";

    unsafe fn free(core: &sys::Core, instance: Self::Instance, frame: &mut Self::RawFrame) {
        core.NDIlib_recv_free_metadata(instance, frame);
    }
}

impl CaptureKind for MetadataKind {
    type Ref<'rx> = MetadataFrameRef<'rx>;
}

impl CaptureStrategy for MetadataKind {
    const FRAME_TYPE: NDIlib_frame_type_e = NDIlib_frame_type_e_NDIlib_frame_type_metadata;

    fn slot(slots: &mut Slots) -> &mut Option<Self::RawFrame> {
        &mut slots.metadata
    }

    fn make_ref<'rx>(guard: Guard<'rx, Self>) -> Result<<Self as CaptureKind>::Ref<'rx>> {
        MetadataFrameRef::new(guard)
    }
}

/// Free strategy for upstream metadata captured by a sender.
#[derive(Debug)]
pub(crate) struct SenderMetadataFree;

/// The sender and authorized optional table that produced one metadata loan.
#[derive(Clone, Copy)]
pub(crate) struct SenderMetadataInstance {
    pub(crate) instance: NDIlib_send_instance_t,
    pub(crate) metadata: &'static sys::SenderMetadata,
}

// SAFETY: this strategy releases only the metadata descriptor returned by
// NDIlib_send_capture through the same sender instance and authorized table.
// That family's text is readable through the first NUL or metadata cap,
// independently of its reported count, so its scanner uses the sender rule.
unsafe impl FrameFree for SenderMetadataFree {
    type Instance = SenderMetadataInstance;
    type RawFrame = NDIlib_metadata_frame_t;
    const REF_DEBUG_NAME: &'static str = "MetadataFrameRef";

    unsafe fn scan_metadata(frame: &Self::RawFrame) -> Result<crate::frames::FrameMetadataState> {
        // SAFETY: sender captures retain the bounded C string even when their
        // positive count excludes its terminator.
        Ok(crate::frames::FrameMetadataState::from_text(unsafe {
            crate::frames::validate_sender_metadata(frame)?
        }))
    }

    unsafe fn free(_core: &sys::Core, instance: Self::Instance, frame: &mut Self::RawFrame) {
        instance
            .metadata
            .NDIlib_send_free_metadata(instance.instance, frame);
    }
}

/// The single inventory of typed receiver capture operations, consumed by the
/// ordinary, KVM and async surfaces so their kinds and names stay in step.
macro_rules! capture_method_kinds {
    ($generate:ident) => {
        $generate! {
            (VideoMode, VideoKind, VideoFrame, VideoFrameRef,
                capture_video, capture_video_ref, capture_video_with, "video"),
            (AudioMode, AudioKind, AudioFrame, AudioFrameRef,
                capture_audio, capture_audio_ref, capture_audio_with, "audio"),
            (MetadataMode, MetadataKind, MetadataFrame, MetadataFrameRef,
                capture_metadata, capture_metadata_ref, capture_metadata_with, "metadata"),
        }
    };
}
pub(crate) use capture_method_kinds;

/// Free strategy for video frames captured through [`FrameSync`](crate::FrameSync).
///
/// Frees via `NDIlib_framesync_free_video`.
#[derive(Debug)]
pub(crate) struct FrameSyncVideoFree;

// SAFETY: This strategy pairs its raw frame and instance types with the
// matching NDIlib_framesync_free_video release through the owning runtime. Its
// embedded metadata retains the bounded first-NUL extent until that release.
unsafe impl FrameFree for FrameSyncVideoFree {
    type Instance = NDIlib_framesync_instance_t;
    type RawFrame = NDIlib_video_frame_v2_t;
    const REF_DEBUG_NAME: &'static str = "FrameSyncVideoRef";

    unsafe fn free(core: &sys::Core, instance: Self::Instance, frame: &mut Self::RawFrame) {
        core.NDIlib_framesync_free_video(instance, frame);
    }
}

/// Free strategy for audio frames captured through [`FrameSync`](crate::FrameSync).
///
/// Frees via `NDIlib_framesync_free_audio_v2`.
#[derive(Debug)]
pub(crate) struct FrameSyncAudioFree;

// SAFETY: This strategy pairs its raw frame and instance types with the
// matching NDIlib_framesync_free_audio_v2 release through the owning runtime. Its
// embedded metadata retains the bounded first-NUL extent until that release.
unsafe impl FrameFree for FrameSyncAudioFree {
    type Instance = NDIlib_framesync_instance_t;
    type RawFrame = NDIlib_audio_frame_v3_t;
    const REF_DEBUG_NAME: &'static str = "FrameSyncAudioRef";
    const AUDIO_TIMESTAMP_UNDEFINED: i64 = 0;

    unsafe fn free(core: &sys::Core, instance: Self::Instance, frame: &mut Self::RawFrame) {
        #[cfg(test)]
        crate::framesync::audio_test_calls::free();
        core.NDIlib_framesync_free_audio_v2(instance, frame);
    }
}

/// Free strategy for audio borrowed from [`AvSync`](crate::advanced::AvSync).
#[cfg(feature = "advanced")]
#[derive(Debug)]
pub(crate) struct AvSyncAudioFree;

/// The native owner and already-authorized table of one AV-sync audio loan.
#[cfg(feature = "advanced")]
#[derive(Clone, Copy)]
pub(crate) enum AvSyncAudioInstance {
    #[cfg_attr(
        not(ndi_has_advanced_av_sync),
        expect(
            dead_code,
            reason = "native construction requires Advanced AV-sync declarations"
        )
    )]
    Native {
        #[cfg(ndi_has_advanced_av_sync)]
        handle: crate::sys::advanced::AvSyncHandle,
        table: &'static crate::sys::advanced::AvSync,
    },
    #[cfg(test)]
    Synthetic(*const std::cell::Cell<usize>),
}

// SAFETY: each instance carries the synchronizer and the same table that
// returned its descriptor. The owner borrow outlives matching native release,
// retaining embedded metadata through its first NUL or the metadata cap.
#[cfg(feature = "advanced")]
unsafe impl FrameFree for AvSyncAudioFree {
    type Instance = AvSyncAudioInstance;
    type RawFrame = NDIlib_audio_frame_v3_t;
    const REF_DEBUG_NAME: &'static str = "AvSyncAudioRef";
    // Advanced 6.3.2 and 6.4.0.2 return zero for the no-source audio
    // descriptor, and sender timestamps for matched samples. The native
    // avsync timestamp regression and synthetic normalization tests pin both.
    const AUDIO_TIMESTAMP_UNDEFINED: i64 = 0;

    unsafe fn free(_core: &sys::Core, instance: Self::Instance, _frame: &mut Self::RawFrame) {
        match instance {
            AvSyncAudioInstance::Native {
                #[cfg(ndi_has_advanced_av_sync)]
                handle,
                table,
            } => {
                crate::sys::advanced::native!(AvSync, table => {
                    if !_frame.p_data.is_null() || !_frame.p_metadata.is_null() {
                        table.NDIlib_avsync_free_audio(handle, _frame);
                    }
                });
            }
            #[cfg(test)]
            AvSyncAudioInstance::Synthetic(_) => {
                unreachable!("synthetic guards use their test free")
            }
        }
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
/// (a receiver or synchronizer), preventing use-after-free by ensuring the
/// owner cannot be dropped while this guard is alive.
///
/// # Type Parameters
///
/// - `'owner`: Lifetime of the owning instance's borrow
/// - `S`: The free strategy (video/audio/metadata, receiver or FrameSync)
///
/// # Safety
///
/// Its unsafe constructors establish the delivered-frame invariant: every
/// non-null pointer retains the initialized active storage and readable metadata
/// described by the frame, unchanged until matching release. Empty descriptors
/// may have null pointers. Only `new` and the test-only `with_test_free` create
/// guards; safe borrowed views rely on this invariant.
pub(crate) struct Guard<'owner, S: FrameFree> {
    instance: S::Instance,
    frame: S::RawFrame,
    // The release names the strategy only in unit tests, whose synthetic
    // variant is the one that uses it.
    #[cfg(not(test))]
    release: Release<'owner>,
    #[cfg(test)]
    release: Release<'owner, S>,
    // Synchronizer buffers stay on their capture thread. Receiver
    // strategies explicitly opt in below using the SDK's cross-thread free
    // guarantee, rather than raw-pointer auto traits from generated bindings.
    _thread_affine: PhantomData<Rc<()>>,
}

/// How a [`Guard`] releases its frame when it is dropped.
///
/// Outside unit tests the only variant is [`Release::Native`], so every
/// guard frees its frame through the SDK.
enum Release<'owner, #[cfg(test)] S: FrameFree> {
    /// Through the free strategy's SDK call, in the runtime the frame was
    /// captured through. The runtime is borrowed from the owner, so the owner
    /// cannot be dropped while the frame is outstanding.
    Native(&'owner Ndi),
    /// Through a unit test's callback, which owns the synthetic frame.
    #[cfg(test)]
    Synthetic(TestFrameFree<S>),
}

// Outside unit tests the release state is the runtime reference alone, as
// large as the `Option<&Ndi>` it replaced, so a guard is no larger.
#[cfg(not(test))]
const _: () = {
    use std::mem::{align_of, size_of};
    assert!(size_of::<Release<'static>>() == size_of::<Option<&'static Ndi>>());
    assert!(align_of::<Release<'static>>() == align_of::<Option<&'static Ndi>>());
};

impl<'owner, S: FrameFree> Guard<'owner, S> {
    /// Create a new frame guard.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - `instance` is a valid instance for the free strategy `S`, created
    ///   through `runtime`
    /// - `frame` belongs to the matching capture/free family `S` and has not
    ///   been freed. Until this guard releases it, every non-null pointer
    ///   describes live storage valid for that descriptor: active pixel rows
    ///   or channel samples are initialized, and metadata is readable through
    ///   its first NUL or `MAX_METADATA_BYTES`. Standalone metadata with a
    ///   positive declared length retains that complete readable extent;
    ///   length zero uses the same bounded NUL rule. For `SenderMetadataFree`,
    ///   the SDK text is instead readable through its first NUL within
    ///   `MAX_METADATA_BYTES`, independently of the descriptor's declared count
    ///   (which can exclude that terminator). The borrowed view validates that
    ///   count without treating it as an initialized storage extent.
    ///   Padding need not be initialized.
    /// - the owning instance and all described storage remain valid and
    ///   unchanged for `'owner`; tie this lifetime to a borrow of that owner.
    ///   Null request descriptors and empty synchronization results are valid.
    pub(crate) unsafe fn new(
        runtime: &'owner Ndi,
        instance: S::Instance,
        frame: S::RawFrame,
    ) -> Self {
        Self {
            instance,
            frame,
            release: Release::Native(runtime),
            _thread_affine: PhantomData,
        }
    }

    /// Wrap a synthetic unit-test frame with an explicit local release callback.
    /// Real captures use `new` and the native free strategy.
    ///
    /// # Safety
    ///
    /// The descriptor obeys the same delivered-frame storage invariant as
    /// [`Self::new`]: every non-null pointer retains its described initialized
    /// active extent and bounded metadata, unchanged for `'owner`.
    /// `free` must accept this synthetic instance and frame, including on
    /// validation failure or panic, without releasing independently owned data.
    /// It must also honor the strategy's threading guarantees: receiver guards
    /// may be dropped from another thread.
    #[cfg(test)]
    pub(crate) unsafe fn with_test_free(
        instance: S::Instance,
        frame: S::RawFrame,
        free: TestFrameFree<S>,
    ) -> Self {
        Self {
            instance,
            frame,
            release: Release::Synthetic(free),
            _thread_affine: PhantomData,
        }
    }

    /// Get a reference to the underlying raw frame.
    pub(crate) fn frame(&self) -> &S::RawFrame {
        &self.frame
    }
}

// SAFETY: NDI-RECV explicitly allows captured video, audio, and metadata frames
// to be freed on another thread. Each guard exclusively owns its captured frame;
// shared references expose immutable SDK storage until the guard is dropped.
// The owner borrow keeps the thread-safe Receiver alive throughout this period.
// https://docs.ndi.video/all/developing-with-ndi/sdk/ndi-recv
unsafe impl Send for Guard<'_, VideoKind> {}
unsafe impl Sync for Guard<'_, VideoKind> {}
unsafe impl Send for Guard<'_, AudioKind> {}
unsafe impl Sync for Guard<'_, AudioKind> {}
unsafe impl Send for Guard<'_, MetadataKind> {}
unsafe impl Sync for Guard<'_, MetadataKind> {}

// SAFETY: NDI-SEND's upstream-metadata section refers to the receive-device
// model, and NDI-RECV permits received metadata to be freed on another thread:
// https://docs.ndi.video/all/developing-with-ndi/sdk/ndi-send
// https://docs.ndi.video/all/developing-with-ndi/sdk/ndi-recv
// SenderMetadataGuard::drop retains the capture lease through matching
// sender free. Shared access exposes only immutable held text, and the
// owner's borrow retains the sender and runtime through release.
unsafe impl Send for Guard<'_, SenderMetadataFree> {}
unsafe impl Sync for Guard<'_, SenderMetadataFree> {}

impl<'owner, S: FrameFree> Drop for Guard<'owner, S> {
    fn drop(&mut self) {
        match self.release {
            // SAFETY: `new` requires the instance and frame to be valid and to
            // belong to this runtime.
            Release::Native(runtime) => unsafe {
                S::free(&runtime.api().core, self.instance, &mut self.frame);
            },
            // SAFETY: with_test_free requires this callback to own release of
            // the synthetic descriptor, including failed validation and unwind.
            #[cfg(test)]
            Release::Synthetic(free) => unsafe { free(self.instance, &mut self.frame) },
        }
    }
}

/// Result of a generic capture operation: the frame `NDIlib_recv_capture_v3`
/// was asked for, or what it reported instead.
pub(crate) enum CaptureResult<'rx, K: CaptureKind> {
    /// Successfully captured a frame of the expected type.
    Frame(Guard<'rx, K>),
    /// No frame for the caller; see [`Notification`].
    Notification(Notification),
}

/// What `NDIlib_recv_capture_v3` reports in place of a frame the caller asked
/// for. None of these carries a frame buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Notification {
    /// Nothing arrived within the timeout. A frame type whose slot the caller
    /// left null is reported the same way, because no frame was delivered.
    None,
    /// The connection's status or the source's capabilities changed.
    StatusChanged,
    /// The receiver is now connected to a different source.
    SourceChanged,
    /// A frame type this crate does not know, with its native value.
    Unknown(u32),
}

impl Notification {
    /// Classify a frame type `NDIlib_recv_capture_v3` returned without a frame
    /// for the caller.
    ///
    /// # Errors
    ///
    /// [`Error::ConnectionLost`] for an error frame, which
    /// `Processing.NDI.Recv.h` documents as a lost connection.
    pub(crate) fn classify(frame_type: NDIlib_frame_type_e) -> Result<Self> {
        match frame_type {
            sys::bindings::NDIlib_frame_type_e_NDIlib_frame_type_none
            | sys::bindings::NDIlib_frame_type_e_NDIlib_frame_type_video
            | sys::bindings::NDIlib_frame_type_e_NDIlib_frame_type_audio
            | sys::bindings::NDIlib_frame_type_e_NDIlib_frame_type_metadata => Ok(Self::None),
            sys::bindings::NDIlib_frame_type_e_NDIlib_frame_type_status_change => {
                Ok(Self::StatusChanged)
            }
            sys::bindings::NDIlib_frame_type_e_NDIlib_frame_type_source_change => {
                Ok(Self::SourceChanged)
            }
            sys::bindings::NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::ConnectionLost)
            }
            value => Ok(Self::Unknown(value)),
        }
    }
}

/// The media descriptors of one `NDIlib_recv_capture_v3` call: `Some` for
/// each slot the call is given, `None` for each it leaves null.
///
/// Only this module makes one, and only to hand it to `Captured::capture`.
pub(crate) struct Slots {
    video: Option<NDIlib_video_frame_v2_t>,
    audio: Option<NDIlib_audio_frame_v3_t>,
    metadata: Option<NDIlib_metadata_frame_t>,
}

impl Slots {
    /// No media slots, so the SDK can deliver notifications alone.
    fn none() -> Self {
        Self {
            video: None,
            audio: None,
            metadata: None,
        }
    }

    /// The slot of kind `K` alone, so that the SDK can deliver no frame of
    /// another kind, which nothing would take and free.
    fn only<K: CaptureStrategy>() -> Self {
        let mut slots = Self::none();
        *K::slot(&mut slots) = Some(K::RawFrame::default());
        slots
    }

    /// All three media slots.
    fn all() -> Self {
        Self {
            video: Some(NDIlib_video_frame_v2_t::default()),
            audio: Some(NDIlib_audio_frame_v3_t::default()),
            metadata: Some(NDIlib_metadata_frame_t::default()),
        }
    }
}

/// What one `NDIlib_recv_capture_v3` call under a [`CaptureLock`] returned:
/// the frame type it reported, and the descriptors it was given.
///
/// Only [`Self::capture`] makes one, straight from that call, so a
/// descriptor it holds was given to the call, which filled it when the frame
/// type is that descriptor's kind and left it alone otherwise. A caller
/// [takes](Self::take) every kind it gave a slot: a frame left in its slot
/// would never be freed.
struct Captured<'rx> {
    runtime: &'rx Ndi,
    instance: NDIlib_recv_instance_t,
    frame_type: NDIlib_frame_type_e,
    slots: Slots,
}

impl<'rx> Captured<'rx> {
    /// Capture under `lock`, within what is left of its attempt, giving the
    /// SDK the descriptors of `slots`.
    fn capture(lock: &CaptureLock<'rx>, mut slots: Slots) -> Self {
        let frame_type = lock.capture(
            slots.video.as_mut(),
            slots.audio.as_mut(),
            slots.metadata.as_mut(),
        );
        Self {
            runtime: lock.runtime(),
            instance: lock.instance(),
            frame_type,
            slots,
        }
    }

    /// The guard of the frame of kind `K` this capture delivered, or `None`
    /// when it delivered none of that kind; the one place a receiver guard
    /// is made.
    fn take<K: CaptureStrategy>(&mut self) -> Option<Guard<'rx, K>> {
        if self.frame_type != K::FRAME_TYPE {
            return None;
        }
        let frame = K::slot(&mut self.slots).take()?;
        // SAFETY: the SDK reported a frame of kind `K` and was given `K`'s
        // descriptor, so it wrote that frame there (Processing.NDI.Recv.h:
        // it fills only the descriptor of the kind it returns). `capture`
        // made that call on `instance`, created through `runtime`, and
        // taking the descriptor out of its slot wraps the frame once. The
        // lock borrowed the receiver for 'rx, as `runtime` and the guard do,
        // so the instance outlives the guard, which frees the frame exactly
        // once through the matching NDIlib_recv_free_* function.
        Some(unsafe { Guard::new(self.runtime, self.instance, frame) })
    }

    /// What the capture reported in place of a frame a caller took.
    ///
    /// # Errors
    ///
    /// As for [`Notification::classify`].
    fn notification(&self) -> Result<Notification> {
        Notification::classify(self.frame_type)
    }
}

/// Capture a frame of kind `K` from the receiver and within the remaining
/// attempt identified by the [`CaptureLock`] proof `lock`.
///
/// Gives `NDIlib_recv_capture_v3` the slot of kind `K` alone; any other
/// result is classified by [`Notification::classify`]. The frame's guard
/// borrows the receiver for `'rx`, as the proof does, and can outlive the proof.
pub(crate) fn capture_raw<'rx, K: CaptureKind>(
    lock: &CaptureLock<'rx>,
) -> Result<CaptureResult<'rx, K>> {
    let mut captured = Captured::capture(lock, Slots::only::<K>());
    match captured.take::<K>() {
        Some(guard) => Ok(CaptureResult::Frame(guard)),
        None => captured.notification().map(CaptureResult::Notification),
    }
}

/// A media frame delivered by a capture with all three media slots. A
/// consumer of this type cannot receive or discard a native notification.
pub(crate) enum Media<'rx> {
    /// A video frame.
    Video(Guard<'rx, VideoKind>),
    /// An audio frame.
    Audio(Guard<'rx, AudioKind>),
    /// A metadata frame.
    Metadata(Guard<'rx, MetadataKind>),
}

/// A sealed set of media slots, paired with exactly the media it can deliver.
/// Only [`AllSlots`] and [`NoSlots`] implement it, so every descriptor given
/// to the SDK is taken and guarded when the SDK reports its frame kind.
pub(crate) trait SlotSet: sealed::Sealed + Sized {
    type Media<'rx>;

    fn capture<'rx>(lock: &CaptureLock<'rx>) -> Result<Delivered<'rx, Self>>;
}

/// Capture video, audio, metadata and notifications.
pub(crate) enum AllSlots {}

impl SlotSet for AllSlots {
    type Media<'rx> = Media<'rx>;

    fn capture<'rx>(lock: &CaptureLock<'rx>) -> Result<Delivered<'rx, Self>> {
        let mut captured = Captured::capture(lock, Slots::all());
        Ok(if let Some(frame) = captured.take::<VideoKind>() {
            Delivered::Media(Media::Video(frame))
        } else if let Some(frame) = captured.take::<AudioKind>() {
            Delivered::Media(Media::Audio(frame))
        } else if let Some(frame) = captured.take::<MetadataKind>() {
            Delivered::Media(Media::Metadata(frame))
        } else {
            Delivered::Notification(captured.notification()?)
        })
    }
}

/// Capture notifications alone, without requesting any media buffers.
pub(crate) enum NoSlots {}

/// A no-slot capture cannot deliver a media frame. Its converter is an
/// exhaustive empty match, without unreachable media or free paths.
pub(crate) enum NoMedia {}

impl SlotSet for NoSlots {
    type Media<'rx> = NoMedia;

    fn capture<'rx>(lock: &CaptureLock<'rx>) -> Result<Delivered<'rx, Self>> {
        let captured = Captured::capture(lock, Slots::none());
        Ok(Delivered::Notification(captured.notification()?))
    }
}

/// What one capture with slot set `S` delivered: media for that set, or what
/// the SDK reported in its place. For [`NoSlots`], the media is uninhabited.
pub(crate) enum Delivered<'rx, S: SlotSet> {
    Media(S::Media<'rx>),
    Notification(Notification),
}

/// Capture with slot set `S` from the receiver and within the remaining
/// attempt identified by the [`CaptureLock`] proof `lock`.
///
/// Any delivered media is wrapped in the guard of its kind, as
/// [`capture_raw`] does for one kind; any other result is classified by
/// [`Notification::classify`]. Every slot set goes through [`Captured`].
///
/// # Errors
///
/// As for [`Notification::classify`].
pub(crate) fn capture_any<'rx, S: SlotSet>(lock: &CaptureLock<'rx>) -> Result<Delivered<'rx, S>> {
    S::capture(lock)
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
    fn synthetic_frame_release_runs_once_on_drop_rejection_and_unwind() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        unsafe fn count_release(
            instance: NDIlib_recv_instance_t,
            _frame: &mut NDIlib_video_frame_v2_t,
        ) {
            // SAFETY: the test keeps this counter alive through every guard.
            unsafe { &*instance.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
        }

        let releases = AtomicUsize::new(0);
        let instance = (&releases as *const AtomicUsize).cast_mut().cast();
        let make_guard = || unsafe {
            Guard::<VideoKind>::with_test_free(
                instance,
                NDIlib_video_frame_v2_t::default(),
                count_release,
            )
        };

        drop(make_guard());
        assert_eq!(releases.load(Ordering::SeqCst), 1);

        let rejected = VideoFrameRef::new(make_guard());
        assert!(rejected.is_err());
        assert_eq!(releases.load(Ordering::SeqCst), 2);
        drop(rejected);
        assert_eq!(releases.load(Ordering::SeqCst), 2);

        let unwound = std::panic::catch_unwind(|| {
            let _guard = make_guard();
            panic!("synthetic fixture assertion failure");
        });
        assert!(unwound.is_err());
        assert_eq!(releases.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn frame_types_without_a_frame_are_classified_once() {
        for (frame_type, expected) in [
            (
                NDIlib_frame_type_e_NDIlib_frame_type_none,
                Notification::None,
            ),
            (
                NDIlib_frame_type_e_NDIlib_frame_type_video,
                Notification::None,
            ),
            (
                NDIlib_frame_type_e_NDIlib_frame_type_audio,
                Notification::None,
            ),
            (
                NDIlib_frame_type_e_NDIlib_frame_type_metadata,
                Notification::None,
            ),
            (
                NDIlib_frame_type_e_NDIlib_frame_type_status_change,
                Notification::StatusChanged,
            ),
            (
                NDIlib_frame_type_e_NDIlib_frame_type_source_change,
                Notification::SourceChanged,
            ),
            (987, Notification::Unknown(987)),
        ] {
            assert_eq!(Notification::classify(frame_type).unwrap(), expected);
        }
        let error = Notification::classify(NDIlib_frame_type_e_NDIlib_frame_type_error);
        assert!(matches!(&error, Err(Error::ConnectionLost)), "{error:?}");
    }

    /// A single-kind capture is given no other slot: the SDK could fill one,
    /// and only the requested kind's frame is taken and freed.
    #[test]
    fn a_capture_is_given_only_the_slots_it_takes_frames_from() {
        fn given(slots: &Slots) -> [bool; 3] {
            [
                slots.video.is_some(),
                slots.audio.is_some(),
                slots.metadata.is_some(),
            ]
        }
        assert_eq!(given(&Slots::only::<VideoKind>()), [true, false, false]);
        assert_eq!(given(&Slots::only::<AudioKind>()), [false, true, false]);
        assert_eq!(given(&Slots::only::<MetadataKind>()), [false, false, true]);
        assert_eq!(given(&Slots::all()), [true, true, true]);
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
