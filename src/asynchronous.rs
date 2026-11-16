//! Async discovery and receivers for Tokio and smol.
//!
//! [`AsyncReceiver`] wraps the synchronous NDI receiver API for an async
//! runtime, with `capture_video`, `capture_audio` and `capture_metadata`
//! methods for owned media and matching `capture_*_with` scoped callbacks.
//! [`AsyncReceiver::capture_any`] returns whichever owned media item or receiver
//! event arrives next. The adapter selects the [`AsyncRuntime`] whose blocking
//! thread pool runs the NDI operations:
//! [`Tokio`] with the `tokio` Cargo feature, [`Smol`] with the `smol` feature.
//! The [`tokio`] and [`smol`] submodules name the adapters for one runtime, for
//! example `grafton_ndi::asynchronous::tokio::AsyncReceiver`. This module
//! exists when either feature is enabled, and each runtime only with its own
//! feature; the crate root does not re-export any of it.
//!
//! [`AsyncFinder`] owns a [`crate::Finder`] or [`crate::SourceCache`] for async
//! selection and complete discovery windows. Its owned [`AsyncSourceWatch`]
//! retains a consumer's generation across awaits without borrowing a finder.
//! Runtime submodules also provide the `AsyncSourceCache` alias for cached
//! discovery. Dropped lookups stop their workers using the same rule as receivers.
//!
//! The NDI SDK operations are synchronous and blocking. Tokio runs them through
//! `tokio::task::spawn_blocking`; smol uses `smol::unblock`, keeping the work off
//! the async runtime's executor threads. Besides capture, [`AsyncReceiver`] has async
//! versions of the receiver's other waiting calls:
//! [`next_event`](AsyncReceiver::next_event), which returns the status and
//! source changes that captures pass over and keep,
//! [`wait_for_source_name_change`](AsyncReceiver::wait_for_source_name_change),
//! [`connect_to`](AsyncReceiver::connect_to),
//! [`disconnect`](AsyncReceiver::disconnect) and
//! [`reconnect`](AsyncReceiver::reconnect). It lends the [`Receiver`] itself,
//! through [`receiver`](AsyncReceiver::receiver), for the controls that do not
//! wait.
//!
//! For owned/scoped captures, `next_event` and `wait_for_source_name_change`, the timeout
//! budget starts when the future is first polled, rather than when it is
//! constructed. The same deadline covers blocking-pool queue delay and SDK waits.
//! This is a wait budget, not a hard completion deadline: scheduling, frame copying,
//! and other processing can delay the result beyond it.
//!
//! [`AsyncReceiver::capture_video_with`] and [`AsyncReceiver::capture_any_with`] run a
//! synchronous callback over validated borrowed media on the same blocking
//! worker that captures it, without wrapper payload allocations or copies.
//! Only its `Send + 'static` result crosses the task boundary. The callback
//! holds an SDK buffer and occupies a worker until it returns; keep processing
//! bounded. Use owned capture or explicitly copy in the callback to keep media
//! across awaits. Owned capture copies all active pixels/samples and present
//! metadata into new allocations (33,177,600 pixel bytes for 4K BGRA).
//!
//! Dropping a future sets a cancellation flag. Work is skipped if the blocking
//! closure observes that flag at its entry gate. A budgeted wait also checks
//! it before each subsequent attempt. An attempt already in flight, lasting at
//! most 100 ms in the SDK, can still consume an item and run its scoped callback
//! after the future was dropped. Callback time and side effects are not cancelled
//! by the wait timeout either. A panic is caught on the worker, where SDK guards
//! are released, then resumed on the awaiting task with its original payload.
//! It unwinds through `select!` or `join!` as any other task panic does. If the
//! future was dropped, its panic payload is discarded with the result.
//!
//! # Features
//!
//! - `tokio` - Enable Tokio runtime support
//! - `smol` - Enable smol runtime support
//!
//! # Example with Tokio
//!
//! ```no_run
//! # #[cfg(feature = "tokio")]
//! # {
//! use grafton_ndi::{Finder, FinderOptions, Ndi, ReceiverOptions,
//!     asynchronous::tokio::{AsyncFinder, AsyncReceiver}};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), grafton_ndi::Error> {
//!     let ndi = Ndi::new()?;
//!     let finder = AsyncFinder::new(Finder::new(&ndi, &FinderOptions::new())?);
//!     let Some(source) = finder.find_source_by_name("CAMERA (Program)",
//!         std::time::Duration::from_secs(5)).await? else { return Ok(()); };
//!
//!     let options = ReceiverOptions::snapshot_preset(source);
//!     let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
//!
//!     let async_receiver = AsyncReceiver::new(receiver);
//!
//!     if let Some(size) = async_receiver.capture_video_with(std::time::Duration::from_secs(5), |frame| {
//!         (frame.width(), frame.height())
//!     }).await? {
//!         println!("Captured dimensions: {size:?}");
//!     }
//!
//!     Ok(())
//! }
//! # }
//! ```

use std::{
    future::Future,
    marker::PhantomData,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::{
    capture::{AudioKind, CaptureKind, MetadataKind, VideoKind},
    receiver::ConnectionTarget,
    timeout::Deadline,
    Error, Receiver, ReceiverEvent, ReceiverItem, ReceiverItemRef, Result, Source,
    SourceNameChange, SpawnFailure,
};

mod discovery;
pub use discovery::{AsyncFinder, AsyncSourceWatch, SourceDiscovery};

mod private {
    use std::{future::Future, sync::atomic::AtomicBool};

    use crate::Result;

    /// Proof that the caller is this crate.
    ///
    /// A bound on [`AsyncRuntime`](super::AsyncRuntime) makes [`Spawn::spawn_blocking`]
    /// reachable from other crates even though they cannot name [`Spawn`];
    /// they cannot name this type either, so they cannot call it.
    #[derive(Debug)]
    pub struct Token;

    /// How a runtime runs blocking work on its thread pool.
    ///
    /// It is not nameable outside the crate, which keeps it out of the API
    /// and seals [`AsyncRuntime`](super::AsyncRuntime).
    pub trait Spawn {
        /// Run `f` on the runtime's blocking thread pool and return its result.
        ///
        /// # Errors
        ///
        /// [`Error::SpawnFailed`](crate::Error::SpawnFailed) when the work
        /// has no runtime context or its task is cancelled. Panics are resumed
        /// on the awaiting task after worker-owned resources are released.
        fn spawn_blocking<F, T>(token: Token, f: F) -> impl Future<Output = Result<T>> + Send
        where
            F: FnOnce() -> T + Send + 'static,
            T: Send + 'static,
        {
            Self::spawn_stoppable(token, move |_| f())
        }

        fn spawn_stoppable<F, T>(token: Token, f: F) -> impl Future<Output = Result<T>> + Send
        where
            F: FnOnce(&AtomicBool) -> T + Send + 'static,
            T: Send + 'static;
    }
}

// Dropping a future skips queued work and stops budgeted receiver waits
// before their next attempt. An attempt or callback already in flight finishes.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[cfg(test)]
fn blocking_operation<F, T>(operation: F) -> (CancelOnDrop, impl FnOnce() -> Result<T> + Send)
where
    F: FnOnce() -> T + Send,
{
    let (guard, work) = blocking_operation_with_stop(move |_| operation());
    (guard, move || work().map(resume_work))
}

fn blocking_operation_with_stop<F, T>(
    operation: F,
) -> (
    CancelOnDrop,
    impl FnOnce() -> Result<std::thread::Result<T>> + Send,
)
where
    F: FnOnce(&AtomicBool) -> T + Send,
{
    let cancelled = Arc::new(AtomicBool::new(false));
    let guard = CancelOnDrop(Arc::clone(&cancelled));
    let work = move || {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::SpawnFailed(SpawnFailure::TaskCancelled));
        }
        Ok(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || operation(&cancelled),
        )))
    };
    (guard, work)
}

