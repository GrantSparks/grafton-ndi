//! Asynchronous video sending with a bounded queue and completion
//! notification.

use std::{
    collections::{hash_map::Entry as IndexEntry, HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Condvar, Mutex,
    },
    time::Duration,
};

#[cfg(any(test, ndi_has_advanced_send_completion))]
use std::ffi::c_void;

use super::{
    send::PendingVideo,
    send_slots::{DescriptorLease, Released, SlotKey},
    CompressedVideoFrame, QueueAttached, ScatterVideoFrame, SubmittedVideo,
};
use crate::{
    frames::Sendable,
    sender::in_flight::{Destroyed, Lend, Lending, Lent, LoanPair, Loans},
    sys::{
        advanced::{native, ScatterSend, SendCompletion},
        bindings::*,
    },
    timeout::Deadline,
    Error, Ndi, QueueLimit, Rejected, Result, Sender, SenderOptions, VideoFrame,
};

use super::aliasable::AliasableBox;
#[cfg(any(test, ndi_has_advanced_send_completion))]
use super::send_slots::DescriptorSlots;

/// Limits on what a [`QueuedSender`] holds at once.
///
/// A frame counts from submission until the application takes it back, so a
/// frame the SDK has finished with but that has not been collected still uses
/// its share. The byte budget includes retained payload capacity, even when a
/// compressed frame uses only part of a pooled vector. The defaults are 8 frames
/// and 512 MiB. Construction reserves one stable frame descriptor per frame.
/// Scatter frames own their scatter-list descriptors. Completed frames retain
/// their slots until collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueueLimits {
    frames: usize,
    bytes: usize,
}

impl Default for QueueLimits {
    fn default() -> Self {
        Self::new()
    }
}

impl QueueLimits {
    /// Requested maximum frame count.
    #[must_use]
    pub fn configured_max_frames(&self) -> usize {
        self.frames
    }

    /// Requested maximum retained bytes.
    #[must_use]
    pub fn configured_max_bytes(&self) -> usize {
        self.bytes
    }

    /// The default limits: 8 frames and 512 MiB.
    pub fn new() -> Self {
        Self {
            frames: 8,
            bytes: 512 * 1024 * 1024,
        }
    }

    /// Set the maximum number of frames held, from 1 to 65 536.
    #[must_use]
    pub fn max_frames(mut self, frames: usize) -> Self {
        self.frames = frames;
        self
    }

    /// Set the maximum retained bytes of frame data and metadata, including
    /// unused payload capacity reserved by pooled vectors. A frame larger
    /// than this limit is refused with [`Error::InvalidInput`]; collecting
    /// completions cannot make it fit.
    #[must_use]
    pub fn max_bytes(mut self, bytes: usize) -> Self {
        self.bytes = bytes;
        self
    }

    #[cfg(any(test, ndi_has_advanced_send_completion))]
    fn validate(self) -> Result<Self> {
        if self.frames == 0
            || self.frames > 65_536
            || self.bytes == 0
            || self.bytes > isize::MAX as usize
        {
            return Err(Error::InvalidInput(crate::InputDefect::QueueLimits));
        }
        Ok(self)
    }
}
/// Identifies one submission to a [`QueuedSender`]. Identifiers increase with
/// each submission and are never reused by the same queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubmissionId(u64);

impl SubmissionId {
    /// The identifier as a number.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A video frame a [`QueuedSender`] can send.
///
/// Each variant owns its complete, nonempty video storage. Cloning copies the
/// bytes into independent allocations; submission moves that storage into the
/// queue until it is collected. Scatter frames retain their separate blocks.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum QueuedVideoFrame {
    /// An uncompressed frame.
    Pixels(VideoFrame),
    /// A compressed frame.
    Compressed(CompressedVideoFrame),
    /// A compressed frame held in separate owned blocks.
    Scatter(ScatterVideoFrame),
}

impl From<VideoFrame> for QueuedVideoFrame {
    fn from(v: VideoFrame) -> Self {
        Self::Pixels(v)
    }
}

impl From<CompressedVideoFrame> for QueuedVideoFrame {
    fn from(v: CompressedVideoFrame) -> Self {
        Self::Compressed(v)
    }
}

impl From<ScatterVideoFrame> for QueuedVideoFrame {
    fn from(frame: ScatterVideoFrame) -> Self {
        Self::Scatter(frame)
    }
}

impl QueuedVideoFrame {
    fn retained_bytes(&self) -> usize {
        match self {
            Self::Pixels(v) => v.retained_bytes(),
            Self::Compressed(v) => v.retained_bytes(),
            Self::Scatter(v) => v.retained_bytes(),
        }
    }

    /// Consume the input only after checking the capability its storage needs.
    fn authorize(
        self,
        sender: &Sender<QueueAttached>,
    ) -> std::result::Result<AuthorizedVideo, Rejected<Self>> {
        self.authorize_with(
            |frame| sender.require_video_layout(frame),
            || sender.runtime().table::<ScatterSend>(),
        )
    }

    /// The same structural authorization with capability lookup supplied by
    /// the caller, so SDK-free boundary tests exercise the production arms.
    fn authorize_with(
        self,
        pixels: impl FnOnce(
            VideoFrame,
        ) -> std::result::Result<Sendable<VideoFrame>, Rejected<VideoFrame>>,
        scatter: impl FnOnce() -> Result<&'static ScatterSend>,
    ) -> std::result::Result<AuthorizedVideo, Rejected<Self>> {
        match self {
            Self::Pixels(frame) => pixels(frame)
                .map(AuthorizedVideo::Pixels)
                .map_err(|rejected| rejected.map(Self::Pixels)),
            Self::Compressed(frame) => Ok(AuthorizedVideo::Compressed(frame)),
            Self::Scatter(frame) => {
                let table = match scatter() {
                    Ok(table) => table,
                    Err(error) => return Err(Rejected::new(error, Self::Scatter(frame))),
                };
                native!(ScatterSend, table => {
                    Ok(AuthorizedVideo::Scatter { frame, table })
                })
            }
        }
    }

    #[cfg(test)]
    fn authorize_for_test(self, scatter: Option<&'static ScatterSend>) -> AuthorizedVideo {
        self.authorize_with(
            |frame| Ok(Sendable::for_test(frame)),
            || {
                scatter.ok_or(Error::UnsupportedCapability {
                    capability: crate::Capability::Runtime(crate::RuntimeFeature::ScatterSend),
                    refusal: crate::CapabilityRefusal::SymbolUnavailable(
                        c"NDIlib_send_send_video_scatter",
                    ),
                })
            },
        )
        .unwrap()
    }
}

/// Authorized payloads do not acquire descriptor storage until admitted.
enum AuthorizedVideo {
    Pixels(Sendable<VideoFrame>),
    Compressed(CompressedVideoFrame),
    #[cfg(ndi_has_advanced_scatter_send)]
    Scatter {
        frame: ScatterVideoFrame,
        table: &'static ScatterSend,
    },
}

impl AuthorizedVideo {
    fn into_frame(self) -> QueuedVideoFrame {
        match self {
            Self::Pixels(frame) => frame.into_frame().into(),
            Self::Compressed(frame) => frame.into(),
            #[cfg(ndi_has_advanced_scatter_send)]
            Self::Scatter { frame, .. } => frame.into(),
        }
    }

    fn retain(self) -> QueuedStorage {
        match self {
            Self::Pixels(frame) => QueuedStorage::Pixels(frame),
            Self::Compressed(frame) => QueuedStorage::Packet(PendingVideo::Contiguous(frame)),
            #[cfg(ndi_has_advanced_scatter_send)]
            Self::Scatter { frame, table } => {
                QueuedStorage::Packet(PendingVideo::Scatter { frame, table })
            }
        }
    }
}

/// Every variant owns the exact capability its payload needs. Packet storage
/// additionally owns the scatter table and stable list when one is needed.
enum QueuedStorage {
    Pixels(Sendable<VideoFrame>),
    Packet(PendingVideo),
}

