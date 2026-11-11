//! Runtime-independent futures backed by a dedicated native-owner thread.

use super::{
    timeout_ms, EventText, FileApi, FileDuration, FileError, FileInfo, FilePlayer, FilePosition,
    FileRecorder, FileResult, FinishStatus, PlayerEvent, PlayerOptions, PlayerStats, RecorderEvent,
    RecorderOptions, RecorderState, RecorderStats, RecordingReport, RecordingTarget,
    RecordingTimecode, SourceFormat, SourceSpec, SourceTimecode, StopRequest,
};
use std::{
    collections::VecDeque,
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
        Arc, Mutex, MutexGuard, Weak,
    },
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};

const TICK: Duration = Duration::from_millis(10);
const WAIT_SLICE: Duration = Duration::from_millis(25);
const MAX_CAPACITY: usize = 4096;

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Bounded queues and the best-effort shutdown wait for a File owner thread.
#[derive(Clone, Debug)]
pub struct AsyncFileOptions {
    command_capacity: usize,
    event_capacity: usize,
    shutdown_wait: Duration,
}

impl Default for AsyncFileOptions {
    fn default() -> Self {
        Self {
            command_capacity: 64,
            event_capacity: 256,
            shutdown_wait: Duration::from_secs(1),
        }
    }
}

impl AsyncFileOptions {
    /// Set pending-command capacity, from 1 through 4096. Full queues reject work.
    pub fn with_command_capacity(mut self, capacity: usize) -> FileResult<Self> {
        validate_capacity(capacity)?;
        self.command_capacity = capacity;
        Ok(self)
    }

    /// Set user-event capacity, from 1 through 4096. Overflow drops the oldest notice.
    ///
    /// The recorder's completion ledger is updated before this separate queue;
    /// user notification loss cannot erase already-observed completion evidence.
    pub fn with_event_capacity(mut self, capacity: usize) -> FileResult<Self> {
        validate_capacity(capacity)?;
        self.event_capacity = capacity;
        Ok(self)
    }

