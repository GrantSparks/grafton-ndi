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

use super::types::resolve_path;
use super::{
    EventText, FileApi, FileDuration, FileInfo, FilePlayer, FilePosition, FileRecorder,
    FinishStatus, PlayerEvent, PlayerOptions, PlayerStats, RecorderEvent, RecorderOptions,
    RecorderState, RecorderStats, RecordingReport, RecordingTarget, RecordingTimecode,
    SourceFormat, SourceSpec, StateError, StopRequest,
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
        Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak,
    },
    task::{Context, Poll, Waker},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

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
/// The default holds 64 pending commands and 256 undelivered events. Each
/// capacity must be from 1 through 4096; the `create_with_options`
/// functions of [`AsyncFilePlayer`] and [`AsyncFileRecorder`] refuse any
/// other with [`Error::InvalidInput`].
#[derive(Clone, Copy, Debug)]
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
    fn validate(self) -> Result<Self> {
        for (queue, capacity) in [
            ("command", self.command_capacity),
            ("event", self.event_capacity),
        ] {
            if !(1..=MAX_CAPACITY).contains(&capacity) {
                return Err(Error::InvalidInput(format!(
                    "async {queue} queue capacity must be between 1 and {MAX_CAPACITY}, not \
                     {capacity}"
                )));
            }
        }
        Ok(self)
    }
}

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

struct Response<T> {
    value: Option<Result<T>>,
    completed: bool,
    waker: WakerSlot,
}

/// The result of a command sent to a player's or recorder's thread.
///
/// Like an `async fn`, this future submits no work until its first poll. Call
/// [`submit`](Self::submit) to submit it immediately instead. Queue admission
/// and operation deadlines start at submission, including any time spent queued.
/// Dropping an unsubmitted future does nothing. After submission, dropping an
/// ordinary command before it begins cancels it; once it has begun, dropping
/// only discards the result. Submitted requests to stop, finish or close run
/// even if their future is dropped, as those methods say.
///
/// An unsubmitted command does not keep an existing player or recorder alive.
/// Dropping the last handle still shuts down the owner as that handle documents;
/// later ordinary submissions fail with [`StateError::Closed`]. Closing remains
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
/// # Errors
///
/// Besides the operation's own errors, every operation fails with
/// [`Error::QueueFull`] and [`QueueLimit::FileCommands`] when the queue
/// refuses it, as described above, and can fail with
/// [`Error::InvalidState`]: with [`StateError::Closed`] when the handle has
/// been closed or its thread stopped normally before carrying the
/// operation out, and with [`StateError::OwnerFailed`] when that thread failed
/// instead. Once the thread has failed, the operation it was running, every
/// operation queued behind it and every later one report
/// [`StateError::OwnerFailed`], never [`StateError::Closed`].
/// A finish or wait already running can also report `Closed` when a concurrent
/// close ends its finalization budget without a settled session outcome.
///
/// # Panics
///
/// Polling again after completion panics.
#[must_use = "futures do nothing until polled or explicitly submitted"]
pub struct FileOperation<T> {
    // Mutex preserves the future's Sync and unwind-safety contracts while the
    // one-shot submission closure itself needs only Send.
    deferred: Mutex<Option<Box<dyn FnOnce() -> Self + Send + 'static>>>,
    response: Arc<Mutex<Response<T>>>,
    cancelled: Arc<AtomicBool>,
    consumed: bool,
}

impl<T> std::fmt::Debug for FileOperation<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileOperation")
            .field("submitted", &lock(&self.deferred).is_none())
            .field("completed", &lock(&self.response).completed)
            .finish_non_exhaustive()
    }
}

impl<T> Future for FileOperation<T> {
    type Output = Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        assert!(!this.consumed, "a completed FileOperation was polled again");
        this.start();
        let value = WakerSlot::poll(&this.response, cx.waker(), |response, _| {
            if let Some(value) = response.value.take() {
                WakerPoll::Ready(value, response.waker.take())
            } else {
                WakerPoll::Pending(Some(&mut response.waker))
            }
        });
        if let Some(value) = value {
            this.consumed = true;
            Poll::Ready(value)
        } else {
            Poll::Pending
        }
    }
}

impl<T> Drop for FileOperation<T> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        // A queued reply may outlive this future, including a cancelled close
        // pruned under Control::closing. Remove executor state before releasing
        // our response ownership, and run its destructor without either lock.
        let waker = lock(&self.response).waker.take();
        WakerSlot::discard(waker);
    }
}

struct Reply<T> {
    response: Arc<Mutex<Response<T>>>,
    cancelled: Arc<AtomicBool>,
    delivered: bool,
    /// The owner thread's control, for a command sent to it. A command that is
    /// dropped without a result reports a failed owner if the thread has
    /// failed.
    owner: Option<Arc<Control>>,
}

fn operation<T>() -> (Reply<T>, FileOperation<T>) {
    let response = Arc::new(Mutex::new(Response {
        value: None,
        completed: false,
        waker: WakerSlot::default(),
    }));
    let cancelled = Arc::new(AtomicBool::new(false));
    (
        Reply {
            response: Arc::clone(&response),
            cancelled: Arc::clone(&cancelled),
            delivered: false,
            owner: None,
        },
        FileOperation {
            deferred: Mutex::new(None),
            response,
            cancelled,
            consumed: false,
        },
    )
}

fn deliver<T>(response: &Mutex<Response<T>>, value: Result<T>, replace: bool) {
    let wake = {
        let mut response = lock(response);
        if response.completed && !replace {
            return;
        }
        response.value = Some(value);
        response.completed = true;
        response.waker.take()
    };
    WakerSlot::wake(wake);
}

/// The error for an operation that did not run because its owner thread is
/// gone: a failed thread, or one that stopped normally.
fn stopped(failed: bool) -> Error {
    Error::InvalidState(if failed {
        StateError::OwnerFailed
    } else {
        StateError::Closed
    })
}

impl<T> FileOperation<T> {
    /// Submit this operation immediately, without waiting for its first poll.
    ///
    /// Submission never waits for the SDK. It checks queue admission and starts
    /// the operation's deadline now. Await the returned future to receive the
    /// result. Dropping it cancels an ordinary command that has not started;
    /// submitted stop, finish and close requests continue even when dropped.
    ///
    /// For example, `drop(recorder.request_stop().submit())` explicitly asks the
    /// recorder to stop without waiting for its result. Merely dropping
    /// `recorder.request_stop()` leaves the recorder running.
    #[must_use = "await the operation for its result, or explicitly drop it"]
    pub fn submit(mut self) -> Self {
        self.start();
        self
    }

    fn deferred(start: impl FnOnce() -> Self + Send + 'static) -> Self {
        Self {
            deferred: Mutex::new(Some(Box::new(start))),
            response: Arc::new(Mutex::new(Response {
                value: None,
                completed: false,
                waker: WakerSlot::default(),
            })),
            cancelled: Arc::new(AtomicBool::new(false)),
            consumed: false,
        }
    }

    fn start(&mut self) {
        loop {
            let start = lock(&self.deferred).take();
            let Some(start) = start else { break };
            // The old value is an unsubmitted placeholder. Its cancellation
            // flag is independent of the submitted operation's flag.
            *self = start();
        }
    }

