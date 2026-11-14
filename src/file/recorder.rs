//! Recording an NDI source to disk, with explicit finalization.

use std::{
    ffi::CString,
    marker::PhantomData,
    ptr::NonNull,
    rc::Rc,
    time::{Duration, Instant},
};

use super::{
    out_param::OutParam,
    types::{track_flags, NamePattern},
    EventText, FileApi, RecorderEvent, RecorderEventKind, RecorderOptions, RecorderStats,
    RecordingTarget, RecordingTimecode, SourceFormat, SourceSpec, StateError,
};
use crate::{
    sys::{self, bindings::*},
    timeout::Deadline,
    Error, MediaTypes, MetadataSource, Result,
};

/// The longest single wait for an event while finalization is pending. Short
/// enough that a caller's interruption is noticed promptly.
const FINISH_POLL: Duration = Duration::from_millis(25);

/// The wait for a further event once every file is reported closed: long
/// enough not to spin while the SDK finishes stopping.
const SETTLE_POLL: Duration = Duration::from_millis(1);

/// How many stale events [`FileRecorder::start`] discards before giving up on
/// finding the queue empty, and how many queued events a wait takes after its
/// deadline before it reports a timeout.
const STALE_EVENT_LIMIT: usize = 256;

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
    /// The session ended, but events were lost or inconsistent, so it cannot
    /// be said whether every file was finalized.
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

/// What the recorder's events have shown about a session so far.
///
/// A confirmed closure means the SDK finalized every file it opened. It says
/// nothing about frames dropped while recording, or about the data having
/// reached stable storage.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RecordingReport {
    session: u64,
    phase: SessionPhase,
    closed_files: u64,
    empty_files: u64,
    error_events: u64,
    write_failures: u64,
    dropped_events: u64,
    last_closed_path: Option<EventText>,
}

/// Where the session a [`RecordingReport`] describes is and, once it is
/// stopping, whether the caller asked it to stop while the SDK was still
/// recording.
///
/// The report's [`closure_confirmed`](RecordingReport::closure_confirmed) and
/// [`stopped_by_sdk`](RecordingReport::stopped_by_sdk), and the recorder's
/// [`RecorderState`], are all read from this one value, so they cannot
/// disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SessionPhase {
    /// No session has been started.
    NotStarted,
    /// Recording, or waiting for its source.
    Recording,
    /// Stopping; the last file is not yet reported closed.
    Stopping { requested: bool },
    /// Ended, with every file the session opened reported closed.
    Closed { requested: bool },
    /// Ended, without that confirmation.
    Indeterminate { requested: bool },
}

impl SessionPhase {
    /// Whether the caller asked the session to stop while the SDK was still
    /// recording.
    fn requested(self) -> bool {
        match self {
            Self::NotStarted | Self::Recording => false,
            Self::Stopping { requested }
            | Self::Closed { requested }
            | Self::Indeterminate { requested } => requested,
        }
    }

    fn state(self) -> RecorderState {
        match self {
            Self::NotStarted => RecorderState::Idle,
            Self::Recording => RecorderState::Recording,
            Self::Stopping { .. } => RecorderState::Stopping,
            Self::Closed { .. } => RecorderState::Closed,
            Self::Indeterminate { .. } => RecorderState::Indeterminate,
        }
    }
}

impl RecordingReport {
    /// The session's number, counting from one for each recorder.
    pub fn session(&self) -> u64 {
        self.session
    }
    /// Whether every file the session opened was reported closed.
    pub fn closure_confirmed(&self) -> bool {
        matches!(self.phase, SessionPhase::Closed { .. })
    }
    /// How many files were reported closed, including those closed by rollovers.
    ///
    /// The SDK reports a file closed whether or not anything was written to
    /// it; see [`empty_files`](Self::empty_files).
    pub fn closed_files(&self) -> u64 {
        self.closed_files
    }
    /// How many of the closed files had no audio or video written to them.
    ///
    /// The SDK opens a file as soon as a session or a rollover starts, and
    /// closes it as finalized even if no frame ever reached it: a stop before
    /// the source delivered anything, a switch to a source that is not there,
    /// a rollover while the source is lost. Such a file is empty or holds
    /// only a header, and may not open in a player. A file counts here when
    /// it was closed without the SDK having reported its first frame.
    ///
    /// Always zero for a recorder that records neither audio nor video,
    /// because the SDK reports no first frame for metadata.
    pub fn empty_files(&self) -> u64 {
        self.empty_files
    }
    /// How many open-failed and error events were seen.
    pub fn error_events(&self) -> u64 {
        self.error_events
    }
    /// How many write-failed events were seen. Not every one is fatal: a
    /// stream that appears after a file was opened is dropped until the next
    /// rollover.
    pub fn write_failures(&self) -> u64 {
        self.write_failures
    }
    /// How many events the SDK discarded during the session because they were
    /// not taken in time.
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }
    /// The path of the file reported closed last, as the SDK gave it.
    pub fn last_closed_path(&self) -> Option<&EventText> {
        self.last_closed_path.as_ref()
    }
    /// Whether the SDK ended the session by itself, rather than because it
    /// was asked to stop.
    ///
    /// True once the session has ended, as [`RecorderState::Closed`] or
    /// [`RecorderState::Indeterminate`], without a stop requested through
    /// [`request_stop`](FileRecorder::request_stop),
    /// [`finish`](FileRecorder::finish), [`abandon`](FileRecorder::abandon)
    /// or dropping the recorder. The SDK ends a session after an error it
    /// cannot recover from, such as a new file it cannot open, and at a
    /// format change when the recorder does not roll over; the report's
    /// [`error_events`](Self::error_events) then usually say why. A stop
    /// requested after the SDK had already stopped recording does not count
    /// as the caller's.
    ///
    /// False while the session is open, and for a session that ran until it
    /// was asked to stop. A closed session with this true was finalized, but
    /// did not record for as long as intended.
    pub fn stopped_by_sdk(&self) -> bool {
        matches!(
            self.phase,
            SessionPhase::Closed { requested: false }
                | SessionPhase::Indeterminate { requested: false }
        )
    }
}

