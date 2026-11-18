//! Recording an NDI source to disk, with explicit finalization.

use std::time::{Duration, Instant};

use super::{
    types::{CheckedRecorderOptions, NamePattern},
    EventText, FileApi, FileBound, FileRecorderOptions, RecorderEvent, RecorderEventKind,
    RecorderStats, RecordingTarget, RecordingTimecode, SourceFormat, SourceSpec, StateConflict,
};
#[cfg(doc)]
use super::{FileFamily, FileNaming};
use crate::{
    sys,
    timeout::{Deadline, Timeout},
    Error, MediaTypes, MetadataSource, Result,
};

mod ledger;
mod native;
mod settle;
mod tracker;
use ledger::Ledger;
pub use ledger::RecordingReport;
use native::NativeRecorder;
use settle::{DrainBudget, LateDrain, LocalFinish, Pumped, SessionId};
pub(super) use settle::{FinishReply, FinishWaiters, Settlement, WaitOutcome};
use tracker::FileTracker;

/// The longest single wait for an event while finalization is pending. Short
/// enough that a caller's interruption is noticed promptly.
const FINISH_POLL: Duration = Duration::from_millis(25);

/// The wait for a further event once every file is reported closed: long
/// enough not to spin while the SDK finishes stopping.
const SETTLE_POLL: Duration = Duration::from_millis(1);

/// How many stale events [`FileRecorder::start`] discards before giving up on
/// finding the queue empty, and how many queued events a wait takes after its
/// deadline before it reports a timeout.
pub(super) const STALE_EVENT_LIMIT: usize = 256;

/// Where a recorder is in its session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RecorderState {
    /// No session has been started.
    Idle,
    /// A session is recording, or waiting for its source.
    Recording,
    /// The session is stopping and its last file is not yet reported closed.
    Stopping,
    /// The session ended and every file it opened was reported closed.
    Closed,
    /// The session ended without [`RecordingReport::closure_confirmed`].
    /// Events were lost, unreadable or inconsistent, or the session ended
    /// before reporting any file. Incomplete tracking requires
    /// [`FileRecorder::reset`] before starting another session; see
    /// [`StateConflict::IncompleteFileTracking`].
    Indeterminate,
}

/// What [`FileRecorder::request_stop`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StopRequest {
    /// This call asked the SDK to stop.
    Requested,
    /// The session was already stopping.
    AlreadyStopping,
    /// No session has been started.
    NotStarted,
    /// The session had already ended with every file closed.
    AlreadyClosed,
    /// The session had already ended without confirmation that every file
    /// was closed.
    AlreadyEnded,
}

/// The outcome of finishing a recording session.
///
/// [`FileRecorder::finish`] and [`AsyncFileRecorder::finish`](super::AsyncFileRecorder::finish)
/// retain a report even when their completion budget expires. The corresponding
/// `wait_for_finish` methods return `None` on timeout and `Some` for a closed or
/// indeterminate session.
///
/// A synchronous recorder stays usable whatever the outcome, but incomplete
/// file tracking requires [`FileRecorder::reset`] before another session can
/// start. An async recorder can be closed by another clone while the operation
/// is running.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FinishStatus {
    /// The session ended within the completion budget.
    Ended(SessionEnd),
    /// `finish`'s completion budget expired before the session ended.
    /// Call [`FileRecorder::wait_for_finish`] to keep waiting, or inspect the
    /// recorder's current report.
    /// An async operation interrupted by a close reports [`StateConflict::Closed`]
    /// instead when closure cannot be established in its remaining budget.
    TimedOut(RecordingReport),
}

/// The final outcome of a recording session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SessionEnd {
    /// Every file the session opened is finalized. The report still records
    /// any errors during the session, and
    /// [`stopped_by_sdk`](RecordingReport::stopped_by_sdk) says whether the
    /// recording ran until it was asked to stop or the SDK ended it sooner.
    Closed(RecordingReport),
    /// The session ended without [`RecordingReport::closure_confirmed`].
    /// Incomplete file tracking requires [`FileRecorder::reset`] before
    /// another session starts. See [`StateConflict::IncompleteFileTracking`].
    Indeterminate(RecordingReport),
}

/// A file the recorder's events reported open.
struct OpenFile {
    path: EventText,
    /// The SDK reported the first audio or video packet written to it.
    has_media: bool,
    /// Opening order is independent of the tracker's path-sorted storage.
    order: u64,
}