    fn failed(error: Error) -> Self {
        let (reply, future) = operation();
        reply.complete(Err(error));
        future
    }

    // Only used before the future is returned to the caller. A rejected job's
    // Reply destructor may already have supplied a generic stopped error.
    fn immediate_failure(&self, error: Error) {
        deliver(&self.response, Err(error), true);
    }
}

impl<T> Reply<T> {
    fn complete(mut self, value: Result<T>) {
        self.delivered = true;
        if !self.cancelled.load(Ordering::Acquire) {
            deliver(&self.response, value, false);
        }
    }
}

impl<T> Drop for Reply<T> {
    fn drop(&mut self) {
        if !self.delivered && !self.cancelled.load(Ordering::Acquire) {
            // Unwinding means the owner thread is failing right now, before
            // it can record that; a command left in the queue sees the record.
            let failed =
                std::thread::panicking() || self.owner.as_ref().is_some_and(|owner| owner.failed());
            deliver(&self.response, Err(stopped(failed)), false);
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
    closed: bool,
    /// The queue closed because the owner thread failed.
    failed: bool,
    waiter: Option<EventWaiter>,
}

struct EventQueue<E>(Mutex<Events<E>>);

impl<E> EventQueue<E> {
    fn new(capacity: usize) -> Self {
        Self(Mutex::new(Events {
            queue: VecDeque::new(),
            capacity,
            dropped: 0,
            closed: false,
            failed: false,
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

    /// End every wait: nothing more will be pushed. Idempotent. `failed`
    /// records that the owner thread failed, so waits report that; once
    /// recorded it stays.
    fn close(&self, failed: bool) {
        let wake = {
            let mut events = lock(&self.0);
            events.closed = true;
            events.failed |= failed;
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
            deadline: None,
            timer: None,
            consumed: false,
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
/// [`Error::InvalidState`] and [`StateError::WaiterPending`]. Once the
/// handle is closed and every queued event has been taken, the future fails
/// with [`StateError::Closed`], or with [`StateError::OwnerFailed`] when the
/// thread failed; that happens as soon as the handle's thread stops taking
/// events, before the native object is destroyed. Dropping the
/// future does not stop events from being pumped.
/// Executor callbacks run on File threads; a handle's last clone dropped
/// there requests shutdown without waiting for native cleanup.
///
/// # Errors
///
/// Besides those above, [`Error::InvalidInput`] when the timeout exceeds
/// [`crate::MAX_TIMEOUT`], [`Error::UnexpectedSdkResult`] for an event the
/// SDK delivered malformed, and [`Error::Io`] when the wait has to be
/// timed and the timer thread cannot be started; a later wait tries to start
/// it again.
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
    /// The wait's budget, from its first poll on.
    deadline: Option<Deadline>,
    /// This wait's registration with the deadline timer, while it has one.
    timer: Option<TimerKey>,
    consumed: bool,
}

impl<E> FileEventFuture<E> {
    /// Complete with `value`, giving up the timer registration.
    fn complete(&mut self, value: Result<Option<E>>) -> Poll<Result<Option<E>>> {
        self.consumed = true;
        if let Some(key) = self.timer.take() {
            TIMER.cancel(key);
        }
        Poll::Ready(value)
    }

    /// Stop being the queue's waiter and return executor state for disposal
    /// after unlocking. Another future's waiter is never removed.
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
        assert!(
            !this.consumed,
            "a completed FileEventFuture was polled again"
        );
        // The wait starts at the first poll, which is also where a timeout
        // out of range is reported, before anything else.
        let deadline = match this.deadline {
            Some(deadline) => deadline,
            None => match Deadline::after(this.budget) {
                Ok(deadline) => *this.deadline.insert(deadline),
                Err(error) => return this.complete(Err(error)),
            },
        };
        let token = Arc::downgrade(&this.token);
        let ready = WakerSlot::poll(&this.events.0, cx.waker(), |events, register| {
            if events
                .waiter
                .as_ref()
                .is_some_and(|w| w.token.strong_count() != 0 && !w.token.ptr_eq(&token))
            {
                return WakerPoll::Ready(Err(Error::InvalidState(StateError::WaiterPending)), None);
            }
            let ready = if let Some(event) = events.queue.pop_front() {
                Some(event.map(Some))
            } else if events.closed {
                Some(Err(stopped(events.failed)))
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
                let waiter = events.waiter.get_or_insert_with(|| EventWaiter {
                    token: Weak::clone(&token),
                    deadline: deadline.end(),
                    waker: WakerSlot::default(),
                });
                waiter.token = Weak::clone(&token);
                waiter.deadline = deadline.end();
            }
            WakerPoll::Pending(events.waiter.as_mut().map(|waiter| &mut waiter.waker))
        });
        if let Some(value) = ready {
            return this.complete(value);
        }
        // The deadline never changes, so one registration serves every poll.
        // It is made with no queue lock held: the timer may wake this future
        // at once, which then finds itself registered as the waiter.
        if this.timer.is_none() {
            match TIMER.register(deadline.end(), Weak::clone(&this.expiry), token) {
                Ok(key) => this.timer = Some(key),
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
        if let Some(key) = self.timer.take() {
            TIMER.cancel(key);
        }
        WakerSlot::discard(waker);
    }
}

struct Closing {
    complete: bool,
    failed: bool,
    /// Every close still awaited. Each completes once the object is gone; a
    /// close whose future was dropped is forgotten when the next one arrives,
    /// so this holds at most the closes still awaited.
    replies: Vec<Reply<()>>,
}

struct Control {
    shutdown: AtomicBool,
    /// The first close or last-handle drop starts the one finalization budget.
    shutdown_at: Mutex<Option<Instant>>,
    /// The owner thread has failed. Set before the thread's command queue is
    /// dropped, so a command left in it can tell.
    failed: AtomicBool,
    closing: Mutex<Closing>,
}

impl Control {
    fn new() -> Self {
        Self {
            shutdown: AtomicBool::new(false),
            shutdown_at: Mutex::new(None),
            failed: AtomicBool::new(false),
            closing: Mutex::new(Closing {
                complete: false,
                failed: false,
                replies: Vec::new(),
            }),
        }
    }

    /// Ask the owner thread to end, and complete once it has destroyed its
    /// object. Every close, including one made while another is awaited,
    /// completes then.
    fn request_close(self: &Arc<Self>) -> FileOperation<()> {
        let control = Arc::clone(self);
        FileOperation::deferred(move || control.request_close_now())
    }

    fn request_close_now(&self) -> FileOperation<()> {
        let (reply, future) = operation();
        {
            let mut closing = lock(&self.closing);
            if closing.complete {
                reply.complete(if closing.failed {
                    Err(Error::InvalidState(StateError::OwnerFailed))
                } else {
                    Ok(())
                });
                return future;
            }
            closing
                .replies
                .retain(|reply| !reply.cancelled.load(Ordering::Acquire));
            closing.replies.push(reply);
        }
        self.request_shutdown();
        future
    }

    /// Record the first shutdown request before publishing it. Return whether
    /// shutdown had already been requested, as last-handle drops need to know.
    fn request_shutdown(&self) -> bool {
        let mut at = lock(&self.shutdown_at);
        let requested = at.is_some();
        at.get_or_insert_with(Instant::now);
        self.shutdown.store(true, Ordering::Release);
        requested
    }

    fn shutdown_requested_at(&self) -> Option<Instant> {
        if self.shutdown.load(Ordering::Acquire) {
            *lock(&self.shutdown_at)
        } else {
            None
        }
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    fn completed(&self, failed: bool) {
        if failed {
            self.failed.store(true, Ordering::Release);
        }
        let replies = {
            let mut closing = lock(&self.closing);
            closing.complete = true;
            closing.failed = failed;
            std::mem::take(&mut closing.replies)
        };
        for reply in replies {
            reply.complete(if failed {
                Err(Error::InvalidState(StateError::OwnerFailed))
            } else {
                Ok(())
            });
        }
    }
}

struct OperationContext {
    cancelled: Arc<AtomicBool>,
    control: Arc<Control>,
}

impl OperationContext {
    #[cfg(test)]
    fn interrupted(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || self.control.shutdown.load(Ordering::Acquire)
    }
}

/// A native object that lives on an owner thread. Dropping it there performs
/// its native cleanup.
trait Owner: 'static {
    type Event: Send + 'static;

    /// Take the next native event without waiting.
    fn poll_native(&mut self) -> Result<Option<Self::Event>>;

    /// Bound destruction-time cleanup by the original shutdown request.
    fn shutdown(&mut self, _requested: Instant) {}
}

impl Owner for FilePlayer {
    type Event = PlayerEvent;
    fn poll_native(&mut self) -> Result<Option<Self::Event>> {
        self.poll_event(Duration::ZERO)
    }
}

impl Owner for FileRecorder {
    type Event = RecorderEvent;
    fn poll_native(&mut self) -> Result<Option<Self::Event>> {
        self.poll_event(Duration::ZERO)
    }

    fn shutdown(&mut self, requested: Instant) {
        self.bound_shutdown_wait(requested);
    }
}

type Job<H> = Box<dyn FnOnce(&mut H) + Send + 'static>;

/// Admission to an owner thread's command queue: how many commands wait, and
/// how many may.
///
/// Every queued command counts, requests to stop included. When the queue is
/// full, one request to stop is admitted beyond it, so that a stop is not
/// lost for want of room; while it waits, a further request to stop is
/// refused like any other command. The queue therefore never holds more than
/// `capacity + 1` commands.
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
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            let (next, reserve) = if state >> 1 < self.capacity {
                (state + 2, false)
            } else if stop && state & 1 == 0 {
                (state | 1, true)
            } else {
                return Err(Error::QueueFull(QueueLimit::FileCommands));
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

/// A command's place in the queue. It is given up when the command starts, or
/// when the command is dropped without running.
struct Place {
    admission: Arc<Admission>,
    /// The place is the one beyond the capacity.
    reserve: bool,
}

impl Drop for Place {
    fn drop(&mut self) {
        if self.reserve {
            let previous = self.admission.state.fetch_and(!1, Ordering::AcqRel);
            debug_assert_ne!(previous & 1, 0);
        } else {
            let previous = self.admission.state.fetch_sub(2, Ordering::AcqRel);
            debug_assert_ne!(previous >> 1, 0);
        }
    }
}

/// The handle side of an owner thread, shared by every clone of an async handle.
struct Worker<H: Owner> {
    sender: Sender<Job<H>>,
    admission: Arc<Admission>,
    control: Arc<Control>,
    events: Arc<EventQueue<H::Event>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl<H: Owner> Drop for Worker<H> {
    fn drop(&mut self) {
        // An explicit close has a future to report through. A plain drop has
        // nothing else, so it waits here: when it returns, the owner thread
        // has destroyed the native object, and a recorder has had its file
        // finalized first.
        let closing = self.control.request_shutdown();
        let Some(thread) = lock(&self.thread).take() else {
            return;
        };
        // A File thread may release this last handle through an executor's
        // waker callback. It must not stall the timer/probe thread or join
        // an owner that could join it back while releasing another handle.
        if !closing && !FILE_THREAD.get() {
            // A panic on the owner thread was already reported to its commands.
            let _ = thread.join();
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
        work: impl FnOnce(&mut H, &OperationContext) -> Result<T> + Send + 'static,
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
        W: FnOnce(&mut H, &OperationContext) -> Result<T> + Send + 'static,
    {
        // Keep submission state, not Worker: an unpolled future must not
        // delay last-handle shutdown or make its own drop join the owner.
        let sender = self.sender.clone();
        let admission = Arc::clone(&self.admission);
        let control = Arc::clone(&self.control);
        FileOperation::deferred(move || {
            // A dead owner takes precedence over argument validation, exactly
            // as a command submitted directly to it does.
            if control.failed() {
                return FileOperation::failed(stopped(true));
            }
            if control.shutdown.load(Ordering::Acquire) {
                return FileOperation::failed(stopped(false));
            }
            let work = match prepare() {
                Ok(work) => work,
                Err(error) => return FileOperation::failed(error),
            };
            Self::submit_now(&sender, &admission, &control, stop, work)
        })
    }

    fn submit_now<T: Send + 'static>(
        sender: &Sender<Job<H>>,
        admission: &Arc<Admission>,
        control: &Arc<Control>,
        stop: bool,
        work: impl FnOnce(&mut H, &OperationContext) -> Result<T> + Send + 'static,
    ) -> FileOperation<T> {
        if control.failed() {
            return FileOperation::failed(stopped(true));
        }
        if control.shutdown.load(Ordering::Acquire) {
            return FileOperation::failed(stopped(false));
        }
        let place = match admission.admit(stop) {
            Ok(place) => place,
            Err(error) => return FileOperation::failed(error),
        };
        let (mut reply, future) = operation();
        reply.owner = Some(Arc::clone(control));
        let context = OperationContext {
            cancelled: Arc::clone(&reply.cancelled),
            control: Arc::clone(control),
        };
        let job: Job<H> = Box::new(move |owner| {
            // The command leaves the queue as it starts.
            drop(place);
            if !stop && context.cancelled.load(Ordering::Acquire) {
                return;
            }
            reply.complete(work(owner, &context));
        });
        if sender.send(job).is_err() {
            future.immediate_failure(stopped(control.failed()));
        }
        future
    }

    fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut H) -> Result<T> + Send + 'static,
    ) -> FileOperation<T> {
        self.submit(false, move |owner, _| work(owner))
    }
}

/// Start an owner thread, create its native object there, and return the
/// handle `wrap` builds around the worker.
///
/// Options out of range fail the operation before anything is started.
fn spawn_owner<H: Owner, T: Send + 'static>(
    name: &str,
    create: impl FnOnce() -> Result<H> + Send + 'static,
    options: AsyncFileOptions,
    wrap: impl FnOnce(Arc<Worker<H>>) -> T + Send + 'static,
) -> FileOperation<T> {
    let name = name.to_owned();
    FileOperation::deferred(move || spawn_owner_now(&name, create, options, wrap))
}

fn spawn_owner_now<H: Owner, T: Send + 'static>(
    name: &str,
    create: impl FnOnce() -> Result<H> + Send + 'static,
    options: AsyncFileOptions,
    wrap: impl FnOnce(Arc<Worker<H>>) -> T + Send + 'static,
) -> FileOperation<T> {
    let options = match options.validate() {
        Ok(options) => options,
        Err(error) => return FileOperation::failed(error),
    };
    let (sender, receiver) = mpsc::channel();
    let control = Arc::new(Control::new());
    let events = Arc::new(EventQueue::new(options.event_capacity));
    let worker = Arc::new(Worker {
        sender,
        admission: Arc::new(Admission::new(options.command_capacity)),
        control: Arc::clone(&control),
        events: Arc::clone(&events),
        thread: Mutex::new(None),
    });
    let (ready, future) = operation();
    let thread_worker = Arc::clone(&worker);
    let thread_control = Arc::clone(&control);
    let thread_events = Arc::clone(&events);
    let spawned = thread::Builder::new().name(name.into()).spawn(move || {
        mark_file_thread();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            if ready.cancelled.load(Ordering::Acquire) {
                return;
            }
            let mut owner = match create() {
                Ok(owner) => owner,
                Err(error) => {
                    ready.complete(Err(error));
                    return;
                }
            };
            ready.complete(Ok(wrap(thread_worker)));
            run_owner(&mut owner, &receiver, &thread_control, &thread_events);
            if let Some(requested) = thread_control.shutdown_requested_at() {
                owner.shutdown(requested);
            }
            // No event is pumped from here on, so waits for one end now
            // rather than after the destruction below, which for a recorder
            // can take its whole drop wait.
            thread_events.close(false);
            // Destroyed here, before the close is reported complete. `H` need
            // not be `Send`: it is created, used and dropped on this thread.
            drop(owner);
        }));
        // Again for the paths above that return early or panic. The failure
        // is recorded before `receiver` drops, which drops the commands still
        // queued: they report it.
        let failed = outcome.is_err();
        thread_events.close(failed);
        thread_control.completed(failed);
        drop(receiver);
    });
    match spawned {
        Ok(thread) => *lock(&worker.thread) = Some(thread),
        Err(error) => {
            future.immediate_failure(Error::Io(error));
            events.close(true);
            control.completed(true);
        }
    }
    future
}

fn run_owner<H: Owner>(
    owner: &mut H,
    receiver: &Receiver<Job<H>>,
    control: &Control,
    events: &EventQueue<H::Event>,
) {
    run_owner_wait(owner, control, events, |wait| {
        if wait {
            receiver.recv_timeout(TICK)
        } else {
            receiver.try_recv().map_err(|error| match error {
                mpsc::TryRecvError::Empty => RecvTimeoutError::Timeout,
                mpsc::TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
            })
        }
    });
}

/// The wait is injectable so tests can assert command fairness and idle
/// backoff without making wall-clock scheduling assertions.
fn run_owner_wait<H: Owner>(
    owner: &mut H,
    control: &Control,
    events: &EventQueue<H::Event>,
    mut command: impl FnMut(bool) -> std::result::Result<Job<H>, RecvTimeoutError>,
) {
    while !control.shutdown.load(Ordering::Acquire) {
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
        if control.shutdown.load(Ordering::Acquire) {
            break;
        }
        // A full burst may have left a backlog. Service commands without
        // inserting a tick before the next burst. Empty/error paths retain
        // the timed wait to avoid spinning.
        match command(!full) {
            Ok(job) => {
                if control.shutdown.load(Ordering::Acquire) {
                    break;
                }
                job(owner);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
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
    fn probe(&self, api: &FileApi, path: &Path) -> FileOperation<FileInfo> {
        // Resolve at submission, before the probe waits in its queue.
        let path = match resolve_path(path, "file path") {
            Ok((path, _)) => path,
            Err(error) => return FileOperation::failed(error),
        };
        let api = api.clone();
        let (reply, future) = operation();
        let job: ProbeJob = Box::new(move || {
            if !reply.cancelled.load(Ordering::Acquire) {
                reply.complete(api.probe(path));
            }
        });
        if let Err(error) = self.submit(job) {
            future.immediate_failure(error);
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
/// Dropping the last clone stops playback, removes the advertised source and
/// then **blocks the dropping thread** until the player's thread has
/// destroyed the player, for as long as the SDK takes. When the drop
/// returns, the source is gone. A caller that must not block, such as a task
/// on an async executor, awaits [`close`](Self::close) instead, which does the
/// same without blocking; dropping the clones afterwards does not block.
/// A last-clone drop performed by an executor callback on a File owner, timer
/// or probe thread requests shutdown without waiting, so those threads can
/// keep delivering notifications. Await `close` to observe cleanup completing.
#[derive(Clone)]
pub struct AsyncFilePlayer {
    worker: Arc<Worker<FilePlayer>>,
}

impl AsyncFilePlayer {
    /// Create a player and advertise its source, with the default queue sizes.
    ///
    /// The future fails as [`FileApi::create_player`] does. The options are
    /// only read, so they stay with the caller whatever the outcome.
    pub fn create(api: &FileApi, options: &PlayerOptions) -> FileOperation<Self> {
        Self::create_with_options(api, options, AsyncFileOptions::new())
    }

    /// Create a player and advertise its source, with the given queue sizes.
    ///
    /// The future fails as [`create`](Self::create) does, and with
    /// [`Error::InvalidInput`] when a queue size is out of range.
    pub fn create_with_options(
        api: &FileApi,
        options: &PlayerOptions,
        asynchronous: AsyncFileOptions,
    ) -> FileOperation<Self> {
        let (api, options) = (api.clone(), options.clone());
        spawn_owner(
            "ndi-file-player",
            move || api.create_player(&options),
            asynchronous,
            |worker| Self { worker },
        )
    }

    /// Read a recording's duration and track layout; see [`FileApi::probe`].
    /// No player is created and nothing is advertised.
    ///
    /// Probes run one at a time, in submission order, on one thread
    /// that every async probe in the process shares; it starts with the first
    /// probe. At most 64 wait for it. Dropping the future before its probe has
    /// begun cancels the probe.
    ///
    /// # Errors
    ///
    /// Those of [`FileApi::probe`], and [`Error::QueueFull`] with
    /// [`QueueLimit::FileProbes`] when 64 probes are already waiting; the same
    /// call can succeed once they have run. [`Error::Io`] when the probe
    /// thread is not running and cannot be started; the next probe tries to
    /// start it again. [`Error::InvalidState`] with [`StateError::OwnerFailed`]
    /// when the probe panicked; the thread goes on with the next one.
    pub fn probe(api: &FileApi, path: impl AsRef<Path>) -> FileOperation<FileInfo> {
        let (api, path) = (api.clone(), path.as_ref().to_path_buf());
        FileOperation::deferred(move || PROBES.probe(&api, &path))
    }

    /// Play a file; see [`FilePlayer::start`].
    pub fn start(&self, path: impl AsRef<Path>) -> FileOperation<()> {
        let path = path.as_ref().to_path_buf();
        self.worker.prepare(
            move || {
                // Resolve before waiting in the command queue.
                let (path, _) = resolve_path(&path, "file path")?;
                Ok(move |player: &mut FilePlayer, _: &OperationContext| player.start(path))
            },
            false,
        )
    }
    /// Stop the current file; the source stays advertised. The stop runs even
    /// if the future is dropped after its first poll or explicit submission.
    pub fn stop(&self) -> FileOperation<()> {
        self.worker.submit(true, |p, _| {
            p.stop();
            Ok(())
        })
    }
    /// Whether a file is playing.
    pub fn is_running(&self) -> FileOperation<bool> {
        self.worker.call(|p| Ok(p.is_running()))
    }
    /// Wait up to `timeout` for the player's next event.
    pub fn next_event(&self, timeout: Duration) -> FileEventFuture<PlayerEvent> {
        self.worker.events.wait(timeout)
    }
    /// How many events the SDK discarded before the player's thread took them.
    pub fn dropped_event_count(&self) -> FileOperation<u64> {
        self.worker.call(|p| Ok(p.dropped_event_count()))
    }
    /// How many events this handle's own queue discarded because
    /// [`next_event`](Self::next_event) did not keep up.
    pub fn notification_dropped_count(&self) -> u64 {
        lock(&self.worker.events.0).dropped
    }
    /// Counters for the current or most recent file.
    pub fn stats(&self) -> FileOperation<Option<PlayerStats>> {
        self.worker.call(|p| p.stats())
    }
    /// Seek to a position on the file's timeline; see [`FilePlayer::seek`].
    pub fn seek(&self, position: FilePosition) -> FileOperation<()> {
        self.worker.call(move |p| p.seek(position))
    }
    /// Seek to an approximate frame index; see
    /// [`FilePlayer::seek_frame_approximate`].
    pub fn seek_frame_approximate(&self, frame: u64) -> FileOperation<()> {
        self.worker.call(move |p| p.seek_frame_approximate(frame))
    }
    /// Pause or resume the current file.
    pub fn set_paused(&self, paused: bool) -> FileOperation<()> {
        self.worker.call(move |p| {
            p.set_paused(paused);
            Ok(())
        })
    }
    /// Whether the current file is paused.
    pub fn is_paused(&self) -> FileOperation<bool> {
        self.worker.call(|p| Ok(p.is_paused()))
    }
    /// Set whether playback restarts at the end of a file. The setting is
    /// kept across files.
    pub fn set_looping(&self, looping: bool) -> FileOperation<()> {
        self.worker.call(move |p| {
            p.set_looping(looping);
            Ok(())
        })
    }
    /// Whether playback restarts at the end of a file.
    pub fn is_looping(&self) -> FileOperation<bool> {
        self.worker.call(|p| Ok(p.is_looping()))
    }
    /// The current file's best-known duration, and whether it is final.
    pub fn duration(&self) -> FileOperation<Option<FileDuration>> {
        self.worker.call(|p| Ok(p.duration()))
    }
    /// The position of the frame sent last.
    pub fn position(&self) -> FileOperation<Option<FilePosition>> {
        self.worker.call(|p| Ok(p.position()))
    }
    /// The recorded source timecode of the frame sent last.
    pub fn source_timecode(&self) -> FileOperation<Option<Timecode>> {
        self.worker.call(|p| Ok(p.source_timecode()))
    }
    /// The track layout of the current file.
    pub fn source_format(&self) -> FileOperation<Option<SourceFormat>> {
        self.worker.call(|p| p.source_format())
    }
    /// The NDI source this player advertises.
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
    /// operations fail with [`StateError::Closed`], and dropping the clones
    /// afterwards does not block. It does not wait in the command queue, so
    /// a full queue never refuses it.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidState`] with [`StateError::OwnerFailed`] when the
    /// object's thread failed.
    pub fn close(&self) -> FileOperation<()> {
        self.worker.control.request_close()
    }
}

/// A [`FileRecorder`] on a thread of its own, driven through futures.
///
/// Clones share one recorder, whose events are pumped continuously, so its
/// session report stays current without the caller polling.
///
/// # Finishing
///
/// Await [`finish`](Self::finish) and check for [`FinishStatus::Closed`] to
/// know a recording is complete, then await [`close`](Self::close). Neither
/// blocks the executor.
///
/// # Dropping blocks
///
/// Dropping the last clone **blocks the dropping thread** until the
/// recorder's thread has finished: it stops a session that is still open,
/// waits up to [`RecorderOptions::drop_wait`] for the SDK to finalize the
/// file, and destroys the recorder. This is deliberate: when the drop
/// returns, the file is finalized as far as the SDK reported within that
/// wait, even when the drop happens as the program exits. The outcome cannot
/// be reported. A caller that must not block, such as a task on an async
/// executor, awaits [`close`](Self::close) instead, which does the same
/// without blocking; dropping the clones afterwards does not block.
/// A last-clone drop performed by an executor callback on a File owner, timer
/// or probe thread requests shutdown without waiting for finalization or
/// destruction. Await `close` to observe cleanup completing.
#[derive(Clone)]
pub struct AsyncFileRecorder {
    worker: Arc<Worker<FileRecorder>>,
}

impl AsyncFileRecorder {
    /// Create a recorder with the default queue sizes.
    ///
    /// The future fails as [`FileApi::create_recorder`] does, including with
    /// [`Error::InvalidInput`] for options that conflict. The options are
    /// only read, so they stay with the caller whatever the outcome.
    pub fn create(api: &FileApi, options: &RecorderOptions) -> FileOperation<Self> {
        Self::create_with_options(api, options, AsyncFileOptions::new())
    }
    /// Create a recorder with the given queue sizes.
    ///
    /// The future fails as [`create`](Self::create) does, and with
    /// [`Error::InvalidInput`] when a queue size is out of range.
    pub fn create_with_options(
        api: &FileApi,
        options: &RecorderOptions,
        asynchronous: AsyncFileOptions,
    ) -> FileOperation<Self> {
        let (api, options) = (api.clone(), options.clone());
        spawn_owner(
            "ndi-file-recorder",
            move || api.create_recorder(&options),
            asynchronous,
            |worker| Self { worker },
        )
    }
    /// Start a session; see [`FileRecorder::start`]. Generated starts carry a
    /// [`SessionTimecode`](super::SessionTimecode), validated before queuing.
    pub fn start(
        &self,
        target: &RecordingTarget,
        timecode: RecordingTimecode,
    ) -> FileOperation<u64> {
        let target = target.clone();
        self.worker.call(move |r| r.start(&target, timecode))
    }
    /// Ask the SDK to stop the session. Once submitted by polling or calling
    /// [`FileOperation::submit`], the request runs even if its future is dropped.
    pub fn request_stop(&self) -> FileOperation<StopRequest> {
        self.worker.submit(true, |r, _| Ok(r.request_stop()))
    }
    /// Stop the session and wait up to `timeout` for its last file to be
    /// closed; see [`FileRecorder::finish`].
    ///
    /// The wait starts on the first poll or explicit submission, including time
    /// the command spends queued. After submission, dropping the future ends
    /// the wait but not the stop: the recorder keeps stopping and its events
    /// keep being pumped, and
    /// [`wait_finished`](Self::wait_finished) can pick the wait up again.
    /// If another clone closes the recorder during the wait, the remaining
    /// budget is narrowed to [`RecorderOptions::drop_wait`] from that close's
    /// submission, or this wait's deadline if earlier. Closure established in
    /// that time is reported normally; an unsettled session reports
    /// [`StateError::Closed`] instead of [`FinishStatus::TimedOut`], because
    /// the recorder is being destroyed and cannot be waited on again.
    ///
    /// Events taken during the wait go to [`next_event`](Self::next_event)
    /// as the recorder's thread delivers any other, a malformed one as
    /// [`Error::UnexpectedSdkResult`]; it does not end the wait.
    ///
    /// # Errors
    ///
    /// Those of [`FileRecorder::finish`], and those every
    /// [`FileOperation`] can fail with.
    pub fn finish(&self, timeout: Duration) -> FileOperation<FinishStatus> {
        self.finish_inner(timeout, true)
    }
    /// Wait up to `timeout` for a stopping session to end; see
    /// [`FileRecorder::wait_finished`].
    ///
    /// Events taken during the wait are delivered as by
    /// [`finish`](Self::finish). A concurrent close narrows the wait and can
    /// return [`StateError::Closed`] on the same terms as `finish`.
    ///
    /// # Errors
    ///
    /// Those of [`FileRecorder::wait_finished`], and those every
    /// [`FileOperation`] can fail with.
    pub fn wait_finished(&self, timeout: Duration) -> FileOperation<FinishStatus> {
        self.finish_inner(timeout, false)
    }

    /// Replace the native recorder after abandoning its old session; see
    /// [`FileRecorder::reset`]. Queued events already delivered to this
    /// worker's event stream remain available to the caller.
    pub fn reset(&self, options: &RecorderOptions) -> FileOperation<RecordingReport> {
        let options = options.clone();
        self.worker.call(move |recorder| recorder.reset(&options))
    }

    fn finish_inner(&self, timeout: Duration, stop: bool) -> FileOperation<FinishStatus> {
        let events = Arc::clone(&self.worker.events);
        self.worker.prepare(
            move || {
                // Start at submission, including queue time and the native stop.
                let deadline = Deadline::after(timeout)?;
                Ok(
                    move |recorder: &mut FileRecorder, context: &OperationContext| {
                        if stop {
                            recorder.request_stop();
                        }
                        let outcome = recorder.wait_finished_with(
                            deadline,
                            |recorder, deadline| {
                                if let Some(requested) = context.control.shutdown_requested_at() {
                                    recorder.limit_shutdown_wait(deadline, requested)
                                } else {
                                    deadline
                                }
                            },
                            |polled| {
                                // As the pump delivers them: a malformed event as its error.
                                if let Some(event) = polled.transpose() {
                                    events.push(event);
                                }
                                context.cancelled.load(Ordering::Acquire)
                            },
                        )?;
                        if matches!(outcome, FinishStatus::TimedOut(_))
                            && context.control.shutdown.load(Ordering::Acquire)
                            && !context.cancelled.load(Ordering::Acquire)
                        {
                            // The active wait has spent the finalization budget;
                            // destruction must not start a second one.
                            recorder.shutdown_wait_spent();
                            return Err(Error::InvalidState(StateError::Closed));
                        }
                        Ok(outcome)
                    },
                )
            },
            stop,
        )
    }

    /// Wait up to `timeout` for the recorder's next event. Dropping the
    /// future does not affect the recording.
    pub fn next_event(&self, timeout: Duration) -> FileEventFuture<RecorderEvent> {
        self.worker.events.wait(timeout)
    }
    /// How many events the SDK discarded before the recorder's thread took them.
    pub fn dropped_event_count(&self) -> FileOperation<u64> {
        self.worker.call(|r| Ok(r.dropped_event_count()))
    }
    /// How many events this handle's own queue discarded because
    /// [`next_event`](Self::next_event) did not keep up.
    pub fn notification_dropped_count(&self) -> u64 {
        lock(&self.worker.events.0).dropped
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
    pub fn is_recording(&self) -> FileOperation<bool> {
        self.worker.call(|r| Ok(r.is_recording()))
    }
    /// Switch to recording another source; see [`FileRecorder::set_source`].
    pub fn set_source(&self, source: &SourceSpec) -> FileOperation<()> {
        let source = source.clone();
        self.worker.call(move |r| r.set_source(&source))
    }
    /// The source the SDK accepted last; see
    /// [`FileRecorder::last_accepted_source`].
    pub fn last_accepted_source(&self) -> FileOperation<Option<SourceSpec>> {
        self.worker.call(|r| Ok(r.last_accepted_source().cloned()))
    }
    /// The file the recorder's events reported open last and not yet closed.
    pub fn reported_current_file(&self) -> FileOperation<Option<EventText>> {
        self.worker.call(|r| Ok(r.reported_current_file().cloned()))
    }
    /// Close the current file and continue in a new one; see
    /// [`FileRecorder::rollover`].
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
    pub fn inject_metadata<S: MetadataSource + ?Sized>(&self, metadata: &S) -> FileOperation<()> {
        let metadata = MetadataSource::to_owned(metadata);
        self.worker.call(move |r| r.inject_metadata(&metadata))
    }
    /// The recorder's running counters.
    pub fn stats(&self) -> FileOperation<Option<RecorderStats>> {
        self.worker.call(|r| r.stats())
    }
    /// The format of the source, once the recorder has seen audio or video.
    pub fn source_format(&self) -> FileOperation<Option<SourceFormat>> {
        self.worker.call(|r| r.source_format())
    }
    /// Stop an open session, destroy the recorder and end its thread, for
    /// every clone of this handle.
    ///
    /// A session that is still open is stopped and given up to
    /// [`RecorderOptions::drop_wait`] from this close's submission to finalize
    /// its file, including time spent in an in-flight finish or wait. A finish
    /// already running uses the earlier of that budget and its own deadline;
    /// destruction does not start another wait after it expires. The future
    /// completes when the SDK has destroyed the recorder, and does not say
    /// whether the file was finalized: await [`finish`](Self::finish) first to
    /// learn that. Nothing blocks the calling thread. Once submitted by polling
    /// or calling [`FileOperation::submit`], the close runs even if its future
    /// is dropped. Closing again, from this or another clone,
    /// while or after it runs completes at the same time, with the same
    /// outcome; later operations fail with [`StateError::Closed`], and
    /// dropping the clones afterwards does not block. It does not wait in the
    /// command queue, so a full queue never refuses it.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidState`] with [`StateError::OwnerFailed`] when the
    /// object's thread failed.
    pub fn close(&self) -> FileOperation<()> {
        self.worker.control.request_close()
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
        type Event = usize;
        fn poll_native(&mut self) -> Result<Option<usize>> {
            self.polls += 1;
            if self.fail {
                return Err(Error::InvalidFrame("malformed fake event".into()));
            }
            if self.remaining == 0 {
                return Ok(None);
            }
            self.remaining -= 1;
            Ok(Some(self.polls))
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
        run_owner_wait(&mut owner, &control, &events, |wait| {
            waits.push(wait);
            match waits.len() {
                1 => Ok(Box::new(|owner: &mut BurstOwner| {
                    assert_eq!(owner.polls, 16)
                })),
                2 => Ok(Box::new(|owner: &mut BurstOwner| {
                    assert_eq!(owner.polls, 32)
                })),
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
    fn event_idle_and_error_paths_wait_and_shutdown_ends_continuous_bursts() {
        for fail in [false, true] {
            let mut owner = BurstOwner {
                remaining: 0,
                polls: 0,
                fail,
            };
            let events = EventQueue::new(10);
            run_owner_wait(&mut owner, &Control::new(), &events, |wait| {
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
        run_owner_wait(&mut owner, &control, &EventQueue::new(16), |wait| {
            assert!(!wait);
            commands += 1;
            let control = Arc::clone(&control);
            Ok(Box::new(move |_: &mut BurstOwner| {
                control.shutdown.store(true, Ordering::Release);
            }))
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
    fn block_owner<H: Owner>(worker: &Worker<H>) -> (FileOperation<()>, Sender<()>) {
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
        owner_failed(block_on(worker.control.request_close()));
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
                Ok(|_: &mut LocalOwner, _: &OperationContext| Ok(()))
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
        block_on(worker.control.request_close()).unwrap();
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
        let mut stopping = worker.submit(true, |owner, _| {
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
            .submit(false, move |owner, context| {
                owner.counter += 1;
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                assert!(context.interrupted());
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
            matches!(result, Err(Error::InvalidState(StateError::OwnerFailed))),
            "expected OwnerFailed, got {result:?}"
        );
    }

    fn closed<T: std::fmt::Debug>(result: Result<T>) {
        assert!(
            matches!(result, Err(Error::InvalidState(StateError::Closed))),
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
        owner_failed(block_on(worker.control.request_close()));
        owner_failed(block_on(worker.call(|_| Ok(()))));
        owner_failed(block_on(worker.submit(true, |_, _| Ok(()))));
        owner_failed(block_on(worker.events.wait(Duration::from_millis(10))));
    }

    #[test]
    fn ordinary_close_reports_closed_to_later_operations_and_waits() {
        let (worker, dropped) = local_worker();
        let pending_wait = worker.events.wait(Duration::from_secs(30));
        block_on(worker.control.request_close()).unwrap();
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
        closed(block_on(worker.call(|_| Ok(()))));
        closed(block_on(worker.submit(true, |_, _| Ok(()))));
        closed(block_on(pending_wait));
        closed(block_on(worker.events.wait(Duration::from_millis(10))));
        block_on(worker.control.request_close()).unwrap();
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

    fn mock_recorder_with_drop_wait(
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
                mock::recorder_api().create_recorder(
                    &RecorderOptions::new(SourceSpec::new("mock source")?).drop_wait(drop_wait),
                )
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
            .with_address(SourceAddress::Ip("192.0.2.10:5962".into()));
        let options = RecorderOptions::new(SourceSpec::try_from(&selected).unwrap());
        let recorder = block_on(spawn_owner(
            "file-endpoint-copy",
            move || {
                installed.install();
                mock::recorder_api().create_recorder(&options)
            },
            AsyncFileOptions::new(),
            |worker| AsyncFileRecorder { worker },
        ))
        .unwrap();
        drop(selected);
        let command = {
            let selected = Source::new("STUDIO (Program)")
                .with_address(SourceAddress::Url("ndi://camera.local:5963/path".into()));
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
        assert!(!recorder.worker.control.shutdown.load(Ordering::Acquire));
        assert!(block_on(recorder.is_recording()).unwrap());
        assert_eq!(device.source_changes.load(Ordering::Acquire), 0);
        assert_eq!(device.rollovers.load(Ordering::Acquire), 0);
        assert_eq!(device.stops.load(Ordering::Acquire), 0);
        assert_eq!(device.destroys.load(Ordering::Acquire), 0);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn unpolled_finish_does_not_consume_its_wait_budget() {
        let (device, recorder) = mock_recorder(Closing::WhenWaitedFor);
        let wait = Duration::from_millis(50);
        let finishing = recorder.finish(wait);
        thread::sleep(wait + wait);
        assert_eq!(device.stops.load(Ordering::Acquire), 0);
        let outcome = block_on(finishing).unwrap();
        assert!(matches!(outcome, FinishStatus::Closed(_)), "{outcome:?}");
        block_on(recorder.close()).unwrap();
    }

    /// Wait until the owner thread has stopped `device`'s session.
    fn await_stop(device: &mock::Recorder) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while device.stops.load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < deadline, "the recorder was never stopped");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn await_wait(device: &mock::Recorder, previous: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while device.waits.load(Ordering::Acquire) <= previous {
            assert!(
                Instant::now() < deadline,
                "the recorder never entered its wait"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn finish_and_wait_report_closure_when_another_clone_closes_within_the_shared_budget() {
        for stop in [true, false] {
            let (device, recorder) = mock_recorder_with_drop_wait(
                Closing::WhenTold,
                AsyncFileOptions::new(),
                Duration::from_secs(2),
            );
            if !stop {
                block_on(recorder.request_stop()).unwrap();
            }
            let previous = device.waits.load(Ordering::Acquire);
            let finishing = if stop {
                recorder.finish(Duration::from_secs(30))
            } else {
                recorder.wait_finished(Duration::from_secs(30))
            }
            .submit();
            await_wait(&device, previous);
            let other = recorder.clone();
            let closed = other.close().submit();
            // The original interruption path returned TimedOut within one
            // 25 ms poll. Keep closure withheld past that point.
            thread::sleep(Duration::from_millis(80));
            assert!(!lock(&finishing.response).completed);
            device.close();
            let outcome = block_on(finishing).unwrap();
            assert!(matches!(outcome, FinishStatus::Closed(_)), "{outcome:?}");
            block_on(closed).unwrap();
            assert!(device.closed_before_destroy.load(Ordering::Acquire));
            assert_eq!(device.destroys.load(Ordering::Acquire), 1);
            assert!(matches!(
                block_on(recorder.wait_finished(Duration::ZERO)),
                Err(Error::InvalidState(StateError::Closed))
            ));
        }
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
        await_wait(&device, 0);
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
            Err(Error::InvalidState(StateError::Closed))
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
        let (device, recorder) = mock_recorder(Closing::WhenTold);
        let finishing = recorder.finish(Duration::from_secs(30)).submit();
        await_wait(&device, 0);
        let closing = recorder.close().submit();
        device.dropped_events.store(1, Ordering::Release);
        device.close();
        let outcome = block_on(finishing).unwrap();
        assert!(
            matches!(outcome, FinishStatus::Indeterminate(_)),
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
            Closing::WhenTold,
            AsyncFileOptions::new(),
            Duration::from_secs(2),
        );
        let finishing = recorder.finish(Duration::from_millis(120)).submit();
        await_wait(&device, 0);
        let began = Instant::now();
        let closed = recorder.close().submit();
        assert!(matches!(
            block_on(finishing),
            Err(Error::InvalidState(StateError::Closed))
        ));
        block_on(closed).unwrap();
        assert!(
            began.elapsed() < Duration::from_millis(500),
            "close extended the finish's deadline"
        );
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn dropping_the_last_handle_during_a_finish_reports_the_closure_it_waits_for() {
        let (device, recorder) = mock_recorder_with_drop_wait(
            Closing::After(Duration::from_millis(120)),
            AsyncFileOptions::new(),
            Duration::from_secs(2),
        );
        let finishing = recorder.finish(Duration::from_secs(30)).submit();
        await_wait(&device, 0);
        let (dropped, drop_done) = mpsc::channel();
        let dropping = thread::spawn(move || {
            drop(recorder);
            dropped.send(()).unwrap();
        });
        let outcome = block_on(finishing).unwrap();
        assert!(matches!(outcome, FinishStatus::Closed(_)), "{outcome:?}");
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
        await_wait(&device, 0);
        drop(finishing);
        // A following command proves cancellation ended the long wait.
        assert_eq!(block_on(recorder.state()).unwrap(), RecorderState::Stopping);
        let closed = recorder.close().submit();
        assert!(!lock(&closed.response).completed);
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
        let waiting = recorder.wait_finished(Duration::from_secs(5)).submit();
        device.close();
        let outcome = block_on(waiting).unwrap();
        let FinishStatus::Closed(report) = outcome else {
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
            move |owner: &mut LocalOwner, _: &OperationContext| {
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
        let running = queue.probe(&api, Path::new("running.mkv"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while PROBES_BEGUN.load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < deadline, "the first probe never began");
            thread::sleep(Duration::from_millis(1));
        }
        // Two wait behind the running one; a third does not fit.
        let waiting = queue.probe(&api, Path::new("waiting.mkv"));
        let cancelled = queue.probe(&api, Path::new("cancelled.mkv"));
        let refused = block_on(queue.probe(&api, Path::new("refused.mkv")));
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
            block_on(queue.probe(&api, Path::new("later.mkv"))),
            Err(Error::NativeOperationFailed { .. })
        ));
        assert_eq!(PROBES_BEGUN.load(Ordering::Acquire), 3);
    }

    #[test]
    fn a_probe_that_panics_fails_alone() {
        let queue = ProbeQueue::new(1);
        let (reply, failing) = operation::<FileInfo>();
        queue
            .submit(Box::new(move || {
                let _unanswered = reply;
                panic!("probe failure");
            }))
            .unwrap();
        owner_failed(block_on(failing));
        // The thread goes on with the next probe.
        assert!(matches!(
            block_on(queue.probe(&mock::recorder_api(), Path::new("b.mkv"))),
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
            !lock(&closed.response).completed,
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
            Err(Error::InvalidState(StateError::WaiterPending))
        ));
        drop(first);
        assert_eq!(block_on(events.wait(Duration::ZERO)).unwrap(), None);
    }

    #[test]
    fn closing_wakes_pending_event_future_and_rejects_new_commands() {
        let (worker, dropped) = local_worker();
        let event = worker.events.wait(Duration::from_secs(3));
        block_on(worker.control.request_close()).unwrap();
        assert!(matches!(
            block_on(event),
            Err(Error::InvalidState(StateError::Closed))
        ));
        assert!(matches!(
            block_on(worker.call(|_| Ok(()))),
            Err(Error::InvalidState(StateError::Closed))
        ));
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn an_async_finish_whose_stop_outlasts_its_wait_takes_the_queued_close() {
        let (device, recorder) = mock_recorder(Closing::DuringSlowStop(Duration::from_millis(80)));
        let outcome = block_on(recorder.finish(Duration::from_millis(10))).unwrap();
        assert!(matches!(outcome, FinishStatus::Closed(_)), "{outcome:?}");
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
        for finishing in [recorder.finish(too_long), recorder.wait_finished(too_long)] {
            let finishing = finishing.submit();
            // Completed at submission: nothing was queued.
            assert!(lock(&finishing.response).completed);
            let result = block_on(finishing);
            assert!(
                matches!(&result, Err(Error::InvalidInput(reason)) if reason.contains("MAX_TIMEOUT")),
                "{result:?}"
            );
        }
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
            matches!(&polled, Poll::Ready(Err(Error::InvalidInput(reason))) if reason.contains("MAX_TIMEOUT")),
            "{polled:?}"
        );
        assert!(waiting.timer.is_none());
        // It took nothing and left no waiter behind.
        assert_eq!(block_on(events.wait(Duration::ZERO)).unwrap(), Some(7));
    }

    /// A waker for a poll whose wake-up the test does not wait for.
    fn idle_waker() -> Waker {
        Waker::from(Arc::new(Signal(Mutex::new(false), Condvar::new())))
    }

    #[test]
    fn an_async_finish_delivers_a_malformed_event_it_takes_to_the_event_stream() {
        let (device, recorder) = mock_recorder_with(Closing::Never, AsyncFileOptions::new());
        let finishing = recorder.finish(Duration::from_secs(5)).submit();
        while device.stops.load(Ordering::Acquire) == 0 {
            thread::sleep(Duration::from_millis(1));
        }
        // The owner thread is inside the finish, so the finish takes this.
        let mut close = mock::recorder_event(mock::FILE_CLOSED, mock::TAKE);
        close.source_format_payload.video_track_count = 3;
        device.push_event(close);
        let outcome = block_on(finishing).unwrap();
        assert!(matches!(outcome, FinishStatus::Closed(_)), "{outcome:?}");
        // The start, pumped before the finish began, then the malformed close.
        let started = block_on(recorder.next_event(Duration::ZERO))
            .unwrap()
            .unwrap();
        assert_eq!(started.kind(), RecorderEventKind::Started);
        let malformed = block_on(recorder.next_event(Duration::ZERO));
        assert!(
            matches!(malformed, Err(Error::UnexpectedSdkResult(_))),
            "{malformed:?}"
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
            matches!(outcome, Err(Error::InvalidState(StateError::Closed))),
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
        let key = answered.timer.expect("a pending wait is timed");
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
        let key = abandoned.timer.expect("a pending wait is timed");
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
    fn async_creation_refuses_conflicting_options() {
        let options = RecorderOptions::new(SourceSpec::new("mock source").unwrap())
            .naming(FileNaming::Exact)
            .format_change_rollover(true);
        assert!(matches!(
            block_on(AsyncFileRecorder::create(&mock::recorder_api(), &options)),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn queue_sizes_out_of_range_are_refused_before_a_thread_starts() {
        for options in [
            AsyncFileOptions::new().command_capacity(0),
            AsyncFileOptions::new().event_capacity(0),
            AsyncFileOptions::new().command_capacity(MAX_CAPACITY + 1),
            AsyncFileOptions::new().event_capacity(MAX_CAPACITY + 1),
        ] {
            let created = spawn_owner(
                "file-test-refused",
                || -> Result<LocalOwner> { panic!("the owner was created") },
                options,
                |worker| worker,
            );
            assert!(matches!(
                block_on(created),
                Err(Error::InvalidInput(reason)) if reason.contains("capacity")
            ));
        }
        let api = mock::recorder_api();
        let player_options = PlayerOptions::new("mock player");
        assert!(matches!(
            block_on(AsyncFilePlayer::create_with_options(
                &api,
                &player_options,
                AsyncFileOptions::new().event_capacity(0),
            )),
            Err(Error::InvalidInput(_))
        ));
        let recorder_options = RecorderOptions::new(SourceSpec::new("mock source").unwrap());
        assert!(matches!(
            block_on(AsyncFileRecorder::create_with_options(
                &api,
                &recorder_options,
                AsyncFileOptions::new().command_capacity(0),
            )),
            Err(Error::InvalidInput(_))
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