/// The outcome of waiting for a session to end.
///
/// A synchronous recorder stays usable whatever the outcome. An async
/// recorder can be closed by another clone while the wait is running.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FinishStatus {
    /// Every file the session opened is finalized. The report still records
    /// any errors during the session, and
    /// [`stopped_by_sdk`](RecordingReport::stopped_by_sdk) says whether the
    /// recording ran until it was asked to stop or the SDK ended it sooner.
    Closed(RecordingReport),
    /// The wait ended first. Call [`FileRecorder::wait_finished`] to keep waiting.
    /// An async wait interrupted by a close reports [`StateError::Closed`]
    /// instead when closure cannot be established in its remaining budget.
    TimedOut(RecordingReport),
    /// The session ended, but its events do not show that every file was
    /// finalized. Waiting longer will not change this.
    Indeterminate(RecordingReport),
}

/// A file the recorder's events reported open.
struct OpenFile {
    path: EventText,
    /// The SDK reported the first audio or video packet written to it.
    has_media: bool,
}

/// The record of one session, built from the recorder's events.
struct Ledger {
    /// The session's report, whose phase is where the session is.
    report: RecordingReport,
    /// The report is that of a session started on the native recorder this
    /// one replaced. This one is idle until it starts its own.
    inherited: bool,
    /// Files reported open and not yet reported closed. A rollover opens the
    /// next file before the previous one is reported closed, so two can be
    /// open at once.
    open_files: Vec<OpenFile>,
    /// The session records audio or video, so every file with content gets a
    /// first-frame event.
    records_media: bool,
    /// An event was lost, malformed or out of place, so `open_files` may not
    /// be the truth.
    uncertain: bool,
    /// The SDK's count of discarded events when the session began.
    dropped_base: u64,
}

impl Ledger {
    /// The ledger of a recorder that has started no session.
    fn idle() -> Self {
        Self::with_report(0, SessionPhase::NotStarted, 0, false)
    }

    /// The ledger of a recorder that replaced one whose last report is
    /// `report`: idle, and numbering its sessions on from that one.
    fn inheriting(report: RecordingReport) -> Self {
        Self {
            report,
            inherited: true,
            ..Self::idle()
        }
    }

    /// The ledger of a session that has just started recording.
    fn session(session: u64, dropped_base: u64, records_media: bool) -> Self {
        Self::with_report(
            session,
            SessionPhase::Recording,
            dropped_base,
            records_media,
        )
    }

    fn with_report(
        session: u64,
        phase: SessionPhase,
        dropped_base: u64,
        records_media: bool,
    ) -> Self {
        Self {
            report: RecordingReport {
                session,
                phase,
                closed_files: 0,
                empty_files: 0,
                error_events: 0,
                write_failures: 0,
                dropped_events: 0,
                last_closed_path: None,
            },
            inherited: false,
            open_files: Vec::new(),
            records_media,
            uncertain: false,
            dropped_base,
        }
    }

    /// Where the recorder is in its session.
    fn state(&self) -> RecorderState {
        if self.inherited {
            RecorderState::Idle
        } else {
            self.report.phase.state()
        }
    }

    /// Whether a session has started and not yet reached a final state.
    fn session_open(&self) -> bool {
        matches!(
            self.state(),
            RecorderState::Recording | RecorderState::Stopping
        )
    }

    /// Whether every file reported open has been reported closed.
    fn all_closed(&self) -> bool {
        self.open_files.is_empty() && self.report.closed_files > 0
    }

    /// Record that a stop is under way, announced by the SDK after an
    /// unrecoverable error, or `requested` by the caller. A stop the caller
    /// requested stays requested.
    fn stopping(&mut self, requested: bool) {
        self.report.phase = SessionPhase::Stopping {
            requested: requested || self.report.phase.requested(),
        };
    }

    /// Record that the caller asked for the session to stop.
    ///
    /// `still_recording` is whether the SDK was still recording when asked:
    /// a request that comes after the SDK has ended the session by itself
    /// does not make the caller the one who stopped it.
    fn stop_requested(&mut self, still_recording: bool) {
        self.stopping(still_recording);
    }

    /// Record the SDK's cumulative count of discarded events.
    ///
    /// Like every event, a loss outside a session belongs to none: a start
    /// the SDK refuses queues events too, and they must not alter the report
    /// of the session before it.
    fn dropped(&mut self, cumulative: u64) {
        if !self.session_open() {
            return;
        }
        self.report.dropped_events = cumulative.saturating_sub(self.dropped_base);
        if cumulative != self.dropped_base {
            self.uncertain = true;
        }
    }

    /// Record one event of the open session.
    fn observe(&mut self, kind: RecorderEventKind, path: EventText) {
        if !self.session_open() {
            return;
        }
        // A path that is cut short cannot be matched with a later event.
        let exact =
            !path.as_bytes().is_empty() && path.is_terminated() && !path.is_possibly_truncated();
        match kind {
            RecorderEventKind::Started | RecorderEventKind::Rollover => {
                let first = kind == RecorderEventKind::Started;
                if !exact
                    || self.open_files.iter().any(|open| open.path == path)
                    || (first && !self.open_files.is_empty())
                {
                    self.uncertain = true;
                }
                self.open_files.push(OpenFile {
                    path,
                    has_media: false,
                });
            }
            RecorderEventKind::FirstFrame => {
                if let Some(open) = self.open_files.iter_mut().find(|open| open.path == path) {
                    open.has_media = true;
                }
            }
            RecorderEventKind::FileClosed => {
                match self.open_files.iter().position(|open| open.path == path) {
                    Some(index) if exact => {
                        let closed = self.open_files.remove(index);
                        if self.records_media && !closed.has_media {
                            self.report.empty_files = self.report.empty_files.saturating_add(1);
                        }
                    }
                    _ => self.uncertain = true,
                }
                self.report.closed_files = self.report.closed_files.saturating_add(1);
                self.report.last_closed_path = Some(path);
            }
            RecorderEventKind::Error => {
                self.report.error_events = self.report.error_events.saturating_add(1);
                // The SDK stops the session itself after this event.
                if self.session_open() {
                    self.stopping(false);
                }
            }
            RecorderEventKind::OpenFailed => {
                self.report.error_events = self.report.error_events.saturating_add(1);
            }
            RecorderEventKind::WriteFailed => {
                self.report.write_failures = self.report.write_failures.saturating_add(1);
            }
            _ => {}
        }
    }