fn resume_work<T>(outcome: std::thread::Result<T>) -> T {
    match outcome {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[cfg(feature = "tokio")]
fn tokio_join_error(error: ::tokio::task::JoinError) -> Error {
    if error.is_panic() {
        std::panic::resume_unwind(error.into_panic());
    }
    Error::SpawnFailed(SpawnFailure::TaskCancelled)
}

/// An async runtime whose blocking thread pool runs the NDI operations of an
/// [`AsyncReceiver`], [`AsyncFinder`] and [`AsyncSourceWatch`]:
/// [`Tokio`] or [`Smol`].
///
/// The trait is sealed and has no items of its own; it only selects the
/// runtime.
pub trait AsyncRuntime: private::Spawn + Send + Sync {}

/// The Tokio runtime, which runs blocking work with
/// `tokio::task::spawn_blocking`.
///
/// The work runs on the runtime of the task that awaits it; awaiting outside
/// a Tokio runtime fails with [`Error::SpawnFailed`]. Only a type, never a
/// value. Requires the `tokio` feature.
#[cfg(feature = "tokio")]
#[derive(Debug)]
pub enum Tokio {}

#[cfg(feature = "tokio")]
impl AsyncRuntime for Tokio {}

#[cfg(feature = "tokio")]
impl private::Spawn for Tokio {
    // Using `impl Future` instead of `async fn` in trait because we need explicit
    // Send bounds on the returned future. This pattern is intentional.
    #[allow(clippy::manual_async_fn)]
    fn spawn_stoppable<F, T>(_: private::Token, f: F) -> impl Future<Output = Result<T>> + Send
    where
        F: FnOnce(&AtomicBool) -> T + Send + 'static,
        T: Send + 'static,
    {
        async {
            let handle = ::tokio::runtime::Handle::try_current()
                .map_err(|_| Error::SpawnFailed(SpawnFailure::NoRuntime))?;
            let (_cancellation, work) = blocking_operation_with_stop(f);
            let outcome = handle
                .spawn_blocking(work)
                .await
                .map_err(tokio_join_error)??;
            Ok(resume_work(outcome))
        }
    }
}

/// The smol runtime, which runs blocking work with `smol::unblock`.
///
/// Only a type, never a value. Requires the `smol` feature.
#[cfg(feature = "smol")]
#[derive(Debug)]
pub enum Smol {}

#[cfg(feature = "smol")]
impl AsyncRuntime for Smol {}

#[cfg(feature = "smol")]
impl private::Spawn for Smol {
    // Using `impl Future` instead of `async fn` in trait because we need explicit
    // Send bounds on the returned future. This pattern is intentional.
    #[allow(clippy::manual_async_fn)]
    fn spawn_stoppable<F, T>(_: private::Token, f: F) -> impl Future<Output = Result<T>> + Send
    where
        F: FnOnce(&AtomicBool) -> T + Send + 'static,
        T: Send + 'static,
    {
        async {
            let (_cancellation, work) = blocking_operation_with_stop(f);
            Ok(resume_work(::smol::unblock(work).await?))
        }
    }
}

/// An async wrapper over a [`Receiver`] that runs its blocking NDI operations
/// on the blocking thread pool of the runtime `R`, so that they do not block
/// the async runtime.
///
/// [`tokio::AsyncReceiver`] and [`smol::AsyncReceiver`] name it for each
/// runtime.
///
/// # Thread Safety
///
/// The underlying [`Receiver`] is shared through an [`Arc`]. The SDK documents
/// concurrent capture calls in `Processing.NDI.Recv.h`. Local connection
/// changes serialize with one another and may overlap captures, as described
/// by [`Receiver::reconnect`]. Source-name waits serialize with one another;
/// waiting for another poller counts against the caller's wait budget.
///
/// # Events and controls
///
/// A capture that the SDK answers with a status or source change instead of
/// a frame keeps that change for [`next_event`](Self::next_event), as the
/// synchronous captures keep it for [`Receiver::next_event`]: both read the
/// same pending events. [`receiver`](Self::receiver) lends the receiver for
/// its controls that do not wait, such as tally, PTZ and upstream metadata.
///
/// # Error Handling
///
/// [`Error::SpawnFailed`] carries a typed [`SpawnFailure`] for a missing Tokio
/// context or a cancelled task. A panic on the worker is resumed on the awaiting
/// task with its original payload, after SDK guards are released on the worker.
/// An abandoned operation discards its result and any caught panic payload.
///
/// # Example
///
/// ```no_run
/// # #[cfg(feature = "tokio")]
/// # {
/// use grafton_ndi::{Ndi, ReceiverOptions, asynchronous::tokio::AsyncReceiver};
///
/// #[tokio::main]
/// async fn main() -> Result<(), grafton_ndi::Error> {
///     let ndi = Ndi::new()?;
///     // ... obtain source ...
///     # let source = grafton_ndi::Source::new("CAMERA (Program)").unwrap();
///
///     let options = ReceiverOptions::snapshot_preset(source);
///     let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
///     let async_receiver = AsyncReceiver::new(receiver);
///
///     // Capture without blocking the runtime
///     match async_receiver.capture_video_with(std::time::Duration::from_millis(100), |frame| {
///         (frame.width(), frame.height())
///     }).await? {
///         Some(size) => println!("Got dimensions: {size:?}"),
///         None => println!("No frame available"),
///     }
///
///     Ok(())
/// }
/// # }
/// ```
pub struct AsyncReceiver<R: AsyncRuntime> {
    inner: Arc<Receiver>,
    _runtime: PhantomData<R>,
}

impl<R: AsyncRuntime> std::fmt::Debug for AsyncReceiver<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncReceiver")
            .field("receiver", &self.inner)
            .field("runtime", &std::any::type_name::<R>())
            .finish_non_exhaustive()
    }
}

// Starts the budget on the future's first poll, so that blocking-pool queue
// delay is spent from it.
async fn with_timeout_budget<R, F, T>(timeout: Duration, operation: F) -> Result<T>
where
    R: AsyncRuntime,
    F: FnOnce(Deadline, &AtomicBool) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    with_deadline_budget::<R, _, _>(Deadline::after(timeout)?, operation).await
}

// Validated discovery inputs keep the deadline started before their conversion.
async fn with_deadline_budget<R, F, T>(deadline: Deadline, operation: F) -> Result<T>
where
    R: AsyncRuntime,
    F: FnOnce(Deadline, &AtomicBool) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    R::spawn_stoppable(private::Token, move |stop| operation(deadline, stop)).await?
}

