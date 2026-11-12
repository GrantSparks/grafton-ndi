//! Futures for the File API that work with any executor.
//!
//! Each player or recorder lives on a thread of its own, which runs the
//! commands sent to it and pumps the SDK's events between them.

use super::{
    timeout_ms, EventText, FileApi, FileDuration, FileError, FileInfo, FilePlayer, FilePosition,
    FileRecorder, FileResult, FinishStatus, PlayerEvent, PlayerOptions, PlayerStats, RecorderEvent,
    RecorderOptions, RecorderState, RecorderStats, RecordingReport, RecordingTarget,
    RecordingTimecode, SourceFormat, SourceSpec, SourceTimecode, StopRequest,
};
use crate::Source;
use std::{
    collections::VecDeque,
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc, Mutex, MutexGuard, Weak,
    },
    task::{Context, Poll, Waker},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// How long the owner thread waits for a command between event pumps.
const TICK: Duration = Duration::from_millis(10);

/// The largest command or event queue an owner may be given.
const MAX_CAPACITY: usize = 4096;

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The sizes of the queues between an async handle and its owner thread.
///
/// The default holds 64 pending commands and 256 undelivered events.
#[derive(Clone, Debug)]
pub struct AsyncFileOptions {
    command_capacity: usize,
    event_capacity: usize,
}

impl Default for AsyncFileOptions {
    fn default() -> Self {
        Self {
            command_capacity: 64,
            event_capacity: 256,
        }
    }
}

impl AsyncFileOptions {
    /// Set how many commands may wait for the owner thread, from 1 through
    /// 4096. A command submitted to a full queue fails at once, except a
    /// request to stop, which is always accepted so that it cannot be lost.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when `capacity` is out of range.
    pub fn with_command_capacity(mut self, capacity: usize) -> FileResult<Self> {
        validate_capacity(capacity)?;
        self.command_capacity = capacity;
        Ok(self)
    }

    /// Set how many events may wait to be received, from 1 through 4096. When
    /// the queue is full the oldest event is discarded and counted.
    ///
    /// A recorder's session report is updated before an event enters this
    /// queue, so an event discarded here never affects what the report says.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when `capacity` is out of range.
    pub fn with_event_capacity(mut self, capacity: usize) -> FileResult<Self> {
        validate_capacity(capacity)?;
        self.event_capacity = capacity;
        Ok(self)
    }
}

fn validate_capacity(capacity: usize) -> FileResult<()> {
    if (1..=MAX_CAPACITY).contains(&capacity) {
        Ok(())
    } else {
        Err(FileError::InvalidInput(format!(
            "async queue capacity must be between 1 and {MAX_CAPACITY}"
        )))
    }
}

struct Response<T> {
    value: Option<FileResult<T>>,
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
    type Output = FileResult<T>;

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
        },
        FileOperation {
            response,
            cancelled,
            consumed: false,
        },
    )
}