    /// Record that the event queue is empty, and whether the SDK is still
    /// recording.
    ///
    /// Once the SDK has stopped and has nothing more to report, the session's
    /// outcome is final: confirmed when every opened file was closed, and
    /// indeterminate otherwise. A stop that is still being carried out leaves
    /// a file open, and the session keeps waiting for its closure.
    /// Finality here relies on rollovers reporting the next file open before
    /// the previous file closes: an empty ledger then cannot hide a pending
    /// open. The SDK headers do not specify the order; the native test
    /// `rollovers_open_the_next_file_before_closing_the_previous_file` pins it.
    fn drained(&mut self, native_recording: bool) {
        if native_recording || !self.session_open() {
            return;
        }
        let requested = self.report.phase.requested();
        if self.uncertain {
            self.report.phase = SessionPhase::Indeterminate { requested };
        } else if self.all_closed() {
            self.report.phase = SessionPhase::Closed { requested };
        } else if self.state() == RecorderState::Stopping && self.open_files.is_empty() {
            // The session stopped without ever reporting a file.
            self.report.phase = SessionPhase::Indeterminate { requested };
        }
    }
}

// Every string and structure passed to the File library is kept only for the
// call that receives it. The SDK headers give a lifetime for each pointer the
// library returns and none for any it is given, and say of the source strings
// the recorder reports that the recorder owns them: what it is given, it
// copies. `sdk_tests` holds the library to that, by overwriting each string as
// soon as the call it was passed to returns.

fn raw_source(source: &SourceSpec) -> NDIlib_file_recorder_source_t {
    let pointer =
        |text: &Option<CString>| text.as_ref().map_or(std::ptr::null(), |text| text.as_ptr());
    NDIlib_file_recorder_source_t {
        p_ndi_name: pointer(&source.name),
        p_url_address: pointer(&source.url),
    }
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
/// [`FinishStatus::Closed`]. Dropping a recorder with a session still open
/// does the same, waiting up to [`RecorderOptions::drop_wait`], but cannot
/// report whether it succeeded; [`abandon`](Self::abandon) skips the wait.
///
/// # What a closed file means
///
/// A report of closure repeats what the SDK said: that it finalized the file.
/// It does not show that every frame reached the disk, and a file can be
/// closed with nothing in it ([`RecordingReport::empty_files`]). With the NDI
/// 6.4 File library a disk that fills while a session is recording ends the
/// process, and one that fills while it is being stopped leaves a file that
/// is reported closed without its data. See
/// [the module's limits](super#limits-of-the-ndi-64-file-library).
///
/// # Events
///
/// Events are taken with [`poll_event`](Self::poll_event), and
/// [`finish`](Self::finish) takes them itself while it waits; their effect on
/// the session is kept in the [`report`](Self::report) either way. The SDK
/// discards events that are not taken promptly, and a session that lost
/// events can no longer be confirmed closed, so poll throughout a recording.
///
/// A recorder belongs to the thread that created it, so it is neither
/// [`Send`] nor [`Sync`]: the File API documents no thread on which a
/// recorder may be used or destroyed, nor whether two calls on one recorder
/// may overlap. Every method calls into the SDK and may block.
/// [`AsyncFileRecorder`](super::AsyncFileRecorder) offers the same operations
/// as futures.
pub struct FileRecorder {
    recording: &'static sys::FileRecording,
    handle: NonNull<NDIlib_file_recorder_instance_type>,
    /// Whether audio or video is recorded, rather than metadata alone.
    records_media: bool,
    /// The source in effect, when the last change of source was accepted.
    accepted_source: Option<SourceSpec>,
    /// How the recorder names its files.
    naming: NamePattern,
    ledger: Ledger,
    /// The SDK has not yet been told to stop the session started last. It
    /// needs to be, even when it ended that session itself: until then it
    /// refuses to start another.
    native_stop_owed: bool,
    drop_wait: Duration,
    /// An async shutdown's existing budget, shared with a running finish.
    drop_deadline: Option<Deadline>,
    // Keeps the NDI runtime initialized.
    _api: FileApi,
    // Neither the NDI File Format API documentation nor
    // Processing.NDI.Lib.File.Recorder.h says on which threads a recorder may
    // be created, used and destroyed, or whether calls on one recorder may
    // overlap; the header mentions concurrent recorders only for the memory
    // each one takes. Without that contract a recorder stays on its creating
    // thread, and AsyncFileRecorder creates, uses and destroys one on a
    // thread of its own.
    _thread: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for FileRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileRecorder")
            .field("state", &self.ledger.state())
            .field("session", &self.ledger.report.session)
            .finish_non_exhaustive()
    }
}

