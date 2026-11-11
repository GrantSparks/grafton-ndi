use super::{
    ffi, timeout_ms, EventText, FileApi, FileError, FileResult, RecorderEvent, RecorderEventKind,
    RecorderOptions, RecorderStats, RecordingTarget, RecordingTimecode, SourceFormat, SourceSpec,
};
use std::{
    ffi::{c_void, CString},
    marker::PhantomData,
    ptr::NonNull,
    rc::Rc,
    time::{Duration, Instant},
};

/// The wrapper's recording/finalization state, independent of source connectivity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecorderState {
    /// No recording session has been accepted.
    Idle,
    /// Recording is starting, writing, or waiting for a disconnected source.
    Recording,
    /// Stop was requested; final close has not been confirmed.
    Stopping,
    /// The final native close was observed without missing lifecycle evidence.
    Closed,
    /// Events were lost or inconsistent, so finalization cannot be certified.
    Indeterminate,
}

/// Result of an idempotent request to stop recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StopRequest {
    /// This call submitted the stop request.
    Requested,
    /// A stop request was already submitted.
    AlreadyStopping,
    /// No recording has been started.
    NotStarted,
    /// Closure was already confirmed.
    AlreadyClosed,
}

/// Observed session outcome. Confirmed closure does not imply lossless capture or fsync durability.
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
    /// Monotonic Rust session identifier for this recorder.
    pub fn session(&self) -> u64 {
        self.session
    }
    /// Whether final container closure was confirmed by native events.
    pub fn closure_confirmed(&self) -> bool {
        self.closure_confirmed
    }
    /// Number of observed file-close events, including rollovers.
    pub fn closed_files(&self) -> u64 {
        self.closed_files
    }
    /// Number of observed open-failed/error events.
    pub fn error_events(&self) -> u64 {
        self.error_events
    }
    /// Number of write-failed events. Some are nonfatal missing-track drops.
    pub fn write_failures(&self) -> u64 {
        self.write_failures
    }
    /// Number of native events lost since this session started.
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }
    /// Last observed closed-file path bytes. This may be truncated by the SDK.
    pub fn last_closed_path(&self) -> Option<&EventText> {
        self.last_closed_path.as_ref()
    }
}

/// A finish wait never destroys the recorder, including on timeout or uncertain history.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum FinishStatus {
    /// The final native close is confirmed. Inspect the report for recording errors/losses.
    Closed(RecordingReport),
    /// The finite wait budget elapsed; call `wait_finished` again on the same owner.
    TimedOut(RecordingReport),
    /// Lifecycle evidence is missing/ambiguous. The native owner is retained.
    Indeterminate(RecordingReport),
}

struct Ledger {
    state: RecorderState,
    report: RecordingReport,
    stop_requested: bool,
    open_path: Option<EventText>,
    candidate_close: bool,
    uncertain: bool,
    dropped_base: u64,
    pending_rollovers: u64,
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
            stop_requested: false,
            open_path: None,
            candidate_close: false,
            uncertain: false,
            dropped_base,
            pending_rollovers: 0,
        }
    }
    fn dropped(&mut self, cumulative: u64) {
        if cumulative < self.dropped_base {
            self.uncertain = true;
        }
        self.report.dropped_events = cumulative.saturating_sub(self.dropped_base);
        if self.report.dropped_events != 0 {
            self.uncertain = true;
        }
        if self.uncertain && self.stop_requested {
            self.state = RecorderState::Indeterminate;
        }
    }
    fn observe(&mut self, raw: &ffi::RecorderEvent) {
        let kind = RecorderEventKind::from_raw(raw.kind as u32);
        let path = EventText::from_buffer(&raw.path);
        match kind {
            RecorderEventKind::Started | RecorderEventKind::Rollover => {
                if self.open_path.is_some()
                    || (self.stop_requested && kind == RecorderEventKind::Rollover)
                {
                    self.uncertain = true;
                }
                if path.as_bytes().is_empty()
                    || !path.is_terminated()
                    || path.is_possibly_truncated()
                {
                    self.uncertain = true;
                }
                if kind == RecorderEventKind::Rollover {
                    self.pending_rollovers = self.pending_rollovers.saturating_sub(1);
                }
                self.open_path = Some(path);
                self.candidate_close = false;
            }
            RecorderEventKind::FileClosed => {
                let matches = self.open_path.as_ref().is_some_and(|open| {
                    open.as_bytes() == path.as_bytes()
                        && path.is_terminated()
                        && !path.is_possibly_truncated()
                });
                if !matches {
                    self.uncertain = true;
                }
                self.open_path = None;
                self.report.closed_files = self.report.closed_files.saturating_add(1);
                self.report.last_closed_path = Some(path);
                self.candidate_close = matches && self.pending_rollovers == 0;
            }
            RecorderEventKind::Error => {
                self.report.error_events = self.report.error_events.saturating_add(1);
                self.stop_requested = true;
                self.state = RecorderState::Stopping;
            }
            RecorderEventKind::OpenFailed => {
                self.report.error_events = self.report.error_events.saturating_add(1);
            }
            RecorderEventKind::WriteFailed => {
                self.report.write_failures = self.report.write_failures.saturating_add(1);
            }
            _ => {}
        }
        if self.uncertain && self.stop_requested {
            self.state = RecorderState::Indeterminate;
        }
    }
    fn drained(&mut self, native_running: bool) {
        if (self.stop_requested || !native_running) && self.uncertain {
            self.state = RecorderState::Indeterminate;
        } else if self.candidate_close && self.open_path.is_none() && !native_running {
            self.state = RecorderState::Closed;
            self.report.closure_confirmed = true;
        }
    }
}

