//! NDI sending functionality for video, audio, and metadata.

use std::{
    ffi::{CStr, CString},
    fmt,
    marker::PhantomData,
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use crate::{
    finder::Source,
    frames::{
        AudioSource, MetadataFrame, MetadataSource, MetadataText, PixelFormat, ScanType,
        SendableVideoLayout, ValidatedVideoLayout, VideoFrame, VideoHeader, VideoParts,
        VideoSource,
    },
    pixels::{send_room, Pixels},
    receiver::Tally,
    sys::{self, bindings::*},
    timeout::Deadline,
    to_ms_checked, Error, FrameRate, Ndi, Rejected, Result, Timecode,
};

#[cfg(not(target_has_atomic = "ptr"))]
compile_error!(
    "This crate requires atomic pointer support. Please use a target with atomics enabled."
);

/// Flush the async video pipeline by passing a true NULL pointer.
///
/// The NDI SDK documentation specifies that calling
/// `NDIlib_send_send_video_async_v2(instance, NULL)` (where the *frame pointer*
/// is NULL, not merely a frame whose `p_data` is NULL) waits for any in-flight
/// async buffer to be released and then returns.
fn async_flush_frame_ptr() -> *const NDIlib_video_frame_v2_t {
    ptr::null()
}

/// The handle of a native sender: the one value through which this crate
/// reaches an SDK sender, and the one place where the sender types' thread
/// safety rests on the SDK's contract rather than on their Rust fields.
///
/// A copy is a borrowed handle. [`SenderResource`] owns the sender and
/// destroys it.
#[derive(Clone, Copy, Debug)]
struct SendInstance(NDIlib_send_instance_t);

// SAFETY: the handle is an opaque pointer with no thread affinity, and the SDK
// lets one sender be used from several threads at once. Its documentation of
// sending (the NDI-Send section) allows video, audio and metadata to be sent
// at any time, from any thread and in any order, and `Processing.NDI.Send.h`
// allows for audio and video submitted from separate threads when it
// describes their clocking. Nothing requires a sender to be destroyed on the
// thread that created it. A copy grants nothing by itself: every use is an
// `unsafe` SDK call made through an owner that keeps the sender alive, and
// `SenderResource` destroys it only when the last owner is released, so no
// call overlaps `NDIlib_send_destroy`. The operations this crate does not
// overlap are serialized by the Rust state around the handle, whose own types
// decide the auto traits of the types that hold it: the ownership and
// synchronization of asynchronous video (`VideoPipeline` and the
// `in_flight` slots), metadata capture with its matching free
// (`Inner::metadata_capture`), and the Advanced queue's completion state,
// which `QueuedSender` registers before it returns.
unsafe impl Send for SendInstance {}
// SAFETY: as for `Send`.
unsafe impl Sync for SendInstance {}

/// A live native sender and the entry points of the runtime that created
/// it: what every SDK call on a sender is made through.
///
/// A copy is a borrowed handle, valid while an owner of the native sender
/// (`Shared` or `Exclusive`) keeps it alive.
#[derive(Clone, Copy)]
pub(crate) struct SendHandle {
    instance: SendInstance,
    core: &'static sys::Core,
}

/// The handle of a sender, and the lock that pairs a metadata capture with
/// its free.
struct Inner {
    // A copy of the handle that the sender's `NativeOwner` owns.
    instance: SendInstance,
    // The entry points of the runtime that created `instance`.
    core: &'static sys::Core,
    metadata_capture: parking_lot::Mutex<()>,
    tally: TallyState,
    #[cfg(ndi_has_advanced_keyframe_demand)]
    keyframe_changes: crate::keyframe_changes::KeyframeChanges,
}

// A panic cannot leave metadata capture or tally half-changed: their locks
// guard native queries, and no application code runs while held. Those locks
// do not poison or declare this themselves, which would otherwise leave
// senders short of `RefUnwindSafe`. The change feed uses unwind-safe standard locks.
impl std::panic::RefUnwindSafe for Inner {}

/// Polling must preserve the SDK's consumable change report for a later wait.
#[derive(Default)]
struct TallyState {
    query: parking_lot::Mutex<()>,
    value: AtomicU8,
    pending: AtomicBool,
}

impl TallyState {
    fn current(&self) -> Tally {
        let bits = self.value.load(Ordering::Acquire);
        Tally::new(bits & 1 != 0, bits & 2 != 0)
    }

    fn remember(&self, tally: Tally) {
        self.value.store(
            u8::from(tally.on_program()) | (u8::from(tally.on_preview()) << 1),
            Ordering::Release,
        );
    }
}

mod ownership {
    use super::*;

    /// The owner of a native sender, which destroys it when dropped:
    /// advertisements retain one without access to sends.
    ///
    /// Nominally public only so that [`NativeOwner`] can name it; its implementation stays private.
    pub struct SenderResource {
        pub(super) instance: SendInstance,
        pub(super) _name: CString,
        pub(super) _groups: Option<CString>,
        #[cfg(feature = "advanced")]
        pub(super) _config: Option<crate::advanced::AdvancedConfig>,
        pub(super) _ndi: Ndi,
    }

    impl SenderResource {
        /// The native sender this token keeps alive.
        pub(crate) fn instance(&self) -> NDIlib_send_instance_t {
            self.instance.0
        }
    }

    impl Drop for SenderResource {
        fn drop(&mut self) {
            // SAFETY: this token is the instance's final owner.
            unsafe { self._ndi.api().core.NDIlib_send_destroy(self.instance.0) };
        }
    }

    /// How a [`Sender`] owns its native sender: [`Shared`] or `Exclusive`, as
    /// the associated type of its sealed mode trait says.
    ///
    /// The trait and both owners are nominally public, so that the sealed mode
    /// trait can name them, but their implementation stays private.
    pub trait NativeOwner: Send + Sync + fmt::Debug {
        /// Own a native sender that was just created.
        fn new(resource: SenderResource) -> Self;
    }

    /// The native sender of a [`RawSend`] sender, which advertiser registrations
    /// may share, and the asynchronous video pipeline that sends through it.
    pub struct Shared {
        // Declared first, so that it is dropped first: dropping the pipeline
        // drains the SDK while `owner` still keeps the native sender alive.
        pub(super) video: VideoPipeline,
        pub(super) owner: Arc<SenderResource>,
    }

    impl NativeOwner for Shared {
        fn new(resource: SenderResource) -> Self {
            let sender = SendHandle {
                instance: resource.instance,
                core: &resource._ndi.api().core,
            };
            Self {
                video: VideoPipeline::new(sender),
                owner: Arc::new(resource),
            }
        }
    }

    impl fmt::Debug for Shared {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Shared").finish_non_exhaustive()
        }
    }

    /// The native sender of a sender in one of the modes the Advanced senders
    /// hold, which nothing else owns: dropping it destroys the native sender.
    ///
    /// A sender is created in such a mode, or converted into one only when no
    /// registration shares its native sender (`Sender::into_attached`), and
    /// such a mode hands out no other owner. So the frames an Advanced sender
    /// keeps for the SDK are never freed while a registration keeps its native
    /// sender alive.
    #[cfg(feature = "advanced")]
    pub struct Exclusive(pub(super) SenderResource);

    #[cfg(feature = "advanced")]
    impl NativeOwner for Exclusive {
        fn new(resource: SenderResource) -> Self {
            Self(resource)
        }
    }

    #[cfg(feature = "advanced")]
    impl fmt::Debug for Exclusive {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Exclusive").finish_non_exhaustive()
        }
    }
}

#[cfg(feature = "advanced")]
use ownership::Exclusive;
pub(crate) use ownership::SenderResource;
use ownership::{NativeOwner, Shared};

mod mode_sealed {
    /// The sealed half of [`SenderMode`](super::SenderMode).
    pub trait Sealed {
        /// How a sender in this mode owns its native sender, and what it
        /// keeps beside it: [`Shared`](super::Shared) for the mode that sends
        /// uncompressed video, `Exclusive` for the modes
        /// the Advanced senders hold.
        type Native: super::NativeOwner;
    }
}

/// Sealed sender capability mode.
///
/// Every mode has the operations that do not depend on how video is sent:
/// the source, metadata in both directions, tally, connection count and
/// metadata, failover, and the Advanced encoder guidance, keyframe and tally
/// controls. [`RawSend`] alone sends uncompressed video; the modes that also
/// send uncompressed audio implement [`AudioSendMode`].
pub trait SenderMode: mode_sealed::Sealed + Send + Sync {}

/// Sealed sender modes that send uncompressed audio with
/// [`Sender::send_audio`]: [`RawSend`] and the queued mode.
pub trait AudioSendMode: SenderMode {}

/// Ordinary sender mode: uncompressed video, including the owned asynchronous
/// pipeline, audio and metadata. Only a sender in this mode can be registered
/// with a [`SendAdvertiser`](crate::discovery::SendAdvertiser).
#[derive(Debug)]
pub enum RawSend {}

/// Sender mode held exclusively by
/// [`CompressedSender`](crate::advanced::CompressedSender), which sends the
/// video and audio itself.
#[cfg(feature = "advanced")]
#[derive(Debug)]
pub enum CompressedAttached {}

/// Sender mode held exclusively by
/// [`QueuedSender`](crate::advanced::QueuedSender), which sends the video
/// itself. It sends uncompressed audio.
#[cfg(feature = "advanced")]
#[derive(Debug)]
pub enum QueueAttached {}

impl mode_sealed::Sealed for RawSend {
    type Native = Shared;
}
impl SenderMode for RawSend {}
impl AudioSendMode for RawSend {}
#[cfg(feature = "advanced")]
impl mode_sealed::Sealed for CompressedAttached {
    type Native = Exclusive;
}
#[cfg(feature = "advanced")]
impl SenderMode for CompressedAttached {}
#[cfg(feature = "advanced")]
impl mode_sealed::Sealed for QueueAttached {
    type Native = Exclusive;
}
#[cfg(feature = "advanced")]
impl SenderMode for QueueAttached {}
#[cfg(feature = "advanced")]
impl AudioSendMode for QueueAttached {}

/// An NDI source that sends video, audio and metadata.
///
/// Create one with [`Sender::new`]. Dropping the sender completes any
/// in-flight asynchronous video and removes the source from the network,
/// unless a [`SendAdvertiser`](crate::discovery::SendAdvertiser) registration
/// still holds it: a registered sender stays on the network until that
/// registration is removed or its advertiser is dropped.
///
/// The default mode, [`RawSend`], has every operation. The Advanced
/// `CompressedSender` and `QueuedSender` hold their sender in another
/// [`SenderMode`] and lend it through their `sender` method: it keeps the
/// operations every mode shares, but not the video sends its owner makes
/// itself, and it cannot be registered with an advertiser.
///
/// A sender is `Send` and `Sync`. Its operations take `&self` and may run on
/// several threads at once, except asynchronous video submissions, which
/// take `&mut self`. Tally polls preserve changes for a waiting thread;
/// concurrent tally waits serialize and each change is consumed once.
#[derive(Debug)]
pub struct Sender<M: SenderMode = RawSend> {
    inner: Inner,
    // The owner of the native sender, with the video pipeline of a mode that
    // sends uncompressed video.
    native: <M as mode_sealed::Sealed>::Native,
    _ndi: Ndi,
    _mode: PhantomData<M>,
}

/// The video pipeline of a sender that sends uncompressed video: the frame
/// the SDK may still be reading, or the one it last released, the copy a
/// synchronous send makes of a frame it cannot send in place.
///
/// [`Shared`] owns it beside the native sender and drops it first, so the
/// pipeline drains the SDK while `sender` is still live.
struct VideoPipeline {
    sender: SendHandle,
    state: Mutex<VideoState>,
}

/// What a sender's video operations share, behind one lock.
#[derive(Default)]
struct VideoState {
    /// The frame an asynchronous send lent the SDK.
    in_flight: InFlight<VideoFrame>,
    /// The pixels of the last borrowed frame that did not
    /// [send in place](BorrowedVideoFrame::sends_in_place), which the SDK
    /// read from here instead. Empty until such a frame is sent, then kept
    /// for the next one.
    staging: Pixels,
    /// A released frame kept for the next asynchronous copy. This is never
    /// the allocation the SDK is still reading from `in_flight`.
    spare: Option<VideoFrame>,
}

impl VideoPipeline {
    fn new(sender: SendHandle) -> Self {
        Self {
            sender,
            state: Mutex::new(VideoState::default()),
        }
    }

    /// Lock the video state and make one synchronizing SDK call through `call`.
    fn synchronize<R>(&self, call: impl FnOnce(&mut VideoState, SendHandle) -> R) -> R {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        call(&mut state, self.sender)
    }

    /// Send `frame` synchronously, which also releases the frame in flight.
    fn send(&self, frame: &(impl VideoSource + ?Sized), authorized: SendableVideoLayout) {
        self.synchronize(|state, sender| {
            // The SDK reads the copy only during the send, under this lock.
            let frame = PreparedVideo::authorized(frame, &mut state.staging, authorized);
            // A synchronous video send also releases the previous async frame,
            // which completes once the send has returned.
            let synchronized = state.in_flight.send(sender, frame);
            state.in_flight.completed(synchronized);
        });
    }

    /// Hand a checked frame to the SDK and return the previous one, if any.
    fn submit(&self, frame: VideoFrame, authorized: SendableVideoLayout) -> Option<VideoFrame> {
        self.synchronize(|state, sender| {
            // The pipeline owns the new frame before the SDK sees it. The
            // previous frame stays owned by `displaced` until the submission,
            // which synchronizes the SDK's access to it, has returned; only
            // the proof of that return completes it.
            let (mut displaced, synchronized) = state.in_flight.submit(sender, frame, authorized);
            displaced.completed(synchronized);
            displaced.take_retained()
        })
    }

    /// Copy into released storage, then retain the displaced frame for reuse.
    fn copy(&self, frame: &(impl VideoSource + ?Sized), authorized: SendableVideoLayout) {
        self.synchronize(|state, sender| {
            let owned = match state.spare.take() {
                Some(mut spare) => {
                    frame.copy_into_owned(&mut spare);
                    spare
                }
                None => frame.owned_copy(),
            };
            let (mut displaced, synchronized) = state.in_flight.submit(sender, owned, authorized);
            // Only the submission's return proves that the SDK has released
            // the displaced storage. A later copy may refill it after this.
            displaced.completed(synchronized);
            state.spare = displaced.take_retained();
        });
    }

    /// Wait for the SDK to release the frame in flight, which stays retained.
    fn flush(&self) {
        self.synchronize(|state, sender| drain(&mut state.in_flight, sender));
    }

    /// Drain the SDK and take the retained frame.
    fn finish(&self) -> Option<VideoFrame> {
        self.synchronize(|state, sender| {
            drain(&mut state.in_flight, sender);
            state.spare = None;
            state.in_flight.take_retained()
        })
    }

    /// End the pipeline: drain the SDK and take the retained frame.
    #[cfg(all(feature = "advanced", ndi_has_advanced_send_completion))]
    fn retire(mut self) -> Option<VideoFrame> {
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        drain(&mut state.in_flight, self.sender);
        state.in_flight.take_retained()
        // Dropping the idle pipeline makes no SDK call.
    }
}

impl Drop for VideoPipeline {
    /// Drain the SDK while destruction holds the state exclusively.
    fn drop(&mut self) {
        // The native sender may outlive this pipeline when an advertiser
        // registration shares it, so the pipeline drains the SDK itself
        // rather than relying on the sender's destruction.
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        drain(&mut state.in_flight, self.sender);
    }
}

