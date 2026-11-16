//! NDI sending functionality for video, audio, and metadata.

#[cfg(test)]
use crate::pixels::send_room;

use std::{
    ffi::{CStr, CString},
    fmt,
    marker::PhantomData,
    num::NonZeroU32,
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use crate::{
    finder::Source,
    frames::{AudioPartsExt, MetadataPartsExt},
    frames::{
        AudioSource, FrameMetadataRef, MetadataFrame, MetadataFrameRef, MetadataSource,
        MetadataText, PixelFormat, ScanType, Sendable, ValidatedVideoLayout, VideoFrame,
        VideoHeader, VideoParts, VideoPartsExt, VideoSource,
    },
    pixels::Pixels,
    receiver::Tally,
    source_retention::RetainedSources,
    sys::{self, bindings::*},
    timeout::Deadline,
    to_ms_checked, Error, FrameRate, Ndi, PictureAspectRatio, Rejected, Result, Timecode,
};

#[cfg(feature = "advanced")]
use crate::advanced::{CompressedAttached, CompressedAudioSendMode, QueueAttached};

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
struct SendHandle<'a> {
    instance: SendInstance,
    core: &'static sys::Core,
    _owner: PhantomData<&'a SenderResource>,
}

/// The handle of a sender, and the lock that pairs a metadata capture with
/// its free.
struct Inner {
    // A copy of the handle that the sender's `NativeOwner` owns.
    instance: SendInstance,
    // The entry points of the runtime that created `instance`.
    core: &'static sys::Core,
    metadata_capture: SenderMetadataGate,
    tally: TallyState,
    #[cfg(ndi_has_advanced_keyframe_demand)]
    keyframe_changes: crate::keyframe_changes::KeyframeChanges,
}

// A panic cannot leave metadata capture or tally half-changed: their locks
// guard native queries, and no application code runs while held. Those locks
// do not poison or declare this themselves, which would otherwise leave
// senders short of `RefUnwindSafe`. The change feed uses unwind-safe standard locks.
impl std::panic::RefUnwindSafe for Inner {}

/// Exclusion spans capture through matching free, including a borrowed frame's
/// lifetime. The mutex itself is held only while changing the occupancy bit,
/// so the release token can safely move to a scoped worker thread.
#[derive(Default)]
struct SenderMetadataGate {
    occupied: parking_lot::Mutex<bool>,
    available: parking_lot::Condvar,
}

// Only the private acquisition/release paths change the occupancy bit, with
// no application code under its lock. SenderMetadataGuard::drop sequences the
// native free and lease release, including on unwind, so a panic cannot leave
// capture state half-mutated. This preserves the captured frame's unwind traits.
impl std::panic::RefUnwindSafe for SenderMetadataGate {}

impl SenderMetadataGate {
    fn acquire_until(&self, end: Instant) -> Option<SenderMetadataLease<'_>> {
        let mut occupied = self.occupied.try_lock_until(end)?;
        while *occupied {
            if self.available.wait_until(&mut occupied, end).timed_out() && *occupied {
                return None;
            }
        }
        *occupied = true;
        Some(SenderMetadataLease { gate: self })
    }
}

/// A movable permit retaining a sender's capture/free exclusion. It borrows
/// the sender-owned gate, so neither the gate nor native sender can disappear
/// while a captured frame retains it. The frame drops before this token.
pub(crate) struct SenderMetadataLease<'a> {
    gate: &'a SenderMetadataGate,
}