// Keep ownership of the validated guard throughout application processing.
// Only the result leaves this scope, including when a task is abandoned.
fn process_captured<V, T>(captured: Option<V>, process: impl FnOnce(&V) -> T) -> Option<T> {
    captured.as_ref().map(process)
}

impl<R: AsyncRuntime> AsyncReceiver<R> {
    /// Create a new async receiver wrapper.
    ///
    /// The receiver is wrapped in an [`Arc`] to allow sharing across async tasks.
    pub fn new(receiver: Receiver) -> Self {
        Self {
            inner: Arc::new(receiver),
            _runtime: PhantomData,
        }
    }

    /// The receiver this wraps, lent for its controls: tally, PTZ, upstream
    /// and connection metadata, delays, the web-control URL and the latest
    /// [`connection_target`](Receiver::connection_target).
    ///
    /// The receiver is shared with every clone of this `AsyncReceiver` and
    /// with the async operations they have in flight. Its methods run on the
    /// calling thread, and those that wait block it: captures with a timeout,
    /// [`next_event`](Receiver::next_event),
    /// [`wait_for_source_name_change`](Receiver::wait_for_source_name_change),
    /// [`connect_to`](Receiver::connect_to),
    /// [`disconnect`](Receiver::disconnect) and
    /// [`reconnect`](Receiver::reconnect). Connection changes serialize with
    /// one another and run native work on the calling thread. Async code should use this
    /// type's async methods for those instead.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "tokio")]
    /// # {
    /// # use grafton_ndi::{Ndi, ReceiverOptions, Tally, asynchronous::tokio::AsyncReceiver};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let source = grafton_ndi::Source::new("CAMERA (Program)").unwrap();
    /// # let receiver = grafton_ndi::Receiver::new(&ndi, &ReceiverOptions::new(source))?;
    /// let async_receiver = AsyncReceiver::new(receiver);
    /// async_receiver.receiver().set_tally(Tally::new(true, false));
    /// if async_receiver.receiver().ptz().is_supported() {
    ///     async_receiver.receiver().ptz().zoom(0.5)?;
    /// }
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    pub fn receiver(&self) -> &Receiver {
        &self.inner
    }

    /// Async version of [`Receiver::capture_any`], delivering owned video,
    /// audio, metadata, or a receiver event through the shared capture loop on the
    /// runtime's blocking pool. Borrowed native frames remain on that worker
    /// and are freed after their validated owned copy has been made. Video/audio
    /// copy active pixels/samples and present metadata into new allocations;
    /// standalone metadata copies its text. Use
    /// [`capture_any_with`](Self::capture_any_with) for immediate inspection
    /// without these payload allocations and copies.
    ///
    /// The wait budget starts at first poll and includes blocking-pool queue
    /// delay and SDK waits. A spent or zero budget makes one non-waiting
    /// attempt. Scheduling and copying can finish after the budget.
    /// Events compete with all other event and combined consumers;
    /// see [`Receiver::capture_any_ref`] for their coalescing contract.
    ///
    /// # Errors
    ///
    /// The errors of [`Receiver::capture_any`], and [`Error::SpawnFailed`]
    /// when no runtime context is entered or the blocking task is cancelled. Oversize timeouts
    /// are rejected before submitting work.
    ///
    /// # Cancellation
    ///
    /// Dropping the future skips queued work and stops a started wait before
    /// its next attempt. The attempt already in flight can still consume an
    /// item whose result is discarded; each native wait lasts at most 100 ms.
    #[doc(alias = "NDIlib_recv_capture_v3")]
    pub async fn capture_any(&self, timeout: Duration) -> Result<Option<ReceiverItem>> {
        self.capture_any_with(timeout, |item| item.to_owned()).await
    }

