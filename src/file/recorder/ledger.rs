//! Recording lifecycle and the native stop obligation derived from it.

#[cfg(test)]
use super::OpenFile;
use super::{
    settle::{LateDrain, WaitOutcome},
    tracker::{FileClose, FileTracker},
    RecorderState, SessionEnd,
};
use crate::{
    file::{EventText, RecorderEventKind, StateConflict},
    Error, Result,
};

/// What the recorder's events have shown about a session so far.
///
/// A confirmed closure means the SDK finalized every file it opened. It says
/// nothing about frames dropped while recording, or about the data having
/// reached stable storage.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RecordingReport {
    phase: SessionPhase,
    counts: SessionReport,
}

/// A ledger produces this immutable phase beside the corresponding counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SessionPhase {
    NotStarted,
    Recording,
    Stopping { requested: bool },
    Ended(FinalPhase),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Closure {
    Confirmed,
    Unconfirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FinalPhase {
    closure: Closure,
    requested: bool,
}

impl FinalPhase {
    fn state(self) -> RecorderState {
        match self.closure {
            Closure::Confirmed => RecorderState::Closed,
            Closure::Unconfirmed => RecorderState::Indeterminate,
        }
    }
}

impl RecordingReport {
    /// The session's number, counting from one for each recorder; zero when
    /// the recorder has not started a session.
    pub fn session(&self) -> u64 {
        self.counts.session
    }
    /// Whether at least one file was reported closed, every reported open
    /// was matched by its close, and file tracking stayed complete.
    ///
    /// A session that ends before reporting any file has no confirmed
    /// closure, even when no event was lost and no cleanup remains pending.
    pub fn closure_confirmed(&self) -> bool {
        matches!(
            self.phase,
            SessionPhase::Ended(FinalPhase {
                closure: Closure::Confirmed,
                ..
            })
        )
    }
    /// How many files were reported closed, including those closed by rollovers.
    ///
    /// The SDK reports a file closed whether or not anything was written to
    /// it; see [`empty_files`](Self::empty_files).
    pub fn closed_files(&self) -> u64 {
        self.counts.closed_files
    }
    /// How many closed files were confirmed to contain no audio or video.
    ///
    /// The SDK opens a file as soon as a session or a rollover starts, and
    /// closes it as finalized even if no frame ever reached it: a stop before
    /// the source delivered anything, a switch to a source that is not there,
    /// a rollover while the source is lost. Such a file is empty or holds
    /// only a header, and may not open in a player. A file counts here when
    /// it closed under complete tracking without a reported first frame.
    ///
    /// The count is exact when [`closure_confirmed`](Self::closure_confirmed)
    /// is true. Otherwise it is a lower bound: files confirmed empty before
    /// tracking became incomplete remain counted, while later closes cannot
    /// establish that a missing first-frame event was never delivered.
    ///
    /// Always zero for a recorder that records neither audio nor video,
    /// because the SDK reports no first frame for metadata.
    pub fn empty_files(&self) -> u64 {
        self.counts.empty_files
    }
    /// How many open-failed and error events were seen.
    pub fn error_events(&self) -> u64 {
        self.counts.error_events
    }
    /// How many write-failed events were seen. Not every one is fatal: a
    /// stream that appears after a file was opened is dropped until the next
    /// rollover.
    pub fn write_failures(&self) -> u64 {
        self.counts.write_failures
    }
    /// How many events the SDK discarded during the session because they were
    /// not taken in time.
    pub fn dropped_events(&self) -> u64 {
        self.counts.dropped_events
    }
    /// The path of the file reported closed last, as the SDK gave it.
    pub fn last_closed_path(&self) -> Option<&EventText> {
        self.counts.last_closed_path.as_ref()
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
            SessionPhase::Ended(FinalPhase {
                requested: false,
                ..
            })
        )
    }
}

/// Counts and paths belong to a session; its lifecycle determines the
/// phase when producing an immutable public report.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SessionReport {
    session: u64,
    closed_files: u64,
    empty_files: u64,
    error_events: u64,
    write_failures: u64,
    dropped_events: u64,
    last_closed_path: Option<EventText>,
}