/// What initiated the owner's cleanup. Abandon can replace an earlier close
/// while that close is draining; synchronous drop never requests that change.
#[derive(Clone, Copy)]
pub(super) enum CleanupRequest {
    Drop,
    Close,
    Abandon,
}

/// A recorder that writes one NDI source to disk.
///
/// The recorder subscribes to the source named in its options and stores the
/// compressed essence as it arrives; it takes no frames from Rust. A session
/// runs from [`start`](Self::start) until its last file is closed, and may
/// span several files through [`rollover`](Self::rollover),
/// [`set_source`](Self::set_source) and the SDK's own rollovers.
///
/// # Finishing
///
/// The SDK finalizes a file only between the stop request and its file-closed
/// event. [`finish`](Self::finish) performs both and reports the outcome;
/// treat a recording as complete only when it returns
/// [`SessionEnd::Closed`]. Dropping a recorder waits up to
/// [`FileRecorderOptions::drop_wait`] while the session is open or file cleanup
/// remains pending, including after an indeterminate outcome. Native event
/// loss, unreadable lifecycle evidence or a tracking limit makes the file set
/// incomplete. Even if every known file closes, drop then consumes the
/// remaining original budget and another session requires [`reset`](Self::reset).
/// This budget starts at drop and includes the native stop; synchronous
/// `finish` starts its event wait after the stop returns.
/// Drop cannot report whether cleanup succeeded;
/// [`abandon`](Self::abandon) skips the wait.
///
/// # What a closed file means
///
/// A report of closure repeats what the SDK said: that it finalized the file.
/// A closed file is not proof its data was written. Inspect the report and
/// read back recordings whose completeness matters. See
/// [the module's limits](super#limits-of-the-ndi-64-file-library).
///
/// # Events
///
/// Events are taken with [`next_event`](Self::next_event), and
/// [`finish`](Self::finish) takes them itself while it waits; their effect on
/// the session is kept in the [`report`](Self::report) either way. The SDK
/// discards events that are not taken promptly, and a session that lost
/// events can no longer be confirmed closed, so poll throughout a recording.
/// File bookkeeping retains at most 16,384 entries and 16 MiB, accounting for
/// entry storage and path capacity. Exceeding either private limit or failing
/// to reserve storage makes tracking incomplete without dropping delivered
/// events or changing the native dropped-event count.
///
/// A recorder belongs to the thread that created it, so it is neither
/// [`Send`] nor [`Sync`]: the File API documents no thread on which a
/// recorder may be used or destroyed, nor whether two calls on one recorder
/// may overlap. Methods that call into the SDK may block; cached state and
/// report snapshots do not call it.
/// [`AsyncFileRecorder`](super::AsyncFileRecorder) offers the same operations
/// as futures.
pub struct FileRecorder {
    native: NativeRecorder,
    /// The configured tracks, used when starting each new session.
    tracks: MediaTypes,
    /// The source in effect, when the last change of source was accepted.
    accepted_source: Option<SourceSpec>,
    /// How the recorder names its files.
    naming: NamePattern,
    ledger: Ledger,
    generation: u64,
    cleanup: Option<Timeout>,
}

/// Rust-only creation state: its validation and family check require no
/// native object, so this value can move to an owner thread before creation.
pub(super) struct PreparedRecorder {
    recording: FileBound<sys::FileRecording>,
    options: CheckedRecorderOptions,
}

impl PreparedRecorder {
    pub(super) fn create(self) -> Result<FileRecorder> {
        let Self { recording, options } = self;
        let native = NativeRecorder::create(recording, &options)?;
        Ok(FileRecorder {
            native,
            tracks: options.tracks,
            cleanup: Some(options.drop_wait),
            accepted_source: Some(options.source),
            naming: options.naming,
            ledger: Ledger::idle(),
            generation: 0,
        })
    }
}

impl std::fmt::Debug for FileRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileRecorder")
            .field("state", &self.ledger.state())
            .field("session", &self.ledger.session_number())
            .finish_non_exhaustive()
    }
}