    /// Process the next video, audio, metadata or receiver event on the
    /// blocking worker, without copying its payload into an owned item.
    ///
    /// `process` synchronously borrows the validated [`ReceiverItemRef`]. It
    /// runs once for a delivered item; timeouts and capture errors do not
    /// call it. The wrapper keeps the native frame guard until the callback
    /// returns (or unwinds), then frees it on the same worker. Only the
    /// returned `T` crosses the task
    /// boundary. Borrowed frames, planes, rows, channels and text cannot escape.
    /// Use an owned result, or explicitly call [`ReceiverItemRef::to_owned`],
    /// to retain data across awaits.
    ///
    /// The callback and its captures must be `Send + 'static`, as must `T`.
    /// Keep processing bounded: it occupies a blocking worker and retains an
    /// SDK buffer throughout. No wrapper payload allocation or copy is made;
    /// runtime/task allocations and allocations by the callback still apply.
    ///
    /// The wait budget, event delivery and cancellation rules are those of
    /// [`capture_any`](Self::capture_any). Callback execution time and side
    /// effects are not cancelled by the timeout or by dropping the future.
    /// An item delivered by the attempt in flight still runs its callback
    /// even if the future was dropped.
    ///
    /// # Errors
    ///
    /// The errors of [`Receiver::capture_any_ref`], and [`Error::SpawnFailed`]
    /// for a missing runtime context or cancelled task. Oversize timeouts are
    /// rejected before submitting work.
    ///
    /// # Panics
    ///
    /// A callback or blocking-operation panic is resumed on the awaiting task
    /// with its original payload, after the native frame is freed on the worker.
    #[doc(alias = "NDIlib_recv_capture_v3")]
    pub async fn capture_any_with<F, T>(&self, timeout: Duration, process: F) -> Result<Option<T>>
    where
        F: for<'frame> FnOnce(&ReceiverItemRef<'frame>) -> T + Send + 'static,
        T: Send + 'static,
    {
        let receiver = Arc::clone(&self.inner);
        with_timeout_budget::<R, _, _>(timeout, move |deadline, stop| {
            Ok(process_captured(
                receiver.capture_any_ref_until(deadline, stop)?,
                process,
            ))
        })
        .await
    }

    /// Async version of [`Receiver::next_event`]: wait up to `timeout` for a
    /// status or source change, or return `None` when none came, running on
    /// the blocking pool.
    ///
    /// The events include those that typed async captures and the
    /// receiver's synchronous captures passed over and kept. They are
    /// coalesced invalidations, not an ordered history; concurrent pollers,
    /// through this type, its clones or the lent [`receiver`](Self::receiver),
    /// compete to consume a pending one.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Total budget to wait for an event, starting when the
    ///   future is first polled and spent as [`Receiver::next_event`] spends
    ///   it. Blocking-pool queue delay and SDK waits count
    ///   against the same budget, and `None` comes only once all of it is
    ///   spent. [`Duration::ZERO`], like a budget that queue delay has
    ///   already spent, makes one attempt without waiting. Completion can
    ///   occur after this budget because scheduling is not bounded by it.
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Errors
    ///
    /// The errors of [`Receiver::next_event`], and [`Error::SpawnFailed`]
    /// when no runtime context is entered or the blocking task is cancelled.
    ///
    /// # Cancellation
    ///
    /// Dropping the future skips queued work and stops a started wait before
    /// its next attempt. The attempt already in flight can still consume an
    /// event whose result is discarded; each native wait lasts at most 100 ms.
    #[doc(alias = "NDIlib_recv_capture_v3")]
    pub async fn next_event(&self, timeout: Duration) -> Result<Option<ReceiverEvent>> {
        let receiver = Arc::clone(&self.inner);
        with_timeout_budget::<R, _, _>(timeout, move |deadline, stop| {
            receiver.next_event_until(deadline, stop)
        })
        .await
    }

    /// Async version of [`Receiver::wait_for_source_name_change`]: wait up to
    /// `timeout` for a change to the name of the source the receiver is
    /// actually connected to, running on the blocking pool.
    ///
    /// `None` means no change within the budget; see
    /// [`Receiver::wait_for_source_name_change`] for what a change reports.
    /// Concurrent pollers compete for changes.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Total budget to wait for a change, starting when the
    ///   future is first polled. Blocking-pool queue delay and waiting for a
    ///   concurrent poller count against the same budget. Completion can
    ///   occur after this budget because scheduling is not bounded by it.
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Errors
    ///
    /// The errors of [`Receiver::wait_for_source_name_change`]:
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`],
    /// [`Error::UnsupportedCapability`] without
    /// [`RuntimeFeature::ConnectedSourceName`](crate::RuntimeFeature::ConnectedSourceName),
    /// and [`Error::UnexpectedSdkResult`] for a name the SDK did not
    /// terminate; and [`Error::SpawnFailed`] for a missing runtime context or
    /// cancelled blocking task.
    ///
    /// Input and capability errors are reported before work is submitted.
    ///
    /// # Cancellation
    ///
    /// Dropping the future skips queued work and stops a started wait before
    /// its next attempt. The attempt already in flight can still consume a
    /// change whose result is discarded; each native wait lasts at most 100 ms.
    /// The SDK reports each change once.
    #[doc(alias = "NDIlib_recv_get_source_name")]
    pub async fn wait_for_source_name_change(
        &self,
        timeout: Duration,
    ) -> Result<Option<SourceNameChange>> {
        let deadline = Deadline::after(timeout)?;
        // The task owns copies of the immutable tables it validated, keeping
        // its inputs independent of reference-lifetime inference on the MSRV.
        let (source_name, strings) = *self.inner.core.source_name_tables()?;
        let receiver = Arc::clone(&self.inner);
        R::spawn_stoppable(private::Token, move |stop| {
            receiver
                .core
                .wait_for_source_name_change_with(&source_name, &strings, deadline, stop)
        })
        .await?
    }

    /// Point the underlying receiver at a different source and remember it as
    /// the target that [`Self::reconnect`] retries. See
    /// [`Receiver::connect_to`].
    ///
    /// Like [`Self::reconnect`], this serializes with other local connection
    /// changes and runs the native request on the blocking pool. Captures may
    /// continue while the SDK applies it. Success means the request was
    /// issued, not that the source is available; watch
    /// [`Self::wait_for_source_name_change`] or [`Receiver::connection_stats`].
    /// The source was validated when constructed. The calling task prepares
    /// its owned target when the future is first polled.
    ///
    /// # Errors
    ///
    /// [`Error::SpawnFailed`] for a missing runtime context or cancelled
    /// blocking task. The source was validated when it was constructed.
    ///
    /// # Cancellation
    ///
    /// Dropping this future skips work if the blocking closure observes the
    /// cancellation flag at its entry gate. After passing that gate, the
    /// connection change continues, including waiting for earlier connection changes,
    /// and may take effect after this future is dropped.
    #[doc(alias = "NDIlib_recv_connect")]
    pub async fn connect_to(&self, source: &Source) -> Result<()> {
        let target = ConnectionTarget::new(source);
        let receiver = Arc::clone(&self.inner);
        R::spawn_blocking(private::Token, move || receiver.connect_target(target)).await
    }

    /// Disconnect the underlying receiver from its source, retaining the
    /// latest target that [`Self::reconnect`] retries. See
    /// [`Receiver::disconnect`].
    ///
    /// Like [`Self::reconnect`], this serializes with local connection changes
    /// and runs the native request on the blocking pool while captures continue.
    ///
    /// # Errors
    ///
    /// [`Error::SpawnFailed`] for a missing runtime context or
    /// cancelled blocking task; [`Receiver::disconnect`] itself does not fail.
    ///
    /// # Cancellation
    ///
    /// Dropping this future skips work if the blocking closure observes the
    /// cancellation flag at its entry gate. After passing that gate,
    /// disconnect continues, including waiting for earlier connection changes, and may
    /// disconnect after this future is dropped.
    #[doc(alias = "NDIlib_recv_connect")]
    pub async fn disconnect(&self) -> Result<()> {
        let receiver = Arc::clone(&self.inner);
        R::spawn_blocking(private::Token, move || receiver.disconnect()).await
    }

    /// Re-establish the underlying receiver's connection to its source
    /// in place. See [`Receiver::reconnect`].
    ///
    /// Reconnect serializes with other local connection changes and runs the
    /// native request on the blocking pool. Captures may continue meanwhile.
    /// Confirm recovery via [`Receiver::connection_stats`]. A remote SourceDiscovery
    /// Server source change does not update the local target this requests.
    ///
    /// # Errors
    ///
    /// [`Error::SpawnFailed`] for a missing runtime context or cancelled
    /// blocking task. The synchronous [`Receiver::reconnect`] cannot fail.
    ///
    /// # Cancellation
    ///
    /// Dropping this future skips work if the blocking closure observes the
    /// cancellation flag at its entry gate. After passing that gate, reconnect
    /// continues, including waiting for earlier connection changes, and may reconnect
    /// after this future is dropped.
    #[doc(alias = "NDIlib_recv_connect")]
    pub async fn reconnect(&self) -> Result<()> {
        let receiver = Arc::clone(&self.inner);
        R::spawn_blocking(private::Token, move || receiver.reconnect()).await
    }
}

impl<R: AsyncRuntime> Clone for AsyncReceiver<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            _runtime: PhantomData,
        }
    }
}

macro_rules! async_receiver_capture_methods {
    ($(($mode:ident, $kind:ident, $owned:ident, $borrowed:ident,
        $capture:ident, $capture_ref:ident, $capture_with:ident, $label:literal)),* $(,)?) => {
        $(
            impl<R: AsyncRuntime> AsyncReceiver<R> {
                #[doc = concat!("Capture owned ", $label, " on the runtime's blocking pool.")]
                ///
                /// The budget starts at first poll and includes blocking-pool queue
                /// delay. A zero or spent budget makes one nonblocking SDK attempt.
                /// Dropping the future stops waiting before its next attempt;
                /// an attempt already running can consume an item that is discarded.
                ///
                /// # Errors
                ///
                /// Invalid timeouts, capture errors, or blocking-worker spawn failure.
                #[doc(alias = "NDIlib_recv_capture_v3")]
                pub async fn $capture(&self, timeout: Duration) -> Result<Option<crate::$owned>> {
                    self.$capture_with(timeout, |frame| frame.to_owned()).await
                }

                #[doc = concat!("Process borrowed ", $label, " on the blocking worker without copying its payload.")]
                ///
                /// `process` runs once for a delivered frame and retains its SDK
                /// guard until it returns or unwinds. Only `T` crosses back to the
                /// awaiting task; native frames and their borrowed slices cannot
                /// escape. The callback and result must be `Send + 'static`.
                /// The budget includes blocking-pool queue delay and SDK waits;
                /// callback execution can finish after it and continues when the
                /// future is dropped. Panics resume on the awaiting task after
                /// the native guard has been released on the worker.
                ///
                /// # Errors
                ///
                /// Invalid timeouts, capture errors, or blocking-worker spawn failure.
                #[doc(alias = "NDIlib_recv_capture_v3")]
                pub async fn $capture_with<F, T>(&self, timeout: Duration, process: F) -> Result<Option<T>>
                where
                    F: for<'frame> FnOnce(&crate::$borrowed<'frame>) -> T + Send + 'static,
                    T: Send + 'static,
                {
                    self.capture_kind_with::<$kind, _, _>(timeout, process).await
                }
            }
        )*
    };
}
crate::capture::capture_method_kinds!(async_receiver_capture_methods);

