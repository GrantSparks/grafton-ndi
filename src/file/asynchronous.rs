//! Futures for the File API that work with any executor.
//!
//! Each player or recorder lives on a thread of its own, which runs the
//! commands sent to it and pumps the SDK's events between them. Probes, which
//! need no player, share one thread for the whole process. The deadlines of
//! event futures are kept by one timer thread for the whole process, so that a
//! command that blocks its owner thread cannot make a wait overrun.

use super::types::resolve_path;
use super::{
    EventText, FileApi, FileDuration, FileInfo, FilePlayer, FilePosition, FileRecorder,
    FinishStatus, PlayerEvent, PlayerOptions, PlayerStats, RecorderEvent, RecorderOptions,
    RecorderState, RecorderStats, RecordingReport, RecordingTarget, RecordingTimecode,
    SourceFormat, SourceSpec, StateError, StopRequest,
};
use crate::{timeout::Deadline, Error, MetadataFrame, QueueLimit, Result, Source, Timecode};
use std::{
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

struct Response<T> {
    value: Option<Result<T>>,
    completed: bool,
    waker: Option<Waker>,
}

/// The result of a command sent to a player's or recorder's thread.
///
/// The command is queued when the method that returns this future is called,
/// not when the future is first polled. Dropping the future before the command
/// has begun cancels it; once it has begun, dropping only discards the result.
/// Requests to stop, finish or close run even if their future is dropped, as
/// those methods say.
///
/// # The command queue
///
/// A handle's commands wait in a queue of
/// [`AsyncFileOptions::command_capacity`] places, and a command submitted to a
/// full queue fails at once with [`Error::QueueFull`] and
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
///
/// # Panics
///
/// Polling again after completion panics.
#[must_use = "dropping this future may cancel queued work or discard an already-started result"]
pub struct FileOperation<T> {
    response: Arc<Mutex<Response<T>>>,
    cancelled: Arc<AtomicBool>,
    consumed: bool,
}

impl<T> std::fmt::Debug for FileOperation<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileOperation")
            .field("completed", &lock(&self.response).completed)
            .finish_non_exhaustive()
    }
}

impl<T> Future for FileOperation<T> {
    type Output = Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        assert!(!this.consumed, "a completed FileOperation was polled again");
        let value = {
            let mut response = lock(&this.response);
            if let Some(value) = response.value.take() {
                Some(value)
            } else {
                response.waker = Some(cx.waker().clone());
                None
            }
        };
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
        waker: None,
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
    if let Some(wake) = wake {
        wake.wake();
    }
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
    waker: Waker,
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
            events.waiter.as_ref().map(|w| w.waker.clone())
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    /// End every wait: nothing more will be pushed. Idempotent. `failed`
    /// records that the owner thread failed, so waits report that; once
    /// recorded it stays.
    fn close(&self, failed: bool) {
        let wake = {
            let mut events = lock(&self.0);
            events.closed = true;
            events.failed |= failed;
            events.waiter.take().map(|w| w.waker)
        };
        if let Some(wake) = wake {
            wake.wake();
        }
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
            let events = lock(&self.0);
            events
                .waiter
                .as_ref()
                .filter(|w| w.token.ptr_eq(token) && w.deadline <= now)
                .map(|w| w.waker.clone())
        };
        if let Some(wake) = wake {
            wake.wake();
        }
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

    /// Stop being the queue's waiter, if this future is.
    fn release_waiter(&self) {
        let token = Arc::downgrade(&self.token);
        let mut events = lock(&self.events.0);
        if events
            .waiter
            .as_ref()
            .is_some_and(|w| w.token.ptr_eq(&token))
        {
            events.waiter = None;
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
        let ready = {
            let mut events = lock(&this.events.0);
            if events
                .waiter
                .as_ref()
                .is_some_and(|w| w.token.strong_count() != 0 && !w.token.ptr_eq(&token))
            {
                Some(Err(Error::InvalidState(StateError::WaiterPending)))
            } else {
                let ready = if let Some(event) = events.queue.pop_front() {
                    Some(event.map(Some))
                } else if events.closed {
                    Some(Err(stopped(events.failed)))
                } else if deadline.is_expired() {
                    Some(Ok(None))
                } else {
                    None
                };
                events.waiter = match ready {
                    Some(_) => None,
                    None => Some(EventWaiter {
                        token: Weak::clone(&token),
                        deadline: deadline.end(),
                        waker: cx.waker().clone(),
                    }),
                };
                ready
            }
        };
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
                    this.release_waiter();
                    return this.complete(Err(Error::Io(error)));
                }
            }
        }
        Poll::Pending
    }
}