impl SessionReport {
    fn new(session: u64) -> Self {
        Self {
            session,
            closed_files: 0,
            empty_files: 0,
            error_events: 0,
            write_failures: 0,
            dropped_events: 0,
            last_closed_path: None,
        }
    }

    fn report(&self, phase: SessionPhase) -> RecordingReport {
        self.clone().into_report(phase)
    }

    fn into_report(self, phase: SessionPhase) -> RecordingReport {
        RecordingReport {
            phase,
            counts: self,
        }
    }
}

/// A successful native start owes exactly one native stop, including when
/// the SDK ends recording itself. Only the native recorder consumes it.
#[must_use = "ending a native recording must issue its owed stop"]
pub(super) struct StopOwed {
    _private: (),
}

#[must_use = "a session transition may owe a native stop"]
pub(super) struct Transition {
    pub(super) stop: Option<StopOwed>,
}

struct OpenSession {
    report: SessionReport,
    files: FileTracker,
    records_media: bool,
    dropped_base: u64,
}

impl OpenSession {
    fn all_closed(&self) -> bool {
        !self.files.cleanup_pending() && self.report.closed_files > 0
    }

    fn final_phase(&self, stopping: Option<bool>) -> Option<FinalPhase> {
        let closure = if self.all_closed() {
            Closure::Confirmed
        } else if !self.files.is_complete() || (stopping.is_some() && self.files.is_empty()) {
            Closure::Unconfirmed
        } else {
            return None;
        };
        Some(FinalPhase {
            closure,
            requested: stopping.unwrap_or(false),
        })
    }

    fn end(self, phase: FinalPhase) -> Session {
        Session::Ended {
            phase,
            counts: self.report,
            files: self.files,
            dropped_base: self.dropped_base,
        }
    }
}

/// Each state owns only the data it can use. A stopping session has already
/// issued its stop; a final session retains only its report and cleanup files.
enum Session {
    Idle {
        last: Option<RecordingReport>,
    },
    Open {
        data: OpenSession,
        stop: StopOwed,
    },
    Stopping {
        data: OpenSession,
        requested: bool,
    },
    Ended {
        phase: FinalPhase,
        counts: SessionReport,
        files: FileTracker,
        dropped_base: u64,
    },
}

impl Session {
    /// Moving out of Open transfers its only native-stop obligation.
    fn stop_requested(self, requested: bool) -> (Self, Option<StopOwed>) {
        match self {
            Self::Open { data, stop } => (Self::Stopping { data, requested }, Some(stop)),
            previous => (previous, None),
        }
    }

    fn phase_after_drain(&self, native_recording: impl FnOnce() -> bool) -> Option<FinalPhase> {
        let phase = match self {
            Self::Open { data, .. } => data.final_phase(None),
            Self::Stopping { data, requested } => data.final_phase(Some(*requested)),
            Self::Idle { .. } | Self::Ended { .. } => None,
        };
        phase.filter(|_| !native_recording())
    }

    fn end(self, phase: FinalPhase) -> (Self, Option<StopOwed>) {
        match self {
            Self::Open { data, stop } => (data.end(phase), Some(stop)),
            Self::Stopping { data, .. } => (data.end(phase), None),
            previous => (previous, None),
        }
    }
}

/// The record of one session, built from the recorder's events.
pub(super) struct Ledger {
    session: Session,
}

impl Ledger {
    pub(super) fn idle() -> Self {
        Self {
            session: Session::Idle { last: None },
        }
    }

    /// A replacement is explicitly idle while retaining the previous report.
    pub(super) fn inheriting(report: RecordingReport) -> Self {
        Self {
            session: Session::Idle { last: Some(report) },
        }
    }

    pub(super) fn session(session: u64, dropped_base: u64, records_media: bool) -> Self {
        Self {
            session: Session::Open {
                data: OpenSession {
                    report: SessionReport::new(session),
                    files: FileTracker::new(),
                    records_media,
                    dropped_base,
                },
                stop: StopOwed { _private: () },
            },
        }
    }