impl<R: AsyncRuntime> AsyncReceiver<R> {
    async fn capture_kind_with<K, F, T>(&self, timeout: Duration, process: F) -> Result<Option<T>>
    where
        K: CaptureKind,
        F: for<'frame> FnOnce(&K::Ref<'frame>) -> T + Send + 'static,
        T: Send + 'static,
    {
        let receiver = Arc::clone(&self.inner);
        with_timeout_budget::<R, _, _>(timeout, move |deadline, stop| {
            Ok(process_captured(
                receiver.capture_ref_until::<K>(deadline, stop)?,
                process,
            ))
        })
        .await
    }
}

#[cfg(feature = "tokio")]
pub mod tokio {
    //! [`AsyncReceiver`] on the Tokio runtime: the
    //! generic [`asynchronous::AsyncReceiver`](super::AsyncReceiver) with
    //! [`Tokio`](super::Tokio), which run blocking work with
    //! `tokio::task::spawn_blocking`.
    //!
    //! # Example
    //!
    //! ```no_run
    //! use grafton_ndi::{Ndi, ReceiverOptions, asynchronous::tokio::AsyncReceiver};
    //!
    //! #[tokio::main]
    //! async fn main() -> Result<(), grafton_ndi::Error> {
    //!     let ndi = Ndi::new()?;
    //!     // ... obtain source ...
    //!     # let source = grafton_ndi::Source::new("CAMERA (Program)").unwrap();
    //!
    //!     let options = ReceiverOptions::snapshot_preset(source);
    //!     let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
    //!     let async_receiver = AsyncReceiver::new(receiver);
    //!
    //!     // Capture without blocking the runtime
    //!     match async_receiver.capture_video_with(std::time::Duration::from_millis(100), |frame| {
    //!         (frame.width(), frame.height())
    //!     }).await? {
    //!         Some(size) => println!("Got dimensions: {size:?}"),
    //!         None => println!("No frame available"),
    //!     }
    //!
    //!     Ok(())
    //! }
    //! ```

    /// An [`AsyncReceiver`](super::AsyncReceiver) on the Tokio runtime.
    pub type AsyncReceiver = super::AsyncReceiver<super::Tokio>;
    /// Source discovery on the Tokio runtime, owning a finder or source cache.
    pub type AsyncFinder<D = crate::Finder> = super::AsyncFinder<super::Tokio, D>;
    /// Cached discovery on the Tokio runtime.
    pub type AsyncSourceCache = AsyncFinder<crate::SourceCache>;
    /// An owned discovery cursor on the Tokio runtime.
    pub type AsyncSourceWatch<D = crate::Finder> = super::AsyncSourceWatch<super::Tokio, D>;
}

#[cfg(feature = "smol")]
pub mod smol {
    //! [`AsyncReceiver`] on the smol runtime: the
    //! generic [`asynchronous::AsyncReceiver`](super::AsyncReceiver) with
    //! [`Smol`](super::Smol), which run blocking work with `smol::unblock`.
    //!
    //! # Example
    //!
    //! ```no_run
    //! use grafton_ndi::{Ndi, ReceiverOptions, asynchronous::smol::AsyncReceiver};
    //!
    //! fn main() -> Result<(), grafton_ndi::Error> {
    //!     ::smol::block_on(async {
    //!         let ndi = Ndi::new()?;
    //!         // ... obtain source ...
    //!         # let source = grafton_ndi::Source::new("CAMERA (Program)").unwrap();
    //!
    //!         let options = ReceiverOptions::snapshot_preset(source);
    //!         let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
    //!         let async_receiver = AsyncReceiver::new(receiver);
    //!
    //!         // Capture without blocking the runtime
    //!         match async_receiver.capture_video_with(std::time::Duration::from_millis(100), |frame| {
    //!             (frame.width(), frame.height())
    //!         }).await? {
    //!             Some(size) => println!("Got dimensions: {size:?}"),
    //!             None => println!("No frame available"),
    //!         }
    //!
    //!         Ok(())
    //!     })
    //! }
    //! ```

    /// An [`AsyncReceiver`](super::AsyncReceiver) on the smol runtime.
    pub type AsyncReceiver = super::AsyncReceiver<super::Smol>;
    /// Source discovery on the smol runtime, owning a finder or source cache.
    pub type AsyncFinder<D = crate::Finder> = super::AsyncFinder<super::Smol, D>;
    /// Cached discovery on the smol runtime.
    pub type AsyncSourceCache = AsyncFinder<crate::SourceCache>;
    /// An owned discovery cursor on the smol runtime.
    pub type AsyncSourceWatch<D = crate::Finder> = super::AsyncSourceWatch<super::Smol, D>;
}

#[cfg(all(test, feature = "tokio"))]
mod timeout_tests {
    use super::*;
    use std::{
        sync::mpsc,
        task::{Context, Poll, Waker},
    };

    fn runtime() -> ::tokio::runtime::Runtime {
        ::tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .build()
            .unwrap()
    }

    #[test]
    fn queued_work_uses_remaining_budget() {
        let runtime = runtime();
        runtime.block_on(async {
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let blocker = ::tokio::task::spawn_blocking(move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            started_rx.recv().unwrap();

            let budget = Duration::from_millis(20);
            let mut capture =
                Box::pin(with_timeout_budget::<Tokio, _, _>(budget, |deadline, _| {
                    Ok((deadline.remaining(), deadline.remaining_ms()))
                }));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(capture.as_mut().poll(&mut context), Poll::Pending));
            // Keep the sole worker occupied until the whole budget has elapsed.
            std::thread::sleep(budget + Duration::from_millis(5));
            release_tx.send(()).unwrap();
            // The queue delay spent the budget: the work starts with none left.
            let (remaining, remaining_ms) = capture.await.unwrap();
            assert_eq!((remaining, remaining_ms), (Duration::ZERO, 0));
            blocker.await.unwrap();
        });
    }

    #[test]
    fn budgeted_work_resumes_the_original_panic() {
        let panic = std::panic::catch_unwind(|| {
            runtime().block_on(with_timeout_budget::<Tokio, _, ()>(
                Duration::ZERO,
                |_, _| std::panic::panic_any(42_u32),
            ))
        })
        .expect_err("a worker panic must unwind the awaiting caller");
        assert_eq!(*panic.downcast::<u32>().unwrap(), 42);
    }

