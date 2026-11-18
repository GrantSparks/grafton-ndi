//! Futures for the File API that work with any executor.
//!
//! Each player or recorder lives on a thread of its own, which runs the
//! commands sent to it and pumps the SDK's events between them. Probes, which
//! need no player, share one thread for the whole process. The deadlines of
//! event futures are kept by one timer thread for the whole process, so that a
//! command that blocks its owner thread cannot make a wait overrun.
//! Executor callbacks on these File threads may drop handles. Such a last-handle
//! drop requests shutdown without joining another File thread, so notifications
//! cannot stall behind cleanup or deadlock one another.

use super::types::{path_string, CheckedRecorderOptions};
use super::{
    EventText, FileApi, FileDuration, FileInfo, FilePlayer, FilePlayerOptions, FilePosition,
    FileRecorder, FileRecorderOptions, FinishStatus, PlayerEvent, PlayerStats, RecorderEvent,
    RecorderState, RecorderStats, RecordingReport, RecordingTarget, RecordingTimecode, SessionEnd,
    SourceFormat, SourceSpec, StateConflict, StopRequest,
};
use crate::{timeout::Deadline, Error, MetadataSource, QueueLimit, Result, Source, Timecode};
use std::{
    cell::Cell,
    collections::{BTreeMap, VecDeque},
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError},
        Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak,
    },
    task::{Context, Poll, Waker},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod owners;
use owners::{FinishResponse, PlayerOwner, RecorderFinish, RecorderOwner};

#[cfg(test)]
mod finish_admission_tests;

/// How long the owner thread waits for a command between event pumps.
const TICK: Duration = Duration::from_millis(10);

/// The largest command or event queue an owner may be given.
const MAX_CAPACITY: usize = 4096;

/// How many probes may wait for the thread that runs them.
const PROBE_CAPACITY: usize = 64;

thread_local! {
    /// Joining an owner here can stall the shared timer/probe worker, or form
    /// a cycle with another owner whose executor callbacks release handles.
    #[cfg_attr(
        target_os = "android",
        allow(
            clippy::missing_const_for_thread_local,
            reason = "Clippy 1.100 re-lints the Android OS-backed TLS expansion despite this const initializer"
        )
    )]
    static FILE_THREAD: Cell<bool> = const { Cell::new(false) };
}

fn mark_file_thread() {
    FILE_THREAD.set(true);
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The sizes of the queues between an async handle and its owner thread.
///
/// The default holds 64 pending commands and 256 undelivered events. A recorder
/// separately admits up to 64 pending finalization waits. Each
/// capacity must be from 1 through 4096; the `create_with_options`
/// functions of [`AsyncFilePlayer`] and [`AsyncFileRecorder`] refuse any
/// other with [`Error::InvalidInput`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AsyncFileOptions {
    command_capacity: usize,
    event_capacity: usize,
}

impl Default for AsyncFileOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncFileOptions {
    /// Requested pending command capacity.
    #[must_use]
    pub fn configured_command_capacity(&self) -> usize {
        self.command_capacity
    }

    /// Requested undelivered event capacity.
    #[must_use]
    pub fn configured_event_capacity(&self) -> usize {
        self.event_capacity
    }

    /// The default sizes: 64 pending commands and 256 undelivered events.
    pub fn new() -> Self {
        Self {
            command_capacity: 64,
            event_capacity: 256,
        }
    }

    /// Set how many commands may wait for the owner thread. A command
    /// submitted to a full queue fails at once with [`Error::QueueFull`] and
    /// [`QueueLimit::FileCommands`]. A request to stop is the exception: one
    /// is accepted beyond a full queue, so that a stop is not lost for want of
    /// room; see [`FileOperation`].
    ///
    /// For recorders, this also sets an independent limit on submitted
    /// [`finish`](AsyncFileRecorder::finish) and
    /// [`wait_for_finish`](AsyncFileRecorder::wait_for_finish) waits, including
    /// queued, registered and settling waits. A full wait budget reports
    /// [`QueueLimit::FileFinishWaits`], while ordinary commands, stop requests,
    /// close and abandon keep their own admission rules. Cancelling a submitted
    /// wait restores its place when the owner removes it; an accepted finish
    /// still requests the stop.
    #[must_use]
    pub fn command_capacity(mut self, capacity: usize) -> Self {
        self.command_capacity = capacity;
        self
    }

    /// Set how many events may wait to be received. When the queue is full
    /// the oldest event is discarded and counted.
    ///
    /// A recorder's session report is updated before an event enters this
    /// queue, so an event discarded here never affects what the report says.
    #[must_use]
    pub fn event_capacity(mut self, capacity: usize) -> Self {
        self.event_capacity = capacity;
        self
    }

    /// Check that both capacities are in range.
    fn validate(self) -> Result<ValidatedAsyncFileOptions> {
        for (queue, capacity) in [
            ("command", self.command_capacity),
            ("event", self.event_capacity),
        ] {
            if !(1..=MAX_CAPACITY).contains(&capacity) {
                return Err(Error::InvalidInput(crate::InputDefect::FileQueueCapacity {
                    queue,
                    capacity,
                }));
            }
        }
        Ok(ValidatedAsyncFileOptions(self))
    }
}

struct ValidatedAsyncFileOptions(AsyncFileOptions);

/// Executor state kept under a response or event queue's lock. Registration
/// clones outside that lock and rechecks readiness after the clone, which can
/// itself reenter File handles. Retired state is always handled after unlocking.
#[derive(Default)]
struct WakerSlot(Option<Waker>);

enum WakerPoll<'a, T> {
    Ready(T, Option<Waker>),
    /// `None` defers creating an event waiter until its incoming waker has
    /// been cloned. Its token/deadline reservation belongs to the queue.
    Pending(Option<&'a mut WakerSlot>),
}

impl WakerSlot {
    /// `check` runs under the state lock. It observes readiness before lending
    /// the slot, and may install an empty slot only when `register` is true.
    /// No caller has to reproduce the unlock/clone/recheck protocol.
    fn poll<S, T>(
        state: &Mutex<S>,
        waker: &Waker,
        mut check: impl for<'a> FnMut(&'a mut S, bool) -> WakerPoll<'a, T>,
    ) -> Option<T> {
        let mut incoming = None;
        let (ready, retired) = loop {
            let mut state = lock(state);
            match check(&mut state, incoming.is_some()) {
                WakerPoll::Ready(value, retired) => break (Some(value), retired),
                WakerPoll::Pending(slot) => {
                    if let Some(slot) = slot {
                        if slot
                            .0
                            .as_ref()
                            .is_some_and(|stored| stored.will_wake(waker))
                        {
                            break (None, None);
                        }
                        if let Some(waker) = incoming.take() {
                            break (None, slot.0.replace(waker));
                        }
                    }
                }
            }
            drop(state);
            incoming = Some(waker.clone());
        };
        Self::discard(retired);
        Self::discard(incoming);
        ready
    }

    fn take(&mut self) -> Option<Waker> {
        self.0.take()
    }

    /// Notify after committing state and releasing internal locks. An
    /// executor panic must not stop an owner or skip other shutdown replies.
    fn wake(waker: Option<Waker>) {
        if let Some(waker) = waker {
            let _ = catch_unwind(AssertUnwindSafe(|| waker.wake()));
        }
    }

    /// Release executor state outside internal locks, including on unwind.
    fn discard(waker: Option<Waker>) {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(waker)));
    }
}

/// Shared state exists only after an operation has been admitted.
struct Shared<T> {
    cancelled: AtomicBool,
    slot: Mutex<Slot<T>>,
    on_take: Option<Arc<dyn OnTake>>,
}

/// An internal result handoff hook. Only creation registers one, so ordinary
/// command results need no knowledge of handles or their owner threads.
trait OnTake: Send + Sync {
    fn on_take(&self);
}

enum Slot<T> {
    Pending(WakerSlot),
    Ready(Result<T>),
    Taken,
}

/// A started operation cannot contain another deferral.
enum Live<T> {
    Waiting(Arc<Shared<T>>),
    Immediate(Result<T>),
}

impl<T> Live<T> {
    fn failed(error: Error) -> Self {
        Self::Immediate(Err(error))
    }
}

enum Stage<T> {
    Deferred(Box<dyn FnOnce() -> Live<T> + Send + 'static>),
    Live(Live<T>),
    Done,
}

/// The result of a command sent to a player's or recorder's thread.
///
/// Like an `async fn`, this future submits no work until its first poll. Call
/// [`submit`](Self::submit) to submit it immediately instead. Queue admission
/// and operation deadlines start at submission, including any time spent queued.
/// Dropping an unsubmitted future does nothing. After submission, dropping an
/// ordinary command before it begins cancels it; once it has begun, dropping
/// only discards the result. Dropping a creation operation before receiving
/// its handle never waits for owner shutdown, including after creation has
/// completed. Submitted requests to stop, finish, close or abandon run
/// even if their future is dropped, as those methods say. A last-handle drop
/// waits for cleanup unless a submitted close or abandon still has a live future to report
/// completion. Dropping that close future restores the blocking drop guarantee.
///
/// An unsubmitted command does not keep an existing player or recorder alive.
/// Dropping the last handle still shuts down the owner as that handle documents;
/// later ordinary submissions fail with [`StateConflict::Closed`]. Closing remains
/// idempotent.
///
/// # The command queue
///
/// A handle's commands wait in a queue of
/// [`AsyncFileOptions::command_capacity`] places, and a command submitted to a
/// full queue fails at submission with [`Error::QueueFull`] and
/// [`QueueLimit::FileCommands`]. A request to stop is one too
/// ([`AsyncFilePlayer::stop`], [`AsyncFileRecorder::request_stop`] and
/// [`AsyncFileRecorder::finish`]), with one exception, so that a stop is not
/// lost for want of room: when the queue is full, one request to stop is
/// accepted beyond it. While that one waits, a further request to stop is
/// refused like any other command, so the queue never holds more than its
/// capacity and one. [`close`](AsyncFileRecorder::close) does not use the
/// queue.
///
/// Recorder `finish` and `wait_for_finish` submissions also share an independent
/// budget of `command_capacity` pending waits. Their waiter place lasts through
/// queueing, registration and settlement; their command place is released when
/// the command starts. A full waiter budget refuses the submission with
/// [`QueueLimit::FileFinishWaits`] before it enters the queue. It does not use
/// command places, including the reserved stop place. Closed/failed owners and
/// invalid timeouts take precedence; when both budgets are full,
/// [`QueueLimit::FileCommands`] takes precedence. Close and abandon bypass both
/// budgets.
///
/// # Errors
///
/// Besides the operation's own errors, every operation fails with
/// [`Error::QueueFull`] and [`QueueLimit::FileCommands`] when the queue
/// refuses it, as described above, and can fail with
/// [`Error::InvalidState`]: with [`StateConflict::Closed`] when the handle has
/// been closed or its thread stopped normally before carrying the
/// operation out, and with [`StateConflict::OwnerFailed`] when that thread failed
/// instead. Once the thread has failed, the operation it was running, every
/// operation queued behind it and every later one report
/// [`StateConflict::OwnerFailed`], never [`StateConflict::Closed`].
/// A submitted finish or wait, including one still queued, is settled with
/// the evidence gathered by a concurrent close. It reports `Closed` only when
/// that close ends its finalization budget without a settled session outcome.
///
/// # Panics
///
/// Polling again after completion panics.
#[must_use = "futures do nothing until polled or explicitly submitted"]
pub struct FileOperation<T> {
    // The closure and immediate result need only Send; the mutex preserves
    // Sync and unwind safety without exposing pinned access to either.
    stage: Mutex<Stage<T>>,
}

// No field is structurally pinned or lent through a pinned reference.
impl<T> Unpin for FileOperation<T> {}

impl<T> std::fmt::Debug for FileOperation<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Formatting never waits for an operation or its response slot.
        let state = self.stage.try_lock().ok().map(|stage| {
            let submitted = !matches!(*stage, Stage::Deferred(_));
            let completed = match &*stage {
                Stage::Deferred(_) => Some(false),
                Stage::Live(Live::Waiting(shared)) => shared
                    .slot
                    .try_lock()
                    .ok()
                    .map(|slot| !matches!(*slot, Slot::Pending(_))),
                Stage::Live(Live::Immediate(_)) | Stage::Done => Some(true),
            };
            (submitted, completed)
        });
        f.debug_struct("FileOperation")
            .field("submitted", &state.map(|(submitted, _)| submitted))
            .field("completed", &state.and_then(|(_, completed)| completed))
            .finish_non_exhaustive()
    }
}

impl<T> Future for FileOperation<T> {
    type Output = Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.start();
        let stage = this.stage.get_mut().unwrap_or_else(PoisonError::into_inner);
        let value = match stage {
            Stage::Live(Live::Waiting(shared)) => {
                let value = WakerSlot::poll(&shared.slot, cx.waker(), |slot, _| match slot {
                    Slot::Pending(waker) => WakerPoll::Pending(Some(waker)),
                    Slot::Ready(_) => {
                        #[expect(
                            clippy::unreachable,
                            reason = "Pending must stay borrowed for waker registration; the locked Ready slot cannot change before replacement"
                        )]
                        let Slot::Ready(value) = std::mem::replace(slot, Slot::Taken) else {
                            unreachable!()
                        };
                        WakerPoll::Ready(value, None)
                    }
                    #[expect(
                        clippy::unreachable,
                        reason = "only this operation takes the response and marks its stage Done before another poll"
                    )]
                    Slot::Taken => unreachable!("a response is taken only by its operation"),
                });
                if value.as_ref().is_some_and(Result::is_ok) {
                    if let Some(hook) = &shared.on_take {
                        hook.on_take();
                    }
                }
                value
            }
            Stage::Live(Live::Immediate(_)) => {
                #[expect(
                    clippy::unreachable,
                    reason = "Waiting must stay stored for cancellation if waker cloning unwinds; the exclusively matched Immediate stage cannot change"
                )]
                let Stage::Live(Live::Immediate(value)) = std::mem::replace(stage, Stage::Done) else {
                    unreachable!()
                };
                Some(value)
            }
            #[expect(
                clippy::panic,
                reason = "polling a completed FileOperation violates its documented Future contract"
            )]
            Stage::Done => panic!("a completed FileOperation was polled again"),
            #[expect(
                clippy::unreachable,
                reason = "start replaces Deferred with Live; a panicking submission leaves Done for cancellation"
            )]
            Stage::Deferred(_) => unreachable!("start resolves the one deferral"),
        };
        if let Some(value) = value {
            *stage = Stage::Done;
            Poll::Ready(value)
        } else {
            Poll::Pending
        }
    }
}

impl<T> Drop for FileOperation<T> {
    fn drop(&mut self) {
        let stage = self.stage.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Stage::Live(Live::Waiting(shared)) = stage {
            shared.cancelled.store(true, Ordering::Release);
            // A queued reply may outlive cancellation. Release executor state
            // outside the shared lock before releasing our shared ownership.
            let waker = match &mut *lock(&shared.slot) {
                Slot::Pending(waker) => waker.take(),
                Slot::Ready(_) | Slot::Taken => None,
            };
            WakerSlot::discard(waker);
        }
    }
}

/// One response protocol; only commands retain their owner's failure source.
struct Reply<T, F: FailureSource = NoOwner> {
    shared: Arc<Shared<T>>,
    owner: F,
}

trait FailureSource: Send + Sync + std::panic::UnwindSafe + std::panic::RefUnwindSafe {
    fn failed(&self) -> bool;
}

struct NoOwner;

impl FailureSource for NoOwner {
    fn failed(&self) -> bool {
        false
    }
}

type CommandReply<T, C> = Reply<T, Arc<Control<C>>>;

fn operation<T>() -> (Reply<T>, Live<T>) {
    operation_with_hook(None)
}

fn operation_with_hook<T>(on_take: Option<Arc<dyn OnTake>>) -> (Reply<T>, Live<T>) {
    let (shared, future) = shared_operation(on_take);
    (
        Reply {
            shared,
            owner: NoOwner,
        },
        future,
    )
}

fn shared_operation<T>(on_take: Option<Arc<dyn OnTake>>) -> (Arc<Shared<T>>, Live<T>) {
    let shared = Arc::new(Shared {
        cancelled: AtomicBool::new(false),
        slot: Mutex::new(Slot::Pending(WakerSlot::default())),
        on_take,
    });
    (Arc::clone(&shared), Live::Waiting(shared))
}

fn deliver<T>(shared: &Shared<T>, value: Result<T>) {
    let wake = {
        let mut slot = lock(&shared.slot);
        let Slot::Pending(waker) = &mut *slot else {
            return;
        };
        let wake = waker.take();
        *slot = Slot::Ready(value);
        wake
    };
    WakerSlot::wake(wake);
}

/// The error for an operation that did not run because its owner thread is
/// gone: a failed thread, or one that stopped normally.
fn stopped(failed: bool) -> Error {
    Error::InvalidState(if failed {
        StateConflict::OwnerFailed
    } else {
        StateConflict::Closed
    })
}

impl<T> FileOperation<T> {
    /// Submit this operation immediately, without waiting for its first poll.
    ///
    /// Submission never waits for the SDK. It checks queue admission and starts
    /// the operation's deadline now. Await the returned future to receive the
    /// result. Dropping it cancels an ordinary command that has not started;
    /// submitted stop, finish, close and abandon requests continue even when dropped.
    ///
    /// For example, `drop(recorder.request_stop().submit())` explicitly asks the
    /// recorder to stop without waiting for its result. Merely dropping
    /// `recorder.request_stop()` leaves the recorder running.
    #[must_use = "await the operation for its result, or explicitly drop it"]
    pub fn submit(mut self) -> Self {
        self.start();
        self
    }

    fn deferred(start: impl FnOnce() -> Live<T> + Send + 'static) -> Self {
        Self {
            stage: Mutex::new(Stage::Deferred(Box::new(start))),
        }
    }

    #[cfg(test)]
    fn live(live: Live<T>) -> Self {
        Self {
            stage: Mutex::new(Stage::Live(live)),
        }
    }

    fn start(&mut self) {
        let stage = self.stage.get_mut().unwrap_or_else(PoisonError::into_inner);
        *stage = match std::mem::replace(stage, Stage::Done) {
            Stage::Deferred(start) => Stage::Live(start()),
            started => started,
        };
    }

    #[cfg(test)]
    fn is_completed(&self) -> bool {
        match &*lock(&self.stage) {
            Stage::Deferred(_) => false,
            Stage::Live(Live::Waiting(shared)) => !matches!(*lock(&shared.slot), Slot::Pending(_)),
            Stage::Live(Live::Immediate(_)) | Stage::Done => true,
        }
    }
}

impl<T, F: FailureSource> Reply<T, F> {
    fn cancelled(&self) -> bool {
        self.shared.cancelled.load(Ordering::Acquire)
    }

    fn complete(self, value: Result<T>) {
        if !self.cancelled() {
            deliver(&self.shared, value);
        }
    }
}

impl<T, F: FailureSource> Drop for Reply<T, F> {
    fn drop(&mut self) {
        if !self.cancelled() {
            // The slot records whether completion was delivered. Commands
            // additionally retain the owner's early failure publication.
            let failed = std::thread::panicking() || self.owner.failed();
            deliver(&self.shared, Err(stopped(failed)));
        }
    }
}

/// What the deadline timer wakes: an event queue, through which it reaches
/// the waiter it was registered for.
trait Expire: Send + Sync {
    /// Wake the waiter that `token` names, if it is still the queue's waiter
    /// and its deadline is no later than `now`.
    fn expire(&self, token: &Weak<()>, now: Instant);
}

/// A registration with the [`DeadlineTimer`]: its deadline, then a number
/// that tells registrations with the same deadline apart.
type TimerKey = (Instant, u64);

/// One wait the timer is to end.
struct TimerEntry {
    queue: Weak<dyn Expire>,
    /// The token of the future that registered the wait.
    token: Weak<()>,
}

struct TimerState {
    /// Every pending registration, earliest deadline first.
    entries: BTreeMap<TimerKey, TimerEntry>,
    next_id: u64,
    /// The timer thread has been started.
    running: bool,
}

/// The thread that wakes event futures whose wait is over.
///
/// One timer serves every File event queue in the process, so that a wait
/// ends on time whatever its owner thread is doing. It is started by the
/// first wait that needs it, then sleeps until the earliest registered
/// deadline, and is woken early when a wait registers an earlier one. It
/// holds weak references only, so it keeps no queue, handle or future alive.
/// A future cancels its registration when it completes or is dropped, so a
/// wait that is over leaves nothing behind.
struct DeadlineTimer {
    state: Mutex<TimerState>,
    changed: Condvar,
}

/// The process's deadline timer.
static TIMER: DeadlineTimer = DeadlineTimer {
    state: Mutex::new(TimerState {
        entries: BTreeMap::new(),
        next_id: 0,
        running: false,
    }),
    changed: Condvar::new(),
};

impl DeadlineTimer {
    /// Arrange for `queue` to be asked to wake the waiter `token` at
    /// `deadline`, starting the timer thread if it is not running.
    ///
    /// # Errors
    ///
    /// The operating system's error when the timer thread is not running
    /// and cannot be started. Nothing is registered, and the next
    /// registration tries to start the thread again.
    fn register(
        &'static self,
        deadline: Instant,
        queue: Weak<dyn Expire>,
        token: Weak<()>,
    ) -> std::io::Result<TimerKey> {
        let mut state = lock(&self.state);
        if !state.running {
            // The thread waits for this lock, so it starts once this
            // registration is in place.
            thread::Builder::new()
                .name("ndi-file-timer".into())
                .spawn(|| self.run())?;
            state.running = true;
        }
        let key = (deadline, state.next_id);
        state.next_id = state.next_id.wrapping_add(1);
        let earliest = state
            .entries
            .first_key_value()
            .is_none_or(|(first, _)| key < *first);
        state.entries.insert(key, TimerEntry { queue, token });
        drop(state);
        if earliest {
            self.changed.notify_one();
        }
        Ok(key)
    }

    /// Forget a registration. Its deadline may already have been handled.
    fn cancel(&self, key: TimerKey) {
        lock(&self.state).entries.remove(&key);
    }

    /// The timer thread: end each registered wait once its deadline passes.
    fn run(&self) {
        mark_file_thread();
        let mut state = lock(&self.state);
        loop {
            let now = Instant::now();
            let mut due = Vec::new();
            while let Some(entry) = state.entries.first_entry() {
                if entry.key().0 > now {
                    break;
                }
                due.push(entry.remove());
            }
            if due.is_empty() {
                let next = state.entries.first_key_value().map(|(key, _)| key.0);
                state = match next {
                    Some(deadline) => {
                        self.changed
                            .wait_timeout(state, deadline.saturating_duration_since(now))
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                    None => self
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner),
                };
                continue;
            }
            // Wakers run with no lock held, so they may register again.
            drop(state);
            for entry in due {
                // A waker that panics must not stop the timer for every
                // other queue. The queue is held only for this call.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    if let Some(queue) = entry.queue.upgrade() {
                        queue.expire(&entry.token, now);
                    }
                }));
            }
            state = lock(&self.state);
        }
    }
}

struct EventWaiter {
    token: Weak<()>,
    deadline: Instant,
    // Taken on notification, then rearmed by polling. The token still reserves
    // the one waiter while its task is scheduled; bursts need only one wake.
    waker: WakerSlot,
}

struct Events<E> {
    queue: VecDeque<Result<E>>,
    capacity: usize,
    dropped: u64,
    end: Option<OwnerExit>,
    waiter: Option<EventWaiter>,
}

struct EventQueue<E>(Mutex<Events<E>>);

impl<E> EventQueue<E> {
    fn new(capacity: usize) -> Self {
        Self(Mutex::new(Events {
            queue: VecDeque::new(),
            capacity,
            dropped: 0,
            end: None,
            waiter: None,
        }))
    }

    fn push(&self, value: Result<E>) {
        let wake = {
            let mut events = lock(&self.0);
            if events.queue.len() == events.capacity {
                events.queue.pop_front();
                events.dropped = events.dropped.saturating_add(1);
            }
            events.queue.push_back(value);
            events.waiter.as_mut().and_then(|w| w.waker.take())
        };
        WakerSlot::wake(wake);
    }

    /// End every wait. A failed end overrides an ordinary stop, including
    /// one published before a native destructor panicked.
    fn close(&self, end: OwnerExit) {
        let wake = {
            let mut events = lock(&self.0);
            events.end = Some(match events.end {
                Some(OwnerExit::Failed) => OwnerExit::Failed,
                _ => end,
            });
            events.waiter.take().and_then(|mut w| w.waker.take())
        };
        WakerSlot::wake(wake);
    }
}

impl<E: Send + 'static> EventQueue<E> {
    fn wait(self: &Arc<Self>, budget: Duration) -> FileEventFuture<E> {
        // Made here, where the event type is known to be `Send`, so that the
        // future itself needs no bounds.
        let expiry: Weak<Self> = Arc::downgrade(self);
        FileEventFuture {
            events: Arc::clone(self),
            expiry: expiry as Weak<dyn Expire>,
            token: Arc::new(()),
            budget,
            state: EventWaitState::Unstarted,
        }
    }
}

impl<E: Send> Expire for EventQueue<E> {
    fn expire(&self, token: &Weak<()>, now: Instant) {
        let wake = {
            let mut events = lock(&self.0);
            events
                .waiter
                .as_mut()
                .filter(|w| w.token.ptr_eq(token) && w.deadline <= now)
                .and_then(|w| w.waker.take())
        };
        WakerSlot::wake(wake);
    }
}

/// The next event from a player or recorder, or `None` when none arrives
/// within the timeout.
///
/// The wait starts when the future is first polled, and ends on time even
/// while the player's or recorder's thread is busy with a command that
/// blocks: a timer thread shared by every handle in the process keeps the
/// deadline. One future may wait on a handle at a time; a second fails with
/// [`Error::InvalidState`] and [`StateConflict::WaiterPending`]. Once the
/// handle is closed and every queued event has been taken, the future fails
/// with [`StateConflict::Closed`], or with [`StateConflict::OwnerFailed`] when the
/// thread failed. When recorder shutdown begins with no submitted
/// [`finish`](AsyncFileRecorder::finish) or
/// [`wait_for_finish`](AsyncFileRecorder::wait_for_finish) pending, the queue
/// closes immediately. Cleanup events, including the last file-closed event,
/// are never added to that queue. Await `finish` before `close` to observe them.
/// Dropping the future leaves event pumping active.
/// Executor callbacks run on File threads; a handle's last clone dropped
/// there requests shutdown without waiting for native cleanup.
///
/// # Errors
///
/// Besides those above, [`Error::InvalidInput`] when the timeout exceeds
/// [`crate::MAX_TIMEOUT`], [`Error::UnexpectedSdkResult`] for a whole-event
/// layout or type mismatch, and [`Error::Io`] when the wait has to be
/// timed and the timer thread cannot be started; a later wait tries to start
/// it again.
/// An unreadable format is withheld through [`PlayerEvent::source_format`]
/// or [`RecorderEvent::source_format`] while the event is delivered.
///
/// # Panics
///
/// Polling again after completion panics.
#[must_use = "event futures must be polled to receive a notification"]
pub struct FileEventFuture<E> {
    events: Arc<EventQueue<E>>,
    /// `events`, as the deadline timer reaches it.
    expiry: Weak<dyn Expire>,
    token: Arc<()>,
    /// The caller's timeout, checked when the wait starts.
    budget: Duration,
    state: EventWaitState,
}

enum EventWaitState {
    Unstarted,
    Waiting { deadline: Deadline },
    Registered { deadline: Deadline, timer: TimerKey },
    Done,
}

impl<E> std::fmt::Debug for FileEventFuture<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match self.state {
            EventWaitState::Unstarted => "Unstarted",
            EventWaitState::Waiting { .. } => "Waiting",
            EventWaitState::Registered { .. } => "Registered",
            EventWaitState::Done => "Done",
        };
        f.debug_struct("FileEventFuture")
            .field("timeout", &self.budget)
            .field("state", &state)
            .finish_non_exhaustive()
    }
}