// SAFETY: The descriptor borrows owned initialized pixels, scan-dependent SDK
// tail, and terminated FrameMetadata. Moving the frame moves none of those
// allocations, which InFlight keeps inaccessible until the SDK releases them.
unsafe impl in_flight::Lend for VideoFrame {
    type Authorization = SendableVideoLayout;

    fn lend(&mut self, authorized: Self::Authorization) -> in_flight::Lent {
        in_flight::Lent::Frame(self.to_raw(authorized))
    }
}

/// Synchronize the SDK if it may still read `state`'s frame, then complete
/// it: the release a flush, a finish and the pipeline's destruction share.
fn drain(state: &mut InFlight<VideoFrame>, sender: SendHandle) {
    if !state.is_in_flight() {
        // A retained frame was completed when the SDK released it.
        return;
    }
    let synchronized = state.flush(sender);
    state.completed(synchronized);
}

/// Frames an asynchronous video send lends to the SDK, and the SDK calls that
/// end the loan.
///
/// `Processing.NDI.Send.h` lets the SDK keep reading an asynchronous frame
/// until a later video send (asynchronous or synchronous, including a null
/// synchronous frame) or sender destruction synchronizes it. The [NDI-Send]
/// documentation additionally specifies the null asynchronous flush used here.
/// `Processing.NDI.Advanced.h` gives scatter submission the same lifetime rules.
/// A [`CompressedSender`](crate::advanced::CompressedSender) conservatively
/// retains each stream's frame until a replacement on that stream returns,
/// even when another stream's video send has already synchronized the SDK.
///
/// [`InFlight`] holds one stream's lent frame. It hands the frame back only in
/// exchange for [`in_flight::Synchronized`], the proof that the SDK has returned from a
/// synchronizing call made after the frame was submitted, or [`Destroyed`],
/// the proof that its native sender no longer exists. Both proofs have
/// private fields, so the rest of the crate obtains one only from the
/// functions here, each of which returns it after the SDK call has returned.
///
/// Each synchronizing call takes the slot by `&mut`, so its caller holds the
/// slot, by a lock or by `&mut`: one slot's calls never overlap. Each takes
/// `sender`, the live sender whose stream the slot holds. Dropping a slot
/// drops whatever it holds, so its owner ends the SDK's access first: a
/// [`VideoPipeline`] drains the SDK when it is dropped, before [`Shared`]
/// releases the native sender, and a `CompressedSender` destroys its native
/// sender before it drops its streams. An owner that is never dropped leaks
/// its frames rather than freeing them.
///
/// [NDI-Send]: https://docs.ndi.video/all/developing-with-ndi/sdk/ndi-send
pub(crate) mod in_flight {
    use std::mem;

    #[cfg(feature = "advanced")]
    use super::Exclusive;
    use super::{async_flush_frame_ptr, PreparedVideo, SendHandle};
    #[cfg(all(feature = "advanced", ndi_has_advanced_scatter_send))]
    use crate::sys::advanced::{FrameScatter, ScatterSend};
    use crate::{sys::bindings::NDIlib_video_frame_v2_t, VideoFrame};

    /// Proof that the SDK has returned from a call that synchronizes a
    /// stream's asynchronous video: an asynchronous or synchronous video
    /// send, or a null flush. `Processing.NDI.Send.h` lists subsequent video
    /// sends; [NDI-Send] additionally documents the null asynchronous flush.
    /// When the call returns, the SDK no longer reads the previous frame.
    ///
    /// [`InFlight::completed`] consumes it, so a frame is released only after
    /// the call that ended the SDK's access to it.
    ///
    /// [NDI-Send]: https://docs.ndi.video/all/developing-with-ndi/sdk/ndi-send
    #[must_use = "the proof releases the frame the SDK synchronized"]
    pub(crate) struct Synchronized(());

    /// Proof that a native sender has been destroyed, which ends the SDK's
    /// access to every frame it was lent, on every stream.
    ///
    /// [`destroy`] makes it; [`InFlight::into_released`] takes it by
    /// reference, since it releases any number of the destroyed sender's
    /// slots.
    #[cfg(feature = "advanced")]
    #[must_use = "the proof releases the frames of the destroyed sender"]
    pub(crate) struct Destroyed(());

    /// A frame an asynchronous video send can lend to the SDK.
    ///
    /// # Safety
    ///
    /// The descriptors [`lend`](Self::lend) returns point only into
    /// allocations the frame owns, which moving the frame does not move: the
    /// SDK goes on using them after the frame has moved into its slot, and
    /// the slot gives no access to a frame in flight.
    pub(crate) unsafe trait Lend {
        /// The descriptors that lend this frame to the SDK.
        type Authorization;

        fn lend(&mut self, authorization: Self::Authorization) -> Lent;
    }

    /// What an asynchronous video send hands the SDK.
    pub(crate) enum Lent {
        /// A frame descriptor, which the SDK reads during the call, for
        /// `NDIlib_send_send_video_async_v2`.
        Frame(NDIlib_video_frame_v2_t),
        /// A frame descriptor without data, and the scatter list of the
        /// frame's blocks, for `NDIlib_send_send_video_scatter_async`. No
        /// header says the SDK copies the list, so the frame owns it too.
        #[cfg(all(feature = "advanced", ndi_has_advanced_scatter_send))]
        Scatter {
            table: &'static ScatterSend,
            frame: NDIlib_video_frame_v2_t,
            scatter: *const FrameScatter,
        },
    }

    /// One stream's asynchronously sent frame, and whether the SDK may still
    /// read it.
    ///
    /// A frame in flight always exists, so the slot cannot say "in flight"
    /// without the allocation the SDK is reading. A frame leaves the in-flight
    /// state only through [`Self::completed`], which takes the proof that the
    /// SDK has synchronized, by being displaced by [`Self::submit`], which
    /// hands it back only with that proof, or through
    /// [`Self::into_released`], which takes the proof that the sender was
    /// destroyed. Only a retained frame can be taken out. The state is
    /// private to this module, so no other code can reach a frame in flight.
    pub(crate) struct InFlight<T>(State<T>);

    #[derive(Default)]
    enum State<T> {
        /// No frame is held.
        #[default]
        Idle,
        /// The SDK has released the frame; it awaits recovery.
        Retained(T),
        /// The SDK may still be reading the frame.
        InFlight(T),
    }

    impl<T> Default for InFlight<T> {
        fn default() -> Self {
            Self(State::Idle)
        }
    }

    impl<T> InFlight<T> {
        pub(crate) fn is_in_flight(&self) -> bool {
            matches!(self.0, State::InFlight(_))
        }

        /// Mark the frame in flight as released, returning it. Returns `None`
        /// when no frame was in flight.
        ///
        /// `_synchronized` proves that the SDK has returned from a
        /// synchronizing call on this slot's stream made after the frame was
        /// submitted.
        pub(crate) fn completed(&mut self, _synchronized: Synchronized) -> Option<&T> {
            match mem::take(&mut self.0) {
                State::InFlight(frame) => {
                    self.0 = State::Retained(frame);
                    self.retained()
                }
                other => {
                    self.0 = other;
                    None
                }
            }
        }

        /// Release the frame `synchronized` proves the SDK has finished with,
        /// and take it, or the frame already retained.
        #[cfg(feature = "advanced")]
        pub(crate) fn release(mut self, synchronized: Synchronized) -> Option<T> {
            let _ = self.completed(synchronized);
            self.take_retained()
        }

        /// Remove the retained frame, leaving the slot idle. A frame in
        /// flight stays held: it can be taken only once it has completed.
        pub(crate) fn take_retained(&mut self) -> Option<T> {
            match mem::take(&mut self.0) {
                State::Retained(frame) => Some(frame),
                other => {
                    self.0 = other;
                    None
                }
            }
        }

        /// The frame, whatever its state, once the native sender it was lent
        /// to has been destroyed.
        #[cfg(feature = "advanced")]
        pub(crate) fn into_released(self, _destroyed: &Destroyed) -> Option<T> {
            match self.0 {
                State::Idle => None,
                State::Retained(frame) | State::InFlight(frame) => Some(frame),
            }
        }

        fn retained(&self) -> Option<&T> {
            match &self.0 {
                State::Retained(frame) => Some(frame),
                _ => None,
            }
        }

        #[cfg(test)]
        pub(crate) fn frame(&self) -> Option<&T> {
            match &self.0 {
                State::Idle => None,
                State::Retained(frame) | State::InFlight(frame) => Some(frame),
            }
        }
    }

    impl<T: Lend> InFlight<T> {
        /// Make `next` the frame in flight and lend it to the SDK. Returns
        /// the state it displaced, whose frame the SDK has released, with the
        /// proof of that release.
        pub(crate) fn submit(
            &mut self,
            sender: SendHandle,
            mut next: T,
            authorization: T::Authorization,
        ) -> (Self, Synchronized) {
            let lent = next.lend(authorization);
            // The slot owns the new frame before the SDK sees it. Moving the
            // frame does not move what `lent` points to (the contract of
            // `Lend`).
            let displaced = Self(mem::replace(&mut self.0, State::InFlight(next)));
            match lent {
                // SAFETY: `sender` is the live sender whose stream this slot
                // holds. The SDK reads the descriptor during the call, so it
                // may be a stack value. It reads what the descriptor points
                // to after the call returns, until the next synchronizing
                // event on this stream: the frame in flight owns it, and
                // leaves the slot only with a later call's proof or the
                // sender's destruction, while the slot's owner ends the SDK's
                // access before dropping it. Until this call returns the SDK
                // may still read the displaced frame: `displaced` owns it
                // across the call and is handed back only with the proof the
                // call produces, so it cannot be dropped first.
                Lent::Frame(frame) => unsafe {
                    sender
                        .core
                        .NDIlib_send_send_video_async_v2(sender.instance.0, &frame)
                },
                #[cfg(all(feature = "advanced", ndi_has_advanced_scatter_send))]
                Lent::Scatter {
                    table,
                    frame,
                    scatter,
                } => {
                    // SAFETY: as for a contiguous frame. The scatter
                    // submission follows the rules of the asynchronous send
                    // (`Processing.NDI.Advanced.h`), and what it reads after
                    // the call, the frame's metadata, the scatter list at
                    // `scatter`, the pointer and size arrays it refers to
                    // and the blocks they name, all belong to the frame in
                    // flight.
                    unsafe {
                        table.NDIlib_send_send_video_scatter_async(
                            sender.instance.0,
                            &frame,
                            scatter,
                        )
                    }
                }
            }
            (displaced, Synchronized(()))
        }
    }

    impl InFlight<VideoFrame> {
        /// Wait for the SDK to release the frame in flight by passing a true
        /// NULL frame pointer, the SDK's documented request to drain the
        /// asynchronous buffer.
        pub(crate) fn flush(&mut self, sender: SendHandle) -> Synchronized {
            // SAFETY: `sender` is the live sender whose stream this slot
            // holds, and a null frame pointer is the SDK's documented request
            // to drain the async buffer.
            unsafe {
                sender
                    .core
                    .NDIlib_send_send_video_async_v2(sender.instance.0, async_flush_frame_ptr())
            };
            Synchronized(())
        }

        /// Send `frame` synchronously. When this returns, the SDK has
        /// finished reading `frame` and has released the frame in flight.
        pub(crate) fn send(
            &mut self,
            sender: SendHandle,
            frame: PreparedVideo<'_>,
        ) -> Synchronized {
            let raw = frame.raw;
            // SAFETY: `sender` is the live sender whose stream this slot
            // holds. `raw` points into pixels and metadata that `frame`
            // borrows for longer than this call, and the pixels are followed
            // by the room the SDK reads after a frame of their format, which
            // the caller checked or made by staging the frame; the SDK has
            // finished reading them when the synchronous send returns and
            // keeps no reference afterwards.
            unsafe {
                sender
                    .core
                    .NDIlib_send_send_video_v2(sender.instance.0, &raw)
            };
            Synchronized(())
        }
    }

    /// Destroy `sender`'s native sender, which nothing else owns in its mode,
    /// and return the proof that the SDK no longer reads any frame it was
    /// lent.
    ///
    /// `Processing.NDI.Send.h` lists destruction among the synchronizing
    /// events, and the Advanced SDK documentation has `NDIlib_send_destroy`
    /// deliver every outstanding completion before it returns.
    #[cfg(feature = "advanced")]
    pub(crate) fn destroy<M>(sender: super::Sender<M>) -> Destroyed
    where
        M: super::SenderMode + super::mode_sealed::Sealed<Native = Exclusive>,
    {
        let super::Sender {
            native: Exclusive(resource),
            ..
        } = sender;
        // The only owner: dropping it calls `NDIlib_send_destroy`.
        drop(resource);
        Destroyed(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A frame in flight comes out of its slot only with a proof, and is
        /// completed only once.
        #[test]
        fn a_slot_releases_its_frame_only_with_proof() {
            let mut slot = InFlight(State::InFlight(1u8));
            assert!(slot.take_retained().is_none());
            assert!(slot.is_in_flight());
            assert_eq!(slot.completed(Synchronized(())), Some(&1));
            assert!(!slot.is_in_flight());
            assert_eq!(slot.completed(Synchronized(())), None);
            assert_eq!(slot.take_retained(), Some(1));
            assert!(slot.take_retained().is_none());
            assert!(slot.frame().is_none());
        }

        #[cfg(feature = "advanced")]
        #[test]
        fn a_displaced_or_destroyed_slot_hands_back_what_it_held() {
            let release = |state| InFlight(state).release(Synchronized(()));
            assert_eq!(release(State::InFlight(1u8)), Some(1));
            assert_eq!(release(State::Retained(2)), Some(2));
            assert_eq!(release(State::Idle), None);
            let destroyed = Destroyed(());
            let unsent = |state| InFlight(state).into_released(&destroyed);
            assert_eq!(unsent(State::InFlight(3u8)), Some(3));
            assert_eq!(unsent(State::Retained(4)), Some(4));
            assert_eq!(unsent(State::Idle), None);
        }
    }
}

use in_flight::InFlight;

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner")
            .field("instance", &self.instance.0)
            .finish_non_exhaustive()
    }
}

/// A borrowed video frame that references external pixel data.
///
/// [`Sender::send_video`] sends it synchronously; the buffer can be reused
/// as soon as the call returns. [`Sender::send_video_async_copy`] copies its
/// pixels and metadata into an owned frame for the asynchronous pipeline.
///
/// The x86 builds of the SDK read a little past the end of many UYVY and
/// UYVA frames. Where [`PixelFormat::send_tail_room`] is nonzero, borrowed
/// frames of these formats are copied into packed storage with a zeroed tail
/// by default, so caller padding and trailing bytes cannot affect the picture.
/// [`BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail`] opts into
/// direct sending when sufficient initialized memory follows the pixels.
/// [`Self::sends_in_place`] reports whether a copy is needed.
///
/// Create one with [`Self::builder`]. Construction borrows without
/// allocation and validates the entire supported uncompressed layout once.
/// Copying it copies only the borrows.
#[derive(Clone, Copy)]
pub struct BorrowedVideoFrame<'buf> {
    layout: ValidatedVideoLayout,
    header: VideoHeader,
    /// Exactly the validated storage extent.
    data: &'buf [u8],
    /// The initialized bytes, borrowed for `'buf` as `data` is, between the
    /// frame's last active byte and the end of the buffer `data` is a prefix
    /// of: the last row's padding and whatever the buffer holds after the
    /// frame.
    tail_room: usize,
    /// Caller opted in, or this is crate-owned storage with a zeroed tail.
    allow_padding_and_tail: bool,
    /// The metadata text, validated with its terminator.
    metadata: Option<MetadataText<'buf>>,
}