fn deliver<T>(response: &Mutex<Response<T>>, value: FileResult<T>, replace: bool) {
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

impl<T> FileOperation<T> {
    fn failed(error: FileError) -> Self {
        let (reply, future) = operation();
        reply.complete(Err(error));
        future
    }

    // Only used before the future is returned to the caller. A rejected job's
    // Reply destructor may already have supplied a generic stopped error.
    fn immediate_failure(&self, error: FileError) {
        deliver(&self.response, Err(error), true);
    }
}

impl<T> Reply<T> {
    fn complete(mut self, value: FileResult<T>) {
        self.delivered = true;
        if !self.cancelled.load(Ordering::Acquire) {
            deliver(&self.response, value, false);
        }
    }
}

impl<T> Drop for Reply<T> {
    fn drop(&mut self) {
        if !self.delivered && !self.cancelled.load(Ordering::Acquire) {
            deliver(
                &self.response,
                Err(FileError::InvalidState(
                    "File owner stopped before returning this operation",
                )),
                false,
            );
        }
    }
}

struct EventWaiter {
    token: Weak<()>,
    deadline: Instant,
    waker: Waker,
}

struct Events<E> {
    queue: VecDeque<FileResult<E>>,
    capacity: usize,
    dropped: u64,
    closed: bool,
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
            waiter: None,
        }))
    }

    fn push(&self, value: FileResult<E>) {
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

    fn tick(&self) {
        let wake = {
            let events = lock(&self.0);
            events
                .waiter
                .as_ref()
                .filter(|w| w.deadline <= Instant::now())
                .map(|w| w.waker.clone())
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    fn close(&self) {
        let wake = {
            let mut events = lock(&self.0);
            events.closed = true;
            events.waiter.take().map(|w| w.waker)
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    fn wait(self: &Arc<Self>, budget: Duration) -> FileEventFuture<E> {
        FileEventFuture {
            events: Arc::clone(self),
            token: Arc::new(()),
            budget,
            deadline: None,
            invalid: timeout_ms(budget).err(),
            consumed: false,
        }
    }
}

/// The next event from a player or recorder, or `None` when none arrives
/// within the wait.
///
/// The wait starts when the future is first polled. One future may wait on a
/// handle at a time; a second fails with [`FileError::InvalidState`]. Dropping
/// the future does not stop events from being pumped.
///
/// # Panics
///
/// Polling again after completion panics.
#[must_use = "event futures must be polled to receive a notification"]
pub struct FileEventFuture<E> {
    events: Arc<EventQueue<E>>,
    token: Arc<()>,
    budget: Duration,
    deadline: Option<Instant>,
    invalid: Option<FileError>,
    consumed: bool,
}

impl<E> Future for FileEventFuture<E> {
    type Output = FileResult<Option<E>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        assert!(
            !this.consumed,
            "a completed FileEventFuture was polled again"
        );
        if let Some(error) = this.invalid.take() {
            this.consumed = true;
            return Poll::Ready(Err(error));
        }
        let deadline = *this
            .deadline
            .get_or_insert_with(|| Instant::now() + this.budget);
        let token = Arc::downgrade(&this.token);
        let mut events = lock(&this.events.0);
        if events
            .waiter
            .as_ref()
            .is_some_and(|w| w.token.strong_count() != 0 && !w.token.ptr_eq(&token))
        {
            this.consumed = true;
            return Poll::Ready(Err(FileError::InvalidState(
                "another File event waiter is already pending",
            )));
        }
        let ready = if let Some(event) = events.queue.pop_front() {
            Some(event.map(Some))
        } else if events.closed {
            Some(Err(FileError::InvalidState("File owner has closed")))
        } else if deadline <= Instant::now() {
            Some(Ok(None))
        } else {
            None
        };
        if let Some(value) = ready {
            events.waiter = None;
            this.consumed = true;
            Poll::Ready(value)
        } else {
            events.waiter = Some(EventWaiter {
                token,
                deadline,
                waker: cx.waker().clone(),
            });
            Poll::Pending
        }
    }
}

impl<E> Drop for FileEventFuture<E> {
    fn drop(&mut self) {
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

struct Closing {
    complete: bool,
    failed: bool,
    reply: Option<Reply<()>>,
}

struct Control {
    shutdown: AtomicBool,
    closing: Mutex<Closing>,
}

impl Control {
    fn new() -> Self {
        Self {
            shutdown: AtomicBool::new(false),
            closing: Mutex::new(Closing {
                complete: false,
                failed: false,
                reply: None,
            }),
        }
    }

    fn request_close(&self) -> FileOperation<()> {
        let (reply, future) = operation();
        {
            let mut closing = lock(&self.closing);
            if closing.complete {
                reply.complete(if closing.failed {
                    Err(FileError::InvalidState("File owner thread failed"))
                } else {
                    Ok(())
                });
                return future;
            }
            if closing
                .reply
                .as_ref()
                .is_some_and(|r| !r.cancelled.load(Ordering::Acquire))
            {
                reply.complete(Err(FileError::InvalidState(
                    "another close waiter is already pending",
                )));
                return future;
            }
            closing.reply = Some(reply);
        }
        self.shutdown.store(true, Ordering::Release);
        future
    }

    fn completed(&self, failed: bool) {
        let reply = {
            let mut closing = lock(&self.closing);
            closing.complete = true;
            closing.failed = failed;
            closing.reply.take()
        };
        if let Some(reply) = reply {
            reply.complete(if failed {
                Err(FileError::InvalidState("File owner thread failed"))
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
    fn poll_native(&mut self) -> FileResult<Option<Self::Event>>;
}

impl Owner for FilePlayer {
    type Event = PlayerEvent;
    fn poll_native(&mut self) -> FileResult<Option<Self::Event>> {
        self.poll_event(Duration::ZERO)
    }
}

impl Owner for FileRecorder {
    type Event = RecorderEvent;
    fn poll_native(&mut self) -> FileResult<Option<Self::Event>> {
        self.poll_event(Duration::ZERO)
    }
}

type Job<H> = Box<dyn FnOnce(&mut H) + Send + 'static>;

/// The handle side of an owner thread, shared by every clone of an async handle.
struct Worker<H: Owner> {
    sender: Sender<Job<H>>,
    /// How many ordinary commands are waiting, and how many may. Requests to
    /// stop are not counted: one must never be refused for want of room.
    waiting: Arc<AtomicUsize>,
    capacity: usize,
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
    fn submit<T: Send + 'static>(
        &self,
        persistent: bool,
        work: impl FnOnce(&mut H, &OperationContext) -> FileResult<T> + Send + 'static,
    ) -> FileOperation<T> {
        if self.control.shutdown.load(Ordering::Acquire) {
            return FileOperation::failed(FileError::InvalidState("File owner is closing"));
        }
        let waiting = (!persistent).then(|| Arc::clone(&self.waiting));
        if let Some(waiting) = &waiting {
            if waiting.fetch_add(1, Ordering::AcqRel) >= self.capacity {
                waiting.fetch_sub(1, Ordering::AcqRel);
                return FileOperation::failed(FileError::InvalidState(
                    "File command queue is full",
                ));
            }
        }
        let (reply, future) = operation();
        let context = OperationContext {
            cancelled: Arc::clone(&reply.cancelled),
            control: Arc::clone(&self.control),
        };
        let job: Job<H> = Box::new(move |owner| {
            if let Some(waiting) = waiting {
                waiting.fetch_sub(1, Ordering::AcqRel);
            }
            if !persistent && context.cancelled.load(Ordering::Acquire) {
                return;
            }
            reply.complete(work(owner, &context));
        });
        if self.sender.send(job).is_err() {
            future.immediate_failure(FileError::InvalidState("File owner has stopped"));
        }
        future
    }

    fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut H) -> FileResult<T> + Send + 'static,
    ) -> FileOperation<T> {
        self.submit(false, move |owner, _| work(owner))
    }
}

/// Start an owner thread, create its native object there, and return the
/// handle `wrap` builds around the worker.
fn spawn_owner<H: Owner, T: Send + 'static>(
    name: &str,
    create: impl FnOnce() -> FileResult<H> + Send + 'static,
    options: AsyncFileOptions,
    wrap: impl FnOnce(Arc<Worker<H>>) -> T + Send + 'static,
) -> FileOperation<T> {
    let (sender, receiver) = mpsc::channel();
    let control = Arc::new(Control::new());
    let events = Arc::new(EventQueue::new(options.event_capacity));
    let worker = Arc::new(Worker {
        sender,
        waiting: Arc::new(AtomicUsize::new(0)),
        capacity: options.command_capacity,
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
            run_owner(&mut owner, receiver, &thread_control, &thread_events);
            // Destroyed here, before the close is reported complete. `H` need
            // not be `Send`: it is created, used and dropped on this thread.
            drop(owner);
        }));
        thread_events.close();
        thread_control.completed(outcome.is_err());
    });
    match spawned {
        Ok(thread) => *lock(&worker.thread) = Some(thread),
        Err(error) => {
            future.immediate_failure(FileError::Io(error));
            events.close();
            control.completed(true);
        }
    }
    future
}

fn run_owner<H: Owner>(
    owner: &mut H,
    receiver: Receiver<Job<H>>,
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
        events.tick();
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

/// A [`FilePlayer`] on a thread of its own, driven through futures.
///
/// Clones share one player. Dropping the last clone stops playback, removes
/// the advertised source and waits for the player's thread to finish, which
/// blocks the dropping thread for as long as the SDK takes to destroy the
/// player. To avoid blocking an executor, await [`close`](Self::close) first.
#[derive(Clone)]
pub struct AsyncFilePlayer {
    worker: Arc<Worker<FilePlayer>>,
}

impl AsyncFilePlayer {
    /// Create a player and advertise its source, with the default queue sizes.
    pub fn create(api: FileApi, options: PlayerOptions) -> FileOperation<Self> {
        Self::create_with_options(api, options, AsyncFileOptions::default())
    }

    /// Create a player and advertise its source, with the given queue sizes.
    pub fn create_with_options(
        api: FileApi,
        options: PlayerOptions,
        asynchronous: AsyncFileOptions,
    ) -> FileOperation<Self> {
        spawn_owner(
            "ndi-file-player",
            move || api.create_player(options),
            asynchronous,
            |worker| Self { worker },
        )
    }

    /// Read a recording's duration and track layout on a temporary thread.
    /// No player is created and nothing is advertised.
    pub fn probe(api: FileApi, path: impl Into<PathBuf>) -> FileOperation<FileInfo> {
        let path = match RecordingTarget::new(path.into()) {
            Ok(target) => target.path().to_owned(),
            Err(error) => return FileOperation::failed(error),
        };
        let (reply, future) = operation();
        if let Err(error) = thread::Builder::new()
            .name("ndi-file-probe".into())
            .spawn(move || {
                if !reply.cancelled.load(Ordering::Acquire) {
                    reply.complete(api.probe(path));
                }
            })
        {
            future.immediate_failure(FileError::Io(error));
        }
        future
    }

    /// Play a file; see [`FilePlayer::start`].
    pub fn start(&self, path: impl Into<PathBuf>) -> FileOperation<()> {
        let path = match RecordingTarget::new(path.into()) {
            Ok(target) => target.path().to_owned(),
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
    /// Wait up to `wait` for the player's next event.
    pub fn next_event(&self, wait: Duration) -> FileEventFuture<PlayerEvent> {
        self.worker.events.wait(wait)
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
    pub fn source_timecode(&self) -> FileOperation<Option<SourceTimecode>> {
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
    /// dropped.
    pub fn close(self) -> FileOperation<()> {
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
/// Dropping the last clone instead blocks the dropping thread until the
/// recorder's thread has finished: it stops a session that is still open,
/// waits up to [`RecorderOptions::with_drop_wait`] for the SDK to finalize the
/// file, and destroys the recorder. The file is therefore finalized, as far as
/// the SDK reports within that wait, by the time the drop returns, but the
/// outcome cannot be reported.
#[derive(Clone)]
pub struct AsyncFileRecorder {
    worker: Arc<Worker<FileRecorder>>,
}

impl AsyncFileRecorder {
    /// Create a recorder with the default queue sizes.
    pub fn create(api: FileApi, options: RecorderOptions) -> FileOperation<Self> {
        Self::create_with_options(api, options, AsyncFileOptions::default())
    }
    /// Create a recorder with the given queue sizes.
    pub fn create_with_options(
        api: FileApi,
        options: RecorderOptions,
        asynchronous: AsyncFileOptions,
    ) -> FileOperation<Self> {
        spawn_owner(
            "ndi-file-recorder",
            move || api.create_recorder(options),
            asynchronous,
            |worker| Self { worker },
        )
    }
    /// Start a session; see [`FileRecorder::start`].
    pub fn start(
        &self,
        target: RecordingTarget,
        timecode: RecordingTimecode,
    ) -> FileOperation<u64> {
        self.worker.call(move |r| r.start(target, timecode))
    }
    /// Ask the SDK to stop the session. The request runs even if the returned
    /// future is dropped.
    pub fn request_stop(&self) -> FileOperation<StopRequest> {
        self.worker.submit(true, |r, _| Ok(r.request_stop()))
    }
    /// Stop the session and wait up to `wait` for its last file to be closed.
    ///
    /// The wait is measured from this call, so it includes time the command
    /// spends queued. Dropping the future ends the wait but not the stop: the
    /// recorder keeps stopping and its events keep being pumped, and
    /// [`wait_finished`](Self::wait_finished) can pick the wait up again.
    pub fn finish(&self, wait: Duration) -> FileOperation<FinishStatus> {
        self.finish_inner(wait, true)
    }
    /// Wait up to `wait` for a stopping session to end; see
    /// [`FileRecorder::wait_finished`].
    pub fn wait_finished(&self, wait: Duration) -> FileOperation<FinishStatus> {
        self.finish_inner(wait, false)
    }

    /// Replace the native recorder after abandoning its old session; see
    /// [`FileRecorder::reset`]. Queued events already delivered to this
    /// worker's event stream remain available to the caller.
    pub fn reset(&self, options: RecorderOptions) -> FileOperation<RecordingReport> {
        self.worker.call(move |recorder| recorder.reset(options))
    }

    fn finish_inner(&self, wait: Duration, stop: bool) -> FileOperation<FinishStatus> {
        if let Err(error) = timeout_ms(wait) {
            return FileOperation::failed(error);
        }
        let submitted = Instant::now();
        let events = Arc::clone(&self.worker.events);
        self.worker.submit(stop, move |recorder, context| {
            if stop {
                recorder.request_stop();
            }
            recorder.wait_finished_from(submitted, wait, |event| {
                if let Some(event) = event {
                    events.push(Ok(event));
                }
                events.tick();
                context.interrupted()
            })
        })
    }

    /// Wait up to `wait` for the recorder's next event. Dropping the future
    /// does not affect the recording.
    pub fn next_event(&self, wait: Duration) -> FileEventFuture<RecorderEvent> {
        self.worker.events.wait(wait)
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
    pub fn set_source(&self, source: SourceSpec) -> FileOperation<()> {
        self.worker.call(move |r| r.set_source(source))
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
    pub fn rollover(&self, target: RecordingTarget) -> FileOperation<()> {
        self.worker.call(move |r| r.rollover(target))
    }
    /// Write a metadata frame into the recording at its current position.
    pub fn inject_metadata(&self, metadata: crate::MetadataFrame) -> FileOperation<()> {
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
    /// [`RecorderOptions::with_drop_wait`] to finalize its file. The future
    /// completes when the SDK has destroyed the recorder, and does not say
    /// whether the file was finalized: await [`finish`](Self::finish) first to
    /// learn that. Nothing blocks the calling thread, and the close runs even
    /// if the future is dropped.
    pub fn close(self) -> FileOperation<()> {
        self.worker.control.request_close()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::mock::{self, Closing},
        *,
    };
    use std::{rc::Rc, sync::Condvar, task::Wake};

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
        fn poll_native(&mut self) -> FileResult<Option<usize>> {
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
        local_worker_with_options(AsyncFileOptions::default())
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
        let options = AsyncFileOptions::default()
            .with_command_capacity(1)
            .unwrap();
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
            Err(FileError::InvalidState("File command queue is full"))
        ));
        release_tx.send(()).unwrap();
        block_on(blocker).unwrap();
        assert_eq!(block_on(queued).unwrap(), 1);
        assert_eq!(block_on(worker.call(|owner| Ok(owner.counter))).unwrap(), 1);
        drop(worker);
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn worker_panic_replies_with_failure_and_destroys_owner_locally() {
        let (worker, dropped) = local_worker();
        let created = block_on(worker.call(|owner| Ok(owner.created))).unwrap();
        let failed: FileOperation<()> = worker.call(|_| panic!("mock owner failure"));
        assert!(block_on(failed).is_err());
        assert_eq!(
            dropped.recv_timeout(Duration::from_secs(1)).unwrap(),
            created
        );
        assert!(block_on(worker.control.request_close()).is_err());
        assert!(block_on(worker.call(|_| Ok(()))).is_err());
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
            AsyncFileOptions::default(),
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
        mock_recorder_with(
            closing,
            AsyncFileOptions::default().with_event_capacity(1).unwrap(),
        )
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
                    .create_recorder(RecorderOptions::new(SourceSpec::new("mock source")?))
            },
            options,
            |worker| AsyncFileRecorder { worker },
        ))
        .unwrap();
        block_on(recorder.start(
            RecordingTarget::new(std::env::temp_dir().join("mock-file-not-created")).unwrap(),
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
            RecordingTarget::new(std::env::temp_dir().join("mock-file-not-created")).unwrap(),
            RecordingTimecode::Preserve,
        ))
        .unwrap();
        assert_eq!(session, 2);
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        block_on(recorder.close()).unwrap();
    }

    #[test]
    fn a_stop_is_carried_out_even_when_the_command_queue_is_full() {
        let options = AsyncFileOptions::default()
            .with_command_capacity(1)
            .unwrap();
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
            Err(FileError::InvalidState("File command queue is full"))
        ));
        // A stop is not: it must not be lost, even if nobody waits for it.
        drop(recorder.request_stop());
        let stopping = recorder.request_stop();
        release_tx.send(()).unwrap();
        block_on(blocker).unwrap();
        assert!(block_on(queued).unwrap());
        // The first request did the stopping; this one found it done, and
        // possibly the file already reported closed.
        assert!(matches!(
            block_on(stopping).unwrap(),
            StopRequest::AlreadyStopping | StopRequest::AlreadyClosed
        ));
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        block_on(recorder.close()).unwrap();
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
        assert!(block_on(events.wait(Duration::ZERO)).is_err());
        drop(first);
        assert_eq!(block_on(events.wait(Duration::ZERO)).unwrap(), None);
    }

    #[test]
    fn closing_wakes_pending_event_future_and_rejects_new_commands() {
        let (worker, dropped) = local_worker();
        let event = worker.events.wait(Duration::from_secs(3));
        block_on(worker.control.request_close()).unwrap();
        assert!(block_on(event).is_err());
        assert!(block_on(worker.call(|_| Ok(()))).is_err());
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}