/// A thread-affine source recorder with explicit, recoverable finalization.
///
/// `finish` consumes events internally; their lifecycle/error counts remain in
/// the report. Applications needing every event should poll throughout recording.
/// Dropping an active recorder releases resources but may leave incomplete output.
pub struct FileRecorder {
    api: FileApi,
    handle: NonNull<c_void>,
    options: RecorderOptions,
    _raw_options: Box<ffi::RecorderOptions>,
    // Retain submitted descriptors and strings through native destruction because
    // replacement/copy timing is not fully specified by the SDK.
    sources: Vec<(SourceSpec, Box<ffi::Source>)>,
    accepted_source: Option<usize>,
    paths: Vec<CString>,
    ledger: Ledger,
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
fn source_raw(source: &SourceSpec) -> ffi::Source {
    ffi::Source {
        p_ndi_name: source
            .name
            .as_ref()
            .map_or(std::ptr::null(), |s| s.as_ptr()),
        p_url_address: source.url.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
    }
}
impl FileRecorder {
    pub(super) fn create(api: FileApi, options: RecorderOptions) -> FileResult<Self> {
        let raw = Box::new(ffi::RecorderOptions {
            source_to_connect_to: source_raw(&options.source),
            track_flags: options.tracks.bits() as i32,
            record_float_audio_planar_32f: options.planar_audio,
            disable_auto_rollover_on_format_change: options.disable_auto_rollover,
            p_output_filename_pattern: options
                .pattern
                .as_ref()
                .map_or(std::ptr::null(), |s| s.as_ptr()),
            disable_proxy_track: options.disable_proxy,
            rollover_max_file_size_bytes: options.rollover_bytes,
        });
        let handle = NonNull::new(unsafe { (api.backend.functions.recorder_create)(&*raw) })
            .ok_or(FileError::Rejected("recorder creation"))?;
        Ok(Self {
            api,
            handle,
            options,
            _raw_options: raw,
            sources: Vec::new(),
            accepted_source: Some(usize::MAX),
            paths: Vec::new(),
            ledger: Ledger::idle(),
            _thread: PhantomData,
        })
    }

    /// Current lifecycle state; disconnected sources may still be `Recording`.
    pub fn state(&self) -> RecorderState {
        self.ledger.state
    }
    /// A copy of all completion/error evidence observed so far.
    pub fn report(&self) -> RecordingReport {
        self.ledger.report.clone()
    }