impl Drop for SenderMetadataLease<'_> {
    fn drop(&mut self) {
        *self.gate.occupied.lock() = false;
        self.gate.available.notify_one();
    }
}

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
        // A registration may keep this native sender alive after Sender drops.
        pub(super) failover: parking_lot::Mutex<RetainedSources>,
        #[cfg(feature = "advanced")]
        pub(super) _config: Option<crate::advanced::AdvancedConfig>,
        pub(super) _ndi: Ndi,
    }

    // No application code runs under the failover lock. A replacement keeps
    // both descriptors alive through the native call, then commits one swap;
    // unwinding cannot leave a dangling native source descriptor.
    impl std::panic::RefUnwindSafe for SenderResource {}

    impl fmt::Debug for SenderResource {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("SenderResource")
                .field("name", &self._name)
                .finish_non_exhaustive()
        }
    }

    impl SenderResource {
        /// The native sender this token keeps alive.
        pub(crate) fn instance(&self) -> NDIlib_send_instance_t {
            self.instance.0
        }
    }

    impl SenderResource {
        pub(super) fn handle(&self) -> SendHandle<'_> {
            SendHandle {
                instance: self.instance,
                core: &self._ndi.api().core,
                _owner: PhantomData,
            }
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
        /// The resource shared by this sender's common operations.
        fn resource(&self) -> &SenderResource;
    }

    /// The native sender of a [`RawSend`] sender, which advertiser registrations
    /// may share, and the asynchronous video pipeline that sends through it.
    pub struct Shared {
        pub(super) video: VideoPipeline,
        pub(super) owner: Arc<SenderResource>,
    }

    impl NativeOwner for Shared {
        fn resource(&self) -> &SenderResource {
            &self.owner
        }

        fn new(resource: SenderResource) -> Self {
            let owner = Arc::new(resource);
            Self {
                video: VideoPipeline::new(Arc::clone(&owner)),
                owner,
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
        fn resource(&self) -> &SenderResource {
            &self.0
        }

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
/// with a [`SendAdvertiser`](crate::discovery_server::SendAdvertiser).
#[derive(Debug)]
pub enum RawSend {}

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
impl CompressedAudioSendMode for CompressedAttached {}
#[cfg(feature = "advanced")]
impl mode_sealed::Sealed for QueueAttached {
    type Native = Exclusive;
}
#[cfg(feature = "advanced")]
impl SenderMode for QueueAttached {}
#[cfg(feature = "advanced")]
impl AudioSendMode for QueueAttached {}
#[cfg(feature = "advanced")]
impl CompressedAudioSendMode for QueueAttached {}

/// An NDI source that sends video, audio and metadata.
///
/// Create one with [`Sender::new`]. Dropping the sender completes any
/// in-flight asynchronous video and removes the source from the network,
/// unless a [`SendAdvertiser`](crate::discovery_server::SendAdvertiser) registration
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
/// several threads at once, except the asynchronous video operations,
/// `submit_video`, `submit_video_copy` and `reclaim_video`, which
/// take `&mut self`. Tally polls preserve changes for a waiting thread;
/// concurrent tally waits serialize and each change is consumed once.
pub struct Sender<M: SenderMode = RawSend> {
    inner: Inner,
    // The owner of the native sender, with the video pipeline of a mode that
    // sends uncompressed video.
    native: <M as mode_sealed::Sealed>::Native,
    _ndi: Ndi,
    _mode: PhantomData<M>,
}

// SAFETY: The descriptor borrows this frame's initialized owned storage and
// terminated metadata; moving it does not move its allocations. The loan's
// reserved tail remains inaccessible until the owning stream releases it.
unsafe impl in_flight::Lend for Sendable<VideoFrame> {
    fn lend(&mut self) -> in_flight::Lent {
        in_flight::Lent::Frame(self.owned_video_descriptor())
    }
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
/// [`in_flight::InFlight`] retains a stream's frame. Native synchronization and release
/// happen together through a private [`in_flight::Stream`] paired by its owner
/// with that owner's live sender. No synchronization token or independently
/// supplied handle can release another sender's loan. Exclusive senders use
/// [`in_flight::Lending`] to destroy the sender before releasing all its loans.
/// A slot dropped without release leaks its frame rather than freeing it.
///
/// [NDI-Send]: https://docs.ndi.video/all/developing-with-ndi/sdk/ndi-send
pub(crate) mod in_flight {
    use std::mem;

    #[cfg(feature = "advanced")]
    use super::Exclusive;
    use super::{async_flush_frame_ptr, PreparedVideo, SendHandle, SenderResource};
    #[cfg(all(feature = "advanced", ndi_has_advanced_scatter_send))]
    use crate::advanced::{ScatterSendable, ScatterVideoLoan};
    use crate::{frames::Sendable, sys::bindings::NDIlib_video_frame_v2_t, VideoFrame};
    use crate::{frames::VideoSource, pixels::Pixels, Result};
    use std::sync::{Arc, Mutex};

    /// A slot paired by its owning pipeline or loan owner with its sender.
    /// Fields are private and there is no constructor accepting a free slot
    /// and handle, so synchronizing callers cannot exchange either half.
    pub(crate) struct Stream<'a, T> {
        slot: &'a mut InFlight<T>,
        sender: SendHandle<'a>,
    }

    /// An uncompressed sender's native owner and all of its video storage.
    /// Pairing stays in this module, including during destruction.
    pub(super) struct VideoPipeline {
        owner: Arc<SenderResource>,
        state: Mutex<VideoState>,
    }

    #[derive(Default)]
    pub(super) struct VideoState {
        pub(super) in_flight: InFlight<Sendable<VideoFrame>>,
        pub(super) staging: Pixels,
        pub(super) spare: Option<VideoFrame>,
    }

    struct VideoPair<'a> {
        stream: Stream<'a, Sendable<VideoFrame>>,
        staging: &'a mut Pixels,
        spare: &'a mut Option<VideoFrame>,
    }

    impl VideoPipeline {
        pub(super) fn new(owner: Arc<SenderResource>) -> Self {
            Self {
                owner,
                state: Mutex::new(VideoState::default()),
            }
        }

        fn synchronize<R>(&self, call: impl FnOnce(VideoPair<'_>) -> R) -> R {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let VideoState {
                in_flight,
                staging,
                spare,
            } = &mut *state;
            call(VideoPair {
                stream: Stream {
                    slot: in_flight,
                    sender: self.owner.handle(),
                },
                staging,
                spare,
            })
        }

        fn exclusive(&mut self) -> VideoPair<'_> {
            let VideoState {
                in_flight,
                staging,
                spare,
            } = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
            VideoPair {
                stream: Stream {
                    slot: in_flight,
                    sender: self.owner.handle(),
                },
                staging,
                spare,
            }
        }

        pub(super) fn send(&self, frame: Sendable<&(impl VideoSource + ?Sized)>) -> Result<()> {
            self.synchronize(|mut pair| {
                let prepared = PreparedVideo::authorized(frame, pair.staging);
                pair.stream.send(prepared);
            });
            Ok(())
        }

        pub(super) fn submit(&mut self, frame: Sendable<VideoFrame>) -> Option<VideoFrame> {
            self.exclusive()
                .stream
                .submit(frame)
                .map(Sendable::into_frame)
        }

        pub(super) fn copy(&mut self, frame: Sendable<&(impl VideoSource + ?Sized)>) {
            let mut pair = self.exclusive();
            let owned = frame.copied(pair.spare.take());
            *pair.spare = pair.stream.submit(owned).map(Sendable::into_frame);
        }

        pub(super) fn flush(&self) {
            self.synchronize(|mut pair| pair.stream.flush());
        }

        pub(super) fn finish(&mut self) -> Option<VideoFrame> {
            let mut pair = self.exclusive();
            pair.stream.flush();
            *pair.spare = None;
            pair.stream.slot.take_retained().map(Sendable::into_frame)
        }

        #[cfg(test)]
        pub(super) fn test_state(&self) -> std::sync::MutexGuard<'_, VideoState> {
            self.state.lock().unwrap()
        }

        #[cfg(all(feature = "advanced", ndi_has_advanced_send_completion))]
        pub(super) fn retire(mut self) -> Option<VideoFrame> {
            self.finish()
        }
    }

    impl Drop for VideoPipeline {
        fn drop(&mut self) {
            // Advertisements may retain the native resource, so drain this
            // pipeline while its own owner and every pointee remain live.
            self.exclusive().stream.flush();
        }
    }

    /// Proof that a native sender has been destroyed, which ends the SDK's
    /// access to every frame it was lent, on every stream.
    ///
    /// [`destroy`] makes it; [`InFlight::into_released`] takes it by
    /// reference, since it releases any number of the destroyed sender's
    /// slots.
    #[cfg(feature = "advanced")]
    #[must_use = "the proof releases the frames of the destroyed sender"]
    pub(crate) struct Destroyed {
        _private: (),
    }

    #[cfg(feature = "advanced")]
    const _: () = assert!(std::mem::size_of::<Destroyed>() == 0);

    /// The loan storage attached to one exclusive native sender.
    #[cfg(feature = "advanced")]
    pub(crate) trait Loans {
        type Recovered;
        fn recover(self, destroyed: &Destroyed) -> Self::Recovered;
        fn release(self, destroyed: &Destroyed);
    }

    /// One owner for an exclusive sender and every native loan it retains.
    ///
    /// The fields stay live until this module takes them exactly once during
    /// close or Drop. Native destruction happens while loans remain owned.
    #[cfg(feature = "advanced")]
    pub(crate) struct Lending<M, L: Loans>
    where
        M: super::SenderMode + super::mode_sealed::Sealed<Native = Exclusive>,
    {
        sender: mem::ManuallyDrop<super::Sender<M>>,
        loans: mem::ManuallyDrop<L>,
    }

    #[cfg(feature = "advanced")]
    impl<M, L: Loans> Lending<M, L>
    where
        M: super::SenderMode + super::mode_sealed::Sealed<Native = Exclusive>,
    {
        pub(crate) fn new(sender: super::Sender<M>, loans: L) -> Self {
            Self {
                sender: mem::ManuallyDrop::new(sender),
                loans: mem::ManuallyDrop::new(loans),
            }
        }

        pub(crate) fn sender(&self) -> &super::Sender<M> {
            &self.sender
        }
        pub(crate) fn loans(&self) -> &L {
            &self.loans
        }
        pub(crate) fn parts(&mut self) -> LoanPair<'_, M, &mut L> {
            LoanPair {
                sender: &self.sender,
                loans: &mut self.loans,
            }
        }

        pub(crate) fn close(self) -> L::Recovered {
            let mut this = mem::ManuallyDrop::new(self);
            // SAFETY: consuming self suppresses Drop, and neither field is
            // accessed after being taken. Loans remain live during destruction.
            let sender = unsafe { mem::ManuallyDrop::take(&mut this.sender) };
            let destroyed = destroy(sender);
            // SAFETY: sender destruction returned, and this field is taken once.
            let loans = unsafe { mem::ManuallyDrop::take(&mut this.loans) };
            loans.recover(&destroyed)
        }
    }

    /// A loan view paired by its owner with that owner's live sender.
    /// Neither field can be supplied independently by a synchronizing caller.
    #[cfg(feature = "advanced")]
    pub(crate) struct LoanPair<'a, M: super::SenderMode, L> {
        sender: &'a super::Sender<M>,
        loans: L,
    }

    #[cfg(feature = "advanced")]
    impl<'a, M: super::SenderMode, L> LoanPair<'a, M, L> {
        pub(crate) fn sender(&self) -> &'a super::Sender<M> {
            self.sender
        }

        pub(crate) fn loans(&self) -> &L {
            &self.loans
        }
    }

    #[cfg(feature = "advanced")]
    impl<'a, M: super::SenderMode, L> LoanPair<'a, M, &'a mut L> {
        /// Queues synchronize their own shared callback state internally.
        pub(crate) fn shared(self) -> LoanPair<'a, M, &'a L> {
            LoanPair {
                sender: self.sender,
                loans: self.loans,
            }
        }
    }

    #[cfg(feature = "advanced")]
    impl<'a>
        LoanPair<'a, crate::advanced::CompressedAttached, &'a mut crate::advanced::PendingStreams>
    {
        /// Submit a frame whose type already proves send admission.
        pub(crate) fn submit_pending(
            &mut self,
            frame: crate::advanced::CompressedVideoFrame,
        ) -> Option<crate::advanced::SubmittedVideo> {
            self.submit_prepared(crate::advanced::PendingVideo::Contiguous(frame))
        }

        // Rejection preserves every block of the caller's owned frame.
        #[allow(clippy::result_large_err)]
        pub(crate) fn submit_scatter(
            &mut self,
            frame: crate::advanced::ScatterVideoFrame,
        ) -> std::result::Result<
            Option<crate::advanced::SubmittedVideo>,
            crate::Rejected<crate::advanced::ScatterVideoFrame>,
        > {
            let proof = crate::advanced::ScatterSendable::new(frame, self.sender.runtime())?;
            Ok(self.submit_prepared(crate::advanced::PendingVideo::Scatter(proof)))
        }

        fn submit_prepared(
            &mut self,
            next: crate::advanced::PendingVideo,
        ) -> Option<crate::advanced::SubmittedVideo> {
            let tier = next.tier();
            Stream {
                slot: self.loans.slot(tier),
                sender: self.sender.handle(),
            }
            .submit(next)
            .map(crate::advanced::PendingVideo::into_submitted)
        }

        /// A null asynchronous flush synchronizes this sender's two streams
        /// together. Release both slots inside this owner operation, then
        /// recover their owned frames.
        pub(crate) fn finish(&mut self) -> crate::advanced::ReclaimedVideo {
            use crate::StreamTier::{Preview, Program};
            if self.loans.slot(Program).is_in_flight() || self.loans.slot(Preview).is_in_flight() {
                // One native flush synchronizes both slots of this paired
                // sender. Keep both releases inside this owner operation.
                let mut program = Stream {
                    slot: self.loans.slot(Program),
                    sender: self.sender.handle(),
                };
                program.flush_native();
                program.slot.mark_released();
                self.loans.slot(Preview).mark_released();
            }
            self.loans.take_retained()
        }
    }

    #[cfg(feature = "advanced")]
    impl<'a> LoanPair<'a, crate::advanced::QueueAttached, &'a crate::advanced::QueueLoans> {
        /// Admission and native dispatch use this pair's own callback and sender.
        /// No caller can supply an entry prepared by another queue.
        pub(crate) fn try_submit_queued(
            &mut self,
            frame: impl Into<crate::advanced::QueuedVideoFrame>,
        ) -> std::result::Result<
            crate::advanced::SubmissionId,
            crate::Rejected<crate::advanced::QueuedVideoFrame>,
        > {
            let admitted = self.loans.prepare(self.sender, frame.into())?;
            // SAFETY: this owner pair derived both the live handle and the
            // admission from its own sender and callback. Admission retains
            // every descriptor and pointee before dispatch; the callback may
            // complete synchronously after its state lock has been released.
            unsafe { admitted.lent.submit(self.sender.handle(), admitted.pointer) };
            Ok(admitted.id)
        }
    }

    #[cfg(feature = "advanced")]
    impl<M, L: Loans> Drop for Lending<M, L>
    where
        M: super::SenderMode + super::mode_sealed::Sealed<Native = Exclusive>,
    {
        fn drop(&mut self) {
            // SAFETY: Drop runs once, with no remaining borrower of either
            // field. The loan storage is held until destruction supplies proof.
            let sender = unsafe { mem::ManuallyDrop::take(&mut self.sender) };
            let destroyed = destroy(sender);
            // SAFETY: destruction ended every loan and the field is taken once.
            let loans = unsafe { mem::ManuallyDrop::take(&mut self.loans) };
            loans.release(&destroyed);
        }
    }

    /// A frame an asynchronous video send can lend to the SDK.
    ///
    /// # Safety
    ///
    /// The descriptors [`lend`](Self::lend) returns point only into
    /// allocations the frame owns, which moving the frame does not move: the
    /// SDK goes on using them after the frame has moved into its slot, and
    /// the slot gives no access to a frame in flight.
    pub(crate) unsafe trait Lend {
        fn lend(&mut self) -> Lent;
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
        Scatter(ScatterSendable<ScatterVideoLoan>),
    }

    impl Lent {
        pub(crate) fn descriptor(&self) -> &NDIlib_video_frame_v2_t {
            match self {
                Self::Frame(frame) => frame,
                #[cfg(all(feature = "advanced", ndi_has_advanced_scatter_send))]
                Self::Scatter(proof) => proof.descriptor(),
            }
        }

        #[cfg(all(feature = "advanced", ndi_has_advanced_scatter_send))]
        pub(crate) fn descriptor_mut(&mut self) -> &mut NDIlib_video_frame_v2_t {
            match self {
                Self::Frame(frame) => frame,
                Self::Scatter(proof) => proof.descriptor_mut(),
            }
        }

        /// Submit through the one native dispatch used by ordinary and queued
        /// asynchronous sending. Null-frame flushing is a distinct operation.
        ///
        /// # Safety
        /// `sender` is live and owns the stream. `descriptor` describes this
        /// loan and stays readable through the native call; with completion
        /// callbacks it stays readable until completion. Every pointee,
        /// including metadata, scatter list, arrays and payload, stays allocated
        /// and inaccessible until native synchronization or completion ends
        /// the loan. Any displaced loan stays live through this call's return.
        unsafe fn submit(
            &self,
            sender: SendHandle<'_>,
            descriptor: *const NDIlib_video_frame_v2_t,
        ) {
            match self {
                Self::Frame(_) => {
                    // SAFETY: the caller guarantees descriptor and loan lifetimes.
                    unsafe {
                        sender
                            .core
                            .NDIlib_send_send_video_async_v2(sender.instance.0, descriptor)
                    };
                }
                #[cfg(all(feature = "advanced", ndi_has_advanced_scatter_send))]
                Self::Scatter(proof) => {
                    // SAFETY: the same lifetime contract also covers the stable
                    // scatter list and the arrays and payload it names.
                    unsafe { proof.submit(sender.instance.0, descriptor) };
                }
            }
        }
    }

    /// One stream's retained storage. Only its private owner-bound Stream
    /// can synchronize native access and mark an in-flight frame released.
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

        fn mark_released(&mut self) -> Option<&T> {
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
        pub(crate) fn into_released(mut self, _destroyed: &Destroyed) -> Option<T> {
            match mem::take(&mut self.0) {
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

    impl<T> Drop for InFlight<T> {
        fn drop(&mut self) {
            if let State::InFlight(frame) = mem::take(&mut self.0) {
                // Retain native pointees even if a future owner forgets proof.
                mem::forget(frame);
                if !std::thread::panicking() {
                    debug_assert!(
                        false,
                        "in-flight frame dropped without synchronization or destruction"
                    );
                }
            }
        }
    }

    impl<T: Lend> Stream<'_, T> {
        /// Install the new loan before dispatch and return displaced storage
        /// only after this same sender's synchronizing call has returned.
        fn submit(&mut self, mut next: T) -> Option<T> {
            let lent = next.lend();
            let mut displaced = InFlight(mem::replace(&mut self.slot.0, State::InFlight(next)));
            // SAFETY: pairing is private to the slot's owning pipeline or
            // LoanPair. Both new and displaced allocations remain owned
            // through dispatch; Lend's contract retains every stable pointee.
            unsafe { lent.submit(self.sender, lent.descriptor()) };
            displaced.mark_released();
            displaced.take_retained()
        }
    }

    impl<T> Stream<'_, T> {
        fn flush_native(&mut self) {
            // SAFETY: this pair's owner retains its live sender and slots.
            // A true null descriptor drains that sender's asynchronous loans.
            unsafe {
                self.sender.core.NDIlib_send_send_video_async_v2(
                    self.sender.instance.0,
                    async_flush_frame_ptr(),
                )
            };
        }

        fn flush(&mut self) {
            if self.slot.is_in_flight() {
                self.flush_native();
                self.slot.mark_released();
            }
        }
    }

    impl Stream<'_, Sendable<VideoFrame>> {
        fn send(&mut self, frame: PreparedVideo<'_>) {
            // SAFETY: the pair retains the slot's live sender. PreparedVideo
            // borrows initialized input or padded staging through this call;
            // the synchronous send also ends this sender's previous loan.
            unsafe {
                self.sender
                    .core
                    .NDIlib_send_send_video_v2(self.sender.instance.0, &frame.raw)
            };
            self.slot.mark_released();
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
    fn destroy<M>(sender: super::Sender<M>) -> Destroyed
    where
        M: super::SenderMode + super::mode_sealed::Sealed<Native = Exclusive>,
    {
        let super::Sender {
            native: Exclusive(resource),
            ..
        } = sender;
        // The only owner: dropping it calls `NDIlib_send_destroy`.
        drop(resource);
        Destroyed { _private: () }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn an_unproven_drop_leaks_the_native_loan() {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static DROPS: AtomicUsize = AtomicUsize::new(0);
            struct Counted;
            impl Drop for Counted {
                fn drop(&mut self) {
                    DROPS.fetch_add(1, Ordering::Relaxed);
                }
            }
            DROPS.store(0, Ordering::Relaxed);
            let slot = InFlight(State::InFlight(Counted));
            let result = std::panic::catch_unwind(|| drop(slot));
            assert_eq!(result.is_err(), cfg!(debug_assertions));
            assert_eq!(
                DROPS.load(Ordering::Relaxed),
                0,
                "native pointees must remain allocated"
            );
        }

        /// A frame in flight comes out of its slot only with a proof, and is
        /// completed only once.
        #[test]
        fn a_slot_releases_its_frame_only_with_proof() {
            let mut slot = InFlight(State::InFlight(1u8));
            assert!(slot.take_retained().is_none());
            assert!(slot.is_in_flight());
            assert_eq!(slot.mark_released(), Some(&1));
            assert!(!slot.is_in_flight());
            assert_eq!(slot.mark_released(), None);
            assert_eq!(slot.take_retained(), Some(1));
            assert!(slot.take_retained().is_none());
            assert!(slot.frame().is_none());
        }

        #[cfg(feature = "advanced")]
        #[test]
        fn a_displaced_or_destroyed_slot_hands_back_what_it_held() {
            let release = |state| {
                let mut slot = InFlight(state);
                slot.mark_released();
                slot.take_retained()
            };
            assert_eq!(release(State::InFlight(1u8)), Some(1));
            assert_eq!(release(State::Retained(2)), Some(2));
            assert_eq!(release(State::Idle), None);
            let destroyed = Destroyed { _private: () };
            let unsent = |state| InFlight(state).into_released(&destroyed);
            assert_eq!(unsent(State::InFlight(3u8)), Some(3));
            assert_eq!(unsent(State::Retained(4)), Some(4));
            assert_eq!(unsent(State::Idle), None);
        }
    }
}

use in_flight::VideoPipeline;

impl<M: SenderMode> fmt::Debug for Sender<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sender")
            .field("name", &self.native.resource()._name)
            .field("mode", &std::any::type_name::<M>())
            .finish_non_exhaustive()
    }
}

