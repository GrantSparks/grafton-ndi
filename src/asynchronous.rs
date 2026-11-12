//! Async receivers for Tokio and smol.
//!
//! [`AsyncReceiver`] wraps the synchronous NDI receiver API for an async
//! runtime, and its `video`, `audio` and `metadata` return an
//! [`AsyncCapture`] view that captures one kind of frame. Both are generic
//! over the [`AsyncRuntime`] whose blocking thread pool runs the NDI operations:
//! [`Tokio`] with the `tokio` Cargo feature, [`Smol`] with the `smol` feature.
//! The [`tokio`] and [`smol`] submodules name both types for one runtime, for
//! example `grafton_ndi::asynchronous::tokio::AsyncReceiver`. This module
//! exists when either feature is enabled, and each runtime only with its own
//! feature; the crate root does not re-export any of it.
//!
//! The NDI SDK operations are inherently synchronous and blocking, so these wrappers
//! use `spawn_blocking` internally to run NDI operations on a thread pool without
//! blocking the async runtime.
//!
//! For `capture`, the timeout budget starts when the future is first polled,
//! rather than when it is constructed. The same deadline covers blocking-pool
//! queue delay, receiver capture-gate contention, and SDK waits.
//! This is a wait budget, not a hard completion deadline: scheduling, frame copying,
//! and other processing can delay the result beyond it.
//!
//! Dropping a future sets a cancellation flag. Work is skipped if the blocking
//! closure observes that flag at its entry gate. After passing that gate, work
//! continues, including waiting for receiver access; it cannot be interrupted by
//! dropping the future.
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
//! use grafton_ndi::{Ndi, ReceiverOptions, asynchronous::tokio::AsyncReceiver};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), grafton_ndi::Error> {
//!     let ndi = Ndi::new()?;
//!     // ... obtain source from finder ...
//!     # let source = grafton_ndi::Source::new("CAMERA (Program)");
//!
//!     let options = ReceiverOptions::snapshot_preset(source);
//!     let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
//!
//!     let async_receiver = AsyncReceiver::new(receiver);
//!
//!     // Capture frame asynchronously without blocking the runtime
//!     if let Some(frame) = async_receiver.video().capture(std::time::Duration::from_secs(5)).await? {
//!         println!("Captured {}x{} frame", frame.width(), frame.height());
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
    timeout::Deadline,
    ConnectionStats, Error, Receiver, Result,
};

mod private {
    use std::future::Future;

    use crate::Result;

    /// Proof that the caller is this crate.
    ///
    /// A bound on [`AsyncRuntime`](super::AsyncRuntime) makes [`Spawn::spawn_blocking`]
    /// reachable from other crates even though they cannot name [`Spawn`];
    /// they cannot name this type either, so they cannot call it.
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
        /// cannot be submitted, is cancelled before it starts, or panics.
        fn spawn_blocking<F, T>(token: Token, f: F) -> impl Future<Output = Result<T>> + Send
        where
            F: FnOnce() -> T + Send + 'static,
            T: Send + 'static;
    }
}

// Dropping a future prevents its queued closure from starting SDK work. Work
// that has already passed this gate cannot be interrupted safely.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn blocking_operation<F, T>(operation: F) -> (CancelOnDrop, impl FnOnce() -> Result<T> + Send)
where
    F: FnOnce() -> T + Send,
{
    let cancelled = Arc::new(AtomicBool::new(false));
    let guard = CancelOnDrop(Arc::clone(&cancelled));
    let work = move || {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::SpawnFailed(
                "Blocking operation cancelled before starting".into(),
            ));
        }
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)).map_err(|payload| {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic payload");
            Error::SpawnFailed(format!("Blocking operation panicked: {message}"))
        })
    };
    (guard, work)
}

