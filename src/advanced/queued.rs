//! Asynchronous video sending with a bounded queue and completion
//! notification.

use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, Ordering},
        Condvar, Mutex,
    },
    time::Duration,
};

use super::CompressedVideoFrame;
use crate::{
    sys::{
        advanced::{native, SendCompletion},
        bindings::*,
    },
    timeout::Deadline,
    Error, Ndi, QueueAttached, QueueLimit, Rejected, Result, Sender, SenderOptions, VideoFrame,
};

use super::aliasable::AliasableBox;

/// Limits on what a [`QueuedSender`] holds at once.
///
/// A frame counts from submission until the application takes it back, so a
/// frame the SDK has finished with but that has not been collected still uses
/// its share. The defaults are 8 frames and 512 MiB.
#[derive(Debug, Clone, Copy)]
pub struct QueueOptions {
    frames: usize,
    bytes: usize,
}

impl Default for QueueOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl QueueOptions {
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

    /// Set the maximum number of bytes of frame data and metadata held.
    #[must_use]
    pub fn max_bytes(mut self, bytes: usize) -> Self {
        self.bytes = bytes;
        self
    }

    fn validate(self) -> Result<Self> {
        if self.frames == 0
            || self.frames > 65_536
            || self.bytes == 0
            || self.bytes > isize::MAX as usize
        {
            return Err(Error::InvalidInput(
                "queue requires 1..=65536 frames and a positive addressable byte limit".into(),
            ));
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
/// Each variant owns its complete, nonempty video allocation. Cloning copies
/// the bytes into independent storage; submission moves that storage into the
/// queue until it is collected.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum QueuedVideoFrame {
    /// An uncompressed frame.
    Pixels(VideoFrame),
    /// A compressed frame.
    Compressed(CompressedVideoFrame),
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

impl QueuedVideoFrame {
    /// A native descriptor that lends this frame to the SDK, which may write
    /// to a compressed frame's bytes for as long as it holds the descriptor.
    fn raw(&mut self) -> NDIlib_video_frame_v2_t {
        match self {
            Self::Pixels(v) => v.lend_raw(),
            Self::Compressed(v) => v.raw(),
        }
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Self::Pixels(v) => v.retained_bytes(),
            Self::Compressed(v) => v.retained_bytes(),
        }
    }

    /// Check that `sender` can send this frame, as its own send methods do.
    fn validate(&self, sender: &Sender<QueueAttached>) -> Result<()> {
        match self {
            Self::Pixels(v) => sender.require_video_format(v.pixel_format()),
            Self::Compressed(v) => v.validate_for_send(),
        }
    }
}
/// A frame the SDK has finished sending, ready to be reused.
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
    // Kept so that the address given to the SDK stays valid. Its data
    // pointer identifies the submission.
    _header: AliasableBox<NDIlib_video_frame_v2_t>,
    frame: QueuedVideoFrame,
    bytes: usize,
}

// SAFETY: the descriptor points only into allocations this entry owns, and
// moving the entry moves none of them. While the frame is queued only the
// SDK reads or writes them: the completion callback moves the entry from
// `State::in_flight` to `State::ready` without touching the frame, and the
// queue hands the frame out only from `ready`, after the SDK has reported
// it complete.
unsafe impl Send for Entry {}

/// The frames a queue holds. Each is in exactly one place: in flight until
/// the SDK reports it complete, then ready until it is collected.
struct State {
    // The frames the SDK may still read, by the address of their data.
    in_flight: HashMap<usize, Entry>,
    // The frames the SDK has finished with, in the order it reported them.
    ready: VecDeque<Entry>,
    bytes: usize,
    next: u64,
    options: QueueOptions,
}

impl State {
    /// The frames held, in flight or ready.
    fn retained(&self) -> usize {
        self.in_flight.len() + self.ready.len()
    }
}

struct CallbackState {
    state: Mutex<State>,
    changed: Condvar,
    protocol_error: AtomicBool,
}

impl CallbackState {
    fn new(options: QueueOptions) -> Result<Box<Self>> {
        let options = options.validate()?;
        let unavailable =
            |_| Error::InvalidInput("unable to allocate bounded completion storage".into());
        let mut in_flight = HashMap::new();
        in_flight.try_reserve(options.frames).map_err(unavailable)?;
        let mut ready = VecDeque::new();
        ready
            .try_reserve_exact(options.frames)
            .map_err(unavailable)?;
        let state = Box::new(Self {
            state: Mutex::new(State {
                in_flight,
                ready,
                bytes: 0,
                next: 1,
                options,
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

    /// Record that the SDK has finished with the frame whose data is at
    /// `address`. Runs on an SDK thread.
    ///
    /// No allocator or user code runs here. Removing the entry from the map
    /// keeps the map's storage (std gives a map's storage back only through
    /// `shrink_to` and `shrink_to_fit`), and `ready` was given room for every
    /// frame the queue can hold before the callback was registered, while
    /// `try_submit` keeps the queue within that many. The entry is moved, so
    /// nothing is freed.
    fn complete(&self, address: usize) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let State {
            in_flight, ready, ..
        } = &mut *state;
        match in_flight.remove(&address) {
            Some(entry) => ready.push_back(entry),
            // No frame in flight has this data: the SDK named a frame twice,
            // or one it was never given.
            None => self.protocol_error.store(true, Ordering::Release),
        }
        self.changed.notify_all();
    }
}
/// The completion callback the SDK calls, on its own threads, when it has
/// finished with an asynchronously sent frame.
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
/// connection, up to the limits in [`QueueOptions`]. No application code runs
/// on an SDK thread: completed frames are collected with
/// [`try_take_completed`](Self::try_take_completed) or
/// [`wait_completed`](Self::wait_completed) and can be reused.
///
/// It is [`Send`] and [`Sync`]: it can be moved to, and its read-only methods
/// called from, any thread, while submitting and collecting frames takes it
/// mutably. Dropping it destroys the native sender, which waits, without a
/// timeout, for the SDK to finish with every frame still queued.
///
/// [`sender`](Self::sender) lends the native sender in the [`QueueAttached`]
/// mode, which has the operations every sender shares (the source, metadata
/// in both directions, tally, connections, failover, and the Advanced
/// controls) and [`send_audio`](Sender::send_audio), but none of the video
/// sends, and which cannot be registered with a
/// [`SendAdvertiser`](crate::discovery::SendAdvertiser).
///
/// # Examples
///
/// ```no_run
/// use std::time::Duration;
/// use grafton_ndi::{
///     advanced::{QueueOptions, QueuedSender},
///     SenderOptions, VideoFrame, Ndi,
/// };
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = Ndi::new()?;
/// let mut sender = QueuedSender::new(
///     &ndi,
///     &SenderOptions::new("Queued"),
///     QueueOptions::default().max_frames(4),
/// )?;
/// let id = sender.try_submit(VideoFrame::builder().resolution(1920, 1080).build()?)?;
/// if let Some(done) = sender.wait_completed(Duration::from_secs(1))? {
///     assert_eq!(done.id(), id);
/// }
/// # Ok(())
/// # }
/// ```
pub struct QueuedSender {
    // Declared first so that it is destroyed first: destroying the native
    // sender is what ends the SDK's completion callbacks. Its mode owns the
    // native sender exclusively, so no other owner, such as an advertiser
    // registration, can keep it alive after the callback state is freed.
    sender: Sender<QueueAttached>,
    // The SDK holds this address for as long as the sender exists.
    callback: AliasableBox<CallbackState>,
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
    pub fn new(ndi: &Ndi, options: &SenderOptions, limits: QueueOptions) -> Result<Self> {
        let table = ndi.table::<SendCompletion>()?;
        let callback = CallbackState::new(limits)?;
        let sender = Sender::open(ndi, options)?;
        Ok(Self::install(sender, callback, table))
    }

    /// Turn an existing sender into a queued one.
    ///
    /// The sender's own asynchronous video send is finished first: the SDK is
    /// drained, and the frame the sender was still holding, if any, is
    /// returned beside the queue. Its completion handler is dropped without
    /// being called, even for a frame that was still in flight, which comes
    /// back here instead of being notified, so a handler that would panic
    /// cannot lose the sender. The conversion is one-way.
    ///
    /// # Errors
    ///
    /// Returns the sender, unchanged, with [`Error::UnsupportedCapability`]
    /// without an Advanced runtime, or [`Error::InvalidInput`] for
    /// limits outside their range or a sender that is registered with an
    /// advertiser (a registration keeps the native sender alive after this
    /// value is dropped, when the completion state no longer exists).
    pub fn from_sender(
        sender: Sender,
        limits: QueueOptions,
    ) -> std::result::Result<(Self, Option<VideoFrame>), Rejected<Sender>> {
        let prepare = || {
            let table = sender.runtime().table::<SendCompletion>()?;
            Ok((table, CallbackState::new(limits)?))
        };
        let (table, callback) = match prepare() {
            Ok(prepared) => prepared,
            Err(error) => return Err(Rejected::new(error, sender)),
        };
        // The conversion takes the native sender only when no registration
        // shares it, and retires the sender's asynchronous video pipeline.
        match sender.into_attached() {
            Ok((sender, earlier)) => Ok((Self::install(sender, callback, table), earlier)),
            Err(sender) => Err(Rejected::new(
                Error::InvalidInput(
                    "queued conversion requires a sender without advertiser registrations".into(),
                ),
                sender,
            )),
        }
    }

    /// Register the completion callback before anything can be submitted.
    ///
    /// The sender's mode owns its native sender exclusively and has no
    /// asynchronous video pipeline, so nothing else is in flight on it: only
    /// the queue sends video on it from now on.
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
        native!(SendCompletion, table => unsafe {
            table.NDIlib_send_set_video_async_completion(
                sender.native_instance(),
                state.cast_mut().cast(),
                Some(completion),
            )
        });
        Self { sender, callback }
    }

    /// The sender this queue sends through, for the operations that do not
    /// send video: [`source`](Sender::source),
    /// [`send_audio`](Sender::send_audio),
    /// [`send_metadata`](Sender::send_metadata) and
    /// [`capture_metadata`](Sender::capture_metadata),
    /// [`tally`](Sender::tally), [`connection_count`](Sender::connection_count),
    /// connection metadata, failover, and the Advanced controls.
    ///
    /// Its mode has none of the video sends, which would bypass the queue, and
    /// it cannot be registered with an advertiser. It is `Sync`, but it
    /// borrows this queue, so it is used between submissions rather than
    /// during one.
    pub fn sender(&self) -> &Sender<QueueAttached> {
        &self.sender
    }

    /// Queue a frame without waiting for an earlier submission to complete.
    ///
    /// The queue owns the frame until it is collected: the SDK writes to a
    /// compressed frame's bytes while it sends them. A collected frame can be
    /// submitted again. Submission does not scan previously submitted frames.
    /// It allocates one retained native descriptor. Lending an uncompressed
    /// frame can also grow its pixel allocation to provide the SDK tail room
    /// its scan type needs; otherwise pixels and metadata move without copies.
    ///
    /// # Errors
    ///
    /// Returns the frame with [`Error::QueueFull`] when the frame would take
    /// the queue past a limit in [`QueueOptions`] ([`QueueLimit::Frames`] or
    /// [`QueueLimit::Bytes`]; collect completed frames and submit it again),
    /// with [`Error::UnexpectedSdkResult`] when the queue has failed (see
    /// [`completion_protocol_failed`](Self::completion_protocol_failed)),
    /// with [`Error::UnsupportedCapability`] when the runtime cannot send an
    /// uncompressed frame's pixel format (as [`Sender::send_video`] checks),
    /// and with [`Error::InvalidFrame`] when the frame is not sendable.
    pub fn try_submit(
        &mut self,
        frame: impl Into<QueuedVideoFrame>,
    ) -> std::result::Result<SubmissionId, Rejected<QueuedVideoFrame>> {
        let mut frame = frame.into();
        let failure = Rejected::new;
        if let Err(error) = frame.validate(&self.sender) {
            return Err(failure(error, frame));
        }
        if self.callback.protocol_error.load(Ordering::Acquire) {
            return Err(failure(
                Error::UnexpectedSdkResult(
                    "native completion protocol failed; drain and close this queue".into(),
                ),
                frame,
            ));
        }
        // Lending can grow a frame's pixel allocation: count it afterwards.
        let header = AliasableBox::new(Box::new(frame.raw()));
        let bytes = frame.retained_bytes();
        let key = header.p_data as usize;
        let pointer = header.as_ptr();
        let id = {
            let mut state = self
                .callback
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let Some(total) = state
                .bytes
                .checked_add(bytes)
                .filter(|total| *total <= state.options.bytes)
            else {
                return Err(failure(Error::QueueFull(QueueLimit::Bytes), frame));
            };
            if state.retained() >= state.options.frames {
                return Err(failure(Error::QueueFull(QueueLimit::Frames), frame));
            }
            let Some(next) = state.next.checked_add(1) else {
                return Err(failure(
                    Error::InvalidInput("queue submission identifiers exhausted".into()),
                    frame,
                ));
            };
            let id = SubmissionId(state.next);
            state.next = next;
            state.bytes = total;
            // Both QueuedVideoFrame variants own nonempty allocations:
            // VideoFrame's validated Pixels and CompressedVideoFrame's
            // AliasableBox<[u8]>. Neither exposes shared ownership, cloning
            // copies the bytes, and submission moves the frame here until
            // collection. Its data address is therefore non-null and distinct
            // from every retained frame, without searching either collection.
            state.in_flight.insert(
                key,
                Entry {
                    id,
                    _header: header,
                    frame,
                    bytes,
                },
            );
            id
        };
        // The entry is recorded before the call and the lock released, because
        // the SDK may complete the frame before the call returns.
        // SAFETY: the sender is live, and the queue owns the descriptor and
        // the frame it lends until the SDK reports completion.
        unsafe {
            self.sender
                .core()
                .NDIlib_send_send_video_async_v2(self.sender.native_instance(), pointer)
        };
        Ok(id)
    }

    /// Take one completed frame, if there is one.
    pub fn try_take_completed(&mut self) -> Option<CompletedVideo> {
        let mut state = self
            .callback
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        take_completed(&mut state)
    }

    /// Wait up to `timeout` for a completed frame. `None` means none completed
    /// in time; the queue is unchanged and can still be used.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`],
    /// and [`Error::UnexpectedSdkResult`] when the queue has failed (see
    /// [`completion_protocol_failed`](Self::completion_protocol_failed)).
    pub fn wait_completed(&mut self, timeout: Duration) -> Result<Option<CompletedVideo>> {
        let deadline = Deadline::after(timeout)?;
        let mut state = self
            .callback
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(frame) = take_completed(&mut state) {
                return Ok(Some(frame));
            }
            if self.callback.protocol_error.load(Ordering::Acquire) {
                return Err(Error::UnexpectedSdkResult(
                    "native completion protocol failed".into(),
                ));
            }
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Ok(None);
            }
            let (next, _) = self
                .callback
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|p| p.into_inner());
            state = next;
        }
    }