    /// Start a new session. Naming is fixed by the recorder options.
    ///
    /// Existing exact filenames are rejected by the SDK; this wrapper never
    /// removes files or creates parent directories to make a start succeed.
    pub fn start(
        &mut self,
        target: RecordingTarget,
        timecode: RecordingTimecode,
    ) -> FileResult<u64> {
        if !matches!(self.state(), RecorderState::Idle | RecorderState::Closed) {
            return Err(FileError::InvalidState(
                "finish or abandon the previous session before starting",
            ));
        }
        // Clear stale notifications before establishing a fresh session boundary.
        // A continuously nonempty queue cannot be treated as a clean boundary.
        let mut empty = false;
        for _ in 0..256 {
            let mut raw = ffi::RecorderEvent::default();
            if unsafe {
                (self.api.backend.functions.recorder_capture)(self.handle.as_ptr(), &mut raw, 0)
            } == 0
            {
                empty = true;
                break;
            }
        }
        if !empty {
            return Err(FileError::InvalidState(
                "event queue has no clean session boundary",
            ));
        }
        let session =
            self.ledger.report.session.checked_add(1).ok_or_else(|| {
                FileError::InvalidInput("recording session counter exhausted".into())
            })?;
        let dropped = self.dropped_event_count();
        let pointer = target.prefix.as_ptr();
        self.paths.push(target.prefix);
        let accepted = unsafe {
            match timecode {
                RecordingTimecode::Preserve => {
                    (self.api.backend.functions.recorder_start)(self.handle.as_ptr(), pointer)
                }
                RecordingTimecode::GenerateFrom(value) => {
                    (self.api.backend.functions.recorder_start_timecode)(
                        self.handle.as_ptr(),
                        pointer,
                        value.ticks(),
                    )
                }
            }
        };
        if !accepted {
            return Err(FileError::Rejected("recorder start"));
        }
        self.ledger = Ledger::session(session, dropped, RecorderState::Recording);
        Ok(session)
    }

    /// Request asynchronous stop once. It does not certify that a file is complete.
    pub fn request_stop(&mut self) -> StopRequest {
        if self.state() == RecorderState::Idle {
            return StopRequest::NotStarted;
        }
        if self.state() == RecorderState::Closed {
            return StopRequest::AlreadyClosed;
        }
        if self.ledger.stop_requested {
            return StopRequest::AlreadyStopping;
        }
        self.ledger.stop_requested = true;
        self.ledger.state = if self.ledger.uncertain {
            RecorderState::Indeterminate
        } else {
            RecorderState::Stopping
        };
        unsafe { (self.api.backend.functions.recorder_stop)(self.handle.as_ptr()) };
        StopRequest::Requested
    }

    /// Poll an owned event, updating completion evidence before decoding the payload.
    pub fn poll_event(&mut self, wait: Duration) -> FileResult<Option<RecorderEvent>> {
        let wait = timeout_ms(wait)?;
        let mut raw = ffi::RecorderEvent::default();
        let kind = unsafe {
            (self.api.backend.functions.recorder_capture)(self.handle.as_ptr(), &mut raw, wait)
        };
        self.ledger.dropped(self.dropped_event_count());
        if kind == 0 {
            self.ledger.drained(self.is_recording());
            return Ok(None);
        }
        if kind != raw.kind {
            self.ledger.uncertain = true;
            return Err(FileError::Protocol(
                "recorder event discriminants disagree".into(),
            ));
        }
        self.ledger.observe(&raw);
        // Even malformed source-format payloads cannot swallow a close event.
        Ok(Some(RecorderEvent::try_from(&raw)?))
    }

    /// Request stop, then wait within the supplied event-wait budget.
    pub fn finish(&mut self, wait: Duration) -> FileResult<FinishStatus> {
        timeout_ms(wait)?;
        self.request_stop();
        self.wait_finished(wait)
    }

    /// Continue waiting on the same owner after timeout. This never destroys it.
    pub fn wait_finished(&mut self, wait: Duration) -> FileResult<FinishStatus> {
        timeout_ms(wait)?;
        if !self.ledger.stop_requested && self.state() != RecorderState::Closed {
            return Err(FileError::InvalidState(
                "request stop before waiting for finalization",
            ));
        }
        let start = Instant::now();
        loop {
            // Drain queued events before accepting a close, to detect a stale
            // rollover-close followed by another opened generation.
            let remaining = wait.saturating_sub(start.elapsed());
            let budget = if self.ledger.candidate_close {
                Duration::ZERO
            } else {
                remaining.min(Duration::from_millis(50))
            };
            let event = self.poll_event(budget)?;
            if event.is_none() {
                match self.state() {
                    RecorderState::Closed => return Ok(FinishStatus::Closed(self.report())),
                    RecorderState::Indeterminate => {
                        return Ok(FinishStatus::Indeterminate(self.report()))
                    }
                    _ => {}
                }
            }
            if start.elapsed() >= wait {
                return Ok(if self.ledger.uncertain {
                    FinishStatus::Indeterminate(self.report())
                } else {
                    FinishStatus::TimedOut(self.report())
                });
            }
        }
    }

    /// Best-effort stop/destruction without claiming the output was finalized.
    pub fn abandon(self) -> RecordingReport {
        self.report()
    }