/// An async runtime whose blocking thread pool runs the NDI operations of an
/// [`AsyncReceiver`] and its [`AsyncCapture`] views: [`Tokio`] or [`Smol`].
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
    fn spawn_blocking<F, T>(_: private::Token, f: F) -> impl Future<Output = Result<T>> + Send
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        async {
            let handle = ::tokio::runtime::Handle::try_current()
                .map_err(|e| Error::SpawnFailed(e.to_string()))?;
            let (_cancellation, work) = blocking_operation(f);
            handle
                .spawn_blocking(work)
                .await
                .map_err(|e| Error::SpawnFailed(e.to_string()))?
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
    fn spawn_blocking<F, T>(_: private::Token, f: F) -> impl Future<Output = Result<T>> + Send
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        async {
            let (_cancellation, work) = blocking_operation(f);
            ::smol::unblock(work).await
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
/// The underlying [`Receiver`] is wrapped in an [`Arc`] to allow sharing across
/// async tasks and safe cloning. The NDI SDK receiver is inherently thread-safe.
///
/// # Error Handling
///
/// The methods that run on the blocking pool convert a panic there, or the
/// cancellation of the blocking task, into [`Error::SpawnFailed`] rather than
/// propagating it.
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
///     # let source = grafton_ndi::Source::new("CAMERA (Program)");
///
///     let options = ReceiverOptions::snapshot_preset(source);
///     let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
///     let async_receiver = AsyncReceiver::new(receiver);
///
///     // Capture without blocking the runtime
///     match async_receiver.video().capture(std::time::Duration::from_millis(100)).await? {
///         Some(frame) => println!("Got frame: {}x{}", frame.width(), frame.height()),
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

// Starts the budget on the future's first poll, so that blocking-pool queue
// delay is spent from it.
async fn with_timeout_budget<R, F, T>(timeout: Duration, operation: F) -> Result<T>
where
    R: AsyncRuntime,
    F: FnOnce(Deadline) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let deadline = Deadline::after(timeout)?;
    R::spawn_blocking(private::Token, move || operation(deadline)).await?
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

    /// Capture **video** frames without blocking the async runtime.
    ///
    /// Returns an [`AsyncCapture`] view; see [`AsyncCapture::capture`].
    /// Mirrors [`Receiver::video`], running each capture on the blocking pool.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "tokio")]
    /// # {
    /// # use grafton_ndi::{Ndi, ReceiverOptions, asynchronous::tokio::AsyncReceiver};
    /// # use std::time::Duration;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let source = grafton_ndi::Source::new("CAMERA (Program)");
    /// # let receiver = grafton_ndi::Receiver::new(&ndi, &ReceiverOptions::new(source))?;
    /// let async_receiver = AsyncReceiver::new(receiver);
    /// if let Some(frame) = async_receiver.video().capture(Duration::from_secs(5)).await? {
    ///     println!("Captured {}x{} frame", frame.width(), frame.height());
    /// }
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    #[must_use = "the AsyncCapture view does nothing until a capture verb is awaited"]
    pub fn video(&self) -> AsyncCapture<'_, R, VideoKind> {
        AsyncCapture::new(self)
    }

    /// Capture **audio** frames without blocking the async runtime.
    ///
    /// Returns an [`AsyncCapture`] view; see [`video`](Self::video) for usage.
    /// Mirrors [`Receiver::audio`].
    #[must_use = "the AsyncCapture view does nothing until a capture verb is awaited"]
    pub fn audio(&self) -> AsyncCapture<'_, R, AudioKind> {
        AsyncCapture::new(self)
    }

    /// Capture **metadata** frames without blocking the async runtime.
    ///
    /// Returns an [`AsyncCapture`] view; see [`video`](Self::video) for usage.
    /// Mirrors [`Receiver::metadata`].
    #[must_use = "the AsyncCapture view does nothing until a capture verb is awaited"]
    pub fn metadata(&self) -> AsyncCapture<'_, R, MetadataKind> {
        AsyncCapture::new(self)
    }

    /// Whether the underlying receiver currently has at least one active
    /// connection to its source. See [`Receiver::is_connected`].
    ///
    /// A cheap, non-blocking SDK query, so it runs inline rather than on
    /// the blocking pool.
    pub fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    /// Connection and frame-throughput statistics for the underlying
    /// receiver. See [`Receiver::connection_stats`].
    ///
    /// A cheap, non-blocking SDK query, so it runs inline rather than on
    /// the blocking pool. `connection_stats().video_frames_received()` is
    /// the canonical liveness signal: it advances as the receiver pulls
    /// frames off the network, independent of how often the caller
    /// captures, so a frozen counter means the feed itself has stalled.
    pub fn connection_stats(&self) -> ConnectionStats {
        self.inner.connection_stats()
    }

    /// Re-establish the underlying receiver's connection to its source
    /// in place. See [`Receiver::reconnect`].
    ///
    /// Unlike the liveness probes above, a reconnect takes the receiver's
    /// capture guard exclusively, so it blocks until the capture attempts in
    /// flight (which run on the blocking pool) return: one attempt of at most
    /// 100 ms each, whatever their timeouts. It therefore runs on the blocking
    /// pool too, never on the async runtime's threads, and is safe to call
    /// while captures on this receiver are in flight — they serialize instead
    /// of racing. Confirm recovery via [`Self::connection_stats`].
    ///
    /// # Errors
    ///
    /// The errors of [`Receiver::reconnect`], and [`Error::SpawnFailed`] when
    /// the blocking task panicked or was cancelled.
    ///
    /// # Cancellation
    ///
    /// Dropping this future skips work if the blocking closure observes the
    /// cancellation flag at its entry gate. After passing that gate, reconnect
    /// continues, including waiting for in-flight captures, and may reconnect
    /// after this future is dropped.
    pub async fn reconnect(&self) -> Result<()> {
        let receiver = Arc::clone(&self.inner);
        R::spawn_blocking(private::Token, move || receiver.reconnect()).await?
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

/// A typed view over an async receiver for capturing frames of one kind,
/// without blocking the async runtime.
///
/// Created by [`AsyncReceiver::video`], [`audio`](AsyncReceiver::audio),
/// and [`metadata`](AsyncReceiver::metadata). Its
/// [`capture`](Self::capture) runs the synchronous
/// [`Capture::capture`](crate::Capture::capture) on the runtime's blocking
/// pool, with the same semantics, and its timeout budget also covers the
/// time the work waits for a blocking-pool thread.
///
/// There is no zero-copy `capture_ref` here: a borrowed frame is tied to
/// the receiver and cannot cross the `spawn_blocking` boundary. Use
/// [`Capture::capture_ref`](crate::Capture::capture_ref) on the synchronous
/// receiver for in-place processing.
pub struct AsyncCapture<'rx, R: AsyncRuntime, K: CaptureKind> {
    recv: &'rx AsyncReceiver<R>,
    _kind: PhantomData<K>,
}