    #[test]
    fn invalid_budget_is_rejected_before_spawning() {
        // No entered runtime: trying to submit work would panic.
        let mut capture = Box::pin(with_timeout_budget::<Tokio, _, ()>(
            crate::MAX_TIMEOUT + Duration::from_nanos(1),
            |_, _| panic!("invalid timeout must not execute blocking work"),
        ));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            capture.as_mut().poll(&mut context),
            Poll::Ready(Err(_))
        ));
    }
}

#[cfg(all(test, feature = "smol"))]
mod smol_tests {
    use super::{
        private::{Spawn, Token},
        *,
    };
    use std::{
        sync::mpsc,
        task::{Context, Poll, Waker},
        time::Instant,
    };

    #[test]
    fn dropping_unpolled_future_starts_no_work() {
        let called = Arc::new(AtomicBool::new(false));
        let worker_called = Arc::clone(&called);
        let operation = Smol::spawn_blocking(Token, move || {
            worker_called.store(true, Ordering::Release);
        });
        drop(operation);
        assert!(!called.load(Ordering::Acquire));
    }

    #[test]
    fn dropping_started_future_allows_work_to_finish() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let mut operation = Box::pin(Smol::spawn_blocking(Token, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            finished_tx.send(42).unwrap();
        }));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            operation.as_mut().poll(&mut context),
            Poll::Pending
        ));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(operation);
        release_tx.send(()).unwrap();
        assert_eq!(
            finished_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            42
        );
    }

    #[test]
    fn invalid_budget_does_not_start_work() {
        let called = Arc::new(AtomicBool::new(false));
        let worker_called = Arc::clone(&called);
        let result = ::smol::block_on(with_timeout_budget::<Smol, _, _>(
            crate::MAX_TIMEOUT + Duration::from_nanos(1),
            move |_, _| {
                worker_called.store(true, Ordering::Release);
                Ok(())
            },
        ));
        assert!(result.is_err());
        assert!(!called.load(Ordering::Acquire));
    }

    #[test]
    fn budgeted_work_resumes_the_original_panic() {
        let panic = std::panic::catch_unwind(|| {
            ::smol::block_on(with_timeout_budget::<Smol, _, ()>(
                Duration::ZERO,
                |_, _| std::panic::panic_any(42_u32),
            ))
        })
        .expect_err("a worker panic must unwind the awaiting caller");
        assert_eq!(*panic.downcast::<u32>().unwrap(), 42);
    }

    #[test]
    fn budget_starts_on_first_poll_and_keeps_one_deadline() {
        let budget = Duration::from_millis(20);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut operation = Box::pin(with_timeout_budget::<Smol, _, _>(
            budget,
            move |deadline: Deadline, _: &AtomicBool| {
                started_tx.send(deadline.end()).unwrap();
                release_rx.recv().unwrap();
                Ok(deadline.remaining())
            },
        ));
        // Construction must not consume the wait budget.
        std::thread::sleep(budget);
        let before_poll = Instant::now();
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            operation.as_mut().poll(&mut context),
            Poll::Pending
        ));
        let after_poll = Instant::now();
        let deadline = started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(deadline >= before_poll + budget);
        assert!(deadline <= after_poll + budget);
        // A wait inside blocking work must not reset the shared capture deadline.
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        release_tx.send(()).unwrap();
        assert_eq!(::smol::block_on(operation).unwrap(), Duration::ZERO);
    }
}

#[cfg(test)]
mod executor_tests {
    use super::{
        private::{Spawn, Token},
        *,
    };

    #[test]
    fn cancellation_skips_queued_operation() {
        let (guard, work) = blocking_operation(|| panic!("cancelled work must not run"));
        drop(guard);
        assert!(matches!(
            work(),
            Err(Error::SpawnFailed(SpawnFailure::TaskCancelled))
        ));
    }

    #[test]
    fn cancellation_allows_started_operation_to_finish() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let (guard, work) = blocking_operation(move || {
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
            42
        });
        let worker = std::thread::spawn(work);
        started_rx.recv().unwrap();
        drop(guard);
        finish_tx.send(()).unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), 42);
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn missing_tokio_context_returns_error() {
        use std::task::{Context, Poll, Waker};
        let mut operation = Box::pin(Tokio::spawn_blocking(Token, || 42));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            operation.as_mut().poll(&mut context),
            Poll::Ready(Err(Error::SpawnFailed(SpawnFailure::NoRuntime)))
        ));
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn validated_connect_requires_a_runtime_without_changing_the_target() {
        use std::task::{Context, Poll, Waker};
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = AsyncReceiver::<Tokio>::new(
            Receiver::new(&ndi, &crate::ReceiverOptions::new(Source::new("").unwrap())).unwrap(),
        );
        assert!(matches!(
            Source::new("NUL\0name"),
            Err(Error::InvalidCString(_))
        ));
        let source = Source::new("validated target").unwrap();
        let mut connect = std::pin::pin!(receiver.connect_to(&source));
        assert!(matches!(
            connect
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Err(Error::SpawnFailed(SpawnFailure::NoRuntime)))
        ));
        assert_eq!(
            receiver.receiver().connection_target(),
            Source::new("").unwrap()
        );
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn tokio_resumes_blocking_panic() {
        let runtime = ::tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let panic = std::panic::catch_unwind(|| {
            runtime.block_on(Tokio::spawn_blocking(Token, || panic!("test panic")))
        })
        .unwrap_err();
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"test panic"));
    }

    #[cfg(feature = "smol")]
    #[test]
    fn smol_resumes_blocking_panic() {
        let panic = std::panic::catch_unwind(|| {
            ::smol::block_on(Smol::spawn_blocking(Token, || panic!("test panic")))
        })
        .unwrap_err();
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"test panic"));
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn cancelled_tokio_task_reports_a_typed_reason() {
        let runtime = ::tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let task = ::tokio::spawn(std::future::pending::<()>());
            task.abort();
            assert!(matches!(
                tokio_join_error(task.await.unwrap_err()),
                Error::SpawnFailed(SpawnFailure::TaskCancelled)
            ));
        });
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn dropping_tokio_future_skips_queued_work() {
        use std::task::{Context, Poll, Waker};
        let runtime = ::tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = ::tokio::task::spawn_blocking(move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            started_rx.recv().unwrap();
            let called = Arc::new(AtomicBool::new(false));
            let worker_called = Arc::clone(&called);
            let mut operation = Box::pin(Tokio::spawn_blocking(Token, move || {
                worker_called.store(true, Ordering::Release)
            }));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(
                operation.as_mut().poll(&mut context),
                Poll::Pending
            ));
            drop(operation);
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            // The single worker processes the cancelled closure before this sentinel.
            ::tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert!(!called.load(Ordering::Acquire));
        });
    }
}

#[cfg(test)]
mod receiver_tests {
    use super::*;
    use crate::{Ndi, ReceiverOptions, RuntimeFeature};