/// A borrowed video frame that references external pixel data.
///
/// [`Sender::send_video`] sends it synchronously; the buffer can be reused
/// as soon as the call returns. [`Sender::submit_video_copy`] copies its
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
// Its initialized report vouches for all storage padding and, only after the
// caller opts in, the known room after the last pixel. Direct SDK reads are
// decided by the common rule over that quantity.
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
    fn frame_metadata(&self) -> FrameMetadataRef<'_> {
        FrameMetadataRef::from_text(self.metadata)
    }
    /// A frame built for sending carries no timestamp; [`Self::to_raw`]
    /// sends none.
    #[inline]
    fn sdk_timestamp(&self) -> Option<crate::SdkTimestamp> {
        None
    }

    fn initialized(&self) -> crate::frames::Initialized {
        crate::frames::Initialized::Storage {
            room_after: if self.allow_padding_and_tail {
                self.tail_room
            } else {
                0
            },
        }
    }
}

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
        Self::authorized(Sendable::for_test(frame), staging)
    }

    fn authorized(
        frame: Sendable<&'a (impl VideoParts + ?Sized)>,
        staging: &'a mut Pixels,
    ) -> Self {
        let raw = if frame.frame().can_send_direct() {
            frame.video_descriptor()
        } else {
            frame.packed(staging).video_descriptor()
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
        /// [`Self::storage`] includes, so the active pixels of a padded buffer
        /// can be read in place.
    }
    timestamp {
        /// Application input has no SDK submission timestamp.
    }
    data(self -> Option<&[u8]>) {
        /// Contiguous active pixels, or `None` when row or plane padding separates
        /// them. Use [`Self::plane`] and [`Self::planes`] for padded active rows, or
        /// [`Self::storage`] for the validated storage including row padding.
    } { crate::frames::VideoPartsExt::active_data(self) }
    to_owned(self) {
        /// Copy this borrowed frame into a validated owned frame.
        ///
        /// Active pixels are copied into tightly packed planes; row padding is
        /// skipped and the owned stride can shrink. Metadata is copied separately;
        /// no reference to the source is retained. [`Self::copy_into`] makes the
        /// same copy into an existing frame, reusing its allocations.
        ///
        /// The copy cannot fail: building the frame validated everything it
        /// relies on, and the packed layout of a valid frame is valid.
    } {
        crate::frames::VideoPartsExt::owned_copy(self)
    }
    copy_into {
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
        /// [`Sender::submit_video`] keeps the frame it is given until the next
        /// send, and returns the previous one, which the SDK has finished with:
        /// that frame is the destination of the next copy.
        /// [`Sender::submit_video_copy`] keeps its own reusable frames for
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
        ///     spare = sender.submit_video(owned)?;
        /// }
        /// sender.reclaim_video();
        /// # Ok(())
        /// # }
        /// ```
    }
    sends_in_place {
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
        /// assert_eq!(in_place.data().unwrap().len(), 720 * 480 * 2);
        ///
        /// let owned = VideoFrame::builder()
        ///     .resolution(720, 480)
        ///     .pixel_format(uyvy)
        ///     .build()?;
        /// assert!(owned.sends_in_place());
        /// # Ok(())
        /// # }
        /// ```
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
    /// [`Self::storage`] never includes the rest. For UYVY/UYVA where the SDK
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
            aspect_ratio_input: PictureAspectRatio::SQUARE_PIXELS,
            scan_type: ScanType::Progressive,
            timecode: Timecode::SYNTHESIZE,
            metadata: None,
            readable_after: 0,
            allow_padding_and_tail: false,
        }
    }

    /// Get the pixel data: exactly the frame's validated storage, including
    /// any row padding. Bytes of the source buffer beyond the frame are not
    /// part of it. [`Self::plane`] and [`Self::planes`] borrow the active
    /// rows alone.
    pub fn storage(&self) -> &[u8] {
        self.data
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
        self.header = self.header.with_timecode(timecode);
        self
    }

    #[cfg(test)]
    fn to_raw(self) -> NDIlib_video_frame_v2_t {
        self.video_descriptor()
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
    aspect_ratio_input: PictureAspectRatio,
    scan_type: ScanType,
    timecode: Timecode,
    metadata: Option<&'buf CStr>,
    /// Readable bytes the caller vouches for after `data`.
    readable_after: usize,
    allow_padding_and_tail: bool,
}