impl FileRecorder {
    /// Create a recorder for the source named in `options`.
    ///
    /// Nothing is written until [`FileRecorder::start`]. The options are only
    /// read, so they stay with the caller whatever the outcome; the recorder
    /// keeps its own copy of the source they name.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the options are malformed or conflict:
    /// an empty track selection, a drop wait beyond [`crate::MAX_TIMEOUT`], a
    /// malformed [`FileNaming::Pattern`], or an exact file name
    /// ([`FileNaming::Exact`], or a pattern without `{index}`) with
    /// format-change rollover switched on or a nonzero rollover size.
    /// [`Error::InvalidCString`] when the filename pattern contains a NUL.
    /// [`Error::NativeOperationFailed`] when the SDK cannot create the
    /// recorder and [`Error::UnsupportedCapability`] when the library does not
    /// provide [`FileFamily::Recording`].
    #[doc(alias = "NDIlib_file_recorder_create")]
    pub fn new(api: &FileApi, options: &FileRecorderOptions) -> Result<Self> {
        Self::prepare(api.clone(), options)?.create()
    }

    pub(super) fn prepare(api: FileApi, options: &FileRecorderOptions) -> Result<PreparedRecorder> {
        let recording = api.into_table::<sys::FileRecording>()?;
        let options = options.check()?;
        Ok(PreparedRecorder { recording, options })
    }

    /// Where the recorder is in its session.
    ///
    /// The state follows the events taken so far. A source that disconnects
    /// leaves a session [`RecorderState::Recording`]; recording resumes when
    /// it returns.
    pub fn state(&self) -> RecorderState {
        self.ledger.state()
    }

    /// What the events taken so far have shown about the current session.
    pub fn report(&self) -> RecordingReport {
        self.ledger.report()
    }

    fn session_id(&self) -> SessionId {
        SessionId {
            recorder: self.generation,
            session: self.ledger.session_number(),
        }
    }

    /// Start a session writing to `target`, and return its number.
    ///
    /// The recorder's [`FileNaming`](super::FileNaming) turns the target into
    /// file names. With the default indexed names the lowest unused index is
    /// chosen; with an exact file name a file that already exists is refused.
    /// The directory the naming leads to must exist. The check does not
    /// reserve the file: a collision created afterwards reaches the SDK and
    /// can cause a native open failure. Linux File SDK 6.4.0.2 uses exclusive
    /// creation and preserves the colliding file; other platforms and SDK
    /// releases have not been verified. Use a recorder-controlled directory
    /// for uninterrupted recording and destination containment. Parent
    /// directories are not pinned against replacement.
    ///
    /// Queued events from an earlier session, including unreadable events,
    /// are consumed and discarded before the native start. Poll them before
    /// starting if their contents are needed. The asynchronous recorder instead
    /// forwards these events through its `next_event` operation.
    ///
    /// The target is only read, so it stays with the caller whatever the
    /// outcome, for example to start again once a missing directory exists.
    /// [`RecordingTimecode::GenerateFrom`] takes a validated
    /// [`SessionTimecode`](super::SessionTimecode), so the native start always
    /// receives an explicit timecode rather than the send synthesize sentinel.
    ///
    /// # Errors
    ///
    /// * [`Error::InvalidState`] with [`StateConflict::PreviousSessionOpen`]
    ///   while an earlier session is recording, stopping, awaiting a known
    ///   file's closure under complete tracking; finish or poll it first.
    ///   [`StateConflict::IncompleteFileTracking`] after a session ends with
    ///   incomplete tracking; [`reset`](Self::reset) is required even after
    ///   every known file closes. [`StateConflict::EarlierSessionEvents`]
    ///   when the SDK keeps reporting events of an earlier session.
    /// * [`Error::InvalidInput`] when the resolved output path is not lossless
    ///   UTF-8, or its longest form exceeds the recorder event's exact path
    ///   limit (4094 UTF-8 bytes with the current SDK), reserving at least
    ///   20 digits per index token. The SDK is not called.
    /// * [`Error::Io`] when the directory of the file the pattern names
    ///   does not exist or cannot be written to, or, with an exact file name,
    ///   with kind [`AlreadyExists`](std::io::ErrorKind::AlreadyExists) when
    ///   that file exists. The SDK is not called.
    ///   On Windows, also reports security queries that could not be completed.
    /// * [`Error::NativeOperationFailed`] when the SDK cannot start the source
    ///   or open the first file. `Processing.NDI.Lib.File.Recorder.h` specifies
    ///   an open-failed event for an output-file open failure; source-start
    ///   failure need not queue one. [`next_event`](Self::next_event) can return
    ///   that event, counted in no session's report. Retrying `start` discards
    ///   queued events before asking the SDK to start again.
    #[doc(alias = "NDIlib_file_recorder_start")]
    #[doc(alias = "NDIlib_file_recorder_start_with_timecode")]
    pub fn start(&mut self, target: &RecordingTarget, timecode: RecordingTimecode) -> Result<u64> {
        self.start_with_sink(target, timecode, &mut |_| {})
    }