    /// An async receiver of `ndi` that stays unconnected: its source has no
    /// name and no address.
    fn unconnected<R: AsyncRuntime>(ndi: &Ndi) -> AsyncReceiver<R> {
        AsyncReceiver::new(
            Receiver::new(ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap(),
        )
    }

    // Scoped entry points keep capturing while local connection changes are
    // locked. A disconnected source can still deliver connection events.
    #[allow(clippy::await_holding_lock)] // Verify captures do not take the connection lock.
    async fn scoped_captures_during_connection_changes<R: AsyncRuntime>(
        receiver: &AsyncReceiver<R>,
    ) {
        let _exclusive = receiver.inner.core.reconnect_lock();
        for timeout in [Duration::ZERO, Duration::from_millis(25)] {
            assert!(receiver
                .capture_video_with(timeout, |_| panic!("no video"))
                .await
                .unwrap()
                .is_none());
            receiver
                .capture_any_with(timeout, |item| {
                    assert!(matches!(item, ReceiverItemRef::Event(_)));
                })
                .await
                .unwrap();
        }
    }

    /// Lending the receiver adds no field and preserves its auto traits.
    /// Receiver and both runtime-specific AsyncReceiver types are UnwindSafe
    /// and RefUnwindSafe, as tests/public_auto_traits.rs asserts.
    #[test]
    fn async_receivers_keep_their_auto_traits() {
        fn shared<T: Send + Sync + Clone + 'static>() {}
        #[cfg(feature = "tokio")]
        shared::<AsyncReceiver<Tokio>>();
        #[cfg(feature = "smol")]
        shared::<AsyncReceiver<Smol>>();
    }

    /// Every future can be spawned on a multi-threaded executor.
    fn futures_are_send<R: AsyncRuntime>(receiver: &AsyncReceiver<R>, source: &Source) {
        fn send<T: Send>(_: T) {}
        send(receiver.capture_video(Duration::ZERO));
        send(receiver.capture_video_with(Duration::ZERO, |frame| frame.width()));
        send(receiver.capture_audio_with(Duration::ZERO, |frame| frame.num_samples()));
        send(receiver.capture_metadata_with(Duration::ZERO, |frame| frame.data().len()));
        send(receiver.capture_any(Duration::ZERO));
        send(receiver.capture_any_with(Duration::ZERO, |_| ()));
        send(receiver.next_event(Duration::ZERO));
        send(receiver.wait_for_source_name_change(Duration::ZERO));
        send(receiver.connect_to(source));
        send(receiver.disconnect());
        send(receiver.reconnect());
    }

    #[test]
    fn async_receiver_futures_are_send() {
        // Type checking is the test: the function is never called.
        #[cfg(feature = "tokio")]
        let _: fn(&AsyncReceiver<Tokio>, &Source) = futures_are_send::<Tokio>;
        #[cfg(feature = "smol")]
        let _: fn(&AsyncReceiver<Smol>, &Source) = futures_are_send::<Smol>;
    }

    /// Every async operation of `receiver`, which `unconnected` made.
    async fn operations_of_an_unconnected_receiver<R: AsyncRuntime>(
        ndi: &Ndi,
        receiver: &AsyncReceiver<R>,
    ) {
        // The lent receiver is the one the operations use.
        assert!(std::ptr::eq(receiver.receiver(), &*receiver.inner));
        assert_eq!(
            receiver.receiver().connection_target(),
            Source::new("").unwrap()
        );

        let excessive = crate::MAX_TIMEOUT + Duration::from_nanos(1);
        assert!(matches!(
            receiver.next_event(excessive).await,
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            receiver.capture_any(excessive).await,
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            receiver.wait_for_source_name_change(excessive).await,
            Err(Error::InvalidInput(_))
        ));
        assert!(receiver.next_event(Duration::ZERO).await.unwrap().is_none());
        assert!(receiver
            .capture_any(Duration::ZERO)
            .await
            .unwrap()
            .is_none());
        let change = receiver.wait_for_source_name_change(Duration::ZERO).await;
        if ndi
            .require_feature(RuntimeFeature::ConnectedSourceName)
            .is_ok()
        {
            assert!(change.is_ok(), "{change:?}");
        } else {
            assert!(
                matches!(change, Err(Error::UnsupportedCapability { .. })),
                "{change:?}"
            );
        }

        assert!(matches!(
            Source::new("NUL\0in name"),
            Err(Error::InvalidCString(_))
        ));
        assert_eq!(
            receiver.receiver().connection_target(),
            Source::new("").unwrap()
        );
        let target = Source::new("changed target").unwrap();
        receiver.connect_to(&target).await.unwrap();
        receiver.disconnect().await.unwrap();
        receiver.reconnect().await.unwrap();
        // Disconnect and reconnect retain the latest requested target.
        assert_eq!(receiver.receiver().connection_target(), target);
    }

    #[cfg(feature = "smol")]
    #[test]
    fn smol_operations_of_an_unconnected_receiver() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = unconnected::<Smol>(&ndi);
        ::smol::block_on(operations_of_an_unconnected_receiver(&ndi, &receiver));
        ::smol::block_on(scoped_captures_during_connection_changes(&receiver));
    }

    #[cfg(feature = "tokio")]
    mod tokio_runtime {
        use super::*;
        use crate::receiver::native_capture_calls;
        use std::{
            sync::mpsc,
            task::{Context, Poll, Waker},
            time::Instant,
        };

        /// A runtime with one blocking thread, so that work queues behind a
        /// blocker, and every native capture is made, and counted, on that
        /// one thread.
        fn runtime() -> ::tokio::runtime::Runtime {
            ::tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(1)
                .build()
                .unwrap()
        }

        /// The native captures made on the one blocking thread since the
        /// previous call.
        async fn worker_capture_calls() -> usize {
            ::tokio::task::spawn_blocking(native_capture_calls::take)
                .await
                .unwrap()
        }