impl<E> FileEventFuture<E> {
    /// Complete with `value`, giving up the timer registration.
    fn complete(&mut self, value: Result<Option<E>>) -> Poll<Result<Option<E>>> {
        self.finish_wait();
        Poll::Ready(value)
    }

    fn finish_wait(&mut self) {
        if let EventWaitState::Registered { timer, .. } =
            std::mem::replace(&mut self.state, EventWaitState::Done)
        {
            TIMER.cancel(timer);
        }
    }

    /// Stop being the queue's waiter and return executor state for disposal
    /// after unlocking. An installed waiter belongs to a live future, or to
    /// a leaked future whose token stays live. Another future's waiter is
    /// never removed.
    fn release_waiter(&self) -> Option<Waker> {
        let token = Arc::downgrade(&self.token);
        let mut events = lock(&self.events.0);
        if events
            .waiter
            .as_ref()
            .is_some_and(|w| w.token.ptr_eq(&token))
        {
            events.waiter.take().and_then(|mut w| w.waker.take())
        } else {
            None
        }
    }
}

impl<E> Future for FileEventFuture<E> {
    type Output = Result<Option<E>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // The wait starts at the first poll, which is also where a timeout
        // out of range is reported, before anything else.
        let deadline = match this.state {
            EventWaitState::Waiting { deadline } | EventWaitState::Registered { deadline, .. } => {
                deadline
            }
            EventWaitState::Unstarted => match Deadline::after(this.budget) {
                Ok(deadline) => {
                    this.state = EventWaitState::Waiting { deadline };
                    deadline
                }
                Err(error) => return this.complete(Err(error)),
            },
            #[expect(
                clippy::panic,
                reason = "polling a completed FileEventFuture violates its documented Future contract"
            )]
            EventWaitState::Done => panic!("a completed FileEventFuture was polled again"),
        };
        let token = Arc::downgrade(&this.token);
        let ready = WakerSlot::poll(&this.events.0, cx.waker(), |events, register| {
            if events
                .waiter
                .as_ref()
                .is_some_and(|w| !w.token.ptr_eq(&token))
            {
                return WakerPoll::Ready(
                    Err(Error::InvalidState(StateConflict::WaiterPending)),
                    None,
                );
            }
            let ready = if let Some(event) = events.queue.pop_front() {
                Some(event.map(Some))
            } else if let Some(end) = events.end {
                Some(Err(stopped(end == OwnerExit::Failed)))
            } else if deadline.is_expired() {
                Some(Ok(None))
            } else {
                None
            };
            if let Some(value) = ready {
                return WakerPoll::Ready(
                    value,
                    events.waiter.take().and_then(|mut w| w.waker.take()),
                );
            }
            if register {
                events.waiter.get_or_insert_with(|| EventWaiter {
                    token: Weak::clone(&token),
                    deadline: deadline.end(),
                    waker: WakerSlot::default(),
                });
            }
            WakerPoll::Pending(events.waiter.as_mut().map(|waiter| &mut waiter.waker))
        });
        if let Some(value) = ready {
            return this.complete(value);
        }
        // The deadline never changes, so one registration serves every poll.
        // It is made with no queue lock held: the timer may wake this future
        // at once, which then finds itself registered as the waiter.
        if matches!(this.state, EventWaitState::Waiting { .. }) {
            match TIMER.register(deadline.end(), Weak::clone(&this.expiry), token) {
                Ok(timer) => this.state = EventWaitState::Registered { deadline, timer },
                Err(error) => {
                    WakerSlot::discard(this.release_waiter());
                    return this.complete(Err(Error::Io(error)));
                }
            }
        }
        Poll::Pending
    }
}

impl<E> Drop for FileEventFuture<E> {
    fn drop(&mut self) {
        let waker = self.release_waiter();
        self.finish_wait();
        WakerSlot::discard(waker);
    }
}

/// Why an owner stopped. Failure is published before native cleanup, while
/// success is known only once destruction completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnerExit {
    Closed,
    Failed,
}

impl OwnerExit {
    fn result(self) -> Result<()> {
        match self {
            Self::Closed => Ok(()),
            Self::Failed => Err(stopped(true)),
        }
    }
}

/// A close's request and successful result belong to its native owner.
/// Players use `()` for both, so recorder abandonment is unrepresentable.
trait CloseOutcome:
    Clone + Send + Sync + std::panic::UnwindSafe + std::panic::RefUnwindSafe + 'static
{
    type Request: Copy + Send;
    fn initial_request() -> Self::Request;
}

impl CloseOutcome for () {
    type Request = ();
    fn initial_request() {}
}

impl CloseOutcome for RecordingReport {
    type Request = super::recorder::CleanupRequest;
    fn initial_request() -> Self::Request {
        super::recorder::CleanupRequest::Close
    }
}

/// Replies share cancellation and delivery through `Reply`, regardless of
/// whether the caller wants the owner's result or just destruction completion.
enum CloseReply<C> {
    Close(Reply<()>),
    Outcome(Reply<C>),
}

impl<C: Clone> CloseReply<C> {
    fn cancelled(&self) -> bool {
        match self {
            Self::Close(reply) => reply.cancelled(),
            Self::Outcome(reply) => reply.cancelled(),
        }
    }

    fn complete(self, outcome: &std::result::Result<C, OwnerExit>) {
        match self {
            Self::Close(reply) => reply.complete(match outcome {
                Ok(_) => Ok(()),
                Err(exit) => exit.result(),
            }),
            Self::Outcome(reply) => reply.complete(
                outcome
                    .clone()
                    .map_err(|exit| stopped(exit == OwnerExit::Failed)),
            ),
        }
    }
}

struct CloseQueue<C: CloseOutcome> {
    request: C::Request,
    replies: Vec<CloseReply<C>>,
}

struct Control<C: CloseOutcome = ()> {
    /// Creation has returned its handle to the caller. Only a claimed handle
    /// can make its last ordinary drop wait for finalization.
    claimed: AtomicBool,
    /// The first close or last-handle drop starts the one finalization budget.
    shutdown_at: OnceLock<Instant>,
    /// Early failure is visible before native cleanup starts. This does not
    /// signal destruction completion; only `closed` publishes that outcome.
    outcome: OnceLock<OwnerExit>,
    /// Only the close protocol depends on the owner's associated result.
    closing: Mutex<CloseQueue<C>>,
    /// The published outcome is the only signal that cleanup completed.
    /// It is immutable and borrowable after destruction, so
    /// ordinary closes do not clone a recorder report just to discard it.
    closed: OnceLock<std::result::Result<C, OwnerExit>>,
}

impl<C: CloseOutcome> OnTake for Control<C> {
    fn on_take(&self) {
        self.claimed.store(true, Ordering::Release);
    }
}

impl<C: CloseOutcome> FailureSource for Arc<Control<C>> {
    fn failed(&self) -> bool {
        self.as_ref().failed()
    }
}

impl Control {
    #[cfg(test)]
    fn new() -> Self {
        Self::with_outcome()
    }
}

impl Control<RecordingReport> {
    fn request_abandon(
        self: &Arc<Self>,
        wake: impl FnOnce() + Send + 'static,
    ) -> FileOperation<RecordingReport> {
        let control = Arc::clone(self);
        FileOperation::deferred(move || {
            let requested = Instant::now();
            let (reply, future) = operation();
            control.register_close(
                requested,
                CloseReply::Outcome(reply),
                |request| {
                    *request = super::recorder::CleanupRequest::Abandon;
                },
                wake,
            );
            future
        })
    }
}

impl<C: CloseOutcome> Control<C> {
    fn with_outcome() -> Self {
        Self {
            claimed: AtomicBool::new(false),
            shutdown_at: OnceLock::new(),
            outcome: OnceLock::new(),
            closing: Mutex::new(CloseQueue {
                request: C::initial_request(),
                replies: Vec::new(),
            }),
            closed: OnceLock::new(),
        }
    }

    fn command<T>(self: &Arc<Self>) -> (CommandReply<T, C>, Live<T>) {
        let (shared, future) = shared_operation(None);
        (
            Reply {
                shared,
                owner: Arc::clone(self),
            },
            future,
        )
    }

    /// Refuse work once the owner failed or shutdown was requested. Failure
    /// takes precedence, including while failed native cleanup is running.
    fn gate(&self) -> Result<()> {
        let failed = self.failed();
        if failed || self.shutdown_requested() {
            Err(stopped(failed))
        } else {
            Ok(())
        }
    }

    /// Ask the owner thread to end, and complete once it has destroyed its
    /// object. Every close, including one made while another is awaited,
    /// completes then.
    fn request_close(self: &Arc<Self>, wake: impl FnOnce() + Send + 'static) -> FileOperation<()> {
        let control = Arc::clone(self);
        FileOperation::deferred(move || control.request_close_now(wake))
    }

    fn request_close_now(&self, wake: impl FnOnce()) -> Live<()> {
        self.request_close_at(Instant::now(), wake)
    }

    fn request_close_at(&self, requested: Instant, wake: impl FnOnce()) -> Live<()> {
        let (reply, future) = operation();
        self.register_close(requested, CloseReply::Close(reply), |_| {}, wake);
        future
    }

    fn register_close(
        &self,
        requested: Instant,
        reply: CloseReply<C>,
        update: impl FnOnce(&mut C::Request),
        wake: impl FnOnce(),
    ) {
        let mut queue = lock(&self.closing);
        if let Some(closed) = self.closed.get() {
            drop(queue);
            reply.complete(closed);
        } else {
            queue.replies.retain(|reply| !reply.cancelled());
            queue.replies.push(reply);
            update(&mut queue.request);
            // Publish admission, the close request and the first budget
            // together. The owner reads its request through this same lock.
            let first = self.shutdown_at.set(requested).is_ok();
            drop(queue);
            if first {
                wake();
            }
        }
    }

    fn close_request(&self) -> C::Request {
        lock(&self.closing).request
    }

    /// A live close reports destruction instead of the last handle joining.
    fn needs_cleanup_join(&self) -> bool {
        let queue = lock(&self.closing);
        self.closed.get().is_none() && queue.replies.iter().all(CloseReply::cancelled)
    }

    fn request_shutdown(&self) -> bool {
        self.request_shutdown_at(Instant::now())
    }

    fn request_shutdown_at(&self, requested: Instant) -> bool {
        // An admitted finish is enqueued before shutdown can publish.
        let _submission = lock(&self.closing);
        self.shutdown_at.set(requested).is_err()
    }

    fn submit_finish<T>(&self, enqueue: impl FnOnce() -> Live<T>) -> Live<T> {
        let _submission = lock(&self.closing);
        // Worker::enqueue checks the owner gate while this lock excludes
        // close publication, then retains it through command delivery.
        enqueue()
    }

    fn shutdown_requested(&self) -> bool {
        self.shutdown_at.get().is_some()
    }

    fn shutdown_requested_at(&self) -> Option<Instant> {
        self.shutdown_at.get().copied()
    }

    fn failed(&self) -> bool {
        self.outcome.get() == Some(&OwnerExit::Failed)
    }

    fn publish_failure(&self) {
        let _ = self.outcome.set(OwnerExit::Failed);
    }

    /// Complete replies only after destruction, using the value returned by
    /// shutdown. An early failure remains authoritative over a cleanup result.
    fn exit<E>(&self, closed: std::result::Result<C, OwnerExit>, events: &EventQueue<E>) {
        let exit = *self.outcome.get_or_init(|| {
            closed
                .as_ref()
                .map_or_else(|exit| *exit, |_| OwnerExit::Closed)
        });
        let closed = if exit == OwnerExit::Failed {
            Err(exit)
        } else {
            closed
        };
        let closed = self.closed.get_or_init(|| closed);
        events.close(exit);
        // A close either queues before this lock is acquired or observes the
        // published outcome under the same lock and completes directly.
        let replies = std::mem::take(&mut lock(&self.closing).replies);
        for reply in replies {
            reply.complete(closed);
        }
    }
}

/// A native object that lives on an owner thread. Dropping it there performs
/// its native cleanup.
trait Owner: 'static {
    type Event: Send + 'static;
    type Finish: FinishRequest;
    type Closed: CloseOutcome;

    /// Take the next native event without waiting.
    fn poll_native(&mut self) -> Result<Option<Self::Event>>;

    /// Settle work that is waiting for the ledger, after publishing events.
    fn after_events(
        &mut self,
        _events: &EventQueue<Self::Event>,
        _control: &Control<Self::Closed>,
    ) {
    }

    fn wait_timeout(&self) -> Duration {
        TICK
    }

    /// Fail registered work before the native cleanup of a panicked owner.
    fn fail_pending(&mut self) {}

    /// Run the finish protocol specific to this owner.
    fn finish_request(&mut self, finish: Self::Finish);

    /// Finish registered work and cleanup within the original close budget.
    fn shutdown(
        &mut self,
        requested: Instant,
        events: &EventQueue<Self::Event>,
        control: &Control<Self::Closed>,
        queued: &mut [Job<Self>],
    ) -> Self::Closed
    where
        Self: Sized;
}

/// Submission state follows the finish family: players have no finish request
/// and need no waiter budget, while recorders retain one shared budget.
trait FinishRequest: Send + 'static {
    type Admission: Send + Sync + 'static;

    fn admission(capacity: usize) -> Self::Admission;
}

impl FinishRequest for std::convert::Infallible {
    type Admission = ();

    fn admission(_: usize) {}
}

/// The native operations common to the two documented async handles. Keep
/// these separate from Owner's event-pump contract so inert test owners need
/// implement only the lifecycle they exercise.
trait HandleOwner: Owner {
    type Options: Clone + Send + 'static;
    const THREAD_NAME: &'static str;

    type Prepared: Send + 'static;

    fn prepare(api: FileApi, options: &Self::Options) -> Result<Self::Prepared>;
    fn create(prepared: Self::Prepared) -> Result<Self>
    where
        Self: Sized;
    fn dropped_event_count(&self) -> u64;
    fn source_format(&self) -> Result<Option<SourceFormat>>;
}

/// Finish requests retain their deadline and reply even if shutdown preempts
/// ordinary command execution. Wake messages carry no admission place.
enum Job<H: Owner> {
    Run(Box<dyn FnOnce(&mut H) + Send + 'static>),
    Finish { finish: H::Finish, place: Place },
    Wake,
}

impl<H: Owner> Job<H> {
    fn run(self, owner: &mut H) {
        match self {
            Self::Run(work) => work(owner),
            Self::Finish { finish, place } => {
                drop(place);
                owner.finish_request(finish);
            }
            Self::Wake => {}
        }
    }

    fn finish(&self) -> Option<&H::Finish> {
        match self {
            Self::Finish { finish, .. } => Some(finish),
            Self::Run(_) | Self::Wake => None,
        }
    }

    /// Called by cleanup only after it has requested the native stop. Ordinary
    /// commands remain held until destruction publishes the owner's outcome.
    fn take_finish(&mut self) -> Option<(Place, H::Finish)> {
        match std::mem::replace(self, Self::Wake) {
            Self::Finish { finish, place } => Some((place, finish)),
            other => {
                *self = other;
                None
            }
        }
    }
}

/// A bounded budget whose places move with the work that owns them. Command
/// queues release places when commands start; finish replies retain separate
/// places until settlement or cancellation cleanup.
///
/// For command admission, every queued command counts, requests to stop included. When the queue is
/// full, one request to stop is admitted beyond it, so that a stop is not
/// lost for want of room; while it waits, a further request to stop is
/// refused like any other command. The queue therefore never holds more than
/// `capacity + 1` commands. Waiter admission never uses this reserved stop place.
struct Admission {
    // The low bit owns the extra stop place; the remaining bits count normal
    // places. One atomic word makes admission and release of either kind
    // indivisible with respect to the queue limit.
    state: AtomicUsize,
    capacity: usize,
}

impl Admission {
    fn new(capacity: usize) -> Self {
        Self {
            state: AtomicUsize::new(0),
            capacity,
        }
    }

    /// A place in the queue for a command, or a request to stop if `stop`.
    ///
    /// # Errors
    ///
    /// [`Error::QueueFull`] with [`QueueLimit::FileCommands`] when the queue
    /// is full and, for a request to stop, another one already waits beyond it.
    fn admit(self: &Arc<Self>, stop: bool) -> Result<Place> {
        self.admit_for(stop, QueueLimit::FileCommands)
    }

    fn admit_for(self: &Arc<Self>, stop: bool, limit: QueueLimit) -> Result<Place> {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            let (next, reserve) = if state >> 1 < self.capacity {
                (state + 2, false)
            } else if stop && state & 1 == 0 {
                (state | 1, true)
            } else {
                return Err(Error::QueueFull(limit));
            };
            match self
                .state
                .compare_exchange_weak(state, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    return Ok(Place {
                        admission: Arc::clone(self),
                        reserve,
                    });
                }
                Err(current) => state = current,
            }
        }
    }
}

/// A move-only place in one admission budget. Queued commands give theirs up
/// when they start or are dropped; finish replies keep theirs through queueing,
/// registration and settlement, or until cancelled work is removed.
struct Place {
    admission: Arc<Admission>,
    /// The place is the one beyond the capacity.
    reserve: bool,
}

impl Drop for Place {
    fn drop(&mut self) {
        if self.reserve {
            let previous = self.admission.state.fetch_and(!1, Ordering::AcqRel);
            debug_invariant!(previous & 1 != 0);
        } else {
            let previous = self.admission.state.fetch_sub(2, Ordering::AcqRel);
            debug_invariant!(previous >> 1 != 0);
        }
    }
}

/// The handle side of an owner thread, shared by every clone of an async handle.
struct Worker<H: Owner> {
    sender: Sender<Job<H>>,
    admission: Arc<Admission>,
    // Only recorder finish requests carry a separate waiter budget.
    finish: <H::Finish as FinishRequest>::Admission,
    control: Arc<Control<H::Closed>>,
    events: Arc<EventQueue<H::Event>>,
    // JoinHandle's shared return packet is not RefUnwindSafe. This owner
    // never examines it after unwinding: the handle is installed once, then
    // taken only with &mut self and consumed by Drop's join. Assert that local
    // boundary, preserving the handles' existing public unwind auto traits.
    thread: OnceLock<AssertUnwindSafe<JoinHandle<()>>>,
}

impl<H: Owner> Drop for Worker<H> {
    fn drop(&mut self) {
        // A handle received by the caller waits on its plain last drop: the
        // native object is destroyed and a recorder's file finalized first.
        // A handle still in its creation response cannot be closed by the
        // caller, so cancellation only requests shutdown and detaches.
        let closing = self.control.request_shutdown();
        if !closing {
            // Wake the channel wait without taking an admission place. The
            // published request remains authoritative if the owner is busy.
            let _ = self.sender.send(Job::Wake);
        }
        let Some(thread) = self.thread.take() else {
            return;
        };
        // A File thread may release this last handle through an executor's
        // waker callback. It must not stall the timer/probe thread or join
        // an owner that could join it back while releasing another handle.
        if self.control.claimed.load(Ordering::Acquire)
            && !FILE_THREAD.get()
            && self.control.needs_cleanup_join()
        {
            // A panic on the owner thread was already reported to its commands.
            let _ = thread.0.join();
        }
    }
}

impl<H: Owner> Worker<H> {
    /// Queue `work` for the owner thread.
    ///
    /// A submitted request to stop (`stop`) runs even if its future is dropped,
    /// and is admitted beyond a full queue as [`Admission`] describes.
    fn submit<T: Send + 'static>(
        &self,
        stop: bool,
        work: impl FnOnce(&mut H) -> Result<T> + Send + 'static,
    ) -> FileOperation<T> {
        self.prepare(move || Ok(work), stop)
    }

    /// Prepare work only when submitted, so validation and deadlines follow the
    /// same initiation contract as queue admission.
    fn prepare<T: Send + 'static, W>(
        &self,
        prepare: impl FnOnce() -> Result<W> + Send + 'static,
        stop: bool,
    ) -> FileOperation<T>
    where
        W: FnOnce(&mut H) -> Result<T> + Send + 'static,
    {
        // Keep submission state, not Worker: an unpolled future must not
        // delay last-handle shutdown or make its own drop join the owner.
        let sender = self.sender.clone();
        let admission = Arc::clone(&self.admission);
        let control = Arc::clone(&self.control);
        FileOperation::deferred(move || {
            Self::enqueue(
                &sender,
                &admission,
                &control,
                stop,
                prepare,
                || Ok(()),
                |work, (), reply, place| {
                    Job::Run(Box::new(move |owner| {
                        // The command leaves the queue as it starts.
                        drop(place);
                        if !stop && reply.cancelled() {
                            return;
                        }
                        reply.complete(work(owner));
                    }))
                },
            )
        })
    }

    /// Gate, validate, admit and deliver every command through the same
    /// owner-bound reply. Recorder finish calls hold Control::submit_finish's
    /// `closing` lock across this kernel, so close cannot overtake delivery.
    /// The optional extra admission runs after the command place is taken and
    /// before the reply is allocated; either refusal releases earlier places.
    fn enqueue<T, P, A>(
        sender: &Sender<Job<H>>,
        admission: &Arc<Admission>,
        control: &Arc<Control<H::Closed>>,
        stop: bool,
        prepare: impl FnOnce() -> Result<P>,
        extra: impl FnOnce() -> Result<A>,
        build: impl FnOnce(P, A, CommandReply<T, H::Closed>, Place) -> Job<H>,
    ) -> Live<T> {
        if let Err(error) = control.gate() {
            return Live::failed(error);
        }
        let prepared = match prepare() {
            Ok(prepared) => prepared,
            Err(error) => return Live::failed(error),
        };
        // Preparation may race shutdown. Preserve the prepared-command path's
        // recheck before queue admission; finish still holds `closing` here.
        if let Err(error) = control.gate() {
            return Live::failed(error);
        }
        let place = match admission.admit(stop) {
            Ok(place) => place,
            Err(error) => return Live::failed(error),
        };
        let extra = match extra() {
            Ok(extra) => extra,
            Err(error) => return Live::failed(error),
        };
        let (reply, future) = control.command();
        if sender.send(build(prepared, extra, reply, place)).is_err() {
            return Live::failed(stopped(control.failed()));
        }
        future
    }

    fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut H) -> Result<T> + Send + 'static,
    ) -> FileOperation<T> {
        self.submit(false, work)
    }

    fn close(&self) -> FileOperation<()> {
        let sender = self.sender.clone();
        self.control.request_close(move || {
            // This no-op wake bypasses queue admission, so even a full queue
            // cannot delay shutdown until the next tick.
            let _ = sender.send(Job::Wake);
        })
    }
}

impl Worker<RecorderOwner> {
    fn finish<T: Send + 'static>(
        &self,
        timeout: Duration,
        stop: bool,
        finish_response: fn(CommandReply<T, RecordingReport>, Place) -> FinishResponse,
    ) -> FileOperation<T> {
        let sender = self.sender.clone();
        let admission = Arc::clone(&self.admission);
        let finish = Arc::clone(&self.finish);
        let control = Arc::clone(&self.control);
        FileOperation::deferred(move || {
            // Start at submission, including contention with another close or
            // finish. The kernel checks this result after the owner gate.
            let deadline = Deadline::after(timeout);
            control.submit_finish(|| {
                Self::enqueue(
                    &sender,
                    &admission,
                    &control,
                    stop,
                    || deadline,
                    || finish.admit_for(false, QueueLimit::FileFinishWaits),
                    |deadline, waiter, reply, place| Job::Finish {
                        finish: RecorderFinish {
                            deadline,
                            stop,
                            reply: finish_response(reply, waiter),
                        },
                        place,
                    },
                )
            })
        })
    }
}

/// The shared async-handle implementation lives on the existing worker:
/// public handles remain documented forwarding methods over one Arc, with
/// no additional allocation or change to their lifetime behavior.
impl<H: HandleOwner> Worker<H> {
    fn create<T: Send + 'static>(
        api: &FileApi,
        options: &H::Options,
        wrap: impl FnOnce(Arc<Self>) -> T + Send + 'static,
    ) -> FileOperation<T> {
        Self::create_with_options(api, options, &AsyncFileOptions::new(), wrap)
    }

    fn create_with_options<T: Send + 'static>(
        api: &FileApi,
        options: &H::Options,
        asynchronous: &AsyncFileOptions,
        wrap: impl FnOnce(Arc<Self>) -> T + Send + 'static,
    ) -> FileOperation<T> {
        let (api, options, asynchronous) = (api.clone(), options.clone(), *asynchronous);
        FileOperation::deferred(move || {
            let asynchronous = match asynchronous.validate() {
                Ok(options) => options,
                Err(error) => return Live::failed(error),
            };
            let prepared = match H::prepare(api, &options) {
                Ok(prepared) => prepared,
                Err(error) => return Live::failed(error),
            };
            spawn_prepared_owner_now(
                H::THREAD_NAME,
                move || H::create(prepared),
                asynchronous,
                wrap,
            )
        })
    }

    fn next_event(&self, timeout: Duration) -> FileEventFuture<H::Event> {
        self.events.wait(timeout)
    }

    fn dropped_event_count(&self) -> FileOperation<u64> {
        self.call(|owner| Ok(owner.dropped_event_count()))
    }

    fn notification_dropped_count(&self) -> u64 {
        lock(&self.events.0).dropped
    }

    fn source_format(&self) -> FileOperation<Option<SourceFormat>> {
        self.call(|owner| owner.source_format())
    }
}

/// Start an owner thread, create its native object there, and return the
/// handle `wrap` builds around the worker.
///
/// Options out of range fail the operation before anything is started.
#[cfg(test)]
fn spawn_owner<H: Owner, T: Send + 'static>(
    name: &str,
    create: impl FnOnce() -> Result<H> + Send + 'static,
    options: AsyncFileOptions,
    wrap: impl FnOnce(Arc<Worker<H>>) -> T + Send + 'static,
) -> FileOperation<T> {
    let name = name.to_owned();
    FileOperation::deferred(move || spawn_owner_now(&name, create, options, wrap))
}

#[cfg(test)]
fn spawn_owner_now<H: Owner, T: Send + 'static>(
    name: &str,
    create: impl FnOnce() -> Result<H> + Send + 'static,
    options: AsyncFileOptions,
    wrap: impl FnOnce(Arc<Worker<H>>) -> T + Send + 'static,
) -> Live<T> {
    let options = match options.validate() {
        Ok(options) => options,
        Err(error) => return Live::failed(error),
    };
    spawn_prepared_owner_now(name, create, options, wrap)
}