impl QueuedStorage {
    fn into_frame(self) -> QueuedVideoFrame {
        let packet = match self {
            Self::Pixels(frame) => return QueuedVideoFrame::Pixels(frame.into_frame()),
            Self::Packet(packet) => packet,
        };
        match packet.into_submitted() {
            SubmittedVideo::Contiguous(frame) => QueuedVideoFrame::Compressed(frame),
            SubmittedVideo::Scatter(frame) => QueuedVideoFrame::Scatter(frame),
        }
    }
}

// SAFETY: Pixels owns initialized payload, terminated metadata and its matching
// format proof; its owned loan reserves the complete scan-dependent SDK room
// at descriptor construction. Packet delegates to PendingVideo, which owns its payload and
// metadata and initializes its own stable scatter list before lending it. The
// completion key points into its retained, nonempty first block. Moving this
// value moves none of its allocations; Entry excludes access until completion
// or destruction has ended the native loan.
unsafe impl Lend for QueuedStorage {
    type Authorization = ();

    fn lend(&mut self, (): ()) -> Lent {
        match self {
            Self::Pixels(frame) => frame.lend(()),
            Self::Packet(frame) => frame.lend_for_queue(),
        }
    }
}
/// A frame the SDK has finished with, ready to be reused.
///
/// It was either reported complete or reclaimed by [`QueuedSender::close`]
/// after native destruction ended the SDK's access to it.
#[derive(Clone, Debug)]
pub struct CompletedVideo {
    id: SubmissionId,
    frame: QueuedVideoFrame,
}

impl CompletedVideo {
    /// The identifier [`QueuedSender::try_submit`] returned for this frame.
    pub fn id(&self) -> SubmissionId {
        self.id
    }

    /// The frame.
    pub fn frame(&self) -> &QueuedVideoFrame {
        &self.frame
    }

    /// Take the frame.
    pub fn into_frame(self) -> QueuedVideoFrame {
        self.frame
    }
}

/// One submitted frame and the descriptor the SDK was given for it.
///
/// The entry owns the frame it lent, so nothing can read the frame's bytes
/// while the SDK may still write to them.
struct Entry {
    id: SubmissionId,
    // Owns the fixed header slot until collection or proven destruction.
    slot: DescriptorLease,
    frame: QueuedStorage,
    bytes: usize,
}

impl Entry {
    fn release(self, proof: Released<'_>) -> (CompletedVideo, SlotKey) {
        let Self {
            id, slot, frame, ..
        } = self;
        // Native release covers the payload and its frame-owned scatter list
        // before the header slot's key can reenter admission.
        let completed = CompletedVideo {
            id,
            frame: frame.into_frame(),
        };
        (completed, slot.release(proof))
    }
}

/// Minted only when the SDK identifies the exact in-flight entry as complete.
/// Its private constructor stays in this module, and the token remains paired
/// with that entry until collection releases both its payload and slot.
pub(super) struct Completion {
    _private: (),
}

const _: () = assert!(std::mem::size_of::<Completion>() == 0);

struct ReadyEntry {
    entry: Entry,
    completion: Completion,
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Entry>();
};

pub(crate) struct Admitted {
    pub(crate) id: SubmissionId,
    pub(crate) lent: Lent,
    pub(crate) pointer: *const NDIlib_video_frame_v2_t,
}

/// The frames a queue holds. Each is in exactly one place: in flight until
/// the SDK reports it complete, then ready until it is collected.
struct State {
    // The frames the SDK may still read, by the address of their data.
    in_flight: HashMap<usize, Entry>,
    // The frames the SDK has finished with, in the order it reported them.
    ready: VecDeque<ReadyEntry>,
    bytes: usize,
    next: u64,
    options: QueueLimits,
    // Free keys are the frame capacity. A lease dropped while unwinding
    // retires its key, so the queue may have fewer usable slots than requested.
    free: Vec<SlotKey>,
    // A fallibly reserved submission-thread workspace for rebuilding a map
    // whose deletion tombstones have consumed its insertion capacity.
    rehash: Vec<(usize, Entry)>,
}

impl State {
    /// The frames held, in flight or ready.
    fn retained(&self) -> usize {
        self.in_flight.len() + self.ready.len()
    }
    /// Check all admission bounds before acquiring descriptor storage. Then
    /// initialize a free stable slot and record ownership before dispatch.
    fn admit(
        &mut self,
        frame: AuthorizedVideo,
        bytes: usize,
        protocol_error: &AtomicBool,
    ) -> std::result::Result<Admitted, Rejected<QueuedVideoFrame>> {
        self.admit_with_lend(frame, bytes, protocol_error, |frame| frame.lend(()))
    }

    /// Derive the completion key from the very descriptor handed to the SDK.
    /// Lending needs no header slot: scatter lists belong to their frames.
    fn admit_with_lend(
        &mut self,
        frame: AuthorizedVideo,
        bytes: usize,
        protocol_error: &AtomicBool,
        lend: impl FnOnce(&mut QueuedStorage) -> Lent,
    ) -> std::result::Result<Admitted, Rejected<QueuedVideoFrame>> {
        let Some(total) = self
            .bytes
            .checked_add(bytes)
            .filter(|total| *total <= self.options.bytes)
        else {
            return Err(Rejected::new(
                Error::QueueFull(QueueLimit::Bytes),
                frame.into_frame(),
            ));
        };
        debug_assert!(self.free.len() + self.retained() <= self.options.frames);
        let Some(slot_key) = self.free.pop() else {
            return Err(Rejected::new(
                Error::QueueFull(QueueLimit::Frames),
                frame.into_frame(),
            ));
        };
        let Some(next) = self.next.checked_add(1) else {
            self.free.push(slot_key);
            return Err(Rejected::new(
                Error::InvalidInput(crate::InputDefect::QueueIdentifiersExhausted),
                frame.into_frame(),
            ));
        };
        let mut frame = frame.retain();
        let lent = lend(&mut frame);
        let key = lent.descriptor().p_data as usize;
        if self.in_flight.capacity() == self.in_flight.len() {
            self.rebuild_index();
        }
        let entry = match self.in_flight.entry(key) {
            IndexEntry::Vacant(entry) => entry,
            IndexEntry::Occupied(_) => {
                self.free.push(slot_key);
                protocol_error.store(true, Ordering::Release);
                return Err(Rejected::new(
                    Error::UnexpectedSdkResult(crate::SdkResultDefect::UnknownCompletion),
                    frame.into_frame(),
                ));
            }
        };
        // All rejection checks and lending precede slot initialization. The
        // move-only key can initialize this header only once.
        let slot = slot_key.claim(*lent.descriptor());
        let pointer = slot.as_ptr();
        let id = SubmissionId(self.next);
        self.next = next;
        self.bytes = total;
        entry.insert(Entry {
            id,
            slot,
            frame,
            bytes,
        });
        Ok(Admitted { id, lent, pointer })
    }

    /// HashMap removal can consume insertion capacity without shrinking its
    /// allocation. Draining resets the existing table, and both transfer
    /// buffers were reserved for the maximum retained count at construction.
    /// Entries move without moving any native pointee; the state lock excludes
    /// completion callbacks throughout the rebuild.
    fn rebuild_index(&mut self) {
        debug_assert!(self.rehash.is_empty());
        debug_assert!(self.rehash.capacity() >= self.in_flight.len());
        for pair in self.in_flight.drain() {
            self.rehash.push(pair);
        }
        debug_assert!(self.in_flight.capacity() > self.rehash.len());
        for (key, entry) in self.rehash.drain(..) {
            let IndexEntry::Vacant(slot) = self.in_flight.entry(key) else {
                unreachable!("rebuilding preserves the unique retained keys");
            };
            slot.insert(entry);
        }
        debug_assert!(self.in_flight.capacity() > self.in_flight.len());
    }