impl FileRecorder {
    pub(super) fn create(api: FileApi, options: &RecorderOptions) -> Result<Self> {
        let recording = api.table::<sys::FileRecording>()?;
        let resolved = options.resolve()?;
        let settings = NDIlib_file_recorder_create_t {
            source_to_connect_to: raw_source(&options.source),
            track_flags: track_flags(options.tracks),
            record_float_audio_planar_32f: options.planar_audio,
            disable_auto_rollover_on_format_change: !resolved.format_change_rollover,
            p_output_filename_pattern: resolved
                .pattern
                .as_ref()
                .map_or(std::ptr::null(), |pattern| pattern.as_ptr()),
            disable_proxy_track: !options.proxy,
            rollover_max_file_size_bytes: options.rollover_size,
        };
        // SAFETY: `settings` and the strings it points to, owned by `options`
        // and `resolved`, are valid for the call.
        let handle = NonNull::new(unsafe { recording.NDIlib_file_recorder_create(&settings) })
            .ok_or(Error::NativeOperationFailed {
                operation: "recorder creation",
            })?;
        Ok(Self {
            recording,
            handle,
            records_media: options.tracks.contains(MediaTypes::VIDEO)
                || options.tracks.contains(MediaTypes::AUDIO),
            drop_wait: options.drop_wait,
            drop_deadline: None,
            accepted_source: Some(options.source.clone()),
            naming: resolved.naming,
            ledger: Ledger::idle(),
            native_stop_owed: false,
            _api: api,
            _thread: PhantomData,
        })
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
        self.ledger.report.clone()
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
    /// The target is only read, so it stays with the caller whatever the
    /// outcome, for example to start again once a missing directory exists.
    ///
    /// # Errors
    ///
    /// * [`Error::InvalidState`] with [`StateError::PreviousSessionOpen`]
    ///   while an earlier session is recording or stopping;
    ///   [`finish`](Self::finish) it first. With
    ///   [`StateError::EarlierSessionEvents`] when the SDK keeps reporting
    ///   events of an earlier session.
    /// * [`Error::Io`] when the directory of the file the pattern names
    ///   does not exist or cannot be written to, or, with an exact file name,
    ///   with kind [`AlreadyExists`](std::io::ErrorKind::AlreadyExists) when
    ///   that file exists. The SDK is not called.
    /// * [`Error::NativeOperationFailed`] when the SDK cannot start the source or open
    ///   the first file. It queues an open-failed event that says why, which
    ///   [`poll_event`](Self::poll_event) returns; the event is counted in no
    ///   session's report.
    pub fn start(&mut self, target: &RecordingTarget, timecode: RecordingTimecode) -> Result<u64> {
        if self.ledger.session_open() {
            return Err(Error::InvalidState(StateError::PreviousSessionOpen));
        }
        self.naming.check(target)?;
        // Events left over from an earlier session must not be counted in this one.
        self.discard_stale_events()?;
        let session = self
            .ledger
            .report
            .session
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("the session counter is exhausted".into()))?;
        let dropped = self.dropped_event_count();
        let prefix = target.prefix.as_ptr();
        // SAFETY: the recorder is live, and `prefix` points into `target`,
        // which the caller lends for longer than the call.
        let accepted = unsafe {
            match timecode {
                RecordingTimecode::Preserve => self
                    .recording
                    .NDIlib_file_recorder_start(self.handle.as_ptr(), prefix),
                RecordingTimecode::GenerateFrom(start) => {
                    self.recording.NDIlib_file_recorder_start_with_timecode(
                        self.handle.as_ptr(),
                        prefix,
                        start.ticks(),
                    )
                }
            }
        };
        if !accepted {
            return Err(Error::NativeOperationFailed {
                operation: "recorder start",
            });
        }
        self.native_stop_owed = true;
        self.ledger = Ledger::session(session, dropped, self.records_media);
        Ok(session)
    }

    /// Take events until the queue is empty, so that none from an earlier
    /// session is attributed to the next one.
    fn discard_stale_events(&mut self) -> Result<()> {
        for _ in 0..STALE_EVENT_LIMIT {
            if self.capture(0)?.is_none() {
                return Ok(());
            }
        }
        Err(Error::InvalidState(StateError::EarlierSessionEvents))
    }

    /// Ask the SDK to stop the session. Idempotent.
    ///
    /// The SDK then finalizes the open file and reports it closed; this call
    /// alone does not show that it has. Use [`finish`](Self::finish) or
    /// [`wait_finished`](Self::wait_finished) to find out.
    pub fn request_stop(&mut self) -> StopRequest {
        let outcome = match self.state() {
            RecorderState::Idle => StopRequest::NotStarted,
            RecorderState::Closed => StopRequest::AlreadyClosed,
            RecorderState::Indeterminate => StopRequest::AlreadyEnded,
            RecorderState::Stopping => StopRequest::AlreadyStopping,
            RecorderState::Recording => {
                let still_recording = self.is_recording();
                self.ledger.stop_requested(still_recording);
                StopRequest::Requested
            }
        };
        // A session the SDK is ending by itself has not been told to stop.
        self.stop_native();
        outcome
    }

