//! Recording an NDI source to disk, with explicit finalization.

use std::{
    marker::PhantomData,
    ptr::NonNull,
    rc::Rc,
    time::{Duration, Instant},
};

use super::{
    out_param,
    types::{track_flags, FileText, NamePattern, ResolvedRecorderOptions},
    EventText, FileApi, FileRecorderOptions, RecorderEvent, RecorderEventKind, RecorderStats,
    RecordingTarget, RecordingTimecode, SourceFormat, SourceSpec, StateConflict,
};
#[cfg(doc)]
use super::{FileFamily, FileNaming};
use crate::{
    frames::MetadataPartsExt,
    sys::{self, bindings::*},
    timeout::{Deadline, Timeout},
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
/// A ledger's session enum produces this immutable report phase together
/// with its counts. The report's closure and stopped-by-SDK verdicts are
/// therefore snapshots of that same session state.
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
    /// The session's number, counting from one for each recorder; zero when
    /// the recorder has not started a session.
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

/// The outcome of finishing a recording session.
///
/// [`FileRecorder::finish`] and [`AsyncFileRecorder::finish`](super::AsyncFileRecorder::finish)
/// retain a report even when their completion budget expires. The corresponding
/// `wait_for_finish` methods return `None` on timeout and `Some` for a closed or
/// indeterminate session.
///
/// A synchronous recorder stays usable whatever the outcome. An async
/// recorder can be closed by another clone while the operation is running.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FinishStatus {
    /// Every file the session opened is finalized. The report still records
    /// any errors during the session, and
    /// [`stopped_by_sdk`](RecordingReport::stopped_by_sdk) says whether the
    /// recording ran until it was asked to stop or the SDK ended it sooner.
    Closed(RecordingReport),
    /// `finish`'s completion budget expired before the session ended.
    /// Call [`FileRecorder::wait_for_finish`] to keep waiting, or inspect the
    /// recorder's current report. `wait_for_finish` returns `None` for this
    /// condition, keeping the report available through the recorder.
    /// An async operation interrupted by a close reports [`StateConflict::Closed`]
    /// instead when closure cannot be established in its remaining budget.
    TimedOut(RecordingReport),
    /// The session ended, but its events do not show that every file was
    /// finalized. Waiting longer will not change this.
    Indeterminate(RecordingReport),
}

impl FinishStatus {
    /// A condition wait reports only settled outcomes; its timeout keeps the
    /// same report in the recorder's ledger for a subsequent snapshot or wait.
    pub(super) fn into_wait_outcome(self) -> Option<Self> {
        match self {
            Self::TimedOut(_) => None,
            outcome => Some(outcome),
        }
    }
}

/// A file the recorder's events reported open.
struct OpenFile {
    path: EventText,
    /// The SDK reported the first audio or video packet written to it.
    has_media: bool,
}

/// The one budget for stopping and destroying this native recorder.
/// A finish's deadline belongs to its caller and never changes this budget.
#[derive(Clone, Copy)]
enum CleanupBudget {
    Skip,
    Spent,
    For(Timeout),
}

impl CleanupBudget {
    fn begin(&mut self, requested: Instant) -> DrainBudget {
        match std::mem::replace(self, Self::Spent) {
            Self::For(wait) => {
                Deadline::starting(requested, wait).map_or(
                    // At the clock's representable limit, spend the checked
                    // duration using elapsed time instead of skipping cleanup.
                    DrainBudget::Relative {
                        start: requested,
                        wait,
                    },
                    DrainBudget::new,
                )
            }
            Self::Skip | Self::Spent => DrainBudget::Spent,
        }
    }
}

/// What initiated the owner's cleanup. Abandon can replace an earlier close
/// while that close is draining; synchronous drop never requests that change.
#[derive(Clone, Copy)]
pub(super) enum CleanupRequest {
    Drop,
    Close,
    Abandon,
}

/// A wait's deadline and its one bounded drain after expiry.
/// Reading expiry once per step keeps the native poll and settlement in agreement.
pub(super) enum DrainBudget {
    Ahead(Deadline),
    Relative { start: Instant, wait: Timeout },
    Late { polls: usize },
    Spent,
}

impl DrainBudget {
    fn new(deadline: Deadline) -> Self {
        Self::Ahead(deadline)
    }

    fn late() -> Self {
        Self::Late { polls: 0 }
    }

    fn poll(&mut self, step: Duration, poll: impl FnOnce(u32) -> bool) -> Pumped {
        self.poll_at(step, Instant::now(), poll)
    }