    /// Release every retained entry only after native destruction. Both
    /// explicit close and implicit drop use this path, so even an unmatched
    /// completion cannot release a frame while the SDK might still use it.
    fn release_retained(&mut self, destroyed: &Destroyed, mut release: impl FnMut(CompletedVideo)) {
        for ready in self.ready.drain(..) {
            let (completed, key) = ready.entry.release(Released::Destroyed(destroyed));
            release(completed);
            self.free.push(key);
        }
        for (_, entry) in self.in_flight.drain() {
            let (completed, key) = entry.release(Released::Destroyed(destroyed));
            release(completed);
            self.free.push(key);
        }
        self.bytes = 0;
    }

    fn take_retained(&mut self, destroyed: &Destroyed) -> Vec<CompletedVideo> {
        let mut completed = Vec::with_capacity(self.retained());
        let ready = self.ready.len();
        self.release_retained(destroyed, |entry| completed.push(entry));
        // The ready prefix already records completion order. Sorting only the
        // unmatched suffix needs no allocation beyond the returned vector.
        completed[ready..].sort_unstable_by_key(|entry| entry.id.get());
        completed
    }
}

pub(crate) struct CallbackState {
    // Immutable, so permanent admission failures need neither the state lock
    // nor a native descriptor allocation.
    max_bytes: usize,
    state: Mutex<State>,
    changed: Condvar,
    protocol_error: AtomicBool,
}

impl CallbackState {
    /// Validate and retain one entry for this callback's owning sender.
    pub(crate) fn prepare(
        &self,
        sender: &Sender<QueueAttached>,
        frame: QueuedVideoFrame,
    ) -> std::result::Result<Admitted, Rejected<QueuedVideoFrame>> {
        let failure = Rejected::new;
        let bytes = frame.retained_bytes();
        let frame = frame.authorize(sender)?;
        if let Err(error) = self.validate_frame_size(bytes) {
            return Err(failure(error, frame.into_frame()));
        }
        if self.protocol_error.load(Ordering::Acquire) {
            return Err(failure(
                Error::UnexpectedSdkResult(crate::SdkResultDefect::UnknownCompletion),
                frame.into_frame(),
            ));
        }
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.admit(frame, bytes, &self.protocol_error)
        }
    }

    #[cfg(any(test, ndi_has_advanced_send_completion))]
    fn new(options: QueueLimits) -> Result<Box<Self>> {
        let options = options.validate()?;
        use super::send_slots::unavailable;
        let mut in_flight = HashMap::new();
        in_flight.try_reserve(options.frames).map_err(unavailable)?;
        let mut ready = VecDeque::new();
        ready
            .try_reserve_exact(options.frames)
            .map_err(unavailable)?;
        let mut rehash = Vec::new();
        rehash
            .try_reserve_exact(options.frames)
            .map_err(unavailable)?;
        let free = DescriptorSlots::allocate_keys(options.frames)?;
        let state = Box::new(Self {
            max_bytes: options.bytes,
            state: Mutex::new(State {
                in_flight,
                ready,
                bytes: 0,
                next: 1,
                options,
                free,
                rehash,
            }),
            changed: Condvar::new(),
            protocol_error: AtomicBool::new(false),
        });
        // Where the standard library allocates a mutex's or condition
        // variable's operating-system object on first use, as it does on
        // macOS, that first use must not be the SDK's thread reporting a
        // completion, which would then allocate. Use both once here, on the
        // thread creating the queue.
        drop(state.state.lock());
        state.changed.notify_all();
        Ok(state)
    }

    fn retained_frames(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retained()
    }

    fn completion_protocol_failed(&self) -> bool {
        self.protocol_error.load(Ordering::Acquire)
    }

    fn validate_frame_size(&self, bytes: usize) -> Result<()> {
        if bytes > self.max_bytes {
            return Err(Error::InvalidInput(
                crate::InputDefect::QueueFrameTooLarge {
                    bytes,
                    limit: self.max_bytes,
                },
            ));
        }
        Ok(())
    }

    /// Record that the SDK has finished with the frame whose data is at
    /// `address`. Runs on an SDK thread.
    ///
    /// No allocator or user code runs here. Removing the entry from the map
    /// keeps the map's storage (std gives a map's storage back only through
    /// `shrink_to` and `shrink_to_fit`), and `ready` was given room for every
    /// frame the queue can hold before the callback was registered, while
    /// `try_submit` keeps the queue within that many. The entry is moved, so
    /// nothing is freed.
    #[cfg(any(test, ndi_has_advanced_send_completion))]
    fn complete(&self, address: usize) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let State {
            in_flight, ready, ..
        } = &mut *state;
        match in_flight.remove(&address) {
            Some(entry) => {
                let completion = Completion { _private: () };
                ready.push_back(ReadyEntry { entry, completion });
            }
            // No frame in flight has this data: the SDK named a frame twice,
            // or one it was never given.
            None => self.protocol_error.store(true, Ordering::Release),
        }
        self.changed.notify_all();
    }
}
/// The callback storage paired with an exclusive queue sender.
pub(crate) type QueueLoans = AliasableBox<CallbackState>;

/// The completion callback the SDK calls, on its own threads, when it has
/// finished with an asynchronously sent frame.
#[cfg(any(test, ndi_has_advanced_send_completion))]
unsafe extern "C" fn completion(opaque: *mut c_void, frame: *const NDIlib_video_frame_v2_t) {
    if opaque.is_null() {
        return;
    }
    // SAFETY: `opaque` is the callback state registered with the sender. The
    // SDK makes no call after the sender is destroyed, and the owning
    // `QueuedSender` releases the state only after that.
    let state = unsafe { &*opaque.cast::<CallbackState>() };
    if frame.is_null() {
        state.protocol_error.store(true, Ordering::Release);
        state.changed.notify_all();
        return;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // The SDK may pass its own copy of the descriptor, so a submission is
        // recognized by its data allocation, which is unique while retained.
        // SAFETY: non-null was checked, and the SDK passes a readable descriptor.
        state.complete(unsafe { (*frame).p_data } as usize);
    }));
    if result.is_err() {
        state.protocol_error.store(true, Ordering::Release);
        state.changed.notify_all();
    }
}

/// A sender that queues video frames and reports when each has been sent.
///
/// An ordinary asynchronous send must wait for the previous frame before it
/// can accept the next. This sender instead keeps every submitted frame until
/// the SDK reports it complete, so submission never waits on a slow
/// connection, up to the limits in [`QueueLimits`]. No application code runs
/// on an SDK thread: completed frames are collected with
/// [`try_next_completed`](Self::try_next_completed) or
/// [`next_completed`](Self::next_completed) and can be reused.
///
/// It is [`Send`] and [`Sync`]: it can be moved to, and its read-only methods
/// called from, any thread, while submitting and collecting frames takes it
/// mutably. [`close`](Self::close) destroys the native sender and returns
/// every retained frame with its submission id. Dropping it destroys the
/// sender and frees those frames instead. Both wait, without a timeout, for
/// the SDK to finish with every frame still queued, so run teardown on a
/// blocking thread when using an async executor.
///
/// [`sender`](Self::sender) lends the native sender in the [`QueueAttached`]
/// mode, which has the operations every sender shares (the source, metadata
/// in both directions, tally, connections, failover, and the Advanced
/// controls) and [`send_audio`](Sender::send_audio), but none of the video
/// sends, and which cannot be registered with a
/// [`SendAdvertiser`](crate::discovery_server::SendAdvertiser).
///
/// # Examples
///
/// ```no_run
/// use std::time::Duration;
/// use grafton_ndi::{
///     advanced::{QueueLimits, QueuedSender},
///     SenderOptions, VideoFrame, Ndi,
/// };
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = Ndi::new()?;
/// let mut sender = QueuedSender::new(
///     &ndi,
///     &SenderOptions::new("Queued"),
///     &QueueLimits::default().max_frames(4),
/// )?;
/// let id = sender.try_submit(VideoFrame::builder().resolution(1920, 1080).build()?)?;
/// if let Some(done) = sender.next_completed(Duration::from_secs(1))? {
///     assert_eq!(done.id(), id);
/// }
/// # Ok(())
/// # }
/// ```
pub struct QueuedSender {
    lending: Lending<QueueAttached, AliasableBox<CallbackState>>,
}