    /// The number of frames held: those still being sent and those completed
    /// but not yet collected.
    pub fn retained_frames(&self) -> usize {
        self.callback
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retained()
    }

    /// Whether the SDK has reported a completion that matches no queued frame,
    /// or the same one twice.
    ///
    /// The queue then no longer knows which frames the SDK still reads, so it
    /// accepts no more submissions; frames already completed can still be
    /// collected, and dropping the sender releases the rest safely.
    pub fn completion_protocol_failed(&self) -> bool {
        self.callback.protocol_error.load(Ordering::Acquire)
    }
}

fn take_completed(state: &mut State) -> Option<CompletedVideo> {
    let entry = state.ready.pop_front()?;
    state.bytes -= entry.bytes;
    Some(CompletedVideo {
        id: entry.id,
        frame: entry.frame,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measure submission with a completed backlog, reusing each collected
    /// frame. No receiver is needed: the SDK completes sends with no peers.
    #[test]
    #[ignore = "manual release-mode queue submission benchmark"]
    fn benchmark_queued_submission_backlog() {
        use std::{hint::black_box, time::Instant};

        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        for backlog in [8, 512, 4096] {
            let options = SenderOptions::new(crate::test_support::unique_name(&format!(
                "queue benchmark {backlog}"
            )))
            .clock_video(false);
            let mut queue =
                QueuedSender::new(&ndi, &options, QueueOptions::new().max_frames(backlog + 1))
                    .unwrap();
            for _ in 0..backlog {
                queue
                    .try_submit(VideoFrame::builder().resolution(2, 2).build().unwrap())
                    .unwrap();
            }
            let deadline = Deadline::after(Duration::from_secs(5)).unwrap();
            let mut state = queue.callback.state.lock().unwrap();
            while state.ready.len() < backlog {
                assert!(!deadline.is_expired(), "the SDK must complete every frame");
                state = queue
                    .callback
                    .changed
                    .wait_timeout(state, deadline.remaining())
                    .unwrap()
                    .0;
            }
            drop(state);

            let mut frame =
                QueuedVideoFrame::from(VideoFrame::builder().resolution(2, 2).build().unwrap());
            let iterations = 10_000;
            let start = Instant::now();
            for _ in 0..iterations {
                black_box(queue.try_submit(frame).unwrap());
                frame = queue
                    .try_take_completed()
                    .or_else(|| queue.wait_completed(Duration::from_secs(5)).unwrap())
                    .expect("the SDK must complete every frame")
                    .into_frame();
            }
            eprintln!(
                "{backlog} retained frames: {:.0} ns per submit/collect",
                start.elapsed().as_nanos() as f64 / f64::from(iterations)
            );
            assert!(!queue.completion_protocol_failed());
        }
    }

    fn insert(state: &CallbackState, id: u64) -> usize {
        let mut frame =
            QueuedVideoFrame::Pixels(VideoFrame::builder().resolution(2, 2).build().unwrap());
        let header = AliasableBox::new(Box::new(frame.raw()));
        let key = header.p_data as usize;
        let bytes = frame.retained_bytes();
        let mut s = state.state.lock().unwrap();
        s.bytes += bytes;
        s.in_flight.insert(
            key,
            Entry {
                id: SubmissionId(id),
                _header: header,
                frame,
                bytes,
            },
        );
        key
    }

    #[test]
    fn concurrent_and_out_of_order_callbacks_preserve_owned_frames() {
        let state = CallbackState::new(QueueOptions::default()).unwrap();
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

    /// Converting a sender does not call its completion handler: a handler
    /// that would panic cannot lose the sender, and the frame still in
    /// flight comes back beside the queue.
    #[test]
    fn converting_a_sender_runs_no_completion_handler() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let options = SenderOptions::new(crate::test_support::unique_name("Queued conversion"))
            .clock_video(false);
        let mut sender = Sender::new(&ndi, &options).unwrap();
        sender.on_async_video_done(|_| panic!("a conversion must not run the completion handler"));
        let frame = VideoFrame::builder().resolution(16, 16).build().unwrap();
        let pixels = frame.data().as_ptr();
        assert!(sender.send_video_async(frame).unwrap().is_none());
        let (mut queue, earlier) =
            QueuedSender::from_sender(sender, QueueOptions::default()).unwrap();
        assert_eq!(earlier.unwrap().data().as_ptr(), pixels);
        // The queue sends on the converted sender.
        let next = VideoFrame::builder().resolution(16, 16).build().unwrap();
        let id = queue.try_submit(next).unwrap();
        let completed = queue.wait_completed(Duration::from_secs(5)).unwrap();
        assert_eq!(completed.unwrap().id(), id);
    }

    #[test]
    fn duplicate_and_unknown_completions_do_not_release_storage() {
        let state = CallbackState::new(QueueOptions::default()).unwrap();
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
        let state = CallbackState::new(QueueOptions::default().max_frames(3)).unwrap();
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