// SAFETY: Construction checks the initialized byte slice covers the validated
// layout; its borrow also retains validated terminated metadata unchanged.
// sends_in_place checks the initialized stride/tail extent before direct SDK
// reads. The default descriptor never points outside those borrowed buffers.
unsafe impl crate::frames::VideoParts for BorrowedVideoFrame<'_> {
    #[inline]
    fn layout(&self) -> &ValidatedVideoLayout {
        &self.layout
    }
    #[inline]
    fn header(&self) -> &VideoHeader {
        &self.header
    }
    #[inline]
    fn pixels(&self) -> *const u8 {
        self.data.as_ptr()
    }
    #[inline]
    fn metadata_text(&self) -> Option<MetadataText<'_>> {
        self.metadata
    }
    /// A frame built for sending carries no timestamp; [`Self::to_raw`]
    /// sends none.
    #[inline]
    fn sdk_timestamp(&self) -> Option<crate::SdkTimestamp> {
        None
    }

    fn can_send_direct(&self) -> bool {
        self.sends_in_place()
    }
}

impl VideoSource for BorrowedVideoFrame<'_> {}

/// A descriptor borrowing either a proven direct input or initialized packed
/// staging. Its lifetime keeps both input metadata and staging alive until the
/// synchronous native call returns. It never forms slices over capture padding.
pub(crate) struct PreparedVideo<'a> {
    raw: NDIlib_video_frame_v2_t,
    _borrow: PhantomData<&'a ()>,
}

impl<'a> PreparedVideo<'a> {
    #[cfg(test)]
    fn new(frame: &'a (impl VideoParts + ?Sized), staging: &'a mut Pixels) -> Self {
        Self::authorized(
            frame,
            staging,
            SendableVideoLayout::for_test(*frame.layout()),
        )
    }

    fn authorized(
        frame: &'a (impl VideoParts + ?Sized),
        staging: &'a mut Pixels,
        authorized: SendableVideoLayout,
    ) -> Self {
        let raw = if frame.can_send_direct() {
            frame.video_descriptor(authorized)
        } else {
            let packed = authorized.packed();
            let room = send_room(packed.layout(), frame.header().scan_type);
            frame.copy_active_pixels(staging, packed.layout(), room);
            crate::frames::sdk_video_frame(
                packed,
                frame.header(),
                staging.as_ptr(),
                frame
                    .metadata_text()
                    .map_or(ptr::null(), MetadataText::as_ptr),
                frame.sdk_timestamp(),
            )
        };
        Self {
            raw,
            _borrow: PhantomData,
        }
    }
}

crate::frames::video_accessors! {
    impl['buf] BorrowedVideoFrame<'buf>;
    debug("BorrowedVideoFrame", "data (bytes)" = data_len_bytes);
    timecode {}
    line_stride_bytes {
        /// It is the stride the frame was built with, which can include row
        /// padding.
    }
    plane {
        /// Its rows exclude the padding of the frame's line stride, which
        /// [`Self::data`] includes, so the active pixels of a padded buffer
        /// can be read in place.
    }
    images("", "\
 # use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
 # let pixels = vec![128; 64 * 36 * 2];
 # let frame = BorrowedVideoFrame::builder(&pixels, 64, 36, PixelFormat::Uyvy).build()?;") {
        /// It encodes the application's own buffer in place, padded rows
        /// included, so a program can keep a still of what it sends without
        /// copying the frame.
    }
}

impl<'buf> BorrowedVideoFrame<'buf> {
    /// Configure a checked borrowed frame over `data`.
    ///
    /// `data` may be longer than the frame: the frame is its start, and
    /// [`Self::data`] never includes the rest. For UYVY/UYVA where the SDK
    /// needs tail room, the default send copies active rows only, even when
    /// `data` includes extra room. Direct sending requires an explicit
    /// [`BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail`] opt-in.
    ///
    /// Its defaults are the packed line stride, 60/1 frames per second,
    /// square pixels (picture aspect ratio `0.0`), progressive scan, [`Timecode::SYNTHESIZE`]
    /// and no metadata. All validation occurs in
    /// [`BorrowedVideoFrameBuilder::build`].
    ///
    /// # Example
    ///
    /// ```
    /// # use grafton_ndi::{BorrowedVideoFrame, FrameRate, PixelFormat};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let buffer = vec![0u8; 1920 * 1080 * 4]; // BGRA buffer
    /// let frame = BorrowedVideoFrame::builder(&buffer, 1920, 1080, PixelFormat::Bgra)
    ///     .frame_rate(FrameRate::new(30_000, 1_001)?)
    ///     .build()?;
    /// assert_eq!(frame.line_stride_bytes(), 1920 * 4);
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder(
        data: &'buf [u8],
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
    ) -> BorrowedVideoFrameBuilder<'buf> {
        BorrowedVideoFrameBuilder {
            data,
            width,
            height,
            pixel_format,
            line_stride: None,
            frame_rate: crate::media::DEFAULT_VIDEO_FRAME_RATE,
            picture_aspect_ratio: VideoHeader::SQUARE_PIXELS,
            scan_type: ScanType::Progressive,
            timecode: Timecode::SYNTHESIZE,
            metadata: None,
            readable_after: 0,
            allow_padding_and_tail: false,
        }
    }

    /// Copy this borrowed frame into a validated owned frame.
    ///
    /// Active pixels are copied into tightly packed planes; row padding is
    /// skipped and the owned stride can shrink. Metadata is copied separately;
    /// no reference to the source is retained. [`Self::copy_into`] makes the
    /// same copy into an existing frame, reusing its allocations.
    ///
    /// The copy cannot fail: building the frame validated everything it
    /// relies on, and the packed layout of a valid frame is valid.
    pub fn to_owned(&self) -> VideoFrame {
        crate::frames::VideoParts::owned_copy(self)
    }

    /// Copy this borrowed frame into `destination`, reusing its allocations.
    ///
    /// `destination` ends up exactly as [`Self::to_owned`] would have
    /// returned it, whatever it held before: tightly packed active rows,
    /// format, dimensions, frame rate, aspect ratio, scan type, timecode and
    /// metadata, and no timestamp. No reference to the source is retained.
    ///
    /// Its pixel buffer is reused when its capacity holds the packed frame
    /// and the SDK tail room required by its format and scan type, including
    /// extra strides for interlaced UYVY/UYVA when needed. Otherwise it is
    /// replaced by one allocation of exactly that size. Its metadata string
    /// is reused when its capacity holds the new text and its terminator;
    /// otherwise it grows once. With sufficient pixel and metadata capacity,
    /// subsequent copies allocate nothing. The copy makes no SDK call and can
    /// run on any thread. Copies without metadata retain the destination's
    /// text allocation for later frames.
    ///
    /// # Example
    ///
    /// A copy-and-send loop that stops allocating once two frames exist.
    /// [`Sender::send_video_async`] keeps the frame it is given until the next
    /// send, and returns the previous one, which the SDK has finished with:
    /// that frame is the destination of the next copy.
    /// [`Sender::send_video_async_copy`] keeps its own reusable frames for
    /// this copy instead.
    ///
    /// ```no_run
    /// # use grafton_ndi::{BorrowedVideoFrame, Ndi, PixelFormat, Sender, SenderOptions, VideoFrame};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// let mut sender = Sender::new(&ndi, &SenderOptions::new("Copied"))?;
    /// // 64 pixels per row, padded to a 512-byte stride.
    /// let mut buffer = vec![0u8; 512 * 32];
    /// let mut spare: Option<VideoFrame> = None;
    /// for shade in 0..=255u8 {
    ///     buffer.fill(shade); // the application draws into its own buffer
    ///     let frame = BorrowedVideoFrame::builder(&buffer, 64, 32, PixelFormat::Bgra)
    ///         .line_stride_bytes(512)
    ///         .build()?;
    ///     let owned = match spare.take() {
    ///         Some(mut reused) => {
    ///             frame.copy_into(&mut reused); // no allocation
    ///             reused
    ///         }
    ///         None => frame.to_owned(), // the first two frames only
    ///     };
    ///     spare = sender.send_video_async(owned)?;
    /// }
    /// sender.finish_video_async();
    /// # Ok(())
    /// # }
    /// ```
    pub fn copy_into(&self, destination: &mut VideoFrame) {
        crate::frames::VideoParts::copy_into_owned(self, destination);
    }

    /// Get the pixel data: exactly the frame's validated storage, including
    /// any row padding. Bytes of the source buffer beyond the frame are not
    /// part of it. [`Self::plane`] and [`Self::planes`] borrow the active
    /// rows alone.
    pub fn data(&self) -> &[u8] {
        self.data
    }

    /// Whether [`Sender::send_video`] sends this frame from its buffer,
    /// without copying its pixels.
    ///
    /// UYVY and UYVA frames are copied by default wherever
    /// [`PixelFormat::send_tail_room`] is nonzero. Opting in with
    /// [`BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail`] allows
    /// direct sending if enough room follows the last pixel. The last row's
    /// padding, the bytes of the
    /// buffer after the frame, and bytes vouched for with
    /// [`BorrowedVideoFrameBuilder::readable_after`] all count towards the
    /// room after that opt-in. An interlaced or single-field frame of those
    /// formats whose height is not a multiple of four needs two line strides more.
    ///
    /// Where the SDK reads frames exactly, which is 64-bit ARM Linux, the
    /// room is zero and every frame sends in place.
    ///
    /// # Example
    ///
    /// ```
    /// # use grafton_ndi::{BorrowedVideoFrame, PixelFormat, VideoFrame};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let uyvy = PixelFormat::Uyvy;
    /// let room = uyvy.send_tail_room();
    /// let exact = vec![0u8; 720 * 480 * 2];
    /// let frame = BorrowedVideoFrame::builder(&exact, 720, 480, uyvy).build()?;
    /// // Copied wherever the SDK needs room after the frame.
    /// assert_eq!(frame.sends_in_place(), room == 0);
    ///
    /// let longer = vec![0u8; 720 * 480 * 2 + room];
    /// let default = BorrowedVideoFrame::builder(&longer, 720, 480, uyvy).build()?;
    /// assert_eq!(default.sends_in_place(), room == 0);
    /// let in_place = BorrowedVideoFrame::builder(&longer, 720, 480, uyvy)
    ///     .allow_send_from_padding_and_tail(true)
    ///     .build()?;
    /// assert!(in_place.sends_in_place());
    /// assert_eq!(in_place.data().len(), 720 * 480 * 2);
    ///
    /// let owned = VideoFrame::builder()
    ///     .resolution(720, 480)
    ///     .pixel_format(uyvy)
    ///     .build()?;
    /// assert!(grafton_ndi::VideoSource::sends_in_place(&owned));
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn sends_in_place(&self) -> bool {
        let room = send_room(&self.layout, self.header.scan_type);
        room == 0 || (self.allow_padding_and_tail && self.tail_room >= room)
    }

    /// Return this frame with `timecode`.
    ///
    /// # Example
    ///
    /// ```
    /// # use grafton_ndi::{BorrowedVideoFrame, PixelFormat, Timecode};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let buffer = vec![0u8; 1920 * 1080 * 2];
    /// let frame = BorrowedVideoFrame::builder(&buffer, 1920, 1080, PixelFormat::Uyvy)
    ///     .build()?
    ///     .with_timecode(Timecode::from_ticks(10_000_000));
    /// assert_eq!(frame.timecode().ticks(), 10_000_000);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_timecode(mut self, timecode: Timecode) -> Self {
        self.header.timecode = timecode;
        self
    }

    #[cfg(test)]
    fn to_raw(self) -> NDIlib_video_frame_v2_t {
        self.video_descriptor(SendableVideoLayout::for_test(*self.layout()))
    }
}

/// Builder for a [`BorrowedVideoFrame`], reached through
/// [`BorrowedVideoFrame::builder`]. No pixels or metadata are copied.
#[derive(Clone, Copy)]
pub struct BorrowedVideoFrameBuilder<'buf> {
    data: &'buf [u8],
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    line_stride: Option<usize>,
    frame_rate: FrameRate,
    picture_aspect_ratio: f32,
    scan_type: ScanType,
    timecode: Timecode,
    metadata: Option<&'buf CStr>,
    /// Readable bytes the caller vouches for after `data`.
    readable_after: usize,
    allow_padding_and_tail: bool,
}

impl<'buf> BorrowedVideoFrameBuilder<'buf> {
    /// Set the first-plane stride in bytes. Zero selects the packed stride.
    #[must_use]
    pub fn line_stride_bytes(mut self, stride: usize) -> Self {
        self.line_stride = Some(stride);
        self
    }

    /// Reset the first-plane stride override to the packed stride.
    #[must_use]
    pub fn clear_line_stride_bytes(mut self) -> Self {
        self.line_stride = None;
        self
    }
    /// Set the frame rate.
    #[must_use]
    pub fn frame_rate(mut self, frame_rate: FrameRate) -> Self {
        self.frame_rate = frame_rate;
        self
    }
    /// Set a finite nonnegative aspect ratio; zero selects square pixels.
    #[must_use]
    pub fn picture_aspect_ratio(mut self, ratio: f32) -> Self {
        self.picture_aspect_ratio = ratio;
        self
    }
    /// Select progressive, interlaced, or field scanning.
    #[must_use]
    pub fn scan_type(mut self, scan_type: ScanType) -> Self {
        self.scan_type = scan_type;
        self
    }
    /// Set the timecode.
    #[must_use]
    pub fn timecode(mut self, timecode: Timecode) -> Self {
        self.timecode = timecode;
        self
    }
    /// Borrow NUL-terminated UTF-8 metadata, validated when built.
    #[must_use]
    pub fn metadata(mut self, metadata: &'buf CStr) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Remove the optional metadata override.
    #[must_use]
    pub fn clear_metadata(mut self) -> Self {
        self.metadata = None;
        self
    }

    /// Allow direct sending from caller padding and trailing memory.
    ///
    /// Defaults to `false`. Where UYVY/UYVA sending needs tail room, the
    /// default path packs active rows and zeroes the tail. Setting this to
    /// `true` permits sending directly if the supplied buffer (plus any
    /// [`Self::readable_after`] room) is large enough. The SDK may encode row
    /// padding and trailing bytes into the picture: only opt in when their
    /// contents are suitable for transmission. All such bytes must be
    /// initialized and stable throughout the send. Insufficient room still
    /// causes a packed copy. Formats with zero tail room send directly either way.
    #[must_use]
    pub fn allow_send_from_padding_and_tail(mut self, allow: bool) -> Self {
        self.allow_padding_and_tail = allow;
        self
    }

    /// Declare that `bytes` bytes of initialized memory follow the builder's
    /// buffer.
    ///
    /// They count towards the [room](PixelFormat::send_tail_room) the SDK
    /// needs after the frame, so that a buffer which ends with its frame and
    /// cannot be made longer, such as one another library allocated with
    /// padding, is [sent in place](BorrowedVideoFrame::sends_in_place)
    /// instead of copied, only after explicitly opting in with
    /// [`Self::allow_send_from_padding_and_tail`]. This declaration alone
    /// does not change the default copying policy. The SDK may read those
    /// bytes and compress them into the picture's last blocks. It writes nothing.
    ///
    /// A buffer the program allocates itself needs none of this: make it
    /// longer, and the frame borrows the room with the pixels.
    ///
    /// # Safety
    ///
    /// The `bytes` bytes that follow the last byte of the buffer given to
    /// [`BorrowedVideoFrame::builder`] must be readable memory for as long
    /// as that buffer is borrowed, initialized, and within the same live
    /// allocation as the buffer. Every byte the SDK may read, including row
    /// padding and trailing room, must be initialized and remain stable
    /// throughout the send; nothing may write it during that time.
    ///
    /// # Example
    ///
    /// ```
    /// # use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let format = PixelFormat::Uyvy;
    /// // A capture library's buffer: the frame, then its own padding.
    /// let capture = vec![0u8; 720 * 480 * 2 + 64];
    /// let pixels = &capture[..720 * 480 * 2];
    ///
    /// let builder = BorrowedVideoFrame::builder(pixels, 720, 480, format)
    ///     .allow_send_from_padding_and_tail(true);
    /// // SAFETY: `pixels` is followed by 64 more bytes of `capture`, which
    /// // outlives the frame; every byte is initialized and remains stable.
    /// let frame = unsafe { builder.readable_after(64) }.build()?;
    /// assert!(frame.sends_in_place());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub unsafe fn readable_after(mut self, bytes: usize) -> Self {
        self.readable_after = bytes;
        self
    }