impl std::fmt::Debug for BorrowedVideoFrameBuilder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BorrowedVideoFrameBuilder")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixel_format", &self.pixel_format)
            .field("line_stride_bytes", &self.line_stride)
            .field("frame_rate", &self.frame_rate)
            .field("picture_aspect_ratio", &self.aspect_ratio_input)
            .field("scan_type", &self.scan_type)
            .field("timecode", &self.timecode)
            .field("metadata", &self.metadata.map(CStr::to_string_lossy))
            .field("readable_after", &self.readable_after)
            .field(
                "allow_send_from_padding_and_tail",
                &self.allow_padding_and_tail,
            )
            .field("data (bytes)", &self.data.len())
            .finish()
    }
}

impl<'buf> BorrowedVideoFrameBuilder<'buf> {
    /// Set the first-plane stride in bytes. It must be positive and hold the
    /// active row. Use [`Self::clear_line_stride_bytes`] for the packed stride.
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
    pub fn picture_aspect_ratio(mut self, ratio: PictureAspectRatio) -> Self {
        self.aspect_ratio_input = ratio;
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
        let picture_aspect_ratio = self.aspect_ratio_input;
        let layout = ValidatedVideoLayout::new_uncompressed(
            self.pixel_format,
            self.width,
            self.height,
            self.line_stride,
        )?;
        let Some(data) = self.data.get(..layout.data_len_bytes()) else {
            return Err(Error::InvalidFrame(
                crate::FrameDefect::VideoBufferTooSmall {
                    format: self.pixel_format,
                    actual: self.data.len(),
                    minimum: layout.data_len_bytes(),
                },
            ));
        };
        let metadata = self.metadata.map(MetadataText::from_cstr).transpose()?;
        Ok(BorrowedVideoFrame {
            layout,
            header: VideoHeader::new(
                self.frame_rate,
                picture_aspect_ratio,
                self.scan_type,
                self.timecode,
            ),
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
                #[cfg(feature = "advanced")]
                None,
                |core, raw| core.named().NDIlib_send_create(raw),
            )
        }
    }