    /// Whether the native recorder is writing or waiting for source frames.
    /// This is not a finalization acknowledgement.
    pub fn is_recording(&self) -> bool {
        unsafe { (self.api.backend.functions.recorder_running)(self.handle.as_ptr()) }
    }
    /// Lifetime cumulative native event losses.
    pub fn dropped_event_count(&self) -> u64 {
        unsafe { (self.api.backend.functions.recorder_dropped)(self.handle.as_ptr()) }
    }

    fn require_mutable_session(&self) -> FileResult<()> {
        if matches!(
            self.state(),
            RecorderState::Stopping | RecorderState::Indeterminate
        ) {
            Err(FileError::InvalidState(
                "recording is stopping or completion is indeterminate",
            ))
        } else {
            Ok(())
        }
    }

    /// Switch the recorded source. An active session rolls into a new file.
    pub fn set_source(&mut self, source: SourceSpec) -> FileResult<()> {
        self.require_mutable_session()?;
        let raw = Box::new(source_raw(&source));
        let pointer = &*raw as *const ffi::Source;
        self.sources.push((source, raw));
        if unsafe {
            (self.api.backend.functions.recorder_set_source)(self.handle.as_ptr(), pointer)
        } {
            self.accepted_source = Some(self.sources.len() - 1);
            if self.state() == RecorderState::Recording {
                self.ledger.pending_rollovers = self.ledger.pending_rollovers.saturating_add(1);
            }
            Ok(())
        } else {
            self.accepted_source = None;
            Err(FileError::Rejected("recorder source change"))
        }
    }

    /// Last successfully accepted source request, not a live connected-source query.
    /// A failed native source change clears this value because rollback is unspecified.
    pub fn last_accepted_source(&self) -> Option<&SourceSpec> {
        self.accepted_source.map(|index| {
            if index == usize::MAX {
                &self.options.source
            } else {
                &self.sources[index].0
            }
        })
    }

    /// Exact native source snapshots remain unavailable until background-stop
    /// invalidation has a documented safe copy boundary.
    pub fn native_source(&self) -> FileResult<SourceSpec> {
        let _ = self.api.backend.functions.recorder_get_source;
        Err(super::unavailable("file.recorder.native_source", "UnverifiedOwnershipContract: background-stop pointer lifetime is not established; use last_accepted_source for request state"))
    }

    /// Current file reported by owned events. It may lag and may be truncated.
    pub fn reported_current_file(&self) -> Option<&EventText> {
        self.ledger.open_path.as_ref()
    }

    /// The SDK's borrowed current-path getter cannot safely be copied while its
    /// background worker may automatically roll over. Use the owned event snapshot.
    pub fn native_current_file(&self) -> FileResult<std::path::PathBuf> {
        let _ = self.api.backend.functions.recorder_current_file;
        Err(super::unavailable("file.recorder.native_current_file", "UnverifiedOwnershipContract: pointer may expire during automatic rollover; use reported_current_file"))
    }

    /// Roll an active session into another prefix, at native keyframe boundaries.
    pub fn rollover(&mut self, target: RecordingTarget) -> FileResult<()> {
        if self.state() != RecorderState::Recording {
            return Err(FileError::InvalidState(
                "rollover requires an active session",
            ));
        }
        let pointer = target.prefix.as_ptr();
        self.paths.push(target.prefix);
        if unsafe { (self.api.backend.functions.recorder_rollover)(self.handle.as_ptr(), pointer) }
        {
            self.ledger.pending_rollovers = self.ledger.pending_rollovers.saturating_add(1);
            Ok(())
        } else {
            Err(FileError::Rejected("recorder rollover"))
        }
    }