    pub(super) fn start_with_sink(
        &mut self,
        target: &RecordingTarget,
        timecode: RecordingTimecode,
        sink: &mut impl FnMut(Result<RecorderEvent>),
    ) -> Result<u64> {
        self.ledger.admit_start()?;
        self.naming.check(target)?;
        // Events left over from an earlier session must not be counted in this one.
        self.discard_stale_events(sink)?;
        // Draining may reveal lost or inconsistent lifecycle evidence for a
        // previously final session. Its immutable report does not release
        // the resulting unknown native-file obligation.
        self.ledger.dropped(self.dropped_event_count());
        self.ledger.admit_start()?;
        let session = self
            .ledger
            .session_number()
            .checked_add(1)
            .ok_or(Error::InvalidInput(
                crate::InputDefect::SessionCounterExhausted,
            ))?;
        let dropped = self.dropped_event_count();
        self.native.start(target, timecode)?;
        self.ledger = Ledger::session(
            session,
            dropped,
            self.tracks.contains(MediaTypes::VIDEO) || self.tracks.contains(MediaTypes::AUDIO),
        );
        Ok(session)
    }

    /// Take events until the queue is empty, so that none from an earlier
    /// session is attributed to the next one.
    fn discard_stale_events(&mut self, sink: &mut impl FnMut(Result<RecorderEvent>)) -> Result<()> {
        let mut budget = DrainBudget::late();
        loop {
            match self.pump(&mut budget, Duration::ZERO, sink) {
                Pumped::Empty { .. } => return Ok(()),
                Pumped::Exhausted => {
                    return Err(Error::InvalidState(StateConflict::EarlierSessionEvents));
                }
                Pumped::Took { .. } => {}
            }
        }
    }

    /// Ask the SDK to stop the session. Idempotent.
    ///
    /// The SDK then finalizes the open file and reports it closed; this call
    /// alone does not show that it has. Use [`finish`](Self::finish) or
    /// [`wait_for_finish`](Self::wait_for_finish) to find out.
    #[doc(alias = "NDIlib_file_recorder_stop")]
    pub fn request_stop(&mut self) -> StopRequest {
        match self.state() {
            RecorderState::Idle => StopRequest::NotStarted,
            RecorderState::Closed => StopRequest::AlreadyClosed,
            RecorderState::Indeterminate => StopRequest::AlreadyEnded,
            RecorderState::Stopping => StopRequest::AlreadyStopping,
            RecorderState::Recording => {
                let still_recording = self.is_recording();
                self.native
                    .apply(self.ledger.stop_requested(still_recording));
                StopRequest::Requested
            }
        }
    }

    /// Take the next queued event, waiting up to `timeout` for one, or
    /// `None` when none arrives in time.
    ///
    /// The event's effect on the session is recorded before it is returned,
    /// including when the event itself is malformed.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`],
    /// and [`Error::UnexpectedSdkResult`] when the SDK's event does not match
    /// this crate's structure layout or its returned event type. An unreadable
    /// format is withheld through [`RecorderEvent::source_format`] while the
    /// rest of the event is delivered.
    #[doc(alias = "NDIlib_file_recorder_capture_event")]
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<RecorderEvent>> {
        self.poll_event_ms(crate::to_ms_checked(timeout)?)
    }

    /// [`next_event`](Self::next_event), waiting up to `wait_ms` milliseconds.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] for a whole-event layout or type mismatch.
    fn poll_event_ms(&mut self, wait_ms: u32) -> Result<Option<RecorderEvent>> {
        let event = self.native.capture(wait_ms);
        if self.ledger.cleanup_pending()
            || (self.ledger.tracker().is_some() && !matches!(&event, Ok(None)))
        {
            self.ledger.dropped(self.dropped_event_count());
        }
        let Some(event) = event.inspect_err(|_| self.ledger.mark_uncertain())? else {
            // Query recording only when the closed or incomplete file set
            // can end the session. This relies on open-before-close rollover
            // events, pinned by `rollovers_open_the_next_file_before_closing_the_previous_file`.
            let transition = self.ledger.drained(|| self.native.is_recording());
            self.native.apply(transition);
            return Ok(None);
        };
        self.native.apply(self.ledger.observe(
            RecorderEventKind::from_raw(event.type_),
            EventText::from_buffer(&event.p_path),
        ));
        // Decoded after the ledger is updated, so that a malformed payload
        // cannot hide the event that closes a file.
        Ok(Some(RecorderEvent::from_sdk(&event)))
    }

