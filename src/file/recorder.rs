//! Recording an NDI source to disk, with explicit finalization.

use std::{
    ffi::CString,
    marker::PhantomData,
    ptr::NonNull,
    rc::Rc,
    time::{Duration, Instant},
};

use super::{
    out_param::OutParam, timeout_ms, EventText, FileApi, FileError, FileResult, RecorderEvent,
    RecorderEventKind, RecorderOptions, RecorderStats, RecordingTarget, RecordingTimecode,
    SourceFormat, SourceSpec,
};
use crate::sys::{self, bindings::*};

/// The longest single wait for an event while finalization is pending. Short
/// enough that a caller's interruption is noticed promptly.
const FINISH_POLL: Duration = Duration::from_millis(25);

/// The wait for a further event once every file is reported closed: long
/// enough not to spin while the SDK finishes stopping.
const SETTLE_POLL: Duration = Duration::from_millis(1);

/// How many stale events [`FileRecorder::start`] discards before giving up on
/// finding the queue empty.
const STALE_EVENT_LIMIT: usize = 256;

/// Where a recorder is in its session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug)]
pub struct RecordingReport {
    session: u64,
    closure_confirmed: bool,
    closed_files: u64,
    error_events: u64,
    write_failures: u64,
    dropped_events: u64,
    last_closed_path: Option<EventText>,
}

impl RecordingReport {
    /// The session's number, counting from one for each recorder.
    pub fn session(&self) -> u64 {
        self.session
    }
    /// Whether every file the session opened was reported closed.
    pub fn closure_confirmed(&self) -> bool {
        self.closure_confirmed
    }
    /// How many files were reported closed, including those closed by rollovers.
    pub fn closed_files(&self) -> u64 {
        self.closed_files
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
}

/// The outcome of waiting for a session to end.
///
/// The recorder stays usable whatever the outcome.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum FinishStatus {
    /// Every file the session opened is finalized. The report still records
    /// any errors during the session.
    Closed(RecordingReport),
    /// The wait ended first. Call [`FileRecorder::wait_finished`] to keep waiting.
    TimedOut(RecordingReport),
    /// The session ended, but its events do not show that every file was
    /// finalized. Waiting longer will not change this.
    Indeterminate(RecordingReport),
}

/// The record of one session, built from the recorder's events.
struct Ledger {
    state: RecorderState,
    report: RecordingReport,
    /// Files reported open and not yet reported closed. A rollover opens the
    /// next file before the previous one is reported closed, so two can be
    /// open at once.
    open_files: Vec<EventText>,
    /// An event was lost, malformed or out of place, so `open_files` may not
    /// be the truth.
    uncertain: bool,
    /// The SDK's count of discarded events when the session began.
    dropped_base: u64,
}

impl Ledger {
    fn idle() -> Self {
        Self::session(0, 0, RecorderState::Idle)
    }

    fn session(session: u64, dropped_base: u64, state: RecorderState) -> Self {
        Self {
            state,
            report: RecordingReport {
                session,
                closure_confirmed: false,
                closed_files: 0,
                error_events: 0,
                write_failures: 0,
                dropped_events: 0,
                last_closed_path: None,
            },
            open_files: Vec::new(),
            uncertain: false,
            dropped_base,
        }
    }

    /// Whether a session has started and not yet reached a final state.
    fn session_open(&self) -> bool {
        matches!(
            self.state,
            RecorderState::Recording | RecorderState::Stopping
        )
    }

    /// Whether every file reported open has been reported closed.
    fn all_closed(&self) -> bool {
        self.open_files.is_empty() && self.report.closed_files > 0
    }

    /// Record that a stop is under way: requested by the caller, or announced
    /// by the SDK after an unrecoverable error.
    fn stopping(&mut self) {
        self.state = RecorderState::Stopping;
    }

    /// Record the SDK's cumulative count of discarded events.
    fn dropped(&mut self, cumulative: u64) {
        self.report.dropped_events = cumulative.saturating_sub(self.dropped_base);
        if cumulative != self.dropped_base {
            self.uncertain = true;
        }
    }