impl Loans for AliasableBox<CallbackState> {
    type Recovered = Vec<CompletedVideo>;

    fn recover(self, destroyed: &Destroyed) -> Vec<CompletedVideo> {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take_retained(destroyed)
    }

    fn release(self, destroyed: &Destroyed) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .release_retained(destroyed, drop);
    }
}

// `QueuedSender` is `Send` and `Sync` from its fields, with no impl of its
// own. The Advanced SDK manual's section on asynchronous sending completions
// (25.4.1.1) has the SDK call the handler on threads of its own, possibly
// several at once, lets it be replaced at any time, and has
// `NDIlib_send_destroy` call every outstanding completion before it returns;
// it puts no thread requirement on the sender or on its completion
// registration. The lent sender is the crate's ordinary `Send + Sync` native
// sender, all state the callback shares is behind `CallbackState`'s mutex
// and atomic, and every operation that changes the queue takes `&mut self`.
// The runtime test `a_queued_sender_moves_between_threads` sends, completes,
// reads and drops one on threads other than the one that created it.
const _: () = {
    const fn send_and_sync<T: Send + Sync>() {}
    send_and_sync::<QueuedSender>();
};

impl QueuedSender {
    /// Create a sender, with the options' Advanced SDK
    /// [configuration](SenderOptions::config) when they carry one.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] without an Advanced runtime (or in a
    /// build without the Advanced SDK), [`Error::InvalidInput`] for
    /// limits outside their range, and the errors of [`Sender::new`].
    #[doc(alias = "NDIlib_send_set_video_async_completion")]
    pub fn new(ndi: &Ndi, options: &SenderOptions, limits: &QueueLimits) -> Result<Self> {
        let table = ndi.table::<SendCompletion>()?;
        native!(SendCompletion, table, uses(options, limits) => {
            let callback = CallbackState::new(*limits)?;
            let sender = Sender::open(ndi, options)?;
            Ok(Self::install(sender, callback, table))
        })
    }

    /// Turn an existing sender into a queued one.
    ///
    /// The sender's own asynchronous video send is finished first: the SDK is
    /// drained, and the frame the sender was still holding, if any, is
    /// returned beside the queue. The conversion is one-way.
    ///
    /// # Errors
    ///
    /// Returns the sender, unchanged, with [`Error::UnsupportedCapability`]
    /// without an Advanced runtime, or [`Error::InvalidInput`] for
    /// limits outside their range or a sender that is registered with an
    /// advertiser (a registration keeps the native sender alive after this
    /// value is dropped, when the completion state no longer exists).
    #[doc(alias = "NDIlib_send_set_video_async_completion")]
    pub fn from_sender(
        sender: Sender,
        limits: &QueueLimits,
    ) -> std::result::Result<(Self, Option<VideoFrame>), Rejected<Sender>> {
        let table = match sender.runtime().table::<SendCompletion>() {
            Ok(table) => table,
            Err(error) => return Err(Rejected::new(error, sender)),
        };
        native!(SendCompletion, table, uses(limits) => {
            let callback = match CallbackState::new(*limits) {
                Ok(callback) => callback,
                Err(error) => return Err(Rejected::new(error, sender)),
            };
            // The conversion takes the native sender only when no registration
            // shares it, and retires the sender's asynchronous video pipeline.
            match sender.into_attached() {
                Ok((sender, earlier)) => Ok((Self::install(sender, callback, table), earlier)),
                Err(sender) => Err(Rejected::new(
                    Error::InvalidInput(crate::InputDefect::QueuedSenderRegistered),
                    sender,
                )),
            }
        })
    }

    /// Register the completion callback before anything can be submitted.
    ///
    /// The sender's mode owns its native sender exclusively and has no
    /// asynchronous video pipeline, so nothing else is in flight on it: only
    /// the queue sends video on it from now on.
    #[cfg(ndi_has_advanced_send_completion)]
    fn install(
        sender: Sender<QueueAttached>,
        callback: Box<CallbackState>,
        table: &'static SendCompletion,
    ) -> Self {
        let callback = AliasableBox::new(callback);
        let state = callback.as_ptr();
        // SAFETY: the sender is live. The state is heap-allocated, the callback
        // only reads it through a shared reference, and the returned value
        // keeps it until after the sender has been destroyed.
        unsafe {
            table.NDIlib_send_set_video_async_completion(
                sender.native_instance(),
                state.cast_mut().cast(),
                Some(completion),
            )
        };
        Self {
            lending: Lending::new(sender, callback),
        }
    }

    /// The sender this queue sends through, for the operations that do not
    /// send video: [`source`](Sender::source),
    /// [`send_audio`](Sender::send_audio),
    /// [`send_compressed_audio`](Sender::send_compressed_audio),
    /// [`send_scatter_audio`](Sender::send_scatter_audio),
    /// [`send_metadata`](Sender::send_metadata) and
    /// [`capture_metadata`](Sender::capture_metadata),
    /// [`tally`](Sender::tally), [`connection_count`](Sender::connection_count),
    /// connection metadata, failover, and the Advanced controls.
    ///
    /// Its mode has none of the video sends, which would bypass the queue, and
    /// it cannot be registered with an advertiser. Use [`split`](Self::split)
    /// to send audio or metadata through it while another thread submits video.
    pub fn sender(&self) -> &Sender<QueueAttached> {
        self.lending.sender()
    }

    /// Lend video submission and completion collection separately from
    /// uncompressed or compressed audio, metadata and controls. The halves can be moved to separate scoped
    /// threads; dropping them restores access to this owner.
    ///
    /// Only the video half submits video, and it is neither `Sync` nor `Clone`.
    pub fn split(&mut self) -> (QueuedVideo<'_>, &Sender<QueueAttached>) {
        let pair = self.lending.parts().shared();
        let sender = pair.sender();
        let video = QueuedVideo {
            pair,
            _not_sync: std::marker::PhantomData,
        };
        (video, sender)
    }