    /// The one creation path: create a sender through `create`, which is handed the core table and the native
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
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(
            &'static sys::Core,
            &NDIlib_send_create_t,
        ) -> sys::table::Called<NDIlib_send_instance_t>,
    ) -> Result<Self> {
        let name = CString::new(options.name.as_str())?;
        let groups = crate::finder::list_cstring(options.groups.as_deref(), "groups")?;
        let core = &ndi.api().core;
        let instance = create(
            core,
            &NDIlib_send_create_t {
                p_ndi_name: name.as_ptr(),
                p_groups: groups.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
                clock_video: options.clock_video,
                clock_audio: options.clock_audio,
            },
        )
        .created()?
        .as_ptr();
        let instance = SendInstance(instance);
        Ok(Self {
            inner: Inner {
                instance,
                core,
                metadata_capture: SenderMetadataGate::default(),
                tally: TallyState::default(),
                #[cfg(ndi_has_advanced_keyframe_demand)]
                keyframe_changes: crate::keyframe_changes::KeyframeChanges::default(),
            },
            native: NativeOwner::new(SenderResource {
                instance,
                _name: name,
                _groups: groups,
                failover: parking_lot::Mutex::new(RetainedSources::default()),
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
    /// Returns [`Error::InvalidCString`] when the name contains a NUL byte,
    /// [`Error::InvalidInput`] for a group containing a comma or NUL or an empty group, [`Error::UnsupportedCapability`] when the options carry an
    /// Advanced configuration that the runtime or this build cannot take, and
    /// [`Error::NativeOperationFailed`] when the SDK creates no sender.
    #[doc(alias = "NDIlib_send_create")]
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
        // Shared owns exactly two references: its owner and the pipeline's.
        // With no other reference, no registration can create another one.
        if Arc::strong_count(&owner) != 2 {
            return Err(Self {
                inner,
                native: Shared { video, owner },
                _ndi,
                _mode,
            });
        }
        let retained = video.retire();
        let resource =
            Arc::try_unwrap(owner).unwrap_or_else(|_| unreachable!("exclusive pipeline owner"));
        let sender = Sender {
            inner,
            native: Exclusive(resource),
            _ndi,
            _mode: PhantomData,
        };
        Ok((sender, retained))
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
    #[doc(alias = "NDIlib_send_send_video_v2")]
    pub fn send_video(&self, frame: &(impl VideoSource + ?Sized)) -> Result<()> {
        let authorized = Sendable::borrowed(frame, &self._ndi)?;
        self.native.video.send(authorized)
    }

    /// Submit an owned frame without copying its pixel or metadata allocations.
    ///
    /// The sender takes ownership before calling the SDK. The next async send
    /// synchronizes the previous send and returns its owned frame for reuse.
    /// The first send returns `None`. The returned frame may have been submitted
    /// by ownership or copying. A copied submission instead retains the frame
    /// it displaces for its next copy. Call [`Self::reclaim_video`] to
    /// recover the final frame.
    ///
    /// Synchronous sends and explicit flushes complete the operation but retain
    /// the owned frame for recovery by the next async send or [`Self::reclaim_video`].
    /// No SDK completion callback is registered, including with Advanced SDK.
    ///
    /// # Errors
    ///
    /// Returns the frame, unsent, with [`Error::UnsupportedCapability`] if
    /// this sender's runtime cannot send its pixel format (the planar 4:4:4
    /// formats need
    /// [`RuntimeFeature::Planar444Video`](crate::RuntimeFeature::Planar444Video)).
    /// No pending send is changed.
    #[doc(alias = "NDIlib_send_send_video_async_v2")]
    pub fn submit_video(
        &mut self,
        frame: VideoFrame,
    ) -> std::result::Result<Option<VideoFrame>, Rejected<VideoFrame>> {
        let authorized = self.require_video_layout(frame)?;
        Ok(self.native.video.submit(authorized))
    }

    /// Drain the SDK and return the final owned frame, if any.
    ///
    /// Frames submitted through either owned or copied sending are returned.
    /// Any spare storage retained for copying is released. Repeated calls
    /// return `None`.
    #[doc(alias = "NDIlib_send_send_video_async_v2")]
    pub fn reclaim_video(&mut self) -> Option<VideoFrame> {
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
    /// Recover the final copy with [`Self::reclaim_video`], which also
    /// releases the spare. Owned submissions through [`Self::submit_video`]
    /// still return the frame they displace.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] if this sender's runtime
    /// cannot send the frame's pixel format, checked before anything is
    /// copied. No pending send is changed on failure.
    #[doc(alias = "NDIlib_send_send_video_async_v2")]
    pub fn submit_video_copy(&mut self, frame: &(impl VideoSource + ?Sized)) -> Result<()> {
        let authorized = Sendable::borrowed(frame, &self._ndi)?;
        self.native.video.copy(authorized);
        Ok(())
    }

    /// Flush pending async video operations synchronously.
    ///
    /// When the SDK may still read a pending frame, sends a null video frame
    /// pointer and blocks until that asynchronous operation completes. If no
    /// frame is in flight, returns without calling the SDK.
    ///
    /// After this returns, the SDK has released the pending buffer. The sender
    /// retains the frame; use [`Self::reclaim_video`] or an owned
    /// submission through [`Self::submit_video`] to recover it without copying.
    #[doc(alias = "NDIlib_send_send_video_async_v2")]
    pub fn flush_video(&self) {
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
    #[doc(alias = "NDIlib_send_send_audio_v3")]
    pub fn send_audio(&self, frame: &(impl AudioSource + ?Sized)) {
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_send_audio_v3(self.inner.instance.0, &frame.audio_descriptor());
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
    fn handle(&self) -> SendHandle<'_> {
        SendHandle {
            instance: self.inner.instance,
            core: self.inner.core,
            _owner: PhantomData,
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
    pub(crate) fn require_video_layout<F: VideoParts>(
        &self,
        frame: F,
    ) -> std::result::Result<Sendable<F>, Rejected<F>> {
        Sendable::new(frame, &self._ndi)
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
    /// [`Error::InvalidUtf8`] for non-UTF-8 text, and [`Error::UnexpectedSdkResult`]
    /// when the SDK returns a frame type other than metadata or no frame.
    #[doc(alias = "NDIlib_send_capture")]
    pub fn capture_metadata(&self, timeout: Duration) -> Result<Option<MetadataFrame>> {
        Ok(self
            .capture_metadata_ref(timeout)?
            .map(|frame| frame.to_owned()))
    }

    /// Borrow metadata sent back by connected receivers without copying it.
    ///
    /// The frame retains capture exclusion until it is dropped, after the SDK
    /// has freed its metadata. Another capture spends its timeout waiting for
    /// that release. Video sends use a separate synchronization path.
    /// The frame can move to a scoped worker thread; its borrow keeps this
    /// sender alive through the matching native free.
    ///
    /// # Errors
    ///
    /// As for [`Self::capture_metadata`].
    #[doc(alias = "NDIlib_send_capture")]
    pub fn capture_metadata_ref(&self, timeout: Duration) -> Result<Option<MetadataFrameRef<'_>>> {
        let metadata = self._ndi.table::<sys::SenderMetadata>()?;
        let deadline = Deadline::after(timeout)?;
        let Some(lease) = self.inner.metadata_capture.acquire_until(deadline.end()) else {
            return Ok(None);
        };
        let mut raw = NDIlib_metadata_frame_t {
            length: 0,
            timecode: 0,
            p_data: ptr::null_mut(),
        };
        let remaining = deadline.remaining_ms();
        // SAFETY: this borrowed sender is live, the lease serializes capture
        // through matching free, and raw is writable descriptor storage.
        let called = unsafe {
            metadata
                .named()
                .NDIlib_send_capture(self.inner.instance.0, &mut raw, remaining)
        };
        let kind = *called.value();
        if kind == NDIlib_frame_type_e_NDIlib_frame_type_metadata {
            // SAFETY: this exact sender/runtime just delivered raw and the
            // lease retains its capture/free exclusion until the borrowed
            // view releases the guard. Sender metadata follows the SDK's
            // bounded NUL-terminated string contract; from_sender validates
            // that contract before exposing any text.
            let guard = unsafe {
                crate::capture::Guard::<crate::capture::SenderMetadataFree>::new(
                    &self._ndi,
                    crate::capture::SenderMetadataInstance {
                        instance: self.inner.instance.0,
                        metadata,
                    },
                    raw,
                )
            };
            MetadataFrameRef::from_sender(guard, lease).map(Some)
        } else if kind == NDIlib_frame_type_e_NDIlib_frame_type_none {
            Ok(None)
        } else {
            Err(Error::UnexpectedSdkResult(
                crate::SdkResultDefect::UnexpectedFrameType {
                    operation: called.function(),
                    kind,
                },
            ))
        }
    }

    /// Sends a metadata frame.
    ///
    /// Accepts owned or captured metadata without copying its text in the
    /// wrapper. The SDK copies the text before returning.
    #[doc(alias = "NDIlib_send_send_metadata")]
    pub fn send_metadata<S: MetadataSource + ?Sized>(&self, metadata: &S) {
        let raw = metadata.metadata_raw();
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
    #[doc(alias = "NDIlib_send_get_tally")]
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
    #[doc(alias = "NDIlib_send_get_tally")]
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

    /// The current number of active connections, without waiting.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] if the SDK returns a negative count.
    #[doc(alias = "NDIlib_send_get_no_connections")]
    pub fn connection_count(&self) -> Result<u32> {
        self.connection_count_with_timeout(0)
    }

    /// Wait up to `timeout` for at least one connected receiver.
    ///
    /// Returns the nonzero connection count as soon as a receiver is present,
    /// or `None` when the wait expires. A zero timeout reads the current count.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] for a negative native count, and
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`].
    #[doc(alias = "NDIlib_send_get_no_connections")]
    pub fn wait_for_connection(&self, timeout: Duration) -> Result<Option<NonZeroU32>> {
        self.connection_count_with_timeout(to_ms_checked(timeout)?)
            .map(NonZeroU32::new)
    }

    fn connection_count_with_timeout(&self, timeout_ms: u32) -> Result<u32> {
        // SAFETY: this borrow keeps the sender and its runtime alive through
        // the bounded query, which writes no Rust-owned state.
        let count = unsafe {
            self.inner
                .core
                .named()
                .NDIlib_send_get_no_connections(self.inner.instance.0, timeout_ms)
        };
        // The SDK count is i32, so every nonnegative value fits u32.
        crate::receiver::native_count(count).map(|count| count as u32)
    }

    /// Remove all metadata registered for delivery to each new connection.
    #[doc(alias = "NDIlib_send_clear_connection_metadata")]
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
    #[doc(alias = "NDIlib_send_add_connection_metadata")]
    pub fn add_connection_metadata<S: MetadataSource + ?Sized>(&self, metadata: &S) {
        let raw = metadata.metadata_raw();
        // The borrowed C string remains live through this synchronous SDK call.
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_add_connection_metadata(self.inner.instance.0, &raw)
        }
    }

    /// Set the validated failover source.
    #[doc(alias = "NDIlib_send_set_failover")]
    pub fn set_failover(&self, source: &Source) {
        self.native
            .resource()
            .failover
            .lock()
            .replace(Some(source), |source| {
                // SAFETY: the native owner and its locked retention slot keep the
                // previous and replacement descriptors live through this request.
                unsafe {
                    self.inner
                        .core
                        .NDIlib_send_set_failover(self.inner.instance.0, source)
                }
            });
    }

    /// Clear the failover source previously configured with [`Self::set_failover`].
    #[doc(alias = "NDIlib_send_set_failover")]
    pub fn clear_failover(&self) {
        self.native
            .resource()
            .failover
            .lock()
            .replace(None, |source| {
                // SAFETY: the sender is live and the retention helper keeps the
                // previous descriptor until this null replacement returns.
                unsafe {
                    self.inner
                        .core
                        .NDIlib_send_set_failover(self.inner.instance.0, source)
                }
            });
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
    #[doc(alias = "NDIlib_send_get_source_name")]
    pub fn source(&self) -> Result<Source> {
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        let source_ptr = unsafe {
            self.inner
                .core
                .named()
                .NDIlib_send_get_source_name(self.inner.instance.0)
        };
        // SAFETY: the SDK sender owns this live descriptor and its strings
        // through the synchronous copy performed here.
        unsafe { Source::try_from_raw(source_ptr) }
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
/// let options = SenderOptions::new("My Sender").groups(["Studio"]);
/// let sender = Sender::new(&ndi, &options)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SenderOptions {
    name: String,
    groups: Option<Vec<String>>,
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
    pub fn configured_groups(&self) -> Option<&[String]> {
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

    /// Set the discovery groups the sender is advertised
    /// in, instead of those of the NDI configuration.
    #[must_use]
    pub fn groups(mut self, groups: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.groups = Some(groups.into_iter().map(Into::into).collect());
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
    fn metadata_capture_lease_moves_threads_and_retains_the_wait_budget() {
        let gate = SenderMetadataGate::default();
        let lease = gate
            .acquire_until(Instant::now() + Duration::from_secs(1))
            .unwrap();
        std::thread::scope(|scope| {
            let (release, released) = std::sync::mpsc::channel();
            let worker = scope.spawn(move || {
                released.recv().unwrap();
                drop(lease);
            });
            let started = Instant::now();
            assert!(gate
                .acquire_until(started + Duration::from_millis(25))
                .is_none());
            assert!(started.elapsed() >= Duration::from_millis(20));
            release.send(()).unwrap();
            worker.join().unwrap();
            let (_, allocations) = crate::test_alloc::allocation_count(|| {
                let lease = gate.acquire_until(Instant::now()).unwrap();
                drop(lease);
            });
            assert_eq!(allocations, 0, "capture permits must not allocate");
        });
    }

    #[test]
    fn metadata_capture_lease_releases_during_unwind() {
        fn traits<T: Send + Sync + std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
        traits::<SenderMetadataLease<'static>>();
        let gate = SenderMetadataGate::default();
        let panic = std::panic::catch_unwind(|| {
            let _lease = gate.acquire_until(Instant::now()).unwrap();
            panic!("unwind with an outstanding metadata lease");
        });
        assert!(panic.is_err());
        assert!(gate.acquire_until(Instant::now()).is_some());
    }

    fn check_metadata_free_before_capture_release(path: &str) {
        use crate::capture::{Guard, SenderMetadataFree, SenderMetadataInstance};
        use std::sync::{atomic::AtomicUsize, OnceLock};

        struct Observation {
            gate: SenderMetadataGate,
            frees: AtomicUsize,
            released_early: AtomicBool,
        }

        unsafe fn observe_free(instance: SenderMetadataInstance, _: &mut NDIlib_metadata_frame_t) {
            // SAFETY: the synthetic instance borrows this observation until
            // the metadata guard has invoked its sole release callback.
            let observation = unsafe { &*instance.instance.cast::<Observation>() };
            observation.frees.fetch_add(1, Ordering::Relaxed);
            observation.released_early.store(
                observation.gate.acquire_until(Instant::now()).is_some(),
                Ordering::Relaxed,
            );
        }

        static TABLE: OnceLock<sys::SenderMetadata> = OnceLock::new();
        let table = TABLE.get_or_init(|| {
            // Synthetic release uses observe_free, never these placeholder
            // entry points. The table only completes the instance's type.
            sys::SenderMetadata::load(&sys::table::ExportsWhere(|_| true))
                .unwrap()
                .unwrap()
        });
        let observation = Observation {
            gate: SenderMetadataGate::default(),
            frees: AtomicUsize::new(0),
            released_early: AtomicBool::new(false),
        };
        let text = if path == "validation" {
            c"\xff"
        } else {
            c"<held/>"
        };
        let raw = NDIlib_metadata_frame_t {
            length: text.to_bytes().len() as i32,
            p_data: text.as_ptr().cast_mut(),
            timecode: 0,
        };
        let lease = observation.gate.acquire_until(Instant::now()).unwrap();
        // SAFETY: the observation and terminated text outlive the guard; the
        // synthetic callback observes release and does not free either one.
        let guard = unsafe {
            Guard::<SenderMetadataFree>::with_test_free(
                SenderMetadataInstance {
                    instance: (&observation as *const Observation).cast_mut().cast(),
                    metadata: table,
                },
                raw,
                observe_free,
            )
        };
        let frame = MetadataFrameRef::from_sender(guard, lease);
        match path {
            "validation" => assert!(matches!(frame, Err(Error::InvalidUtf8(_)))),
            "unwind" => {
                let frame = frame.unwrap();
                let panic = std::panic::catch_unwind(|| {
                    let _frame = frame;
                    panic!("unwind while upstream metadata is borrowed");
                });
                assert!(panic.is_err());
            }
            "normal" => drop(frame.unwrap()),
            _ => unreachable!(),
        }
        assert_eq!(observation.frees.load(Ordering::Relaxed), 1);
        assert!(!observation.released_early.load(Ordering::Relaxed));
        assert!(observation.gate.acquire_until(Instant::now()).is_some());
    }

    #[test]
    fn sender_metadata_guard_frees_before_releasing_capture() {
        check_metadata_free_before_capture_release("normal");
    }

    #[test]
    fn sender_metadata_validation_failure_frees_before_releasing_capture() {
        check_metadata_free_before_capture_release("validation");
    }

    #[test]
    fn sender_metadata_unwind_frees_before_releasing_capture() {
        check_metadata_free_before_capture_release("unwind");
    }

    #[test]
    fn initialized_extents_preserve_direct_sending_for_every_format_and_scan() {
        use crate::frames::Initialized;
        for format in PixelFormat::ALL {
            for scan in ScanType::ALL {
                for height in [4, 6] {
                    let owned = VideoFrame::builder()
                        .resolution(18, height)
                        .pixel_format(format)
                        .scan_type(scan)
                        .build()
                        .unwrap();
                    assert!(owned.can_send_direct(), "owned {format:?} {scan:?}");
                    assert_eq!(
                        owned.initialized(),
                        Initialized::Storage {
                            room_after: owned.tail_room()
                        }
                    );
                    assert_eq!(
                        <&VideoFrame as VideoParts>::initialized(&&owned),
                        owned.initialized()
                    );
                    for padding in [0, 8] {
                        let stride = format.line_stride_bytes(18).unwrap() + padding;
                        let layout = ValidatedVideoLayout::new_uncompressed(
                            format,
                            18,
                            height,
                            Some(stride),
                        )
                        .unwrap();
                        let room = send_room(&layout, scan);
                        for extra in [0, room] {
                            let bytes = vec![0; layout.data_len_bytes() + extra];
                            for allow in [false, true] {
                                let borrowed =
                                    BorrowedVideoFrame::builder(&bytes, 18, height, format)
                                        .line_stride_bytes(stride)
                                        .scan_type(scan)
                                        .allow_send_from_padding_and_tail(allow)
                                        .build()
                                        .unwrap();
                                let expected = room == 0
                                    || (allow && layout.trailing_padding() + extra >= room);
                                assert_eq!(borrowed.sends_in_place(), expected,
                                    "{format:?} {scan:?} height={height} padding={padding} extra={extra} allow={allow}");
                                assert_eq!(borrowed.sends_in_place(), expected);
                                assert_eq!(
                                    <&BorrowedVideoFrame<'_> as VideoParts>::initialized(
                                        &&borrowed
                                    ),
                                    borrowed.initialized()
                                );
                                let mut storage = Pixels::default();
                                let packed = borrowed.copy_active_pixels(&mut storage);
                                assert!(packed.can_send_direct(), "staging {format:?} {scan:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_short_owned_extent_stages_synchronous_sends_and_is_repaired_before_lending() {
        use crate::frames::descriptor_tests::assert_extent;
        if PixelFormat::Uyvy.send_tail_room() == 0 {
            return;
        }
        let frame = VideoFrame::builder()
            .resolution(18, 6)
            .pixel_format(PixelFormat::Uyvy)
            .scan_type(ScanType::Interlaced)
            .data(vec![128; 18 * 6 * 2])
            .build()
            .unwrap()
            .without_send_room_for_test();
        assert_eq!(frame.tail_room(), 0);
        assert!(!frame.sends_in_place());
        let mut staging = Pixels::default();
        let raw = PreparedVideo::new(&frame, &mut staging).raw;
        assert_ne!(raw.p_data.cast_const(), frame.data().as_ptr());
        assert_extent(&raw, staging.as_ptr(), staging.len() + staging.room());
        let mut owned = Sendable::for_test(frame);
        let lent = in_flight::Lend::lend(&mut owned);
        assert!(owned.frame().can_send_direct());
        assert_eq!(
            owned.tail_room(),
            send_room(owned.layout(), owned.header().scan_type())
        );
        assert_extent(
            lent.descriptor(),
            owned.data().as_ptr(),
            owned.data().len() + owned.tail_room(),
        );
    }

    #[test]
    fn captured_video_sending_uses_initialized_active_rows_and_preserves_descriptors() {
        use crate::capture::{FrameSyncVideoFree, Guard, VideoKind};
        use crate::{FrameSyncVideoRef, VideoFrameRef};
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
                assert!(staging.room_bytes().iter().all(|&byte| byte == 0));
                assert_eq!(
                    staging.room(),
                    send_room(&source.layout().packed(), source.header().scan_type())
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
                        .picture_aspect_ratio(PictureAspectRatio::new(1.25).unwrap())
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
                        VideoFrameRef::new(Guard::<VideoKind>::with_test_free(
                            instance,
                            raw,
                            |instance, _| {
                                (*(instance as *const AtomicUsize)).fetch_add(1, Ordering::SeqCst);
                            },
                        ))
                    }
                    .unwrap();
                    let framesync = unsafe {
                        FrameSyncVideoRef::new(Guard::<FrameSyncVideoFree>::with_test_free(
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
                        receiver.initialized(),
                        crate::frames::Initialized::ActiveRows
                    );
                    assert_eq!(
                        framesync.initialized(),
                        crate::frames::Initialized::ActiveRows
                    );
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

    #[test]
    fn padded_prepared_and_copied_descriptors_fit_their_own_storage() {
        use crate::{capture::Guard, frames::descriptor_tests::assert_extent, VideoFrameRef};
        for (format, stride) in [
            (PixelFormat::Bgra, 512),
            (PixelFormat::Uyvy, 256),
            (PixelFormat::P216, 256),
        ] {
            let planes = if format == PixelFormat::P216 { 2 } else { 1 };
            let storage = vec![0u8; stride * 32 * planes];
            let frame = BorrowedVideoFrame::builder(&storage, 64, 32, format)
                .line_stride_bytes(stride)
                .build()
                .unwrap();
            // Captured rows are padded and need staging even for BGRA.
            // SAFETY: the guard borrows the initialized, still-live storage;
            // the synthetic release callback owns and frees nothing.
            let captured = unsafe {
                VideoFrameRef::new(Guard::with_test_free(
                    ptr::null_mut(),
                    frame.video_descriptor(),
                    |_, _| {},
                ))
            }
            .unwrap();
            let mut staging = Pixels::default();
            let prepared = PreparedVideo::authorized(Sendable::for_test(&captured), &mut staging);
            let raw = prepared.raw;
            assert_ne!(raw.p_data.cast_const(), storage.as_ptr());
            assert_extent(&raw, staging.as_ptr(), staging.len() + staging.room());
            // This is the packed copy submitted by submit_video_copy;
            // its authorization is still that of the padded source.
            let mut owned = Sendable::for_test(&frame).copied(None);
            #[allow(irrefutable_let_patterns)]
            let in_flight::Lent::Frame(raw) = in_flight::Lend::lend(&mut owned) else {
                panic!("pixels")
            };
            assert_eq!(
                unsafe { raw.__bindgen_anon_1.line_stride_in_bytes } as usize,
                64 * if format == PixelFormat::Bgra { 4 } else { 2 }
            );
            assert_extent(
                &raw,
                owned.data().as_ptr(),
                owned.data().len() + owned.tail_room(),
            );
            if let Some(mut sender) = test_sender() {
                sender.submit_video_copy(&frame).unwrap();
                {
                    let state = sender.native.video.test_state();
                    let retained = state.in_flight.frame().unwrap();
                    assert_extent(
                        &retained.as_test_raw(),
                        retained.data().as_ptr(),
                        retained.data().len() + retained.tail_room(),
                    );
                }
                sender.reclaim_video();
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
        let expected_ptr = frame.metadata_raw().p_data;
        for _ in 0..3 {
            sender.send_metadata(&frame);
            assert_eq!(frame.metadata_raw().p_data, expected_ptr);
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
        assert!(sender.submit_video(first).unwrap().is_none());
        {
            let video = sender.native.video.test_state();
            let Some(frame) = video.in_flight.frame() else {
                panic!("missing owned frame")
            };
            assert_eq!(frame.as_test_raw().p_data, pixels as *mut u8);
            assert_eq!(frame.as_test_raw().p_metadata, metadata);
        }
        let second = test_owned_frame(22);
        let second_pixels = second.data().as_ptr();
        let recycled = sender.submit_video(second).unwrap().unwrap();
        assert_eq!(recycled.data().as_ptr(), pixels);
        assert_eq!(recycled.metadata_cstr().unwrap().as_ptr(), metadata);
        assert_eq!(recycled.metadata(), Some("<frame value='11'/>"));
        let final_frame = sender.reclaim_video().unwrap();
        assert_eq!(final_frame.data().as_ptr(), second_pixels);
        assert!(sender.reclaim_video().is_none());
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
            .picture_aspect_ratio(PictureAspectRatio::new(1.25).unwrap())
            .timecode(Timecode::from_ticks(1234))
            .metadata(c"<metadata/>")
            .build()
            .unwrap();
        let shorter = BorrowedVideoFrame::builder(&pixels, 16, 16, PixelFormat::Bgra)
            .metadata(c"<short/>")
            .build()
            .unwrap();
        sender.submit_video_copy(&source).unwrap();
        sender.submit_video_copy(&source).unwrap();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..4 {
                sender.submit_video_copy(&shorter).unwrap();
                sender.submit_video_copy(&shorter).unwrap();
                sender.submit_video_copy(&source).unwrap();
                sender.submit_video_copy(&source).unwrap();
            }
        });
        assert_eq!(allocations, 0);
        let copy = sender.reclaim_video().unwrap();
        assert_eq!(copy.data(), pixels);
        assert_eq!(copy.frame_rate(), source.frame_rate());
        assert_eq!(copy.picture_aspect_ratio().get(), 1.25);
        assert_eq!(copy.timecode(), Timecode::from_ticks(1234));
        assert_eq!(copy.metadata(), Some("<metadata/>"));
        assert!(sender.native.video.test_state().spare.is_none());
        assert!(sender.reclaim_video().is_none());
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
        sender.submit_video_copy(&present).unwrap();
        sender.submit_video_copy(&present).unwrap();
        // Exclusive async sends do not lock the state. Warm this test's
        // inspection lock, whose first use may allocate on some platforms.
        drop(sender.native.video.test_state());
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..4 {
                for (source, expected) in [
                    (&absent, None),
                    (&empty, Some("")),
                    (&present, Some("<metadata/>")),
                ] {
                    sender.submit_video_copy(source).unwrap();
                    let state = sender.native.video.test_state();
                    assert_eq!(state.in_flight.frame().unwrap().metadata(), expected);
                }
            }
        });
        assert_eq!(allocations, 0);
        assert_eq!(
            sender.reclaim_video().unwrap().metadata(),
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
        sender.submit_video_copy(&small).unwrap();
        sender.submit_video_copy(&small).unwrap();
        // Each of the two reusable pixel buffers grows once, with no new
        // metadata storage needed. No further growth is needed afterward.
        for _ in 0..2 {
            let ((), allocations) = crate::test_alloc::allocation_count(|| {
                sender.submit_video_copy(&large).unwrap();
            });
            assert_eq!(allocations, 1);
        }
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            sender.submit_video_copy(&large).unwrap();
            sender.submit_video_copy(&large).unwrap();
        });
        assert_eq!(allocations, 0);
        // Longer metadata likewise grows each reusable metadata buffer once.
        for _ in 0..2 {
            let ((), allocations) = crate::test_alloc::allocation_count(|| {
                sender.submit_video_copy(&longer).unwrap();
            });
            assert_eq!(allocations, 1);
        }
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..4 {
                sender.submit_video_copy(&longer).unwrap();
                sender.submit_video_copy(&large).unwrap();
            }
        });
        assert_eq!(allocations, 0);
        sender.reclaim_video();
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
                                assert_eq!(staging.room(), send_room(&layout.packed(), scan));
                                assert!(staging.room_bytes().iter().all(|&byte| byte == 0));
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
                let len = format.buffer_len(18, 4, None).unwrap();
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
            .picture_aspect_ratio(PictureAspectRatio::new(1.25).unwrap())
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
        assert_eq!(staging.room_bytes(), [0; 64]);
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
            let state = sender.native.video.test_state();
            (
                state.staging.as_ptr(),
                state.staging.as_slice().to_vec(),
                state.staging.room_bytes().to_vec(),
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
        sender.submit_video(frame).unwrap();
        sender.send_video(&test_owned_frame(9)).unwrap();
        sender.flush_video();
        assert_eq!(sender.reclaim_video().unwrap().data().as_ptr(), pixels);

        // A copied send retains the released owned frame for its next copy.
        let owned = test_owned_frame(2);
        let owned_pixels = owned.data().as_ptr();
        sender.submit_video(owned).unwrap();
        let data = vec![3; 1024];
        let metadata = CString::new("<copy/>").unwrap();
        {
            let borrowed = BorrowedVideoFrame::builder(&data, 16, 16, PixelFormat::Bgra)
                .metadata(&metadata)
                .build()
                .unwrap();
            sender.submit_video_copy(&borrowed).unwrap();
        }
        {
            let video = sender.native.video.test_state();
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
            let video = sender.native.video.test_state();
            let Some(copy) = video.in_flight.frame() else {
                panic!("missing copied snapshot")
            };
            assert_eq!(copy.data(), vec![3; 1024]);
            assert_eq!(copy.metadata().unwrap(), "<copy/>");
        }
        // Copies are ordinary owned frames and are returned for reuse too.
        let copied = sender.submit_video(test_owned_frame(4)).unwrap().unwrap();
        assert_eq!(copied.data()[0], 3);
        assert_eq!(copied.metadata(), Some("<copy/>"));
        sender.reclaim_video().unwrap();
        sender.flush_video();
    }

    #[test]
    fn every_synchronizing_call_completes_a_frame_once_and_recovery_keeps_its_identity() {
        let Some(mut sender) = test_sender() else {
            return;
        };
        let in_flight = |sender: &Sender| sender.native.video.test_state().in_flight.is_in_flight();
        let frame = |value| {
            let frame = test_owned_frame(value);
            let pixels = frame.data().as_ptr();
            (frame, pixels)
        };

        // A synchronous send completes the frame in flight; the next
        // submission returns that original allocation.
        let (a, a_pixels) = frame(1);
        assert!(sender.submit_video(a).unwrap().is_none());
        assert!(in_flight(&sender));
        sender.send_video(&test_owned_frame(9)).unwrap();
        assert!(!in_flight(&sender));
        let (b, b_pixels) = frame(2);
        let previous = sender.submit_video(b).unwrap().unwrap();
        assert_eq!(previous.data().as_ptr(), a_pixels);

        // A flush does the same, and a second flush finds nothing in flight.
        sender.flush_video();
        assert!(!in_flight(&sender));
        sender.flush_video();
        let (c, c_pixels) = frame(3);
        let previous = sender.submit_video(c).unwrap().unwrap();
        assert_eq!(previous.data().as_ptr(), b_pixels);

        // A submission completes the in-flight frame it displaces.
        let (d, d_pixels) = frame(4);
        let previous = sender.submit_video(d).unwrap().unwrap();
        assert_eq!(previous.data().as_ptr(), c_pixels);
        assert!(in_flight(&sender));

        // Finishing recovers a frame a synchronous send already completed.
        sender.send_video(&test_owned_frame(9)).unwrap();
        assert_eq!(sender.reclaim_video().unwrap().data().as_ptr(), d_pixels);
        assert!(sender.reclaim_video().is_none());

        // A sender can also be dropped after an explicit flush.
        sender.submit_video(frame(5).0).unwrap();
        sender.flush_video();
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
        assert!(sender.submit_video(test_owned_frame(1)).unwrap().is_none());
        if supported {
            sender.send_video(&frame).unwrap();
            let previous = sender.submit_video(frame).unwrap().unwrap();
            assert_eq!(previous.data()[0], 1);
            sender.submit_video_copy(&borrowed).unwrap();
            assert_eq!(
                sender
                    .native
                    .video
                    .test_state()
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
                        capability: crate::Capability::Runtime(RuntimeFeature::Planar444Video),
                        ..
                    }
                )
            };
            assert!(unsupported(&sender.send_video(&frame).unwrap_err()));
            let rejected = sender.submit_video(frame).unwrap_err();
            assert!(unsupported(rejected.error()));
            assert_eq!(rejected.into_inner().data().as_ptr(), pixels);
            assert!(unsupported(
                &sender.submit_video_copy(&borrowed).unwrap_err()
            ));
            assert!(sender.native.video.test_state().in_flight.is_in_flight());
        }
        assert!(sender.reclaim_video().is_some());
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
                None,
                Some(format.line_stride_bytes(4).unwrap()),
                Some(format.line_stride_bytes(4).unwrap() + 8),
            ] {
                let layout = ValidatedVideoLayout::new_uncompressed(format, 4, 4, stride).unwrap();
                // A longer buffer is accepted; the frame is its layout's extent.
                let data = vec![19; layout.data_len_bytes() + 3];
                let metadata = c"<pixels>é</pixels>";
                let rate = FrameRate::new(30000, 1001).unwrap();
                let (borrowed, allocations) = crate::test_alloc::allocation_count(|| {
                    let builder = BorrowedVideoFrame::builder(&data, 4, 4, format);
                    let builder = match stride {
                        Some(stride) => builder.line_stride_bytes(stride),
                        None => builder,
                    };
                    let frame = builder
                        .frame_rate(rate)
                        .picture_aspect_ratio(PictureAspectRatio::new(1.25).unwrap())
                        .scan_type(ScanType::Interlaced)
                        .timecode(Timecode::from_ticks(0))
                        .metadata(metadata)
                        .build()
                        .unwrap();
                    for _ in 0..3 {
                        assert_eq!(frame.metadata(), Some("<pixels>é</pixels>"));
                        assert_eq!(frame.storage(), &data[..layout.data_len_bytes()]);
                        assert_eq!(frame.pixel_format(), format);
                    }
                    frame
                });
                assert_eq!(allocations, 0);
                let (owned, allocations) =
                    crate::test_alloc::allocation_count(|| borrowed.to_owned());
                assert_eq!(allocations, 2);
                assert_eq!(owned.frame_rate(), rate);
                assert_eq!(owned.picture_aspect_ratio().get(), 1.25);
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
                let short =
                    BorrowedVideoFrame::builder(&data[..layout.data_len_bytes() - 1], 4, 4, format);
                let short = match stride {
                    Some(stride) => short.line_stride_bytes(stride),
                    None => short,
                };
                assert!(short.build().is_err());
            }
            for stride in [0, 1, i32::MAX.unsigned_abs() as usize, usize::MAX] {
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
        assert_eq!(frame.picture_aspect_ratio().get(), 0.0);

        let frame = BorrowedVideoFrame::builder(&buffer, 64, 36, PixelFormat::Uyvy)
            .picture_aspect_ratio(PictureAspectRatio::SQUARE_PIXELS)
            .build()
            .unwrap()
            .with_timecode(Timecode::from_ticks(1234));
        assert_eq!(frame.timecode(), Timecode::from_ticks(1234));
        assert_eq!(frame.picture_aspect_ratio().get(), 0.0);
        let raw = frame.to_raw();
        assert_eq!((raw.timecode, raw.picture_aspect_ratio), (1234, 0.0));
        assert_eq!(raw.timestamp, NDIlib_recv_timestamp_undefined);

        for bad in [-1.0, f32::NAN, f32::INFINITY] {
            assert!(PictureAspectRatio::new(bad).is_err(), "{bad}");
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
                    builder = builder.picture_aspect_ratio(PictureAspectRatio::new(ratio).unwrap());
                }
                let frame = builder.build().unwrap();
                let expected = ratio.unwrap_or(0.0);
                assert_eq!(frame.picture_aspect_ratio().get(), expected);
                assert_eq!(frame.to_raw().picture_aspect_ratio, expected);
                let mut copied = frame.to_owned();
                assert_eq!(copied.picture_aspect_ratio().get(), expected);
                copied.set_picture_aspect_ratio(PictureAspectRatio::new(2.0).unwrap());
                frame.copy_into(&mut copied);
                assert_eq!(copied.picture_aspect_ratio().get(), expected);
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
        assert_eq!(frame.data().unwrap().len(), 1920 * 1080 * 4);
    }

    #[test]
    fn test_borrowed_builder_undersized_buffer() {
        // Buffer too small should fail
        let buffer = vec![0u8; 1920 * 1080 * 4 - 1];
        let result = borrowed(&buffer, 1920, 1080, PixelFormat::Bgra);
        assert!(result.is_err());
        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(matches!(
                msg,
                crate::FrameDefect::VideoBufferTooSmall { .. }
            ));
            assert!(matches!(
                msg,
                crate::FrameDefect::VideoBufferTooSmall {
                    format: PixelFormat::Bgra,
                    ..
                }
            ));
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
        let expected_size = PixelFormat::Nv12.buffer_len(1920, 1080, None).unwrap();
        let buffer = vec![0u8; expected_size];
        assert!(borrowed(&buffer, 1920, 1080, PixelFormat::Nv12).is_ok());
    }

    #[test]
    fn test_borrowed_builder_i420() {
        // I420 planar format
        let expected_size = PixelFormat::I420.buffer_len(640, 480, None).unwrap();
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
        assert_eq!(frame.picture_aspect_ratio().get(), 0.0);
        assert_eq!(frame.scan_type(), ScanType::Progressive);
        assert_eq!(frame.timecode(), Timecode::SYNTHESIZE);
        assert_eq!(frame.data().unwrap().len(), buffer.len());
        assert_eq!(frame.line_stride_bytes(), 1920 * 4);
        assert!(frame.metadata().is_none());
        // Copies share the borrow, and Debug summarizes rather than dumps pixels.
        let copy = frame;
        assert_eq!(
            copy.data().unwrap().as_ptr(),
            frame.data().unwrap().as_ptr()
        );
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
            let expected_size = format.buffer_len(width, height, None).unwrap();

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

#[cfg(any(
    ndi_test_send_ownership,
    ndi_test_send_ownership_control,
    ndi_test_send_ownership_types
))]
#[path = "../tests/ui/internal/send_ownership.rs"]
mod ownership_contract;