impl<E> Drop for FileEventFuture<E> {
    fn drop(&mut self) {
        self.release_waiter();
        if let Some(key) = self.timer.take() {
            TIMER.cancel(key);
        }
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
    /// The owner thread has failed. Set before the thread's command queue is
    /// dropped, so a command left in it can tell.
    failed: AtomicBool,
    closing: Mutex<Closing>,
}

impl Control {
    fn new() -> Self {
        Self {
            shutdown: AtomicBool::new(false),
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
    fn request_close(&self) -> FileOperation<()> {
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
        self.shutdown.store(true, Ordering::Release);
        future
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
    waiting: AtomicUsize,
    capacity: usize,
    /// A request to stop is waiting beyond the capacity.
    reserve_taken: AtomicBool,
}

impl Admission {
    fn new(capacity: usize) -> Self {
        Self {
            waiting: AtomicUsize::new(0),
            capacity,
            reserve_taken: AtomicBool::new(false),
        }
    }

    /// A place in the queue for a command, or a request to stop if `stop`.
    ///
    /// # Errors
    ///
    /// [`Error::QueueFull`] with [`QueueLimit::FileCommands`] when the queue
    /// is full and, for a request to stop, another one already waits beyond it.
    fn admit(self: &Arc<Self>, stop: bool) -> Result<Place> {
        let place = |reserve| Place {
            admission: Arc::clone(self),
            reserve,
        };
        if self.waiting.fetch_add(1, Ordering::AcqRel) < self.capacity {
            return Ok(place(false));
        }
        if stop && !self.reserve_taken.swap(true, Ordering::AcqRel) {
            return Ok(place(true));
        }
        self.waiting.fetch_sub(1, Ordering::AcqRel);
        Err(Error::QueueFull(QueueLimit::FileCommands))
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
        self.admission.waiting.fetch_sub(1, Ordering::AcqRel);
        if self.reserve {
            self.admission.reserve_taken.store(false, Ordering::Release);
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
        let closing = self.control.shutdown.swap(true, Ordering::AcqRel);
        let Some(thread) = lock(&self.thread).take() else {
            return;
        };
        // The owner thread drops the last handle itself when the caller gave
        // up on a command that carried it; it cannot wait for itself.
        if !closing && thread.thread().id() != thread::current().id() {
            // A panic on the owner thread was already reported to its commands.
            let _ = thread.join();
        }
    }
}

impl<H: Owner> Worker<H> {
    /// Queue `work` for the owner thread.
    ///
    /// A request to stop (`stop`) runs even if its future is dropped first,
    /// and is admitted beyond a full queue as [`Admission`] describes.
    fn submit<T: Send + 'static>(
        &self,
        stop: bool,
        work: impl FnOnce(&mut H, &OperationContext) -> Result<T> + Send + 'static,
    ) -> FileOperation<T> {
        if self.control.failed() {
            return FileOperation::failed(stopped(true));
        }
        if self.control.shutdown.load(Ordering::Acquire) {
            return FileOperation::failed(stopped(false));
        }
        let place = match self.admission.admit(stop) {
            Ok(place) => place,
            Err(error) => return FileOperation::failed(error),
        };
        let (mut reply, future) = operation();
        reply.owner = Some(Arc::clone(&self.control));
        let context = OperationContext {
            cancelled: Arc::clone(&reply.cancelled),
            control: Arc::clone(&self.control),
        };
        let job: Job<H> = Box::new(move |owner| {
            // The command leaves the queue as it starts.
            drop(place);
            if !stop && context.cancelled.load(Ordering::Acquire) {
                return;
            }
            reply.complete(work(owner, &context));
        });
        if self.sender.send(job).is_err() {
            future.immediate_failure(stopped(self.control.failed()));
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
    while !control.shutdown.load(Ordering::Acquire) {
        // Limit each drain burst so a busy source cannot starve commands.
        for _ in 0..16 {
            match owner.poll_native() {
                Ok(Some(event)) => events.push(Ok(event)),
                Ok(None) => break,
                Err(error) => {
                    events.push(Err(error));
                    break;
                }
            }
        }
        match receiver.recv_timeout(TICK) {
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
        // Resolved here, so that the working directory is the one at call
        // time rather than whenever the probe thread gets to it.
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
    /// Probes run one at a time, in the order they were made, on one thread
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
        PROBES.probe(api, path.as_ref())
    }

    /// Play a file; see [`FilePlayer::start`].
    pub fn start(&self, path: impl AsRef<Path>) -> FileOperation<()> {
        // Resolved here, so that the working directory is the one at call
        // time rather than whenever the owner thread gets to it.
        let path = match resolve_path(path.as_ref(), "file path") {
            Ok((path, _)) => path,
            Err(error) => return FileOperation::failed(error),
        };
        self.worker.call(move |p| p.start(path))
    }
    /// Stop the current file; the source stays advertised. The stop runs even
    /// if the returned future is dropped.
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
    /// blocks the calling thread, and the close runs even if the future is
    /// dropped. Closing again, from this or another clone, while or after it
    /// runs completes at the same time, with the same outcome; later
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
    /// Start a session; see [`FileRecorder::start`].
    pub fn start(
        &self,
        target: &RecordingTarget,
        timecode: RecordingTimecode,
    ) -> FileOperation<u64> {
        let target = target.clone();
        self.worker.call(move |r| r.start(&target, timecode))
    }
    /// Ask the SDK to stop the session. The request runs even if the returned
    /// future is dropped.
    pub fn request_stop(&self) -> FileOperation<StopRequest> {
        self.worker.submit(true, |r, _| Ok(r.request_stop()))
    }
    /// Stop the session and wait up to `timeout` for its last file to be
    /// closed; see [`FileRecorder::finish`].
    ///
    /// The wait is measured from this call, so it includes time the command
    /// spends queued. Dropping the future ends the wait but not the stop: the
    /// recorder keeps stopping and its events keep being pumped, and
    /// [`wait_finished`](Self::wait_finished) can pick the wait up again.
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
    /// [`finish`](Self::finish).
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
        // The budget starts with the call, so it includes the time the
        // command spends queued and, for a finish, the stop.
        let deadline = match Deadline::after(timeout) {
            Ok(deadline) => deadline,
            Err(error) => return FileOperation::failed(error),
        };
        let events = Arc::clone(&self.worker.events);
        self.worker.submit(stop, move |recorder, context| {
            if stop {
                recorder.request_stop();
            }
            recorder.wait_finished_from(deadline, |polled| {
                // As the pump delivers them: a malformed event as its error.
                if let Some(event) = polled.transpose() {
                    events.push(event);
                }
                context.interrupted()
            })
        })
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
    pub fn inject_metadata(&self, metadata: &MetadataFrame) -> FileOperation<()> {
        let metadata = metadata.clone();
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
    /// [`RecorderOptions::drop_wait`] to finalize its file. The future
    /// completes when the SDK has destroyed the recorder, and does not say
    /// whether the file was finalized: await [`finish`](Self::finish) first to
    /// learn that. Nothing blocks the calling thread, and the close runs even
    /// if the future is dropped. Closing again, from this or another clone,
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
    fn block_on<F: Future>(future: F) -> F::Output {
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
        let blocker = worker.call(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(worker.call(|owner| {
            owner.counter += 1;
            Ok(())
        }));
        drop(worker.submit(true, |owner, _| {
            owner.counter += 10;
            Ok(())
        }));
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
        let started = worker.submit(false, move |owner, context| {
            owner.counter += 1;
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            assert!(context.interrupted());
            Ok(())
        });
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
        let blocker = worker.call(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let queued = worker.call(|owner| {
            owner.counter += 1;
            Ok(owner.counter)
        });
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
        let blocker = worker.call(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            panic!("mock owner failure");
            #[allow(unreachable_code)]
            Ok(())
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // Queued behind the command that fails.
        let queued: FileOperation<()> = worker.call(|_| Ok(()));
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
        );
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
        let device = mock::Recorder::new(closing);
        let installed = Arc::clone(&device);
        let recorder = block_on(spawn_owner(
            "file-test-recorder",
            move || {
                installed.install();
                mock::recorder_api()
                    .create_recorder(&RecorderOptions::new(SourceSpec::new("mock source")?))
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
    fn cancelled_finish_retains_recorder_and_notification_loss_preserves_closure_evidence() {
        let (device, recorder) = mock_recorder(Closing::After(Duration::from_millis(150)));
        let finishing = recorder.finish(Duration::from_secs(5));
        while device.stops.load(Ordering::Acquire) == 0 {
            thread::sleep(Duration::from_millis(1));
        }
        drop(finishing);
        assert_eq!(block_on(recorder.state()).unwrap(), RecorderState::Stopping);
        assert_eq!(device.destroys.load(Ordering::Acquire), 0);
        let outcome = block_on(recorder.wait_finished(Duration::from_secs(5))).unwrap();
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
        let blocker = recorder.worker.call(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // The one queue slot is taken, so an ordinary command is refused.
        let queued = recorder.is_recording();
        assert!(matches!(
            block_on(recorder.is_recording()),
            Err(Error::QueueFull(QueueLimit::FileCommands))
        ));
        // A stop is not: it must not be lost, even if nobody waits for it.
        drop(recorder.request_stop());
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
        let blocker = worker.call(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let add = |amount| {
            move |owner: &mut LocalOwner, _: &OperationContext| {
                owner.counter += amount;
                Ok::<(), Error>(())
            }
        };
        // Requests to stop take places like any command, and run even when
        // nobody waits for them.
        drop(worker.submit(true, add(1)));
        drop(worker.submit(true, add(10)));
        // The queue is full: one more stop is admitted beyond it.
        let beyond = worker.submit(true, add(100));
        // That is the most it holds.
        for refused in [
            worker.submit(true, add(1000)),
            worker.submit(false, add(1000)),
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
        assert_eq!(worker.admission.waiting.load(Ordering::Acquire), 0);
        assert!(!worker.admission.reserve_taken.load(Ordering::Acquire));
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn closes_made_while_one_is_awaited_complete_with_it() {
        let (device, recorder) = mock_recorder(Closing::After(Duration::from_millis(150)));
        let clone = recorder.clone();
        let first = recorder.close();
        let second = clone.close();
        // A close whose future is dropped neither blocks nor cancels the others.
        drop(recorder.close());
        block_on(second).unwrap();
        block_on(first).unwrap();
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        // The handles stay usable for what a closed handle can do.
        block_on(recorder.close()).unwrap();
        closed(block_on(clone.state()));
        // No clone blocks when dropped once the object is closed.
        let begun = Instant::now();
        drop((recorder, clone));
        assert!(begun.elapsed() < Duration::from_millis(100));
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
        let (device, recorder) = mock_recorder(Closing::After(Duration::from_millis(300)));
        let begun = Instant::now();
        let closed = recorder.close();
        assert!(
            begun.elapsed() < Duration::from_millis(150),
            "close returned only after {:?}",
            begun.elapsed()
        );
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
    fn an_async_finish_measures_its_wait_from_the_call() {
        // The command waits behind another for longer than its whole wait,
        // so once it runs only the events already queued are taken, and the
        // close comes after them.
        let (device, recorder) = mock_recorder(Closing::After(Duration::from_millis(50)));
        let (entered_tx, entered_rx) = mpsc::channel();
        let blocker = recorder.worker.call(move |_| {
            entered_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(400));
            Ok(())
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let finishing = recorder.finish(Duration::from_millis(200));
        block_on(blocker).unwrap();
        let outcome = block_on(finishing).unwrap();
        assert!(matches!(outcome, FinishStatus::TimedOut(_)), "{outcome:?}");
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn an_async_timeout_out_of_range_fails_at_once_and_stops_nothing() {
        let (device, recorder) = mock_recorder(Closing::AtStop);
        let too_long = crate::MAX_TIMEOUT + Duration::from_nanos(1);
        for finishing in [recorder.finish(too_long), recorder.wait_finished(too_long)] {
            // Completed before it was returned: nothing was queued.
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
        let finishing = recorder.finish(Duration::from_secs(5));
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
        let blocker = worker.call(move |_| {
            entered_tx.send(()).unwrap();
            // Far longer than the wait; bounded so a failure cannot hang.
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
            Ok(())
        });
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
        let (device, recorder) = mock_recorder(Closing::After(Duration::from_secs(3)));
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
        let closed = recorder.close();
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