        /// Occupy the one blocking thread until the returned sender is used.
        fn occupy_worker() -> (mpsc::Sender<()>, ::tokio::task::JoinHandle<()>) {
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let blocker = ::tokio::task::spawn_blocking(move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            started_rx.recv().unwrap();
            (release_tx, blocker)
        }

        #[test]
        fn tokio_operations_of_an_unconnected_receiver() {
            let Some(ndi) = crate::test_support::ndi() else {
                return;
            };
            let receiver = unconnected::<Tokio>(&ndi);
            runtime().block_on(operations_of_an_unconnected_receiver(&ndi, &receiver));
            runtime().block_on(scoped_captures_during_connection_changes(&receiver));
        }

        #[test]
        fn scoped_capture_spends_queue_delay_and_skips_cancelled_callbacks() {
            let Some(ndi) = crate::test_support::ndi() else {
                return;
            };
            let receiver = unconnected::<Tokio>(&ndi);
            runtime().block_on(async {
                worker_capture_calls().await;
                let (release, blocker) = occupy_worker();
                let budget = Duration::from_millis(25);
                let mut capture =
                    Box::pin(receiver.capture_video_with(budget, |_| panic!("no frame")));
                let mut context = Context::from_waker(Waker::noop());
                assert!(matches!(capture.as_mut().poll(&mut context), Poll::Pending));
                std::thread::sleep(budget + Duration::from_millis(10));
                release.send(()).unwrap();
                assert!(capture.await.unwrap().is_none());
                blocker.await.unwrap();
                assert_eq!(
                    ::tokio::task::spawn_blocking(native_capture_calls::take_with_longest_wait)
                        .await
                        .unwrap(),
                    (1, 0)
                );

                let (release, blocker) = occupy_worker();
                let mut capture =
                    Box::pin(receiver.capture_any_with(Duration::from_secs(1), |_| {
                        panic!("cancelled callback")
                    }));
                assert!(matches!(capture.as_mut().poll(&mut context), Poll::Pending));
                drop(capture);
                release.send(()).unwrap();
                blocker.await.unwrap();
                assert_eq!(worker_capture_calls().await, 0);
            });
        }

        #[test]
        fn async_connect_allocates_only_preparation_and_dispatch() {
            let Some(ndi) = crate::test_support::ndi() else {
                return;
            };
            let receiver = unconnected::<Tokio>(&ndi);
            let runtime = runtime();
            runtime.block_on(receiver.reconnect()).unwrap();
            for source in [
                Source::new("allocation target").unwrap(),
                Source::new("allocation target")
                    .unwrap()
                    .with_address(crate::SourceAddress::new("192.0.2.1:5960").unwrap()),
            ] {
                runtime.block_on(receiver.connect_to(&source)).unwrap();
                let ((), synchronous) =
                    crate::test_alloc::allocation_count(|| receiver.receiver().connect_to(&source));
                let (result, dispatch) =
                    crate::test_alloc::allocation_count(|| runtime.block_on(receiver.reconnect()));
                result.unwrap();
                let (result, asynchronous) = crate::test_alloc::allocation_count(|| {
                    runtime.block_on(receiver.connect_to(&source))
                });
                result.unwrap();
                assert_eq!(asynchronous, synchronous + dispatch, "source {source:?}");
            }
            assert!(matches!(
                Source::new("NUL\0target"),
                Err(Error::InvalidCString(_))
            ));
        }

        #[test]
        fn dropping_a_queued_connect_preserves_the_target() {
            let Some(ndi) = crate::test_support::ndi() else {
                return;
            };
            let receiver = unconnected::<Tokio>(&ndi);
            runtime().block_on(async {
                let initial = receiver.receiver().connection_target();
                let (release, blocker) = occupy_worker();
                let target = Source::new("cancelled target").unwrap();
                let mut connect = Box::pin(receiver.connect_to(&target));
                let mut context = Context::from_waker(Waker::noop());
                assert!(matches!(connect.as_mut().poll(&mut context), Poll::Pending));
                drop(connect);
                release.send(()).unwrap();
                blocker.await.unwrap();
                // A sentinel on the sole worker follows the cancelled closure.
                ::tokio::task::spawn_blocking(|| {}).await.unwrap();
                assert_eq!(receiver.receiver().connection_target(), initial);
            });
        }

        #[test]
        fn next_event_with_zero_timeout_makes_one_attempt_without_waiting() {
            let Some(ndi) = crate::test_support::ndi() else {
                return;
            };
            let receiver = unconnected::<Tokio>(&ndi);
            runtime().block_on(async {
                worker_capture_calls().await;
                let began = Instant::now();
                assert!(receiver.next_event(Duration::ZERO).await.unwrap().is_none());
                let waited = began.elapsed();
                assert!(waited < Duration::from_millis(500), "{waited:?}");
                assert_eq!(worker_capture_calls().await, 1);
            });
        }

        #[test]
        fn next_event_spends_blocking_pool_queue_delay_from_its_budget() {
            let Some(ndi) = crate::test_support::ndi() else {
                return;
            };
            let receiver = unconnected::<Tokio>(&ndi);
            runtime().block_on(async {
                worker_capture_calls().await;
                let (release, blocker) = occupy_worker();
                let budget = Duration::from_millis(300);
                let mut poll = Box::pin(receiver.next_event(budget));
                let mut context = Context::from_waker(Waker::noop());
                assert!(matches!(poll.as_mut().poll(&mut context), Poll::Pending));
                // Keep the one blocking thread until the whole budget is spent.
                std::thread::sleep(budget + Duration::from_millis(20));
                release.send(()).unwrap();
                assert!(poll.await.unwrap().is_none());
                blocker.await.unwrap();
                // Nothing was left: one attempt without waiting, where a budget
                // started on the blocking thread would have made three or more.
                assert_eq!(worker_capture_calls().await, 1);
            });
        }

        #[test]
        fn dropping_a_queued_next_event_makes_no_attempt() {
            let Some(ndi) = crate::test_support::ndi() else {
                return;
            };
            let receiver = unconnected::<Tokio>(&ndi);
            runtime().block_on(async {
                worker_capture_calls().await;
                let (release, blocker) = occupy_worker();
                let mut poll = Box::pin(receiver.next_event(Duration::from_secs(5)));
                let mut context = Context::from_waker(Waker::noop());
                assert!(matches!(poll.as_mut().poll(&mut context), Poll::Pending));
                drop(poll);
                release.send(()).unwrap();
                blocker.await.unwrap();
                // The one blocking thread runs the cancelled closure before
                // this count, and the closure stopped at its gate.
                assert_eq!(worker_capture_calls().await, 0);
            });
        }

        #[test]
        fn combined_capture_counts_queue_delay_and_skips_cancelled_queued_work() {
            let Some(ndi) = crate::test_support::ndi() else {
                return;
            };
            let receiver = unconnected::<Tokio>(&ndi);
            runtime().block_on(async {
                worker_capture_calls().await;
                let (release, blocker) = occupy_worker();
                let budget = Duration::from_millis(30);
                let mut capture = Box::pin(receiver.capture_any(budget));
                let mut context = Context::from_waker(Waker::noop());
                assert!(matches!(capture.as_mut().poll(&mut context), Poll::Pending));
                std::thread::sleep(budget + Duration::from_millis(10));
                release.send(()).unwrap();
                assert!(capture.await.unwrap().is_none());
                blocker.await.unwrap();
                let (calls, longest) =
                    ::tokio::task::spawn_blocking(native_capture_calls::take_with_longest_wait)
                        .await
                        .unwrap();
                assert_eq!((calls, longest), (1, 0));

                let (release, blocker) = occupy_worker();
                let mut capture = Box::pin(receiver.capture_any(Duration::from_secs(5)));
                assert!(matches!(capture.as_mut().poll(&mut context), Poll::Pending));
                drop(capture);
                release.send(()).unwrap();
                blocker.await.unwrap();
                assert_eq!(worker_capture_calls().await, 0);
            });
        }

        #[test]
        fn combined_capture_reaches_the_sdk_while_connection_changes_are_locked() {
            let Some(ndi) = crate::test_support::ndi() else {
                return;
            };
            let receiver = unconnected::<Tokio>(&ndi);
            let _exclusive = receiver.inner.core.reconnect_lock();
            runtime().block_on(async {
                worker_capture_calls().await;
                assert!(receiver
                    .capture_any(Duration::ZERO)
                    .await
                    .unwrap()
                    .is_none());
                assert_eq!(worker_capture_calls().await, 1);
                let began = Instant::now();
                assert!(receiver
                    .capture_any(Duration::from_millis(25))
                    .await
                    .unwrap()
                    .is_none());
                assert!(began.elapsed() >= Duration::from_millis(25));
                assert!(worker_capture_calls().await >= 1);
            });
        }
    }
}

#[cfg(test)]
mod scoped_tests;

#[cfg(test)]
mod source_name_cancellation_sdk_tests;