    /// The poll and its result belong to the same step: only this operation
    /// can spend the budget, whether the late phase is empty or reaches its limit.
    fn poll_at(&mut self, step: Duration, at: Instant, poll: impl FnOnce(u32) -> bool) -> Pumped {
        let expired = match self {
            Self::Ahead(deadline) => deadline.end() <= at,
            Self::Relative { start, wait } => wait.remaining_at(*start, at).is_zero(),
            Self::Late { .. } | Self::Spent => false,
        };
        if expired {
            *self = Self::late();
        }
        let (wait, late) = match self {
            Self::Ahead(deadline) => (deadline.step_ms_at(at, step), false),
            Self::Relative { start, wait } => (
                // The remainder is bounded by a checked Timeout even when
                // the clock cannot represent its end.
                crate::timeout::sdk_millis(wait.remaining_at(*start, at).min(step))
                    .unwrap_or(wait.millis()),
                false,
            ),
            Self::Late { .. } => (0, true),
            Self::Spent => return Pumped::Exhausted,
        };
        let empty = poll(wait);
        if let Self::Late { polls } = self {
            *polls += 1;
            if empty || *polls == STALE_EVENT_LIMIT {
                *self = Self::Spent;
            }
        }
        if empty {
            Pumped::Empty { late }
        } else {
            Pumped::Took { late }
        }
    }
}

/// Evidence from one poll, or from exhausting its bounded final drain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pumped {
    Took { late: bool },
    Empty { late: bool },
    Exhausted,
}

impl Pumped {
    fn expired(self) -> bool {
        !matches!(
            self,
            Self::Took { late: false } | Self::Empty { late: false }
        )
    }

    fn drain_over(self) -> bool {
        !matches!(self, Self::Took { .. })
    }
}

/// One fixed settlement batch. Reply callbacks cannot advance its clock.
pub(super) struct Settlement {
    pub(super) drained: bool,
    pub(super) cleanup_expired: bool,
    pub(super) at: Instant,
}

/// How a registered finish wait returns its result. Synchronous waits use a
/// stack slot; async waits use their executor-independent reply.
pub(super) trait FinishReply {
    fn cancelled(&self) -> bool;
    fn complete(self, result: Result<FinishStatus>);
}

/// A recording session belongs to exactly one native recorder generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SessionId {
    recorder: u64,
    session: u64,
}

struct FinishWaiter<R> {
    deadline: Deadline,
    session: SessionId,
    reply: R,
}

/// The finish registry lives beside the ledger whose evidence settles it.
pub(super) struct FinishWaiters<R> {
    waiters: Vec<FinishWaiter<R>>,
    // Captured in the same step that resolves waiters, including shutdown
    // initiated by an executor callback during reply delivery.
    notify_cleanup: bool,
}

impl<R: FinishReply> FinishWaiters<R> {
    pub(super) fn new() -> Self {
        Self {
            waiters: Vec::new(),
            notify_cleanup: false,
        }
    }

    pub(super) fn notifies_cleanup(&self) -> bool {
        self.notify_cleanup || !self.waiters.is_empty()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.waiters.len()
    }

    #[cfg(test)]
    pub(super) fn capacity(&self) -> usize {
        self.waiters.capacity()
    }
    pub(super) fn is_empty(&self) -> bool {
        self.waiters.is_empty()
    }

    pub(super) fn retain(&mut self) {
        self.waiters.retain(|waiter| !waiter.reply.cancelled());
    }

    pub(super) fn register(&mut self, recorder: &FileRecorder, deadline: Deadline, reply: R) {
        if let Err(error) = recorder.check_wait() {
            reply.complete(Err(error));
        } else if !reply.cancelled() {
            self.waiters.push(FinishWaiter {
                deadline,
                session: recorder.session_id(),
                reply,
            });
        }
    }

    pub(super) fn close(&mut self) {
        self.fail(StateConflict::Closed);
    }

    pub(super) fn fail(&mut self, error: StateConflict) {
        for waiter in self.waiters.drain(..) {
            waiter.reply.complete(Err(Error::InvalidState(error)));
        }
    }

    pub(super) fn wait_timeout(&self, tick: Duration) -> Duration {
        self.waiters
            .iter()
            .fold(tick, |wait, waiter| wait.min(waiter.deadline.remaining()))
    }

    /// Fix the due batch before delivering any reply: executor callbacks may
    /// take arbitrary time, so later deadlines need a fresh drain next time.
    pub(super) fn settle_at(
        &mut self,
        recorder: &mut FileRecorder,
        batch: Settlement,
        sink: &mut impl FnMut(Result<RecorderEvent>),
        closing: impl Fn() -> bool,
        mut cleanup: Option<&mut DrainBudget>,
    ) -> bool {
        let Settlement {
            mut drained,
            cleanup_expired,
            at,
        } = batch;
        self.retain();
        if self.waiters.is_empty() {
            return drained;
        }
        self.notify_cleanup |= closing();
        if self
            .waiters
            .iter()
            .any(|waiter| cleanup_expired || waiter.deadline.end() <= at)
        {
            // A pump poll before expiry is never evidence that the queue is
            // empty after expiry. Every newly due batch gets its own drain.
            let mut budget = DrainBudget::late();
            loop {
                let polled = if let Some(cleanup) = cleanup.as_deref_mut() {
                    // The local late phase and the final cleanup both account
                    // for this poll. An exhausted cleanup makes no native call.
                    budget.poll(Duration::ZERO, |_| {
                        recorder.pump(cleanup, Duration::ZERO, sink).drain_over()
                    })
                } else {
                    recorder.pump(&mut budget, Duration::ZERO, sink)
                };
                if polled.drain_over() {
                    break;
                }
            }
            drained = true;
        }
        let session = recorder.session_id();
        let mut index = 0;
        while index < self.waiters.len() {
            let waiter = &self.waiters[index];
            let outcome = if waiter.session != session {
                Some(Err(Error::InvalidState(StateConflict::Closed)))
            } else {
                recorder
                    .settle_wait(cleanup_expired || waiter.deadline.end() <= at, drained)
                    .map(|outcome| {
                        if matches!(outcome, FinishStatus::TimedOut(_)) && closing() {
                            Err(Error::InvalidState(StateConflict::Closed))
                        } else {
                            Ok(outcome)
                        }
                    })
            };
            if let Some(outcome) = outcome {
                self.waiters.swap_remove(index).reply.complete(outcome);
            } else {
                index += 1;
            }
        }
        self.notify_cleanup |= closing();
        drained
    }
}