    fn video(&mut self) -> QueuedVideo<'_> {
        self.split().0
    }

    /// Queue a frame without waiting for an earlier submission to complete.
    ///
    /// The queue owns the frame until it is collected: the SDK writes to a
    /// compressed frame's bytes while it sends them. A collected frame can be
    /// submitted again.
    /// Descriptor storage is reserved at construction, so accepted submission
    /// allocates nothing. Pixels, blocks and metadata move without
    /// copies or growth; owned uncompressed frames already hold all SDK tail
    /// room their layout and scan type need.
    ///
    /// # Errors
    ///
    /// Returns the frame with [`Error::InvalidInput`] when its retained
    /// allocation capacity alone exceeds [`QueueLimits::max_bytes`], or with
    /// [`Error::QueueFull`] when the frames already retained leave no room
    /// within [`QueueLimits`] ([`QueueLimit::Frames`] or
    /// [`QueueLimit::Bytes`]; collect completed frames and submit it again),
    /// with [`Error::UnexpectedSdkResult`] when the queue has failed (see
    /// [`completion_protocol_failed`](Self::completion_protocol_failed)),
    /// with [`Error::UnsupportedCapability`] when the runtime lacks scatter
    /// sending or cannot send an uncompressed frame's pixel format
    /// (as [`Sender::send_video`] checks).
    #[doc(alias = "NDIlib_send_send_video_async_v2")]
    #[doc(alias = "NDIlib_send_send_video_scatter_async")]
    pub fn try_submit(
        &mut self,
        frame: impl Into<QueuedVideoFrame>,
    ) -> std::result::Result<SubmissionId, Rejected<QueuedVideoFrame>> {
        self.video().try_submit(frame)
    }

    /// Take one completed frame, if there is one.
    pub fn try_next_completed(&mut self) -> Option<CompletedVideo> {
        self.video().try_next_completed()
    }

    /// Wait up to `timeout` for a completed frame. `None` means none completed
    /// in time; the queue is unchanged and can still be used.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`],
    /// and [`Error::UnexpectedSdkResult`] when the queue has failed (see
    /// [`completion_protocol_failed`](Self::completion_protocol_failed)).
    pub fn next_completed(&mut self, timeout: Duration) -> Result<Option<CompletedVideo>> {
        self.video().next_completed(timeout)
    }

    /// The number of frames held: those still being sent and those completed
    /// but not yet collected.
    pub fn retained_frames(&self) -> usize {
        self.lending.loans().retained_frames()
    }

    /// Whether the SDK has reported a completion that matches no queued frame,
    /// or the same one twice.
    ///
    /// The queue then no longer knows which frames the SDK still reads, so it
    /// accepts no more submissions; frames already completed can still be
    /// collected. Use [`close`](Self::close) to destroy the native sender and
    /// take back every frame still retained, including unmatched submissions.
    pub fn completion_protocol_failed(&self) -> bool {
        self.lending.loans().completion_protocol_failed()
    }

    /// Destroy the sender and take back every frame it still holds.
    ///
    /// The returned count equals [`retained_frames`](Self::retained_frames)
    /// immediately before this call. Frames reported complete, including by
    /// callbacks during destruction, come first in completion order. Any
    /// unmatched frames follow in submission-id order. Each frame keeps its
    /// original allocations and submission id, even after a
    /// [completion-protocol failure](Self::completion_protocol_failed).
    ///
    /// Native destruction waits without a timeout for the SDK to finish with
    /// every pending frame. Run this on a blocking thread when using an async
    /// executor. Only the returned collection is allocated.
    #[doc(alias = "NDIlib_send_destroy")]
    pub fn close(self) -> Vec<CompletedVideo> {
        self.lending.close()
    }
}

/// Exclusive video operations borrowed from a [`QueuedSender`].
///
/// Move this half to a scoped video thread while the other half sends audio,
/// metadata or controls. It is `Send`, but neither `Sync` nor `Clone`, and
/// cannot outlive its owner. Dropping it leaves pending frames with the owner.
pub struct QueuedVideo<'a> {
    pair: LoanPair<'a, QueueAttached, &'a AliasableBox<CallbackState>>,
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}

impl QueuedVideo<'_> {
    /// See [`QueuedSender::try_submit`].
    ///
    /// # Errors
    ///
    /// As for [`QueuedSender::try_submit`].
    #[doc(alias = "NDIlib_send_send_video_async_v2")]
    #[doc(alias = "NDIlib_send_send_video_scatter_async")]
    pub fn try_submit(
        &mut self,
        frame: impl Into<QueuedVideoFrame>,
    ) -> std::result::Result<SubmissionId, Rejected<QueuedVideoFrame>> {
        self.pair.try_submit_queued(frame)
    }

    /// See [`QueuedSender::try_next_completed`].
    pub fn try_next_completed(&mut self) -> Option<CompletedVideo> {
        let mut state = self
            .pair
            .loans()
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        take_completed(&mut state)
    }

    /// See [`QueuedSender::next_completed`].
    ///
    /// # Errors
    ///
    /// As for [`QueuedSender::next_completed`].
    pub fn next_completed(&mut self, timeout: Duration) -> Result<Option<CompletedVideo>> {
        let deadline = Deadline::after(timeout)?;
        let mut state = self
            .pair
            .loans()
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(frame) = take_completed(&mut state) {
                return Ok(Some(frame));
            }
            if self.pair.loans().protocol_error.load(Ordering::Acquire) {
                return Err(Error::UnexpectedSdkResult(
                    crate::SdkResultDefect::UnknownCompletion,
                ));
            }
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Ok(None);
            }
            let (next, _) = self
                .pair
                .loans()
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|p| p.into_inner());
            state = next;
        }
    }

    /// See [`QueuedSender::retained_frames`].
    pub fn retained_frames(&self) -> usize {
        self.pair.loans().retained_frames()
    }

    /// See [`QueuedSender::completion_protocol_failed`].
    pub fn completion_protocol_failed(&self) -> bool {
        self.pair.loans().completion_protocol_failed()
    }
}