impl<'rx, R: AsyncRuntime, K: CaptureKind> AsyncCapture<'rx, R, K>
where
    K::Owned: Send + 'static,
{
    fn new(recv: &'rx AsyncReceiver<R>) -> Self {
        Self {
            recv,
            _kind: PhantomData,
        }
    }

    /// Async version of [`Capture::capture`](crate::Capture::capture): wait
    /// up to `timeout` for a frame and return an owned copy of it, or `None`
    /// when none arrived in time, running on the blocking pool.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Total budget to wait for a frame, starting when the future
    ///   is first polled. Blocking-pool queue delay and capture-gate contention
    ///   count against the same budget, and `None` comes only once all of it
    ///   is spent. Completion can occur after this budget because scheduling
    ///   and frame processing are not bounded by it. Must not exceed
    ///   [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Errors
    ///
    /// The errors of [`Capture::capture`](crate::Capture::capture), and
    /// [`Error::SpawnFailed`] when the blocking task panicked or was
    /// cancelled.
    ///
    /// # Cancellation
    ///
    /// Dropping this future skips work if the blocking closure observes the
    /// cancellation flag at its entry gate. After passing that gate, work
    /// continues, including waiting for receiver access, and can consume a frame
    /// whose result is discarded.
    pub async fn capture(&self, timeout: Duration) -> Result<Option<K::Owned>> {
        let receiver = Arc::clone(&self.recv.inner);
        with_timeout_budget::<R, _, _>(timeout, move |deadline| {
            receiver.core.capture_until::<K>(deadline)
        })
        .await
    }
}

#[cfg(feature = "tokio")]
pub mod tokio {
    //! [`AsyncReceiver`] and [`AsyncCapture`] on the Tokio runtime: the
    //! generic [`asynchronous::AsyncReceiver`](super::AsyncReceiver) and
    //! [`asynchronous::AsyncCapture`](super::AsyncCapture) with
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
    //!     # let source = grafton_ndi::Source::new("CAMERA (Program)");
    //!
    //!     let options = ReceiverOptions::snapshot_preset(source);
    //!     let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
    //!     let async_receiver = AsyncReceiver::new(receiver);
    //!
    //!     // Capture without blocking the runtime
    //!     match async_receiver.video().capture(std::time::Duration::from_millis(100)).await? {
    //!         Some(frame) => println!("Got frame: {}x{}", frame.width(), frame.height()),
    //!         None => println!("No frame available"),
    //!     }
    //!
    //!     Ok(())
    //! }
    //! ```