struct LocalFinish<'a>(&'a std::cell::RefCell<Option<Result<FinishStatus>>>);

impl FinishReply for LocalFinish<'_> {
    fn cancelled(&self) -> bool {
        false
    }
    fn complete(self, result: Result<FinishStatus>) {
        *self.0.borrow_mut() = Some(result);
    }
}

/// Counts and paths belong to a session; its lifecycle determines the
/// phase when producing an immutable public report.
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
        RecordingReport {
            session: self.session,
            phase,
            closed_files: self.closed_files,
            empty_files: self.empty_files,
            error_events: self.error_events,
            write_failures: self.write_failures,
            dropped_events: self.dropped_events,
            last_closed_path: self.last_closed_path.clone(),
        }
    }

    fn into_report(self, phase: SessionPhase) -> RecordingReport {
        RecordingReport {
            session: self.session,
            phase,
            closed_files: self.closed_files,
            empty_files: self.empty_files,
            error_events: self.error_events,
            write_failures: self.write_failures,
            dropped_events: self.dropped_events,
            last_closed_path: self.last_closed_path,
        }
    }
}

/// A successful native start owes exactly one native stop, including when
/// the SDK ends recording itself. Only stop_native consumes this obligation.
#[must_use = "ending a native recording must issue its owed stop"]
struct StopOwed;

struct OpenSession {
    report: SessionReport,
    open_files: Vec<OpenFile>,
    records_media: bool,
    uncertain: bool,
    dropped_base: u64,
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
        report: RecordingReport,
        pending: Vec<OpenFile>,
    },
}

/// The record of one session, built from the recorder's events.
struct Ledger {
    session: Session,
}

impl Ledger {
    fn idle() -> Self {
        Self {
            session: Session::Idle { last: None },
        }
    }

    /// A replacement is explicitly idle while retaining the previous report.
    fn inheriting(report: RecordingReport) -> Self {
        Self {
            session: Session::Idle { last: Some(report) },
        }
    }