fn spawn_prepared_owner_now<H: Owner, T: Send + 'static>(
    name: &str,
    create: impl FnOnce() -> Result<H> + Send + 'static,
    options: ValidatedAsyncFileOptions,
    wrap: impl FnOnce(Arc<Worker<H>>) -> T + Send + 'static,
) -> Live<T> {
    let options = options.0;
    let (sender, receiver) = mpsc::channel();
    let control = Arc::new(Control::<H::Closed>::with_outcome());
    let events = Arc::new(EventQueue::new(options.event_capacity));
    let worker = Arc::new(Worker {
        sender,
        admission: Arc::new(Admission::new(options.command_capacity)),
        finish: H::Finish::admission(options.command_capacity),
        control: Arc::clone(&control),
        events: Arc::clone(&events),
        thread: OnceLock::new(),
    });
    let (ready, future) = operation_with_hook(Some(control.clone()));
    let thread_worker = Arc::clone(&worker);
    let thread_control = Arc::clone(&control);
    let thread_events = Arc::clone(&events);
    let spawned = thread::Builder::new().name(name.into()).spawn(move || {
        mark_file_thread();
        // Creation and use are separate unwind regions so the owner stays
        // alive while its failure is published and its waiters are resolved.
        let created = catch_unwind(AssertUnwindSafe(|| {
            if ready.cancelled() {
                return None;
            }
            match create() {
                Ok(owner) => Some((ready, owner)),
                Err(error) => {
                    ready.complete(Err(error));
                    None
                }
            }
        }));
        let mut queued = Vec::new();
        let closed = match created {
            Ok(Some((ready, mut owner))) => {
                let running = catch_unwind(AssertUnwindSafe(|| {
                    ready.complete(Ok(wrap(thread_worker)));
                    let dequeued =
                        run_owner(&mut owner, &receiver, &thread_control, &thread_events);
                    queued.extend(dequeued.into_iter().chain(receiver.try_iter()));
                    let closed = owner.shutdown(
                        thread_control
                            .shutdown_requested_at()
                            .unwrap_or_else(Instant::now),
                        &thread_events,
                        &thread_control,
                        &mut queued,
                    );
                    thread_events.close(OwnerExit::Closed);
                    closed
                }));
                let mut closed = match running {
                    Ok(closed) => Ok(closed),
                    Err(_) => {
                        thread_control.publish_failure();
                        // A second failure cannot skip event closure or destruction.
                        let _ = catch_unwind(AssertUnwindSafe(|| owner.fail_pending()));
                        thread_events.close(OwnerExit::Failed);
                        Err(OwnerExit::Failed)
                    }
                };
                // H need not be Send: every native operation and destruction
                // remain on this thread, after failure notification. The typed
                // shutdown result is retained until native destruction returns.
                if catch_unwind(AssertUnwindSafe(|| drop(owner))).is_err() {
                    thread_control.publish_failure();
                    closed = Err(OwnerExit::Failed);
                }
                closed
            }
            // No native owner or public handle exists in these creation paths.
            Ok(None) => Err(OwnerExit::Closed),
            Err(_) => Err(OwnerExit::Failed),
        };
        // The outcome is recorded before queued commands drop; awaited closes
        // complete here, only once the native object has been destroyed.
        thread_control.exit(closed, &thread_events);
        drop(queued);
        drop(receiver);
    });
    match spawned {
        Ok(thread) => {
            let _ = worker.thread.set(AssertUnwindSafe(thread));
        }
        Err(error) => {
            control.exit(Err(OwnerExit::Failed), &events);
            return Live::failed(Error::Io(error));
        }
    }
    future
}

fn run_owner<H: Owner>(
    owner: &mut H,
    receiver: &Receiver<Job<H>>,
    control: &Control<H::Closed>,
    events: &EventQueue<H::Event>,
) -> Option<Job<H>> {
    run_owner_wait(owner, control, events, |wait, timeout| {
        if wait {
            receiver.recv_timeout(timeout)
        } else {
            receiver.try_recv().map_err(|error| match error {
                mpsc::TryRecvError::Empty => RecvTimeoutError::Timeout,
                mpsc::TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
            })
        }
    })
}

/// The wait is injectable so tests can assert command fairness and idle
/// backoff without making wall-clock scheduling assertions.
fn run_owner_wait<H: Owner>(
    owner: &mut H,
    control: &Control<H::Closed>,
    events: &EventQueue<H::Event>,
    mut command: impl FnMut(bool, Duration) -> std::result::Result<Job<H>, RecvTimeoutError>,
) -> Option<Job<H>> {
    while !control.shutdown_requested() {
        // Limit each drain burst so a busy source cannot starve commands.
        let mut full = true;
        for _ in 0..16 {
            match owner.poll_native() {
                Ok(Some(event)) => events.push(Ok(event)),
                Ok(None) => {
                    full = false;
                    break;
                }
                Err(error) => {
                    events.push(Err(error));
                    full = false;
                    break;
                }
            }
        }
        owner.after_events(events, control);
        if control.shutdown_requested() {
            break;
        }
        // A full burst may have left a backlog. Service commands without
        // inserting a tick before the next burst. Empty/error paths retain
        // the timed wait to avoid spinning.
        match command(!full, owner.wait_timeout()) {
            Ok(job) => {
                if control.shutdown_requested() {
                    return Some(job);
                }
                job.run(owner);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    None
}

type ProbeJob = Box<dyn FnOnce() + Send + 'static>;

/// The thread that runs async probes, one at a time, and the bounded queue in
/// front of it.
///
/// A probe needs no player, so it has no owner thread of its own to queue on.
/// One thread serves every probe in the process: it is started by the first
/// probe, then waits for the next, and ends only once its queue is gone.
struct ProbeQueue {
    /// The sending side of the queue, once the thread is running.
    sender: Mutex<Option<SyncSender<ProbeJob>>>,
    /// How many probes may wait.
    capacity: usize,
}

/// The queue every [`AsyncFilePlayer::probe`] uses.
static PROBES: ProbeQueue = ProbeQueue::new(PROBE_CAPACITY);

impl ProbeQueue {
    const fn new(capacity: usize) -> Self {
        Self {
            sender: Mutex::new(None),
            capacity,
        }
    }

    /// Probe `path` with `api` once the probes queued before it have run.
    fn probe(&self, api: &FileApi, path: &Path) -> Live<FileInfo> {
        // Match synchronous probe precedence before starting a thread or
        // consuming a queue place, then retain the proved path text.
        let playback = match api.bind_table::<crate::sys::FilePlayback>() {
            Ok(playback) => playback,
            Err(error) => return Live::failed(error),
        };
        let path = match path_string(path) {
            Ok(path) => path,
            Err(error) => return Live::failed(error),
        };
        let (reply, future) = operation();
        let job: ProbeJob = Box::new(move || {
            if !reply.cancelled() {
                reply.complete(FilePlayer::probe_checked(&playback, &path));
            }
        });
        if let Err(error) = self.submit(job) {
            return Live::failed(error);
        }
        future
    }

    /// Queue `job`, starting the thread if it is not running.
    ///
    /// # Errors
    ///
    /// [`Error::QueueFull`] with [`QueueLimit::FileProbes`] when the queue is
    /// full, and [`Error::Io`] when the thread cannot be started. The job is
    /// dropped without running.
    fn submit(&self, job: ProbeJob) -> Result<()> {
        let mut sender = lock(&self.sender);
        let queue = match &mut *sender {
            Some(queue) => queue,
            idle => {
                let (queue, jobs) = mpsc::sync_channel::<ProbeJob>(self.capacity);
                thread::Builder::new()
                    .name("ndi-file-probe".into())
                    .spawn(move || {
                        mark_file_thread();
                        for job in jobs {
                            // A probe that panics fails alone: its reply,
                            // dropped while unwinding, reports a failed owner.
                            let _ = catch_unwind(AssertUnwindSafe(job));
                        }
                    })?;
                idle.insert(queue)
            }
        };
        match queue.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(Error::QueueFull(QueueLimit::FileProbes)),
            // The thread outlives every probe, so this needs it to have ended
            // some other way. The next probe starts another.
            Err(TrySendError::Disconnected(_)) => {
                *sender = None;
                Err(stopped(true))
            }
        }
    }
}

/// A [`FilePlayer`] on a thread of its own, driven through futures.
///
/// Clones share one player.
///
/// # Dropping blocks
///
/// This applies after creation returns the handle. Dropping its creation
/// operation before receiving the handle requests cleanup without waiting.
///
/// Dropping the last clone stops playback, removes the advertised source and
/// then **blocks the dropping thread** until the player's thread has
/// destroyed the player, for as long as the SDK takes. When the drop
/// returns, the source is gone. A caller that must not block, such as a task
/// on an async executor, awaits [`close`](Self::close) instead, which does the
/// same without blocking; dropping the clones afterwards does not block.
/// While a submitted close still has a live future, last-clone drop does not
/// wait because that future reports completion. Dropping the future makes the
/// last-clone drop wait for cleanup again.
/// A last-clone drop performed by an executor callback on a File owner, timer
/// or probe thread requests shutdown without waiting, so those threads can
/// keep delivering notifications. Await `close` to observe cleanup completing.
#[derive(Clone)]
pub struct AsyncFilePlayer {
    worker: Arc<Worker<PlayerOwner>>,
    name: Arc<str>,
}

impl std::fmt::Debug for AsyncFilePlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncFilePlayer")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl AsyncFilePlayer {
    /// Create a player and advertise its source, with the default queue sizes.
    ///
    /// The future fails as [`FilePlayer::new`] does. The options are
    /// only read, so they stay with the caller whatever the outcome.
    /// Options and the required File family are checked at submission, before
    /// an owner thread starts. Native creation runs on that thread.
    ///
    /// # Errors
    ///
    /// Those of [`FilePlayer::new`], and [`Error::Io`] when the owner thread
    /// cannot be started.
    #[doc(alias = "NDIlib_file_player_create")]
    pub fn create(api: &FileApi, options: &FilePlayerOptions) -> FileOperation<Self> {
        let name = Arc::from(options.name.as_str());
        Worker::<PlayerOwner>::create(api, options, move |worker| Self { worker, name })
    }

    /// Create a player and advertise its source, with the given queue sizes.
    ///
    /// The future fails as [`create`](Self::create) does, and with
    /// [`Error::InvalidInput`] when a queue size is out of range.
    /// Queue sizes, options and the required File family are checked at
    /// submission, before an owner thread starts.
    #[doc(alias = "NDIlib_file_player_create")]
    pub fn create_with_options(
        api: &FileApi,
        options: &FilePlayerOptions,
        asynchronous: &AsyncFileOptions,
    ) -> FileOperation<Self> {
        let name = Arc::from(options.name.as_str());
        Worker::<PlayerOwner>::create_with_options(api, options, asynchronous, move |worker| Self {
            worker,
            name,
        })
    }

    /// Read a recording's duration and track layout; see [`FilePlayer::probe`].
    /// No player is created and nothing is advertised.
    ///
    /// Probes run one at a time, in submission order, on one thread
    /// that every async probe in the process shares; it starts with the first
    /// probe. At most 64 wait for it. Dropping the future before its probe has
    /// begun cancels the probe.
    ///
    /// # Errors
    ///
    /// Those of [`FilePlayer::probe`], and [`Error::QueueFull`] with
    /// [`QueueLimit::FileProbes`] when 64 probes are already waiting; the same
    /// call can succeed once they have run. [`Error::Io`] when the probe
    /// thread is not running and cannot be started; the next probe tries to
    /// start it again. [`Error::InvalidState`] with [`StateConflict::OwnerFailed`]
    /// when the probe panicked; the thread goes on with the next one.
    #[doc(alias = "NDIlib_file_player_probe_file")]
    pub fn probe(api: &FileApi, path: impl AsRef<Path>) -> FileOperation<FileInfo> {
        let (api, path) = (api.clone(), path.as_ref().to_path_buf());
        FileOperation::deferred(move || PROBES.probe(&api, &path))
    }

    /// Play a file; see [`FilePlayer::start`].
    #[doc(alias = "NDIlib_file_player_start")]
    pub fn start(&self, path: impl AsRef<Path>) -> FileOperation<()> {
        let path = path.as_ref().to_path_buf();
        self.worker.prepare(
            move || {
                // Resolve before waiting in the command queue.
                let path = path_string(&path)?;
                Ok(move |player: &mut PlayerOwner| player.start_checked(&path))
            },
            false,
        )
    }
    /// Stop the current file; the source stays advertised. The stop runs even
    /// if the future is dropped after its first poll or explicit submission.
    #[doc(alias = "NDIlib_file_player_stop")]
    pub fn stop(&self) -> FileOperation<()> {
        self.worker.submit(true, |p| {
            p.stop();
            Ok(())
        })
    }
    /// Whether a file is playing.
    #[doc(alias = "NDIlib_file_player_is_running")]
    pub fn is_running(&self) -> FileOperation<bool> {
        self.worker.call(|p| Ok(p.is_running()))
    }
    /// Wait up to `timeout` for the player's next event.
    #[doc(alias = "NDIlib_file_player_capture_event")]
    pub fn next_event(&self, timeout: Duration) -> FileEventFuture<PlayerEvent> {
        self.worker.next_event(timeout)
    }
    /// How many events the SDK discarded before the player's thread took them.
    #[doc(alias = "NDIlib_file_player_get_dropped_event_count")]
    pub fn dropped_event_count(&self) -> FileOperation<u64> {
        self.worker.dropped_event_count()
    }
    /// How many events this handle's own queue discarded because
    /// [`next_event`](Self::next_event) did not keep up.
    pub fn notification_dropped_count(&self) -> u64 {
        self.worker.notification_dropped_count()
    }
    /// Counters for the current or most recent file.
    #[doc(alias = "NDIlib_file_player_get_stats")]
    pub fn stats(&self) -> FileOperation<Option<PlayerStats>> {
        self.worker.call(|p| p.stats())
    }
    /// Seek to a position on the file's timeline; see [`FilePlayer::seek`].
    #[doc(alias = "NDIlib_file_player_seek_to_time")]
    pub fn seek(&self, position: FilePosition) -> FileOperation<()> {
        self.worker.call(move |p| p.seek(position))
    }
    /// Seek to an approximate frame index; see
    /// [`FilePlayer::seek_frame_approximate`].
    #[doc(alias = "NDIlib_file_player_seek_to_frame")]
    pub fn seek_frame_approximate(&self, frame: u64) -> FileOperation<()> {
        self.worker.call(move |p| p.seek_frame_approximate(frame))
    }
    /// Pause or resume the current file.
    #[doc(alias = "NDIlib_file_player_set_paused")]
    pub fn set_paused(&self, paused: bool) -> FileOperation<()> {
        self.worker.call(move |p| {
            p.set_paused(paused);
            Ok(())
        })
    }
    /// Whether the current file is paused.
    #[doc(alias = "NDIlib_file_player_get_paused")]
    pub fn is_paused(&self) -> FileOperation<bool> {
        self.worker.call(|p| Ok(p.is_paused()))
    }
    /// Set whether playback restarts at the end of a file. The setting is
    /// kept across files.
    #[doc(alias = "NDIlib_file_player_set_loop")]
    pub fn set_looping(&self, looping: bool) -> FileOperation<()> {
        self.worker.call(move |p| {
            p.set_looping(looping);
            Ok(())
        })
    }
    /// Whether playback restarts at the end of a file.
    #[doc(alias = "NDIlib_file_player_get_loop")]
    pub fn is_looping(&self) -> FileOperation<bool> {
        self.worker.call(|p| Ok(p.is_looping()))
    }
    /// The current file's best-known duration, and whether it is final.
    #[doc(alias = "NDIlib_file_player_get_current_duration")]
    pub fn duration(&self) -> FileOperation<Option<FileDuration>> {
        self.worker.call(|p| Ok(p.duration()))
    }
    /// The position of the frame sent last.
    #[doc(alias = "NDIlib_file_player_get_current_position")]
    pub fn position(&self) -> FileOperation<Option<FilePosition>> {
        self.worker.call(|p| Ok(p.position()))
    }
    /// The recorded source timecode of the frame sent last.
    #[doc(alias = "NDIlib_file_player_get_current_source_timecode")]
    pub fn source_timecode(&self) -> FileOperation<Option<Timecode>> {
        self.worker.call(|p| Ok(p.source_timecode()))
    }
    /// The track layout of the current file.
    #[doc(alias = "NDIlib_file_player_get_source_format")]
    pub fn source_format(&self) -> FileOperation<Option<SourceFormat>> {
        self.worker.source_format()
    }
    /// The NDI source this player advertises.
    #[doc(alias = "NDIlib_file_player_get_advertised_source")]
    pub fn advertised_source(&self) -> FileOperation<Source> {
        self.worker.call(|p| p.advertised_source())
    }
    /// Stop playback, remove the advertised source and end the player's
    /// thread, for every clone of this handle.
    ///
    /// The future completes when the SDK has destroyed the player. Nothing
    /// blocks the calling thread. Once submitted by polling or calling
    /// [`FileOperation::submit`], the close runs even if its future is dropped.
    /// Closing again, from this or another clone, while or after it runs
    /// completes at the same time, with the same outcome; later
    /// operations fail with [`StateConflict::Closed`], and dropping the clones
    /// afterwards does not block. It does not wait in the command queue, so
    /// a full queue never refuses it.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidState`] with [`StateConflict::OwnerFailed`] when the
    /// object's thread failed.
    #[doc(alias = "NDIlib_file_player_destroy")]
    pub fn close(&self) -> FileOperation<()> {
        self.worker.close()
    }
}

/// A [`FileRecorder`] on a thread of its own, driven through futures.
///
/// Clones share one recorder, whose events are pumped continuously, so its
/// session report stays current without the caller polling.
///
/// # Finishing
///
/// Await [`finish`](Self::finish) and check for [`SessionEnd::Closed`] to
/// know a recording is complete, then await [`close`](Self::close). Neither
/// blocks the executor.
/// Native event loss, unreadable lifecycle evidence or a file-tracking limit
/// requires [`reset`](Self::reset) before another session can start. Closing
/// then uses the remaining cleanup budget even if every known file closes;
/// the indeterminate report remains unchanged. Evicted application event
/// notifications do not make native file tracking incomplete.
///
/// # Dropping blocks
///
/// This applies after creation returns the handle. Dropping its creation
/// operation before receiving the handle requests cleanup without waiting.
///
/// Dropping the last clone **blocks the dropping thread** until the
/// recorder's thread has finished. Its cleanup follows
/// [`FileRecorderOptions::drop_wait`]. Await [`close`](Self::close) to perform
/// that cleanup without blocking the executor.
///
/// While a submitted close or abandon still has a live future, last-clone drop does not
/// wait because that future reports completion. Dropping the future makes the
/// last-clone drop wait for cleanup again.
/// A last-clone drop performed by an executor callback on a File owner, timer
/// or probe thread requests shutdown without waiting for finalization or
/// destruction. Await `close` to observe cleanup completing.
#[derive(Clone)]
pub struct AsyncFileRecorder {
    worker: Arc<Worker<RecorderOwner>>,
}

impl std::fmt::Debug for AsyncFileRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncFileRecorder")
            .field("mode", &"recording")
            .finish_non_exhaustive()
    }
}

impl AsyncFileRecorder {
    /// Create a recorder with the default queue sizes.
    ///
    /// The future fails as [`FileRecorder::new`] does, including with
    /// [`Error::InvalidInput`] for options that conflict. The options are
    /// only read, so they stay with the caller whatever the outcome.
    /// Options and the required File family are checked at submission, before
    /// an owner thread starts. Native creation runs on that thread.
    ///
    /// # Errors
    ///
    /// Those of [`FileRecorder::new`], and [`Error::Io`] when the owner thread
    /// cannot be started.
    #[doc(alias = "NDIlib_file_recorder_create")]
    pub fn create(api: &FileApi, options: &FileRecorderOptions) -> FileOperation<Self> {
        Worker::<RecorderOwner>::create(api, options, |worker| Self { worker })
    }
    /// Create a recorder with the given queue sizes.
    ///
    /// The future fails as [`create`](Self::create) does, and with
    /// [`Error::InvalidInput`] when a queue size is out of range.
    /// Queue sizes, options and the required File family are checked at
    /// submission, before an owner thread starts.
    #[doc(alias = "NDIlib_file_recorder_create")]
    pub fn create_with_options(
        api: &FileApi,
        options: &FileRecorderOptions,
        asynchronous: &AsyncFileOptions,
    ) -> FileOperation<Self> {
        Worker::<RecorderOwner>::create_with_options(api, options, asynchronous, |worker| Self {
            worker,
        })
    }
    /// Start a session; see [`FileRecorder::start`]. Generated starts carry a
    /// [`SessionTimecode`](super::SessionTimecode), validated before queuing.
    /// Events left by an earlier refused start are delivered through
    /// [`next_event`](Self::next_event) before the new session begins.
    /// A recording or stopping session returns [`StateConflict::PreviousSessionOpen`].
    /// After it ends, see [`StateConflict::IncompleteFileTracking`] for when
    /// a new session requires a reset.
    #[doc(alias = "NDIlib_file_recorder_start")]
    #[doc(alias = "NDIlib_file_recorder_start_with_timecode")]
    pub fn start(
        &self,
        target: &RecordingTarget,
        timecode: RecordingTimecode,
    ) -> FileOperation<u64> {
        let target = target.clone();
        let events = Arc::clone(&self.worker.events);
        self.worker
            .call(move |r| r.start_notifying(&target, timecode, &events))
    }
    /// Ask the SDK to stop the session. Once submitted by polling or calling
    /// [`FileOperation::submit`], the request runs even if its future is dropped.
    #[doc(alias = "NDIlib_file_recorder_stop")]
    pub fn request_stop(&self) -> FileOperation<StopRequest> {
        self.worker.submit(true, |r| Ok(r.request_stop()))
    }
    /// Stop the session and wait up to `timeout` for its last file to be
    /// closed; see [`FileRecorder::finish`].
    ///
    /// The wait starts on the first poll or explicit submission, including time
    /// the command spends queued. After submission, dropping the future ends
    /// the wait but not the stop: the recorder keeps stopping and its events
    /// keep being pumped, and
    /// [`wait_for_finish`](Self::wait_for_finish) can pick the wait up again.
    /// While waiting for closure, the owner continues running other commands
    /// and waits from every clone between event bursts. A native call such as
    /// the stop itself may still block that thread. Each concurrent wait keeps
    /// its own submission deadline and observes the same session report.
    /// Submitted finish and condition waits share a separate budget of
    /// [`AsyncFileOptions::command_capacity`] places. When it is full, a new
    /// submission fails with [`Error::QueueFull`] and
    /// [`QueueLimit::FileFinishWaits`] before requesting a stop. Drop or await
    /// existing waits to restore admission; cancellation is reclaimed by the
    /// owner. Ordinary commands and explicit stop requests remain available.
    ///
    /// If another clone closes the recorder during the wait, the
    /// [`FileRecorderOptions::drop_wait`] deadline can end the wait early.
    /// Closure established in that time is reported normally; an unsettled session reports
    /// [`StateConflict::Closed`] instead of [`FinishStatus::TimedOut`], because
    /// the recorder is being destroyed and cannot be waited on again.
    ///
    /// Events taken during the wait go to [`next_event`](Self::next_event).
    /// An unreadable format is withheld through [`RecorderEvent::source_format`]
    /// while the event is delivered. A whole-event layout or type mismatch is
    /// delivered as [`Error::UnexpectedSdkResult`]; it does not end the wait.
    ///
    /// # Errors
    ///
    /// Those of [`FileRecorder::finish`], and those every
    /// [`FileOperation`] can fail with.
    #[doc(alias = "NDIlib_file_recorder_stop")]
    #[doc(alias = "NDIlib_file_recorder_capture_event")]
    pub fn finish(&self, timeout: Duration) -> FileOperation<FinishStatus> {
        self.finish_inner(timeout, true, FinishResponse::Finish)
    }
    /// Wait up to `timeout` for a stopping session to end; see
    /// [`FileRecorder::wait_for_finish`].
    ///
    /// Returns `None` when the timeout expires, or `Some` with
    /// [`SessionEnd::Closed`] or [`SessionEnd::Indeterminate`] when the
    /// session ends. The current report remains available through
    /// [`report`](Self::report), including after a timeout.
    ///
    /// Events taken during the wait are delivered as by
    /// [`finish`](Self::finish); other commands and waits continue running.
    /// A concurrent close bounds the wait and can
    /// return [`StateConflict::Closed`] on the same terms as `finish`.
    /// It shares `finish`'s independent pending-wait limit and can report
    /// [`Error::QueueFull`] with [`QueueLimit::FileFinishWaits`].
    ///
    /// # Errors
    ///
    /// Those of [`FileRecorder::wait_for_finish`], and those every
    /// [`FileOperation`] can fail with.
    #[doc(alias = "NDIlib_file_recorder_capture_event")]
    pub fn wait_for_finish(&self, timeout: Duration) -> FileOperation<Option<SessionEnd>> {
        self.finish_inner(timeout, false, FinishResponse::Wait)
    }

    /// Replace the native recorder after abandoning its old session; see
    /// [`FileRecorder::reset`]. Queued events already delivered to this
    /// worker's event stream remain available to the caller. A successful reset
    /// ends pending finish waits for the old recorder with [`StateConflict::Closed`];
    /// a failed reset leaves them waiting on the original session.
    /// Options are checked at submission, before waiting in the command queue.
    #[doc(alias = "NDIlib_file_recorder_destroy")]
    #[doc(alias = "NDIlib_file_recorder_create")]
    pub fn reset(&self, options: &FileRecorderOptions) -> FileOperation<RecordingReport> {
        let options = options.clone();
        self.worker.prepare(
            move || {
                let options = options.check()?;
                Ok(move |recorder: &mut RecorderOwner| recorder.reset(options))
            },
            false,
        )
    }

    fn finish_inner<T: Send + 'static>(
        &self,
        timeout: Duration,
        stop: bool,
        finish_response: fn(CommandReply<T, RecordingReport>, Place) -> FinishResponse,
    ) -> FileOperation<T> {
        self.worker.finish(timeout, stop, finish_response)
    }

    /// Wait up to `timeout` for the recorder's next event. Dropping the
    /// future does not affect the recording.
    #[doc(alias = "NDIlib_file_recorder_capture_event")]
    pub fn next_event(&self, timeout: Duration) -> FileEventFuture<RecorderEvent> {
        self.worker.next_event(timeout)
    }
    /// How many events the SDK discarded before the recorder's thread took them.
    #[doc(alias = "NDIlib_file_recorder_get_dropped_event_count")]
    pub fn dropped_event_count(&self) -> FileOperation<u64> {
        self.worker.dropped_event_count()
    }
    /// How many events this handle's own queue discarded because
    /// [`next_event`](Self::next_event) did not keep up.
    pub fn notification_dropped_count(&self) -> u64 {
        self.worker.notification_dropped_count()
    }
    /// Where the recorder is in its session.
    pub fn state(&self) -> FileOperation<RecorderState> {
        self.worker.call(|r| Ok(r.state()))
    }
    /// What the recorder's events have shown about the current session.
    pub fn report(&self) -> FileOperation<RecordingReport> {
        self.worker.call(|r| Ok(r.report()))
    }
    /// Whether the SDK is recording or waiting for frames.
    #[doc(alias = "NDIlib_file_recorder_is_recording")]
    pub fn is_recording(&self) -> FileOperation<bool> {
        self.worker.call(|r| Ok(r.is_recording()))
    }
    /// Switch to recording another source; see [`FileRecorder::set_source`].
    #[doc(alias = "NDIlib_file_recorder_set_source")]
    pub fn set_source(&self, source: &SourceSpec) -> FileOperation<()> {
        let source = source.clone();
        self.worker.call(move |r| r.set_source(&source))
    }
    /// The source the SDK accepted last; see
    /// [`FileRecorder::last_accepted_source`].
    pub fn last_accepted_source(&self) -> FileOperation<Option<SourceSpec>> {
        self.worker.call(|r| Ok(r.last_accepted_source().cloned()))
    }
    /// The most recently opened file still retained in the file tracker; see
    /// [`FileRecorder::reported_current_file`] for incomplete-tracking limits.
    pub fn reported_current_file(&self) -> FileOperation<Option<EventText>> {
        self.worker.call(|r| Ok(r.reported_current_file().cloned()))
    }
    /// Close the current file and continue in a new one; see
    /// [`FileRecorder::rollover`].
    #[doc(alias = "NDIlib_file_recorder_rollover")]
    pub fn rollover(&self, target: &RecordingTarget) -> FileOperation<()> {
        let target = target.clone();
        self.worker.call(move |r| r.rollover(&target))
    }
    /// Write a metadata frame into the recording at its current position;
    /// see [`FileRecorder::inject_metadata`].
    ///
    /// Copies the source's text and timecode when this method is called,
    /// retaining the copy until the deferred worker command runs or is
    /// discarded. Accepts owned and captured metadata without retaining a
    /// native frame or borrowing the source through the command's lifetime.
    #[doc(alias = "NDIlib_file_recorder_inject_metadata")]
    pub fn inject_metadata<S: MetadataSource + ?Sized>(&self, metadata: &S) -> FileOperation<()> {
        let metadata = MetadataSource::to_owned(metadata);
        self.worker.call(move |r| r.inject_metadata(&metadata))
    }
    /// The recorder's running counters.
    #[doc(alias = "NDIlib_file_recorder_get_stats")]
    pub fn stats(&self) -> FileOperation<Option<RecorderStats>> {
        self.worker.call(|r| r.stats())
    }
    /// The format of the source, once the recorder has seen audio or video.
    #[doc(alias = "NDIlib_file_recorder_get_source_format")]
    pub fn source_format(&self) -> FileOperation<Option<SourceFormat>> {
        self.worker.source_format()
    }
    /// Stop and destroy the recorder without waiting for the SDK to finalize
    /// an open file, returning the last known session report after destruction.
    /// The file of an unfinished session may be left truncated.
    ///
    /// Like [`close`](Self::close), this affects every clone, bypasses the
    /// command queue and runs after submission even if its future is dropped.
    /// A close pending when abandon is submitted completes with the same
    /// shutdown; later operations fail with [`StateConflict::Closed`].
    /// An already running native call cannot be interrupted.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidState`] with [`StateConflict::OwnerFailed`] when the
    /// object's thread failed.
    #[doc(alias = "NDIlib_file_recorder_destroy")]
    pub fn abandon(&self) -> FileOperation<RecordingReport> {
        let sender = self.worker.sender.clone();
        self.worker.control.request_abandon(move || {
            let _ = sender.send(Job::Wake);
        })
    }

    /// Stop an open session, destroy the recorder and end its thread, for
    /// every clone of this handle.
    ///
    /// Shutdown follows [`FileRecorderOptions::drop_wait`]. A submitted
    /// [`finish`](Self::finish) or [`wait_for_finish`](Self::wait_for_finish)
    /// keeps its own earlier deadline. The future completes when the SDK has
    /// destroyed the recorder. Await [`finish`](Self::finish) first to learn
    /// whether the SDK confirmed closure. Once submitted by polling
    /// or calling [`FileOperation::submit`], the close runs even if its future
    /// is dropped. Closing again, from this or another clone,
    /// while or after it runs completes at the same time, with the same
    /// outcome; later operations fail with [`StateConflict::Closed`], and
    /// dropping the clones afterwards does not block. It does not wait in the
    /// command queue, so a full queue never refuses it.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidState`] with [`StateConflict::OwnerFailed`] when the
    /// object's thread failed.
    #[doc(alias = "NDIlib_file_recorder_destroy")]
    pub fn close(&self) -> FileOperation<()> {
        self.worker.close()
    }
}