    /// An [`AsyncReceiver`](super::AsyncReceiver) on the Tokio runtime.
    pub type AsyncReceiver = super::AsyncReceiver<super::Tokio>;
    /// An [`AsyncCapture`](super::AsyncCapture) on the Tokio runtime.
    pub type AsyncCapture<'rx, K> = super::AsyncCapture<'rx, super::Tokio, K>;
}

#[cfg(feature = "smol")]
pub mod smol {
    //! [`AsyncReceiver`] and [`AsyncCapture`] on the smol runtime: the
    //! generic [`asynchronous::AsyncReceiver`](super::AsyncReceiver) and
    //! [`asynchronous::AsyncCapture`](super::AsyncCapture) with
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
    //!         # let source = grafton_ndi::Source::new("CAMERA (Program)");
    //!
    //!         let options = ReceiverOptions::snapshot_preset(source);
    //!         let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
    //!         let async_receiver = AsyncReceiver::new(receiver);
    //!
    //!         // Capture without blocking the runtime
    //!         match async_receiver.video().capture(std::time::Duration::from_millis(100)).await? {
    //!             Some(frame) => println!("Got frame: {}x{}", frame.width(), frame.height()),
    //!             None => println!("No frame available"),
    //!         }
    //!
    //!         Ok(())
    //!     })
    //! }
    //! ```

    /// An [`AsyncReceiver`](super::AsyncReceiver) on the smol runtime.
    pub type AsyncReceiver = super::AsyncReceiver<super::Smol>;
    /// An [`AsyncCapture`](super::AsyncCapture) on the smol runtime.
    pub type AsyncCapture<'rx, K> = super::AsyncCapture<'rx, super::Smol, K>;
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
            let mut capture = Box::pin(with_timeout_budget::<Tokio, _, _>(budget, |deadline| {
                Ok((
                    deadline.remaining(),
                    deadline.remaining_ms(),
                    deadline.elapsed(),
                ))
            }));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(capture.as_mut().poll(&mut context), Poll::Pending));
            // Keep the sole worker occupied until the whole budget has elapsed.
            std::thread::sleep(budget + Duration::from_millis(5));
            release_tx.send(()).unwrap();
            let (remaining, remaining_ms, elapsed) = capture.await.unwrap();
            assert_eq!((remaining, remaining_ms), (Duration::ZERO, 0));
            // The queue delay is part of the time the budget reports as spent.
            assert!(elapsed >= budget + Duration::from_millis(5), "{elapsed:?}");
            blocker.await.unwrap();
        });
    }

    #[test]
    fn invalid_budget_is_rejected_before_spawning() {
        // No entered runtime: trying to submit work would panic.
        let mut capture = Box::pin(with_timeout_budget::<Tokio, _, ()>(
            crate::MAX_TIMEOUT + Duration::from_nanos(1),
            |_| panic!("invalid timeout must not execute blocking work"),
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
            move |_| {
                worker_called.store(true, Ordering::Release);
                Ok(())
            },
        ));
        assert!(result.is_err());
        assert!(!called.load(Ordering::Acquire));
    }

    #[test]
    fn budget_starts_on_first_poll_and_keeps_one_deadline() {
        let budget = Duration::from_millis(20);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut operation = Box::pin(with_timeout_budget::<Smol, _, _>(
            budget,
            move |deadline: Deadline| {
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
        assert!(matches!(work(), Err(Error::SpawnFailed(_))));
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
            Poll::Ready(Err(Error::SpawnFailed(_)))
        ));
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn tokio_converts_blocking_panic() {
        let runtime = ::tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let result = runtime.block_on(Tokio::spawn_blocking(Token, || panic!("test panic")));
        assert!(
            matches!(result, Err(Error::SpawnFailed(message)) if message.contains("test panic"))
        );
    }

    #[cfg(feature = "smol")]
    #[test]
    fn smol_converts_blocking_panic() {
        let result = ::smol::block_on(Smol::spawn_blocking(Token, || panic!("test panic")));
        assert!(
            matches!(result, Err(Error::SpawnFailed(message)) if message.contains("test panic"))
        );
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