    /// Validate dimensions, stride, initialized slice extent, headers, and metadata.
    ///
    /// Successful construction allocates nothing and never loads or queries
    /// an NDI runtime: whether the runtime can send the pixel format is
    /// checked when the frame is sent. The input slices remain borrowed.
    ///
    /// # Errors
    ///
    /// Invalid layout/header values, a buffer smaller than the layout, or
    /// metadata exceeding the 4 MiB cap return [`Error::InvalidFrame`];
    /// invalid metadata UTF-8 returns [`Error::InvalidUtf8`].
    pub fn build(self) -> Result<BorrowedVideoFrame<'buf>> {
        crate::frames::validate_picture_aspect_ratio(self.picture_aspect_ratio)?;
        let layout = ValidatedVideoLayout::new_uncompressed(
            self.pixel_format,
            self.width,
            self.height,
            self.line_stride,
        )?;
        let Some(data) = self.data.get(..layout.data_len_bytes()) else {
            return Err(Error::InvalidFrame(format!(
                "Buffer too small for {:?}: got {} bytes, expected at least {}",
                self.pixel_format,
                self.data.len(),
                layout.data_len_bytes(),
            )));
        };
        let metadata = self.metadata.map(MetadataText::from_cstr).transpose()?;
        Ok(BorrowedVideoFrame {
            layout,
            header: VideoHeader {
                frame_rate: self.frame_rate,
                picture_aspect_ratio: self.picture_aspect_ratio,
                scan_type: self.scan_type,
                timecode: self.timecode,
            },
            data,
            tail_room: (layout.trailing_padding() + (self.data.len() - data.len()))
                .saturating_add(self.readable_after),
            allow_padding_and_tail: self.allow_padding_and_tail,
            metadata,
        })
    }
}

impl<M: SenderMode> Sender<M> {
    /// Create a sender in this mode, through the creation path of
    /// [`Sender::new`]: the configured entry point when the options carry an
    /// Advanced configuration, the plain one otherwise.
    pub(crate) fn open(ndi: &Ndi, options: &SenderOptions) -> Result<Self> {
        #[cfg(feature = "advanced")]
        if let Some(config) = &options.config {
            return Self::new_configured(ndi, options, config);
        }
        // SAFETY: the SDK's sender creation returns null or a fresh sender
        // that nothing else owns, and the settings outlive the call.
        unsafe {
            Self::create(
                ndi,
                options,
                "NDIlib_send_create",
                #[cfg(feature = "advanced")]
                None,
                |core, raw| core.NDIlib_send_create(raw),
            )
        }
    }

    /// The one creation path: create a sender through `create`, the entry
    /// point `operation`, which is handed the core table and the native
    /// settings for `options`, and keep `config` for as long as the native
    /// sender lives. The new sender is owned as its mode owns one.
    ///
    /// # Safety
    ///
    /// `create` returns null or a live sender of `ndi`'s runtime, made from
    /// the settings it was handed, that nothing else owns.
    pub(crate) unsafe fn create(
        ndi: &Ndi,
        options: &SenderOptions,
        operation: &'static str,
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(&'static sys::Core, &NDIlib_send_create_t) -> NDIlib_send_instance_t,
    ) -> Result<Self> {
        let name = CString::new(options.name.as_str())?;
        let groups = options.groups.as_deref().map(CString::new).transpose()?;
        let core = &ndi.api().core;
        let instance = create(
            core,
            &NDIlib_send_create_t {
                p_ndi_name: name.as_ptr(),
                p_groups: groups.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
                clock_video: options.clock_video,
                clock_audio: options.clock_audio,
            },
        );
        if instance.is_null() {
            return Err(Error::NativeOperationFailed { operation });
        }
        let instance = SendInstance(instance);
        Ok(Self {
            inner: Inner {
                instance,
                core,
                metadata_capture: parking_lot::Mutex::new(()),
                tally: TallyState::default(),
                #[cfg(ndi_has_advanced_keyframe_demand)]
                keyframe_changes: crate::keyframe_changes::KeyframeChanges::default(),
            },
            native: NativeOwner::new(SenderResource {
                instance,
                _name: name,
                _groups: groups,
                #[cfg(feature = "advanced")]
                _config: config,
                _ndi: ndi.clone(),
            }),
            _ndi: ndi.clone(),
            _mode: PhantomData,
        })
    }
}

impl Sender {
    /// Creates a new NDI send instance.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] when the name or groups contain a
    /// NUL byte, [`Error::UnsupportedCapability`] when the options carry an
    /// Advanced configuration that the runtime or this build cannot take, and
    /// [`Error::NativeOperationFailed`] when the SDK creates no sender.
    pub fn new(ndi: &Ndi, options: &SenderOptions) -> Result<Self> {
        Self::open(ndi, options)
    }

    /// A new owner of the native sender, which keeps it alive after this
    /// value is dropped. Only an ordinary sender hands one out: the wrappers
    /// that hold a sender in another mode free what the SDK reads once their
    /// native sender is gone, so it must not outlive them.
    pub(crate) fn native_owner(&self) -> Arc<SenderResource> {
        Arc::clone(&self.native.owner)
    }

    /// The same native sender in mode `T`, which owns it exclusively, and the
    /// frame this sender's asynchronous video pipeline still held.
    ///
    /// The pipeline is retired: the SDK is drained, so nothing this sender
    /// submitted is in flight once the sender is in mode `T`, which has no
    /// pipeline. Returns the sender unchanged, with nothing synchronized,
    /// when an advertiser registration shares its native sender: the
    /// registration could keep it alive after the frames an Advanced sender
    /// lends the SDK are freed.
    #[cfg(all(feature = "advanced", ndi_has_advanced_send_completion))]
    // The refusal hands the sender back, so it is as large as the sender, as
    // the errors of the Advanced module that return their input are.
    #[allow(clippy::result_large_err)]
    pub(crate) fn into_attached<T>(
        self,
    ) -> std::result::Result<(Sender<T>, Option<VideoFrame>), Self>
    where
        T: SenderMode + mode_sealed::Sealed<Native = Exclusive>,
    {
        let Self {
            inner,
            native: Shared { video, owner },
            _ndi,
            _mode,
        } = self;
        match Arc::try_unwrap(owner) {
            Ok(resource) => {
                // `resource` keeps the native sender alive while the pipeline
                // drains it.
                let retained = video.retire();
                let sender = Sender {
                    inner,
                    native: Exclusive(resource),
                    _ndi,
                    _mode: PhantomData,
                };
                Ok((sender, retained))
            }
            Err(owner) => Err(Self {
                inner,
                native: Shared { video, owner },
                _ndi,
                _mode,
            }),
        }
    }

    /// Send a video frame synchronously.
    ///
    /// Accepts every [`VideoSource`]: owned frames, checked external buffers,
    /// receiver captures and FrameSync captures, all by shared reference.
    /// Borrowed UYVY/UYVA frames are copied into packed storage with a zeroed
    /// tail wherever [`PixelFormat::send_tail_room`] is nonzero, so padding
    /// and trailing bytes cannot affect the encoded picture. The sender
    /// reuses one staging allocation. Captures with padded rows also stage,
    /// since their guard guarantees only active pixels are initialized. Packed
    /// captures needing no trailing room send in place. Owned
    /// frames retain zeroed, scan-dependent tail room and always send in place.
    /// To permit direct sending from initialized caller padding
    /// and trailing memory, explicitly use
    /// [`BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail`];
    /// [`BorrowedVideoFrame::sends_in_place`] tells which a frame is.
    ///
    /// The SDK has released the input buffer when this returns; the caller may
    /// then reuse or release it. This also completes a previous async video send.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] if this sender's runtime
    /// cannot send the frame's pixel format (the planar 4:4:4 formats need
    /// [`RuntimeFeature::Planar444Video`](crate::RuntimeFeature::Planar444Video)).
    /// Nothing is sent and no pending send is changed.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{BorrowedVideoFrame, Ndi, PixelFormat, Sender, SenderOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let sender = Sender::new(&ndi, &SenderOptions::new("Padded"))?;
    /// // 64 pixels per row, padded to a 512-byte stride.
    /// let mut buffer = vec![0u8; 512 * 32];
    /// let frame = BorrowedVideoFrame::builder(&buffer, 64, 32, PixelFormat::Bgra)
    ///     .line_stride_bytes(512)
    ///     .build()?;
    /// sender.send_video(&frame)?;
    /// buffer.fill(0xff); // the SDK is done with the buffer
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_video(&self, frame: &(impl VideoSource + ?Sized)) -> Result<()> {
        let authorized = self.require_video_layout(*frame.layout())?;
        self.native.video.send(frame, authorized);
        Ok(())
    }

    /// Submit an owned frame without copying its pixel or metadata allocations.
    ///
    /// The sender takes ownership before calling the SDK. The next async send
    /// synchronizes the previous send and returns its owned frame for reuse.
    /// The first send returns `None`. The returned frame may have been submitted
    /// by ownership or copying. A copied submission instead retains the frame
    /// it displaces for its next copy. Call [`Self::finish_video_async`] to
    /// recover the final frame.
    ///
    /// Synchronous sends and explicit flushes complete the operation but retain
    /// the owned frame for recovery by the next async send or finish call.
    /// No SDK completion callback is registered, including with Advanced SDK.
    ///
    /// # Errors
    ///
    /// Returns the frame, unsent, with [`Error::UnsupportedCapability`] if
    /// this sender's runtime cannot send its pixel format (the planar 4:4:4
    /// formats need
    /// [`RuntimeFeature::Planar444Video`](crate::RuntimeFeature::Planar444Video)).
    /// No pending send is changed.
    pub fn send_video_async(
        &mut self,
        frame: VideoFrame,
    ) -> std::result::Result<Option<VideoFrame>, Rejected<VideoFrame>> {
        let authorized = match self.require_video_layout(*frame.layout()) {
            Ok(authorized) => authorized,
            Err(error) => return Err(Rejected::new(error, frame)),
        };
        Ok(self.native.video.submit(frame, authorized))
    }

    /// Drain the SDK and return the final owned frame, if any.
    ///
    /// Frames submitted through either owned or copied sending are returned.
    /// Any spare storage retained for copying is released. Repeated calls
    /// return `None`.
    pub fn finish_video_async(&mut self) -> Option<VideoFrame> {
        self.native.video.finish()
    }

    /// Copy borrowed pixels and metadata, then submit the owned copy asynchronously.
    ///
    /// The caller may immediately reuse or release the source buffer and metadata.
    /// The sender keeps the previous completed frame as spare storage for its
    /// next copy, whether that frame was submitted by ownership or copying.
    /// Once both frames have enough pixel and metadata capacity, repeated
    /// copies reuse their allocations. Larger layouts or longer metadata grow
    /// each buffer as needed.
    ///
    /// Recover the final copy with [`Self::finish_video_async`], which also
    /// releases the spare. Owned submissions through [`Self::send_video_async`]
    /// still return the frame they displace.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] if this sender's runtime
    /// cannot send the frame's pixel format, checked before anything is
    /// copied. No pending send is changed on failure.
    pub fn send_video_async_copy(&mut self, frame: &(impl VideoSource + ?Sized)) -> Result<()> {
        let authorized = self.require_video_layout(*frame.layout())?;
        self.native.video.copy(frame, authorized.packed());
        Ok(())
    }

    /// Flush pending async video operations synchronously.
    ///
    /// When the SDK may still read a pending frame, sends a null video frame
    /// pointer and blocks until that asynchronous operation completes. If no
    /// frame is in flight, returns without calling the SDK.
    ///
    /// After this returns, the SDK has released the pending buffer. The sender
    /// retains the frame; use [`Self::finish_video_async`] or an owned
    /// submission through [`Self::send_video_async`] to recover it without copying.
    pub fn flush_async_blocking(&self) {
        self.native.video.flush();
    }
}

impl<M: AudioSendMode> Sender<M> {
    /// Sends an audio frame synchronously.
    ///
    /// Accepts every [`AudioSource`], including receiver and FrameSync captures.
    /// Samples and metadata are submitted in place with the original channel
    /// stride; no intermediate planar copy or wrapper allocation is made.
    /// The SDK has released the input buffer when this returns, so the caller
    /// may then reuse or release it. This calls `NDIlib_send_send_audio_v3`.
    /// It is available in the modes that implement [`AudioSendMode`]: on an
    /// ordinary sender and on the one a `QueuedSender` lends.
    ///
    /// See the NDI SDK documentation section on `NDIlib_send_send_audio_v3` for more details.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, SenderOptions, AudioFrame};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let options = SenderOptions::new("Test");
    /// # let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    /// let mut audio_buffer = vec![0.0f32; 48000 * 2]; // 1 second of stereo audio
    ///
    /// // Fill buffer with audio data...
    /// let frame = AudioFrame::builder()
    ///     .sample_rate(48000)
    ///     .num_channels(2)
    ///     .num_samples(48000)
    ///     .data(audio_buffer.clone())
    ///     .build()?;
    /// sender.send_audio(&frame);
    ///
    /// // Buffer can be reused immediately
    /// audio_buffer.fill(0.5);
    /// let frame2 = AudioFrame::builder()
    ///     .sample_rate(48000)
    ///     .num_channels(2)
    ///     .num_samples(48000)
    ///     .data(audio_buffer)
    ///     .build()?;
    /// sender.send_audio(&frame2);
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_audio(&self, audio_frame: &(impl AudioSource + ?Sized)) {
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_send_audio_v3(self.inner.instance.0, &audio_frame.audio_descriptor());
        }
    }
}

impl<M: SenderMode> Sender<M> {
    pub(crate) fn native_instance(&self) -> NDIlib_send_instance_t {
        self.inner.instance.0
    }

    /// All selectors consume the SDK's same one-shot report, including a
    /// null selector beside a format-specific selector. Share the native wait
    /// across them and filter the current demand at each caller.
    #[cfg(ndi_has_advanced_keyframe_demand)]
    pub(crate) fn keyframe_changes(&self) -> &crate::keyframe_changes::KeyframeChanges {
        &self.inner.keyframe_changes
    }

    /// The handle the asynchronous video slots of this sender's owner send
    /// through.
    #[cfg(feature = "advanced")]
    pub(crate) fn handle(&self) -> SendHandle {
        SendHandle {
            instance: self.inner.instance,
            core: self.inner.core,
        }
    }