    /// Stop the session and wait up to `timeout` for its last file to be
    /// closed.
    ///
    /// Events that arrive during the wait are consumed; their effect is in
    /// the returned report. A malformed event is consumed too: what it showed
    /// about the session is recorded as [`next_event`](Self::next_event)
    /// records it, and the wait goes on. One that could not be read at all
    /// makes the outcome [`SessionEnd::Indeterminate`] rather than an error.
    ///
    /// The wait starts once the stop has returned, and covers taking the
    /// closure's event, not the stop itself, which the SDK may carry out
    /// before it returns. Events already queued are taken even when the wait
    /// is over, so `finish(Duration::ZERO)` reports [`SessionEnd::Closed`]
    /// when the SDK finalized the file during the stop.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`],
    /// without requesting the stop, and [`Error::InvalidState`] with
    /// [`StateConflict::StopNotRequested`] when no session was started. Neither
    /// is returned once the wait has begun: whatever the SDK's events hold,
    /// the outcome is a [`FinishStatus`].
    #[doc(alias = "NDIlib_file_recorder_stop")]
    #[doc(alias = "NDIlib_file_recorder_capture_event")]
    pub fn finish(&mut self, timeout: Duration) -> Result<FinishStatus> {
        // Checked here, so that a timeout out of range leaves the session
        // recording; the wait driver's budget starts only after the stop.
        let timeout = Timeout::new(timeout)?;
        self.request_stop();
        // Synchronous finish's documented event-wait budget begins after
        // the native stop returns. The checked duration survives that call.
        self.wait_until(Deadline::starting(Instant::now(), timeout)?)
    }

    /// Wait up to `timeout` for a stopping session to end.
    ///
    /// Returns `None` when the timeout expires, and `Some` with
    /// [`SessionEnd::Closed`] or [`SessionEnd::Indeterminate`] when the
    /// session ends. A timeout retains the session's report in
    /// [`report`](Self::report), and another wait can continue observing it.
    ///
    /// Use this to keep waiting after `finish` returns [`FinishStatus::TimedOut`].
    /// Events already queued are taken even when `timeout` is over, so a closure
    /// that has been reported is never missed. Malformed events are taken and
    /// recorded as [`finish`](Self::finish) describes, without ending the
    /// wait.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`],
    /// and [`Error::InvalidState`] with
    /// [`StateConflict::StopNotRequested`] when no session was started or the
    /// session has not been asked to stop. Neither is returned once the wait
    /// has begun.
    #[doc(alias = "NDIlib_file_recorder_capture_event")]
    pub fn wait_for_finish(&mut self, timeout: Duration) -> Result<Option<SessionEnd>> {
        self.settle_until(Deadline::after(timeout)?)
            .map(WaitOutcome::into_end)
    }

    pub(super) fn wait_until(&mut self, deadline: Deadline) -> Result<FinishStatus> {
        self.settle_until(deadline).map(WaitOutcome::into_finish)
    }

    fn settle_until(&mut self, deadline: Deadline) -> Result<WaitOutcome> {
        self.check_wait()?;
        let result = std::cell::RefCell::new(None);
        let mut waiters = FinishWaiters::new();
        waiters.register(self, deadline, LocalFinish(&result));
        let mut budget = DrainBudget::new(deadline);
        loop {
            // A synchronous caller already has this final ledger. Preserve
            // its queued events for next_event rather than draining again.
            if let Some(outcome) = self.settle_wait(None) {
                return Ok(outcome);
            }
            waiters.settle_at(
                self,
                Settlement {
                    cleanup_expired: false,
                    at: Instant::now(),
                },
                &mut |_| {},
                || false,
                Some(&mut budget),
            );
            if let Some(result) = result.borrow_mut().take() {
                return result;
            }
            let step = if self.ledger.all_closed() {
                SETTLE_POLL
            } else {
                FINISH_POLL
            };
            self.pump(&mut budget, step, &mut |_| {});
        }
    }

    pub(super) fn check_wait(&self) -> Result<()> {
        if matches!(self.state(), RecorderState::Idle | RecorderState::Recording) {
            Err(Error::InvalidState(StateConflict::StopNotRequested))
        } else {
            Ok(())
        }
    }