    /// Inject metadata at the current file timeline position. The SDK copies it
    /// before return; editorial timecode does not reposition the packet.
    pub fn inject_metadata(&mut self, metadata: &crate::MetadataFrame) -> FileResult<()> {
        if self.state() != RecorderState::Recording {
            return Err(FileError::InvalidState(
                "metadata injection requires an active session",
            ));
        }
        let data = CString::new(metadata.data())
            .map_err(|_| FileError::InvalidInput("metadata contains NUL".into()))?;
        let length = i32::try_from(data.as_bytes_with_nul().len())
            .map_err(|_| FileError::InvalidInput("metadata exceeds native length".into()))?;
        let raw = ffi::Metadata {
            length,
            timecode: metadata.timecode(),
            p_data: data.as_ptr().cast_mut(),
        };
        if unsafe { (self.api.backend.functions.recorder_inject)(self.handle.as_ptr(), &raw) } {
            Ok(())
        } else {
            Err(FileError::Rejected("metadata injection"))
        }
    }
    /// Live telemetry, if available.
    pub fn stats(&self) -> Option<RecorderStats> {
        let mut raw = ffi::RecorderStats::default();
        unsafe { (self.api.backend.functions.recorder_stats)(self.handle.as_ptr(), &mut raw) }
            .then(|| RecorderStats::from(&raw))
    }
    /// Detected source format, once audio or video has been seen.
    pub fn source_format(&self) -> FileResult<Option<SourceFormat>> {
        let mut raw = ffi::SourceFormat::default();
        if unsafe { (self.api.backend.functions.recorder_format)(self.handle.as_ptr(), &mut raw) } {
            Ok(Some(SourceFormat::try_from(&raw)?))
        } else {
            Ok(None)
        }
    }
}
impl Drop for FileRecorder {
    fn drop(&mut self) {
        // No event-draining loop or user callback in Drop. Native destruction
        // releases resources, but may block and does not certify file durability.
        unsafe {
            if !self.ledger.stop_requested && self.state() != RecorderState::Closed {
                (self.api.backend.functions.recorder_stop)(self.handle.as_ptr());
            }
            (self.api.backend.functions.recorder_destroy)(self.handle.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(kind: i32, path: &str) -> ffi::RecorderEvent {
        let mut raw = ffi::RecorderEvent {
            kind,
            ..Default::default()
        };
        for (slot, value) in raw.path.iter_mut().zip(path.bytes()) {
            *slot = value as _;
        }
        raw
    }
    #[test]
    fn final_close_requires_matching_file_and_a_drained_stopped_session() {
        let mut ledger = Ledger::session(1, 0, RecorderState::Recording);
        ledger.observe(&event(1, "take.mkv"));
        ledger.stop_requested = true;
        ledger.observe(&event(10, "take.mkv"));
        assert!(!ledger.report.closure_confirmed);
        ledger.drained(true);
        assert!(!ledger.report.closure_confirmed);
        ledger.drained(false);
        assert!(ledger.report.closure_confirmed);
    }
    #[test]
    fn missing_or_stale_close_never_becomes_success() {
        let mut ledger = Ledger::session(1, 7, RecorderState::Recording);
        ledger.observe(&event(1, "first.mkv"));
        ledger.stop_requested = true;
        ledger.observe(&event(10, "other.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Indeterminate);
        assert!(!ledger.report.closure_confirmed);
    }
    #[test]
    fn sdk_event_loss_taints_an_otherwise_matching_final_close() {
        let mut ledger = Ledger::session(1, 7, RecorderState::Recording);
        ledger.observe(&event(1, "take.mkv"));
        ledger.stop_requested = true;
        ledger.observe(&event(10, "take.mkv"));
        ledger.dropped(8);
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Indeterminate);
    }
    #[test]
    fn queued_rollover_after_stop_cannot_certify_previous_close() {
        let mut ledger = Ledger::session(1, 0, RecorderState::Recording);
        ledger.observe(&event(1, "first.mkv"));
        ledger.stop_requested = true;
        ledger.observe(&event(10, "first.mkv"));
        ledger.observe(&event(2, "second.mkv"));
        ledger.observe(&event(10, "second.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Indeterminate);
    }
    #[test]
    fn automatic_native_stop_can_confirm_its_observed_close() {
        let mut ledger = Ledger::session(1, 0, RecorderState::Recording);
        ledger.observe(&event(1, "take.mkv"));
        ledger.observe(&event(6, "take.mkv")); // format-change stop is not an error
        ledger.observe(&event(10, "take.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Closed);
        assert!(ledger.report.closure_confirmed);
    }

    #[test]
    fn native_error_can_close_but_report_preserves_recording_failure() {
        let mut ledger = Ledger::session(1, 0, RecorderState::Recording);
        ledger.observe(&event(1, "take.mkv"));
        ledger.observe(&event(4, "take.mkv"));
        ledger.observe(&event(5, "take.mkv"));
        ledger.observe(&event(10, "take.mkv"));
        ledger.drained(false);
        assert_eq!(ledger.state, RecorderState::Closed);
        assert_eq!(ledger.report.error_events, 1);
        assert_eq!(ledger.report.write_failures, 1);
    }
}