#[cfg(test)]
mod waker_tests;

#[cfg(test)]
mod native_tests;

#[cfg(test)]
mod tests {
    use super::{
        super::{
            mock::{self, Closing},
            FileNaming, RecorderEventKind,
        },
        *,
    };
    use std::{rc::Rc, task::Wake};

    #[test]
    fn formatting_an_operation_does_not_wait_for_its_stage_lock() {
        let operation = Arc::new(FileOperation::<()>::live(Live::Immediate(Ok(()))));
        let stage = lock(&operation.stage);
        let (sender, receiver) = mpsc::channel();
        let other = Arc::clone(&operation);
        let thread = thread::spawn(move || sender.send(format!("{other:?}")).unwrap());
        let formatted = receiver.recv_timeout(Duration::from_secs(1));
        drop(stage);
        thread.join().unwrap();
        let formatted = formatted.expect("Debug waited for an operation's stage lock");
        assert!(formatted.starts_with("FileOperation {"));
        assert!(formatted.contains("submitted: None"));
    }

    struct Signal(Mutex<bool>, Condvar);
    impl Wake for Signal {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            *lock(&self.0) = true;
            self.1.notify_one();
        }
    }

    struct PanickingWake(mpsc::Sender<()>);

    impl Wake for PanickingWake {
        fn wake(self: Arc<Self>) {
            let _ = self.0.send(());
            panic!("consumer waker panicked");
        }
    }

    fn panicking_waker() -> (Waker, Receiver<()>) {
        let (called, calls) = mpsc::channel();
        (Waker::from(Arc::new(PanickingWake(called))), calls)
    }

    pub(super) fn block_on<F: Future>(future: F) -> F::Output {
        let signal = Arc::new(Signal(Mutex::new(false), Condvar::new()));
        let waker = Waker::from(Arc::clone(&signal));
        let mut future = std::pin::pin!(future);
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut Context::from_waker(&waker)) {
                return value;
            }
            let mut notified = lock(&signal.0);
            while !*notified {
                let (guard, timeout) = signal
                    .1
                    .wait_timeout(notified, Duration::from_secs(5))
                    .unwrap();
                assert!(!timeout.timed_out(), "test future was not woken");
                notified = guard;
            }
            *notified = false;
        }
    }

    struct BurstOwner {
        remaining: usize,
        polls: usize,
        fail: bool,
    }

    impl Owner for BurstOwner {
        type Closed = ();
        fn shutdown(
            &mut self,
            _: Instant,
            _: &EventQueue<Self::Event>,
            _: &Control,
            _: &mut [Job<Self>],
        ) {
        }
        type Finish = std::convert::Infallible;
        fn finish_request(&mut self, finish: Self::Finish) {
            match finish {}
        }
        type Event = usize;
        fn poll_native(&mut self) -> Result<Option<usize>> {
            self.polls += 1;
            if self.fail {
                return Err(Error::InvalidFrame(
                    crate::FrameDefect::MetadataMissingTerminator,
                ));
            }
            if self.remaining == 0 {
                return Ok(None);
            }
            self.remaining -= 1;
            Ok(Some(self.polls))
        }
    }

    #[test]
    fn player_finish_is_uninhabited() {
        fn require_uninhabited(job: Job<PlayerOwner>) {
            if let Job::Finish { finish, .. } = job {
                match finish {}
            }
        }
        let _: fn(Job<PlayerOwner>) = require_uninhabited;
    }

    #[test]
    fn player_commands_gate_closed_and_failed_owners_before_admission() {
        for failed in [false, true] {
            for during_preparation in [false, true] {
                let (sender, receiver) = mpsc::channel();
                let control = Arc::new(Control::new());
                let admission = Arc::new(Admission::new(1));
                let place = admission.admit(false).unwrap();
                let worker = Worker::<PlayerOwner> {
                    sender,
                    admission: Arc::clone(&admission),
                    finish: (),
                    control: Arc::clone(&control),
                    events: Arc::new(EventQueue::new(1)),
                    thread: OnceLock::new(),
                };
                let stop = move || {
                    if failed {
                        control.publish_failure();
                    } else {
                        control.request_shutdown();
                    }
                };
                let prepare = if during_preparation {
                    Some(stop)
                } else {
                    stop();
                    None
                };
                let refused = worker.prepare(
                    move || {
                        if let Some(stop) = prepare {
                            stop();
                        } else {
                            panic!("closed player ran argument preparation");
                        }
                        Ok(|_: &mut PlayerOwner| Ok(()))
                    },
                    false,
                );
                if failed {
                    owner_failed(block_on(refused));
                } else {
                    closed(block_on(refused));
                }
                assert_eq!(admission.state.load(Ordering::Acquire), 2);
                assert!(matches!(
                    receiver.try_recv(),
                    Err(mpsc::TryRecvError::Empty)
                ));
                drop(place);
            }
        }
    }

    #[test]
    fn player_close_carries_only_a_unit_request_and_outcome() {
        fn require_unit_request(
            request: <<PlayerOwner as Owner>::Closed as CloseOutcome>::Request,
        ) {
            let () = request;
        }
        fn require_unit_outcome(outcome: <PlayerOwner as Owner>::Closed) {
            let () = outcome;
        }
        let _: fn(()) = require_unit_request;
        let _: fn(()) = require_unit_outcome;
    }

    #[test]
    fn cancelled_replies_never_deliver_a_result_or_drop_fallback() {
        fn check<F: FailureSource>(reply: Reply<(), F>, future: Live<()>, complete: bool) {
            let shared = Arc::clone(&reply.shared);
            drop(FileOperation::live(future));
            assert!(reply.cancelled());
            if complete {
                reply.complete(Ok(()));
            } else {
                drop(reply);
            }
            assert!(matches!(*lock(&shared.slot), Slot::Pending(_)));
        }
        for complete in [false, true] {
            let control = Arc::new(Control::new());
            let (reply, future) = control.command::<()>();
            control.publish_failure();
            check(reply, future, complete);
            let (reply, future) = operation();
            check(reply, future, complete);
        }
    }

    #[test]
    fn dropped_replies_use_only_their_actual_failure_source() {
        for failed in [false, true] {
            let control = Arc::new(Control::new());
            let (plain, plain_result) = operation::<()>();
            let (command, command_result) = control.command::<()>();
            if failed {
                control.publish_failure();
            }
            drop(plain);
            drop(command);
            closed(block_on(FileOperation::live(plain_result)));
            if failed {
                owner_failed(block_on(FileOperation::live(command_result)));
            } else {
                closed(block_on(FileOperation::live(command_result)));
            }
        }
    }

    #[test]
    fn event_backlogs_continue_without_ticks_and_keep_command_fairness() {
        let control = Control::new();
        let events = EventQueue::new(20);
        let mut owner = BurstOwner {
            remaining: 40,
            polls: 0,
            fail: false,
        };
        let mut waits = Vec::new();
        run_owner_wait(&mut owner, &control, &events, |wait, _| {
            waits.push(wait);
            match waits.len() {
                1 => Ok(Job::Run(Box::new(|owner: &mut BurstOwner| {
                    assert_eq!(owner.polls, 16)
                }))),
                2 => Ok(Job::Run(Box::new(|owner: &mut BurstOwner| {
                    assert_eq!(owner.polls, 32)
                }))),
                _ => Err(RecvTimeoutError::Disconnected),
            }
        });
        assert_eq!(waits, [false, false, true]);
        assert_eq!(owner.polls, 41);
        let events = lock(&events.0);
        assert_eq!(events.queue.len(), 20);
        assert_eq!(events.dropped, 20);
        assert_eq!(events.queue.front().unwrap().as_ref().unwrap(), &21);
        assert_eq!(events.queue.back().unwrap().as_ref().unwrap(), &40);
    }

    #[test]
    fn close_and_last_handle_drop_wake_the_owner_during_its_command_wait() {
        for closing in [false, true] {
            let (sender, receiver) = mpsc::channel();
            let control = Arc::new(Control::new());
            let events = Arc::new(EventQueue::new(1));
            let worker = Arc::new(Worker::<BurstOwner> {
                sender,
                admission: Arc::new(Admission::new(1)),
                finish: (),
                control: Arc::clone(&control),
                events: Arc::clone(&events),
                thread: OnceLock::new(),
            });
            let (entered, waiting) = mpsc::channel();
            thread::scope(|scope| {
                scope.spawn(move || {
                    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
                    if closing {
                        block_on(worker.close()).unwrap();
                    } else {
                        drop(worker);
                    }
                });
                let mut owner = BurstOwner {
                    remaining: 0,
                    polls: 0,
                    fail: false,
                };
                run_owner_wait(&mut owner, &control, &events, |wait, _| {
                    assert!(wait);
                    entered.send(()).unwrap();
                    // The long bound is a test deadlock guard. Success requires
                    // receiving a message, never expiring a scheduling interval.
                    let message = receiver.recv_timeout(Duration::from_secs(5));
                    assert!(message.is_ok(), "shutdown published no channel wake");
                    message
                });
                assert!(control.shutdown_requested());
                control.exit(Ok(()), &events);
            });
        }
    }

    #[test]
    fn event_idle_and_error_paths_wait_and_shutdown_ends_continuous_bursts() {
        for fail in [false, true] {
            let mut owner = BurstOwner {
                remaining: 0,
                polls: 0,
                fail,
            };
            let events = EventQueue::new(10);
            run_owner_wait(&mut owner, &Control::new(), &events, |wait, _| {
                assert!(wait, "idle and malformed-event paths must back off");
                Err(RecvTimeoutError::Disconnected)
            });
            assert_eq!(owner.polls, 1);
            assert_eq!(lock(&events.0).queue.len(), usize::from(fail));
        }
        let control = Arc::new(Control::new());
        let mut owner = BurstOwner {
            remaining: usize::MAX,
            polls: 0,
            fail: false,
        };
        let mut commands = 0;
        run_owner_wait(&mut owner, &control, &EventQueue::new(16), |wait, _| {
            assert!(!wait);
            commands += 1;
            let control = Arc::clone(&control);
            Ok(Job::Run(Box::new(move |_: &mut BurstOwner| {
                control.request_shutdown();
            })))
        });
        assert_eq!(commands, 1);
        assert_eq!(owner.polls, 16);
    }

    struct LocalOwner {
        created: thread::ThreadId,
        dropped: mpsc::Sender<thread::ThreadId>,
        counter: usize,
        _not_send: Rc<()>,
    }
    impl Owner for LocalOwner {
        type Closed = ();
        fn shutdown(
            &mut self,
            _: Instant,
            _: &EventQueue<Self::Event>,
            _: &Control,
            _: &mut [Job<Self>],
        ) {
        }
        type Finish = std::convert::Infallible;
        fn finish_request(&mut self, finish: Self::Finish) {
            match finish {}
        }
        type Event = usize;
        fn poll_native(&mut self) -> Result<Option<usize>> {
            Ok(None)
        }
    }
    impl Drop for LocalOwner {
        fn drop(&mut self) {
            assert_eq!(self.created, thread::current().id());
            let _ = self.dropped.send(thread::current().id());
        }
    }
    fn local_worker() -> (Arc<Worker<LocalOwner>>, Receiver<thread::ThreadId>) {
        local_worker_with_options(AsyncFileOptions::new())
    }
    fn local_worker_with_options(
        options: AsyncFileOptions,
    ) -> (Arc<Worker<LocalOwner>>, Receiver<thread::ThreadId>) {
        let (tx, rx) = mpsc::channel();
        let worker = block_on(spawn_owner(
            "file-test-owner",
            move || {
                Ok(LocalOwner {
                    created: thread::current().id(),
                    dropped: tx,
                    counter: 0,
                    _not_send: Rc::new(()),
                })
            },
            options,
            |worker| worker,
        ))
        .unwrap();
        (worker, rx)
    }

    /// Hold the owner until the test has registered its futures' wakers.
    pub(super) fn block_owner<H: Owner>(worker: &Worker<H>) -> (FileOperation<()>, Sender<()>) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocked = worker
            .call(move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .submit();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        (blocked, release_tx)
    }

    #[test]
    fn a_panicking_creation_waker_does_not_destroy_the_returned_owner() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let mut creating = Box::pin(spawn_owner(
            "file-test-create-wake",
            move || {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(LocalOwner {
                    created: thread::current().id(),
                    dropped: dropped_tx,
                    counter: 0,
                    _not_send: Rc::new(()),
                })
            },
            AsyncFileOptions::new(),
            |worker| worker,
        ));
        let (waker, woken) = panicking_waker();
        assert!(creating
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        release_tx.send(()).unwrap();
        woken.recv_timeout(Duration::from_secs(1)).unwrap();

        let worker = block_on(creating).unwrap();
        let created = block_on(worker.call(|owner| Ok(owner.created))).unwrap();
        assert!(dropped_rx.try_recv().is_err());
        drop(worker);
        assert_eq!(
            dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            created
        );
    }

    #[test]
    fn a_panicking_command_waker_does_not_stop_recording() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        let (blocked, release) = block_owner(&recorder.worker);
        let (waker, woken) = panicking_waker();
        let mut first = Box::pin(recorder.is_recording());
        assert!(first
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        let queued = recorder.is_recording().submit();
        release.send(()).unwrap();
        block_on(blocked).unwrap();

        assert!(block_on(queued).unwrap());
        assert!(block_on(first).unwrap());
        woken.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(woken.try_recv().is_err(), "the reply was notified twice");
        assert_eq!(device.stops.load(Ordering::Acquire), 0);
        assert_eq!(device.destroys.load(Ordering::Acquire), 0);
        block_on(recorder.close()).unwrap();
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn a_panicking_event_waker_does_not_stop_recording_or_lose_the_event() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        let started = block_on(recorder.next_event(Duration::from_secs(1)))
            .unwrap()
            .unwrap();
        assert_eq!(started.kind(), RecorderEventKind::Started);
        let (waker, woken) = panicking_waker();
        let mut event = Box::pin(recorder.next_event(Duration::from_secs(30)));
        assert!(event
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());

        device.push(mock::FIRST_FRAME, mock::TAKE);
        woken.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(block_on(recorder.is_recording()).unwrap());
        assert_eq!(
            block_on(event).unwrap().unwrap().kind(),
            RecorderEventKind::FirstFrame
        );
        assert_eq!(device.stops.load(Ordering::Acquire), 0);
        assert_eq!(device.destroys.load(Ordering::Acquire), 0);
        block_on(recorder.close()).unwrap();
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn panicking_shutdown_wakers_do_not_skip_other_closes_or_recorder_cleanup() {
        for event_panics in [false, true] {
            let (device, recorder) = mock_recorder(Closing::AtStop);
            block_on(recorder.next_event(Duration::from_secs(1)))
                .unwrap()
                .unwrap();
            let (blocked, release) = block_owner(&recorder.worker);
            let (event_waker, event_woken) = panicking_waker();
            let event_waker = if event_panics {
                event_waker
            } else {
                idle_waker()
            };
            let mut event = Box::pin(recorder.next_event(Duration::from_secs(30)));
            assert!(event
                .as_mut()
                .poll(&mut Context::from_waker(&event_waker))
                .is_pending());
            let (close_waker, close_woken) = panicking_waker();
            let mut first = Box::pin(recorder.close());
            assert!(first
                .as_mut()
                .poll(&mut Context::from_waker(&close_waker))
                .is_pending());
            let mut second = Box::pin(recorder.close());
            assert!(second
                .as_mut()
                .poll(&mut Context::from_waker(&idle_waker()))
                .is_pending());
            release.send(()).unwrap();
            block_on(blocked).unwrap();

            block_on(second).unwrap();
            block_on(first).unwrap();
            closed(block_on(event));
            close_woken.recv_timeout(Duration::from_secs(1)).unwrap();
            if event_panics {
                event_woken.recv_timeout(Duration::from_secs(1)).unwrap();
            }
            assert_eq!(device.stops.load(Ordering::Acquire), 1);
            assert_eq!(device.destroys.load(Ordering::Acquire), 1);
            assert!(device.closed_before_destroy.load(Ordering::Acquire));
            // A failed notification must not misclassify a normal shutdown.
            block_on(recorder.close()).unwrap();
            closed(block_on(recorder.is_recording()));
        }
    }

    #[test]
    fn a_reply_can_wake_a_panicking_consumer_during_owner_unwind() {
        let (worker, dropped) = local_worker();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut failing = Box::pin(worker.call(move |_| -> Result<()> {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            panic!("owner work panicked");
        }));
        let (waker, woken) = panicking_waker();
        assert!(failing
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let queued = worker.call(|_| Ok(())).submit();
        release_tx.send(()).unwrap();

        owner_failed(block_on(queued));
        owner_failed(block_on(failing));
        woken.recv_timeout(Duration::from_secs(1)).unwrap();
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
        owner_failed(block_on(worker.control.request_close(|| {})));
    }

    #[test]
    fn a_panicking_timeout_waker_does_not_stop_other_event_deadlines() {
        let events = Arc::new(EventQueue::<u32>::new(1));
        let (waker, woken) = panicking_waker();
        let mut event = Box::pin(events.wait(Duration::from_millis(10)));
        assert!(event
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        woken.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(block_on(event).unwrap(), None);
        let other = Arc::new(EventQueue::<u32>::new(1));
        assert_eq!(
            block_on(other.wait(Duration::from_millis(10))).unwrap(),
            None
        );
    }

    #[test]
    fn unpolled_creation_does_not_start_an_owner() {
        let created = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&created);
        let operation = spawn_owner(
            "file-test-unpolled-create",
            move || -> Result<LocalOwner> {
                flag.store(true, Ordering::Release);
                panic!("an unpolled create must not run")
            },
            AsyncFileOptions::new(),
            |worker| worker,
        );
        assert!(!created.load(Ordering::Acquire));
        drop(operation);
        assert!(!created.load(Ordering::Acquire));
    }

    #[test]
    fn ordinary_operations_start_on_poll_or_explicit_submission() {
        let (worker, dropped) = local_worker();
        let deferred = worker.call(|owner| {
            owner.counter += 1;
            Ok(())
        });
        assert_eq!(worker.admission.state.load(Ordering::Acquire), 0);
        assert_eq!(block_on(worker.call(|owner| Ok(owner.counter))).unwrap(), 0);
        drop(deferred);
        assert_eq!(block_on(worker.call(|owner| Ok(owner.counter))).unwrap(), 0);

        let operation = worker.call(|owner| {
            owner.counter += 1;
            Ok(())
        });
        block_on(operation).unwrap();
        assert_eq!(block_on(worker.call(|owner| Ok(owner.counter))).unwrap(), 1);

        let submitted = worker
            .call(|owner| {
                owner.counter += 10;
                Ok(())
            })
            .submit();
        // Reading the same owner's FIFO observes explicit submission before
        // the submitted future has ever been polled.
        assert_eq!(
            block_on(worker.call(|owner| Ok(owner.counter))).unwrap(),
            11
        );
        block_on(submitted).unwrap();
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn unpolled_operations_do_not_postpone_last_handle_shutdown() {
        let (worker, dropped) = local_worker();
        let deferred = worker.call(|owner| {
            owner.counter += 1;
            Ok(())
        });
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
        closed(block_on(deferred));
    }

    #[test]
    fn deferred_preparation_does_not_run_after_owner_shutdown() {
        let (worker, dropped) = local_worker();
        let prepared = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&prepared);
        let deferred = worker.prepare(
            move || {
                flag.store(true, Ordering::Release);
                Ok(|_: &mut LocalOwner| Ok(()))
            },
            false,
        );
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
        closed(block_on(deferred));
        assert!(!prepared.load(Ordering::Acquire));
    }

    #[test]
    fn non_send_owner_is_created_used_and_destroyed_on_its_thread() {
        let (worker, dropped) = local_worker();
        let created = block_on(worker.call(|owner| Ok(owner.created))).unwrap();
        assert_ne!(created, thread::current().id());
        block_on(worker.control.request_close(|| {})).unwrap();
        assert_eq!(
            dropped.recv_timeout(Duration::from_secs(1)).unwrap(),
            created
        );
    }

    #[test]
    fn queued_cancellation_skips_mutation_but_accepted_stop_persists() {
        let (worker, dropped) = local_worker();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = worker
            .call(move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .submit();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let mut ordinary = worker.call(|owner| {
            owner.counter += 1;
            Ok(())
        });
        assert!(Pin::new(&mut ordinary)
            .poll(&mut Context::from_waker(&idle_waker()))
            .is_pending());
        drop(ordinary);
        let mut stopping = worker.submit(true, |owner| {
            owner.counter += 10;
            Ok(())
        });
        assert!(Pin::new(&mut stopping)
            .poll(&mut Context::from_waker(&idle_waker()))
            .is_pending());
        drop(stopping);
        release_tx.send(()).unwrap();
        block_on(blocker).unwrap();
        assert_eq!(
            block_on(worker.call(|owner| Ok(owner.counter))).unwrap(),
            10
        );
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn cancelled_started_work_preserves_owner_for_following_commands() {
        let (worker, dropped) = local_worker();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let started = worker
            .submit(false, move |owner| {
                owner.counter += 1;
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .submit();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(started);
        release_tx.send(()).unwrap();
        assert_eq!(block_on(worker.call(|owner| Ok(owner.counter))).unwrap(), 1);
        assert!(dropped.try_recv().is_err());
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn command_submission_allocates_only_its_live_state_and_job() {
        let (worker, dropped) =
            local_worker_with_options(AsyncFileOptions::new().command_capacity(1));
        // Warm the channel before holding its owner, so its lazy block
        // allocation is not counted as an operation allocation.
        block_on(worker.call(|_| Ok(()))).unwrap();
        let (blocker, release) = block_owner(&worker);
        let (accepted, allocations) =
            crate::test_alloc::allocation_count(|| worker.call(|owner| Ok(owner.counter)).submit());
        assert_eq!(allocations, 3, "deferral, shared response and job");
        let refused = worker.call(|_| Ok(()));
        let (refused, allocations) = crate::test_alloc::allocation_count(|| refused.submit());
        assert_eq!(allocations, 0, "a full queue creates no live response");
        assert!(matches!(
            block_on(refused),
            Err(Error::QueueFull(QueueLimit::FileCommands))
        ));
        release.send(()).unwrap();
        block_on(blocker).unwrap();
        block_on(accepted).unwrap();
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn a_deferred_file_operation_cannot_return_another_deferral() {
        // Compile the actual lifecycle declarations and constructor body.
        // Only unrelated shared response data and the error type are stubbed;
        // an alias or changed constructor cannot drift from this contract.
        fn declaration<'a>(source: &'a str, marker: &str) -> &'a str {
            let body = &source[source.find(marker).unwrap()..];
            let mut depth = 0;
            for (index, byte) in body.bytes().enumerate() {
                match byte {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return &body[..=index];
                        }
                    }
                    _ => {}
                }
            }
            panic!("unterminated declaration: {marker}")
        }
        let source = include_str!("asynchronous.rs");
        let live = declaration(source, "enum Live<T> {");
        let stage = declaration(source, "enum Stage<T> {");
        let future = declaration(source, "pub struct FileOperation<T> {");
        let constructor = declaration(source, "fn deferred(start:");
        let definitions = format!(
            "#![allow(dead_code)]\nuse std::sync::{{Arc, Mutex}};\n\
             type Result<T> = std::result::Result<T, ()>;\n\
             struct Shared<T>(std::marker::PhantomData<T>);\n\
             {live}\n{stage}\n{future}\n\
             impl<T> FileOperation<T> {{ {constructor} }}\n"
        );
        let fixture =
            include_str!("../../tests/ui/internal/fail/file_operation_cannot_defer_again.rs");
        let directory = crate::test_support::FixtureDir::create("file-operation-contract");
        for (name, body, succeeds) in [
            (
                "live",
                "fn main() { let _ = FileOperation::<()>::deferred(|| Live::Immediate(Ok(()))); }",
                true,
            ),
            ("nested", fixture, false),
        ] {
            let path = directory.join(format!("{name}.rs"));
            std::fs::write(&path, format!("{definitions}\n{body}")).unwrap();
            let output = std::process::Command::new("rustc")
                .args(["--edition=2021", "--emit=metadata", "--out-dir"])
                .arg(directory.path())
                .arg(path)
                .output()
                .unwrap();
            let error = String::from_utf8(output.stderr).unwrap();
            assert_eq!(output.status.success(), succeeds, "{name}: {error}");
            if !succeeds {
                assert!(
                    error.contains("E0308") && error.contains("Live<()>"),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn bounded_queue_rejects_excess_work_without_executing_it() {
        let options = AsyncFileOptions::new().command_capacity(1);
        let (worker, dropped) = local_worker_with_options(options);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = worker
            .call(move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .submit();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let queued = worker
            .call(|owner| {
                owner.counter += 1;
                Ok(owner.counter)
            })
            .submit();
        assert!(matches!(
            block_on(worker.call(|owner| {
                owner.counter += 100;
                Ok(())
            })),
            Err(Error::QueueFull(QueueLimit::FileCommands))
        ));
        release_tx.send(()).unwrap();
        block_on(blocker).unwrap();
        assert_eq!(block_on(queued).unwrap(), 1);
        assert_eq!(block_on(worker.call(|owner| Ok(owner.counter))).unwrap(), 1);
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    fn owner_failed<T: std::fmt::Debug>(result: Result<T>) {
        assert!(
            matches!(result, Err(Error::InvalidState(StateConflict::OwnerFailed))),
            "expected OwnerFailed, got {result:?}"
        );
    }

    fn closed<T: std::fmt::Debug>(result: Result<T>) {
        assert!(
            matches!(result, Err(Error::InvalidState(StateConflict::Closed))),
            "expected Closed, got {result:?}"
        );
    }

    #[test]
    fn worker_panic_replies_with_failure_and_destroys_owner_locally() {
        let (worker, dropped) = local_worker();
        let created = block_on(worker.call(|owner| Ok(owner.created))).unwrap();
        // A wait that is already pending when the thread fails.
        let mut pending = worker.events.wait(Duration::from_secs(30));
        let signal = Arc::new(Signal(Mutex::new(false), Condvar::new()));
        let waker = Waker::from(Arc::clone(&signal));
        assert!(Pin::new(&mut pending)
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = worker
            .call(move |_| -> Result<()> {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                panic!("mock owner failure");
            })
            .submit();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // Queued behind the command that fails.
        let queued: FileOperation<()> = worker.call(|_| Ok(())).submit();
        release_tx.send(()).unwrap();
        owner_failed(block_on(blocker));
        owner_failed(block_on(queued));
        owner_failed(block_on(pending));
        assert_eq!(
            dropped.recv_timeout(Duration::from_secs(1)).unwrap(),
            created
        );
        owner_failed(block_on(worker.control.request_close(|| {})));
        owner_failed(block_on(worker.call(|_| Ok(()))));
        owner_failed(block_on(worker.submit(true, |_| Ok(()))));
        owner_failed(block_on(worker.events.wait(Duration::from_millis(10))));
    }

    #[test]
    fn ordinary_close_reports_closed_to_later_operations_and_waits() {
        let (worker, dropped) = local_worker();
        let pending_wait = worker.events.wait(Duration::from_secs(30));
        block_on(worker.control.request_close(|| {})).unwrap();
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
        closed(block_on(worker.call(|_| Ok(()))));
        closed(block_on(worker.submit(true, |_| Ok(()))));
        closed(block_on(pending_wait));
        closed(block_on(worker.events.wait(Duration::from_millis(10))));
        block_on(worker.control.request_close(|| {})).unwrap();
    }

    /// A blocked destructor makes joining observable without timing how long
    /// normal native destruction takes. Both paths use the same created owner.
    fn check_delivered_creation_drop(claimed: bool) {
        // These handoffs involve multiple threads waking and can be slow on
        // a starved runner. Only the held-destructor check tests overlap.
        const READY: Duration = Duration::from_secs(10);

        struct DeliveryWake(Sender<()>);
        impl Wake for DeliveryWake {
            fn wake(self: Arc<Self>) {
                let _ = self.0.send(());
            }
        }
        struct HeldDrop {
            entered: Sender<thread::ThreadId>,
            release: Receiver<()>,
            finished: Sender<()>,
        }
        impl Owner for HeldDrop {
            type Closed = ();
            fn shutdown(
                &mut self,
                _: Instant,
                _: &EventQueue<Self::Event>,
                _: &Control,
                _: &mut [Job<Self>],
            ) {
            }
            type Finish = std::convert::Infallible;
            fn finish_request(&mut self, finish: Self::Finish) {
                match finish {}
            }
            type Event = ();
            fn poll_native(&mut self) -> Result<Option<()>> {
                Ok(None)
            }
        }
        impl Drop for HeldDrop {
            fn drop(&mut self) {
                let _ = self.entered.send(thread::current().id());
                let _ = self.release.recv();
                let _ = self.finished.send(());
            }
        }
        let (create_release, create_gate) = mpsc::channel();
        let (delivered, delivery) = mpsc::channel();
        let (drop_entered, drop_entry) = mpsc::channel();
        let (drop_release, drop_gate) = mpsc::channel();
        let (drop_finished, drop_finish) = mpsc::channel();
        let mut creation = spawn_owner(
            "file-test-creation-handoff",
            move || {
                create_gate.recv_timeout(READY).unwrap();
                Ok(HeldDrop {
                    entered: drop_entered,
                    release: drop_gate,
                    finished: drop_finished,
                })
            },
            AsyncFileOptions::new(),
            |worker| worker,
        );
        let waker = Waker::from(Arc::new(DeliveryWake(delivered)));
        assert!(Pin::new(&mut creation)
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        create_release.send(()).unwrap();
        if let Err(error) = delivery.recv_timeout(READY) {
            drop(drop_release);
            panic!("the owner did not deliver its creation: {error}");
        }
        let (drop_returned, drop_return) = mpsc::channel();
        let dropper = if claimed {
            let worker = block_on(creation).unwrap();
            thread::spawn(move || {
                drop(worker);
                drop_returned.send(()).unwrap();
            })
        } else {
            thread::spawn(move || {
                drop(creation);
                drop_returned.send(()).unwrap();
            })
        };
        let owner_thread = drop_entry.recv_timeout(READY);
        let dropper_thread = dropper.thread().id();
        let returned = drop_return.recv_timeout(Duration::from_secs(2));
        // Always release and join before asserting, including on a regression.
        drop_release.send(()).unwrap();
        drop_finish.recv_timeout(READY).unwrap();
        dropper.join().unwrap();
        assert_ne!(
            owner_thread.expect("owner did not begin destruction"),
            dropper_thread
        );
        assert_eq!(returned.is_ok(), !claimed);
    }

    #[test]
    fn dropping_a_delivered_unclaimed_creation_does_not_join() {
        check_delivered_creation_drop(false);
    }

    #[test]
    fn dropping_a_received_handle_still_joins_its_owner() {
        check_delivered_creation_drop(true);
    }

    #[test]
    fn cancelling_creation_after_its_gate_still_destroys_the_unclaimed_owner() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let created = spawn_owner(
            "file-test-cancel-create",
            move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(LocalOwner {
                    created: thread::current().id(),
                    dropped: dropped_tx,
                    counter: 0,
                    _not_send: Rc::new(()),
                })
            },
            AsyncFileOptions::new(),
            |worker| worker,
        )
        .submit();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(created);
        release_tx.send(()).unwrap();
        assert_ne!(
            dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            thread::current().id()
        );
    }

    fn mock_recorder(closing: Closing) -> (Arc<mock::Recorder>, AsyncFileRecorder) {
        mock_recorder_with(closing, AsyncFileOptions::new().event_capacity(1))
    }

    fn mock_recorder_with(
        closing: Closing,
        options: AsyncFileOptions,
    ) -> (Arc<mock::Recorder>, AsyncFileRecorder) {
        mock_recorder_with_drop_wait(closing, options, Duration::from_secs(5))
    }

    pub(super) fn mock_recorder_with_drop_wait(
        closing: Closing,
        options: AsyncFileOptions,
        drop_wait: Duration,
    ) -> (Arc<mock::Recorder>, AsyncFileRecorder) {
        let device = mock::Recorder::new(closing);
        let installed = Arc::clone(&device);
        let recorder = block_on(spawn_owner(
            "file-test-recorder",
            move || {
                installed.install();
                crate::file::FileRecorder::new(
                    &mock::recorder_api(),
                    &FileRecorderOptions::new(SourceSpec::new("mock source")?).drop_wait(drop_wait),
                )
                .map(RecorderOwner::new)
            },
            options,
            |worker| AsyncFileRecorder { worker },
        ))
        .unwrap();
        block_on(recorder.start(
            &RecordingTarget::new(std::env::temp_dir().join("mock-file-not-created")).unwrap(),
            RecordingTimecode::Preserve,
        ))
        .unwrap();
        (device, recorder)
    }

    #[test]
    fn metadata_commands_snapshot_owned_and_captured_sources_before_submission() {
        use crate::{
            capture::{Guard, MetadataKind},
            sys::bindings::{NDIlib_metadata_frame_t, NDIlib_recv_instance_t},
            MetadataFrame, MetadataFrameRef,
        };
        use std::ffi::CString;

        unsafe fn release(instance: NDIlib_recv_instance_t, _: &mut NDIlib_metadata_frame_t) {
            // SAFETY: the test's counter outlives its synthetic capture guard.
            unsafe { &*instance.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
        }

        let (device, recorder) = mock_recorder(Closing::AtStop);
        let mut owned = MetadataFrame::new("<owned/>")
            .unwrap()
            .with_timecode(Timecode::from_ticks(123));
        let owned_command = recorder.inject_metadata(&owned);
        owned.set_data("<changed/>").unwrap();
        owned.set_timecode(Timecode::from_ticks(999));
        drop(owned);

        let releases = AtomicUsize::new(0);
        let text = CString::new("<captured label='é'/>").unwrap();
        let original_address = text.as_ptr() as usize;
        let raw = NDIlib_metadata_frame_t {
            length: text.as_bytes_with_nul().len() as i32,
            timecode: -456,
            p_data: text.as_ptr().cast_mut(),
        };
        // SAFETY: the text and release counter both outlive the capture guard.
        // The callback counts releases without freeing the borrowed text.
        let captured = unsafe {
            MetadataFrameRef::new(Guard::<MetadataKind>::with_test_free(
                std::ptr::from_ref(&releases).cast_mut().cast(),
                raw,
                release,
            ))
            .unwrap()
        };
        let source: &dyn MetadataSource = &captured;
        let captured_command = recorder.inject_metadata(source);
        drop(captured);
        assert_eq!(releases.load(Ordering::SeqCst), 1);
        drop(text);
        assert!(
            lock(&device.injected).is_empty(),
            "commands are still deferred"
        );

        block_on(owned_command).unwrap();
        block_on(captured_command).unwrap();
        let frames = lock(&device.injected);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].bytes, b"<owned/>\0");
        assert_eq!(frames[0].timecode, 123);
        assert_eq!(frames[1].bytes, "<captured label='é'/>\0".as_bytes());
        assert_eq!(frames[1].timecode, -456);
        assert_ne!(frames[1].address, original_address);
        assert_eq!(frames[1].length as usize, frames[1].bytes.len());
        drop(frames);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn async_source_commands_keep_the_selected_endpoint_after_inputs_are_dropped() {
        use crate::{Source, SourceAddress};
        let device = mock::Recorder::new(Closing::AtStop);
        let installed = Arc::clone(&device);
        let selected = Source::new("STUDIO (Camera 2)")
            .unwrap()
            .with_address(SourceAddress::new("192.0.2.10:5962").unwrap());
        let options = FileRecorderOptions::new(SourceSpec::try_from(&selected).unwrap());
        let recorder = block_on(spawn_owner(
            "file-endpoint-copy",
            move || {
                installed.install();
                crate::file::FileRecorder::new(&mock::recorder_api(), &options)
                    .map(RecorderOwner::new)
            },
            AsyncFileOptions::new(),
            |worker| AsyncFileRecorder { worker },
        ))
        .unwrap();
        drop(selected);
        let command = {
            let selected = Source::new("STUDIO (Program)")
                .unwrap()
                .with_address(SourceAddress::new("ndi://camera.local:5963/path").unwrap());
            let source = SourceSpec::try_from(&selected).unwrap();
            recorder.set_source(&source)
        };
        block_on(command).unwrap();
        let remembered = block_on(recorder.last_accepted_source()).unwrap().unwrap();
        assert_eq!(remembered.name(), None);
        assert_eq!(remembered.url(), Some("ndi://camera.local:5963/path"));
        assert_eq!(
            *device.selected_sources.lock().unwrap(),
            vec![
                (None, Some("192.0.2.10:5962".into())),
                (None, Some("ndi://camera.local:5963/path".into())),
            ]
        );
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn unpolled_recorder_mutations_and_shutdown_requests_do_nothing() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        let source = SourceSpec::new("replacement source").unwrap();
        let commands = [
            recorder.set_source(&source),
            recorder.rollover(&RecordingTarget::new("unused-target").unwrap()),
            recorder.close(),
        ];
        drop(commands);
        drop(recorder.request_stop());
        drop(recorder.finish(Duration::ZERO));
        assert!(!recorder.worker.control.shutdown_requested());
        assert!(block_on(recorder.is_recording()).unwrap());
        assert_eq!(device.source_changes.load(Ordering::Acquire), 0);
        assert_eq!(device.rollovers.load(Ordering::Acquire), 0);
        assert_eq!(device.stops.load(Ordering::Acquire), 0);
        assert_eq!(device.destroys.load(Ordering::Acquire), 0);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn unpolled_finish_does_not_consume_its_wait_budget() {
        // Inspect the job itself before any owner can register or settle it.
        // This makes the submission boundary observable even on a slow runner.
        let (sender, jobs) = mpsc::channel();
        let recorder = AsyncFileRecorder {
            worker: Arc::new(Worker {
                sender,
                admission: Arc::new(Admission::new(1)),
                finish: RecorderFinish::admission(1),
                control: Arc::new(Control::with_outcome()),
                events: Arc::new(EventQueue::new(1)),
                thread: OnceLock::new(),
            }),
        };
        let wait = Duration::from_millis(50);
        let finishing = recorder.finish(wait);
        assert!(matches!(jobs.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let constructed = Instant::now();
        let submitting = loop {
            let now = Instant::now();
            if now > constructed {
                break now;
            }
            thread::yield_now();
        };
        let _finishing = finishing.submit();
        let submitted = Instant::now();
        let Job::Finish { finish, .. } = jobs.try_recv().unwrap() else {
            panic!("finish must enqueue a finish request");
        };
        assert!(finish.deadline.end() >= submitting + wait);
        assert!(finish.deadline.end() <= submitted + wait);
    }

    #[test]
    fn queued_finishes_and_waits_use_the_close_evidence_and_release_admission() {
        for closes_file in [false, true] {
            for last_drop in [false, true] {
                let drop_wait = Duration::from_millis(30);
                let (device, recorder) = mock_recorder_with_drop_wait(
                    if closes_file {
                        Closing::AtStop
                    } else {
                        Closing::Never
                    },
                    AsyncFileOptions::new(),
                    drop_wait,
                );
                let (blocked, release) = block_owner(&recorder.worker);
                let finishing = recorder.finish(Duration::from_secs(30)).submit();
                let waiting = recorder.wait_for_finish(Duration::from_secs(30)).submit();
                let ordinary = recorder.is_recording().submit();
                let admission = Arc::clone(&recorder.worker.admission);
                let finish_admission = Arc::clone(&recorder.worker.finish);
                let control = Arc::clone(&recorder.worker.control);
                let started = Instant::now();
                if last_drop {
                    thread::scope(|scope| {
                        scope.spawn(move || {
                            let limit = Instant::now() + Duration::from_secs(5);
                            while !control.shutdown_requested() {
                                assert!(Instant::now() < limit);
                                thread::yield_now();
                            }
                            release.send(()).unwrap();
                        });
                        drop(recorder);
                    });
                } else {
                    let closing = recorder.close().submit();
                    release.send(()).unwrap();
                    block_on(closing).unwrap();
                }
                block_on(blocked).unwrap();
                if closes_file {
                    assert!(matches!(
                        block_on(finishing).unwrap(),
                        FinishStatus::Ended(SessionEnd::Closed(_))
                    ));
                    assert!(matches!(
                        block_on(waiting).unwrap(),
                        Some(SessionEnd::Closed(_))
                    ));
                    assert!(device.closed_before_destroy.load(Ordering::Acquire));
                } else {
                    closed(block_on(finishing));
                    closed(block_on(waiting));
                    assert!(started.elapsed() >= drop_wait);
                }
                closed(block_on(ordinary));
                assert_eq!(admission.state.load(Ordering::Acquire), 0);
                assert_eq!(finish_admission.state.load(Ordering::Acquire), 0);
                assert_eq!(device.stops.load(Ordering::Acquire), 1);
                assert_eq!(device.destroys.load(Ordering::Acquire), 1);
            }
        }
    }

    #[test]
    fn back_to_back_finish_then_close_or_last_drop_never_loses_the_outcome() {
        for last_drop in [false, true] {
            for _ in 0..200 {
                let (device, recorder) = mock_recorder(Closing::AtStop);
                let finishing = recorder.finish(Duration::from_secs(5)).submit();
                if last_drop {
                    drop(recorder);
                } else {
                    block_on(recorder.close()).unwrap();
                }
                assert!(matches!(
                    block_on(finishing).unwrap(),
                    FinishStatus::Ended(SessionEnd::Closed(_))
                ));
                assert_eq!(device.stops.load(Ordering::Acquire), 1);
                assert_eq!(device.destroys.load(Ordering::Acquire), 1);
                assert!(device.closed_before_destroy.load(Ordering::Acquire));
            }
        }
    }

    #[test]
    fn queued_finishes_keep_their_submission_deadline_during_close() {
        let (_, recorder) = mock_recorder_with_drop_wait(
            Closing::Never,
            AsyncFileOptions::new(),
            Duration::from_secs(1),
        );
        let (blocked, release) = block_owner(&recorder.worker);
        let finishing = recorder.finish(Duration::ZERO).submit();
        let waiting = recorder.wait_for_finish(Duration::ZERO).submit();
        let closing = recorder.close().submit();
        release.send(()).unwrap();
        block_on(blocked).unwrap();
        closed(block_on(finishing));
        closed(block_on(waiting));
        // The expired submissions settle before the longer cleanup budget;
        // closure cannot subsequently turn their earlier result into success.
        assert!(!closing.is_completed());
        block_on(closing).unwrap();
    }

    #[test]
    fn a_close_delayed_by_the_submission_gate_keeps_its_original_cleanup_budget() {
        for explicit_close in [false, true] {
            let budget = Duration::from_millis(20);
            let (device, recorder) = mock_recorder_with_drop_wait(
                Closing::WhenWaitedFor,
                AsyncFileOptions::new(),
                budget,
            );
            let (blocked, release) = block_owner(&recorder.worker);
            let finishing = recorder.finish(Duration::from_secs(30)).submit();
            let control = Arc::clone(&recorder.worker.control);
            let sender = recorder.worker.sender.clone();
            // Supply an already-spent request time to model gate contention
            // without relying on a scheduling interval crossing a deadline.
            let requested = Instant::now() - budget - Duration::from_secs(1);
            let gate = lock(&control.closing);
            let entered = std::sync::Barrier::new(2);
            let closing = thread::scope(|scope| {
                let control = Arc::clone(&control);
                let entered = &entered;
                let publication = scope.spawn(move || {
                    entered.wait();
                    if explicit_close {
                        Some(control.request_close_at(requested, || {
                            let _ = sender.send(Job::Wake);
                        }))
                    } else {
                        assert!(!control.request_shutdown_at(requested));
                        let _ = sender.send(Job::Wake);
                        None
                    }
                });
                entered.wait();
                assert!(!recorder.worker.control.shutdown_requested());
                drop(gate);
                publication.join().unwrap()
            });
            assert_eq!(control.shutdown_requested_at(), Some(requested));
            release.send(()).unwrap();
            block_on(blocked).unwrap();
            closed(block_on(finishing));
            if let Some(closing) = closing {
                block_on(FileOperation::live(closing)).unwrap();
            }
            let limit = Instant::now() + Duration::from_secs(5);
            while device.destroys.load(Ordering::Acquire) == 0 {
                assert!(Instant::now() < limit);
                thread::yield_now();
            }
            assert_eq!(device.waits.load(Ordering::Acquire), 0);
            assert_eq!(device.stops.load(Ordering::Acquire), 1);
            assert!(!device.closed_before_destroy.load(Ordering::Acquire));
        }
    }

    #[test]
    fn an_accepted_finish_is_enqueued_before_shutdown_can_publish() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        let (blocked, release_owner) = block_owner(&recorder.worker);
        let control = Arc::clone(&recorder.worker.control);
        let sender = recorder.worker.sender.clone();
        let place = recorder.worker.admission.admit(true).unwrap();
        let waiter = recorder.worker.finish.admit(false).unwrap();
        let (reply, finishing) = control.command();
        let (entered, admitted) = mpsc::channel();
        let (release_submission, proceed) = mpsc::channel();
        let (closing_sent, closing_received) = mpsc::channel();
        let (close_attempted, attempting_close) = mpsc::channel();
        thread::scope(|scope| {
            let submitting = Arc::clone(&control);
            scope.spawn(move || {
                let submitted = submitting.submit_finish(|| {
                    entered.send(()).unwrap();
                    proceed.recv_timeout(Duration::from_secs(5)).unwrap();
                    sender
                        .send(Job::Finish {
                            finish: RecorderFinish {
                                deadline: Deadline::after(Duration::from_secs(5)).unwrap(),
                                stop: true,
                                reply: FinishResponse::Finish(reply, waiter),
                            },
                            place,
                        })
                        .unwrap_or_else(|_| panic!("owner queue closed"));
                    Live::Immediate(Ok(()))
                });
                block_on(FileOperation::live(submitted)).unwrap();
            });
            admitted.recv_timeout(Duration::from_secs(5)).unwrap();
            let other = recorder.clone();
            scope.spawn(move || {
                close_attempted.send(()).unwrap();
                closing_sent.send(other.close().submit()).unwrap();
            });
            attempting_close
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            assert!(matches!(
                closing_received.recv_timeout(Duration::from_millis(30)),
                Err(RecvTimeoutError::Timeout)
            ));
            assert!(
                !control.shutdown_requested(),
                "shutdown overtook an accepted finish"
            );
            release_submission.send(()).unwrap();
            let closing = closing_received
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            release_owner.send(()).unwrap();
            block_on(blocked).unwrap();
            assert!(matches!(
                block_on(FileOperation::live(finishing)).unwrap(),
                FinishStatus::Ended(SessionEnd::Closed(_))
            ));
            block_on(closing).unwrap();
        });
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn queued_commands_finishes_and_waits_report_owner_failure_after_a_panic() {
        let (_, mut recorder) = mock_recorder(Closing::AtStop);
        let (blocked, release) = block_owner(&recorder.worker);
        let panic = recorder
            .worker
            .call::<()>(|_| panic!("owner failed"))
            .submit();
        let command = recorder.worker.call(|owner| Ok(owner.state())).submit();
        let finishing = recorder.finish(Duration::from_secs(5)).submit();
        let waiting = recorder.wait_for_finish(Duration::from_secs(5)).submit();
        release.send(()).unwrap();
        block_on(blocked).unwrap();
        owner_failed(block_on(panic));
        owner_failed(block_on(command));
        owner_failed(block_on(finishing));
        owner_failed(block_on(waiting));
        owner_failed(block_on(recorder.state()));
        owner_failed(block_on(recorder.finish(Duration::ZERO)));
        owner_failed(block_on(recorder.close()));
        // Failure and close report their outcomes before the owner drops its
        // queued commands. A dropped reply can wake its waiter before the
        // same response's permit drops, so join before checking reclamation.
        Arc::get_mut(&mut recorder.worker)
            .expect("the test owns the only recorder handle")
            .thread
            .take()
            .expect("the owner thread was installed at creation")
            .0
            .join()
            .expect("the owner catches its command's panic");
        assert_eq!(recorder.worker.finish.state.load(Ordering::Acquire), 0);
    }

    /// Wait until the owner thread has stopped `device`'s session.
    fn await_stop(device: &mock::Recorder) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while device.stops.load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < deadline, "the recorder was never stopped");
            thread::sleep(Duration::from_millis(1));
        }
    }

    pub(super) fn await_finish(recorder: &AsyncFileRecorder) {
        // A one-place command queue may still hold the finish itself. Wait
        // for its command place before enqueueing this registration barrier.
        let deadline = Instant::now() + Duration::from_secs(5);
        while recorder.worker.admission.state.load(Ordering::Acquire) != 0 {
            assert!(Instant::now() < deadline, "finish command never started");
            thread::yield_now();
        }
        // A following command proves registration returned to the pump. No
        // native blocking capture is needed to keep a finish pending.
        assert!(block_on(recorder.worker.call(|owner| Ok(owner.pending_finishes()))).unwrap() > 0);
    }

    #[test]
    fn a_registered_finish_releases_admission_for_sixty_four_status_commands() {
        let (device, recorder) =
            mock_recorder_with_drop_wait(Closing::Never, AsyncFileOptions::new(), Duration::ZERO);
        let finishing = recorder.finish(Duration::from_secs(2)).submit();
        await_finish(&recorder);
        let commands: Vec<_> = (0..64).map(|_| recorder.stats().submit()).collect();
        for command in commands {
            block_on(command).unwrap();
        }
        assert!(!finishing.is_completed());
        assert_eq!(device.waits.load(Ordering::Acquire), 0);
        drop(finishing);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    #[ignore = "manual saturated finish-wait allocation and bookkeeping measurement"]
    fn finish_wait_saturation_measurement() {
        const ATTEMPTS: usize = 16_384;
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::Never,
            AsyncFileOptions::new().command_capacity(1),
            Duration::ZERO,
        );
        let pending = recorder.finish(crate::MAX_TIMEOUT).submit();
        await_finish(&recorder);
        let before = block_on(recorder.worker.call(|owner| Ok(owner.finish_storage()))).unwrap();
        for batch in 0..4 {
            let started = Instant::now();
            let (((), allocations), peak) = crate::test_alloc::peak_bytes(|| {
                crate::test_alloc::allocation_count(|| {
                    for _ in 0..ATTEMPTS {
                        assert!(matches!(
                            block_on(recorder.wait_for_finish(crate::MAX_TIMEOUT)),
                            Err(Error::QueueFull(QueueLimit::FileFinishWaits))
                        ));
                    }
                })
            });
            let elapsed = started.elapsed();
            let events = Arc::clone(&recorder.worker.events);
            let control = Arc::clone(&recorder.worker.control);
            let (storage, bookkeeping_allocations, bookkeeping_time) =
                block_on(recorder.worker.call(move |owner| {
                    let started = Instant::now();
                    let (_, allocations) = crate::test_alloc::allocation_count(|| {
                        for _ in 0..ATTEMPTS {
                            let _ = owner.wait_timeout();
                            owner.after_events(&events, &control);
                        }
                    });
                    Ok((owner.finish_storage(), allocations, started.elapsed()))
                }))
                .unwrap();
            assert_eq!(storage, before, "saturation must not grow registry storage");
            assert_eq!(storage.0, 1);
            assert_eq!(recorder.worker.finish.state.load(Ordering::Acquire), 2);
            assert_eq!(device.waits.load(Ordering::Acquire), 0);
            println!(
                "finish saturation batch={batch} attempts={ATTEMPTS} registry_len={} registry_capacity={} submission_allocations={allocations} submission_peak_bytes={peak} submission_time_us={} bookkeeping_allocations={bookkeeping_allocations} bookkeeping_time_us={}",
                storage.0,
                storage.1,
                elapsed.as_micros(),
                bookkeeping_time.as_micros(),
            );
        }
        drop(pending);
        block_on(recorder.close()).unwrap();
        assert_eq!(recorder.worker.finish.state.load(Ordering::Acquire), 0);
    }

    #[test]
    fn zero_timeout_finish_takes_the_closure_behind_a_full_event_burst() {
        let (device, recorder) = mock_recorder_with(Closing::AtStop, AsyncFileOptions::new());
        for _ in 0..40 {
            device.push(mock::FIRST_FRAME, mock::TAKE);
        }
        assert!(matches!(
            block_on(recorder.finish(Duration::ZERO)).unwrap(),
            FinishStatus::Ended(SessionEnd::Closed(_))
        ));
        assert_eq!(device.waits.load(Ordering::Acquire), 0);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn condition_wait_timeout_retains_report_and_a_later_wait_observes_closure() {
        let (device, recorder) = mock_recorder(Closing::WhenTold);
        block_on(recorder.request_stop()).unwrap();
        let before = block_on(recorder.report()).unwrap();
        let expired = recorder.wait_for_finish(Duration::ZERO);
        assert!(
            !expired.is_completed(),
            "an unpolled wait must remain deferred"
        );
        assert!(block_on(expired).unwrap().is_none());
        assert_eq!(block_on(recorder.report()).unwrap(), before);
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        let waiting = recorder.wait_for_finish(Duration::from_secs(5)).submit();
        await_finish(&recorder);
        assert_eq!(
            block_on(recorder.report()).unwrap(),
            before,
            "report commands must remain available during a pending wait"
        );
        device.close();
        let Some(SessionEnd::Closed(report)) = block_on(waiting).unwrap() else {
            panic!("the later wait must observe confirmed closure");
        };
        assert!(report.closure_confirmed());
        assert_eq!(report, block_on(recorder.report()).unwrap());
        block_on(recorder.close()).unwrap();
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn condition_wait_reports_an_indeterminate_session_as_a_settled_outcome() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::AtStop,
            AsyncFileOptions::new(),
            Duration::from_millis(60),
        );
        device.dropped_events.store(1, Ordering::Release);
        block_on(recorder.request_stop()).unwrap();
        let Some(SessionEnd::Indeterminate(report)) =
            block_on(recorder.wait_for_finish(Duration::ZERO)).unwrap()
        else {
            panic!("the lost SDK event prevents confirmation, not settlement");
        };
        assert_eq!(report.dropped_events(), 1);
        assert_eq!(report, block_on(recorder.report()).unwrap());
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn concurrent_finish_waiters_keep_their_deadlines_and_share_the_closure() {
        let (device, recorder) = mock_recorder(Closing::WhenTold);
        let first = recorder.finish(Duration::from_secs(5)).submit();
        await_finish(&recorder);
        let expired = recorder.wait_for_finish(Duration::ZERO).submit();
        assert!(block_on(expired).unwrap().is_none());
        let second = recorder.wait_for_finish(Duration::from_secs(5)).submit();
        assert_eq!(
            block_on(recorder.worker.call(|owner| Ok(owner.pending_finishes()))).unwrap(),
            2
        );
        device.close();
        let first = block_on(first).unwrap();
        let second = block_on(second).unwrap();
        let FinishStatus::Ended(first @ SessionEnd::Closed(_)) = first else {
            panic!("expected confirmed closure, got {first:?}");
        };
        assert_eq!(Some(first), second);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn cancelled_waiters_are_pruned_without_cancelling_other_finishes() {
        let (device, recorder) = mock_recorder(Closing::WhenTold);
        let cancelled = recorder.finish(Duration::from_secs(5)).submit();
        await_finish(&recorder);
        let retained = recorder.wait_for_finish(Duration::from_secs(5)).submit();
        drop(cancelled);
        // The owner may already have pruned this iteration before cancellation.
        // Complete one command so the next inspection follows a fresh pump.
        block_on(recorder.worker.call(|_| Ok(()))).unwrap();
        assert_eq!(
            block_on(recorder.worker.call(|owner| Ok(owner.pending_finishes()))).unwrap(),
            1
        );
        device.close();
        assert!(matches!(
            block_on(retained).unwrap(),
            Some(SessionEnd::Closed(_))
        ));
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn reset_ends_old_waiters_and_a_failed_reset_preserves_them() {
        let (device, recorder) = mock_recorder(Closing::WhenTold);
        let finishing = recorder.finish(Duration::from_secs(5)).submit();
        await_finish(&recorder);
        let replacement = mock::Recorder::new(Closing::AtStop);
        replacement.refuse_creation.store(true, Ordering::Release);
        let installed = Arc::clone(&replacement);
        block_on(recorder.worker.call(move |_| {
            installed.install();
            Ok(())
        }))
        .unwrap();
        let options = FileRecorderOptions::new(SourceSpec::new("replacement").unwrap());
        assert!(block_on(recorder.reset(&options)).is_err());
        assert!(!finishing.is_completed());
        assert_eq!(recorder.worker.finish.state.load(Ordering::Acquire), 2);
        replacement.refuse_creation.store(false, Ordering::Release);
        let installed = Arc::clone(&replacement);
        block_on(recorder.worker.call(move |_| {
            installed.install();
            Ok(())
        }))
        .unwrap();
        block_on(recorder.reset(&options)).unwrap();
        assert_eq!(recorder.worker.finish.state.load(Ordering::Acquire), 0);
        assert!(matches!(
            block_on(finishing),
            Err(Error::InvalidState(StateConflict::Closed))
        ));
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert_eq!(
            block_on(recorder.start(
                &RecordingTarget::new(std::env::temp_dir().join("mock-replaced")).unwrap(),
                RecordingTimecode::Preserve,
            ))
            .unwrap(),
            2
        );
        assert!(matches!(
            block_on(recorder.finish(Duration::ZERO)).unwrap(),
            FinishStatus::Ended(SessionEnd::Closed(_))
        ));
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn an_owner_exit_answers_commands_events_and_every_close_with_one_outcome() {
        for outcome in [OwnerExit::Closed, OwnerExit::Failed] {
            let control = Arc::new(Control::new());
            let events = Arc::new(EventQueue::<()>::new(1));
            let (reply, command) = control.command::<()>();
            let closing = control.request_close(|| {}).submit();
            control.exit(
                if outcome == OwnerExit::Closed {
                    Ok(())
                } else {
                    Err(outcome)
                },
                &events,
            );
            drop(reply);
            for result in [
                block_on(FileOperation::live(command)),
                block_on(events.wait(Duration::from_secs(30))).map(|_| ()),
            ] {
                assert!(matches!(
                    result,
                    Err(Error::InvalidState(error)) if error == if outcome == OwnerExit::Failed {
                        StateConflict::OwnerFailed
                    } else {
                        StateConflict::Closed
                    }
                ));
            }
            for result in [block_on(closing), block_on(control.request_close(|| {}))] {
                if outcome == OwnerExit::Failed {
                    assert!(matches!(
                        result,
                        Err(Error::InvalidState(StateConflict::OwnerFailed))
                    ));
                } else {
                    assert!(result.is_ok());
                }
            }
        }
    }

    #[test]
    fn closes_racing_an_owner_exit_are_never_lost() {
        for outcome in [OwnerExit::Closed, OwnerExit::Failed] {
            for _ in 0..32 {
                let control = Arc::new(Control::new());
                let barrier = Arc::new(std::sync::Barrier::new(5));
                thread::scope(|scope| {
                    for _ in 0..4 {
                        let control = Arc::clone(&control);
                        let barrier = Arc::clone(&barrier);
                        scope.spawn(move || {
                            barrier.wait();
                            let closes: Vec<_> = (0..8)
                                .map(|_| control.request_close(|| {}).submit())
                                .collect();
                            for close in closes {
                                let result = block_on(close);
                                assert_eq!(result.is_ok(), outcome == OwnerExit::Closed);
                            }
                        });
                    }
                    barrier.wait();
                    control.exit(
                        if outcome == OwnerExit::Closed {
                            Ok(())
                        } else {
                            Err(outcome)
                        },
                        &EventQueue::<()>::new(1),
                    );
                });
            }
        }
    }

    #[test]
    fn published_close_outcomes_answer_late_requests_before_queued_replies_are_drained() {
        struct PauseOnWake {
            published: Arc<std::sync::Barrier>,
            resume: Arc<std::sync::Barrier>,
        }
        impl Wake for PauseOnWake {
            fn wake(self: Arc<Self>) {
                self.published.wait();
                self.resume.wait();
            }
        }
        struct CountWake(AtomicUsize);
        impl Wake for CountWake {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::AcqRel);
            }
        }

        let (_, recorder) = mock_recorder(Closing::AtStop);
        let report = block_on(recorder.abandon()).unwrap();
        for outcome in [OwnerExit::Closed, OwnerExit::Failed] {
            let control = Arc::new(Control::<RecordingReport>::with_outcome());
            let events = Arc::new(EventQueue::<()>::new(1));
            let mut early = control.request_close(|| {}).submit();
            let wakes = Arc::new(CountWake(AtomicUsize::new(0)));
            let waker = Waker::from(Arc::clone(&wakes));
            assert!(Pin::new(&mut early)
                .poll(&mut Context::from_waker(&waker))
                .is_pending());
            let published = Arc::new(std::sync::Barrier::new(2));
            let resume = Arc::new(std::sync::Barrier::new(2));
            let waker = Waker::from(Arc::new(PauseOnWake {
                published: Arc::clone(&published),
                resume: Arc::clone(&resume),
            }));
            let mut event = events.wait(Duration::from_secs(60));
            assert!(Pin::new(&mut event)
                .poll(&mut Context::from_waker(&waker))
                .is_pending());
            thread::scope(|scope| {
                let closing = scope.spawn(|| {
                    control.exit(
                        if outcome == OwnerExit::Closed {
                            Ok(report.clone())
                        } else {
                            Err(outcome)
                        },
                        &events,
                    );
                });
                // EventQueue wakes after outcome publication and before exit
                // takes the close queue lock. Hold that exact interleaving.
                published.wait();
                let checked = catch_unwind(AssertUnwindSafe(|| {
                    assert!(control.closed.get().is_some());
                    assert!(!early.is_completed());
                    assert_eq!(lock(&control.closing).replies.len(), 1);
                    let late_close =
                        block_on(control.request_close(|| panic!("late close woke owner")));
                    let late_abandon =
                        block_on(control.request_abandon(|| panic!("late abandon woke owner")));
                    if outcome == OwnerExit::Closed {
                        late_close.unwrap();
                        assert_eq!(late_abandon.unwrap(), report);
                    } else {
                        owner_failed(late_close);
                        owner_failed(late_abandon);
                    }
                    // A completed abandon answers from the outcome, without
                    // changing the close request that cleanup already consumed.
                    assert!(matches!(
                        control.close_request(),
                        super::super::recorder::CleanupRequest::Close
                    ));
                    assert!(!control.needs_cleanup_join());
                    assert_eq!(lock(&control.closing).replies.len(), 1);
                    assert!(!early.is_completed());
                }));
                resume.wait();
                closing.join().unwrap();
                checked.unwrap();
            });
            let early = block_on(early);
            if outcome == OwnerExit::Closed {
                early.unwrap();
            } else {
                owner_failed(early);
            }
            assert_eq!(wakes.0.load(Ordering::Acquire), 1);
            assert!(lock(&control.closing).replies.is_empty());
            // Repeated exit does not answer the drained replies again.
            control.exit(Err(OwnerExit::Failed), &events);
            assert_eq!(wakes.0.load(Ordering::Acquire), 1);
        }
    }

    #[test]
    fn ordinary_early_and_late_closes_borrow_the_published_outcome_without_cloning() {
        struct CountedOutcome(Arc<AtomicUsize>);
        impl Clone for CountedOutcome {
            fn clone(&self) -> Self {
                self.0.fetch_add(1, Ordering::AcqRel);
                Self(Arc::clone(&self.0))
            }
        }
        impl CloseOutcome for CountedOutcome {
            type Request = ();
            fn initial_request() {}
        }
        let clones = Arc::new(AtomicUsize::new(0));
        let control = Arc::new(Control::<CountedOutcome>::with_outcome());
        let early: Vec<_> = (0..4)
            .map(|_| control.request_close(|| {}).submit())
            .collect();
        control.exit(
            Ok(CountedOutcome(Arc::clone(&clones))),
            &EventQueue::<()>::new(1),
        );
        for close in early {
            block_on(close).unwrap();
        }
        for _ in 0..4 {
            block_on(control.request_close(|| panic!("completed close woke owner"))).unwrap();
        }
        assert_eq!(clones.load(Ordering::Acquire), 0);
        // Only a caller explicitly requesting an owned outcome needs a clone.
        let (reply, future) = operation();
        control.register_close(Instant::now(), CloseReply::Outcome(reply), |_| {}, || {});
        let _outcome = block_on(FileOperation::live(future)).unwrap();
        assert_eq!(clones.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_failed_recorder_reports_failure_before_cleanup_but_closes_after_destruction() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::WhenTold,
            AsyncFileOptions::new(),
            Duration::from_secs(5),
        );
        block_on(recorder.next_event(Duration::from_secs(1)))
            .unwrap()
            .unwrap();
        let finishing = recorder.finish(Duration::from_secs(30)).submit();
        await_finish(&recorder);
        let mut event = Box::pin(recorder.next_event(Duration::from_secs(30)));
        assert!(event
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending());
        let command = recorder
            .worker
            .call(|_| -> Result<()> { panic!("owner failure window") })
            .submit();
        let event_result = block_on(event);
        let finish_result = block_on(finishing);
        let destroys_before = device.destroys.load(Ordering::Acquire);
        let refused = recorder.report().submit();
        let refused_immediately = refused.is_completed();
        let closing = recorder.close().submit();
        let close_waited = !closing.is_completed();
        let abandoning = recorder.abandon().submit();
        assert!(
            !abandoning.is_completed(),
            "failure completed abandon before destruction"
        );
        device.close();
        let close_result = block_on(closing);
        owner_failed(block_on(abandoning));
        assert_eq!(destroys_before, 0);
        assert!(refused_immediately);
        assert!(
            close_waited,
            "failure publication completed close before destruction"
        );
        for result in [
            event_result.map(|_| ()),
            finish_result.map(|_| ()),
            block_on(command),
            block_on(refused).map(|_| ()),
            close_result,
        ] {
            assert!(matches!(
                result,
                Err(Error::InvalidState(StateConflict::OwnerFailed))
            ));
        }
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_failed_player_delivers_its_event_failure_before_native_destruction() {
        static DESTROYS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn create(
            _: *const crate::sys::bindings::NDIlib_file_player_create_t,
        ) -> crate::sys::bindings::NDIlib_file_player_instance_t {
            std::ptr::NonNull::dangling().as_ptr()
        }
        unsafe extern "C" fn destroy(_: crate::sys::bindings::NDIlib_file_player_instance_t) {
            DESTROYS.fetch_add(1, Ordering::Release);
        }
        struct FailureWake {
            event: Mutex<FileEventFuture<PlayerEvent>>,
            observed: Sender<(bool, usize)>,
        }
        impl Wake for FailureWake {
            fn wake(self: Arc<Self>) {
                let failed = matches!(
                    Pin::new(&mut *lock(&self.event)).poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Ready(Err(Error::InvalidState(StateConflict::OwnerFailed)))
                );
                let _ = self
                    .observed
                    .send((failed, DESTROYS.load(Ordering::Acquire)));
            }
        }
        let api = FileApi::with_tables(
            crate::sys::FilePlayback {
                NDIlib_file_player_create: create,
                NDIlib_file_player_destroy: destroy,
                ..mock::playback()
            },
            mock::recording(),
        );
        let player = block_on(AsyncFilePlayer::create(
            &api,
            &FilePlayerOptions::new("failed player"),
        ))
        .unwrap();
        let (observed, receive) = mpsc::channel();
        let task = Arc::new(FailureWake {
            event: Mutex::new(player.next_event(Duration::from_secs(30))),
            observed,
        });
        let waker = Waker::from(Arc::clone(&task));
        assert!(Pin::new(&mut *lock(&task.event))
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        let failed = player
            .worker
            .call(|_| -> Result<()> { panic!("player owner failure") })
            .submit();
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(5)).unwrap(),
            (true, 0)
        );
        assert!(matches!(
            block_on(failed),
            Err(Error::InvalidState(StateConflict::OwnerFailed))
        ));
        assert!(matches!(
            block_on(player.close()),
            Err(Error::InvalidState(StateConflict::OwnerFailed))
        ));
        assert_eq!(DESTROYS.load(Ordering::Acquire), 1);
    }

    #[test]
    fn owner_failure_completes_every_registered_finish_before_native_cleanup() {
        let (_device, recorder) =
            mock_recorder_with_drop_wait(Closing::Never, AsyncFileOptions::new(), Duration::ZERO);
        let first = recorder.finish(Duration::from_secs(5)).submit();
        await_finish(&recorder);
        let second = recorder.wait_for_finish(Duration::from_secs(5)).submit();
        let failed = recorder
            .worker
            .call(|_| -> Result<()> { panic!("owner failure") })
            .submit();
        for result in [block_on(first).map(|_| ()), block_on(second).map(|_| ())] {
            assert!(matches!(
                result,
                Err(Error::InvalidState(StateConflict::OwnerFailed))
            ));
        }
        assert!(matches!(
            block_on(failed),
            Err(Error::InvalidState(StateConflict::OwnerFailed))
        ));
        assert!(matches!(
            block_on(recorder.close()),
            Err(Error::InvalidState(StateConflict::OwnerFailed))
        ));
        assert_eq!(recorder.worker.finish.state.load(Ordering::Acquire), 0);
    }

    #[test]
    fn finish_and_wait_report_closure_when_another_clone_closes_within_the_shared_budget() {
        fn check<T: std::fmt::Debug>(
            stop: bool,
            start: impl FnOnce(&AsyncFileRecorder) -> FileOperation<T>,
            is_closed: impl FnOnce(&T) -> bool,
        ) {
            let (device, recorder) = mock_recorder_with_drop_wait(
                Closing::WhenTold,
                AsyncFileOptions::new(),
                Duration::from_secs(2),
            );
            if !stop {
                block_on(recorder.request_stop()).unwrap();
            }
            let finishing = start(&recorder).submit();
            await_finish(&recorder);
            let other = recorder.clone();
            let closed = other.close().submit();
            // The original interruption path returned TimedOut within one
            // 25 ms poll. Keep closure withheld past that point.
            thread::sleep(Duration::from_millis(80));
            assert!(!finishing.is_completed());
            device.close();
            let outcome = block_on(finishing).unwrap();
            assert!(is_closed(&outcome), "{outcome:?}");
            block_on(closed).unwrap();
            assert!(device.closed_before_destroy.load(Ordering::Acquire));
            assert_eq!(device.destroys.load(Ordering::Acquire), 1);
            assert!(matches!(
                block_on(recorder.wait_for_finish(Duration::ZERO)),
                Err(Error::InvalidState(StateConflict::Closed))
            ));
        }
        check(
            true,
            |recorder| recorder.finish(Duration::from_secs(30)),
            |outcome| matches!(outcome, FinishStatus::Ended(SessionEnd::Closed(_))),
        );
        check(
            false,
            |recorder| recorder.wait_for_finish(Duration::from_secs(30)),
            |outcome| matches!(outcome, Some(SessionEnd::Closed(_))),
        );
    }

    #[test]
    fn close_spends_one_drop_wait_and_reports_closed_instead_of_a_false_timeout() {
        struct ObserveFinish {
            device: Arc<mock::Recorder>,
            replied: Sender<(usize, Instant)>,
        }

        impl Wake for ObserveFinish {
            fn wake(self: Arc<Self>) {
                // Reply completion calls this on the owner, before it can
                // proceed to destruction. Caller scheduling cannot move the
                // observation past a mistakenly restarted cleanup wait.
                let _ = self
                    .replied
                    .send((self.device.waits.load(Ordering::Acquire), Instant::now()));
            }
        }

        let budget = Duration::from_millis(160);
        let (device, recorder) =
            mock_recorder_with_drop_wait(Closing::WhenTold, AsyncFileOptions::new(), budget);
        let mut finishing = Box::pin(recorder.finish(Duration::from_secs(30)).submit());
        await_finish(&recorder);
        let (replied, reply) = mpsc::channel();
        let waker = Waker::from(Arc::new(ObserveFinish {
            device: Arc::clone(&device),
            replied,
        }));
        assert!(finishing
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        let closed = recorder.close().submit();
        let requested = recorder.worker.control.shutdown_requested_at().unwrap();
        let (waits_at_reply, replied_at) = reply.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            block_on(finishing),
            Err(Error::InvalidState(StateConflict::Closed))
        ));
        block_on(closed).unwrap();
        let elapsed = replied_at.duration_since(requested);
        assert!(
            elapsed >= budget,
            "shutdown skipped its finalization wait: {elapsed:?}"
        );
        assert_eq!(
            device.waits.load(Ordering::Acquire),
            waits_at_reply,
            "destruction started another native event wait after the finish reply"
        );
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert!(!device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn an_in_flight_finish_still_reports_indeterminate_closure_and_delivers_events_during_close() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::WhenTold,
            AsyncFileOptions::new().event_capacity(1),
            Duration::from_secs(2),
        );
        let finishing = recorder.finish(Duration::from_secs(30)).submit();
        await_finish(&recorder);
        let closing = recorder.close().submit();
        device.dropped_events.store(1, Ordering::Release);
        device.close();
        let outcome = block_on(finishing).unwrap();
        assert!(
            matches!(outcome, FinishStatus::Ended(SessionEnd::Indeterminate(_))),
            "{outcome:?}"
        );
        block_on(closing).unwrap();
        // The one-slot queue retains the closure taken during the finish.
        let event = block_on(recorder.next_event(Duration::ZERO))
            .unwrap()
            .unwrap();
        assert_eq!(event.kind(), RecorderEventKind::FileClosed);
    }

    #[test]
    fn an_in_flight_finish_keeps_its_earlier_deadline_when_a_clone_closes() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::After(Duration::from_millis(300)),
            AsyncFileOptions::new(),
            Duration::from_secs(2),
        );
        let finishing = recorder.finish(Duration::from_millis(120)).submit();
        await_finish(&recorder);
        let began = Instant::now();
        let closed = recorder.close().submit();
        assert!(matches!(
            block_on(finishing),
            Err(Error::InvalidState(StateConflict::Closed))
        ));
        assert!(
            began.elapsed() < Duration::from_millis(500),
            "close extended the finish's deadline"
        );
        assert_eq!(device.destroys.load(Ordering::Acquire), 0);
        block_on(closed).unwrap();
        assert!(began.elapsed() < Duration::from_secs(2));
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn sync_drop_keeps_an_indeterminate_recorder_until_its_known_file_closes() {
        let device = mock::Recorder::new(Closing::StoppedBeforeClose(Duration::from_millis(100)));
        device.install();
        let mut recorder = crate::file::FileRecorder::new(
            &mock::recorder_api(),
            &FileRecorderOptions::new(SourceSpec::new("mock").unwrap())
                .drop_wait(Duration::from_secs(2)),
        )
        .unwrap();
        let target = RecordingTarget::new(std::env::temp_dir().join("mock-cleanup")).unwrap();
        recorder
            .start(&target, RecordingTimecode::Preserve)
            .unwrap();
        recorder.next_event(Duration::ZERO).unwrap();
        device.dropped_events.store(1, Ordering::Release);
        let outcome = recorder.finish(Duration::ZERO).unwrap();
        assert!(matches!(
            outcome,
            FinishStatus::Ended(SessionEnd::Indeterminate(_))
        ));
        assert!(matches!(
            recorder.start(&target, RecordingTimecode::Preserve),
            Err(Error::InvalidState(StateConflict::IncompleteFileTracking))
        ));
        drop(recorder);
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn async_close_keeps_an_indeterminate_recorder_until_its_known_file_closes() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::StoppedBeforeClose(Duration::from_millis(100)),
            AsyncFileOptions::new(),
            Duration::from_secs(2),
        );
        device.dropped_events.store(1, Ordering::Release);
        let outcome = block_on(recorder.finish(Duration::ZERO)).unwrap();
        assert!(matches!(
            outcome,
            FinishStatus::Ended(SessionEnd::Indeterminate(_))
        ));
        block_on(recorder.close()).unwrap();
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn a_known_file_closed_after_an_indeterminate_report_still_requires_reset() {
        let device = mock::Recorder::new(Closing::Never);
        device.install();
        let mut recorder = crate::file::FileRecorder::new(
            &mock::recorder_api(),
            &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()),
        )
        .unwrap();
        let target = RecordingTarget::new(std::env::temp_dir().join("mock-restart")).unwrap();
        recorder
            .start(&target, RecordingTimecode::Preserve)
            .unwrap();
        recorder.next_event(Duration::ZERO).unwrap();
        device.dropped_events.store(1, Ordering::Release);
        assert!(matches!(
            recorder.finish(Duration::ZERO).unwrap(),
            FinishStatus::Ended(SessionEnd::Indeterminate(_))
        ));
        let report = recorder.report();
        assert!(matches!(
            recorder.start(&target, RecordingTimecode::Preserve),
            Err(Error::InvalidState(StateConflict::IncompleteFileTracking))
        ));
        device.close();
        recorder.next_event(Duration::ZERO).unwrap();
        assert_eq!(recorder.report(), report);
        assert!(matches!(
            recorder.start(&target, RecordingTimecode::Preserve),
            Err(Error::InvalidState(StateConflict::IncompleteFileTracking))
        ));
        assert_eq!(device.starts.load(Ordering::Acquire), 1);
        let replacement = mock::Recorder::new(Closing::AtStop);
        replacement.install();
        assert_eq!(
            recorder
                .reset(&FileRecorderOptions::new(
                    SourceSpec::new("replacement").unwrap()
                ))
                .unwrap(),
            report
        );
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert_eq!(
            recorder
                .start(&target, RecordingTimecode::Preserve)
                .unwrap(),
            2
        );
        recorder.abandon();
    }

    #[test]
    fn an_indeterminate_recorder_with_a_lost_close_spends_only_one_cleanup_budget() {
        let budget = Duration::from_millis(60);
        let (device, recorder) =
            mock_recorder_with_drop_wait(Closing::Never, AsyncFileOptions::new(), budget);
        device.dropped_events.store(1, Ordering::Release);
        assert!(matches!(
            block_on(recorder.finish(Duration::ZERO)).unwrap(),
            FinishStatus::Ended(SessionEnd::Indeterminate(_))
        ));
        let began = Instant::now();
        block_on(recorder.close()).unwrap();
        assert!(began.elapsed() >= budget);
        assert!(began.elapsed() < Duration::from_secs(1));
        assert!(!device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn async_close_spends_its_budget_after_a_lost_open_and_every_known_close() {
        for delayed_hidden_close in [false, true] {
            let budget = Duration::from_secs(2);
            let (device, recorder) = mock_recorder_with_drop_wait(
                Closing::AtStop,
                AsyncFileOptions::new().event_capacity(1),
                budget,
            );
            let started = block_on(recorder.next_event(Duration::from_secs(1)))
                .unwrap()
                .unwrap();
            assert_eq!(started.kind(), RecorderEventKind::Started);
            // The missing event is an open for hidden.mkv. The only known
            // path closes at stop, leaving no known path to await.
            device.dropped_events.store(1, Ordering::Release);
            let FinishStatus::Ended(SessionEnd::Indeterminate(report)) =
                block_on(recorder.finish(Duration::ZERO)).unwrap()
            else {
                panic!("losing an open must prevent confirmed closure");
            };
            assert_eq!(report.closed_files(), 1);
            assert_eq!(report.dropped_events(), 1);
            assert_eq!(block_on(recorder.reported_current_file()).unwrap(), None);
            let target =
                RecordingTarget::new(std::env::temp_dir().join("hidden-open-restart")).unwrap();
            assert!(matches!(
                block_on(recorder.start(&target, RecordingTimecode::Preserve)),
                Err(Error::InvalidState(StateConflict::IncompleteFileTracking))
            ));
            assert_eq!(device.starts.load(Ordering::Acquire), 1);

            if !delayed_hidden_close {
                device.push(mock::FILE_CLOSED, "hidden.mkv");
                // A following command runs after the event pump. Later
                // cleanup evidence cannot rewrite the settled report.
                assert_eq!(block_on(recorder.report()).unwrap(), report);
            }
            let captures = device.captures.load(Ordering::Acquire);
            let waits = device.waits.load(Ordering::Acquire);
            let (blocked, release) = block_owner(&recorder.worker);
            // Retain a queued finish so cleanup keeps delivering native
            // events even though the immutable report has already settled.
            let finishing = recorder.wait_for_finish(Duration::from_secs(30)).submit();
            let closed = recorder.close().submit();
            let requested = recorder.worker.control.shutdown_requested_at().unwrap();
            release.send(()).unwrap();
            block_on(blocked).unwrap();
            assert_eq!(
                block_on(finishing).unwrap(),
                Some(SessionEnd::Indeterminate(report))
            );
            if delayed_hidden_close {
                let limit = Instant::now() + Duration::from_secs(5);
                while device.waits.load(Ordering::Acquire) == waits {
                    assert!(
                        Instant::now() < limit,
                        "cleanup never waited for hidden files"
                    );
                    assert_eq!(device.destroys.load(Ordering::Acquire), 0);
                    thread::yield_now();
                }
                device.push(mock::FILE_CLOSED, "hidden.mkv");
            }
            block_on(closed).unwrap();
            assert!(
                requested.elapsed() >= budget,
                "a known-empty incomplete tracker shortened cleanup"
            );
            assert!(device.captures.load(Ordering::Acquire) > captures);
            let event = block_on(recorder.next_event(Duration::ZERO))
                .unwrap()
                .unwrap();
            assert_eq!(event.kind(), RecorderEventKind::FileClosed);
            assert_eq!(event.path().to_str().unwrap(), "hidden.mkv");
            assert_eq!(device.stops.load(Ordering::Acquire), 1);
            assert_eq!(device.destroys.load(Ordering::Acquire), 1);
            block_on(recorder.close()).unwrap();
            drop(recorder);
            assert_eq!(device.stops.load(Ordering::Acquire), 1);
            assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        }
    }

    #[test]
    fn async_indeterminate_reports_count_only_files_confirmed_empty_before_loss() {
        for empty_before_loss in [false, true] {
            let (device, recorder) = mock_recorder_with_drop_wait(
                Closing::Never,
                AsyncFileOptions::new(),
                Duration::ZERO,
            );
            block_on(recorder.next_event(Duration::from_secs(1))).unwrap();
            if empty_before_loss {
                device.push(mock::FILE_CLOSED, mock::TAKE);
                let limit = Instant::now() + Duration::from_secs(1);
                loop {
                    let report = block_on(recorder.report()).unwrap();
                    if report.closed_files() == 1 {
                        assert_eq!(report.empty_files(), 1);
                        break;
                    }
                    assert!(
                        Instant::now() < limit,
                        "the complete close was never observed"
                    );
                }
            }
            device.dropped_events.store(1, Ordering::Release);
            if !empty_before_loss {
                device.push(mock::FILE_CLOSED, mock::TAKE);
            }
            device.push(mock::ROLLOVER, "hidden.mkv");
            device.push(mock::FILE_CLOSED, "hidden.mkv");
            let FinishStatus::Ended(SessionEnd::Indeterminate(report)) =
                block_on(recorder.finish(Duration::ZERO)).unwrap()
            else {
                panic!("lost file evidence prevents confirmed closure");
            };
            assert_eq!(report.empty_files(), u64::from(empty_before_loss));
            assert_eq!(report.closed_files(), 2);
            assert_eq!(report.dropped_events(), 1);
            block_on(recorder.abandon()).unwrap();
        }
    }

    #[test]
    fn async_start_distinguishes_active_sessions_from_incomplete_tracking() {
        let (device, recorder) =
            mock_recorder_with_drop_wait(Closing::Never, AsyncFileOptions::new(), Duration::ZERO);
        let target = RecordingTarget::new(std::env::temp_dir().join("async-admission")).unwrap();
        for stopping in [false, true] {
            if stopping {
                block_on(recorder.request_stop()).unwrap();
            }
            assert!(matches!(
                block_on(recorder.start(&target, RecordingTimecode::Preserve)),
                Err(Error::InvalidState(StateConflict::PreviousSessionOpen))
            ));
        }
        device.close();
        let FinishStatus::Ended(SessionEnd::Closed(report)) =
            block_on(recorder.finish(Duration::from_secs(1))).unwrap()
        else {
            panic!("the known file closed");
        };
        device.dropped_events.store(1, Ordering::Release);
        for _ in 0..2 {
            assert!(matches!(
                block_on(recorder.start(&target, RecordingTimecode::Preserve)),
                Err(Error::InvalidState(StateConflict::IncompleteFileTracking))
            ));
            assert_eq!(
                block_on(recorder.finish(Duration::ZERO)).unwrap(),
                FinishStatus::Ended(SessionEnd::Closed(report.clone()))
            );
        }
        let replacement = mock::Recorder::new(Closing::AtStop);
        let installed = Arc::clone(&replacement);
        block_on(recorder.worker.call(move |_| {
            installed.install();
            Ok(())
        }))
        .unwrap();
        assert_eq!(
            block_on(recorder.reset(&FileRecorderOptions::new(
                SourceSpec::new("replacement").unwrap()
            )))
            .unwrap(),
            report
        );
        assert_eq!(
            block_on(recorder.start(&target, RecordingTimecode::Preserve)).unwrap(),
            2
        );
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn async_reset_retires_unknown_cleanup_only_after_replacement_succeeds() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::AtStop,
            AsyncFileOptions::new(),
            Duration::from_millis(60),
        );
        device.dropped_events.store(1, Ordering::Release);
        let FinishStatus::Ended(SessionEnd::Indeterminate(report)) =
            block_on(recorder.finish(Duration::ZERO)).unwrap()
        else {
            panic!("lost native events must leave an indeterminate report");
        };
        assert_eq!(block_on(recorder.reported_current_file()).unwrap(), None);
        let options = FileRecorderOptions::new(SourceSpec::new("replacement").unwrap());
        let target =
            RecordingTarget::new(std::env::temp_dir().join("unknown-cleanup-reset")).unwrap();
        let replacement = mock::Recorder::new(Closing::AtStop);
        replacement.refuse_creation.store(true, Ordering::Release);
        let installed = Arc::clone(&replacement);
        block_on(recorder.worker.call(move |_| {
            installed.install();
            Ok(())
        }))
        .unwrap();
        assert!(matches!(
            block_on(recorder.reset(&options)),
            Err(Error::NativeOperationFailed { .. })
        ));
        assert_eq!(device.destroys.load(Ordering::Acquire), 0);
        assert_eq!(block_on(recorder.report()).unwrap(), report);
        assert!(matches!(
            block_on(recorder.start(&target, RecordingTimecode::Preserve)),
            Err(Error::InvalidState(StateConflict::IncompleteFileTracking))
        ));
        assert_eq!(device.starts.load(Ordering::Acquire), 1);

        replacement.refuse_creation.store(false, Ordering::Release);
        let installed = Arc::clone(&replacement);
        block_on(recorder.worker.call(move |_| {
            installed.install();
            Ok(())
        }))
        .unwrap();
        assert_eq!(block_on(recorder.reset(&options)).unwrap(), report);
        assert_eq!(block_on(recorder.report()).unwrap(), report);
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert_eq!(
            block_on(recorder.start(&target, RecordingTimecode::Preserve)).unwrap(),
            2
        );
        assert!(matches!(
            block_on(recorder.finish(Duration::ZERO)).unwrap(),
            FinishStatus::Ended(SessionEnd::Closed(_))
        ));
        block_on(recorder.close()).unwrap();
        assert_eq!(replacement.starts.load(Ordering::Acquire), 1);
        assert_eq!(replacement.stops.load(Ordering::Acquire), 1);
        assert_eq!(replacement.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn cancelling_a_finish_does_not_erase_unknown_cleanup_during_concurrent_close() {
        let budget = Duration::from_secs(2);
        let (device, recorder) =
            mock_recorder_with_drop_wait(Closing::WhenTold, AsyncFileOptions::new(), budget);
        let cancelled = recorder.finish(Duration::from_secs(30)).submit();
        await_finish(&recorder);
        drop(cancelled);
        let waiting = recorder.wait_for_finish(Duration::from_secs(30)).submit();
        await_finish(&recorder);
        let closed = recorder.clone().close().submit();
        let requested = recorder.worker.control.shutdown_requested_at().unwrap();
        device.dropped_events.store(1, Ordering::Release);
        device.close();
        let Some(SessionEnd::Indeterminate(report)) = block_on(waiting).unwrap() else {
            panic!("the remaining waiter must retain the loss verdict");
        };
        assert_eq!(report.dropped_events(), 1);
        assert!(!report.closure_confirmed());
        device.push(mock::FILE_CLOSED, "hidden.mkv");
        block_on(closed).unwrap();
        assert!(requested.elapsed() >= budget);
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert_eq!(recorder.worker.finish.state.load(Ordering::Acquire), 0);
    }

    #[test]
    fn incomplete_async_cleanup_respects_zero_wait_and_abandon() {
        for abandon in [false, true] {
            let (device, recorder) = mock_recorder_with_drop_wait(
                Closing::AtStop,
                AsyncFileOptions::new(),
                if abandon {
                    Duration::from_secs(30)
                } else {
                    Duration::ZERO
                },
            );
            device.dropped_events.store(1, Ordering::Release);
            let FinishStatus::Ended(SessionEnd::Indeterminate(report)) =
                block_on(recorder.finish(Duration::ZERO)).unwrap()
            else {
                panic!("lost events must prevent confirmation");
            };
            assert_eq!(block_on(recorder.reported_current_file()).unwrap(), None);
            let waits = device.waits.load(Ordering::Acquire);
            if abandon {
                assert_eq!(block_on(recorder.abandon()).unwrap(), report);
            } else {
                block_on(recorder.close()).unwrap();
            }
            assert_eq!(device.waits.load(Ordering::Acquire), waits);
            assert_eq!(device.stops.load(Ordering::Acquire), 1);
            assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        }
    }

    #[test]
    fn dropping_the_last_handle_during_a_finish_reports_the_closure_it_waits_for() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::After(Duration::from_millis(120)),
            AsyncFileOptions::new(),
            Duration::from_secs(2),
        );
        let finishing = recorder.finish(Duration::from_secs(30)).submit();
        await_finish(&recorder);
        let (dropped, drop_done) = mpsc::channel();
        let dropping = thread::spawn(move || {
            drop(recorder);
            dropped.send(()).unwrap();
        });
        let outcome = block_on(finishing).unwrap();
        assert!(
            matches!(outcome, FinishStatus::Ended(SessionEnd::Closed(_))),
            "{outcome:?}"
        );
        drop_done.recv_timeout(Duration::from_secs(1)).unwrap();
        dropping.join().unwrap();
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn cancelling_a_finish_still_releases_its_wait_before_close_waits_for_finalization() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::WhenTold,
            AsyncFileOptions::new(),
            Duration::from_secs(2),
        );
        let finishing = recorder.finish(Duration::from_secs(30)).submit();
        await_finish(&recorder);
        drop(finishing);
        // A status command still runs if cancellation follows this iteration's
        // pruning. Inspect after the next pump to prove the wait was released.
        assert_eq!(block_on(recorder.state()).unwrap(), RecorderState::Stopping);
        assert_eq!(
            block_on(recorder.worker.call(|owner| Ok(owner.pending_finishes()))).unwrap(),
            0
        );
        let closed = recorder.close().submit();
        assert!(!closed.is_completed());
        device.close();
        block_on(closed).unwrap();
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn cancelled_finish_retains_recorder_and_notification_loss_preserves_closure_evidence() {
        // The file is reported closed only once the test has seen the session
        // still stopping: a close on a timer came first whenever the owner
        // thread was slow to answer.
        let (device, recorder) = mock_recorder(Closing::WhenTold);
        let finishing = recorder.finish(Duration::from_secs(5)).submit();
        await_stop(&device);
        drop(finishing);
        assert_eq!(block_on(recorder.state()).unwrap(), RecorderState::Stopping);
        assert_eq!(device.destroys.load(Ordering::Acquire), 0);
        let waiting = recorder.wait_for_finish(Duration::from_secs(5)).submit();
        device.close();
        let outcome = block_on(waiting).unwrap();
        let Some(SessionEnd::Closed(report)) = outcome else {
            panic!("expected confirmed closure after the later close event: {outcome:?}")
        };
        assert!(report.closure_confirmed());
        assert_eq!(report.dropped_events(), 0);
        // The one-slot event queue kept only the newest of the two events.
        assert_eq!(recorder.notification_dropped_count(), 1);
        block_on(recorder.close()).unwrap();
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn dropping_the_last_handle_returns_after_the_file_is_closed_and_the_recorder_destroyed() {
        let (device, recorder) = mock_recorder(Closing::After(Duration::from_millis(150)));
        let clone = recorder.clone();
        drop(recorder);
        assert_eq!(
            device.destroys.load(Ordering::Acquire),
            0,
            "another handle keeps the recorder"
        );
        drop(clone);
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn a_dropped_close_does_not_let_the_last_drop_skip_finalization() {
        let (device, recorder) = mock_recorder(Closing::After(Duration::from_millis(150)));
        drop(recorder.close().submit());
        drop(recorder);
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn an_awaited_close_keeps_the_last_drop_nonblocking() {
        let (device, recorder) = mock_recorder(Closing::WhenTold);
        let closed = recorder.close().submit();
        drop(recorder);
        assert_eq!(device.destroys.load(Ordering::Acquire), 0);
        assert!(!closed.is_completed());
        await_stop(&device);
        device.close();
        block_on(closed).unwrap();
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    fn mock_player() -> (Arc<AtomicUsize>, AsyncFilePlayer) {
        use crate::sys::{self, bindings::*};
        thread_local! {
            static DESTROYS: std::cell::RefCell<Option<Arc<AtomicUsize>>> = const {
                std::cell::RefCell::new(None)
            };
        }
        unsafe extern "C" fn create(
            _: *const NDIlib_file_player_create_t,
        ) -> NDIlib_file_player_instance_t {
            DESTROYS.with(|count| {
                Box::into_raw(Box::new(count.borrow().as_ref().unwrap().clone())).cast()
            })
        }
        unsafe extern "C" fn destroy(instance: NDIlib_file_player_instance_t) {
            // SAFETY: create transferred this Box to the sole native owner.
            let count = unsafe { Box::from_raw(instance.cast::<Arc<AtomicUsize>>()) };
            count.fetch_add(1, Ordering::Release);
        }
        let count = Arc::new(AtomicUsize::new(0));
        let installed = Arc::clone(&count);
        let player = block_on(spawn_owner(
            "file-test-player",
            move || {
                DESTROYS.with(|count| *count.borrow_mut() = Some(installed));
                FilePlayer::new(
                    &FileApi::with_tables(
                        sys::FilePlayback {
                            NDIlib_file_player_create: create,
                            NDIlib_file_player_destroy: destroy,
                            ..mock::playback()
                        },
                        mock::recording(),
                    ),
                    &FilePlayerOptions::new("mock player"),
                )
                .map(PlayerOwner::new)
            },
            AsyncFileOptions::new(),
            |worker| AsyncFilePlayer {
                worker,
                name: Arc::from("mock player"),
            },
        ))
        .unwrap();
        (count, player)
    }

    #[test]
    fn async_player_debug_retains_its_name_across_clones() {
        let (_, player) = mock_player();
        assert_eq!(
            format!("{player:?}"),
            "AsyncFilePlayer { name: \"mock player\", .. }"
        );
        let other = player.clone();
        assert!(Arc::ptr_eq(&player.name, &other.name));
        assert_eq!(format!("{player:?}"), format!("{other:?}"));
    }

    #[test]
    fn a_dropped_player_close_makes_the_last_drop_wait_for_destruction() {
        let (destroys, player) = mock_player();
        let (blocked, release) = block_owner(&player.worker);
        drop(player.close().submit());
        let (returned, returning) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(player);
            returned.send(()).unwrap();
        });
        let early = returning.recv_timeout(Duration::from_millis(100));
        release.send(()).unwrap();
        block_on(blocked).unwrap();
        dropper.join().unwrap();
        assert!(
            early.is_err(),
            "last drop returned before native destruction"
        );
        assert_eq!(destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn an_awaited_player_close_keeps_the_last_drop_nonblocking() {
        let (destroys, player) = mock_player();
        let (blocked, release) = block_owner(&player.worker);
        let closed = player.close().submit();
        drop(player);
        assert_eq!(destroys.load(Ordering::Acquire), 0);
        assert!(!closed.is_completed());
        release.send(()).unwrap();
        block_on(blocked).unwrap();
        block_on(closed).unwrap();
        assert_eq!(destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_session_the_sdk_ended_itself_can_be_followed_by_another() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        device.end_session_itself();
        // The owner thread takes events continuously, so the session closes
        // without anyone asking it to stop.
        let deadline = Instant::now() + Duration::from_secs(5);
        while block_on(recorder.state()).unwrap() != RecorderState::Closed {
            assert!(Instant::now() < deadline, "the session never closed");
            thread::sleep(Duration::from_millis(5));
        }
        let session = block_on(recorder.start(
            &RecordingTarget::new(std::env::temp_dir().join("mock-file-not-created")).unwrap(),
            RecordingTimecode::Preserve,
        ))
        .unwrap();
        assert_eq!(session, 2);
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn abandon_destroys_without_spending_drop_wait() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::Never,
            AsyncFileOptions::new(),
            Duration::from_secs(2),
        );
        let began = Instant::now();
        let report = block_on(recorder.abandon()).unwrap();
        assert!(began.elapsed() < Duration::from_millis(500));
        assert_eq!(report.session(), 1);
        assert!(!report.closure_confirmed());
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert_eq!(block_on(recorder.abandon()).unwrap(), report);
        block_on(recorder.close()).unwrap();
        closed(block_on(recorder.report()));
    }

    #[test]
    fn a_discarded_abandon_still_skips_cleanup_after_every_handle_is_dropped() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::Never,
            AsyncFileOptions::new(),
            Duration::from_secs(2),
        );
        let (blocked, release) = block_owner(&recorder.worker);
        let other = recorder.clone();
        drop(recorder.abandon().submit());
        drop(recorder);
        let began = Instant::now();
        release.send(()).unwrap();
        block_on(blocked).unwrap();
        // With no live close future, the last handle joins destruction, but
        // the retained abandon request must still bypass its two-second wait.
        drop(other);
        assert!(began.elapsed() < Duration::from_millis(500));
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn abandon_after_owner_failure_reports_the_recorded_failure() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        owner_failed(block_on(
            recorder
                .worker
                .call(|_| -> Result<()> { panic!("owner failed before abandon") }),
        ));
        owner_failed(block_on(recorder.abandon()));
        owner_failed(block_on(recorder.abandon()));
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_close_racing_abandon_completes_with_it() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::Never,
            AsyncFileOptions::new(),
            Duration::from_secs(2),
        );
        let (blocked, release) = block_owner(&recorder.worker);
        let closing = recorder.close().submit();
        let abandoned = recorder.abandon().submit();
        assert!(!closing.is_completed());
        assert!(!abandoned.is_completed());
        let began = Instant::now();
        release.send(()).unwrap();
        block_on(blocked).unwrap();
        let report = block_on(abandoned).unwrap();
        block_on(closing).unwrap();
        assert!(began.elapsed() < Duration::from_millis(500));
        assert_eq!(report.session(), 1);
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn abandon_ends_a_close_that_is_already_draining() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::Never,
            AsyncFileOptions::new(),
            Duration::from_secs(2),
        );
        let closing = recorder.close().submit();
        await_stop(&device);
        let began = Instant::now();
        let report = block_on(recorder.abandon()).unwrap();
        block_on(closing).unwrap();
        assert!(began.elapsed() < Duration::from_millis(500));
        assert_eq!(report.session(), 1);
        assert!(!report.closure_confirmed());
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_queued_finish_settles_when_abandoned() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        let (blocked, release) = block_owner(&recorder.worker);
        let finishing = recorder.finish(Duration::from_secs(30)).submit();
        let abandoned = recorder.abandon().submit();
        release.send(()).unwrap();
        block_on(blocked).unwrap();
        let report = block_on(abandoned).unwrap();
        assert_eq!(report.session(), 1);
        closed(block_on(finishing));
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert_eq!(recorder.worker.admission.state.load(Ordering::Acquire), 0);
        assert_eq!(recorder.worker.finish.state.load(Ordering::Acquire), 0);
    }

    #[test]
    fn abandon_bypasses_a_full_command_queue() {
        let (device, recorder) =
            mock_recorder_with(Closing::Never, AsyncFileOptions::new().command_capacity(1));
        let (blocked, release) = block_owner(&recorder.worker);
        let queued = recorder.is_recording().submit();
        let stopping = recorder.request_stop().submit();
        assert_eq!(recorder.worker.admission.state.load(Ordering::Acquire), 3);
        let abandoned = recorder.abandon().submit();
        assert_eq!(recorder.worker.admission.state.load(Ordering::Acquire), 3);
        assert!(!abandoned.is_completed());
        release.send(()).unwrap();
        block_on(blocked).unwrap();
        assert_eq!(block_on(abandoned).unwrap().session(), 1);
        closed(block_on(queued));
        closed(block_on(stopping));
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn an_unsubmitted_abandon_does_not_shut_down_the_recorder() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        drop(recorder.abandon());
        assert!(block_on(recorder.is_recording()).unwrap());
        assert_eq!(device.stops.load(Ordering::Acquire), 0);
        assert_eq!(device.destroys.load(Ordering::Acquire), 0);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn close_bypasses_a_full_command_queue_and_its_reserved_stop() {
        let (device, recorder) =
            mock_recorder_with(Closing::AtStop, AsyncFileOptions::new().command_capacity(1));
        let (blocked, release) = block_owner(&recorder.worker);
        let queued = recorder.is_recording().submit();
        let stopping = recorder.request_stop().submit();
        assert_eq!(recorder.worker.admission.state.load(Ordering::Acquire), 3);
        let closing = recorder.close().submit();
        assert_eq!(recorder.worker.admission.state.load(Ordering::Acquire), 3);
        assert!(!closing.is_completed());
        release.send(()).unwrap();
        block_on(blocked).unwrap();
        block_on(closing).unwrap();
        closed(block_on(queued));
        closed(block_on(stopping));
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_stop_is_carried_out_even_when_the_command_queue_is_full() {
        let options = AsyncFileOptions::new().command_capacity(1);
        let (device, recorder) = mock_recorder_with(Closing::AtStop, options);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = recorder
            .worker
            .call(move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .submit();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // The one queue slot is taken, so an ordinary command is refused.
        let queued = recorder.is_recording().submit();
        assert!(matches!(
            block_on(recorder.is_recording()),
            Err(Error::QueueFull(QueueLimit::FileCommands))
        ));
        // A stop is not: it must not be lost, even if nobody waits for it.
        drop(recorder.request_stop().submit());
        // One stop beyond the full queue is all it takes: another, of any
        // kind, is refused while that one waits.
        assert!(matches!(
            block_on(recorder.request_stop()),
            Err(Error::QueueFull(QueueLimit::FileCommands))
        ));
        assert!(matches!(
            block_on(recorder.finish(Duration::ZERO)),
            Err(Error::QueueFull(QueueLimit::FileCommands))
        ));
        release_tx.send(()).unwrap();
        block_on(blocker).unwrap();
        assert!(block_on(queued).unwrap());
        // The stop beyond the queue runs next, though nobody waits for it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while device.stops.load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < deadline, "the stop was lost");
            thread::sleep(Duration::from_millis(1));
        }
        // It did the stopping; a later request finds it done, and possibly
        // the file already reported closed.
        assert!(matches!(
            block_on(recorder.request_stop()).unwrap(),
            StopRequest::AlreadyStopping | StopRequest::AlreadyClosed
        ));
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn a_full_queue_holds_its_capacity_and_one_stop_at_most() {
        let options = AsyncFileOptions::new().command_capacity(2);
        let (worker, dropped) = local_worker_with_options(options);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = worker
            .call(move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .submit();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let add = |amount| {
            move |owner: &mut LocalOwner| {
                owner.counter += amount;
                Ok::<(), Error>(())
            }
        };
        // Requests to stop take places like any command, and run even when
        // nobody waits for them.
        drop(worker.submit(true, add(1)).submit());
        drop(worker.submit(true, add(10)).submit());
        // The queue is full: one more stop is admitted beyond it.
        let beyond = worker.submit(true, add(100)).submit();
        // That is the most it holds.
        for refused in [
            worker.submit(true, add(1000)).submit(),
            worker.submit(false, add(1000)).submit(),
        ] {
            assert!(matches!(
                block_on(refused),
                Err(Error::QueueFull(QueueLimit::FileCommands))
            ));
        }
        release_tx.send(()).unwrap();
        block_on(blocker).unwrap();
        // The queue runs in order, so the stop beyond it runs last.
        block_on(beyond).unwrap();
        assert_eq!(
            block_on(worker.call(|owner| Ok(owner.counter))).unwrap(),
            111
        );
        // Every place was given back, the one beyond the capacity too.
        assert_eq!(worker.admission.state.load(Ordering::Acquire), 0);
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn a_reserved_stop_does_not_consume_a_normal_queue_place() {
        let admission = Arc::new(Admission::new(1));
        let normal = admission.admit(false).unwrap();
        let reserved = admission.admit(true).unwrap();
        assert!(matches!(
            admission.admit(true),
            Err(Error::QueueFull(QueueLimit::FileCommands))
        ));

        // The normal command starts while the extra stop still waits.
        drop(normal);
        let next_normal = admission.admit(false).unwrap();
        assert!(matches!(
            admission.admit(false),
            Err(Error::QueueFull(QueueLimit::FileCommands))
        ));

        drop(reserved);
        let next_reserved = admission.admit(true).unwrap();
        drop(next_normal);
        drop(next_reserved);
        assert_eq!(admission.state.load(Ordering::Acquire), 0);
    }

    #[test]
    fn concurrent_admissions_never_exceed_the_queue_and_one_stop() {
        use std::sync::Barrier;

        const CAPACITY: usize = 2;
        const THREADS: usize = 8;
        let admission = Arc::new(Admission::new(CAPACITY));
        let active = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(THREADS));
        thread::scope(|scope| {
            for thread_index in 0..THREADS {
                let admission = Arc::clone(&admission);
                let active = Arc::clone(&active);
                let start = Arc::clone(&start);
                scope.spawn(move || {
                    start.wait();
                    for _ in 0..2_000 {
                        if let Ok(place) = admission.admit(thread_index % 2 == 0) {
                            let count = active.fetch_add(1, Ordering::AcqRel) + 1;
                            assert!(count <= CAPACITY + 1, "admitted {count} commands");
                            thread::yield_now();
                            active.fetch_sub(1, Ordering::AcqRel);
                            drop(place);
                        } else {
                            thread::yield_now();
                        }
                    }
                });
            }
        });
        assert_eq!(active.load(Ordering::Acquire), 0);
        assert_eq!(admission.state.load(Ordering::Acquire), 0);
    }

    /// Captured by a command that never runs: the owner thread drops it with
    /// the rest of its queue only after it has completed its close, and is
    /// held there until the test lets it go, so that a handle dropped
    /// meanwhile would wait for a thread that is still running.
    struct HoldsOwnerThread {
        held: mpsc::Sender<()>,
        let_go: Receiver<()>,
        released: mpsc::Sender<bool>,
    }
    impl Drop for HoldsOwnerThread {
        fn drop(&mut self) {
            let _ = self.held.send(());
            // Bounded, so that a handle drop that waits for this thread fails
            // the test instead of hanging it.
            let let_go = self.let_go.recv_timeout(Duration::from_secs(5)).is_ok();
            let _ = self.released.send(let_go);
        }
    }

    #[test]
    fn closes_made_while_one_is_awaited_complete_with_it() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        // The owner thread runs this until every close below has been made.
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = recorder
            .worker
            .call(move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .submit();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (held_tx, held_rx) = mpsc::channel();
        let (let_go_tx, let_go_rx) = mpsc::channel();
        let (released_tx, released_rx) = mpsc::channel();
        let hold = HoldsOwnerThread {
            held: held_tx,
            let_go: let_go_rx,
            released: released_tx,
        };
        let never_run = recorder
            .worker
            .call(move |_| {
                drop(hold);
                Ok(())
            })
            .submit();
        let clone = recorder.clone();
        let first = recorder.close().submit();
        let second = clone.close().submit();
        // A close whose future is dropped neither blocks nor cancels the others.
        drop(recorder.close().submit());
        release_tx.send(()).unwrap();
        block_on(blocker).unwrap();
        block_on(second).unwrap();
        block_on(first).unwrap();
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        // The handles stay usable for what a closed handle can do.
        block_on(recorder.close()).unwrap();
        closed(block_on(clone.state()));
        // The owner thread has completed the close, and is still running.
        held_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // No clone waits for it when dropped once the object is closed.
        drop((recorder, clone));
        let _ = let_go_tx.send(());
        assert!(
            released_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            "dropping the last handle waited for the owner thread"
        );
        // The command left in the queue was not carried out.
        closed(block_on(never_run));
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    /// A probe of a [`gated_api`] waits here until a test opens the gate.
    static PROBE_GATE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
    /// How many probes of a [`gated_api`] have begun.
    static PROBES_BEGUN: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn gated_probe(
        _: *const std::ffi::c_char,
        _: *mut crate::sys::bindings::NDIlib_file_player_file_info_t,
    ) -> bool {
        PROBES_BEGUN.fetch_add(1, Ordering::AcqRel);
        let mut open = lock(&PROBE_GATE.0);
        while !*open {
            open = PROBE_GATE
                .1
                .wait(open)
                .unwrap_or_else(PoisonError::into_inner);
        }
        // The file cannot be read, so the probe fails as the SDK reports it.
        false
    }

    /// An API whose probes wait for [`PROBE_GATE`].
    fn gated_api() -> FileApi {
        FileApi::with_tables(
            crate::sys::FilePlayback {
                NDIlib_file_player_probe_file: gated_probe,
                ..mock::playback()
            },
            mock::recording(),
        )
    }

    #[test]
    fn public_probe_waits_for_submission() {
        static BEGUN: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn probe(
            _: *const std::ffi::c_char,
            _: *mut crate::sys::bindings::NDIlib_file_player_file_info_t,
        ) -> bool {
            BEGUN.fetch_add(1, Ordering::AcqRel);
            false
        }
        let api = FileApi::with_tables(
            crate::sys::FilePlayback {
                NDIlib_file_player_probe_file: probe,
                ..mock::playback()
            },
            mock::recording(),
        );
        let deferred = AsyncFilePlayer::probe(&api, "unpolled.mkv");
        assert_eq!(BEGUN.load(Ordering::Acquire), 0);
        drop(deferred);
        assert_eq!(BEGUN.load(Ordering::Acquire), 0);
        assert!(matches!(
            block_on(AsyncFilePlayer::probe(&api, "polled.mkv")),
            Err(Error::NativeOperationFailed { .. })
        ));
        assert_eq!(BEGUN.load(Ordering::Acquire), 1);
    }

    #[test]
    fn probes_run_one_at_a_time_and_a_full_queue_refuses_more() {
        let api = gated_api();
        let queue = ProbeQueue::new(2);
        let running = FileOperation::live(queue.probe(&api, Path::new("running.mkv")));
        let deadline = Instant::now() + Duration::from_secs(5);
        while PROBES_BEGUN.load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < deadline, "the first probe never began");
            thread::sleep(Duration::from_millis(1));
        }
        // Two wait behind the running one; a third does not fit.
        let waiting = FileOperation::live(queue.probe(&api, Path::new("waiting.mkv")));
        let cancelled = FileOperation::live(queue.probe(&api, Path::new("cancelled.mkv")));
        let refused = block_on(FileOperation::live(
            queue.probe(&api, Path::new("refused.mkv")),
        ));
        assert!(
            matches!(refused, Err(Error::QueueFull(QueueLimit::FileProbes))),
            "{refused:?}"
        );
        // Only one runs at a time.
        thread::sleep(Duration::from_millis(20));
        assert_eq!(PROBES_BEGUN.load(Ordering::Acquire), 1);
        // A probe dropped before it begins is not carried out.
        drop(cancelled);
        *lock(&PROBE_GATE.0) = true;
        PROBE_GATE.1.notify_all();
        for probed in [running, waiting] {
            assert!(matches!(
                block_on(probed),
                Err(Error::NativeOperationFailed { .. })
            ));
        }
        // The thread has room again once they have run.
        assert!(matches!(
            block_on(FileOperation::live(
                queue.probe(&api, Path::new("later.mkv"))
            )),
            Err(Error::NativeOperationFailed { .. })
        ));
        assert_eq!(PROBES_BEGUN.load(Ordering::Acquire), 3);
    }

    #[test]
    fn a_missing_playback_family_never_starts_or_occupies_a_probe_queue() {
        let queue = ProbeQueue::new(0);
        let api = FileApi::with_runtime(crate::sys::FileRuntime::test_double(
            Err(c"NDIlib_file_player_seek_to_frame"),
            Ok(mock::recording()),
        ));
        for path in ["", "bad\0path.mkv", "recording.mkv"] {
            assert!(matches!(
                block_on(FileOperation::live(queue.probe(&api, Path::new(path)))),
                Err(Error::UnsupportedCapability { .. })
            ));
            assert!(lock(&queue.sender).is_none());
        }
    }

    #[test]
    fn a_relative_player_start_retains_the_directory_at_submission() {
        const CHILD: &str = "GRAFTON_FILE_START_PATH_CHILD";
        if let Some(directory) = std::env::var_os(CHILD) {
            static STARTED_PATH: Mutex<Option<String>> = Mutex::new(None);
            unsafe extern "C" fn create(
                _: *const crate::sys::bindings::NDIlib_file_player_create_t,
            ) -> crate::sys::bindings::NDIlib_file_player_instance_t {
                std::ptr::NonNull::dangling().as_ptr()
            }
            unsafe extern "C" fn start(
                _: crate::sys::bindings::NDIlib_file_player_instance_t,
                path: *const std::ffi::c_char,
            ) -> bool {
                // SAFETY: the player wrapper retains its validated FileText.
                *lock(&STARTED_PATH) = Some(
                    unsafe { std::ffi::CStr::from_ptr(path) }
                        .to_str()
                        .unwrap()
                        .to_owned(),
                );
                true
            }
            let api = FileApi::with_runtime(crate::sys::FileRuntime::test_double(
                Ok(crate::sys::FilePlayback {
                    NDIlib_file_player_create: create,
                    NDIlib_file_player_start: start,
                    ..mock::playback()
                }),
                Ok(mock::recording()),
            ));
            let player = block_on(AsyncFilePlayer::create(
                &api,
                &FilePlayerOptions::new("paths"),
            ))
            .unwrap();
            let (blocked, release) = block_owner(&player.worker);
            let directory = std::path::PathBuf::from(directory);
            std::env::set_current_dir(directory.join("submitted")).unwrap();
            let submitted_path = std::env::current_dir().unwrap().join("clip.mkv");
            let starting = player.start("clip.mkv").submit();
            std::env::set_current_dir(directory.join("running")).unwrap();
            release.send(()).unwrap();
            block_on(blocked).unwrap();
            block_on(starting).unwrap();
            assert_eq!(lock(&STARTED_PATH).as_deref(), submitted_path.to_str());
            block_on(player.close()).unwrap();
            return;
        }
        let directory = crate::test_support::FixtureDir::create("file-start-path");
        std::fs::create_dir_all(directory.join("submitted")).unwrap();
        std::fs::create_dir_all(directory.join("running")).unwrap();
        crate::test_support::TestChild::new("file::asynchronous::tests::a_relative_player_start_retains_the_directory_at_submission")

            .env(CHILD, directory.path())
            .run();
    }

    #[test]
    fn a_probe_that_panics_fails_alone() {
        let queue = ProbeQueue::new(1);
        let (reply, failing) = operation::<FileInfo>();
        let failing = FileOperation::live(failing);
        queue
            .submit(Box::new(move || {
                let _unanswered = reply;
                panic!("probe failure");
            }))
            .unwrap();
        owner_failed(block_on(failing));
        // The thread goes on with the next probe.
        assert!(matches!(
            block_on(FileOperation::live(
                queue.probe(&mock::recorder_api(), Path::new("b.mkv"))
            )),
            Err(Error::NativeOperationFailed { .. })
        ));
    }

    #[test]
    fn an_explicit_close_does_not_block_the_caller() {
        // The file is reported closed only when the test says so: until
        // then, the close can complete only once the drop wait has run out
        // its default 5 s.
        let (device, recorder) = mock_recorder(Closing::WhenTold);
        let closed = recorder.close().submit();
        // It returned while the owner thread still waits for the file.
        assert_eq!(device.destroys.load(Ordering::Acquire), 0);
        assert!(
            !closed.is_completed(),
            "the close completed before the file was reported closed"
        );
        await_stop(&device);
        device.close();
        block_on(closed).unwrap();
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn event_timeout_wakes_without_blocking_commands() {
        let (worker, dropped) = local_worker();
        let event = worker.events.wait(Duration::from_millis(20));
        assert_eq!(
            block_on(worker.call(|owner| {
                owner.counter = 5;
                Ok(owner.counter)
            }))
            .unwrap(),
            5
        );
        assert_eq!(block_on(event).unwrap(), None);
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn event_overflow_drops_only_user_notifications_and_counts_them() {
        let events = Arc::new(EventQueue::new(2));
        events.push(Ok(1));
        events.push(Ok(2));
        events.push(Ok(3));
        assert_eq!(lock(&events.0).dropped, 1);
        assert_eq!(block_on(events.wait(Duration::ZERO)).unwrap(), Some(2));
        assert_eq!(block_on(events.wait(Duration::ZERO)).unwrap(), Some(3));
    }

    #[test]
    fn only_one_pending_event_waiter_and_cancel_releases_registration() {
        let events = Arc::new(EventQueue::<u32>::new(1));
        let signal = Arc::new(Signal(Mutex::new(false), Condvar::new()));
        let waker = Waker::from(signal);
        let mut first = Box::pin(events.wait(Duration::from_secs(1)));
        assert!(first
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        assert!(matches!(
            block_on(events.wait(Duration::ZERO)),
            Err(Error::InvalidState(StateConflict::WaiterPending))
        ));
        drop(first);
        assert_eq!(block_on(events.wait(Duration::ZERO)).unwrap(), None);
    }

    #[test]
    fn closing_wakes_pending_event_future_and_rejects_new_commands() {
        let (worker, dropped) = local_worker();
        let event = worker.events.wait(Duration::from_secs(3));
        block_on(worker.control.request_close(|| {})).unwrap();
        assert!(matches!(
            block_on(event),
            Err(Error::InvalidState(StateConflict::Closed))
        ));
        assert!(matches!(
            block_on(worker.call(|_| Ok(()))),
            Err(Error::InvalidState(StateConflict::Closed))
        ));
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn an_async_finish_whose_stop_outlasts_its_wait_takes_the_queued_close() {
        let (device, recorder) = mock_recorder(Closing::DuringSlowStop(Duration::from_millis(80)));
        let outcome = block_on(recorder.finish(Duration::from_millis(10))).unwrap();
        assert!(
            matches!(outcome, FinishStatus::Ended(SessionEnd::Closed(_))),
            "{outcome:?}"
        );
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn an_async_finish_measures_its_wait_from_submission() {
        // The command waits behind another until its whole wait has passed,
        // so once it runs only the events already queued are taken. The close
        // is reported only to a capture that waits for it: a wait measured
        // from when the command ran would take it and end Closed.
        let (device, recorder) = mock_recorder(Closing::WhenWaitedFor);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = recorder
            .worker
            .call(move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .submit();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let wait = Duration::from_millis(200);
        let finishing = recorder.finish(wait).submit();
        // At least the whole wait has passed since submission once this returns.
        thread::sleep(wait);
        release_tx.send(()).unwrap();
        block_on(blocker).unwrap();
        let outcome = block_on(finishing).unwrap();
        assert!(matches!(outcome, FinishStatus::TimedOut(_)), "{outcome:?}");
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        // The close was there for a wait to take: the drop wait takes it.
        block_on(recorder.close()).unwrap();
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn an_async_timeout_out_of_range_fails_at_submission_and_stops_nothing() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        let too_long = crate::MAX_TIMEOUT + Duration::from_nanos(1);
        fn assert_refused<T: std::fmt::Debug>(finishing: FileOperation<T>, too_long: Duration) {
            let finishing = finishing.submit();
            // Completed at submission: nothing was queued.
            assert!(finishing.is_completed());
            let result = block_on(finishing);
            assert!(
                matches!(&result, Err(Error::InvalidInput(crate::InputDefect::TimeoutTooLong { requested })) if *requested == too_long),
                "{result:?}"
            );
        }
        assert_refused(recorder.finish(too_long), too_long);
        assert_refused(recorder.wait_for_finish(too_long), too_long);
        assert_eq!(
            block_on(recorder.state()).unwrap(),
            RecorderState::Recording
        );
        assert_eq!(device.stops.load(Ordering::Acquire), 0);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn an_event_wait_out_of_range_fails_at_its_first_poll_without_waiting() {
        let events = Arc::new(EventQueue::<u32>::new(1));
        events.push(Ok(7));
        let mut waiting = Box::pin(events.wait(crate::MAX_TIMEOUT + Duration::from_nanos(1)));
        let polled = waiting
            .as_mut()
            .poll(&mut Context::from_waker(&idle_waker()));
        assert!(
            matches!(&polled, Poll::Ready(Err(Error::InvalidInput(crate::InputDefect::TimeoutTooLong { requested }))) if *requested == crate::MAX_TIMEOUT + Duration::from_nanos(1)),
            "{polled:?}"
        );
        assert!(matches!(waiting.state, EventWaitState::Done));
        // It took nothing and left no waiter behind.
        assert_eq!(block_on(events.wait(Duration::ZERO)).unwrap(), Some(7));
    }

    /// A waker for a poll whose wake-up the test does not wait for.
    fn idle_waker() -> Waker {
        Waker::from(Arc::new(Signal(Mutex::new(false), Condvar::new())))
    }

    #[test]
    fn unreadable_player_and_probe_formats_are_delivered_through_async_handles() {
        let api = FileApi::with_tables(mock::playback_with_unreadable_formats(), mock::recording());
        let player = block_on(AsyncFilePlayer::create(
            &api,
            &FilePlayerOptions::new("mock player"),
        ))
        .unwrap();
        let event = block_on(player.next_event(Duration::from_secs(5)))
            .unwrap()
            .unwrap();
        assert_eq!(event.kind(), super::super::PlayerEventKind::Ended);
        assert_eq!(event.path().to_str().unwrap(), "p");
        assert_eq!(event.first_pts().ticks(), 42);
        assert_eq!(
            event.source_format(),
            Err(crate::SdkResultDefect::NegativeFormatCount {
                what: "audio sample count",
                value: -1
            })
        );
        block_on(player.close()).unwrap();
        let info = block_on(AsyncFilePlayer::probe(&api, "mock.mkv")).unwrap();
        assert_eq!(info.duration().value().ticks(), 123);
        assert_eq!(info.track_count(), 4);
        assert_eq!(
            info.source_format(),
            Err(crate::SdkResultDefect::FileTrackCount {
                count: 3,
                capacity: 2
            })
        );
    }

    #[test]
    fn an_async_finish_delivers_a_malformed_event_it_takes_to_the_event_stream() {
        let (device, recorder) = mock_recorder_with(Closing::Never, AsyncFileOptions::new());
        let finishing = recorder.finish(Duration::from_secs(5)).submit();
        while device.stops.load(Ordering::Acquire) == 0 {
            thread::sleep(Duration::from_millis(1));
        }
        // The event pump takes this while the finish is registered.
        let mut close = mock::recorder_event(mock::FILE_CLOSED, mock::TAKE);
        close.source_format_payload.video_track_count = 3;
        device.push_event(close);
        let outcome = block_on(finishing).unwrap();
        assert!(
            matches!(outcome, FinishStatus::Ended(SessionEnd::Closed(_))),
            "{outcome:?}"
        );
        // The start, pumped before the finish began, then the malformed close.
        let started = block_on(recorder.next_event(Duration::ZERO))
            .unwrap()
            .unwrap();
        assert_eq!(started.kind(), RecorderEventKind::Started);
        let malformed = block_on(recorder.next_event(Duration::ZERO))
            .unwrap()
            .unwrap();
        assert_eq!(malformed.kind(), RecorderEventKind::FileClosed);
        assert_eq!(malformed.path().to_str().unwrap(), mock::TAKE);
        assert_eq!(
            malformed.source_format(),
            Err(crate::SdkResultDefect::FileTrackCount {
                count: 3,
                capacity: 2,
            })
        );
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn an_event_wait_ends_on_time_while_the_owner_thread_is_blocked() {
        let (worker, dropped) = local_worker();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let blocker = worker
            .call(move |_| {
                entered_tx.send(()).unwrap();
                // Far longer than the wait; bounded so a failure cannot hang.
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
                Ok(())
            })
            .submit();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let begun = Instant::now();
        assert_eq!(
            block_on(worker.events.wait(Duration::from_millis(50))).unwrap(),
            None
        );
        let waited = begun.elapsed();
        assert!(waited >= Duration::from_millis(50), "{waited:?}");
        assert!(waited < Duration::from_secs(1), "the wait took {waited:?}");
        release_tx.send(()).unwrap();
        block_on(blocker).unwrap();
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn a_pending_event_wait_ends_at_close_without_waiting_for_the_drop_wait() {
        // The file is reported closed only when the test says so: until
        // then, the drop wait can end only by running out its default 5 s.
        let (device, recorder) = mock_recorder(Closing::WhenTold);
        let started = block_on(recorder.next_event(Duration::from_secs(1)))
            .unwrap()
            .unwrap();
        assert_eq!(started.kind(), RecorderEventKind::Started);
        let mut waiting = Box::pin(recorder.next_event(Duration::from_secs(30)));
        assert!(waiting
            .as_mut()
            .poll(&mut Context::from_waker(&idle_waker()))
            .is_pending());
        let begun = Instant::now();
        let closed = recorder.close().submit();
        let outcome = block_on(waiting.as_mut());
        let waited = begun.elapsed();
        assert!(
            matches!(outcome, Err(Error::InvalidState(StateConflict::Closed))),
            "{outcome:?}"
        );
        assert!(
            waited < Duration::from_millis(1500),
            "the wait took {waited:?}"
        );
        assert_eq!(
            device.destroys.load(Ordering::Acquire),
            0,
            "the drop wait was still running"
        );
        // The drop wait stopped the session; the close it waits for follows.
        await_stop(&device);
        device.close();
        block_on(closed).unwrap();
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn the_timer_wakes_for_an_earlier_deadline_registered_after_a_later_one() {
        let later = Arc::new(EventQueue::<u32>::new(1));
        let sooner = Arc::new(EventQueue::<u32>::new(1));
        let mut long = Box::pin(later.wait(Duration::from_secs(30)));
        assert!(long
            .as_mut()
            .poll(&mut Context::from_waker(&idle_waker()))
            .is_pending());
        // Let the timer settle into its sleep until the later deadline, so
        // that only being woken for the earlier one can end the next wait.
        thread::sleep(Duration::from_millis(50));
        // No owner thread exists: only the timer can end this wait.
        let begun = Instant::now();
        assert_eq!(
            block_on(sooner.wait(Duration::from_millis(50))).unwrap(),
            None
        );
        let waited = begun.elapsed();
        assert!(waited >= Duration::from_millis(50), "{waited:?}");
        assert!(waited < Duration::from_secs(1), "the wait took {waited:?}");
    }

    #[test]
    fn a_wait_is_ended_by_its_own_deadline_and_through_its_latest_waker() {
        let events = Arc::new(EventQueue::<u32>::new(1));
        // A wait dropped while pending must not end the next one early.
        let mut abandoned = Box::pin(events.wait(Duration::from_millis(20)));
        assert!(abandoned
            .as_mut()
            .poll(&mut Context::from_waker(&idle_waker()))
            .is_pending());
        drop(abandoned);
        let mut waiting = Box::pin(events.wait(Duration::from_millis(200)));
        let begun = Instant::now();
        assert!(waiting
            .as_mut()
            .poll(&mut Context::from_waker(&idle_waker()))
            .is_pending());
        // Polled again with another waker, which the timer must wake instead.
        assert_eq!(block_on(waiting.as_mut()).unwrap(), None);
        let waited = begun.elapsed();
        assert!(waited >= Duration::from_millis(200), "{waited:?}");
        assert!(waited < Duration::from_secs(2), "the wait took {waited:?}");
    }

    #[test]
    fn a_wait_that_completes_or_is_dropped_leaves_no_timer_registration() {
        let events = Arc::new(EventQueue::<u32>::new(1));
        let registered = |key: TimerKey| lock(&TIMER.state).entries.contains_key(&key);

        let mut answered = Box::pin(events.wait(Duration::from_secs(60)));
        assert!(answered
            .as_mut()
            .poll(&mut Context::from_waker(&idle_waker()))
            .is_pending());
        let EventWaitState::Registered { timer: key, .. } = answered.state else {
            panic!("a pending wait is timed");
        };
        assert!(registered(key));
        events.push(Ok(7));
        assert!(matches!(
            answered
                .as_mut()
                .poll(&mut Context::from_waker(&idle_waker())),
            Poll::Ready(Ok(Some(7)))
        ));
        assert!(!registered(key));

        let mut abandoned = Box::pin(events.wait(Duration::from_secs(60)));
        assert!(abandoned
            .as_mut()
            .poll(&mut Context::from_waker(&idle_waker()))
            .is_pending());
        let EventWaitState::Registered { timer: key, .. } = abandoned.state else {
            panic!("a pending wait is timed");
        };
        assert!(registered(key));
        drop(abandoned);
        assert!(!registered(key));
    }

    #[test]
    fn event_futures_keep_their_auto_traits() {
        fn send_sync_unpin<T: Send + Sync + Unpin>() {}
        send_sync_unpin::<FileEventFuture<RecorderEvent>>();
        send_sync_unpin::<FileEventFuture<PlayerEvent>>();
    }

    #[test]
    fn refused_creation_options_and_families_complete_at_submission() {
        struct NeverCreated;
        impl Owner for NeverCreated {
            type Closed = ();
            fn shutdown(
                &mut self,
                _: Instant,
                _: &EventQueue<Self::Event>,
                _: &Control,
                _: &mut [Job<Self>],
            ) {
            }
            type Finish = std::convert::Infallible;
            fn finish_request(&mut self, finish: Self::Finish) {
                match finish {}
            }
            type Event = ();
            fn poll_native(&mut self) -> Result<Option<()>> {
                unreachable!()
            }
        }
        impl HandleOwner for NeverCreated {
            type Options = FileRecorderOptions;
            type Prepared = super::super::recorder::PreparedRecorder;
            const THREAD_NAME: &'static str = "must-not-create";
            fn prepare(api: FileApi, options: &Self::Options) -> Result<Self::Prepared> {
                FileRecorder::prepare(api, options)
            }
            fn create(_: Self::Prepared) -> Result<Self> {
                panic!("the refused owner was created")
            }
            fn dropped_event_count(&self) -> u64 {
                unreachable!()
            }
            fn source_format(&self) -> Result<Option<SourceFormat>> {
                unreachable!()
            }
        }
        let api = mock::recorder_api();
        for options in [
            FileRecorderOptions::new(SourceSpec::new("mock").unwrap())
                .naming(FileNaming::Exact)
                .format_change_rollover(true),
            FileRecorderOptions::new(SourceSpec::new("mock").unwrap())
                .naming(FileNaming::Pattern("{prefix}/{index}/clip.mkv".into())),
        ] {
            let refused = Worker::<NeverCreated>::create(&api, &options, |worker| worker).submit();
            assert!(refused.is_completed());
            assert!(matches!(block_on(refused), Err(Error::InvalidInput(_))));
            let public = AsyncFileRecorder::create(&api, &options).submit();
            assert!(public.is_completed());
            assert!(matches!(block_on(public), Err(Error::InvalidInput(_))));
        }
        let bad_player = AsyncFilePlayer::create(&api, &FilePlayerOptions::new("")).submit();
        assert!(bad_player.is_completed());
        assert!(matches!(block_on(bad_player), Err(Error::InvalidInput(_))));
        let missing = FileApi::with_runtime(crate::sys::FileRuntime::test_double(
            Err(c"NDIlib_file_player_create"),
            Err(c"NDIlib_file_recorder_create"),
        ));
        let player = AsyncFilePlayer::create(&missing, &FilePlayerOptions::new("mock")).submit();
        assert!(player.is_completed());
        assert!(matches!(
            block_on(player),
            Err(Error::UnsupportedCapability { .. })
        ));
        let options = FileRecorderOptions::new(SourceSpec::new("mock").unwrap());
        let recorder = AsyncFileRecorder::create(&missing, &options).submit();
        assert!(recorder.is_completed());
        assert!(matches!(
            block_on(recorder),
            Err(Error::UnsupportedCapability { .. })
        ));
        let never = Worker::<NeverCreated>::create(&missing, &options, |worker| worker).submit();
        assert!(never.is_completed());
        assert!(matches!(
            block_on(never),
            Err(Error::UnsupportedCapability { .. })
        ));
    }

    #[test]
    fn a_refused_create_allocates_only_its_options_validation() {
        let options = FileRecorderOptions::new(SourceSpec::new("mock").unwrap())
            .naming(FileNaming::Exact)
            .format_change_rollover(true);
        let (resolved, expected) = crate::test_alloc::allocation_count(|| options.check());
        assert!(resolved.is_err());
        let operation = AsyncFileRecorder::create(&mock::recorder_api(), &options);
        let (operation, allocations) = crate::test_alloc::allocation_count(|| operation.submit());
        assert!(operation.is_completed());
        assert_eq!(
            allocations, expected,
            "no channel, control, event queue or owner thread"
        );
        assert!(matches!(block_on(operation), Err(Error::InvalidInput(_))));
    }

    #[test]
    fn a_refused_reset_does_not_wait_behind_a_blocked_owner_command() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        let (blocked, release) = block_owner(&recorder.worker);
        let options = FileRecorderOptions::new(SourceSpec::new("replacement").unwrap())
            .naming(FileNaming::Exact)
            .format_change_rollover(true);
        let refused = recorder.reset(&options).submit();
        let immediate = refused.is_completed();
        let creates = device.creates.load(Ordering::Acquire);
        release.send(()).unwrap();
        block_on(blocked).unwrap();
        assert!(immediate, "invalid reset waited in the command queue");
        assert_eq!(creates, 1);
        assert!(matches!(block_on(refused), Err(Error::InvalidInput(_))));
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn async_creation_refuses_conflicting_options() {
        let options = FileRecorderOptions::new(SourceSpec::new("mock source").unwrap())
            .naming(FileNaming::Exact)
            .format_change_rollover(true);
        assert!(matches!(
            block_on(AsyncFileRecorder::create(&mock::recorder_api(), &options)),
            Err(Error::InvalidInput(
                crate::InputDefect::ExactNameFormatRollover
            ))
        ));
        for pattern in [
            "{prefix}/{index:03}/clip.mkv",
            "{index}/{prefix}.mkv",
            "{prefix}_{index}/clip.mkv",
            "{index}{prefix}.mkv",
        ] {
            let options = FileRecorderOptions::new(SourceSpec::new("mock source").unwrap())
                .naming(FileNaming::Pattern(pattern.into()));
            let api = mock::recorder_api();
            assert!(matches!(
                crate::file::FileRecorder::new(&api, &options),
                Err(Error::InvalidInput(
                    crate::InputDefect::FilenamePatternIndexInDirectory
                ))
            ));
            assert!(matches!(
                block_on(AsyncFileRecorder::create(&api, &options)),
                Err(Error::InvalidInput(
                    crate::InputDefect::FilenamePatternIndexInDirectory
                ))
            ));
        }
    }

    #[test]
    fn queue_sizes_out_of_range_are_refused_before_a_thread_starts() {
        for (options, queue, capacity) in [
            (AsyncFileOptions::new().command_capacity(0), "command", 0),
            (AsyncFileOptions::new().event_capacity(0), "event", 0),
            (
                AsyncFileOptions::new().command_capacity(MAX_CAPACITY + 1),
                "command",
                MAX_CAPACITY + 1,
            ),
            (
                AsyncFileOptions::new().event_capacity(MAX_CAPACITY + 1),
                "event",
                MAX_CAPACITY + 1,
            ),
        ] {
            let created = spawn_owner(
                "file-test-refused",
                || -> Result<LocalOwner> { panic!("the owner was created") },
                options,
                |worker| worker,
            );
            let Err(Error::InvalidInput(cause)) = block_on(created) else {
                panic!("an out-of-range queue capacity was not refused");
            };
            assert_eq!(
                cause,
                crate::InputDefect::FileQueueCapacity { queue, capacity }
            );
        }
        let api = mock::recorder_api();
        let player_options = FilePlayerOptions::new("mock player");
        assert!(matches!(
            block_on(AsyncFilePlayer::create_with_options(
                &api,
                &player_options,
                &AsyncFileOptions::new().event_capacity(0),
            )),
            Err(Error::InvalidInput(crate::InputDefect::FileQueueCapacity {
                queue: "event",
                capacity: 0
            }))
        ));
        let recorder_options = FileRecorderOptions::new(SourceSpec::new("mock source").unwrap());
        assert!(matches!(
            block_on(AsyncFileRecorder::create_with_options(
                &api,
                &recorder_options,
                &AsyncFileOptions::new().command_capacity(0),
            )),
            Err(Error::InvalidInput(crate::InputDefect::FileQueueCapacity {
                queue: "command",
                capacity: 0
            }))
        ));
        // The largest sizes are accepted.
        let (worker, dropped) = local_worker_with_options(
            AsyncFileOptions::new()
                .command_capacity(MAX_CAPACITY)
                .event_capacity(MAX_CAPACITY),
        );
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}