    /// Set the finite event-drain budget used when the final handle is dropped/closed.
    ///
    /// This is not a deadline for native destruction. Explicit `finish` is needed
    /// to obtain a recording report before releasing the owner.
    pub fn with_shutdown_wait(mut self, wait: Duration) -> FileResult<Self> {
        timeout_ms(wait)?;
        self.shutdown_wait = wait;
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

/// A queued File operation. It works with any executor that polls Rust futures.
///
/// Methods enqueue immediately, even before their returned future is polled.
/// Dropping a normal operation before its execution gate prevents its native
/// call. Once native work starts, dropping the future only abandons its result.
/// Stop/finish/close requests have stronger persistence documented on those methods.
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

/// Wait for an owned notification without blocking the owner thread or executor.
///
/// Its wait budget starts on first poll. Only one pending event waiter is allowed
/// per owner; concurrent waiters receive an explicit state error. Cancelling this
/// future never stops the internal native event pump.
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

trait Owner: 'static {
    type Event: Send + 'static;
    fn poll_native(&mut self) -> FileResult<Option<Self::Event>>;
    fn cleanup(&mut self, budget: Duration);
}

impl Owner for FilePlayer {
    type Event = PlayerEvent;
    fn poll_native(&mut self) -> FileResult<Option<Self::Event>> {
        self.poll_event(Duration::ZERO)
    }
    fn cleanup(&mut self, _: Duration) {
        self.stop();
    }
}

impl Owner for FileRecorder {
    type Event = RecorderEvent;
    fn poll_native(&mut self) -> FileResult<Option<Self::Event>> {
        self.poll_event(Duration::ZERO)
    }
    fn cleanup(&mut self, budget: Duration) {
        if self.state() != RecorderState::Idle {
            let _ = self.finish(budget);
        }
    }
}

type Job<H> = Box<dyn FnOnce(&mut H) + Send + 'static>;

struct Worker<H: Owner> {
    sender: SyncSender<Job<H>>,
    control: Arc<Control>,
    events: Arc<EventQueue<H::Event>>,
}

impl<H: Owner> Drop for Worker<H> {
    fn drop(&mut self) {
        self.control.shutdown.store(true, Ordering::Release);
        // No join on the dropping thread. The owner thread notices shutdown or
        // channel disconnection and retains/destroys its native object itself.
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
        let (reply, future) = operation();
        let context = OperationContext {
            cancelled: Arc::clone(&reply.cancelled),
            control: Arc::clone(&self.control),
        };
        let job: Job<H> = Box::new(move |owner| {
            if !persistent && context.cancelled.load(Ordering::Acquire) {
                return;
            }
            reply.complete(work(owner, &context));
        });
        match self.sender.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(job)) => {
                future.immediate_failure(FileError::InvalidState("File command queue is full"));
                drop(job);
            }
            Err(TrySendError::Disconnected(job)) => {
                future.immediate_failure(FileError::InvalidState("File owner has stopped"));
                drop(job);
            }
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

fn spawn_owner<H: Owner, T: Send + 'static>(
    name: &str,
    create: impl FnOnce() -> FileResult<H> + Send + 'static,
    options: AsyncFileOptions,
    wrap: impl FnOnce(Arc<Worker<H>>) -> T + Send + 'static,
) -> FileOperation<T> {
    let (sender, receiver) = mpsc::sync_channel(options.command_capacity);
    let control = Arc::new(Control::new());
    let events = Arc::new(EventQueue::new(options.event_capacity));
    let worker = Arc::new(Worker {
        sender,
        control: Arc::clone(&control),
        events: Arc::clone(&events),
    });
    let (ready, future) = operation();
    let thread_control = Arc::clone(&control);
    let thread_events = Arc::clone(&events);
    let result = thread::Builder::new().name(name.into()).spawn(move || {
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
            ready.complete(Ok(wrap(worker)));
            run_owner(&mut owner, receiver, &thread_control, &thread_events);
            owner.cleanup(options.shutdown_wait);
            // Explicitly destroy here before reporting close completion. H need
            // not be Send: it was created, used and dropped on this one thread.
            drop(owner);
        }));
        thread_events.close();
        thread_control.completed(outcome.is_err());
    });
    if let Err(error) = result {
        future.immediate_failure(FileError::Io(error));
        events.close();
        control.completed(true);
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

/// A cloneable command handle; its native player remains on one dedicated thread.
///
/// Dropping the last handle requests cleanup without joining that thread. Await
/// [`close`](Self::close) to know that native destruction has completed.
#[derive(Clone)]
pub struct AsyncFilePlayer {
    worker: Arc<Worker<FilePlayer>>,
}

impl AsyncFilePlayer {
    /// Create/advertise on the owner thread, with bounded default queues.
    pub fn create(api: FileApi, options: PlayerOptions) -> FileOperation<Self> {
        Self::create_with_options(api, options, AsyncFileOptions::default())
    }

    /// Create/advertise on the owner thread with explicit queue/shutdown settings.
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

    /// Probe on a temporary worker without creating an advertised player.
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

    /// Start a file. Native work already past its execution gate cannot be cancelled.
    pub fn start(&self, path: impl Into<PathBuf>) -> FileOperation<()> {
        let path = match RecordingTarget::new(path.into()) {
            Ok(target) => target.path().to_owned(),
            Err(error) => return FileOperation::failed(error),
        };
        self.worker.call(move |p| p.start(path))
    }
    /// Accepted stop requests execute even when their response future is dropped.
    pub fn stop(&self) -> FileOperation<()> {
        self.worker.submit(true, |p, _| {
            p.stop();
            Ok(())
        })
    }
    /// Query active playback.
    pub fn is_running(&self) -> FileOperation<bool> {
        self.worker.call(|p| Ok(p.is_running()))
    }
    /// Receive an owned user notification while the native event pump continues.
    pub fn next_event(&self, wait: Duration) -> FileEventFuture<PlayerEvent> {
        self.worker.events.wait(wait)
    }
    /// Native queue losses, distinct from user notification losses.
    pub fn dropped_event_count(&self) -> FileOperation<u64> {
        self.worker.call(|p| Ok(p.dropped_event_count()))
    }
    /// User notification losses caused by the bounded Rust event queue.
    pub fn notification_dropped_count(&self) -> u64 {
        lock(&self.worker.events.0).dropped
    }
    /// Owned playback telemetry.
    pub fn stats(&self) -> FileOperation<Option<PlayerStats>> {
        self.worker.call(|p| Ok(p.stats()))
    }
    /// Codec-dependent seek; GOP streams snap to a preceding keyframe.
    pub fn seek(&self, position: FilePosition) -> FileOperation<()> {
        self.worker.call(move |p| p.seek(position))
    }
    /// Approximate frame-index seek through the SDK's frame-rate conversion.
    pub fn seek_frame_approximate(&self, frame: u64) -> FileOperation<()> {
        self.worker.call(move |p| p.seek_frame_approximate(frame))
    }
    /// Set current-file pause state.
    pub fn set_paused(&self, paused: bool) -> FileOperation<()> {
        self.worker.call(move |p| {
            p.set_paused(paused);
            Ok(())
        })
    }
    /// Query current-file pause state.
    pub fn is_paused(&self) -> FileOperation<bool> {
        self.worker.call(|p| Ok(p.is_paused()))
    }
    /// Set instance looping, which survives file changes.
    pub fn set_looping(&self, looping: bool) -> FileOperation<()> {
        self.worker.call(move |p| {
            p.set_looping(looping);
            Ok(())
        })
    }
    /// Query persistent loop state.
    pub fn is_looping(&self) -> FileOperation<bool> {
        self.worker.call(|p| Ok(p.is_looping()))
    }
    /// Current duration and final/provisional flag.
    pub fn duration(&self) -> FileOperation<Option<FileDuration>> {
        self.worker.call(|p| Ok(p.duration()))
    }
    /// Position of the primary packet last sent.
    pub fn position(&self) -> FileOperation<Option<FilePosition>> {
        self.worker.call(|p| Ok(p.position()))
    }
    /// Preserved editorial timecode.
    pub fn source_timecode(&self) -> FileOperation<Option<SourceTimecode>> {
        self.worker.call(|p| Ok(p.source_timecode()))
    }
    /// Owned track format.
    pub fn source_format(&self) -> FileOperation<Option<SourceFormat>> {
        self.worker.call(|p| p.source_format())
    }
    /// Copy the source advertised by the owner.
    pub fn advertised_source(&self) -> FileOperation<SourceSpec> {
        self.worker.call(|p| p.advertised_source())
    }
    /// Shut down this owner for all clones and await native destruction.
    ///
    /// The request persists if the returned future is dropped. No executor thread
    /// is joined or blocked; native destruction may still take unbounded time.
    pub fn close(self) -> FileOperation<()> {
        self.worker.control.request_close()
    }
}

/// A cloneable command handle with a continuously pumped recorder event ledger.
///
/// Dropping the last handle requests bounded event draining and native cleanup
/// on its owner thread. Await [`close`](Self::close) to observe cleanup, and use
/// [`finish`](Self::finish) first when a confirmed recording report is required.
#[derive(Clone)]
pub struct AsyncFileRecorder {
    worker: Arc<Worker<FileRecorder>>,
}

impl AsyncFileRecorder {
    /// Construct the native recorder on a dedicated owner thread.
    pub fn create(api: FileApi, options: RecorderOptions) -> FileOperation<Self> {
        Self::create_with_options(api, options, AsyncFileOptions::default())
    }
    /// Construct with explicit bounded queue/shutdown settings.
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
    /// Start an explicit session, retaining target/timecode on its owner thread.
    pub fn start(
        &self,
        target: RecordingTarget,
        timecode: RecordingTimecode,
    ) -> FileOperation<u64> {
        self.worker.call(move |r| r.start(target, timecode))
    }
    /// Accepted stop persists even if its response future is cancelled.
    pub fn request_stop(&self) -> FileOperation<StopRequest> {
        self.worker.submit(true, |r, _| Ok(r.request_stop()))
    }
    /// Request stop and wait for closure within an event-wait budget.
    ///
    /// Cancellation retains the recorder, leaves stop requested and resumes idle
    /// event pumping. The wait budget includes command queue delay. A native call
    /// already in progress is not interrupted and can exceed this budget.
    pub fn finish(&self, wait: Duration) -> FileOperation<FinishStatus> {
        self.finish_inner(wait, true)
    }
    /// Resume a prior finish wait without consuming or destroying the recorder.
    pub fn wait_finished(&self, wait: Duration) -> FileOperation<FinishStatus> {
        self.finish_inner(wait, false)
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
            if !matches!(
                recorder.state(),
                RecorderState::Stopping | RecorderState::Closed | RecorderState::Indeterminate
            ) {
                return Err(FileError::InvalidState(
                    "request stop before waiting for finalization",
                ));
            }
            loop {
                match recorder.state() {
                    RecorderState::Closed => return Ok(FinishStatus::Closed(recorder.report())),
                    RecorderState::Indeterminate => {
                        return Ok(FinishStatus::Indeterminate(recorder.report()))
                    }
                    _ => {}
                }
                let remaining = wait.saturating_sub(submitted.elapsed());
                let slice = if context.interrupted() {
                    Duration::ZERO
                } else {
                    remaining.min(WAIT_SLICE)
                };
                if let Some(event) = recorder.poll_event(slice)? {
                    events.push(Ok(event));
                }
                events.tick();
                match recorder.state() {
                    RecorderState::Closed => return Ok(FinishStatus::Closed(recorder.report())),
                    RecorderState::Indeterminate => {
                        return Ok(FinishStatus::Indeterminate(recorder.report()))
                    }
                    _ => {}
                }
                if context.interrupted() || submitted.elapsed() >= wait {
                    return Ok(FinishStatus::TimedOut(recorder.report()));
                }
            }
        })
    }

    /// Receive user events; cancelling a waiter does not cancel recording or pumping.
    pub fn next_event(&self, wait: Duration) -> FileEventFuture<RecorderEvent> {
        self.worker.events.wait(wait)
    }
    /// Native event queue losses.
    pub fn dropped_event_count(&self) -> FileOperation<u64> {
        self.worker.call(|r| Ok(r.dropped_event_count()))
    }
    /// Losses from the separate bounded user notification queue.
    pub fn notification_dropped_count(&self) -> u64 {
        lock(&self.worker.events.0).dropped
    }
    /// Owned lifecycle snapshot.
    pub fn state(&self) -> FileOperation<RecorderState> {
        self.worker.call(|r| Ok(r.state()))
    }
    /// Owned completion/error evidence.
    pub fn report(&self) -> FileOperation<RecordingReport> {
        self.worker.call(|r| Ok(r.report()))
    }
    /// Whether native recording is active; not a finalization acknowledgement.
    pub fn is_recording(&self) -> FileOperation<bool> {
        self.worker.call(|r| Ok(r.is_recording()))
    }
    /// Change source, triggering native rollover during an active session.
    pub fn set_source(&self, source: SourceSpec) -> FileOperation<()> {
        self.worker.call(move |r| r.set_source(source))
    }
    /// Last accepted request, not an authoritative current-source query.
    pub fn last_accepted_source(&self) -> FileOperation<Option<SourceSpec>> {
        self.worker.call(|r| Ok(r.last_accepted_source().cloned()))
    }
    /// Exact source query remains unavailable until its native lifetime contract is established.
    pub fn native_source(&self) -> FileOperation<SourceSpec> {
        self.worker.call(|r| r.native_source())
    }
    /// Owned current-file notification snapshot; may lag or be truncated.
    pub fn reported_current_file(&self) -> FileOperation<Option<EventText>> {
        self.worker.call(|r| Ok(r.reported_current_file().cloned()))
    }
    /// Exact path query preserves the synchronous API's explicit ownership restriction.
    pub fn native_current_file(&self) -> FileOperation<PathBuf> {
        self.worker.call(|r| r.native_current_file())
    }
    /// Request keyframe-aligned rollover.
    pub fn rollover(&self, target: RecordingTarget) -> FileOperation<()> {
        self.worker.call(move |r| r.rollover(target))
    }
    /// Transfer an owned metadata frame to the owner; native injection copies it.
    pub fn inject_metadata(&self, metadata: crate::MetadataFrame) -> FileOperation<()> {
        self.worker.call(move |r| r.inject_metadata(&metadata))
    }
    /// Owned recorder counters.
    pub fn stats(&self) -> FileOperation<Option<RecorderStats>> {
        self.worker.call(|r| Ok(r.stats()))
    }
    /// Owned detected source format.
    pub fn source_format(&self) -> FileOperation<Option<SourceFormat>> {
        self.worker.call(|r| r.source_format())
    }
    /// Stop/drain within the configured shutdown budget, then destroy on the owner thread.
    ///
    /// This closes all clones. It certifies resource cleanup, not file finalization;
    /// call `finish` and inspect its report first when finalization is required.
    pub fn close(self) -> FileOperation<()> {
        self.worker.control.request_close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        ffi::c_char,
        rc::Rc,
        sync::{atomic::AtomicUsize, Condvar},
        task::Wake,
    };

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
        fn cleanup(&mut self, _: Duration) {
            assert_eq!(self.created, thread::current().id());
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

    // This fake implements the native recorder boundary without loading NDI,
    // opening files, or advertising/subscribing to a source.
    struct RecorderMock {
        events: Mutex<VecDeque<super::super::ffi::RecorderEvent>>,
        changed: Condvar,
        running: AtomicBool,
        stops: AtomicUsize,
        destroys: AtomicUsize,
        stopped: mpsc::Sender<()>,
    }
    impl RecorderMock {
        fn push(&self, kind: i32) {
            let mut event = super::super::ffi::RecorderEvent {
                kind,
                ..Default::default()
            };
            for (slot, byte) in event.path.iter_mut().zip(b"mock-take.mkv") {
                *slot = *byte as c_char;
            }
            lock(&self.events).push_back(event);
            self.changed.notify_one();
        }
    }
    thread_local! {
        static RECORDER_MOCK: RefCell<Option<Arc<RecorderMock>>> = const { RefCell::new(None) };
    }
    unsafe fn recorder_mock<'a>(handle: super::super::ffi::Handle) -> &'a RecorderMock {
        unsafe { &*handle.cast::<Arc<RecorderMock>>() }
    }
    unsafe extern "C" fn mock_recorder_create(
        _: *const super::super::ffi::RecorderOptions,
    ) -> super::super::ffi::Handle {
        let shared = RECORDER_MOCK.with(|slot| slot.borrow_mut().take().unwrap());
        Box::into_raw(Box::new(shared)).cast()
    }
    unsafe extern "C" fn mock_recorder_start(
        handle: super::super::ffi::Handle,
        _: *const c_char,
    ) -> bool {
        let state = unsafe { recorder_mock(handle) };
        state.running.store(true, Ordering::Release);
        state.push(1);
        true
    }
    unsafe extern "C" fn mock_recorder_capture(
        handle: super::super::ffi::Handle,
        out: *mut super::super::ffi::RecorderEvent,
        wait_ms: u32,
    ) -> i32 {
        let state = unsafe { recorder_mock(handle) };
        let mut events = lock(&state.events);
        if events.is_empty() && wait_ms != 0 {
            (events, _) = state
                .changed
                .wait_timeout(events, Duration::from_millis(u64::from(wait_ms)))
                .unwrap();
        }
        if let Some(event) = events.pop_front() {
            let kind = event.kind;
            unsafe { out.write(event) };
            kind
        } else {
            0
        }
    }
    unsafe extern "C" fn mock_recorder_stop(handle: super::super::ffi::Handle) {
        let state = unsafe { recorder_mock(handle) };
        state.stops.fetch_add(1, Ordering::AcqRel);
        state.running.store(false, Ordering::Release);
        let _ = state.stopped.send(());
    }
    unsafe extern "C" fn mock_recorder_running(handle: super::super::ffi::Handle) -> bool {
        unsafe { recorder_mock(handle) }
            .running
            .load(Ordering::Acquire)
    }
    unsafe extern "C" fn mock_recorder_dropped(_: super::super::ffi::Handle) -> u64 {
        0
    }
    unsafe extern "C" fn mock_recorder_destroy(handle: super::super::ffi::Handle) {
        let state = unsafe { Box::from_raw(handle.cast::<Arc<RecorderMock>>()) };
        state.destroys.fetch_add(1, Ordering::AcqRel);
    }

    #[test]
    fn cancelled_finish_retains_recorder_and_notification_loss_preserves_closure_evidence() {
        let (stop_tx, stop_rx) = mpsc::channel();
        let state = Arc::new(RecorderMock {
            events: Mutex::new(VecDeque::new()),
            changed: Condvar::new(),
            running: AtomicBool::new(false),
            stops: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            stopped: stop_tx,
        });
        let owner_state = Arc::clone(&state);
        let mut functions = super::super::ffi::Functions::mocked();
        functions.recorder_create = mock_recorder_create;
        functions.recorder_start = mock_recorder_start;
        functions.recorder_capture = mock_recorder_capture;
        functions.recorder_stop = mock_recorder_stop;
        functions.recorder_running = mock_recorder_running;
        functions.recorder_dropped = mock_recorder_dropped;
        functions.recorder_destroy = mock_recorder_destroy;
        let api = FileApi::mocked(functions);
        let recorder = block_on(spawn_owner(
            "file-test-recorder",
            move || {
                RECORDER_MOCK.with(|slot| *slot.borrow_mut() = Some(owner_state));
                api.create_recorder(RecorderOptions::new(SourceSpec::new("mock source")?))
            },
            AsyncFileOptions::default().with_event_capacity(1).unwrap(),
            |worker| AsyncFileRecorder { worker },
        ))
        .unwrap();
        block_on(recorder.start(
            RecordingTarget::new(std::env::temp_dir().join("mock-file-not-created")).unwrap(),
            RecordingTimecode::Preserve,
        ))
        .unwrap();
        let finishing = recorder.finish(Duration::from_secs(1));
        stop_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(finishing);
        assert_eq!(block_on(recorder.state()).unwrap(), RecorderState::Stopping);
        assert_eq!(state.destroys.load(Ordering::Acquire), 0);
        state.push(10);
        let outcome = block_on(recorder.wait_finished(Duration::from_secs(1))).unwrap();
        let FinishStatus::Closed(report) = outcome else {
            panic!("expected confirmed closure after the later close event")
        };
        assert!(report.closure_confirmed());
        assert_eq!(report.dropped_events(), 0);
        assert_eq!(recorder.notification_dropped_count(), 1);
        block_on(recorder.close()).unwrap();
        assert_eq!(state.stops.load(Ordering::Acquire), 1);
        assert_eq!(state.destroys.load(Ordering::Acquire), 1);
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