    /// The core entry points of the runtime that created this sender.
    #[cfg(feature = "advanced")]
    pub(crate) fn core(&self) -> &'static sys::Core {
        self.inner.core
    }

    pub(crate) fn runtime(&self) -> &Ndi {
        &self._ndi
    }

    /// Check that this sender's runtime can send `pixel_format`: the one
    /// check every video send makes before anything reaches the SDK.
    pub(crate) fn require_video_layout(
        &self,
        layout: ValidatedVideoLayout,
    ) -> Result<SendableVideoLayout> {
        SendableVideoLayout::new(layout, &self._ndi)
    }

    /// Capture metadata sent back by connected receivers.
    ///
    /// The result owns its data. The wait budget includes contention with other
    /// metadata captures and does not block the video-send synchronization lock.
    /// Captured text uses the SDK's guaranteed NUL termination, bounded to
    /// 4 MiB, rather than assuming every positive native length is the exact
    /// UTF-8 byte count including its terminator. Invalid descriptor counts and
    /// invalid UTF-8 still return an error, and every captured frame is freed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] when the runtime lacks the
    /// sender metadata capture functions, [`Error::InvalidInput`] when the
    /// timeout exceeds [`crate::MAX_TIMEOUT`], [`Error::InvalidFrame`] for an
    /// invalid metadata count, a positive count with null data, or a missing
    /// bounded terminator,
    /// [`Error::InvalidUtf8`] for non-UTF-8 text, and [`Error::CaptureFailed`]
    /// when the SDK returns a frame type other than metadata or no frame.
    pub fn capture_metadata(&self, timeout: Duration) -> Result<Option<MetadataFrame>> {
        let metadata = self._ndi.table::<sys::SenderMetadata>()?;
        let deadline = Deadline::after(timeout)?;
        let Some(_lock) = self.inner.metadata_capture.try_lock_until(deadline.end()) else {
            return Ok(None);
        };
        let mut raw = NDIlib_metadata_frame_t {
            length: 0,
            timecode: 0,
            p_data: ptr::null_mut(),
        };
        let remaining = deadline.remaining_ms();
        // SAFETY: the sender instance is live for this borrow and `raw` is
        // writable storage for one metadata descriptor.
        let kind =
            unsafe { metadata.NDIlib_send_capture(self.inner.instance.0, &mut raw, remaining) };
        if kind == NDIlib_frame_type_e_NDIlib_frame_type_metadata {
            /// Frees one frame that `NDIlib_send_capture` filled, through the
            /// sender and the runtime table that filled it, when dropped.
            struct Release {
                instance: NDIlib_send_instance_t,
                raw: NDIlib_metadata_frame_t,
                metadata: &'static sys::SenderMetadata,
            }
            impl Drop for Release {
                fn drop(&mut self) {
                    // SAFETY: a `Release` is built only below, right after
                    // `NDIlib_send_capture` reported that it filled `raw` on
                    // `instance` through `metadata`, the table of the runtime
                    // that created the sender. The guard never leaves this
                    // call, whose `&Sender` keeps the sender alive and whose
                    // capture lock is released only after the guard drops.
                    // This drop is the frame's only release: it is freed
                    // exactly once, through the same instance, after its last
                    // read.
                    unsafe {
                        self.metadata
                            .NDIlib_send_free_metadata(self.instance, &self.raw)
                    };
                }
            }
            // From here the guard owns the captured frame, so every exit frees
            // it: a validation error, a panic, or the owned copy's return.
            let guard = Release {
                instance: self.inner.instance.0,
                raw,
                metadata,
            };
            // SAFETY: `NDIlib_send_capture` filled `guard.raw` on this sender
            // and reported a metadata frame, which stays allocated until the
            // guard frees it. Processing.NDI.structs.h independently guarantees
            // `p_data` is a NUL-terminated UTF-8 string, and the SDK's
            // Send_VirtualPTZ example reads captured sender metadata as that
            // string, without using `length` as its byte extent. The bounded
            // scan relies on this native C-string contract for positive counts
            // too; generic raw descriptors retain their stricter validation.
            // The text borrows `guard.raw`, so it is copied before the guard
            // frees the original, unchanged descriptor.
            let text = unsafe { crate::frames::validate_sender_metadata(&guard.raw)? };
            let timecode = Timecode::from_ticks(guard.raw.timecode);
            Ok(Some(MetadataFrame::copied(text, timecode)))
        } else if kind == NDIlib_frame_type_e_NDIlib_frame_type_none {
            Ok(None)
        } else {
            Err(Error::CaptureFailed(format!(
                "sender metadata capture returned {kind}"
            )))
        }
    }

    /// Sends a metadata frame.
    ///
    /// Accepts owned or captured metadata without copying its text in the
    /// wrapper. The SDK copies the text before returning.
    pub fn send_metadata<S: MetadataSource + ?Sized>(&self, metadata_frame: &S) {
        let raw = metadata_frame.metadata_raw();
        // The borrowed C string remains live through this synchronous SDK call.
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_send_metadata(self.inner.instance.0, &raw);
        }
    }

    /// The tally this sender currently reports.
    ///
    /// Polls without waiting and returns the most recent state the SDK reported
    /// to this sender, whether or not it has changed since the last query. Call
    /// [`Self::wait_for_tally_change`] to block until it changes. Never fails
    /// and may be called from any thread.
    /// While another thread waits, returns the cached state without a native
    /// query. That state may lag the change until the waiter wakes. Polling
    /// preserves a change for [`Self::wait_for_tally_change`] to report.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, SenderOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let options = SenderOptions::new("Test Sender");
    /// let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    ///
    /// let tally = sender.tally();
    /// println!("On program: {}, On preview: {}", tally.on_program(), tally.on_preview());
    /// # Ok(())
    /// # }
    /// ```
    pub fn tally(&self) -> Tally {
        let state = &self.inner.tally;
        let Some(_query) = state.query.try_lock() else {
            return state.current();
        };
        let (changed, tally) = self.get_tally(0);
        state.remember(tally);
        if changed {
            state.pending.store(true, Ordering::Release);
        }
        tally
    }

    /// Wait up to `timeout` for the tally to change.
    ///
    /// Returns `Ok(Some(tally))` with the new state when the SDK reports a
    /// change, and `Ok(None)` when `timeout` elapses first. Use
    /// [`Self::tally`] to read the current state without waiting.
    /// A change observed by polling is kept for the next wait. Changes
    /// coalesce, and each report goes to one waiter, rather than broadcasting
    /// to every waiter. Concurrent waits serialize their native queries;
    /// contention spends the same timeout budget as the native wait.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if `timeout` exceeds [`crate::MAX_TIMEOUT`]
    /// (about 49.7 days).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, SenderOptions};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let options = SenderOptions::new("Test Sender");
    /// let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    ///
    /// if let Some(tally) = sender.wait_for_tally_change(Duration::from_secs(1))? {
    ///     println!("On program: {}, On preview: {}", tally.on_program(), tally.on_preview());
    /// } else {
    ///     println!("Tally did not change");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn wait_for_tally_change(&self, timeout: Duration) -> Result<Option<Tally>> {
        let deadline = Deadline::after(timeout)?;
        let state = &self.inner.tally;
        let Some(_query) = state.query.try_lock_until(deadline.end()) else {
            return Ok(None);
        };
        if state.pending.swap(false, Ordering::AcqRel) {
            return Ok(Some(state.current()));
        }
        let (changed, tally) = self.get_tally(deadline.remaining_ms());
        state.remember(tally);
        Ok(changed.then_some(tally))
    }

    /// Query the SDK's tally, waiting up to `timeout_ms`; returns whether it
    /// changed, and the tally.
    fn get_tally(&self, timeout_ms: u32) -> (bool, Tally) {
        let mut raw_tally = Tally::default().to_sdk();
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        let changed = unsafe {
            self.inner
                .core
                .NDIlib_send_get_tally(self.inner.instance.0, &mut raw_tally, timeout_ms)
        };
        (
            changed,
            Tally::new(raw_tally.on_program, raw_tally.on_preview),
        )
    }

    /// Get the number of active connections to this sender.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum wait for at least one connected receiver. A zero
    ///   timeout returns the current count immediately. A nonzero timeout
    ///   returns as soon as a receiver is connected, or returns zero when the
    ///   wait expires, as specified by `Processing.NDI.Send.h`. Must not exceed
    ///   [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// Number of active connections as a `u32`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnexpectedSdkResult`] if the SDK returns a negative connection count.
    /// Returns [`Error::InvalidInput`] if `timeout` exceeds [`crate::MAX_TIMEOUT`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, SenderOptions};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let options = SenderOptions::new("Test Sender");
    /// let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    ///
    /// // Get connection count with 1 second timeout
    /// let count = sender.connection_count(Duration::from_secs(1))?;
    /// println!("Active connections: {}", count);
    /// # Ok(())
    /// # }
    /// ```
    pub fn connection_count(&self, timeout: Duration) -> Result<u32> {
        let timeout_ms = to_ms_checked(timeout)?;
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        let count = unsafe {
            self.inner
                .core
                .NDIlib_send_get_no_connections(self.inner.instance.0, timeout_ms)
        };

        u32::try_from(count).map_err(|_| {
            Error::UnexpectedSdkResult(format!(
                "NDIlib_send_get_no_connections returned a negative count: {count}"
            ))
        })
    }

    /// Remove all metadata registered for delivery to each new connection.
    pub fn clear_connection_metadata(&self) {
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_clear_connection_metadata(self.inner.instance.0)
        }
    }

    /// Register metadata sent now and to every new connection.
    ///
    /// Accepts owned or captured metadata. The SDK retains its own copy, so
    /// the input can be released as soon as this method returns.
    pub fn add_connection_metadata<S: MetadataSource + ?Sized>(&self, metadata_frame: &S) {
        let raw = metadata_frame.metadata_raw();
        // The borrowed C string remains live through this synchronous SDK call.
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_add_connection_metadata(self.inner.instance.0, &raw)
        }
    }

    /// Sets failover source.
    ///
    /// # Errors
    ///
    /// Returns an error if source conversion fails.
    pub fn set_failover(&self, source: &Source) -> Result<()> {
        let raw_source = source.to_raw()?;
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_set_failover(self.inner.instance.0, &raw_source.raw)
        }
        Ok(())
    }

    /// Clear the failover source previously configured with [`Self::set_failover`].
    pub fn clear_failover(&self) {
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_set_failover(self.inner.instance.0, ptr::null())
        }
    }

    /// Get the source information for this sender.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NativeOperationFailed`] when the SDK supplies no
    /// descriptor, or one that names neither a source nor an address, and
    /// [`Error::UnexpectedSdkResult`] when its name or address is longer than
    /// the 4 MiB one SDK string may be.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, SenderOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let options = SenderOptions::new("Test Sender");
    /// let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    /// let source = sender.source()?;
    /// println!("Sender source: {source}");
    /// # Ok(())
    /// # }
    /// ```
    pub fn source(&self) -> Result<Source> {
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        let source_ptr = unsafe {
            self.inner
                .core
                .NDIlib_send_get_source_name(self.inner.instance.0)
        };
        // SAFETY: the SDK sender owns this live descriptor and its strings
        // through the synchronous copy performed here.
        unsafe { Source::try_from_raw(source_ptr, "NDIlib_send_get_source_name") }
    }
}

/// Configuration for creating a [`Sender`].
///
/// The defaults pace video and audio submissions to their frame and sample
/// rates, in the groups of the NDI configuration.
///
/// # Examples
///
/// ```no_run
/// # use grafton_ndi::{Ndi, SenderOptions, Sender};
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = Ndi::new()?;
/// let options = SenderOptions::new("My Sender").groups("Studio");
/// let sender = Sender::new(&ndi, &options)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SenderOptions {
    name: String,
    groups: Option<String>,
    clock_video: bool,
    clock_audio: bool,
    #[cfg(feature = "advanced")]
    pub(crate) config: Option<crate::advanced::AdvancedConfig>,
}

impl SenderOptions {
    /// Replace the advertised name; an empty name lets the SDK choose.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Inherit discovery groups from the NDI configuration.
    #[must_use]
    pub fn clear_groups(mut self) -> Self {
        self.groups = None;
        self
    }