    #[cfg(test)]
    fn with_report(
        session: u64,
        phase: SessionPhase,
        dropped_base: u64,
        records_media: bool,
    ) -> Self {
        let mut ledger = Self::session(session, dropped_base, records_media);
        match phase {
            SessionPhase::NotStarted => Self::idle(),
            SessionPhase::Recording => ledger,
            SessionPhase::Stopping { requested } => {
                let _ = ledger.stop_requested(requested);
                ledger
            }
            SessionPhase::Ended(phase) => Self {
                session: Session::Ended {
                    phase,
                    counts: SessionReport::new(session),
                    files: FileTracker::new(),
                    dropped_base,
                },
            },
        }
    }

    pub(super) fn report(&self) -> RecordingReport {
        match &self.session {
            Session::Idle { last } => last
                .clone()
                .unwrap_or_else(|| SessionReport::new(0).into_report(SessionPhase::NotStarted)),
            Session::Open { data, .. } => data.report.report(SessionPhase::Recording),
            Session::Stopping { data, requested } => data.report.report(SessionPhase::Stopping {
                requested: *requested,
            }),
            Session::Ended { phase, counts, .. } => counts.report(SessionPhase::Ended(*phase)),
        }
    }

    pub(super) fn session_number(&self) -> u64 {
        match &self.session {
            Session::Idle { last } => last.as_ref().map_or(0, RecordingReport::session),
            Session::Open { data, .. } | Session::Stopping { data, .. } => data.report.session,
            Session::Ended { counts, .. } => counts.session,
        }
    }

    pub(super) fn state(&self) -> RecorderState {
        match &self.session {
            Session::Idle { .. } => RecorderState::Idle,
            Session::Open { .. } => RecorderState::Recording,
            Session::Stopping { .. } => RecorderState::Stopping,
            Session::Ended { phase, .. } => phase.state(),
        }
    }

    fn open(&self) -> Option<&OpenSession> {
        match &self.session {
            Session::Open { data, .. } | Session::Stopping { data, .. } => Some(data),
            _ => None,
        }
    }

    fn open_mut(&mut self) -> Option<&mut OpenSession> {
        match &mut self.session {
            Session::Open { data, .. } | Session::Stopping { data, .. } => Some(data),
            _ => None,
        }
    }

    pub(super) fn session_open(&self) -> bool {
        matches!(
            self.session,
            Session::Open { .. } | Session::Stopping { .. }
        )
    }

    pub(super) fn tracker(&self) -> Option<&FileTracker> {
        match &self.session {
            Session::Open { data, .. } | Session::Stopping { data, .. } => Some(&data.files),
            Session::Ended { files, .. } => Some(files),
            Session::Idle { .. } => None,
        }
    }

    fn tracker_mut(&mut self) -> Option<&mut FileTracker> {
        match &mut self.session {
            Session::Open { data, .. } | Session::Stopping { data, .. } => Some(&mut data.files),
            Session::Ended { files, .. } => Some(files),
            Session::Idle { .. } => None,
        }
    }

    #[cfg(test)]
    fn files(&self) -> &[OpenFile] {
        self.tracker().map_or(&[], FileTracker::files)
    }

    pub(super) fn cleanup_pending(&self) -> bool {
        self.session_open() || self.tracker().is_some_and(FileTracker::cleanup_pending)
    }

    pub(super) fn admit_start(&self) -> Result<()> {
        let conflict = if self.session_open() {
            Some(StateConflict::PreviousSessionOpen)
        } else {
            self.tracker().and_then(|files| {
                if !files.is_complete() {
                    Some(StateConflict::IncompleteFileTracking)
                } else if !files.is_empty() {
                    Some(StateConflict::PreviousSessionOpen)
                } else {
                    None
                }
            })
        };
        conflict.map_or(Ok(()), |state| Err(Error::InvalidState(state)))
    }

    pub(super) fn all_closed(&self) -> bool {
        self.open().is_some_and(OpenSession::all_closed)
    }

    pub(super) fn settle(&self, late: Option<&LateDrain>) -> Option<WaitOutcome> {
        if let Session::Ended { phase, counts, .. } = &self.session {
            let report = counts.report(SessionPhase::Ended(*phase));
            let end = match phase.closure {
                Closure::Confirmed => SessionEnd::Closed(report),
                Closure::Unconfirmed => SessionEnd::Indeterminate(report),
            };
            Some(WaitOutcome::Ended(end))
        } else {
            late.map(|proof| WaitOutcome::Expired(self.report(), *proof))
        }
    }