    fn settle_wait(&self, late: Option<&LateDrain>) -> Option<WaitOutcome> {
        self.ledger.settle(late)
    }

    /// Share abandon's cleanup policy with the async owner's shutdown driver.
    pub(super) fn skip_cleanup(&mut self) {
        self.cleanup = None;
    }

    /// Start cleanup once, retaining an async close's original submission time.
    fn begin_cleanup(&mut self, requested: Instant) -> DrainBudget {
        DrainBudget::cleanup(self.cleanup.take(), requested)
    }

    pub(super) fn cleanup_pending(&self) -> bool {
        self.ledger.cleanup_pending()
    }

    /// Poll once through the shared deadline/drain rule and publish its event.
    fn pump(
        &mut self,
        budget: &mut DrainBudget,
        step: Duration,
        sink: &mut impl FnMut(Result<RecorderEvent>),
    ) -> Pumped {
        budget.poll(step, |wait| {
            let polled = self.poll_event_ms(wait);
            let empty = matches!(polled, Ok(None));
            if let Some(event) = polled.transpose() {
                sink(event);
            }
            empty
        })
    }

    /// Drop and async close share the same cleanup driver, including the
    /// waiters' polls: a final drain spends this recorder's budget only once.
    pub(super) fn drain_cleanup<R: FinishReply>(
        &mut self,
        requested: Instant,
        waiters: &mut FinishWaiters<R>,
        queued: impl IntoIterator<Item = (Deadline, R)>,
        tick: Duration,
        sink: &mut impl FnMut(Result<RecorderEvent>),
        closing: impl Fn() -> CleanupRequest,
    ) {
        if matches!(closing(), CleanupRequest::Abandon) {
            self.skip_cleanup();
        }
        let mut budget = self.begin_cleanup(requested);
        self.request_stop();
        for (deadline, reply) in queued {
            waiters.register(self, deadline, reply);
        }
        waiters.retain();
        while self.cleanup_pending() || !waiters.is_empty() {
            if matches!(closing(), CleanupRequest::Abandon) {
                // A close may already be draining when abandon is submitted.
                // Stop spending its remaining budget before the next native
                // poll. An in-progress native call itself is not interruptible.
                budget = DrainBudget::Spent;
            }
            let polled = self.pump(&mut budget, waiters.wait_timeout(tick), sink);
            waiters.settle_at(
                self,
                Settlement {
                    cleanup_expired: polled.expired(),
                    at: Instant::now(),
                },
                sink,
                || !matches!(closing(), CleanupRequest::Drop),
                Some(&mut budget),
            );
            if matches!(budget, DrainBudget::Spent) {
                break;
            }
        }
        debug_assert!(waiters.is_empty(), "a completed drain settles every waiter");
    }

    /// Stop and destroy the recorder at once, without waiting for the SDK to
    /// finalize an open file, and return what was known about the session.
    ///
    /// The file of a session that had not ended may be left truncated.
    #[doc(alias = "NDIlib_file_recorder_destroy")]
    pub fn abandon(mut self) -> RecordingReport {
        self.skip_cleanup();
        self.report()
    }

    /// Abandon this native recorder and replace it with a new idle recorder.
    ///
    /// Use this after a finalization timeout when the SDK stopped producing
    /// events, or after native event loss, unreadable lifecycle evidence or a
    /// tracking limit made the old session's file tracking incomplete. Incomplete
    /// tracking causes [`StateConflict::IncompleteFileTracking`] on another
    /// start after the session ends, even if all known files later close. A
    /// successful reset clears that refusal. The returned
    /// report preserves that uncertainty; the old file may be truncated.
    /// The replacement has its own event queue, so delayed events from the old
    /// session cannot affect a new recording. The last report is retained and
    /// session numbers continue increasing across resets.
    ///
    /// The replacement is created first. If creation fails, this recorder and
    /// its report are kept and can still be polled or finalized. The options
    /// are only read, as by [`FileRecorder::new`].
    ///
    /// # Errors
    ///
    /// The errors of [`FileRecorder::new`].
    #[doc(alias = "NDIlib_file_recorder_destroy")]
    #[doc(alias = "NDIlib_file_recorder_create")]
    pub fn reset(&mut self, options: &FileRecorderOptions) -> Result<RecordingReport> {
        self.reset_with(|recorder| {
            PreparedRecorder {
                recording: recorder.native.recording(),
                options: options.check()?,
            }
            .create()
        })
    }