    /// Tell the SDK to stop, unless the session started last was already told.
    fn stop_native(&mut self) {
        if std::mem::take(&mut self.native_stop_owed) {
            // SAFETY: the recorder is live for this borrow.
            unsafe {
                self.recording
                    .NDIlib_file_recorder_stop(self.handle.as_ptr())
            };
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
    /// and [`Error::UnexpectedSdkResult`] when the SDK's event is malformed.
    pub fn poll_event(&mut self, timeout: Duration) -> Result<Option<RecorderEvent>> {
        self.poll_event_ms(crate::to_ms_checked(timeout)?)
    }

    /// [`poll_event`](Self::poll_event), waiting up to `wait_ms` milliseconds.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] when the SDK's event is malformed.
    fn poll_event_ms(&mut self, wait_ms: u32) -> Result<Option<RecorderEvent>> {
        let event = self.capture(wait_ms);
        self.ledger.dropped(self.dropped_event_count());
        let Some(event) = event.inspect_err(|_| self.ledger.uncertain = true)? else {
            // This empty-capture/recording check relies on open-before-close
            // rollover events, pinned by the native File test
            // `rollovers_open_the_next_file_before_closing_the_previous_file`.
            self.ledger.drained(self.is_recording());
            if !self.ledger.session_open() {
                // The session is over. When the SDK ended it itself, this is
                // the stop it still needs before it will start another.
                self.stop_native();
            }
            return Ok(None);
        };
        self.ledger.observe(
            RecorderEventKind::from_raw(event.type_),
            EventText::from_buffer(&event.p_path),
        );
        // Decoded after the ledger is updated, so that a malformed payload
        // cannot hide the event that closes a file.
        Ok(Some(RecorderEvent::from_sdk(&event)?))
    }

    /// Take the next native event, or `None` when the queue stays empty for
    /// `wait_ms` milliseconds.
    fn capture(&mut self, wait_ms: u32) -> Result<Option<NDIlib_file_recorder_event_t>> {
        let mut event = OutParam::<NDIlib_file_recorder_event_t>::new();
        // SAFETY: the recorder is live and `event` is writable storage for the
        // structure the SDK fills in.
        let kind = unsafe {
            self.recording.NDIlib_file_recorder_capture_event(
                self.handle.as_ptr(),
                event.as_mut_ptr(),
                wait_ms,
            )
        };
        let event = event.read()?;
        if kind == NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_none {
            return Ok(None);
        }
        if kind != event.type_ {
            return Err(Error::UnexpectedSdkResult(
                "the recorder returned one event type and stored another".into(),
            ));
        }
        Ok(Some(event))
    }

    /// Stop the session and wait up to `timeout` for its last file to be
    /// closed.
    ///
    /// Events that arrive during the wait are consumed; their effect is in
    /// the returned report. A malformed event is consumed too: what it showed
    /// about the session is recorded as [`poll_event`](Self::poll_event)
    /// records it, and the wait goes on. One that could not be read at all
    /// makes the outcome [`FinishStatus::Indeterminate`] rather than an error.
    ///
    /// The wait starts once the stop has returned, and covers taking the
    /// closure's event, not the stop itself, which the SDK may carry out
    /// before it returns. Events already queued are taken even when the wait
    /// is over, so `finish(Duration::ZERO)` reports [`FinishStatus::Closed`]
    /// when the SDK finalized the file during the stop.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`],
    /// without requesting the stop, and [`Error::InvalidState`] with
    /// [`StateError::StopNotRequested`] when no session was started. Neither
    /// is returned once the wait has begun: whatever the SDK's events hold,
    /// the outcome is a [`FinishStatus`].
    pub fn finish(&mut self, timeout: Duration) -> Result<FinishStatus> {
        // Checked here, so that a timeout out of range leaves the session
        // recording; the budget itself starts after the stop, in
        // `wait_finished`.
        crate::to_ms_checked(timeout)?;
        self.request_stop();
        self.wait_finished(timeout)
    }

    /// Wait up to `timeout` for a stopping session to end.
    ///
    /// Use this to keep waiting after [`FinishStatus::TimedOut`]. Events
    /// already queued are taken even when `timeout` is over, so a closure
    /// that has been reported is never missed. Malformed events are taken and
    /// recorded as [`finish`](Self::finish) describes, without ending the
    /// wait.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`],
    /// and [`Error::InvalidState`] with
    /// [`StateError::StopNotRequested`] when no session was started or the
    /// session has not been asked to stop. Neither is returned once the wait
    /// has begun.
    pub fn wait_finished(&mut self, timeout: Duration) -> Result<FinishStatus> {
        self.wait_finished_from(Deadline::after(timeout)?, |_| false)
    }

    /// Wait for a stopping session to end, until `deadline` is spent.
    ///
    /// `each` is called after every poll with what the poll returned,
    /// including a malformed event's error, and ends the wait early by
    /// returning true. A poll that fails has still taken an event, whose
    /// effect is in the ledger, so the wait goes on to the outcome the ledger
    /// then shows.
    ///
    /// Once the deadline has passed, events already queued are still taken,
    /// without waiting, until the queue is found empty: the deadline may have
    /// passed during the native stop, after the closure was reported.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidState`] with [`StateError::StopNotRequested`] when no
    /// session was started or the session has not been asked to stop.
    pub(super) fn wait_finished_from(
        &mut self,
        deadline: Deadline,
        each: impl FnMut(Result<Option<RecorderEvent>>) -> bool,
    ) -> Result<FinishStatus> {
        self.wait_finished_with(deadline, |_, deadline| deadline, each)
    }

    /// The same wait, with a budget the async shutdown path can narrow before
    /// each poll. Cancellation stays separate from shutdown and ends the wait
    /// after the current poll, whose event is still delivered by `each`.
    pub(super) fn wait_finished_with(
        &mut self,
        mut deadline: Deadline,
        mut budget: impl FnMut(&mut Self, Deadline) -> Deadline,
        mut each: impl FnMut(Result<Option<RecorderEvent>>) -> bool,
    ) -> Result<FinishStatus> {
        if matches!(self.state(), RecorderState::Idle | RecorderState::Recording) {
            return Err(Error::InvalidState(StateError::StopNotRequested));
        }
        let mut interrupted = false;
        let mut late_polls = 0;
        loop {
            deadline = budget(self, deadline);
            match self.state() {
                RecorderState::Closed => return Ok(FinishStatus::Closed(self.report())),
                RecorderState::Indeterminate => {
                    return Ok(FinishStatus::Indeterminate(self.report()))
                }
                _ => {}
            }
            if interrupted {
                return Ok(FinishStatus::TimedOut(self.report()));
            }
            // Read once, so that this poll and the check after it agree on
            // whether it was a late one.
            let expired = deadline.is_expired();
            if expired {
                if late_polls == STALE_EVENT_LIMIT {
                    return Ok(FinishStatus::TimedOut(self.report()));
                }
                late_polls += 1;
            }
            // With every file closed, one empty poll settles the outcome.
            let step = if self.ledger.all_closed() {
                SETTLE_POLL
            } else {
                FINISH_POLL
            };
            // A failed poll is an event that was taken but could not be
            // decoded, or could not be read at all. Either way its effect is
            // already in the ledger, which the loop goes on to settle.
            let polled =
                self.poll_event_ms(deadline.within_at(Instant::now(), step).remaining_ms());
            let drained = matches!(polled, Ok(None));
            interrupted = each(polled);
            if expired && drained {
                // The queue was empty and its state recorded: what it shows
                // now is the outcome.
                return Ok(match self.state() {
                    RecorderState::Closed => FinishStatus::Closed(self.report()),
                    RecorderState::Indeterminate => FinishStatus::Indeterminate(self.report()),
                    _ => FinishStatus::TimedOut(self.report()),
                });
            }
        }
    }

    /// Reuse the close's deadline for both an active finish and destruction.
    pub(super) fn limit_shutdown_wait(
        &mut self,
        deadline: Deadline,
        requested: Instant,
    ) -> Deadline {
        let deadline = deadline.within_at(requested, self.drop_wait);
        self.drop_deadline = Some(deadline);
        deadline
    }

    pub(super) fn bound_shutdown_wait(&mut self, requested: Instant) {
        if self.drop_deadline.is_none() {
            self.drop_deadline = Deadline::starting(requested, self.drop_wait).ok();
            if self.drop_deadline.is_none() {
                // An unrepresentable deadline cannot provide a bounded wait.
                self.drop_wait = Duration::ZERO;
            }
        }
    }

    pub(super) fn shutdown_wait_spent(&mut self) {
        self.drop_wait = Duration::ZERO;
    }

    /// Stop and destroy the recorder at once, without waiting for the SDK to
    /// finalize an open file, and return what was known about the session.
    ///
    /// The file of a session that had not ended may be left truncated.
    pub fn abandon(mut self) -> RecordingReport {
        self.drop_wait = Duration::ZERO;
        self.report()
    }

    /// Abandon this native recorder and replace it with a new idle recorder.
    ///
    /// Use this after a finalization timeout when the SDK stopped producing
    /// events and the old session cannot be confirmed closed. The returned
    /// report preserves that uncertainty; the old file may be truncated.
    /// The replacement has its own event queue, so delayed events from the old
    /// session cannot affect a new recording. The last report is retained and
    /// session numbers continue increasing across resets.
    ///
    /// The replacement is created first. If creation fails, this recorder and
    /// its report are kept and can still be polled or finalized. The options
    /// are only read, as by [`FileApi::create_recorder`].
    ///
    /// # Errors
    ///
    /// The errors of [`FileApi::create_recorder`].
    pub fn reset(&mut self, options: &RecorderOptions) -> Result<RecordingReport> {
        let mut replacement = Self::create(self._api.clone(), options)?;
        replacement.ledger = Ledger::inheriting(self.report());
        Ok(std::mem::replace(self, replacement).abandon())
    }

    /// Whether the SDK is recording or waiting for frames. False once a stop
    /// has taken effect; this alone does not show that a file was finalized.
    pub fn is_recording(&self) -> bool {
        // SAFETY: the recorder is live for this borrow.
        unsafe {
            self.recording
                .NDIlib_file_recorder_is_recording(self.handle.as_ptr())
        }
    }

    /// How many events the SDK has discarded since the recorder was created
    /// because they were not taken in time.
    pub fn dropped_event_count(&self) -> u64 {
        // SAFETY: the recorder is live for this borrow.
        unsafe {
            self.recording
                .NDIlib_file_recorder_get_dropped_event_count(self.handle.as_ptr())
        }
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
    /// * [`Error::InvalidState`] with [`StateError::ExactFileName`]
    ///   during a session, recording or stopping, of a recorder with an exact
    ///   file name, and with [`StateError::Stopping`] while any other
    ///   recorder's session is stopping. The SDK is not called.
    /// * [`Error::NativeOperationFailed`] when the SDK cannot start the source. The
    ///   SDK does not say which source is then in effect, so
    ///   [`last_accepted_source`](Self::last_accepted_source) returns `None`
    ///   until a later change succeeds.
    pub fn set_source(&mut self, source: &SourceSpec) -> Result<()> {
        if self.naming.is_exact() && self.ledger.session_open() {
            return Err(Error::InvalidState(StateError::ExactFileName));
        }
        if self.state() == RecorderState::Stopping {
            return Err(Error::InvalidState(StateError::Stopping));
        }
        let raw = raw_source(source);
        // SAFETY: the recorder is live, and `raw` and the strings it points
        // to, owned by `source`, are valid for the call.
        let accepted = unsafe {
            self.recording
                .NDIlib_file_recorder_set_source(self.handle.as_ptr(), &raw)
        };
        // Kept only once accepted: the copy is what the recorder retains.
        self.accepted_source = accepted.then(|| source.clone());
        if accepted {
            Ok(())
        } else {
            Err(Error::NativeOperationFailed {
                operation: "recorder source change",
            })
        }
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

    /// The file the recorder's events reported open last and not yet closed.
    ///
    /// The path is as the SDK gave it, and lags the recorder by the events not
    /// yet taken.
    pub fn reported_current_file(&self) -> Option<&EventText> {
        self.ledger.open_files.last().map(|open| &open.path)
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
    /// * [`Error::InvalidState`] with [`StateError::NotRecording`] unless
    ///   a session is recording.
    /// * [`Error::Io`] when the directory of the file the pattern names
    ///   does not exist or cannot be written to, or, with an exact file name,
    ///   with kind [`AlreadyExists`](std::io::ErrorKind::AlreadyExists) when
    ///   that file exists. The SDK is not called and the session goes on.
    /// * [`Error::NativeOperationFailed`] when the SDK refuses the target.
    pub fn rollover(&mut self, target: &RecordingTarget) -> Result<()> {
        if self.state() != RecorderState::Recording {
            return Err(Error::InvalidState(StateError::NotRecording));
        }
        self.naming.check(target)?;
        // SAFETY: the recorder is live, and the prefix points into `target`,
        // which the caller lends for longer than the call. The SDK copies it.
        let accepted = unsafe {
            self.recording
                .NDIlib_file_recorder_rollover(self.handle.as_ptr(), target.prefix.as_ptr())
        };
        if accepted {
            Ok(())
        } else {
            Err(Error::NativeOperationFailed {
                operation: "recorder rollover",
            })
        }
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
    /// [`Error::InvalidState`] with [`StateError::NotRecording`] unless a
    /// session is recording, and [`Error::NativeOperationFailed`] when the
    /// SDK refuses it, for example because the metadata track is not being
    /// recorded.
    pub fn inject_metadata<S: MetadataSource + ?Sized>(&mut self, metadata: &S) -> Result<()> {
        if self.state() != RecorderState::Recording {
            return Err(Error::InvalidState(StateError::NotRecording));
        }
        let frame = metadata.metadata_raw();
        // SAFETY: the recorder is live. `frame.p_data` points to the
        // NUL-terminated text that `metadata` owns, `frame.length` bytes with
        // the terminator, within the crate's metadata cap. `metadata` is
        // borrowed for the whole of this synchronous call, and
        // `Processing.NDI.Lib.File.Recorder.h` has the SDK take the frame as
        // `const` and copy the payload before returning, so it neither keeps
        // nor writes through `p_data`.
        let injected = unsafe {
            self.recording
                .NDIlib_file_recorder_inject_metadata(self.handle.as_ptr(), &frame)
        };
        if injected {
            Ok(())
        } else {
            Err(Error::NativeOperationFailed {
                operation: "metadata injection",
            })
        }
    }

    /// The recorder's running counters, when the SDK has any.
    /// Interpret these with the recorder session state and events; they do
    /// not use receiver drop-percentage semantics.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] when the SDK's structure does not match this
    /// crate's layout.
    pub fn stats(&self) -> Result<Option<RecorderStats>> {
        let mut stats = OutParam::<NDIlib_file_recorder_stats_t>::new();
        // SAFETY: the recorder is live and `stats` is writable storage for the
        // structure the SDK fills in.
        let available = unsafe {
            self.recording
                .NDIlib_file_recorder_get_stats(self.handle.as_ptr(), stats.as_mut_ptr())
        };
        let stats = stats.read()?;
        Ok(available.then(|| RecorderStats::from_sdk(&stats)))
    }

    /// The format of the source, once the recorder has seen audio or video.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] when the SDK's structure is malformed.
    pub fn source_format(&self) -> Result<Option<SourceFormat>> {
        let mut format = OutParam::<NDIlib_file_source_format_t>::new();
        // SAFETY: the recorder is live and `format` is writable storage for
        // the structure the SDK fills in.
        let known = unsafe {
            self.recording
                .NDIlib_file_recorder_get_source_format(self.handle.as_ptr(), format.as_mut_ptr())
        };
        let format = format.read()?;
        known.then(|| SourceFormat::from_sdk(&format)).transpose()
    }
}

impl Drop for FileRecorder {
    fn drop(&mut self) {
        // The SDK finalizes a file between the stop request and the
        // file-closed event; destroying sooner may truncate it.
        if self.ledger.session_open() {
            self.request_stop();
            if !self.drop_wait.is_zero() {
                // The outcome cannot be reported from here. The wait itself
                // cannot fail: the session is stopping and `drop_wait` was
                // checked when the recorder was created, and a malformed
                // event does not end it.
                if let Some(deadline) = self.drop_deadline {
                    let _ = self.wait_finished_from(deadline, |_| false);
                } else {
                    let _ = self.wait_finished(self.drop_wait);
                }
            }
        }
        // SAFETY: the recorder is live and is destroyed exactly once, on the
        // thread that created it.
        unsafe {
            self.recording
                .NDIlib_file_recorder_destroy(self.handle.as_ptr())
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> EventText {
        let mut buffer = [0 as std::ffi::c_char; 64];
        for (slot, byte) in buffer.iter_mut().zip(text.bytes()) {
            *slot = byte as std::ffi::c_char;
        }
        EventText::from_buffer(&buffer)
    }

    fn session() -> Ledger {
        Ledger::session(1, 0, true)
    }

    use RecorderEventKind::{
        Error, FileClosed, FormatChanged, OpenFailed, Rollover, Started, WriteFailed,
    };

    #[test]
    fn closure_needs_the_matching_file_and_a_stopped_drained_recorder() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.stop_requested(true);
        ledger.observe(FileClosed, path("take.mkv"));
        assert!(!ledger.report.closure_confirmed());
        ledger.drained(true);
        assert_eq!(ledger.state(), RecorderState::Stopping);
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report.closure_confirmed());
    }

    #[test]
    fn a_close_for_another_file_never_confirms_the_session() {
        let mut ledger = session();
        ledger.observe(Started, path("first.mkv"));
        ledger.stop_requested(true);
        ledger.observe(FileClosed, path("other.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert!(!ledger.report.closure_confirmed());
    }

    #[test]
    fn lost_events_taint_an_otherwise_matching_close() {
        let mut ledger = Ledger::session(1, 7, true);
        ledger.observe(Started, path("take.mkv"));
        ledger.stop_requested(true);
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.dropped(8);
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert_eq!(ledger.report.dropped_events, 1);
    }

    #[test]
    fn a_rollover_reports_the_next_file_open_before_the_previous_one_closed() {
        // Finality in `drained` and `poll_event_ms` relies on this observed
        // SDK order. The native File test
        // `rollovers_open_the_next_file_before_closing_the_previous_file`
        // pins it for explicit, source-switch and size rollovers, with and
        // without proxy recording.
        let mut ledger = session();
        ledger.observe(Started, path("first.mkv"));
        ledger.observe(Rollover, path("second.mkv"));
        assert_eq!(ledger.open_files.len(), 2);
        ledger.observe(FileClosed, path("first.mkv"));
        ledger.drained(true);
        assert_eq!(ledger.state(), RecorderState::Recording);
        ledger.stop_requested(true);
        ledger.observe(FileClosed, path("second.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert_eq!(ledger.report.closed_files, 2);
    }

    #[test]
    fn a_rollover_reported_after_the_previous_close_is_followed_too() {
        let mut ledger = session();
        ledger.observe(Started, path("first.mkv"));
        ledger.observe(FileClosed, path("first.mkv"));
        // Still recording: the close of the first file is not the end.
        ledger.drained(true);
        assert_eq!(ledger.state(), RecorderState::Recording);
        ledger.observe(Rollover, path("second.mkv"));
        ledger.stop_requested(true);
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Stopping);
        ledger.observe(FileClosed, path("second.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
    }

    #[test]
    fn a_rollover_that_never_happens_does_not_hold_the_session_open() {
        // A rollover was requested and the session stopped before the cut:
        // the SDK reports only the close of the file that was open.
        let mut ledger = session();
        ledger.observe(Started, path("first.mkv"));
        ledger.stop_requested(true);
        ledger.observe(FileClosed, path("first.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
    }

    #[test]
    fn a_stop_the_sdk_decides_on_confirms_its_observed_close() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.observe(FormatChanged, path("take.mkv"));
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report.closure_confirmed());
    }

    #[test]
    fn a_fatal_error_closes_the_session_and_stays_in_the_report() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.observe(WriteFailed, path("take.mkv"));
        ledger.observe(Error, path("take.mkv"));
        assert_eq!(ledger.state(), RecorderState::Stopping);
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert_eq!(ledger.report.error_events, 1);
        assert_eq!(ledger.report.write_failures, 1);
    }

    #[test]
    fn a_failed_rollover_open_is_counted_without_opening_a_file() {
        let mut ledger = session();
        ledger.observe(Started, path("first.mkv"));
        ledger.observe(OpenFailed, path("second.mkv"));
        ledger.observe(FileClosed, path("first.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert_eq!(ledger.report.error_events, 1);
    }

    #[test]
    fn a_session_that_stops_without_reporting_a_file_is_indeterminate_not_pending() {
        let mut ledger = session();
        ledger.stop_requested(true);
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
    }

    #[test]
    fn events_outside_a_session_change_nothing() {
        // A start the SDK refuses queues an open-failed event while the
        // previous session's record is still the current one.
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.stop_requested(true);
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        ledger.observe(OpenFailed, path("next.mkv"));
        ledger.dropped(3);
        assert_eq!(ledger.report.error_events, 0);
        assert_eq!(ledger.report.dropped_events, 0);
        assert!(ledger.report.closure_confirmed());
    }

    #[test]
    fn a_session_stopped_on_request_was_not_stopped_by_the_sdk() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        assert!(!ledger.report.stopped_by_sdk(), "an open session");
        ledger.stop_requested(true);
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.drained(true);
        assert!(!ledger.report.stopped_by_sdk(), "still stopping");
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(!ledger.report.stopped_by_sdk());
    }

    #[test]
    fn an_sdk_error_that_starts_the_stop_is_not_a_request() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.observe(OpenFailed, path("next.mkv"));
        ledger.observe(Error, path(""));
        assert_eq!(ledger.state(), RecorderState::Stopping);
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report.closure_confirmed());
        assert!(ledger.report.stopped_by_sdk());
    }

    #[test]
    fn a_close_at_a_format_change_with_no_request_was_the_sdks() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.observe(FormatChanged, path("take.mkv"));
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report.stopped_by_sdk());
    }

    #[test]
    fn a_request_after_the_sdk_stopped_recording_leaves_the_stop_to_the_sdk() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        // The SDK had ended the session before the request reached it.
        ledger.stop_requested(false);
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report.stopped_by_sdk());
    }

    #[test]
    fn an_indeterminate_end_with_no_request_was_the_sdks() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.dropped(1);
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert!(ledger.report.stopped_by_sdk());
    }

    #[test]
    fn a_truncated_or_repeated_open_cannot_be_matched() {
        let mut ledger = session();
        ledger.observe(
            Started,
            EventText::from_buffer(&[b'a' as std::ffi::c_char; 8]),
        );
        assert!(ledger.uncertain);

        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.observe(Rollover, path("take.mkv"));
        assert!(ledger.uncertain);
    }

    /// The state and the report's two verdicts are read from one phase, so
    /// every phase gives one consistent combination.
    #[test]
    fn the_state_and_the_reports_verdicts_agree_in_every_phase() {
        use SessionPhase::*;
        for (phase, state, confirmed, by_sdk) in [
            (NotStarted, RecorderState::Idle, false, false),
            (Recording, RecorderState::Recording, false, false),
            (
                Stopping { requested: false },
                RecorderState::Stopping,
                false,
                false,
            ),
            (
                Stopping { requested: true },
                RecorderState::Stopping,
                false,
                false,
            ),
            (
                Closed { requested: false },
                RecorderState::Closed,
                true,
                true,
            ),
            (
                Closed { requested: true },
                RecorderState::Closed,
                true,
                false,
            ),
            (
                Indeterminate { requested: false },
                RecorderState::Indeterminate,
                false,
                true,
            ),
            (
                Indeterminate { requested: true },
                RecorderState::Indeterminate,
                false,
                false,
            ),
        ] {
            let ledger = Ledger::with_report(1, phase, 0, true);
            assert_eq!(ledger.state(), state, "{phase:?}");
            assert_eq!(ledger.report.closure_confirmed(), confirmed, "{phase:?}");
            assert_eq!(ledger.report.stopped_by_sdk(), by_sdk, "{phase:?}");
            // A replacement keeps the report but has no session of its own.
            let inherited = Ledger::inheriting(ledger.report.clone());
            assert_eq!(inherited.state(), RecorderState::Idle, "{phase:?}");
            assert!(!inherited.session_open(), "{phase:?}");
            assert_eq!(inherited.report, ledger.report, "{phase:?}");
        }
    }

    #[test]
    fn a_stop_requested_before_an_sdk_error_stays_requested() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.stop_requested(true);
        ledger.observe(Error, path("take.mkv"));
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(!ledger.report.stopped_by_sdk());
    }
}