    fn transition(
        &mut self,
        transition: impl FnOnce(Session) -> (Session, Option<StopOwed>),
    ) -> Transition {
        let previous = std::mem::replace(&mut self.session, Session::Idle { last: None });
        let (next, stop) = transition(previous);
        self.session = next;
        Transition { stop }
    }

    pub(super) fn stop_requested(&mut self, requested: bool) -> Transition {
        self.transition(|session| session.stop_requested(requested))
    }

    pub(super) fn mark_uncertain(&mut self) {
        if let Some(files) = self.tracker_mut() {
            files.mark_incomplete();
        }
    }

    #[cfg(test)]
    fn is_uncertain(&self) -> bool {
        self.tracker().is_some_and(|files| !files.is_complete())
    }

    pub(super) fn dropped(&mut self, cumulative: u64) {
        match &mut self.session {
            Session::Open { data, .. } | Session::Stopping { data, .. } => {
                data.report.dropped_events = cumulative.saturating_sub(data.dropped_base);
                if cumulative != data.dropped_base {
                    data.files.mark_incomplete();
                }
            }
            Session::Ended {
                files,
                dropped_base,
                ..
            } => {
                if cumulative != *dropped_base {
                    files.mark_incomplete();
                }
            }
            Session::Idle { .. } => {}
        }
    }

    pub(super) fn observe(&mut self, kind: RecorderEventKind, path: EventText) -> Transition {
        let Some(data) = self.open_mut() else {
            // Cleanup evidence cannot change a report already returned.
            if let Some(files) = self.tracker_mut() {
                match kind {
                    RecorderEventKind::Started
                    | RecorderEventKind::Rollover
                    | RecorderEventKind::Unknown(_) => {
                        // A late open contradicts the finalized complete set;
                        // an unknown identifier could describe another open.
                        files.mark_incomplete();
                    }
                    RecorderEventKind::FirstFrame => files.first_frame(&path),
                    RecorderEventKind::FileClosed => {
                        files.close(&path);
                    }
                    _ => {}
                }
            }
            return Transition { stop: None };
        };
        match kind {
            RecorderEventKind::Started | RecorderEventKind::Rollover => {
                data.files.open(kind, path);
            }
            RecorderEventKind::FirstFrame => {
                data.files.first_frame(&path);
            }
            RecorderEventKind::FileClosed => {
                if data.files.close(&path) == FileClose::ConfirmedEmpty && data.records_media {
                    data.report.empty_files = data.report.empty_files.saturating_add(1);
                }
                data.report.closed_files = data.report.closed_files.saturating_add(1);
                data.report.last_closed_path = Some(path);
            }
            RecorderEventKind::Error => {
                data.report.error_events = data.report.error_events.saturating_add(1);
                return self.stop_requested(false);
            }
            RecorderEventKind::OpenFailed => {
                data.report.error_events = data.report.error_events.saturating_add(1);
            }
            RecorderEventKind::WriteFailed => {
                data.report.write_failures = data.report.write_failures.saturating_add(1);
            }
            RecorderEventKind::Unknown(_) => data.files.mark_incomplete(),
            _ => {}
        }
        Transition { stop: None }
    }