    /// Remove the per-instance Advanced configuration override.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn clear_config(mut self) -> Self {
        self.config = None;
        self
    }

    /// Requested advertised name; an empty name lets the SDK choose.
    #[must_use]
    pub fn configured_name(&self) -> &str {
        &self.name
    }

    /// Requested discovery groups, or `None` to inherit the NDI configuration.
    #[must_use]
    pub fn configured_groups(&self) -> Option<&str> {
        self.groups.as_deref()
    }

    /// Whether video clocking was requested.
    #[must_use]
    pub fn configured_clock_video(&self) -> bool {
        self.clock_video
    }

    /// Whether audio clocking was requested.
    #[must_use]
    pub fn configured_clock_audio(&self) -> bool {
        self.clock_audio
    }

    /// The stored Advanced configuration handle, if any.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn configured_config(&self) -> Option<&crate::advanced::AdvancedConfig> {
        self.config.as_ref()
    }

    /// A sender advertised as `name`, which the SDK chooses when it is empty,
    /// with the default settings.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            groups: None,
            clock_video: true,
            clock_audio: true,
            #[cfg(feature = "advanced")]
            config: None,
        }
    }

    /// Set the comma-separated discovery groups the sender is advertised
    /// in, instead of those of the NDI configuration.
    #[must_use]
    pub fn groups(mut self, groups: impl Into<String>) -> Self {
        self.groups = Some(groups.into());
        self
    }

    /// Set whether video submissions are paced to the frame rate, rather
    /// than returning at once.
    #[must_use]
    pub fn clock_video(mut self, clock: bool) -> Self {
        self.clock_video = clock;
        self
    }

    /// Set whether audio submissions are paced to the sample rate, rather
    /// than returning at once.
    #[must_use]
    pub fn clock_audio(mut self, clock: bool) -> Self {
        self.clock_audio = clock;
        self
    }

    /// Create the sender with this per-instance Advanced SDK configuration,
    /// which it keeps for as long as it lives.
    ///
    /// [`Sender::new`] then needs an Advanced runtime, and a build with the
    /// Advanced SDK.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn config(mut self, config: crate::advanced::AdvancedConfig) -> Self {
        self.config = Some(config);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeFeature;

    #[test]
    fn captured_video_sending_uses_initialized_active_rows_and_preserves_descriptors() {
        use crate::capture::{FrameSyncVideoFree, Guard, VideoKind};
        use crate::frames::VideoRef;
        use std::{
            mem::MaybeUninit,
            sync::atomic::{AtomicUsize, Ordering},
        };

        fn verify(source: &(impl VideoSource + ?Sized), original: &NDIlib_video_frame_v2_t) {
            let expected = source.owned_copy();
            let mut staging = Pixels::default();
            let (raw, allocations) = crate::test_alloc::allocation_count(|| {
                PreparedVideo::new(source, &mut staging).raw
            });
            let direct = source.sends_in_place();
            assert_eq!(allocations, usize::from(!direct));
            assert_eq!(raw.p_metadata, original.p_metadata);
            assert_eq!(raw.timestamp, original.timestamp);
            assert_eq!(raw.timecode, original.timecode);
            assert_eq!(raw.picture_aspect_ratio, original.picture_aspect_ratio);
            assert_eq!(raw.frame_format_type, original.frame_format_type);
            assert_eq!(
                (raw.xres, raw.yres, raw.FourCC),
                (original.xres, original.yres, original.FourCC)
            );
            assert_eq!(
                (raw.frame_rate_N, raw.frame_rate_D),
                (original.frame_rate_N, original.frame_rate_D)
            );
            if direct {
                assert_eq!(raw.p_data, original.p_data);
                assert!(staging.as_slice().is_empty());
            } else {
                assert_eq!(raw.p_data, staging.as_ptr().cast_mut());
                assert_ne!(raw.p_data, original.p_data);
                assert_eq!(staging.as_slice(), expected.data());
                assert!(staging.room().iter().all(|&byte| byte == 0));
                assert_eq!(
                    staging.tail_room(),
                    send_room(&source.layout().packed(), source.header().scan_type)
                );
            }
            let pointer = raw.p_data;
            let (next, allocations) = crate::test_alloc::allocation_count(|| {
                PreparedVideo::new(source, &mut staging).raw
            });
            assert_eq!(allocations, 0);
            assert_eq!(next.p_data, pointer);
        }

        for format in PixelFormat::ALL {
            for scan in ScanType::ALL {
                for padding in [0, 8] {
                    let stride = format.line_stride_bytes(4).unwrap() + padding;
                    let len = ValidatedVideoLayout::new_uncompressed(format, 4, 6, Some(stride))
                        .unwrap()
                        .data_len_bytes();
                    let initialized = vec![71; len];
                    let template = BorrowedVideoFrame::builder(&initialized, 4, 6, format)
                        .line_stride_bytes(stride)
                        .scan_type(scan)
                        .frame_rate(FrameRate::new(30_000, 1_001).unwrap())
                        .picture_aspect_ratio(1.25)
                        .timecode(Timecode::from_ticks(12345))
                        .metadata(c"<captured/>")
                        .build()
                        .unwrap();
                    // Only active rows are initialized. Reading any capture padding
                    // or unproven SDK tail is invalid under Miri as well as Rust.
                    let mut storage = vec![MaybeUninit::<u8>::uninit(); len];
                    for plane in template.planes() {
                        for row in plane.rows() {
                            let start = row.as_ptr() as usize - initialized.as_ptr() as usize;
                            for byte in &mut storage[start..start + row.len()] {
                                byte.write(71);
                            }
                        }
                    }
                    let raw = NDIlib_video_frame_v2_t {
                        p_data: storage.as_mut_ptr().cast(),
                        timestamp: 912,
                        ..template.to_raw()
                    };
                    let frees = AtomicUsize::new(0);
                    let instance = (&frees as *const AtomicUsize).cast_mut().cast();
                    // SAFETY: each guard borrows the still-live storage and static
                    // metadata. Its callback counts release and frees no memory.
                    let receiver = unsafe {
                        VideoRef::new(Guard::<VideoKind>::with_test_free(
                            instance,
                            raw,
                            |instance, _| {
                                (*(instance as *const AtomicUsize)).fetch_add(1, Ordering::SeqCst);
                            },
                        ))
                    }
                    .unwrap();
                    let framesync = unsafe {
                        VideoRef::new(Guard::<FrameSyncVideoFree>::with_test_free(
                            instance.cast(),
                            raw,
                            |instance, _| {
                                (*(instance as *const AtomicUsize)).fetch_add(1, Ordering::SeqCst);
                            },
                        ))
                    }
                    .unwrap();
                    let eligible = padding == 0 && send_room(receiver.layout(), scan) == 0;
                    assert_eq!(
                        receiver.sends_in_place(),
                        eligible,
                        "{format:?} {scan:?} padding={padding}"
                    );
                    verify(&receiver, &raw);
                    verify(&framesync, &raw);
                    assert_eq!(frees.load(Ordering::SeqCst), 0);
                    drop(receiver);
                    drop(framesync);
                    assert_eq!(frees.load(Ordering::SeqCst), 2);
                }
            }
        }
    }

    fn test_owned_frame(value: u8) -> VideoFrame {
        let mut frame = VideoFrame::builder()
            .resolution(16, 16)
            .pixel_format(PixelFormat::Bgra)
            .frame_rate(FrameRate::new(30, 1).unwrap())
            .metadata(format!("<frame value='{value}'/>"))
            .build()
            .unwrap();
        frame.data_mut().fill(value);
        frame
    }

    /// A sender on the real runtime, or `None` when the test must be skipped.
    fn test_sender() -> Option<Sender> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let name = crate::test_support::unique_name(&format!(
            "Owned async regression {}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let ndi = crate::test_support::ndi()?;
        Some(
            Sender::new(
                &ndi,
                &SenderOptions::new(name)
                    .clock_video(false)
                    .clock_audio(false),
            )
            .unwrap(),
        )
    }

    #[test]
    fn metadata_send_methods_reuse_validated_text_with_sdk() {
        let Some(sender) = test_sender() else {
            return;
        };
        let frame = MetadataFrame::new("<ndi_text>é</ndi_text>")
            .unwrap()
            .with_timecode(Timecode::from_ticks(42));
        let expected_ptr = frame.to_raw().p_data;
        for _ in 0..3 {
            sender.send_metadata(&frame);
            assert_eq!(frame.to_raw().p_data, expected_ptr);
        }
        sender.add_connection_metadata(&frame);
        sender.clear_connection_metadata();
    }

    #[test]
    fn owned_async_recycles_original_pixel_and_metadata_allocations() {
        let Some(mut sender) = test_sender() else {
            return;
        };
        let first = test_owned_frame(11);
        let pixels = first.data().as_ptr();
        let metadata = first.metadata_cstr().unwrap().as_ptr();
        assert!(sender.send_video_async(first).unwrap().is_none());
        {
            let video = sender.native.video.state.lock().unwrap();
            let Some(frame) = video.in_flight.frame() else {
                panic!("missing owned frame")
            };
            assert_eq!(frame.as_test_raw().p_data, pixels as *mut u8);
            assert_eq!(frame.as_test_raw().p_metadata, metadata);
        }
        let second = test_owned_frame(22);
        let second_pixels = second.data().as_ptr();
        let recycled = sender.send_video_async(second).unwrap().unwrap();
        assert_eq!(recycled.data().as_ptr(), pixels);
        assert_eq!(recycled.metadata_cstr().unwrap().as_ptr(), metadata);
        assert_eq!(recycled.metadata(), Some("<frame value='11'/>"));
        let final_frame = sender.finish_video_async().unwrap();
        assert_eq!(final_frame.data().as_ptr(), second_pixels);
        assert!(sender.finish_video_async().is_none());
    }

    #[test]
    fn synchronous_send_of_a_padded_borrowed_frame_allocates_nothing() {
        let Some(sender) = test_sender() else {
            return;
        };
        // 16 pixels per row, padded to a 96-byte stride.
        let data = vec![5u8; 96 * 16];
        let metadata = c"<padded/>";
        let borrowed = BorrowedVideoFrame::builder(&data, 16, 16, PixelFormat::Bgra)
            .line_stride_bytes(96)
            .metadata(metadata)
            .build()
            .unwrap();
        // Warm any lazily created SDK or runtime state before counting.
        sender.send_video(&borrowed).unwrap();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            sender.send_video(&borrowed).unwrap();
            // Lint allowed: the by-reference form is what is under test.
            #[allow(clippy::needless_borrows_for_generic_args)]
            sender.send_video(&borrowed).unwrap();
        });
        assert_eq!(allocations, 0);
        // An owned frame is lent the same way.
        let owned = test_owned_frame(3);
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            sender.send_video(&owned).unwrap();
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn copied_async_sends_reuse_pixels_and_metadata_after_warmup() {
        let Some(mut sender) = test_sender() else {
            return;
        };
        let pixels = vec![128; 16 * 16 * 4];
        let source = BorrowedVideoFrame::builder(&pixels, 16, 16, PixelFormat::Bgra)
            .frame_rate(FrameRate::new(30, 1).unwrap())
            .picture_aspect_ratio(1.25)
            .timecode(Timecode::from_ticks(1234))
            .metadata(c"<metadata/>")
            .build()
            .unwrap();
        let shorter = BorrowedVideoFrame::builder(&pixels, 16, 16, PixelFormat::Bgra)
            .metadata(c"<short/>")
            .build()
            .unwrap();
        sender.send_video_async_copy(&source).unwrap();
        sender.send_video_async_copy(&source).unwrap();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..4 {
                sender.send_video_async_copy(&shorter).unwrap();
                sender.send_video_async_copy(&shorter).unwrap();
                sender.send_video_async_copy(&source).unwrap();
                sender.send_video_async_copy(&source).unwrap();
            }
        });
        assert_eq!(allocations, 0);
        let copy = sender.finish_video_async().unwrap();
        assert_eq!(copy.data(), pixels);
        assert_eq!(copy.frame_rate(), source.frame_rate());
        assert_eq!(copy.picture_aspect_ratio(), 1.25);
        assert_eq!(copy.timecode(), Timecode::from_ticks(1234));
        assert_eq!(copy.metadata(), Some("<metadata/>"));
        assert!(sender.native.video.state.lock().unwrap().spare.is_none());
        assert!(sender.finish_video_async().is_none());
    }

    #[test]
    fn copied_async_sends_reuse_metadata_across_absent_and_empty_frames() {
        let Some(mut sender) = test_sender() else {
            return;
        };
        let pixels = vec![128; 16 * 16 * 4];
        let absent = BorrowedVideoFrame::builder(&pixels, 16, 16, PixelFormat::Bgra)
            .build()
            .unwrap();
        let empty = BorrowedVideoFrame::builder(&pixels, 16, 16, PixelFormat::Bgra)
            .metadata(c"")
            .build()
            .unwrap();
        let present = BorrowedVideoFrame::builder(&pixels, 16, 16, PixelFormat::Bgra)
            .metadata(c"<metadata/>")
            .build()
            .unwrap();
        sender.send_video_async_copy(&present).unwrap();
        sender.send_video_async_copy(&present).unwrap();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..4 {
                for (source, expected) in [
                    (&absent, None),
                    (&empty, Some("")),
                    (&present, Some("<metadata/>")),
                ] {
                    sender.send_video_async_copy(source).unwrap();
                    let state = sender.native.video.state.lock().unwrap();
                    assert_eq!(state.in_flight.frame().unwrap().metadata(), expected);
                }
            }
        });
        assert_eq!(allocations, 0);
        assert_eq!(
            sender.finish_video_async().unwrap().metadata(),
            Some("<metadata/>")
        );
    }

    #[test]
    fn copied_async_storage_grows_once_per_buffer_then_reuses_capacity() {
        let Some(mut sender) = test_sender() else {
            return;
        };
        let small_pixels = vec![128; 16 * 16 * 4];
        let large_pixels = vec![128; 64 * 16 * 4];
        let small = BorrowedVideoFrame::builder(&small_pixels, 16, 16, PixelFormat::Bgra)
            .metadata(c"<a/>")
            .build()
            .unwrap();
        let large = BorrowedVideoFrame::builder(&large_pixels, 64, 16, PixelFormat::Bgra)
            .metadata(c"<a/>")
            .build()
            .unwrap();
        let metadata = CString::new("x".repeat(256)).unwrap();
        let longer = BorrowedVideoFrame::builder(&large_pixels, 64, 16, PixelFormat::Bgra)
            .metadata(&metadata)
            .build()
            .unwrap();
        sender.send_video_async_copy(&small).unwrap();
        sender.send_video_async_copy(&small).unwrap();
        // Each of the two reusable pixel buffers grows once, with no new
        // metadata storage needed. No further growth is needed afterward.
        for _ in 0..2 {
            let ((), allocations) = crate::test_alloc::allocation_count(|| {
                sender.send_video_async_copy(&large).unwrap();
            });
            assert_eq!(allocations, 1);
        }
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            sender.send_video_async_copy(&large).unwrap();
            sender.send_video_async_copy(&large).unwrap();
        });
        assert_eq!(allocations, 0);
        // Longer metadata likewise grows each reusable metadata buffer once.
        for _ in 0..2 {
            let ((), allocations) = crate::test_alloc::allocation_count(|| {
                sender.send_video_async_copy(&longer).unwrap();
            });
            assert_eq!(allocations, 1);
        }
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..4 {
                sender.send_video_async_copy(&longer).unwrap();
                sender.send_video_async_copy(&large).unwrap();
            }
        });
        assert_eq!(allocations, 0);
        sender.finish_video_async();
    }

    #[test]
    fn owned_video_preparation_preserves_storage_for_every_scan_type() {
        for format in PixelFormat::ALL {
            for height in [4, 6] {
                for scan in ScanType::ALL {
                    let frame = VideoFrame::builder()
                        .resolution(18, height)
                        .pixel_format(format)
                        .scan_type(scan)
                        .metadata("<owned/>")
                        .build()
                        .unwrap();
                    let mut staging = Pixels::default();
                    let (raw, allocations) = crate::test_alloc::allocation_count(|| {
                        PreparedVideo::new(&frame, &mut staging).raw
                    });
                    assert_eq!(allocations, 0, "{format:?} {height} {scan:?}");
                    assert_eq!(raw.p_data.cast_const(), frame.data().as_ptr());
                    assert_eq!(raw.p_metadata, frame.as_test_raw().p_metadata);
                    assert_eq!(raw.frame_format_type, scan.to_sdk());
                    assert!(staging.as_slice().is_empty());
                }
            }
        }
    }

    #[test]
    fn default_yuv_staging_ignores_all_caller_padding_and_tail_room() {
        for format in [PixelFormat::Uyvy, PixelFormat::Uyva] {
            if format.send_tail_room() == 0 {
                continue;
            }
            for scan in [
                ScanType::Progressive,
                ScanType::Interlaced,
                ScanType::Field1,
            ] {
                for stride in [36, 40, 164] {
                    let layout =
                        ValidatedVideoLayout::new_uncompressed(format, 18, 6, Some(stride))
                            .unwrap();
                    let len = layout.data_len_bytes();
                    let room = send_room(&layout, scan);
                    let mut staging = Pixels::default();
                    let mut expected = None;
                    for extra in [0, room] {
                        for vouched in [0, room] {
                            for padding in [16, 235] {
                                let mut buffer = vec![padding; len + extra + vouched];
                                let ranges: Vec<_> = {
                                    let frame = BorrowedVideoFrame::builder(&buffer, 18, 6, format)
                                        .line_stride_bytes(stride)
                                        .build()
                                        .unwrap();
                                    frame
                                        .planes()
                                        .flat_map(|plane| {
                                            plane
                                                .rows()
                                                .map(|row| {
                                                    let start = row.as_ptr() as usize
                                                        - buffer.as_ptr() as usize;
                                                    start..start + row.len()
                                                })
                                                .collect::<Vec<_>>()
                                        })
                                        .collect()
                                };
                                for range in ranges {
                                    buffer[range].fill(128);
                                }
                                let builder = BorrowedVideoFrame::builder(
                                    &buffer[..len + extra],
                                    18,
                                    6,
                                    format,
                                )
                                .line_stride_bytes(stride)
                                .scan_type(scan);
                                // SAFETY: the same initialized allocation extends
                                // `vouched` bytes past the slice and stays unchanged.
                                let frame =
                                    unsafe { builder.readable_after(vouched) }.build().unwrap();
                                assert!(!frame.sends_in_place());
                                let raw = PreparedVideo::new(&frame, &mut staging).raw;
                                assert_eq!(
                                    unsafe { raw.__bindgen_anon_1.line_stride_in_bytes },
                                    36
                                );
                                assert_eq!(raw.frame_format_type, scan.to_sdk());
                                assert_eq!(staging.as_slice(), frame.to_owned().data());
                                if let Some(expected) = &expected {
                                    assert_eq!(staging.as_slice(), expected);
                                } else {
                                    expected = Some(staging.as_slice().to_vec());
                                }
                                assert_eq!(staging.tail_room(), send_room(&layout.packed(), scan));
                                assert!(staging.room().iter().all(|&byte| byte == 0));
                                let address = staging.as_ptr();
                                let ((), allocations) = crate::test_alloc::allocation_count(|| {
                                    let _ = PreparedVideo::new(&frame, &mut staging);
                                });
                                assert_eq!(allocations, 0);
                                assert_eq!(staging.as_ptr(), address);
                            }
                        }
                    }
                }
            }
        }
    }

    /// The room the SDK reads after a UYVY or UYVA frame is whatever follows
    /// the last pixel in the caller's buffer: the last row's padding, the
    /// bytes after the frame, and the bytes the caller vouches for. Other
    /// formats need none, and an interlaced or single-field frame whose
    /// height is not a multiple of four needs two rows more.
    #[test]
    fn a_borrowed_frame_sends_in_place_when_room_follows_its_last_pixel() {
        let buffer = vec![0u8; 8192];
        let in_place = |format, height, scan, stride: Option<usize>, len: usize, vouched| {
            let mut builder = BorrowedVideoFrame::builder(&buffer[..len], 18, height, format)
                .scan_type(scan)
                .allow_send_from_padding_and_tail(true);
            if let Some(stride) = stride {
                builder = builder.line_stride_bytes(stride);
            }
            // SAFETY: `buffer` extends at least `vouched` bytes past every
            // length used here.
            unsafe { builder.readable_after(vouched) }
                .build()
                .unwrap()
                .sends_in_place()
        };
        let progressive = ScanType::Progressive;
        // No format but UYVY and UYVA is read past.
        for format in PixelFormat::ALL {
            if !matches!(format, PixelFormat::Uyvy | PixelFormat::Uyva) {
                assert_eq!(format.send_tail_room(), 0, "{format:?}");
                let len = format.buffer_len(18, 4).unwrap();
                for scan in ScanType::ALL {
                    assert!(
                        in_place(format, 4, scan, None, len, 0),
                        "{format:?} {scan:?}"
                    );
                }
            }
        }
        let (uyvy, uyva) = (PixelFormat::Uyvy, PixelFormat::Uyva);
        let room = uyvy.send_tail_room();
        assert_eq!(room, uyva.send_tail_room());
        if room == 0 {
            // The SDK reads frames exactly here: nothing needs room.
            for scan in ScanType::ALL {
                assert!(in_place(uyvy, 6, scan, None, 216, 0), "{scan:?}");
                assert!(in_place(uyva, 6, scan, None, 324, 0), "{scan:?}");
            }
            return;
        }
        assert_eq!(room, 64);
        // Packed rows of 36 bytes, four rows: the frame is 144 bytes.
        assert!(!in_place(uyvy, 4, progressive, None, 144, 0));
        assert!(!in_place(uyvy, 4, progressive, None, 144 + 63, 0));
        assert!(in_place(uyvy, 4, progressive, None, 144 + 64, 0));
        // Row padding counts: 64 bytes of it, or 60 and four more bytes.
        assert!(in_place(uyvy, 4, progressive, Some(100), 400, 0));
        assert!(!in_place(uyvy, 4, progressive, Some(96), 384, 0));
        assert!(!in_place(uyvy, 4, progressive, Some(96), 384 + 3, 0));
        assert!(in_place(uyvy, 4, progressive, Some(96), 384 + 4, 0));
        // So do bytes the caller vouches for, alone or with the others.
        assert!(!in_place(uyvy, 4, progressive, None, 144, 63));
        assert!(in_place(uyvy, 4, progressive, None, 144, 64));
        assert!(in_place(uyvy, 4, progressive, Some(96), 384 + 2, 2));

        // UYVA ends with its alpha plane, whose rows are half the stride.
        assert!(!in_place(uyva, 4, progressive, None, 216, 0));
        assert!(!in_place(uyva, 4, progressive, None, 216 + 63, 0));
        assert!(in_place(uyva, 4, progressive, None, 216 + 64, 0));
        assert!(in_place(uyva, 4, progressive, Some(164), 984, 0));
        assert!(!in_place(uyva, 4, progressive, Some(160), 960, 0));
        assert!(in_place(uyva, 4, progressive, Some(160), 960 + 2, 0));
        // Interlaced and single-field frames need the same room when their
        // height is a multiple of four, and two line strides more when not.
        for scan in [ScanType::Interlaced, ScanType::Field0, ScanType::Field1] {
            assert!(!in_place(uyvy, 4, scan, None, 144 + 63, 0), "{scan:?}");
            assert!(in_place(uyvy, 4, scan, None, 144 + 64, 0), "{scan:?}");
            // Six rows of 36 bytes: 216 bytes, and 72 + 64 of room.
            assert!(!in_place(uyvy, 6, scan, None, 216 + 64, 0), "{scan:?}");
            assert!(!in_place(uyvy, 6, scan, None, 216 + 135, 0), "{scan:?}");
            assert!(in_place(uyvy, 6, scan, None, 216 + 136, 0), "{scan:?}");
            assert!(in_place(uyvy, 5, scan, None, 180 + 136, 0), "{scan:?}");
            // With a 100-byte stride the rows are 200 bytes, less the 64 of
            // padding that already follow the last pixel.
            assert!(
                !in_place(uyvy, 6, scan, Some(100), 600 + 199, 0),
                "{scan:?}"
            );
            assert!(in_place(uyvy, 6, scan, Some(100), 600 + 200, 0), "{scan:?}");
            assert!(in_place(uyva, 6, scan, None, 324 + 136, 0), "{scan:?}");
            assert!(!in_place(uyva, 6, scan, None, 324 + 135, 0), "{scan:?}");
        }
        assert!(in_place(uyvy, 6, progressive, None, 216 + 64, 0));
    }

    /// Staging packs active pixels, preserves headers and metadata, reuses
    /// its allocation, and leaves explicitly direct frames alone.
    #[test]
    fn staging_packs_active_pixels_preserves_headers_and_reuses_storage() {
        // 18 pixels per row, padded to a 40-byte stride.
        let buffer: Vec<u8> = (0..160).map(|i| i as u8).collect();
        let metadata = c"<staged/>";
        let frame = BorrowedVideoFrame::builder(&buffer, 18, 4, PixelFormat::Uyvy)
            .line_stride_bytes(40)
            .frame_rate(FrameRate::new(30000, 1001).unwrap())
            .picture_aspect_ratio(1.25)
            .scan_type(ScanType::Interlaced)
            .timecode(Timecode::from_ticks(12345))
            .metadata(metadata)
            .build()
            .unwrap();
        if frame.sends_in_place() {
            // The SDK reads frames exactly here, so nothing is ever staged.
            assert_eq!(PixelFormat::Uyvy.send_tail_room(), 0);
            return;
        }
        let original = frame.to_raw();

        let mut staging = Pixels::default();
        let raw = PreparedVideo::new(&frame, &mut staging).raw;
        assert_eq!(raw.p_data, staging.as_ptr().cast_mut());
        assert_ne!(raw.p_data, original.p_data);
        assert_eq!(staging.as_slice(), frame.to_owned().data());
        assert_eq!(staging.room(), [0; 64]);
        assert_eq!(staging.capacity(), 144 + 64);
        // Everything but the pixel pointer describes the caller's frame.
        // SAFETY: both descriptors are uncompressed, so the union holds the stride.
        let strides = unsafe {
            (
                raw.__bindgen_anon_1.line_stride_in_bytes,
                original.__bindgen_anon_1.line_stride_in_bytes,
            )
        };
        assert_eq!(strides, (36, 40));
        assert_eq!(
            (raw.xres, raw.yres, raw.FourCC, raw.timecode, raw.p_metadata),
            (
                original.xres,
                original.yres,
                original.FourCC,
                original.timecode,
                original.p_metadata
            )
        );
        assert_eq!(
            (raw.frame_rate_N, raw.frame_rate_D, raw.frame_format_type),
            (
                original.frame_rate_N,
                original.frame_rate_D,
                original.frame_format_type
            )
        );
        assert_eq!(raw.picture_aspect_ratio, original.picture_aspect_ratio);

        // The next frame of the same size reuses the allocation.
        let other = vec![9u8; 160];
        let next = BorrowedVideoFrame::builder(&other, 18, 4, PixelFormat::Uyvy)
            .line_stride_bytes(40)
            .build()
            .unwrap();
        let address = staging.as_ptr();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            let _ = PreparedVideo::new(&next, &mut staging);
        });
        assert_eq!(allocations, 0);
        assert_eq!(
            (staging.as_ptr(), staging.as_slice()),
            (address, next.to_owned().data())
        );

        // A frame with room is sent from its own buffer: nothing is staged.
        let roomy = vec![1u8; 160 + 64];
        let direct = BorrowedVideoFrame::builder(&roomy, 18, 4, PixelFormat::Uyvy)
            .allow_send_from_padding_and_tail(true)
            .line_stride_bytes(40)
            .build()
            .unwrap();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            let raw = PreparedVideo::new(&direct, &mut staging).raw;
            assert_eq!(raw.p_data.cast_const(), roomy.as_ptr());
        });
        assert_eq!(allocations, 0);
        assert_eq!(staging.as_slice(), next.to_owned().data());
    }

    /// A UYVY frame over a buffer that ends with it reaches the SDK from the
    /// sender's own copy, made in one buffer that later sends reuse.
    #[test]
    fn synchronous_send_copies_a_frame_without_room_into_one_reused_buffer() {
        let Some(sender) = test_sender() else {
            return;
        };
        let staging = |sender: &Sender| {
            let state = sender.native.video.state.lock().unwrap();
            (
                state.staging.as_ptr(),
                state.staging.as_slice().to_vec(),
                state.staging.room().to_vec(),
            )
        };
        assert!(staging(&sender).1.is_empty());

        let exact: Vec<u8> = (0..18 * 2 * 16)
            .map(|i| (64 + (i >> 4) % 128) as u8)
            .collect();
        let frame = BorrowedVideoFrame::builder(&exact, 18, 16, PixelFormat::Uyvy)
            .build()
            .unwrap();
        if frame.sends_in_place() {
            // The SDK reads frames exactly here, so the sender copies none.
            assert_eq!(PixelFormat::Uyvy.send_tail_room(), 0);
            sender.send_video(&frame).unwrap();
            assert!(staging(&sender).1.is_empty());
            return;
        }
        sender.send_video(&frame).unwrap();
        let (address, copy, room) = staging(&sender);
        assert_eq!(copy, exact);
        assert_eq!(room, [0; 64]);

        // The steady state: the same buffer, and no allocation.
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            sender.send_video(&frame).unwrap();
        });
        assert_eq!(allocations, 0);
        assert_eq!(staging(&sender).0, address);

        // Frames that send in place leave the copy alone.
        let mut roomy = exact.clone();
        roomy.reverse();
        roomy.resize(exact.len() + PixelFormat::Uyvy.send_tail_room(), 0);
        let direct = BorrowedVideoFrame::builder(&roomy, 18, 16, PixelFormat::Uyvy)
            .allow_send_from_padding_and_tail(true)
            .build()
            .unwrap();
        assert!(direct.sends_in_place());
        let owned = direct.to_owned();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            sender.send_video(&direct).unwrap();
            sender.send_video(&owned).unwrap();
        });
        assert_eq!(allocations, 0);
        assert_eq!(staging(&sender), (address, copy, room));

        // A larger frame replaces the buffer once.
        let larger = vec![128u8; 34 * 2 * 16];
        let frame = BorrowedVideoFrame::builder(&larger, 34, 16, PixelFormat::Uyvy)
            .build()
            .unwrap();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            sender.send_video(&frame).unwrap();
        });
        assert_eq!(allocations, 1);
        assert_eq!(staging(&sender).1, larger);
    }

    #[test]
    fn synchronous_send_describes_an_owned_frame_as_the_frame_itself() {
        let owned = test_owned_frame(8);
        let mut staging = Pixels::default();
        let (expected, lent) = (
            owned.as_test_raw(),
            PreparedVideo::new(&owned, &mut staging).raw,
        );
        assert_eq!(lent.p_data, expected.p_data);
        assert_eq!(lent.p_metadata, expected.p_metadata);
        // SAFETY: both descriptors are uncompressed, so the union holds the stride.
        let strides = unsafe {
            (
                lent.__bindgen_anon_1.line_stride_in_bytes,
                expected.__bindgen_anon_1.line_stride_in_bytes,
            )
        };
        assert_eq!(strides.0, strides.1);
        assert_eq!(
            (lent.xres, lent.yres, lent.FourCC, lent.timecode),
            (
                expected.xres,
                expected.yres,
                expected.FourCC,
                expected.timecode
            )
        );
        assert_eq!(
            (lent.frame_rate_N, lent.frame_rate_D, lent.frame_format_type),
            (
                expected.frame_rate_N,
                expected.frame_rate_D,
                expected.frame_format_type
            )
        );
        assert_eq!(lent.picture_aspect_ratio, expected.picture_aspect_ratio);
    }

    #[test]
    fn mixed_sync_flush_and_copy_sends_preserve_recovery() {
        let Some(mut sender) = test_sender() else {
            return;
        };
        let frame = test_owned_frame(1);
        let pixels = frame.data().as_ptr();
        sender.send_video_async(frame).unwrap();
        sender.send_video(&test_owned_frame(9)).unwrap();
        sender.flush_async_blocking();
        assert_eq!(sender.finish_video_async().unwrap().data().as_ptr(), pixels);

        // A copied send retains the released owned frame for its next copy.
        let owned = test_owned_frame(2);
        let owned_pixels = owned.data().as_ptr();
        sender.send_video_async(owned).unwrap();
        let data = vec![3; 1024];
        let metadata = CString::new("<copy/>").unwrap();
        {
            let borrowed = BorrowedVideoFrame::builder(&data, 16, 16, PixelFormat::Bgra)
                .metadata(&metadata)
                .build()
                .unwrap();
            sender.send_video_async_copy(&borrowed).unwrap();
        }
        {
            let video = sender.native.video.state.lock().unwrap();
            let Some(copy) = video.in_flight.frame() else {
                panic!("missing copied snapshot")
            };
            assert_ne!(copy.data().as_ptr(), data.as_ptr());
            assert_ne!(copy.metadata_cstr().unwrap().as_ptr(), metadata.as_ptr());
            let spare = video.spare.as_ref().unwrap();
            assert_eq!(spare.data().as_ptr(), owned_pixels);
            assert_eq!(spare.data()[0], 2);
        }
        drop(data);
        drop(metadata);
        {
            let video = sender.native.video.state.lock().unwrap();
            let Some(copy) = video.in_flight.frame() else {
                panic!("missing copied snapshot")
            };
            assert_eq!(copy.data(), vec![3; 1024]);
            assert_eq!(copy.metadata().unwrap(), "<copy/>");
        }
        // Copies are ordinary owned frames and are returned for reuse too.
        let copied = sender
            .send_video_async(test_owned_frame(4))
            .unwrap()
            .unwrap();
        assert_eq!(copied.data()[0], 3);
        assert_eq!(copied.metadata(), Some("<copy/>"));
        sender.finish_video_async().unwrap();
        sender.flush_async_blocking();
    }

    #[test]
    fn every_synchronizing_call_completes_a_frame_once_and_recovery_keeps_its_identity() {
        let Some(mut sender) = test_sender() else {
            return;
        };
        let in_flight = |sender: &Sender| {
            sender
                .native
                .video
                .state
                .lock()
                .unwrap()
                .in_flight
                .is_in_flight()
        };
        let frame = |value| {
            let frame = test_owned_frame(value);
            let pixels = frame.data().as_ptr();
            (frame, pixels)
        };

        // A synchronous send completes the frame in flight; the next
        // submission returns that original allocation.
        let (a, a_pixels) = frame(1);
        assert!(sender.send_video_async(a).unwrap().is_none());
        assert!(in_flight(&sender));
        sender.send_video(&test_owned_frame(9)).unwrap();
        assert!(!in_flight(&sender));
        let (b, b_pixels) = frame(2);
        let previous = sender.send_video_async(b).unwrap().unwrap();
        assert_eq!(previous.data().as_ptr(), a_pixels);

        // A flush does the same, and a second flush finds nothing in flight.
        sender.flush_async_blocking();
        assert!(!in_flight(&sender));
        sender.flush_async_blocking();
        let (c, c_pixels) = frame(3);
        let previous = sender.send_video_async(c).unwrap().unwrap();
        assert_eq!(previous.data().as_ptr(), b_pixels);

        // A submission completes the in-flight frame it displaces.
        let (d, d_pixels) = frame(4);
        let previous = sender.send_video_async(d).unwrap().unwrap();
        assert_eq!(previous.data().as_ptr(), c_pixels);
        assert!(in_flight(&sender));

        // Finishing recovers a frame a synchronous send already completed.
        sender.send_video(&test_owned_frame(9)).unwrap();
        assert_eq!(
            sender.finish_video_async().unwrap().data().as_ptr(),
            d_pixels
        );
        assert!(sender.finish_video_async().is_none());

        // A sender can also be dropped after an explicit flush.
        sender.send_video_async(frame(5).0).unwrap();
        sender.flush_async_blocking();
        drop(sender);
    }

    #[test]
    fn test_async_flush_uses_null_frame_pointer() {
        assert!(async_flush_frame_ptr().is_null());
    }

    #[test]
    fn planar_444_sends_check_the_senders_runtime_before_the_sdk() {
        let Some(mut sender) = test_sender() else {
            return;
        };
        let supported = sender
            .runtime()
            .require_feature(RuntimeFeature::Planar444Video)
            .is_ok();
        let frame = VideoFrame::builder()
            .resolution(16, 16)
            .pixel_format(PixelFormat::Nb4g)
            .build()
            .unwrap();
        let pixels = frame.data().as_ptr();
        let data = frame.data().to_vec();
        let borrowed = BorrowedVideoFrame::builder(&data, 16, 16, PixelFormat::Nb4g)
            .build()
            .unwrap();
        // An earlier frame stays in flight through every rejection.
        assert!(sender
            .send_video_async(test_owned_frame(1))
            .unwrap()
            .is_none());
        if supported {
            sender.send_video(&frame).unwrap();
            let previous = sender.send_video_async(frame).unwrap().unwrap();
            assert_eq!(previous.data()[0], 1);
            sender.send_video_async_copy(&borrowed).unwrap();
            assert_eq!(
                sender
                    .native
                    .video
                    .state
                    .lock()
                    .unwrap()
                    .spare
                    .as_ref()
                    .unwrap()
                    .data()
                    .as_ptr(),
                pixels
            );
        } else {
            let unsupported = |error: &Error| {
                matches!(
                    error,
                    Error::UnsupportedCapability {
                        feature: RuntimeFeature::Planar444Video,
                        ..
                    }
                )
            };
            assert!(unsupported(&sender.send_video(&frame).unwrap_err()));
            let rejected = sender.send_video_async(frame).unwrap_err();
            assert!(unsupported(rejected.error()));
            assert_eq!(rejected.into_inner().data().as_ptr(), pixels);
            assert!(unsupported(
                &sender.send_video_async_copy(&borrowed).unwrap_err()
            ));
            assert!(sender
                .native
                .video
                .state
                .lock()
                .unwrap()
                .in_flight
                .is_in_flight());
        }
        assert!(sender.finish_video_async().is_some());
    }

    #[test]
    fn borrowed_builder_validates_padded_layouts_and_preserves_headers_without_allocating() {
        for format in [
            PixelFormat::Bgra,
            PixelFormat::Bgrx,
            PixelFormat::Rgba,
            PixelFormat::Rgbx,
            PixelFormat::Uyvy,
            PixelFormat::Uyva,
            PixelFormat::P216,
            PixelFormat::Pa16,
            PixelFormat::I420,
            PixelFormat::Yv12,
            PixelFormat::Nv12,
        ] {
            for stride in [
                0,
                format.line_stride_bytes(4).unwrap(),
                format.line_stride_bytes(4).unwrap() + 8,
            ] {
                let layout =
                    ValidatedVideoLayout::new_uncompressed(format, 4, 4, Some(stride)).unwrap();
                // A longer buffer is accepted; the frame is its layout's extent.
                let data = vec![19; layout.data_len_bytes() + 3];
                let metadata = c"<pixels>é</pixels>";
                let rate = FrameRate::new(30000, 1001).unwrap();
                let (borrowed, allocations) = crate::test_alloc::allocation_count(|| {
                    let frame = BorrowedVideoFrame::builder(&data, 4, 4, format)
                        .line_stride_bytes(stride)
                        .frame_rate(rate)
                        .picture_aspect_ratio(1.25)
                        .scan_type(ScanType::Interlaced)
                        .timecode(Timecode::from_ticks(0))
                        .metadata(metadata)
                        .build()
                        .unwrap();
                    for _ in 0..3 {
                        assert_eq!(frame.metadata(), Some("<pixels>é</pixels>"));
                        assert_eq!(frame.data(), &data[..layout.data_len_bytes()]);
                        assert_eq!(frame.pixel_format(), format);
                    }
                    frame
                });
                assert_eq!(allocations, 0);
                let (owned, allocations) =
                    crate::test_alloc::allocation_count(|| borrowed.to_owned());
                assert_eq!(allocations, 2);
                assert_eq!(owned.frame_rate(), rate);
                assert_eq!(owned.picture_aspect_ratio(), 1.25);
                assert_eq!(owned.scan_type(), ScanType::Interlaced);
                assert_eq!(
                    (owned.timecode(), owned.timestamp()),
                    (Timecode::from_ticks(0), None)
                );
                assert_eq!(owned.metadata(), borrowed.metadata());
                let mut staging = Pixels::default();
                let (raw, allocations) = crate::test_alloc::allocation_count(|| {
                    PreparedVideo::new(&owned, &mut staging).raw
                });
                assert_eq!(allocations, 0);
                assert_eq!(raw.p_metadata, owned.as_test_raw().p_metadata);
                assert_eq!(raw.p_data, owned.as_test_raw().p_data);
                assert!(BorrowedVideoFrame::builder(
                    &data[..layout.data_len_bytes() - 1],
                    4,
                    4,
                    format
                )
                .line_stride_bytes(stride)
                .build()
                .is_err());
            }
            for stride in [1, i32::MAX.unsigned_abs() as usize, usize::MAX] {
                let expected = ValidatedVideoLayout::new_uncompressed(format, 4, 4, Some(stride));
                let result = BorrowedVideoFrame::builder(&[], 4, 4, format)
                    .line_stride_bytes(stride)
                    .build();
                assert!(expected.is_err());
                assert!(result.is_err());
            }
        }
    }

    #[test]
    fn borrowed_metadata_is_bounded_utf8_and_preserves_empty_presence() {
        let data = [0; 16];
        for metadata in [None, Some(c"")] {
            let mut builder = BorrowedVideoFrame::builder(&data, 2, 2, PixelFormat::Bgra);
            if let Some(metadata) = metadata {
                builder = builder.metadata(metadata);
            }
            let frame = builder.build().unwrap();
            let (owned, allocations) = crate::test_alloc::allocation_count(|| frame.to_owned());
            assert_eq!(allocations, 1 + usize::from(metadata.is_some()));
            assert_eq!(owned.metadata(), metadata.map(|_| ""));
        }
        let invalid = c"\xff";
        assert!(matches!(
            BorrowedVideoFrame::builder(&data, 2, 2, PixelFormat::Bgra)
                .metadata(invalid)
                .build(),
            Err(Error::InvalidUtf8(_))
        ));
        for len in [
            crate::frames::MAX_METADATA_BYTES,
            crate::frames::MAX_METADATA_BYTES + 1,
        ] {
            let mut bytes = vec![b'x'; len];
            bytes[len - 1] = 0;
            let metadata = CStr::from_bytes_with_nul(&bytes).unwrap();
            let result = BorrowedVideoFrame::builder(&data, 2, 2, PixelFormat::Bgra)
                .metadata(metadata)
                .build();
            assert_eq!(result.is_ok(), len == crate::frames::MAX_METADATA_BYTES);
        }
    }

    #[test]
    fn test_borrowed_frame_timecode_and_aspect_ratio() {
        let buffer = vec![0u8; 64 * 36 * 2];
        let frame = BorrowedVideoFrame::builder(&buffer, 64, 36, PixelFormat::Uyvy)
            .build()
            .unwrap();
        assert_eq!(frame.timecode(), Timecode::SYNTHESIZE);
        assert_eq!(frame.picture_aspect_ratio(), 0.0);

        let frame = BorrowedVideoFrame::builder(&buffer, 64, 36, PixelFormat::Uyvy)
            .picture_aspect_ratio(0.0)
            .build()
            .unwrap()
            .with_timecode(Timecode::from_ticks(1234));
        assert_eq!(frame.timecode(), Timecode::from_ticks(1234));
        assert_eq!(frame.picture_aspect_ratio(), 0.0);
        let raw = frame.to_raw();
        assert_eq!((raw.timecode, raw.picture_aspect_ratio), (1234, 0.0));
        assert_eq!(raw.timestamp, NDIlib_recv_timestamp_undefined);

        for bad in [-1.0, f32::NAN, f32::INFINITY] {
            assert!(
                BorrowedVideoFrame::builder(&buffer, 64, 36, PixelFormat::Uyvy)
                    .picture_aspect_ratio(bad)
                    .build()
                    .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn borrowed_video_aspect_defaults_and_copies_preserve_the_sdk_sentinel() {
        for (width, height) in [(640, 480), (1920, 1080), (1080, 1920), (64, 64)] {
            let pixels = vec![0; width as usize * height as usize * 4];
            for ratio in [None, Some(0.0), Some(1.25)] {
                let mut builder =
                    BorrowedVideoFrame::builder(&pixels, width, height, PixelFormat::Bgra);
                if let Some(ratio) = ratio {
                    builder = builder.picture_aspect_ratio(ratio);
                }
                let frame = builder.build().unwrap();
                let expected = ratio.unwrap_or(0.0);
                assert_eq!(frame.picture_aspect_ratio(), expected);
                assert_eq!(frame.to_raw().picture_aspect_ratio, expected);
                let mut copied = frame.to_owned();
                assert_eq!(copied.picture_aspect_ratio(), expected);
                copied.set_picture_aspect_ratio(2.0).unwrap();
                frame.copy_into(&mut copied);
                assert_eq!(copied.picture_aspect_ratio(), expected);
            }
        }
    }

    /// Build a borrowed frame of `format` over `buffer` at 60 frames per second.
    fn borrowed(
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<BorrowedVideoFrame<'_>> {
        BorrowedVideoFrame::builder(buffer, width, height, format).build()
    }

    #[test]
    fn test_borrowed_builder_exact_size() {
        // BGRA format: 1920x1080x4 bytes
        let buffer = vec![0u8; 1920 * 1080 * 4];
        assert!(borrowed(&buffer, 1920, 1080, PixelFormat::Bgra).is_ok());
    }

    #[test]
    fn test_borrowed_builder_oversized_buffer() {
        // Buffer larger than needed should succeed
        let buffer = vec![0u8; 1920 * 1080 * 4 + 1000];
        let frame = borrowed(&buffer, 1920, 1080, PixelFormat::Bgra).unwrap();
        assert_eq!(frame.data().len(), 1920 * 1080 * 4);
    }

    #[test]
    fn test_borrowed_builder_undersized_buffer() {
        // Buffer too small should fail
        let buffer = vec![0u8; 1920 * 1080 * 4 - 1];
        let result = borrowed(&buffer, 1920, 1080, PixelFormat::Bgra);
        assert!(result.is_err());
        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(msg.contains("Buffer too small"));
            assert!(msg.contains("Bgra"));
        }
    }

    #[test]
    fn test_borrowed_builder_uyvy() {
        // UYVY format: 1920x1080x2 bytes
        let buffer = vec![0u8; 1920 * 1080 * 2];
        assert!(borrowed(&buffer, 1920, 1080, PixelFormat::Uyvy).is_ok());
    }

    #[test]
    fn test_borrowed_builder_nv12() {
        // NV12 planar format: Y plane + UV plane
        let expected_size = PixelFormat::Nv12.buffer_len(1920, 1080).unwrap();
        let buffer = vec![0u8; expected_size];
        assert!(borrowed(&buffer, 1920, 1080, PixelFormat::Nv12).is_ok());
    }

    #[test]
    fn test_borrowed_builder_i420() {
        // I420 planar format
        let expected_size = PixelFormat::I420.buffer_len(640, 480).unwrap();
        let buffer = vec![0u8; expected_size];
        assert!(borrowed(&buffer, 640, 480, PixelFormat::I420).is_ok());
    }

    #[test]
    fn test_borrowed_builder_rejects_invalid_layout() {
        let buffer = vec![0u8; 4096];

        assert!(matches!(
            borrowed(&buffer, 0, 480, PixelFormat::Bgra),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            borrowed(&buffer, 640, 0, PixelFormat::Bgra),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            borrowed(&buffer, 641, 480, PixelFormat::I420),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_getters() {
        let buffer = vec![0u8; 1920 * 1080 * 4];
        let frame = BorrowedVideoFrame::builder(&buffer, 1920, 1080, PixelFormat::Bgra)
            .frame_rate(FrameRate::new(60, 1).unwrap())
            .build()
            .unwrap();

        assert_eq!(frame.width(), 1920);
        assert_eq!(frame.height(), 1080);
        assert_eq!(frame.pixel_format(), PixelFormat::Bgra);
        assert_eq!(frame.frame_rate(), FrameRate::new(60, 1).unwrap());
        assert_eq!(frame.picture_aspect_ratio(), 0.0);
        assert_eq!(frame.scan_type(), ScanType::Progressive);
        assert_eq!(frame.timecode(), Timecode::SYNTHESIZE);
        assert_eq!(frame.data().len(), buffer.len());
        assert_eq!(frame.line_stride_bytes(), 1920 * 4);
        assert!(frame.metadata().is_none());
        // Copies share the borrow, and Debug summarizes rather than dumps pixels.
        let copy = frame;
        assert_eq!(copy.data().as_ptr(), frame.data().as_ptr());
        assert!(format!("{frame:?}").contains("data (bytes): 8294400"));
        assert_eq!(
            borrowed(&buffer, 1920, 1080, PixelFormat::Bgra)
                .unwrap()
                .frame_rate(),
            FrameRate::new(60, 1).unwrap()
        );
    }

    #[test]
    fn test_all_pixel_formats_validation() {
        // Test that validation works correctly for all pixel formats
        let test_cases = vec![
            (PixelFormat::Bgra, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::Rgba, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::Bgrx, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::Rgbx, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::Uyvy, 1920, 1080, 1920 * 1080 * 2),
            (PixelFormat::Uyva, 1920, 1080, 1920 * 1080 * 3),
            (PixelFormat::P216, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::Pa16, 1920, 1080, 1920 * 1080 * 6),
        ];

        for (format, width, height, expected_min_size) in test_cases {
            // Exact size should work
            let buffer = vec![0u8; expected_min_size];
            let result = borrowed(&buffer, width, height, format);
            assert!(result.is_ok(), "Failed for format {:?}", format);

            // One byte too small should fail
            if expected_min_size > 0 {
                let buffer = vec![0u8; expected_min_size - 1];
                let result = borrowed(&buffer, width, height, format);
                assert!(result.is_err(), "Should fail for undersized {:?}", format);
            }
        }
    }

    #[test]
    fn test_planar_formats() {
        // Test planar 4:2:0 formats (NV12, I420, YV12)
        let width = 1920;
        let height = 1080;

        for format in [PixelFormat::Nv12, PixelFormat::I420, PixelFormat::Yv12] {
            let expected_size = format.buffer_len(width, height).unwrap();

            let buffer = vec![0u8; expected_size];
            let result = borrowed(&buffer, width, height, format);
            assert!(result.is_ok(), "Failed for planar format {:?}", format);

            // One byte too small should fail
            if expected_size > 0 {
                let buffer = vec![0u8; expected_size - 1];
                let result = borrowed(&buffer, width, height, format);
                assert!(result.is_err(), "Should fail for undersized {:?}", format);
            }
        }
    }
}