    /// Record one event.
    fn observe(&mut self, kind: RecorderEventKind, path: EventText) {
        // A path that is cut short cannot be matched with a later event.
        let exact =
            !path.as_bytes().is_empty() && path.is_terminated() && !path.is_possibly_truncated();
        match kind {
            RecorderEventKind::Started | RecorderEventKind::Rollover => {
                let first = kind == RecorderEventKind::Started;
                if !exact
                    || self.open_files.contains(&path)
                    || (first && !self.open_files.is_empty())
                {
                    self.uncertain = true;
                }
                self.open_files.push(path);
            }
            RecorderEventKind::FileClosed => {
                match self.open_files.iter().position(|open| *open == path) {
                    Some(index) if exact => {
                        self.open_files.remove(index);
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
                    self.stopping();
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
    fn drained(&mut self, native_recording: bool) {
        if native_recording || !self.session_open() {
            return;
        }
        if self.uncertain {
            self.state = RecorderState::Indeterminate;
        } else if self.all_closed() {
            self.state = RecorderState::Closed;
            self.report.closure_confirmed = true;
        } else if self.state == RecorderState::Stopping && self.open_files.is_empty() {
            // The session stopped without ever reporting a file.
            self.state = RecorderState::Indeterminate;
        }
    }
}

/// Storage handed to the SDK that must outlive the call that passed it.
///
/// The SDK says only of injected metadata that it is copied before the call
/// returns. Everything else is kept until it can no longer be referenced: the
/// creation settings and their strings for the life of the recorder, and what
/// a session was given until a later session has started.
struct Retained {
    _options: RecorderOptions,
    _create: Box<NDIlib_file_recorder_create_t>,
    prefixes: Vec<CString>,
    sources: Vec<(SourceSpec, Box<NDIlib_file_recorder_source_t>)>,
    /// The index in `sources` of the one the SDK accepted last, if any.
    source_in_effect: Option<usize>,
}

impl Retained {
    /// Release what only sessions before the one that just started could use:
    /// every prefix but the newest, and every source older than the one in effect.
    fn session_started(&mut self) {
        let newest = self.prefixes.len().saturating_sub(1);
        self.prefixes.drain(..newest);
        let in_effect = self.source_in_effect.unwrap_or(0);
        self.sources.drain(..in_effect);
        self.source_in_effect = self.source_in_effect.map(|_| 0);
    }
}

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
/// does the same, waiting up to [`RecorderOptions::with_drop_wait`], but cannot
/// report whether it succeeded; [`abandon`](Self::abandon) skips the wait.
///
/// # Events
///
/// Events are taken with [`poll_event`](Self::poll_event), and
/// [`finish`](Self::finish) takes them itself while it waits; their effect on
/// the session is kept in the [`report`](Self::report) either way. The SDK
/// discards events that are not taken promptly, and a session that lost
/// events can no longer be confirmed closed, so poll throughout a recording.
///
/// A recorder belongs to the thread that created it. Every method calls into
/// the SDK and may block. [`AsyncFileRecorder`](super::AsyncFileRecorder)
/// offers the same operations as futures.
pub struct FileRecorder {
    recording: &'static sys::FileRecording,
    handle: NonNull<NDIlib_file_recorder_instance_type>,
    retained: Retained,
    /// The source in effect, when the last change of source was accepted.
    accepted_source: Option<SourceSpec>,
    ledger: Ledger,
    drop_wait: Duration,
    // Keeps the NDI runtime initialized.
    _api: FileApi,
    _thread: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for FileRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileRecorder")
            .field("state", &self.ledger.state)
            .field("session", &self.ledger.report.session)
            .finish_non_exhaustive()
    }
}

impl FileRecorder {
    pub(super) fn create(api: FileApi, options: RecorderOptions) -> FileResult<Self> {
        let recording = api.recording()?;
        let create = Box::new(NDIlib_file_recorder_create_t {
            source_to_connect_to: raw_source(&options.source),
            track_flags: options.tracks.bits(),
            record_float_audio_planar_32f: options.planar_audio,
            disable_auto_rollover_on_format_change: options.disable_auto_rollover,
            p_output_filename_pattern: options
                .pattern
                .as_ref()
                .map_or(std::ptr::null(), |pattern| pattern.as_ptr()),
            disable_proxy_track: options.disable_proxy,
            rollover_max_file_size_bytes: options.rollover_bytes,
        });
        // SAFETY: `create` and the strings it points to, owned by `options`,
        // are valid for the call and are kept for the life of the recorder.
        let handle = NonNull::new(unsafe { recording.NDIlib_file_recorder_create(&*create) })
            .ok_or(FileError::Rejected("recorder creation"))?;
        Ok(Self {
            recording,
            handle,
            accepted_source: Some(options.source.clone()),
            drop_wait: options.drop_wait,
            retained: Retained {
                _options: options,
                _create: create,
                prefixes: Vec::new(),
                sources: Vec::new(),
                source_in_effect: None,
            },
            ledger: Ledger::idle(),
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
        self.ledger.state
    }

    /// What the events taken so far have shown about the current session.
    pub fn report(&self) -> RecordingReport {
        self.ledger.report.clone()
    }

    /// Start a session writing to `target`, and return its number.
    ///
    /// The recorder's filename pattern turns the target into file names. With
    /// the default indexed pattern the lowest unused index is chosen; with an
    /// exact filename the SDK refuses a file that already exists. Nothing is
    /// overwritten or removed, and the parent directory must exist.
    ///
    /// # Errors
    ///
    /// * [`FileError::InvalidState`] while an earlier session is recording or
    ///   stopping; [`finish`](Self::finish) it first.
    /// * [`FileError::Rejected`] when the SDK cannot start the source or open
    ///   the first file.
    pub fn start(
        &mut self,
        target: RecordingTarget,
        timecode: RecordingTimecode,
    ) -> FileResult<u64> {
        if self.ledger.session_open() {
            return Err(FileError::InvalidState(
                "finish the previous session before starting another",
            ));
        }
        // Events left over from an earlier session must not be counted in this one.
        self.discard_stale_events()?;
        let session =
            self.ledger.report.session.checked_add(1).ok_or_else(|| {
                FileError::InvalidInput("the session counter is exhausted".into())
            })?;
        let dropped = self.dropped_event_count();
        let prefix = target.prefix.as_ptr();
        self.retained.prefixes.push(target.prefix);
        // SAFETY: the recorder is live, and `prefix` points into a string the
        // recorder keeps until a later session has started.
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
            return Err(FileError::Rejected("recorder start"));
        }
        self.retained.session_started();
        self.ledger = Ledger::session(session, dropped, RecorderState::Recording);
        Ok(session)
    }

    /// Take events until the queue is empty, so that none from an earlier
    /// session is attributed to the next one.
    fn discard_stale_events(&mut self) -> FileResult<()> {
        for _ in 0..STALE_EVENT_LIMIT {
            if self.capture(0)?.is_none() {
                return Ok(());
            }
        }
        Err(FileError::InvalidState(
            "the recorder keeps producing events from an earlier session",
        ))
    }

    /// Ask the SDK to stop the session. Idempotent.
    ///
    /// The SDK then finalizes the open file and reports it closed; this call
    /// alone does not show that it has. Use [`finish`](Self::finish) or
    /// [`wait_finished`](Self::wait_finished) to find out.
    pub fn request_stop(&mut self) -> StopRequest {
        match self.state() {
            RecorderState::Idle => return StopRequest::NotStarted,
            RecorderState::Closed => return StopRequest::AlreadyClosed,
            RecorderState::Indeterminate => return StopRequest::AlreadyEnded,
            RecorderState::Stopping => return StopRequest::AlreadyStopping,
            RecorderState::Recording => {}
        }
        self.ledger.stopping();
        // SAFETY: the recorder is live for this borrow.
        unsafe {
            self.recording
                .NDIlib_file_recorder_stop(self.handle.as_ptr())
        };
        StopRequest::Requested
    }

    /// Take the next queued event, waiting up to `wait` for one.
    ///
    /// The event's effect on the session is recorded before it is returned,
    /// including when the event itself is malformed.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when `wait` is `u32::MAX` milliseconds or
    /// more, and [`FileError::Protocol`] when the SDK's event is malformed.
    pub fn poll_event(&mut self, wait: Duration) -> FileResult<Option<RecorderEvent>> {
        let wait = timeout_ms(wait)?;
        let event = self.capture(wait);
        self.ledger.dropped(self.dropped_event_count());
        let Some(event) = event.inspect_err(|_| self.ledger.uncertain = true)? else {
            self.ledger.drained(self.is_recording());
            return Ok(None);
        };
        self.ledger.observe(
            RecorderEventKind::from_raw(event.type_),
            EventText::from_buffer(&event.p_path),
        );
        // Decoded after the ledger is updated, so that a malformed payload
        // cannot hide the event that closes a file.
        Ok(Some(RecorderEvent::try_from(&event)?))
    }

    /// Take the next native event, or `None` when the queue stays empty for
    /// `wait_ms` milliseconds.
    fn capture(&mut self, wait_ms: u32) -> FileResult<Option<NDIlib_file_recorder_event_t>> {
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
            return Err(FileError::Protocol(
                "the recorder returned one event type and stored another".into(),
            ));
        }
        Ok(Some(event))
    }

    /// Stop the session and wait up to `wait` for its last file to be closed.
    ///
    /// Events that arrive during the wait are consumed; their effect is in
    /// the returned report.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidState`] when no session was started, and the
    /// errors of [`poll_event`](Self::poll_event).
    pub fn finish(&mut self, wait: Duration) -> FileResult<FinishStatus> {
        timeout_ms(wait)?;
        self.request_stop();
        self.wait_finished(wait)
    }

    /// Wait up to `wait` for a stopping session to end.
    ///
    /// Use this to keep waiting after [`FinishStatus::TimedOut`].
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidState`] when no session was started or the session
    /// has not been asked to stop, and the errors of
    /// [`poll_event`](Self::poll_event).
    pub fn wait_finished(&mut self, wait: Duration) -> FileResult<FinishStatus> {
        self.wait_finished_from(Instant::now(), wait, |_| false)
    }

    /// Wait until `wait` after `started` for a stopping session to end.
    ///
    /// `each` is called after every poll with the event it returned, if any,
    /// and ends the wait early by returning true.
    pub(super) fn wait_finished_from(
        &mut self,
        started: Instant,
        wait: Duration,
        mut each: impl FnMut(Option<RecorderEvent>) -> bool,
    ) -> FileResult<FinishStatus> {
        timeout_ms(wait)?;
        if matches!(self.state(), RecorderState::Idle | RecorderState::Recording) {
            return Err(FileError::InvalidState(
                "request a stop before waiting for the session to end",
            ));
        }
        let mut interrupted = false;
        loop {
            match self.state() {
                RecorderState::Closed => return Ok(FinishStatus::Closed(self.report())),
                RecorderState::Indeterminate => {
                    return Ok(FinishStatus::Indeterminate(self.report()))
                }
                _ => {}
            }
            let remaining = wait.saturating_sub(started.elapsed());
            if interrupted || remaining.is_zero() {
                return Ok(FinishStatus::TimedOut(self.report()));
            }
            // With every file closed, one empty poll settles the outcome.
            let budget = remaining.min(if self.ledger.all_closed() {
                SETTLE_POLL
            } else {
                FINISH_POLL
            });
            let event = self.poll_event(budget)?;
            interrupted = each(event);
        }
    }

    /// Stop and destroy the recorder at once, without waiting for the SDK to
    /// finalize an open file, and return what was known about the session.
    ///
    /// The file of a session that had not ended may be left truncated.
    pub fn abandon(mut self) -> RecordingReport {
        self.drop_wait = Duration::ZERO;
        self.report()
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
    /// to a new file. Before a session, the source is used by the next
    /// [`start`](Self::start).
    ///
    /// # Errors
    ///
    /// * [`FileError::InvalidState`] while a session is stopping or ended
    ///   without a confirmed closure.
    /// * [`FileError::Rejected`] when the SDK cannot start the source. The
    ///   SDK does not say which source is then in effect, so
    ///   [`last_accepted_source`](Self::last_accepted_source) returns `None`
    ///   until a later change succeeds.
    pub fn set_source(&mut self, source: SourceSpec) -> FileResult<()> {
        if matches!(
            self.state(),
            RecorderState::Stopping | RecorderState::Indeterminate
        ) {
            return Err(FileError::InvalidState(
                "the session is stopping, or ended without a confirmed closure",
            ));
        }
        let raw = Box::new(raw_source(&source));
        // SAFETY: the recorder is live, and `raw` and the strings it points to
        // are kept until a later session has started.
        let accepted = unsafe {
            self.recording
                .NDIlib_file_recorder_set_source(self.handle.as_ptr(), &*raw)
        };
        self.retained.sources.push((source, raw));
        if accepted {
            let index = self.retained.sources.len() - 1;
            self.retained.source_in_effect = Some(index);
            self.accepted_source = Some(self.retained.sources[index].0.clone());
            Ok(())
        } else {
            self.accepted_source = None;
            Err(FileError::Rejected("recorder source change"))
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
        self.ledger.open_files.last()
    }

    /// Close the current file and continue the session in a new one named
    /// from `target`.
    ///
    /// For compressed video the SDK makes the cut at the next keyframe, so the
    /// new file starts decodable. A rollover that has not happened by the time
    /// the session stops is dropped.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidState`] unless a session is recording, and
    /// [`FileError::Rejected`] when the SDK refuses the target.
    pub fn rollover(&mut self, target: RecordingTarget) -> FileResult<()> {
        if self.state() != RecorderState::Recording {
            return Err(FileError::InvalidState(
                "a rollover needs a session that is recording",
            ));
        }
        let prefix = target.prefix.as_ptr();
        self.retained.prefixes.push(target.prefix);
        // SAFETY: the recorder is live, and `prefix` points into a string the
        // recorder keeps until a later session has started.
        let accepted = unsafe {
            self.recording
                .NDIlib_file_recorder_rollover(self.handle.as_ptr(), prefix)
        };
        if accepted {
            Ok(())
        } else {
            Err(FileError::Rejected("recorder rollover"))
        }
    }

    /// Write a metadata frame into the recording at its current position.
    ///
    /// The frame's timecode is stored and re-published on playback; it does
    /// not move the frame on the file's timeline.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidState`] unless a session is recording,
    /// [`FileError::InvalidInput`] when the metadata contains a NUL or is too
    /// long, and [`FileError::Rejected`] when the SDK refuses it, for example
    /// because the metadata track is not being recorded.
    pub fn inject_metadata(&mut self, metadata: &crate::MetadataFrame) -> FileResult<()> {
        if self.state() != RecorderState::Recording {
            return Err(FileError::InvalidState(
                "metadata can only be injected into a session that is recording",
            ));
        }
        let data = CString::new(metadata.data())
            .map_err(|_| FileError::InvalidInput("the metadata contains a NUL".into()))?;
        let length = i32::try_from(data.as_bytes_with_nul().len()).map_err(|_| {
            FileError::InvalidInput("the metadata is longer than the SDK can accept".into())
        })?;
        let frame = NDIlib_metadata_frame_t {
            length,
            timecode: metadata.timecode(),
            p_data: data.as_ptr().cast_mut(),
        };
        // SAFETY: the recorder is live. The SDK copies the frame before it
        // returns and does not write through `p_data`.
        let injected = unsafe {
            self.recording
                .NDIlib_file_recorder_inject_metadata(self.handle.as_ptr(), &frame)
        };
        if injected {
            Ok(())
        } else {
            Err(FileError::Rejected("metadata injection"))
        }
    }

    /// The recorder's running counters, when the SDK has any.
    ///
    /// # Errors
    ///
    /// [`FileError::Protocol`] when the SDK's structure does not match this
    /// crate's layout.
    pub fn stats(&self) -> FileResult<Option<RecorderStats>> {
        let mut stats = OutParam::<NDIlib_file_recorder_stats_t>::new();
        // SAFETY: the recorder is live and `stats` is writable storage for the
        // structure the SDK fills in.
        let available = unsafe {
            self.recording
                .NDIlib_file_recorder_get_stats(self.handle.as_ptr(), stats.as_mut_ptr())
        };
        let stats = stats.read()?;
        Ok(available.then(|| RecorderStats::from(&stats)))
    }

    /// The format of the source, once the recorder has seen audio or video.
    ///
    /// # Errors
    ///
    /// [`FileError::Protocol`] when the SDK's structure is malformed.
    pub fn source_format(&self) -> FileResult<Option<SourceFormat>> {
        let mut format = OutParam::<NDIlib_file_source_format_t>::new();
        // SAFETY: the recorder is live and `format` is writable storage for
        // the structure the SDK fills in.
        let known = unsafe {
            self.recording
                .NDIlib_file_recorder_get_source_format(self.handle.as_ptr(), format.as_mut_ptr())
        };
        let format = format.read()?;
        known.then(|| SourceFormat::try_from(&format)).transpose()
    }
}

#[cfg(test)]
impl FileRecorder {
    /// How many recording prefixes and sources are still kept for the SDK.
    pub(super) fn retained_inputs(&self) -> (usize, usize) {
        (self.retained.prefixes.len(), self.retained.sources.len())
    }
}

impl Drop for FileRecorder {
    fn drop(&mut self) {
        // The SDK finalizes a file between the stop request and the
        // file-closed event; destroying sooner may truncate it.
        if self.ledger.session_open() {
            self.request_stop();
            if !self.drop_wait.is_zero() {
                // The outcome cannot be reported from here. An error ends the wait.
                let _ = self.wait_finished(self.drop_wait);
            }
        }
        // SAFETY: the recorder is live and is destroyed exactly once, on the
        // thread that created it. The storage in `retained` and `options` is
        // released only after this call returns.
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
        Ledger::session(1, 0, RecorderState::Recording)
    }

    use RecorderEventKind::{
        Error, FileClosed, FormatChanged, OpenFailed, Rollover, Started, WriteFailed,
    };

    #[test]
    fn closure_needs_the_matching_file_and_a_stopped_drained_recorder() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.stopping();
        ledger.observe(FileClosed, path("take.mkv"));
        assert!(!ledger.report.closure_confirmed);
        ledger.drained(true);
        assert_eq!(ledger.state, RecorderState::Stopping);
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Closed);
        assert!(ledger.report.closure_confirmed);
    }

    #[test]
    fn a_close_for_another_file_never_confirms_the_session() {
        let mut ledger = session();
        ledger.observe(Started, path("first.mkv"));
        ledger.stopping();
        ledger.observe(FileClosed, path("other.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Indeterminate);
        assert!(!ledger.report.closure_confirmed);
    }

    #[test]
    fn lost_events_taint_an_otherwise_matching_close() {
        let mut ledger = Ledger::session(1, 7, RecorderState::Recording);
        ledger.observe(Started, path("take.mkv"));
        ledger.stopping();
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.dropped(8);
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Indeterminate);
        assert_eq!(ledger.report.dropped_events, 1);
    }

    #[test]
    fn a_rollover_reports_the_next_file_open_before_the_previous_one_closed() {
        // The order the SDK emits: the new file's rollover, then the old
        // file's close.
        let mut ledger = session();
        ledger.observe(Started, path("first.mkv"));
        ledger.observe(Rollover, path("second.mkv"));
        assert_eq!(ledger.open_files.len(), 2);
        ledger.observe(FileClosed, path("first.mkv"));
        ledger.drained(true);
        assert_eq!(ledger.state, RecorderState::Recording);
        ledger.stopping();
        ledger.observe(FileClosed, path("second.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Closed);
        assert_eq!(ledger.report.closed_files, 2);
    }

    #[test]
    fn a_rollover_reported_after_the_previous_close_is_followed_too() {
        let mut ledger = session();
        ledger.observe(Started, path("first.mkv"));
        ledger.observe(FileClosed, path("first.mkv"));
        // Still recording: the close of the first file is not the end.
        ledger.drained(true);
        assert_eq!(ledger.state, RecorderState::Recording);
        ledger.observe(Rollover, path("second.mkv"));
        ledger.stopping();
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Stopping);
        ledger.observe(FileClosed, path("second.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Closed);
    }

    #[test]
    fn a_rollover_that_never_happens_does_not_hold_the_session_open() {
        // A rollover was requested and the session stopped before the cut:
        // the SDK reports only the close of the file that was open.
        let mut ledger = session();
        ledger.observe(Started, path("first.mkv"));
        ledger.stopping();
        ledger.observe(FileClosed, path("first.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Closed);
    }

    #[test]
    fn a_stop_the_sdk_decides_on_confirms_its_observed_close() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.observe(FormatChanged, path("take.mkv"));
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Closed);
        assert!(ledger.report.closure_confirmed);
    }

    #[test]
    fn a_fatal_error_closes_the_session_and_stays_in_the_report() {
        let mut ledger = session();
        ledger.observe(Started, path("take.mkv"));
        ledger.observe(WriteFailed, path("take.mkv"));
        ledger.observe(Error, path("take.mkv"));
        assert_eq!(ledger.state, RecorderState::Stopping);
        ledger.observe(FileClosed, path("take.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Closed);
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
        assert_eq!(ledger.state, RecorderState::Closed);
        assert_eq!(ledger.report.error_events, 1);
    }

    #[test]
    fn a_session_that_stops_without_reporting_a_file_is_indeterminate_not_pending() {
        let mut ledger = session();
        ledger.stopping();
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Indeterminate);
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
}