    /// Finality relies on open-before-close rollover events, pinned by the
    /// native rollover-order test. Every exit transfers any owed native stop.
    pub(super) fn drained(&mut self, native_recording: impl FnOnce() -> bool) -> Transition {
        let Some(phase) = self.session.phase_after_drain(native_recording) else {
            return Transition { stop: None };
        };
        self.transition(|session| session.end(phase))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_report_debug_names_its_closed_path() {
        let path = b"take.mkv\0\0".map(|byte| byte as std::ffi::c_char);
        let report = RecordingReport {
            phase: SessionPhase::Ended(FinalPhase {
                closure: Closure::Confirmed,
                requested: true,
            }),
            counts: SessionReport {
                closed_files: 1,
                last_closed_path: Some(EventText::from_buffer(&path)),
                ..SessionReport::new(1)
            },
        };
        let debug = format!("{report:?}");
        assert!(debug.contains("\"take.mkv\""));
        assert!(!debug.contains("116, 97"));
    }

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
    fn idle_and_ended_session_transitions_preserve_their_reports_and_owe_no_stop() {
        let mut closed = session();
        let _ = closed.observe(Started, path("take.mkv"));
        let _ = closed.observe(FileClosed, path("take.mkv"));
        assert!(closed.drained(|| false).stop.is_some());
        for mut ledger in [Ledger::idle(), closed] {
            let state = ledger.state();
            let report = ledger.report();
            for requested in [false, true] {
                assert!(ledger.stop_requested(requested).stop.is_none());
                for native_recording in [false, true] {
                    assert!(ledger.drained(|| native_recording).stop.is_none());
                    assert_eq!(ledger.state(), state);
                    assert_eq!(ledger.report(), report);
                }
            }
        }
    }

    #[test]
    fn active_incomplete_tracking_keeps_the_session_refusal_until_it_ends() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("known.mkv"));
        ledger.dropped(1);
        for stopping in [false, true] {
            if stopping {
                assert!(ledger.stop_requested(true).stop.is_some());
            }
            assert!(matches!(
                ledger.admit_start(),
                Err(crate::Error::InvalidState(
                    StateConflict::PreviousSessionOpen
                ))
            ));
        }
        assert!(
            ledger.drained(|| false).stop.is_none(),
            "the stop was already issued"
        );
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert!(matches!(
            ledger.admit_start(),
            Err(crate::Error::InvalidState(
                StateConflict::IncompleteFileTracking
            ))
        ));
        let report = ledger.report();
        let _ = ledger.observe(FileClosed, path("known.mkv"));
        assert!(
            ledger.cleanup_pending(),
            "a known close cannot retire unknown files"
        );
        assert_eq!(
            ledger.report(),
            report,
            "cleanup keeps the final report fixed"
        );
        assert!(matches!(
            ledger.admit_start(),
            Err(crate::Error::InvalidState(
                StateConflict::IncompleteFileTracking
            ))
        ));
    }

    #[test]
    fn empty_polls_query_native_recording_only_when_the_session_can_end() {
        for (state, decisive) in [
            (RecorderState::Idle, [false; 4]),
            (RecorderState::Recording, [false, false, true, true]),
            (RecorderState::Stopping, [true, false, true, true]),
            (RecorderState::Closed, [false; 4]),
        ] {
            for (evidence, expected_query) in decisive.into_iter().enumerate() {
                let mut ledger = if state == RecorderState::Idle {
                    Ledger::idle()
                } else {
                    session()
                };
                if state == RecorderState::Closed {
                    let _ = ledger.observe(Started, path("previous.mkv"));
                    let _ = ledger.observe(FileClosed, path("previous.mkv"));
                    let _ = ledger.drained(|| false);
                }
                if state == RecorderState::Stopping {
                    let _ = ledger.stop_requested(true);
                }
                if evidence == 1 || evidence == 2 {
                    let _ = ledger.observe(Started, path("take.mkv"));
                }
                if evidence == 2 {
                    let _ = ledger.observe(FileClosed, path("take.mkv"));
                }
                if evidence == 3 {
                    ledger.dropped(1);
                }

                let mut queried = false;
                let transition = ledger.drained(|| {
                    queried = true;
                    true
                });
                assert!(transition.stop.is_none());
                assert_eq!(queried, expected_query, "{state:?}, evidence {evidence}");
                assert_eq!(ledger.state(), state);
            }
        }
    }

    #[test]
    fn closure_needs_the_matching_file_and_a_stopped_drained_recorder() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        assert!(!ledger.report().closure_confirmed());
        let _ = ledger.drained(|| true);
        assert_eq!(ledger.state(), RecorderState::Stopping);
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report().closure_confirmed());
    }

    #[test]
    fn a_close_for_another_file_never_confirms_the_session() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("first.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("other.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert!(!ledger.report().closure_confirmed());
    }

    #[test]
    fn empty_file_counts_use_only_evidence_observed_under_complete_tracking() {
        for empty_before_loss in [false, true] {
            let mut ledger = session();
            let _ = ledger.observe(Started, path("known.mkv"));
            if empty_before_loss {
                let _ = ledger.observe(FileClosed, path("known.mkv"));
                assert_eq!(ledger.report().empty_files(), 1);
            }
            ledger.dropped(1);
            if !empty_before_loss {
                // The lost event could have been this file's first frame.
                let _ = ledger.observe(FileClosed, path("known.mkv"));
            }
            // Opens after lost evidence are deliberately not retained, and
            // neither they nor their closes can prove another empty file.
            let _ = ledger.observe(Rollover, path("hidden.mkv"));
            let _ = ledger.observe(FileClosed, path("hidden.mkv"));
            let _ = ledger.drained(|| false);
            let report = ledger.report();
            assert_eq!(report.empty_files(), u64::from(empty_before_loss));
            assert_eq!(report.closed_files(), 2);
            assert!(!report.closure_confirmed());
            assert_eq!(report.dropped_events(), 1);
        }
    }

    #[test]
    fn lost_events_taint_an_otherwise_matching_close() {
        let mut ledger = Ledger::session(1, 7, true);
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        ledger.dropped(8);
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert_eq!(ledger.report().dropped_events(), 1);
    }

    #[test]
    fn an_indeterminate_report_keeps_its_pending_cleanup_without_changing_the_report() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        ledger.dropped(1);
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert!(ledger.cleanup_pending());
        let report = ledger.report();
        let _ = ledger.observe(FileClosed, path("other.mkv"));
        assert!(ledger.cleanup_pending());
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        assert!(ledger.cleanup_pending());
        assert!(ledger.files().is_empty());
        assert_eq!(ledger.report(), report);
    }

    #[test]
    fn a_lost_open_retains_unknown_cleanup_after_every_known_file_closes() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("known.mkv"));
        // A rollover to hidden.mkv was discarded by the native event queue.
        ledger.dropped(1);
        let _ = ledger.observe(FileClosed, path("known.mkv"));
        assert!(ledger.files().is_empty());
        let stop = ledger.drained(|| false).stop;
        assert!(stop.is_some());
        assert!(ledger.drained(|| false).stop.is_none());
        let report = ledger.report();
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert!(ledger.cleanup_pending());
        let _ = ledger.observe(FileClosed, path("hidden.mkv"));
        assert!(ledger.cleanup_pending());
        assert_eq!(ledger.report(), report);
    }

    #[test]
    fn an_unknown_event_keeps_cleanup_incomplete_without_inventing_native_loss() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("known.mkv"));
        // A newer native identifier may describe a file lifecycle transition
        // whose meaning this version of the wrapper cannot establish.
        let _ = ledger.observe(RecorderEventKind::Unknown(4321), path("hidden.mkv"));
        let _ = ledger.observe(FileClosed, path("known.mkv"));
        assert!(ledger.files().is_empty());
        let stop = ledger.drained(|| false).stop;
        assert!(stop.is_some());
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert!(ledger.cleanup_pending());
        assert_eq!(ledger.report().dropped_events(), 0);
        assert_eq!(ledger.report().error_events(), 0);
        assert_eq!(ledger.report().write_failures(), 0);
    }

    #[test]
    fn later_evidence_preserves_a_final_report_and_taints_its_cleanup_knowledge() {
        for evidence in 0..6 {
            let mut ledger = session();
            let _ = ledger.observe(Started, path("known.mkv"));
            let _ = ledger.observe(FileClosed, path("known.mkv"));
            let _ = ledger.drained(|| false);
            assert_eq!(ledger.state(), RecorderState::Closed);
            assert!(!ledger.cleanup_pending());
            let report = ledger.report();
            match evidence {
                0 => ledger.dropped(1),
                1 => ledger.mark_uncertain(),
                2 => {
                    let _ = ledger.observe(Rollover, path("hidden.mkv"));
                }
                3 => {
                    let _ = ledger.observe(RecorderEventKind::FirstFrame, path("hidden.mkv"));
                }
                4 => {
                    let _ = ledger.observe(FileClosed, path("hidden.mkv"));
                }
                5 => {
                    let _ = ledger.observe(RecorderEventKind::Unknown(4321), path("hidden.mkv"));
                }
                _ => unreachable!(),
            }
            assert!(ledger.cleanup_pending(), "evidence {evidence}");
            assert!(ledger.is_uncertain());
            assert_eq!(ledger.report(), report);
            let replacement = Ledger::inheriting(report.clone());
            assert!(!replacement.cleanup_pending());
            assert_eq!(replacement.report(), report);
        }
    }

    #[test]
    fn a_wait_settles_only_on_a_final_report_or_an_expired_completed_drain() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.stop_requested(true);
        assert!(ledger.settle(None).is_none());
        assert!(matches!(
            ledger.settle(Some(&LateDrain::for_test())),
            Some(WaitOutcome::Expired(..))
        ));
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(|| false);
        assert!(matches!(
            ledger.settle(None),
            Some(WaitOutcome::Ended(SessionEnd::Closed(_)))
        ));
    }

    #[test]
    fn a_rollover_reports_the_next_file_open_before_the_previous_one_closed() {
        // Finality in `drained` and `poll_event_ms` relies on this observed
        // SDK order. The native File test
        // `rollovers_open_the_next_file_before_closing_the_previous_file`
        // pins it for explicit, source-switch and size rollovers, with and
        // without proxy recording.
        let mut ledger = session();
        let _ = ledger.observe(Started, path("first.mkv"));
        let _ = ledger.observe(Rollover, path("second.mkv"));
        assert_eq!(ledger.files().len(), 2);
        let _ = ledger.observe(FileClosed, path("first.mkv"));
        let _ = ledger.drained(|| true);
        assert_eq!(ledger.state(), RecorderState::Recording);
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("second.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert_eq!(ledger.report().closed_files(), 2);
    }

    #[test]
    fn a_rollover_reported_after_the_previous_close_is_followed_too() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("first.mkv"));
        let _ = ledger.observe(FileClosed, path("first.mkv"));
        // Still recording: the close of the first file is not the end.
        let _ = ledger.drained(|| true);
        assert_eq!(ledger.state(), RecorderState::Recording);
        let _ = ledger.observe(Rollover, path("second.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Stopping);
        let _ = ledger.observe(FileClosed, path("second.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
    }

    #[test]
    fn a_rollover_that_never_happens_does_not_hold_the_session_open() {
        // A rollover was requested and the session stopped before the cut:
        // the SDK reports only the close of the file that was open.
        let mut ledger = session();
        let _ = ledger.observe(Started, path("first.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("first.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
    }

    #[test]
    fn a_stop_the_sdk_decides_on_confirms_its_observed_close() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.observe(FormatChanged, path("take.mkv"));
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report().closure_confirmed());
    }

    #[test]
    fn a_fatal_error_closes_the_session_and_stays_in_the_report() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.observe(WriteFailed, path("take.mkv"));
        let _ = ledger.observe(Error, path("take.mkv"));
        assert_eq!(ledger.state(), RecorderState::Stopping);
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert_eq!(ledger.report().error_events(), 1);
        assert_eq!(ledger.report().write_failures(), 1);
    }

    #[test]
    fn a_failed_rollover_open_is_counted_without_opening_a_file() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("first.mkv"));
        let _ = ledger.observe(OpenFailed, path("second.mkv"));
        let _ = ledger.observe(FileClosed, path("first.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert_eq!(ledger.report().error_events(), 1);
    }

    #[test]
    fn a_session_that_stops_without_reporting_a_file_is_indeterminate_not_pending() {
        let mut ledger = session();
        let _ = ledger.stop_requested(true);
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert!(!ledger.report().closure_confirmed());
        assert_eq!(ledger.report().dropped_events(), 0);
        assert!(!ledger.cleanup_pending());
    }

    #[test]
    fn events_outside_a_session_change_nothing() {
        // A start the SDK refuses queues an open-failed event while the
        // previous session's record is still the current one.
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        let _ = ledger.observe(OpenFailed, path("next.mkv"));
        ledger.dropped(3);
        assert_eq!(ledger.report().error_events(), 0);
        assert_eq!(ledger.report().dropped_events(), 0);
        assert!(ledger.report().closure_confirmed());
    }

    #[test]
    fn a_session_stopped_on_request_was_not_stopped_by_the_sdk() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        assert!(!ledger.report().stopped_by_sdk(), "an open session");
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(|| true);
        assert!(!ledger.report().stopped_by_sdk(), "still stopping");
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(!ledger.report().stopped_by_sdk());
    }

    #[test]
    fn an_sdk_error_that_starts_the_stop_is_not_a_request() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.observe(OpenFailed, path("next.mkv"));
        let _ = ledger.observe(Error, path(""));
        assert_eq!(ledger.state(), RecorderState::Stopping);
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report().closure_confirmed());
        assert!(ledger.report().stopped_by_sdk());
    }

    #[test]
    fn a_close_at_a_format_change_with_no_request_was_the_sdks() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.observe(FormatChanged, path("take.mkv"));
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report().stopped_by_sdk());
    }

    #[test]
    fn a_request_after_the_sdk_stopped_recording_leaves_the_stop_to_the_sdk() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        // The SDK had ended the session before the request reached it.
        let _ = ledger.stop_requested(false);
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report().stopped_by_sdk());
    }

    #[test]
    fn an_indeterminate_end_with_no_request_was_the_sdks() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        ledger.dropped(1);
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert!(ledger.report().stopped_by_sdk());
    }

    #[test]
    fn a_truncated_or_repeated_open_cannot_be_matched() {
        let mut ledger = session();
        let _ = ledger.observe(
            Started,
            EventText::from_buffer(&[b'a' as std::ffi::c_char; 8]),
        );
        assert!(ledger.is_uncertain());

        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.observe(Rollover, path("take.mkv"));
        assert!(ledger.is_uncertain());
    }

    /// The state and the report's two verdicts are read from one phase, so
    /// every phase gives one consistent combination.
    #[test]
    fn final_reports_distinguish_confirmed_closure_from_lost_or_missing_files() {
        for requested in [false, true] {
            for (opened_and_closed, lost_event) in [(true, false), (true, true), (false, false)] {
                let mut ledger = session();
                if opened_and_closed {
                    let _ = ledger.observe(Started, path("take.mkv"));
                    let _ = ledger.observe(FileClosed, path("take.mkv"));
                }
                if lost_event {
                    ledger.dropped(1);
                }
                if requested || !opened_and_closed {
                    let _ = ledger.stop_requested(requested);
                }
                let _ = ledger.drained(|| false);
                let confirmed = opened_and_closed && !lost_event;
                let report = ledger.report();
                let expected = if confirmed {
                    SessionEnd::Closed(report)
                } else {
                    SessionEnd::Indeterminate(report)
                };
                assert_eq!(
                    ledger.settle(None).and_then(WaitOutcome::into_end),
                    Some(expected)
                );
                assert_eq!(ledger.report().closure_confirmed(), confirmed);
                assert_eq!(ledger.report().stopped_by_sdk(), !requested);
            }
        }
    }

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
                Ended(FinalPhase {
                    closure: Closure::Confirmed,
                    requested: false,
                }),
                RecorderState::Closed,
                true,
                true,
            ),
            (
                Ended(FinalPhase {
                    closure: Closure::Confirmed,
                    requested: true,
                }),
                RecorderState::Closed,
                true,
                false,
            ),
            (
                Ended(FinalPhase {
                    closure: Closure::Unconfirmed,
                    requested: false,
                }),
                RecorderState::Indeterminate,
                false,
                true,
            ),
            (
                Ended(FinalPhase {
                    closure: Closure::Unconfirmed,
                    requested: true,
                }),
                RecorderState::Indeterminate,
                false,
                false,
            ),
        ] {
            let ledger = Ledger::with_report(1, phase, 0, true);
            assert_eq!(ledger.state(), state, "{phase:?}");
            assert_eq!(ledger.report().closure_confirmed(), confirmed, "{phase:?}");
            assert_eq!(ledger.report().stopped_by_sdk(), by_sdk, "{phase:?}");
            // A replacement keeps the report but has no session of its own.
            let inherited = Ledger::inheriting(ledger.report());
            assert_eq!(inherited.state(), RecorderState::Idle, "{phase:?}");
            assert!(!inherited.session_open(), "{phase:?}");
            assert_eq!(inherited.report(), ledger.report(), "{phase:?}");
        }
    }

    #[test]
    fn a_stop_requested_before_an_sdk_error_stays_requested() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(Error, path("take.mkv"));
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(|| false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(!ledger.report().stopped_by_sdk());
    }
}