fn take_completed(state: &mut State) -> Option<CompletedVideo> {
    let ReadyEntry { entry, completion } = state.ready.pop_front()?;
    state.bytes -= entry.bytes;
    let (completed, key) = entry.release(Released::Completed(completion));
    state.free.push(key);
    Some(completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::VideoPartsExt;

    #[test]
    fn completed_packet_debug_is_bounded_by_fields_not_payload() {
        use crate::advanced::{
            packets::testing::assert_bounded_debug, EnvelopeCodec, PacketEnvelope, VideoCodec,
            VideoPacketFormat,
        };
        let summaries = |size| {
            let bytes =
                PacketEnvelope::encode(EnvelopeCodec::H264, 17, 13, true, &vec![7; size], &[1, 2])
                    .unwrap();
            let format = VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap();
            [
                QueuedVideoFrame::Compressed(
                    CompressedVideoFrame::new(format, bytes.clone()).unwrap(),
                ),
                QueuedVideoFrame::Scatter(
                    ScatterVideoFrame::new(
                        format,
                        vec![bytes[..44].to_vec(), bytes[44..].to_vec()],
                    )
                    .unwrap(),
                ),
            ]
            .map(|frame| {
                format!(
                    "{:?}",
                    CompletedVideo {
                        id: SubmissionId(1),
                        frame
                    }
                )
            })
        };
        assert_bounded_debug(&summaries(1024), &summaries(1024 * 1024));
    }

    #[test]
    fn split_submission_reuses_reserved_descriptors() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let mut queue = QueuedSender::new(
            &ndi,
            &SenderOptions::new(crate::test_support::unique_name("split-allocation"))
                .clock_video(false),
            &QueueLimits::default(),
        )
        .unwrap();
        let frame = VideoFrame::builder().resolution(16, 16).build().unwrap();
        let ((mut video, _audio), allocations) =
            crate::test_alloc::allocation_count(|| queue.split());
        assert_eq!(allocations, 0);
        let (result, allocations) = crate::test_alloc::allocation_count(|| video.try_submit(frame));
        result.unwrap();
        assert_eq!(allocations, 0);
    }

    /// Drive the same entry check with an already-retained key, while the
    /// rejected frame owns a different allocation and is never sent.
    fn reject_an_occupied_completion_key(callback: &CallbackState) -> (SubmissionId, usize) {
        let frame = VideoFrame::builder().resolution(2, 2).build().unwrap();
        let bytes = frame.retained_bytes();
        let mut state = callback.state.lock().unwrap();
        let admitted = state
            .admit(
                QueuedVideoFrame::from(frame).authorize_for_test(None),
                bytes,
                &callback.protocol_error,
            )
            .unwrap();
        let key = admitted.lent.descriptor().p_data as usize;
        let before = (state.free.len(), state.retained(), state.bytes, state.next);
        let other = VideoFrame::builder().resolution(4, 2).build().unwrap();
        let other_pointer = other.data().as_ptr();
        let bytes = other.retained_bytes();
        let rejected = state
            .admit_with_lend(
                QueuedVideoFrame::from(other).authorize_for_test(None),
                bytes,
                &callback.protocol_error,
                |frame| {
                    Lent::Frame(NDIlib_video_frame_v2_t {
                        p_data: key as *mut u8,
                        ..*frame.lend(()).descriptor()
                    })
                },
            )
            .err()
            .expect("an occupied completion key is refused");
        assert!(matches!(rejected.error(), Error::UnexpectedSdkResult(_)));
        let QueuedVideoFrame::Pixels(other) = rejected.into_parts().1 else {
            panic!("the refused input is returned");
        };
        assert_eq!(other.data().as_ptr(), other_pointer);
        assert!(callback.completion_protocol_failed());
        assert_eq!(
            before,
            (state.free.len(), state.retained(), state.bytes, state.next)
        );
        let retained = &state.in_flight[&key];
        assert_eq!(retained.id, admitted.id);
        let QueuedStorage::Pixels(frame) = &retained.frame else {
            panic!("the retained entry is untouched");
        };
        assert_eq!(frame.video_descriptor().p_data as usize, key);
        (admitted.id, key)
    }

    #[test]
    fn an_occupied_completion_key_preserves_every_retained_entry_and_free_slot() {
        let callback = CallbackState::new(QueueLimits::default().max_frames(2)).unwrap();
        let (id, key) = reject_an_occupied_completion_key(&callback);
        callback.complete(key);
        let mut state = callback.state.lock().unwrap();
        let completed = take_completed(&mut state).unwrap();
        assert_eq!(completed.id(), id);
        assert_eq!(state.free.len(), 2);
        assert_eq!(state.bytes, 0);
    }

    #[test]
    fn close_recovers_the_entry_after_an_occupied_completion_key_was_refused() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let queue = QueuedSender::new(
            &ndi,
            &SenderOptions::new(crate::test_support::unique_name("duplicate-key-close")),
            &QueueLimits::default().max_frames(2),
        )
        .unwrap();
        let (id, key) = reject_an_occupied_completion_key(queue.lending.loans());
        let completed = queue.close();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id(), id);
        let QueuedVideoFrame::Pixels(frame) = completed[0].frame() else {
            panic!("close recovers the retained frame");
        };
        assert_eq!(frame.data().as_ptr() as usize, key);
    }

    #[test]
    fn slots_are_retained_through_out_of_order_completion_and_reused_after_collection() {
        let callback = CallbackState::new(QueueLimits::default().max_frames(3)).unwrap();
        let mut admitted = Vec::new();
        for _ in 0..3 {
            let frame = VideoFrame::builder().resolution(2, 2).build().unwrap();
            let bytes = frame.retained_bytes();
            admitted.push(
                callback
                    .state
                    .lock()
                    .unwrap()
                    .admit(
                        QueuedVideoFrame::from(frame).authorize_for_test(None),
                        bytes,
                        &callback.protocol_error,
                    )
                    .unwrap(),
            );
        }
        assert!(callback.state.lock().unwrap().free.is_empty());
        std::thread::scope(|scope| {
            for index in [2, 0] {
                let callback = &callback;
                let key = admitted[index].lent.descriptor().p_data as usize;
                scope.spawn(move || callback.complete(key));
            }
        });
        let mut state = callback.state.lock().unwrap();
        assert_eq!(
            (state.in_flight.len(), state.ready.len(), state.free.len()),
            (1, 2, 0)
        );
        let (completed, allocations) =
            crate::test_alloc::allocation_count(|| take_completed(&mut state).unwrap());
        assert_eq!(allocations, 0);
        let prior = admitted
            .iter()
            .find(|entry| entry.id == completed.id())
            .unwrap();
        let frame = VideoFrame::builder().resolution(4, 2).build().unwrap();
        let bytes = frame.retained_bytes();
        let replacement = state
            .admit(
                QueuedVideoFrame::from(frame).authorize_for_test(None),
                bytes,
                &callback.protocol_error,
            )
            .unwrap();
        assert_eq!(replacement.pointer, prior.pointer);
        // SAFETY: the new synthetic header is initialized in the reclaimed
        // slot and has never been given to native code.
        assert_eq!(unsafe { (*replacement.pointer).xres }, 4);
        for other in admitted.iter().filter(|entry| entry.id != completed.id()) {
            assert_ne!(replacement.pointer, other.pointer);
            // SAFETY: these retained synthetic headers were never exposed to
            // native code, remain initialized, and their slots are not reused.
            assert_eq!(unsafe { (*other.pointer).xres }, 2);
        }
        assert!(state.free.is_empty());
    }

    #[cfg(ndi_has_advanced_scatter_send)]
    #[test]
    fn native_admission_rejection_and_retry_use_only_reserved_descriptor_storage() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let packet =
            super::super::VideoPacketFormat::new(super::super::VideoCodec::SpeedHq422, 16, 16)
                .unwrap();
        for variant in 0..3 {
            let make = || match variant {
                0 => QueuedVideoFrame::Pixels(
                    VideoFrame::builder().resolution(16, 16).build().unwrap(),
                ),
                1 => QueuedVideoFrame::Compressed(
                    CompressedVideoFrame::new(packet, vec![1, 2, 3, 4]).unwrap(),
                ),
                _ => QueuedVideoFrame::Scatter(
                    ScatterVideoFrame::new(packet, vec![vec![1, 2, 3, 4], vec![5, 6]]).unwrap(),
                ),
            };
            for limit in [QueueLimit::Frames, QueueLimit::Bytes] {
                let first = make();
                let limits = if limit == QueueLimit::Frames {
                    QueueLimits::default().max_frames(1)
                } else {
                    QueueLimits::default()
                        .max_frames(2)
                        .max_bytes(first.retained_bytes())
                };
                let mut queue = QueuedSender::new(
                    &ndi,
                    &SenderOptions::new(crate::test_support::unique_name("reserved-admission"))
                        .clock_video(false),
                    &limits,
                )
                .unwrap();
                let (result, allocations) =
                    crate::test_alloc::allocation_count(|| queue.try_submit(first));
                let first_id = result.unwrap();
                assert_eq!(allocations, 0, "accepted variant {variant}");
                let second = make();
                let (result, allocations) =
                    crate::test_alloc::allocation_count(|| queue.try_submit(second));
                let rejected = result.unwrap_err();
                assert!(matches!(rejected.error(), Error::QueueFull(actual) if *actual == limit));
                assert_eq!(allocations, 1, "only the Rejected box, variant {variant}");
                assert_eq!(
                    queue
                        .next_completed(Duration::from_secs(5))
                        .unwrap()
                        .unwrap()
                        .id(),
                    first_id
                );
                let second = rejected.into_parts().1;
                let (result, allocations) =
                    crate::test_alloc::allocation_count(|| queue.try_submit(second));
                result.unwrap();
                assert_eq!(allocations, 0, "accepted retry variant {variant}");
                assert_eq!(queue.close().len(), 1);
            }
        }
    }

    #[test]
    fn rebuilding_the_completion_index_reuses_reserved_storage_and_preserves_loans() {
        let callback = CallbackState::new(QueueLimits::default().max_frames(4)).unwrap();
        let admitted = (0..3)
            .map(|_| {
                let frame = VideoFrame::builder().resolution(2, 2).build().unwrap();
                let bytes = frame.retained_bytes();
                callback
                    .state
                    .lock()
                    .unwrap()
                    .admit(
                        QueuedVideoFrame::from(frame).authorize_for_test(None),
                        bytes,
                        &callback.protocol_error,
                    )
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let mut state = callback.state.lock().unwrap();
        let before = (
            state.bytes,
            state.next,
            state.free.len(),
            state.in_flight.capacity(),
            state.rehash.capacity(),
        );
        let ((), allocations) = crate::test_alloc::allocation_count(|| state.rebuild_index());
        assert_eq!(allocations, 0);
        assert_eq!(
            (
                state.bytes,
                state.next,
                state.free.len(),
                state.in_flight.capacity(),
                state.rehash.capacity()
            ),
            before
        );
        assert!(state.rehash.is_empty());
        for loan in admitted {
            let entry = state
                .in_flight
                .get(&(loan.lent.descriptor().p_data as usize))
                .unwrap();
            assert_eq!(entry.id, loan.id);
            // SAFETY: the synthetic header remains owned and initialized in
            // the same leased slot; no native sender accesses this test state.
            assert_eq!(unsafe { (*loan.pointer).xres }, 2);
        }
    }

    #[test]
    fn descriptor_admission_and_collection_stay_allocation_free_under_churn() {
        for capacity in [2, 4, 7, 8, 14, 16, 28, 56, 112] {
            let callback = CallbackState::new(QueueLimits::default().max_frames(capacity)).unwrap();
            let mut frames = (0..2304)
                .map(|_| VideoFrame::builder().resolution(2, 2).build().unwrap())
                .collect::<Vec<_>>();
            let mut keys = vec![0usize; capacity - 1];
            for key in keys.iter_mut() {
                let frame = frames.pop().unwrap();
                let bytes = frame.retained_bytes();
                let admitted = callback
                    .state
                    .lock()
                    .unwrap()
                    .admit(
                        QueuedVideoFrame::from(frame).authorize_for_test(None),
                        bytes,
                        &callback.protocol_error,
                    )
                    .unwrap();
                *key = admitted.lent.descriptor().p_data as usize;
            }
            for index in 0..2000 {
                let frame = frames.pop().unwrap();
                let bytes = frame.retained_bytes();
                let authorized = QueuedVideoFrame::from(frame).authorize_for_test(None);
                let (admitted, allocations) = crate::test_alloc::allocation_count(|| {
                    callback.state.lock().unwrap().admit(
                        authorized,
                        bytes,
                        &callback.protocol_error,
                    )
                });
                let admitted = admitted.unwrap();
                assert_eq!(
                    allocations, 0,
                    "accepted submission {index}, capacity {capacity}"
                );
                let completed = std::mem::replace(
                    &mut keys[index % (capacity - 1)],
                    admitted.lent.descriptor().p_data as usize,
                );
                callback.complete(completed);
                let (collected, allocations) = crate::test_alloc::allocation_count(|| {
                    take_completed(&mut callback.state.lock().unwrap())
                });
                assert!(collected.is_some());
                assert_eq!(allocations, 0);
            }
        }
    }

    #[test]
    fn permanent_admission_check_needs_no_lock_or_descriptor() {
        let state = CallbackState::new(QueueLimits::default().max_bytes(4)).unwrap();
        // An occupied lock cannot delay this allocation-free refusal.
        let _locked = state.state.lock().unwrap();
        let (result, allocations) =
            crate::test_alloc::allocation_count(|| state.validate_frame_size(1024));
        let Error::InvalidInput(message) = result.unwrap_err() else {
            panic!("an oversized frame is not retryable");
        };
        assert_eq!(
            message,
            crate::InputDefect::QueueFrameTooLarge {
                bytes: 1024,
                limit: 4
            }
        );
        assert_eq!(allocations, 0);
        let (result, allocations) =
            crate::test_alloc::allocation_count(|| state.validate_frame_size(4));
        result.unwrap();
        assert_eq!(allocations, 0);
    }

    #[test]
    fn queued_pixel_descriptor_fits_packed_copy_of_padded_source() {
        for (format, stride) in [
            (crate::PixelFormat::Bgra, 512),
            (crate::PixelFormat::Uyvy, 256),
            (crate::PixelFormat::P216, 256),
        ] {
            let planes = if format == crate::PixelFormat::P216 {
                2
            } else {
                1
            };
            let storage = vec![0u8; stride * 32 * planes];
            let borrowed = crate::BorrowedVideoFrame::builder(&storage, 64, 32, format)
                .line_stride_bytes(stride)
                .build()
                .unwrap();
            let pixels = borrowed.owned_copy();
            let readable = pixels.data().len() + pixels.tail_room();
            let pointer = pixels.data().as_ptr();
            let bytes = pixels.retained_bytes();
            let frame = QueuedVideoFrame::from(pixels).authorize_for_test(None);
            let callback = CallbackState::new(QueueLimits::default()).unwrap();
            let mut state = callback.state.lock().unwrap();
            let admitted = state.admit(frame, bytes, &callback.protocol_error).unwrap();
            crate::frames::descriptor_tests::assert_extent(
                admitted.lent.descriptor(),
                pointer,
                readable,
            );
        }
    }

    #[test]
    fn every_authorized_storage_lends_its_original_completion_key() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let table = ndi.table::<ScatterSend>().unwrap();
        let format =
            super::super::VideoPacketFormat::new(super::super::VideoCodec::SpeedHq422, 16, 16)
                .unwrap();
        for frame in [
            QueuedVideoFrame::Pixels(VideoFrame::builder().resolution(16, 16).build().unwrap()),
            QueuedVideoFrame::Compressed(
                CompressedVideoFrame::new(format, vec![1, 2, 3, 4]).unwrap(),
            ),
            QueuedVideoFrame::Scatter(
                ScatterVideoFrame::new(format, vec![vec![1, 2, 3, 4], vec![5, 6]]).unwrap(),
            ),
        ] {
            let expected = match &frame {
                QueuedVideoFrame::Pixels(frame) => frame.data().as_ptr(),
                QueuedVideoFrame::Compressed(frame) => frame.data().as_ptr(),
                QueuedVideoFrame::Scatter(frame) => frame.blocks().next().unwrap().as_ptr(),
            };
            let bytes = frame.retained_bytes();
            let authorized = frame.authorize_for_test(Some(table));
            let callback = CallbackState::new(QueueLimits::default()).unwrap();
            let admitted = callback
                .state
                .lock()
                .unwrap()
                .admit(authorized, bytes, &callback.protocol_error)
                .unwrap();
            assert_eq!(admitted.lent.descriptor().p_data.cast_const(), expected);
        }
    }

    fn insert(callback: &CallbackState, id: u64) -> usize {
        let pixels = VideoFrame::builder().resolution(2, 2).build().unwrap();
        let bytes = pixels.retained_bytes();
        let frame = QueuedVideoFrame::from(pixels).authorize_for_test(None);
        let mut state = callback.state.lock().unwrap();
        let admitted = state.admit(frame, bytes, &callback.protocol_error).unwrap();
        assert_eq!(admitted.id.get(), id);
        admitted.lent.descriptor().p_data as usize
    }

    #[test]
    fn a_retired_slot_reduces_capacity_and_collection_still_allows_retry() {
        let callback = CallbackState::new(QueueLimits::default().max_frames(3)).unwrap();
        drop(
            callback
                .state
                .lock()
                .unwrap()
                .free
                .pop()
                .unwrap()
                .claim(Default::default()),
        );
        let first = insert(&callback, 1);
        insert(&callback, 2);
        let frame = VideoFrame::builder().resolution(2, 2).build().unwrap();
        let pointer = frame.data().as_ptr();
        let bytes = frame.retained_bytes();
        let rejected = callback
            .state
            .lock()
            .unwrap()
            .admit(
                QueuedVideoFrame::from(frame).authorize_for_test(None),
                bytes,
                &callback.protocol_error,
            )
            .err()
            .unwrap();
        assert!(matches!(
            rejected.error(),
            Error::QueueFull(QueueLimit::Frames)
        ));
        let QueuedVideoFrame::Pixels(frame) = rejected.into_parts().1 else {
            panic!("the refused frame keeps its storage");
        };
        assert_eq!(frame.data().as_ptr(), pointer);
        callback.complete(first);
        let mut state = callback.state.lock().unwrap();
        assert_eq!(take_completed(&mut state).unwrap().id().get(), 1);
        let admitted = state
            .admit(
                QueuedVideoFrame::from(frame).authorize_for_test(None),
                bytes,
                &callback.protocol_error,
            )
            .unwrap();
        assert_eq!(admitted.id.get(), 3);
        assert_eq!(admitted.lent.descriptor().p_data.cast_const(), pointer);
        assert!(state.free.is_empty());
    }

    #[test]
    fn retired_slots_preserve_byte_frame_and_identifier_refusal_order() {
        let callback = CallbackState::new(QueueLimits::default().max_frames(1)).unwrap();
        let frame = || VideoFrame::builder().resolution(2, 2).build().unwrap();
        let mut state = callback.state.lock().unwrap();
        state.next = u64::MAX;
        let original_bytes = state.options.bytes;
        state.options.bytes = 0;
        let input = frame();
        let bytes = input.retained_bytes();
        let reject = |state: &mut State, input: VideoFrame| {
            state
                .admit(
                    QueuedVideoFrame::from(input).authorize_for_test(None),
                    bytes,
                    &callback.protocol_error,
                )
                .err()
                .unwrap()
                .into_parts()
                .0
        };
        assert!(matches!(
            reject(&mut state, input),
            Error::QueueFull(QueueLimit::Bytes)
        ));
        state.options.bytes = original_bytes;
        assert!(matches!(
            reject(&mut state, frame()),
            Error::InvalidInput(crate::InputDefect::QueueIdentifiersExhausted)
        ));
        assert_eq!(state.free.len(), 1, "refusal returns the unused key");
        drop(state.free.pop().unwrap().claim(Default::default()));
        assert!(matches!(
            reject(&mut state, frame()),
            Error::QueueFull(QueueLimit::Frames)
        ));
    }

    #[test]
    fn a_retired_slot_does_not_panic_on_close_or_drop() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        for close in [true, false] {
            let queue = QueuedSender::new(
                &ndi,
                &SenderOptions::new(crate::test_support::unique_name("retired-slot-teardown")),
                &QueueLimits::default().max_frames(4),
            )
            .unwrap();
            let callback = queue.lending.loans();
            drop(
                callback
                    .state
                    .lock()
                    .unwrap()
                    .free
                    .pop()
                    .unwrap()
                    .claim(Default::default()),
            );
            let first = insert(callback, 1);
            insert(callback, 2);
            let third = insert(callback, 3);
            callback.complete(third);
            callback.complete(first);
            if close {
                assert_eq!(
                    queue
                        .close()
                        .iter()
                        .map(|done| done.id().get())
                        .collect::<Vec<_>>(),
                    [3, 1, 2]
                );
            } else {
                drop(queue);
            }
        }
    }

    #[test]
    fn close_recovers_ready_then_unmatched_frames_with_only_the_result_allocation() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let queue = QueuedSender::new(
            &ndi,
            &SenderOptions::new(crate::test_support::unique_name("close-order")),
            &QueueLimits::default(),
        )
        .unwrap();
        let state = queue.lending.loans();
        // These entries were not sent, so the native sender cannot report
        // additional completions. They exercise both teardown collections.
        let original = [1, 2, 3, 4, 5].map(|id| (id, insert(state, id)));
        state.complete(original[3].1);
        state.complete(original[1].1);
        let retained = queue.retained_frames();
        let (completed, allocations) = crate::test_alloc::allocation_count(|| queue.close());
        assert_eq!(allocations, 1, "only the returned frame collection");
        assert_eq!(completed.len(), retained);
        assert_eq!(
            completed
                .iter()
                .map(|done| done.id().get())
                .collect::<Vec<_>>(),
            [4, 2, 1, 3, 5]
        );
        for done in completed {
            let QueuedVideoFrame::Pixels(frame) = done.frame() else {
                panic!("the frame changed type");
            };
            let expected = original
                .iter()
                .find(|(id, _)| *id == done.id().get())
                .unwrap()
                .1;
            assert_eq!(frame.data().as_ptr() as usize, expected);
        }
    }

    #[test]
    fn drop_releases_retained_frames_without_allocating_a_recovery_collection() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let queue = QueuedSender::new(
            &ndi,
            &SenderOptions::new(crate::test_support::unique_name("drop-allocation")),
            &QueueLimits::default(),
        )
        .unwrap();
        let state = queue.lending.loans();
        let first = insert(state, 1);
        insert(state, 2);
        state.complete(first);
        let ((), allocations) = crate::test_alloc::allocation_count(|| drop(queue));
        assert_eq!(allocations, 0);
    }

    #[test]
    fn closing_an_empty_queue_needs_no_result_allocation() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let queue = QueuedSender::new(
            &ndi,
            &SenderOptions::new(crate::test_support::unique_name("close-empty")),
            &QueueLimits::default(),
        )
        .unwrap();
        let (completed, allocations) = crate::test_alloc::allocation_count(|| queue.close());
        assert!(completed.is_empty());
        assert_eq!(allocations, 0);
    }

    #[test]
    fn concurrent_and_out_of_order_callbacks_preserve_owned_frames() {
        let state = CallbackState::new(QueueLimits::default()).unwrap();
        let a = insert(&state, 1);
        let b = insert(&state, 2);
        state.complete(b);
        std::thread::scope(|scope| {
            scope.spawn(|| state.complete(a));
        });
        let mut s = state.state.lock().unwrap();
        assert_eq!((s.in_flight.len(), s.ready.len()), (0, 2));
        assert_eq!(take_completed(&mut s).unwrap().id().get(), 2);
        assert_eq!(take_completed(&mut s).unwrap().id().get(), 1);
        assert_eq!(s.bytes, 0);
    }

    /// Conversion drains the sender and returns its frame beside the queue.
    #[test]
    fn converting_a_sender_returns_its_in_flight_frame() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let options = SenderOptions::new(crate::test_support::unique_name("Queued conversion"))
            .clock_video(false);
        let mut sender = Sender::new(&ndi, &options).unwrap();
        let frame = VideoFrame::builder().resolution(16, 16).build().unwrap();
        let pixels = frame.data().as_ptr();
        assert!(sender.submit_video(frame).unwrap().is_none());
        let (mut queue, earlier) =
            QueuedSender::from_sender(sender, &QueueLimits::default()).unwrap();
        assert_eq!(earlier.unwrap().data().as_ptr(), pixels);
        // The queue sends on the converted sender.
        let next = VideoFrame::builder().resolution(16, 16).build().unwrap();
        let id = queue.try_submit(next).unwrap();
        let completed = queue.next_completed(Duration::from_secs(5)).unwrap();
        assert_eq!(completed.unwrap().id(), id);
    }

    #[test]
    fn duplicate_and_unknown_completions_do_not_release_storage() {
        let state = CallbackState::new(QueueLimits::default()).unwrap();
        let a = insert(&state, 1);
        state.complete(a);
        state.complete(a);
        state.complete(0);
        let mut s = state.state.lock().unwrap();
        assert_eq!((s.in_flight.len(), s.ready.len()), (0, 1));
        assert!(state.protocol_error.load(Ordering::Acquire));
        assert!(take_completed(&mut s).is_some());
        assert!(take_completed(&mut s).is_none());
    }

    /// The SDK's thread neither allocates nor frees when it reports a
    /// completion, a duplicate or an unknown frame: the entry moves between
    /// storage reserved before the callback was registered, and neither
    /// collection gives any of it back.
    #[test]
    fn completion_moves_an_entry_without_allocating_or_freeing() {
        let state = CallbackState::new(QueueLimits::default().max_frames(3)).unwrap();
        let keys = [insert(&state, 1), insert(&state, 2), insert(&state, 3)];
        let capacities = |state: &CallbackState| {
            let s = state.state.lock().unwrap();
            (s.in_flight.capacity(), s.ready.capacity())
        };
        let reserved = capacities(&state);
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for key in [keys[2], keys[0], keys[1], keys[0], 0] {
                state.complete(key);
            }
        });
        assert_eq!(allocations, 0);
        assert_eq!(capacities(&state), reserved);
        let mut s = state.state.lock().unwrap();
        assert_eq!((s.in_flight.len(), s.ready.len()), (0, 3));
        let order: Vec<_> = std::iter::from_fn(|| take_completed(&mut s))
            .map(|completed| completed.id().get())
            .collect();
        assert_eq!(order, [3, 1, 2]);
        assert!(state.protocol_error.load(Ordering::Acquire));
    }
}

#[cfg(test)]
#[path = "queued_boundary_tests.rs"]
mod boundary_tests;

impl std::fmt::Debug for QueuedSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueuedSender")
            .field("sender", self.sender())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for QueuedVideo<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueuedVideo")
            .field("sender", self.pair.sender())
            .finish_non_exhaustive()
    }
}