    pub(super) fn reset_checked(
        &mut self,
        options: CheckedRecorderOptions,
    ) -> Result<RecordingReport> {
        self.reset_with(|recorder| {
            PreparedRecorder {
                recording: recorder.native.recording(),
                options,
            }
            .create()
        })
    }

    fn reset_with(
        &mut self,
        create: impl FnOnce(&Self) -> Result<Self>,
    ) -> Result<RecordingReport> {
        let generation = self.generation.checked_add(1).ok_or(Error::InvalidInput(
            crate::InputDefect::RecorderGenerationExhausted,
        ))?;
        let mut replacement = create(self)?;
        replacement.generation = generation;
        replacement.ledger = Ledger::inheriting(self.report());
        Ok(std::mem::replace(self, replacement).abandon())
    }

    /// Whether the SDK is recording or waiting for frames. False once a stop
    /// has taken effect; this alone does not show that a file was finalized.
    #[doc(alias = "NDIlib_file_recorder_is_recording")]
    pub fn is_recording(&self) -> bool {
        self.native.is_recording()
    }

    /// How many events the SDK has discarded since the recorder was created
    /// because they were not taken in time.
    #[doc(alias = "NDIlib_file_recorder_get_dropped_event_count")]
    pub fn dropped_event_count(&self) -> u64 {
        self.native.dropped_event_count()
    }

    /// Switch to recording another source.
    ///
    /// During a session the SDK starts the new source at once and rolls over
    /// to a new file, named from the session's current target. Before a
    /// session, the source is used by the next [`start`](Self::start).
    ///
    /// A recorder with an exact file name
    /// ([`FileNaming::Exact`](super::FileNaming::Exact), or a
    /// [`FileNaming::Pattern`](super::FileNaming::Pattern) without `{index}`)
    /// cannot roll over to a file of the same name, and the SDK would end the
    /// session instead, so it refuses a change of source during a session
    /// and the session goes on. Before a session the change works as for any
    /// recorder.
    ///
    /// The source stays with the caller; the recorder keeps a copy of it
    /// when the SDK accepts it, for
    /// [`last_accepted_source`](Self::last_accepted_source).
    ///
    /// # Errors
    ///
    /// * [`Error::InvalidState`] with [`StateConflict::ExactFileName`]
    ///   during a session, recording or stopping, of a recorder with an exact
    ///   file name, and with [`StateConflict::Stopping`] while any other
    ///   recorder's session is stopping. The SDK is not called.
    /// * [`Error::NativeOperationFailed`] when the SDK cannot start the source. The
    ///   SDK does not say which source is then in effect, so
    ///   [`last_accepted_source`](Self::last_accepted_source) returns `None`
    ///   until a later change succeeds.
    #[doc(alias = "NDIlib_file_recorder_set_source")]
    pub fn set_source(&mut self, source: &SourceSpec) -> Result<()> {
        if self.naming.is_exact() && self.ledger.session_open() {
            return Err(Error::InvalidState(StateConflict::ExactFileName));
        }
        if self.state() == RecorderState::Stopping {
            return Err(Error::InvalidState(StateConflict::Stopping));
        }
        let accepted = self.native.set_source(source);
        // Kept only once accepted: the copy is what the recorder retains.
        self.accepted_source = accepted.is_ok().then(|| source.clone());
        accepted
    }

    /// The source the SDK accepted last: the one given at creation, or by the
    /// last successful [`set_source`](Self::set_source).
    ///
    /// `None` after a rejected change, because the SDK does not say which
    /// source is then in effect. This is what was requested, not a statement
    /// that the source is connected.
    pub fn last_accepted_source(&self) -> Option<&SourceSpec> {
        self.accepted_source.as_ref()
    }

    /// The most recently opened file still retained in the file tracker.
    ///
    /// The path is as the SDK gave it, and can lag events not yet taken. Once
    /// tracking is incomplete, later opens are not retained; consume
    /// [`next_event`](Self::next_event) for their paths. `None` does not prove
    /// that no files remain open.
    pub fn reported_current_file(&self) -> Option<&EventText> {
        self.ledger.tracker().and_then(FileTracker::current)
    }