    fn session(session: u64, dropped_base: u64, records_media: bool) -> Self {
        Self {
            session: Session::Open {
                data: OpenSession {
                    report: SessionReport::new(session),
                    open_files: Vec::new(),
                    records_media,
                    uncertain: false,
                    dropped_base,
                },
                stop: StopOwed,
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
            SessionPhase::Closed { .. } | SessionPhase::Indeterminate { .. } => Self {
                session: Session::Ended {
                    report: SessionReport::new(session).into_report(phase),
                    pending: Vec::new(),
                },
            },
        }
    }

    fn report(&self) -> RecordingReport {
        match &self.session {
            Session::Idle { last } => last
                .clone()
                .unwrap_or_else(|| SessionReport::new(0).into_report(SessionPhase::NotStarted)),
            Session::Open { data, .. } => data.report.report(SessionPhase::Recording),
            Session::Stopping { data, requested } => data.report.report(SessionPhase::Stopping {
                requested: *requested,
            }),
            Session::Ended { report, .. } => report.clone(),
        }
    }

    fn session_number(&self) -> u64 {
        match &self.session {
            Session::Idle { last } => last.as_ref().map_or(0, |report| report.session),
            Session::Open { data, .. } | Session::Stopping { data, .. } => data.report.session,
            Session::Ended { report, .. } => report.session,
        }
    }

    fn state(&self) -> RecorderState {
        match &self.session {
            Session::Idle { .. } => RecorderState::Idle,
            Session::Open { .. } => RecorderState::Recording,
            Session::Stopping { .. } => RecorderState::Stopping,
            Session::Ended { report, .. } => report.phase.state(),
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

    fn session_open(&self) -> bool {
        matches!(
            self.session,
            Session::Open { .. } | Session::Stopping { .. }
        )
    }

    fn files(&self) -> &[OpenFile] {
        match &self.session {
            Session::Open { data, .. } | Session::Stopping { data, .. } => &data.open_files,
            Session::Ended { pending, .. } => pending,
            Session::Idle { .. } => &[],
        }
    }

    fn cleanup_pending(&self) -> bool {
        self.session_open() || !self.files().is_empty()
    }

    fn all_closed(&self) -> bool {
        self.open()
            .is_some_and(|data| data.open_files.is_empty() && data.report.closed_files > 0)
    }

    fn settle(&self, expired: bool, drained: bool) -> Option<FinishStatus> {
        match self.state() {
            RecorderState::Closed => Some(FinishStatus::Closed(self.report())),
            RecorderState::Indeterminate => Some(FinishStatus::Indeterminate(self.report())),
            _ if expired && drained => Some(FinishStatus::TimedOut(self.report())),
            _ => None,
        }
    }

    /// Moving out of Open transfers the only native-stop obligation to its
    /// caller; repeating the transition cannot mint another one.
    fn stop_requested(&mut self, requested: bool) -> Option<StopOwed> {
        let previous = std::mem::replace(&mut self.session, Session::Idle { last: None });
        match previous {
            Session::Open { data, stop } => {
                self.session = Session::Stopping { data, requested };
                Some(stop)
            }
            previous => {
                self.session = previous;
                None
            }
        }
    }

    fn mark_uncertain(&mut self) {
        if let Some(data) = self.open_mut() {
            data.uncertain = true;
        }
    }

    #[cfg(test)]
    fn is_uncertain(&self) -> bool {
        self.open().is_some_and(|data| data.uncertain)
    }

    fn dropped(&mut self, cumulative: u64) {
        let Some(data) = self.open_mut() else {
            return;
        };
        data.report.dropped_events = cumulative.saturating_sub(data.dropped_base);
        if cumulative != data.dropped_base {
            data.uncertain = true;
        }
    }

    fn observe(&mut self, kind: RecorderEventKind, path: EventText) -> Option<StopOwed> {
        let exact =
            !path.as_bytes().is_empty() && path.is_terminated() && !path.is_possibly_truncated();
        let Some(data) = self.open_mut() else {
            // Cleanup evidence cannot change a report already returned.
            if let Session::Ended { pending, .. } = &mut self.session {
                if kind == RecorderEventKind::FileClosed && exact {
                    if let Some(index) = pending.iter().position(|open| open.path == path) {
                        pending.remove(index);
                    }
                }
            }
            return None;
        };
        match kind {
            RecorderEventKind::Started | RecorderEventKind::Rollover => {
                if !exact
                    || data.open_files.iter().any(|open| open.path == path)
                    || (kind == RecorderEventKind::Started && !data.open_files.is_empty())
                {
                    data.uncertain = true;
                }
                data.open_files.push(OpenFile {
                    path,
                    has_media: false,
                });
            }
            RecorderEventKind::FirstFrame => {
                if let Some(open) = data.open_files.iter_mut().find(|open| open.path == path) {
                    open.has_media = true;
                }
            }
            RecorderEventKind::FileClosed => {
                match data.open_files.iter().position(|open| open.path == path) {
                    Some(index) if exact => {
                        let closed = data.open_files.remove(index);
                        if data.records_media && !closed.has_media {
                            data.report.empty_files = data.report.empty_files.saturating_add(1);
                        }
                    }
                    _ => data.uncertain = true,
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
            _ => {}
        }
        None
    }

    /// Finality relies on open-before-close rollover events, pinned by the
    /// native rollover-order test. Every exit transfers any owed native stop.
    fn drained(&mut self, native_recording: bool) -> Option<StopOwed> {
        if native_recording {
            return None;
        }
        let data = self.open()?;
        let requested = match self.session {
            Session::Stopping { requested, .. } => requested,
            _ => false,
        };
        let phase = if data.uncertain {
            SessionPhase::Indeterminate { requested }
        } else if data.open_files.is_empty() && data.report.closed_files > 0 {
            SessionPhase::Closed { requested }
        } else if matches!(self.session, Session::Stopping { .. }) && data.open_files.is_empty() {
            SessionPhase::Indeterminate { requested }
        } else {
            return None;
        };
        let previous = std::mem::replace(&mut self.session, Session::Idle { last: None });
        let (data, stop) = match previous {
            Session::Open { data, stop } => (data, Some(stop)),
            Session::Stopping { data, .. } => (data, None),
            _ => unreachable!("only an open session can finish"),
        };
        self.session = Session::Ended {
            report: data.report.into_report(phase),
            pending: data.open_files,
        };
        stop
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
        |text: &Option<FileText>| text.as_ref().map_or(std::ptr::null(), |text| text.as_ptr());
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
/// [`FinishStatus::Closed`]. Dropping a recorder waits up to
/// [`FileRecorderOptions::drop_wait`] while the session is open or a file it
/// reported open has not been reported closed, including after an
/// indeterminate outcome. This budget starts at drop and includes the native
/// stop; synchronous `finish` starts its event wait after the stop returns.
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
///
/// A recorder belongs to the thread that created it, so it is neither
/// [`Send`] nor [`Sync`]: the File API documents no thread on which a
/// recorder may be used or destroyed, nor whether two calls on one recorder
/// may overlap. Methods that call into the SDK may block; cached state and
/// report snapshots do not call it.
/// [`AsyncFileRecorder`](super::AsyncFileRecorder) offers the same operations
/// as futures.
pub struct FileRecorder {
    recording: &'static sys::FileRecording,
    handle: NonNull<NDIlib_file_recorder_instance_type>,
    /// The configured tracks, used when starting each new session.
    tracks: MediaTypes,
    /// The source in effect, when the last change of source was accepted.
    accepted_source: Option<SourceSpec>,
    /// How the recorder names its files.
    naming: NamePattern,
    ledger: Ledger,
    generation: u64,
    cleanup: CleanupBudget,
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

/// Rust-only creation state: its validation and family check require no
/// native object, so this value can move to an owner thread before creation.
pub(super) struct PreparedRecorder {
    api: FileApi,
    recording: &'static sys::FileRecording,
    options: CheckedRecorderOptions,
}

/// Every option needed for creation, retaining the one resolved naming proof.
/// Reset can validate this before queueing without retaining another runtime.
pub(super) struct CheckedRecorderOptions {
    source: SourceSpec,
    tracks: MediaTypes,
    planar_audio: bool,
    proxy: bool,
    rollover_size: u64,
    resolved: ResolvedRecorderOptions,
}

impl CheckedRecorderOptions {
    pub(super) fn new(options: &FileRecorderOptions) -> Result<Self> {
        let resolved = options.resolve()?;
        Ok(Self {
            source: options.source.clone(),
            tracks: options.tracks,
            planar_audio: options.planar_audio,
            proxy: options.proxy,
            rollover_size: options.rollover_size,
            resolved,
        })
    }
}

impl PreparedRecorder {
    pub(super) fn create(self) -> Result<FileRecorder> {
        let Self {
            api,
            recording,
            options,
        } = self;
        let resolved = options.resolved;
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
        let handle =
            unsafe { recording.named().NDIlib_file_recorder_create(&settings) }.created()?;
        Ok(FileRecorder {
            recording,
            handle,
            tracks: options.tracks,
            cleanup: CleanupBudget::For(resolved.drop_wait),
            accepted_source: Some(options.source),
            naming: resolved.naming,
            ledger: Ledger::idle(),
            generation: 0,
            _api: api,
            _thread: PhantomData,
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
        let recording = api.table::<sys::FileRecording>()?;
        let options = CheckedRecorderOptions::new(options)?;
        Ok(PreparedRecorder {
            api,
            recording,
            options,
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
    ///   while an earlier session is recording, stopping, or still has a
    ///   known file awaiting its closure event after an indeterminate outcome;
    ///   finish or poll it first, or [`reset`](Self::reset) it. With
    ///   [`StateConflict::EarlierSessionEvents`] when the SDK keeps reporting
    ///   events of an earlier session.
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
        if self.ledger.cleanup_pending() {
            return Err(Error::InvalidState(StateConflict::PreviousSessionOpen));
        }
        self.naming.check(target)?;
        // Events left over from an earlier session must not be counted in this one.
        self.discard_stale_events(sink)?;
        let session = self
            .ledger
            .session_number()
            .checked_add(1)
            .ok_or(Error::InvalidInput(
                crate::InputDefect::SessionCounterExhausted,
            ))?;
        let dropped = self.dropped_event_count();
        let prefix = target.prefix.as_ptr();
        // SAFETY: the recorder is live, and `prefix` points into `target`,
        // which the caller lends for longer than the call.
        let accepted = unsafe {
            match timecode {
                RecordingTimecode::Preserve => self
                    .recording
                    .named()
                    .NDIlib_file_recorder_start(self.handle.as_ptr(), prefix),
                RecordingTimecode::GenerateFrom(start) => self
                    .recording
                    .named()
                    .NDIlib_file_recorder_start_with_timecode(
                        self.handle.as_ptr(),
                        prefix,
                        start.ticks(),
                    ),
            }
        };
        accepted.performed()?;
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
                if let Some(stop) = self.ledger.stop_requested(still_recording) {
                    self.stop_native(stop);
                }
                StopRequest::Requested
            }
        }
    }

    fn stop_native(&mut self, _stop: StopOwed) {
        // SAFETY: the recorder is live and the consumed token is the one
        // obligation minted by its successful native start.
        unsafe {
            self.recording
                .NDIlib_file_recorder_stop(self.handle.as_ptr())
        };
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
        let event = self.capture(wait_ms);
        if self.ledger.session_open() {
            self.ledger.dropped(self.dropped_event_count());
        }
        let Some(event) = event.inspect_err(|_| self.ledger.mark_uncertain())? else {
            // This empty-capture/recording check relies on open-before-close
            // rollover events, pinned by the native File test
            // `rollovers_open_the_next_file_before_closing_the_previous_file`.
            if self.ledger.session_open() {
                if let Some(stop) = self.ledger.drained(self.is_recording()) {
                    self.stop_native(stop);
                }
            }
            return Ok(None);
        };
        if let Some(stop) = self.ledger.observe(
            RecorderEventKind::from_raw(event.type_),
            EventText::from_buffer(&event.p_path),
        ) {
            self.stop_native(stop);
        }
        // Decoded after the ledger is updated, so that a malformed payload
        // cannot hide the event that closes a file.
        Ok(Some(RecorderEvent::from_sdk(&event)))
    }

    /// Take the next native event, or `None` when the queue stays empty for
    /// `wait_ms` milliseconds.
    fn capture(&mut self, wait_ms: u32) -> Result<Option<NDIlib_file_recorder_event_t>> {
        out_param::capture_event(
            |event| {
                // SAFETY: the recorder is live and the helper keeps guarded
                // event storage writable through the call.
                unsafe {
                    self.recording.NDIlib_file_recorder_capture_event(
                        self.handle.as_ptr(),
                        event,
                        wait_ms,
                    )
                }
            },
            |event| event.type_,
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_none,
            "the recorder returned one event type and stored another",
        )
    }

    /// Stop the session and wait up to `timeout` for its last file to be
    /// closed.
    ///
    /// Events that arrive during the wait are consumed; their effect is in
    /// the returned report. A malformed event is consumed too: what it showed
    /// about the session is recorded as [`next_event`](Self::next_event)
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
    /// [`FinishStatus::Closed`] or [`FinishStatus::Indeterminate`] when the
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
    pub fn wait_for_finish(&mut self, timeout: Duration) -> Result<Option<FinishStatus>> {
        self.wait_until(Deadline::after(timeout)?)
            .map(FinishStatus::into_wait_outcome)
    }

    pub(super) fn wait_until(&mut self, deadline: Deadline) -> Result<FinishStatus> {
        self.check_wait()?;
        let result = std::cell::RefCell::new(None);
        let mut waiters = FinishWaiters::new();
        waiters.register(self, deadline, LocalFinish(&result));
        let mut budget = DrainBudget::new(deadline);
        loop {
            // A synchronous caller already has this final ledger. Preserve
            // its queued events for next_event rather than draining again.
            if let Some(outcome) = self.settle_wait(false, false) {
                return Ok(outcome);
            }
            waiters.settle_at(
                self,
                Settlement {
                    drained: false,
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

    pub(super) fn settle_wait(&self, expired: bool, drained: bool) -> Option<FinishStatus> {
        self.ledger.settle(expired, drained)
    }

    /// Share abandon's cleanup policy with the async owner's shutdown driver.
    pub(super) fn skip_cleanup(&mut self) {
        self.cleanup = CleanupBudget::Skip;
    }

    /// Start cleanup once, retaining an async close's original submission time.
    pub(super) fn begin_cleanup(&mut self, requested: Instant) -> DrainBudget {
        self.cleanup.begin(requested)
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
            let drained = waiters.settle_at(
                self,
                Settlement {
                    drained: polled.drain_over(),
                    cleanup_expired: polled.expired(),
                    at: Instant::now(),
                },
                sink,
                || !matches!(closing(), CleanupRequest::Drop),
                Some(&mut budget),
            );
            if polled.expired() && drained {
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
    /// events and the old session cannot be confirmed closed. The returned
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
        self.reset_with(|recorder| Self::new(&recorder._api, options))
    }

    pub(super) fn reset_checked(
        &mut self,
        options: CheckedRecorderOptions,
    ) -> Result<RecordingReport> {
        self.reset_with(|recorder| {
            PreparedRecorder {
                api: recorder._api.clone(),
                recording: recorder.recording,
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
        // SAFETY: the recorder is live for this borrow.
        unsafe {
            self.recording
                .NDIlib_file_recorder_is_recording(self.handle.as_ptr())
        }
    }

    /// How many events the SDK has discarded since the recorder was created
    /// because they were not taken in time.
    #[doc(alias = "NDIlib_file_recorder_get_dropped_event_count")]
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
        let raw = raw_source(source);
        // SAFETY: the recorder is live, and `raw` and the strings it points
        // to, owned by `source`, are valid for the call.
        let accepted = unsafe {
            self.recording
                .named()
                .NDIlib_file_recorder_set_source(self.handle.as_ptr(), &raw)
        };
        // Kept only once accepted: the copy is what the recorder retains.
        self.accepted_source = accepted.value().then(|| source.clone());
        accepted.performed()
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
        self.ledger.files().last().map(|open| &open.path)
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
        // SAFETY: the recorder is live, and the prefix points into `target`,
        // which the caller lends for longer than the call. The SDK copies it.
        let accepted = unsafe {
            self.recording
                .named()
                .NDIlib_file_recorder_rollover(self.handle.as_ptr(), target.prefix.as_ptr())
        };
        accepted.performed()
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
                .named()
                .NDIlib_file_recorder_inject_metadata(self.handle.as_ptr(), &frame)
        };
        injected.performed()
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
        out_param::checked_fill(|stats| {
            // SAFETY: the recorder is live and the helper keeps guarded stats
            // storage writable through the call.
            unsafe {
                self.recording
                    .NDIlib_file_recorder_get_stats(self.handle.as_ptr(), stats)
            }
        })
        .map(|stats| stats.map(|stats| RecorderStats::from_sdk(&stats)))
    }

    /// The format of the source, once the recorder has seen audio or video.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] when the SDK's structure is malformed.
    #[doc(alias = "NDIlib_file_recorder_get_source_format")]
    pub fn source_format(&self) -> Result<Option<SourceFormat>> {
        out_param::checked_fill(|format| {
            // SAFETY: the recorder is live and the helper keeps guarded
            // format storage writable through the call.
            unsafe {
                self.recording
                    .NDIlib_file_recorder_get_source_format(self.handle.as_ptr(), format)
            }
        })?
        .map(|format| SourceFormat::from_sdk(&format).map_err(Error::UnexpectedSdkResult))
        .transpose()
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

    #[test]
    fn recording_report_debug_names_its_closed_path() {
        let path = b"take.mkv\0\0".map(|byte| byte as std::ffi::c_char);
        let report = RecordingReport {
            session: 1,
            phase: SessionPhase::Closed { requested: true },
            closed_files: 1,
            empty_files: 0,
            error_events: 0,
            write_failures: 0,
            dropped_events: 0,
            last_closed_path: Some(EventText::from_buffer(&path)),
        };
        let debug = format!("{report:?}");
        assert!(debug.contains("\"take.mkv\""));
        assert!(!debug.contains("116, 97"));
    }

    #[test]
    fn checked_cleanup_waits_keep_their_budget_until_it_is_spent() {
        let start = Instant::now();
        for wait in [Duration::from_nanos(1), FINISH_POLL, crate::MAX_TIMEOUT] {
            let mut cleanup = CleanupBudget::For(Timeout::new(wait).unwrap());
            let mut budget = cleanup.begin(start);
            assert!(!matches!(budget, DrainBudget::Spent));
            assert_eq!(
                budget.poll_at(FINISH_POLL, start, |millis| {
                    assert!(millis > 0);
                    false
                }),
                Pumped::Took { late: false },
            );
            assert!(matches!(cleanup.begin(start), DrainBudget::Spent));
        }
    }

    #[test]
    fn an_unrepresentable_cleanup_end_spends_the_checked_wait_using_elapsed_time() {
        // Find an instant at the platform clock's upper boundary without
        // relying on a particular Instant representation.
        let now = Instant::now();
        let (mut low, mut high) = (0_u64, u64::MAX);
        while low < high {
            let middle = low + (high - low) / 2 + 1;
            if now.checked_add(Duration::from_secs(middle)).is_some() {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let start = now.checked_add(Duration::from_secs(low)).unwrap();
        let wait = Timeout::new(Duration::from_secs(2)).unwrap();
        assert!(Deadline::starting(start, wait).is_err());
        let mut budget = CleanupBudget::For(wait).begin(start);
        assert!(matches!(budget, DrainBudget::Relative { .. }));
        assert_eq!(
            budget.poll_at(FINISH_POLL, start, |millis| {
                assert_eq!(millis, 25);
                false
            }),
            Pumped::Took { late: false },
        );

        // The same fallback remains bounded when the clock can advance.
        let mut budget = DrainBudget::Relative { start: now, wait };
        assert_eq!(
            budget.poll_at(FINISH_POLL, now + Duration::from_secs(2), |millis| {
                assert_eq!(millis, 0);
                true
            }),
            Pumped::Empty { late: true },
        );
        assert!(matches!(budget, DrainBudget::Spent));
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
    fn every_drain_uses_one_expiry_read_and_one_bounded_late_phase() {
        let at = Instant::now();
        let deadline =
            Deadline::starting(at, Timeout::new(Duration::from_millis(7)).unwrap()).unwrap();
        let mut budget = DrainBudget::new(deadline);
        for (step, expected) in [(FINISH_POLL, 7), (SETTLE_POLL, 1)] {
            assert_eq!(
                budget.poll_at(step, at, |wait| {
                    assert_eq!(wait, expected);
                    false
                }),
                Pumped::Took { late: false }
            );
        }
        for _ in 0..STALE_EVENT_LIMIT {
            assert_eq!(
                budget.poll_at(FINISH_POLL, deadline.end(), |wait| {
                    assert_eq!(wait, 0);
                    false
                }),
                Pumped::Took { late: true }
            );
        }
        assert_eq!(
            budget.poll_at(FINISH_POLL, deadline.end(), |_| panic!(
                "spent budget polled"
            )),
            Pumped::Exhausted
        );
        assert_eq!(
            budget.poll_at(FINISH_POLL, at, |_| panic!("spent budget polled")),
            Pumped::Exhausted
        );
    }

    #[test]
    fn an_empty_late_poll_spends_the_budget_and_a_spent_budget_never_captures() {
        let mut budget = DrainBudget::late();
        assert_eq!(
            budget.poll(Duration::ZERO, |_| true),
            Pumped::Empty { late: true }
        );
        let device = super::super::mock::Recorder::new(super::super::mock::Closing::Never);
        device.install();
        let mut recorder = crate::file::FileRecorder::new(
            &super::super::mock::recorder_api(),
            &super::super::FileRecorderOptions::new(
                super::super::SourceSpec::new("spent budget").unwrap(),
            ),
        )
        .unwrap();
        let before = device.captures.load(std::sync::atomic::Ordering::Acquire);
        assert_eq!(
            recorder.pump(&mut budget, FINISH_POLL, &mut |_| panic!(
                "spent budget emitted"
            )),
            Pumped::Exhausted
        );
        assert_eq!(
            device.captures.load(std::sync::atomic::Ordering::Acquire),
            before
        );
    }

    #[test]
    fn closure_needs_the_matching_file_and_a_stopped_drained_recorder() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        assert!(!ledger.report().closure_confirmed());
        let _ = ledger.drained(true);
        assert_eq!(ledger.state(), RecorderState::Stopping);
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report().closure_confirmed());
    }

    #[test]
    fn a_close_for_another_file_never_confirms_the_session() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("first.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("other.mkv"));
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert!(!ledger.report().closure_confirmed());
    }

    #[test]
    fn lost_events_taint_an_otherwise_matching_close() {
        let mut ledger = Ledger::session(1, 7, true);
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        ledger.dropped(8);
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert_eq!(ledger.report().dropped_events, 1);
    }

    #[test]
    fn an_indeterminate_report_keeps_its_pending_cleanup_without_changing_the_report() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        ledger.dropped(1);
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
        assert!(ledger.cleanup_pending());
        let report = ledger.report();
        let _ = ledger.observe(FileClosed, path("other.mkv"));
        assert!(ledger.cleanup_pending());
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        assert!(!ledger.cleanup_pending());
        assert_eq!(ledger.report(), report);
    }

    #[test]
    fn a_wait_settles_only_on_a_final_report_or_an_expired_completed_drain() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.stop_requested(true);
        assert_eq!(ledger.settle(false, false), None);
        assert_eq!(ledger.settle(false, true), None);
        assert_eq!(ledger.settle(true, false), None);
        assert!(matches!(
            ledger.settle(true, true),
            Some(FinishStatus::TimedOut(_))
        ));
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(false);
        assert!(matches!(
            ledger.settle(false, false),
            Some(FinishStatus::Closed(_))
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
        let _ = ledger.drained(true);
        assert_eq!(ledger.state(), RecorderState::Recording);
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("second.mkv"));
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert_eq!(ledger.report().closed_files, 2);
    }

    #[test]
    fn a_rollover_reported_after_the_previous_close_is_followed_too() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("first.mkv"));
        let _ = ledger.observe(FileClosed, path("first.mkv"));
        // Still recording: the close of the first file is not the end.
        let _ = ledger.drained(true);
        assert_eq!(ledger.state(), RecorderState::Recording);
        let _ = ledger.observe(Rollover, path("second.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Stopping);
        let _ = ledger.observe(FileClosed, path("second.mkv"));
        let _ = ledger.drained(false);
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
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
    }

    #[test]
    fn a_stop_the_sdk_decides_on_confirms_its_observed_close() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.observe(FormatChanged, path("take.mkv"));
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(false);
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
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert_eq!(ledger.report().error_events, 1);
        assert_eq!(ledger.report().write_failures, 1);
    }

    #[test]
    fn a_failed_rollover_open_is_counted_without_opening_a_file() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("first.mkv"));
        let _ = ledger.observe(OpenFailed, path("second.mkv"));
        let _ = ledger.observe(FileClosed, path("first.mkv"));
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert_eq!(ledger.report().error_events, 1);
    }

    #[test]
    fn a_session_that_stops_without_reporting_a_file_is_indeterminate_not_pending() {
        let mut ledger = session();
        let _ = ledger.stop_requested(true);
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Indeterminate);
    }

    #[test]
    fn events_outside_a_session_change_nothing() {
        // A start the SDK refuses queues an open-failed event while the
        // previous session's record is still the current one.
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        let _ = ledger.observe(OpenFailed, path("next.mkv"));
        ledger.dropped(3);
        assert_eq!(ledger.report().error_events, 0);
        assert_eq!(ledger.report().dropped_events, 0);
        assert!(ledger.report().closure_confirmed());
    }

    #[test]
    fn a_session_stopped_on_request_was_not_stopped_by_the_sdk() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        assert!(!ledger.report().stopped_by_sdk(), "an open session");
        let _ = ledger.stop_requested(true);
        let _ = ledger.observe(FileClosed, path("take.mkv"));
        let _ = ledger.drained(true);
        assert!(!ledger.report().stopped_by_sdk(), "still stopping");
        let _ = ledger.drained(false);
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
        let _ = ledger.drained(false);
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
        let _ = ledger.drained(false);
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
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(ledger.report().stopped_by_sdk());
    }

    #[test]
    fn an_indeterminate_end_with_no_request_was_the_sdks() {
        let mut ledger = session();
        let _ = ledger.observe(Started, path("take.mkv"));
        ledger.dropped(1);
        let _ = ledger.drained(false);
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
        let _ = ledger.drained(false);
        assert_eq!(ledger.state(), RecorderState::Closed);
        assert!(!ledger.report().stopped_by_sdk());
    }
}

#[cfg(test)]
#[path = "drain_budget_tests.rs"]
mod drain_budget_tests;