    /// Close the current file and continue the session in a new one named
    /// from `target`.
    ///
    /// For compressed video the SDK makes the cut at the next keyframe, so the
    /// new file starts decodable. A rollover that has not happened by the time
    /// the session stops is dropped.
    ///
    /// The SDK accepts a rollover it then cannot carry out, and ends the
    /// session when the new file cannot be opened: it reports open-failed,
    /// closes the old file and reports an error. The file the pattern names
    /// for the target is therefore checked first: its directory must exist
    /// and be writable, and with an exact file name the file must not exist.
    /// The usual mistakes then leave the session running, and the target,
    /// which is only read, stays with the caller to correct and try again.
    /// The preflight is not a reservation: a path created after it can make
    /// the later native open fail and end the session even after this method
    /// returns `Ok(())`. Observe open-failed/error events and the final
    /// [`RecordingReport`]. The exclusive-creation behavior tested on Linux
    /// File SDK 6.4.0.2 preserves colliding regular files and symlinks; other
    /// platforms and SDK releases have not been verified. A
    /// recorder-controlled output directory avoids this race with other writers.
    ///
    /// # Errors
    ///
    /// * [`Error::InvalidState`] with [`StateConflict::NotRecording`] unless
    ///   a session is recording.
    /// * [`Error::InvalidInput`] when the resolved output path is not lossless
    ///   UTF-8, or its longest form exceeds the recorder event's exact path
    ///   limit (4094 UTF-8 bytes with the current SDK), reserving at least
    ///   20 digits per index token. The SDK is not called and the session goes on.
    /// * [`Error::Io`] when the directory of the file the pattern names
    ///   does not exist or cannot be written to, or, with an exact file name,
    ///   with kind [`AlreadyExists`](std::io::ErrorKind::AlreadyExists) when
    ///   that file exists. The SDK is not called and the session goes on.
    ///   On Windows, also reports security queries that could not be completed.
    /// * [`Error::NativeOperationFailed`] when the SDK refuses the target.
    #[doc(alias = "NDIlib_file_recorder_rollover")]
    pub fn rollover(&mut self, target: &RecordingTarget) -> Result<()> {
        if self.state() != RecorderState::Recording {
            return Err(Error::InvalidState(StateConflict::NotRecording));
        }
        self.naming.check(target)?;
        self.native.rollover(target)
    }

    /// Write a metadata frame into the recording at its current position.
    ///
    /// The frame's timecode is stored and re-published on playback; it does
    /// not move the frame on the file's timeline. The frame's text is handed
    /// to the SDK as it is, without a wrapper copy. Owned and captured
    /// metadata are accepted, and need only outlive this synchronous call.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidState`] with [`StateConflict::NotRecording`] unless a
    /// session is recording, and [`Error::NativeOperationFailed`] when the
    /// SDK refuses it, for example because the metadata track is not being
    /// recorded.
    #[doc(alias = "NDIlib_file_recorder_inject_metadata")]
    pub fn inject_metadata<S: MetadataSource + ?Sized>(&mut self, metadata: &S) -> Result<()> {
        if self.state() != RecorderState::Recording {
            return Err(Error::InvalidState(StateConflict::NotRecording));
        }
        self.native.inject_metadata(metadata)
    }

    /// The recorder's running counters, when the SDK has any.
    /// Interpret these with the recorder session state and events; they do
    /// not use receiver drop-percentage semantics.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] when the SDK's structure does not match this
    /// crate's layout.
    #[doc(alias = "NDIlib_file_recorder_get_stats")]
    pub fn stats(&self) -> Result<Option<RecorderStats>> {
        self.native.stats()
    }

    /// The format of the source, once the recorder has seen audio or video.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] when the SDK's structure is malformed.
    #[doc(alias = "NDIlib_file_recorder_get_source_format")]
    pub fn source_format(&self) -> Result<Option<SourceFormat>> {
        self.native.source_format()
    }
}

impl Drop for FileRecorder {
    fn drop(&mut self) {
        // The SDK finalizes a file between the stop request and the
        // file-closed event; destroying sooner may truncate it.
        if self.ledger.cleanup_pending() {
            self.drain_cleanup(
                Instant::now(),
                &mut FinishWaiters::<LocalFinish<'_>>::new(),
                std::iter::empty(),
                FINISH_POLL,
                &mut |_| {},
                || CleanupRequest::Drop,
            );
        }
    }
}

#[cfg(test)]
#[path = "drain_budget_tests.rs"]
mod drain_budget_tests;

#[cfg(ndi_test_file_late_drain)]
#[path = "../../tests/ui/internal/file_late_drain.rs"]
mod late_drain_contract;

#[cfg(any(ndi_test_file_proofs, ndi_test_file_proofs_discarded))]
#[path = "../../tests/ui/internal/file_proofs.rs"]
mod file_proof_contract;
