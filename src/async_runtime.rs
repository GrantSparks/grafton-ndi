//! Async runtime integration for Tokio and async-std.
//!
//! This module provides async wrappers around the synchronous NDI receiver API,
//! allowing integration with async Rust applications using Tokio or async-std runtimes.
//!
//! The NDI SDK operations are inherently synchronous and blocking, so these wrappers
//! use `spawn_blocking` internally to run NDI operations on a thread pool without
//! blocking the async runtime.
//!
//! For reliable `capture_*` methods, the timeout budget starts when the async
//! method is called. The blocking task receives only the remaining budget when it
//! begins, so `spawn_blocking` queue delay does not expand the SDK wait budget.
//! Runtime scheduling can still make the awaited future complete after the timeout;
//! these wrappers do not use runtime-level cancellation because that would not stop
//! a queued or running blocking NDI call.
//!
//! # Features
//!
//! - `tokio` - Enable Tokio runtime support
//! - `async-std` - Enable async-std runtime support
//!
//! # Example with Tokio
//!
//! ```no_run
//! # #[cfg(feature = "tokio")]
//! # {
//! use grafton_ndi::{NDI, ReceiverOptionsBuilder, tokio::AsyncReceiver};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), grafton_ndi::Error> {
//!     let ndi = NDI::new()?;
//!     // ... obtain source from finder ...
//!     # let source = grafton_ndi::Source {
//!     #     name: "Test".into(),
//!     #     address: grafton_ndi::SourceAddress::None
//!     # };
//!
//!     let options = ReceiverOptionsBuilder::snapshot_preset(source).build();
//!     let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
//!
//!     let async_receiver = AsyncReceiver::new(receiver);
//!
//!     // Capture frame asynchronously without blocking the runtime
//!     let frame = async_receiver.capture_video(std::time::Duration::from_secs(5)).await?;
//!     println!("Captured {}x{} frame", frame.width(), frame.height());
//!
//!     Ok(())
//! }
//! # }
//! ```

use std::{
    future::Future,
    marker::PhantomData,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    frames::{AudioFrame, MetadataFrame, VideoFrame},
    to_ms_checked, ConnectionStats, Receiver, Result,
};

#[cfg(feature = "tokio")]
use crate::Error;

/// Trait for async runtime spawn-blocking abstraction.
///
/// This trait enables runtime-agnostic async code by abstracting the spawn-blocking
/// mechanism. Each runtime (Tokio, async-std) provides its own implementation.
///
/// The trait is sealed to prevent external implementations and ensure consistent
/// error handling across all supported runtimes.
pub trait SpawnBlocking: sealed::Sealed + Clone + Send + Sync + 'static {
    /// Spawns a blocking operation on a thread pool and returns its result.
    ///
    /// # Errors
    ///
    /// Returns `Err(Error::SpawnFailed)` if the blocking task panics or is cancelled.
    fn spawn_blocking<F, R>(f: F) -> impl Future<Output = Result<R>> + Send
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static;
}

mod sealed {
    pub trait Sealed {}

    #[cfg(feature = "tokio")]
    impl Sealed for super::TokioRuntime {}

    #[cfg(feature = "async-std")]
    impl Sealed for super::AsyncStdRuntime {}
}

/// Tokio async runtime marker type.
///
/// Used as a type parameter for [`AsyncReceiverGeneric`] to select Tokio's
/// `spawn_blocking` implementation.
#[cfg(feature = "tokio")]
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioRuntime;

#[cfg(feature = "tokio")]
impl SpawnBlocking for TokioRuntime {
    // Using `impl Future` instead of `async fn` in trait because we need explicit
    // Send bounds on the returned future. This pattern is intentional.
    #[allow(clippy::manual_async_fn)]
    fn spawn_blocking<F, R>(f: F) -> impl Future<Output = Result<R>> + Send
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        async {
            ::tokio::task::spawn_blocking(f)
                .await
                .map_err(|e| Error::SpawnFailed(e.to_string()))
        }
    }
}

/// async-std runtime marker type.
///
/// Used as a type parameter for [`AsyncReceiverGeneric`] to select async-std's
/// `spawn_blocking` implementation.
#[cfg(feature = "async-std")]
#[derive(Clone, Copy, Debug, Default)]
pub struct AsyncStdRuntime;

#[cfg(feature = "async-std")]
impl SpawnBlocking for AsyncStdRuntime {
    // Using `impl Future` instead of `async fn` in trait because we need explicit
    // Send bounds on the returned future. This pattern is intentional.
    #[allow(clippy::manual_async_fn)]
    fn spawn_blocking<F, R>(f: F) -> impl Future<Output = Result<R>> + Send
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        async { Ok(::async_std::task::spawn_blocking(f).await) }
    }
}

/// Generic async receiver wrapper parameterized by runtime.
///
/// This struct provides async versions of the [`Receiver`] methods by running
/// blocking NDI operations on the runtime's thread pool using `spawn_blocking`.
///
/// # Type Parameters
///
/// - `R`: The async runtime type, implementing [`SpawnBlocking`]. Use
///   [`TokioRuntime`] or [`AsyncStdRuntime`].
///
/// # Thread Safety
///
/// The underlying [`Receiver`] is wrapped in an [`Arc`] to allow sharing across
/// async tasks and safe cloning. The NDI SDK receiver is inherently thread-safe.
///
/// # Example
///
/// ```no_run
/// # #[cfg(feature = "tokio")]
/// # {
/// use grafton_ndi::{NDI, ReceiverOptionsBuilder, tokio::AsyncReceiver};
///
/// #[tokio::main]
/// async fn main() -> Result<(), grafton_ndi::Error> {
///     let ndi = NDI::new()?;
///     // ... obtain source ...
///     # let source = grafton_ndi::Source {
///     #     name: "Test".into(),
///     #     address: grafton_ndi::SourceAddress::None
///     # };
///
///     let options = ReceiverOptionsBuilder::snapshot_preset(source).build();
///     let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
///     let async_receiver = AsyncReceiver::new(receiver);
///
///     // Non-blocking async capture
///     match async_receiver.capture_video_timeout(std::time::Duration::from_millis(100)).await? {
///         Some(frame) => println!("Got frame: {}x{}", frame.width(), frame.height()),
///         None => println!("No frame available"),
///     }
///
///     Ok(())
/// }
/// # }
/// ```
pub struct AsyncReceiverGeneric<R: SpawnBlocking> {
    inner: Arc<Receiver>,
    _runtime: PhantomData<R>,
}

fn validated_timeout_start(timeout: Duration) -> Result<Instant> {
    to_ms_checked(timeout)?;
    Ok(Instant::now())
}

fn remaining_timeout(timeout: Duration, start_time: Instant) -> Duration {
    timeout.saturating_sub(start_time.elapsed())
}

impl<R: SpawnBlocking> AsyncReceiverGeneric<R> {
    /// Create a new async receiver wrapper.
    ///
    /// The receiver is wrapped in an [`Arc`] to allow sharing across async tasks.
    pub fn new(receiver: Receiver) -> Self {
        Self {
            inner: Arc::new(receiver),
            _runtime: PhantomData,
        }
    }

    /// Async version of [`Receiver::capture_video`].
    ///
    /// Captures a video frame, blocking until received or timeout expires, without blocking
    /// the async runtime. Uses the runtime's `spawn_blocking` internally.
    ///
    /// This is the **primary method** for reliable video frame capture in async contexts.
    /// It handles retries automatically to work around NDI SDK synchronization behavior.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Total budget to wait for a frame, starting when this async
    ///   method is called. Blocking task queue delay is subtracted before the
    ///   synchronous receiver starts waiting. Must not exceed
    ///   [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// * `Ok(frame)` - Successfully captured a video frame
    /// * `Err(Error::FrameTimeout)` - No frame received within timeout (includes retry details)
    /// * `Err(Error::SpawnFailed)` - The blocking task panicked or was cancelled
    /// * `Err(_)` - Another error occurred during capture
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "tokio")]
    /// # {
    /// # use grafton_ndi::{NDI, ReceiverOptions, tokio::AsyncReceiver};
    /// # use std::time::Duration;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = grafton_ndi::Source {
    /// #     name: "Test".into(),
    /// #     address: grafton_ndi::SourceAddress::None
    /// # };
    /// # let options = ReceiverOptions::builder(source).build();
    /// # let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
    /// let async_receiver = AsyncReceiver::new(receiver);
    /// let frame = async_receiver.capture_video(Duration::from_secs(5)).await?;
    /// println!("Captured {}x{} frame", frame.width(), frame.height());
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    pub async fn capture_video(&self, timeout: Duration) -> Result<VideoFrame> {
        let start_time = validated_timeout_start(timeout)?;
        let receiver = Arc::clone(&self.inner);
        R::spawn_blocking(move || receiver.capture_video(remaining_timeout(timeout, start_time)))
            .await?
    }

    /// Async version of [`Receiver::capture_video_timeout`].
    ///
    /// Attempts to capture a video frame with a timeout (polling variant).
    /// May return `None` if no frame is available within the timeout.
    ///
    /// **For most use cases, prefer [`Self::capture_video`]** which handles retries
    /// automatically and provides reliable frame capture.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for a frame.
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// * `Ok(Some(frame))` - Successfully captured a video frame
    /// * `Ok(None)` - No frame available within timeout
    /// * `Err(Error::SpawnFailed)` - The blocking task panicked or was cancelled
    /// * `Err(_)` - An error occurred during capture
    pub async fn capture_video_timeout(&self, timeout: Duration) -> Result<Option<VideoFrame>> {
        let receiver = Arc::clone(&self.inner);
        R::spawn_blocking(move || receiver.capture_video_timeout(timeout)).await?
    }

    /// Async version of [`Receiver::capture_audio`].
    ///
    /// Captures an audio frame, blocking until received or timeout expires, without blocking
    /// the async runtime.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Total budget to wait for a frame, starting when this async
    ///   method is called. Blocking task queue delay is subtracted before the
    ///   synchronous receiver starts waiting. Must not exceed
    ///   [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// * `Ok(frame)` - Successfully captured an audio frame
    /// * `Err(Error::FrameTimeout)` - No frame received within timeout (includes retry details)
    /// * `Err(Error::SpawnFailed)` - The blocking task panicked or was cancelled
    /// * `Err(_)` - An error occurred during capture
    pub async fn capture_audio(&self, timeout: Duration) -> Result<AudioFrame> {
        let start_time = validated_timeout_start(timeout)?;
        let receiver = Arc::clone(&self.inner);
        R::spawn_blocking(move || receiver.capture_audio(remaining_timeout(timeout, start_time)))
            .await?
    }

    /// Async version of [`Receiver::capture_audio_timeout`].
    ///
    /// Attempts to capture an audio frame with a timeout (polling variant).
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for a frame.
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// * `Ok(Some(frame))` - Successfully captured an audio frame
    /// * `Ok(None)` - No frame available within timeout
    /// * `Err(Error::SpawnFailed)` - The blocking task panicked or was cancelled
    /// * `Err(_)` - An error occurred during capture
    pub async fn capture_audio_timeout(&self, timeout: Duration) -> Result<Option<AudioFrame>> {
        let receiver = Arc::clone(&self.inner);
        R::spawn_blocking(move || receiver.capture_audio_timeout(timeout)).await?
    }

    /// Async version of [`Receiver::capture_metadata`].
    ///
    /// Captures a metadata frame, blocking until received or timeout expires, without blocking
    /// the async runtime.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Total budget to wait for a frame, starting when this async
    ///   method is called. Blocking task queue delay is subtracted before the
    ///   synchronous receiver starts waiting. Must not exceed
    ///   [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// * `Ok(frame)` - Successfully captured a metadata frame
    /// * `Err(Error::FrameTimeout)` - No frame received within timeout (includes retry details)
    /// * `Err(Error::SpawnFailed)` - The blocking task panicked or was cancelled
    /// * `Err(_)` - An error occurred during capture
    pub async fn capture_metadata(&self, timeout: Duration) -> Result<MetadataFrame> {
        let start_time = validated_timeout_start(timeout)?;
        let receiver = Arc::clone(&self.inner);
        R::spawn_blocking(move || receiver.capture_metadata(remaining_timeout(timeout, start_time)))
            .await?
    }

    /// Async version of [`Receiver::capture_metadata_timeout`].
    ///
    /// Attempts to capture a metadata frame with a timeout (polling variant).
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for a frame.
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// * `Ok(Some(frame))` - Successfully captured a metadata frame
    /// * `Ok(None)` - No frame available within timeout
    /// * `Err(Error::SpawnFailed)` - The blocking task panicked or was cancelled
    /// * `Err(_)` - An error occurred during capture
    pub async fn capture_metadata_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<MetadataFrame>> {
        let receiver = Arc::clone(&self.inner);
        R::spawn_blocking(move || receiver.capture_metadata_timeout(timeout)).await?
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
    /// the blocking pool. `connection_stats().video_frames_received` is
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
    /// capture guard exclusively, so it can block until in-flight captures
    /// (which run on the blocking pool) drain. It therefore runs on the
    /// blocking pool too, never on the async runtime's threads, and is safe to
    /// call while captures on this receiver are in flight — they serialize
    /// instead of racing. Confirm recovery via [`Self::connection_stats`].
    pub async fn reconnect(&self) -> Result<()> {
        let receiver = Arc::clone(&self.inner);
        R::spawn_blocking(move || receiver.reconnect()).await?
    }
}

impl<R: SpawnBlocking> Clone for AsyncReceiverGeneric<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            _runtime: PhantomData,
        }
    }
}

// Backward-compatible module re-exports

#[cfg(feature = "tokio")]
pub mod tokio {
    //! Tokio async runtime integration.
    //!
    //! Provides [`AsyncReceiver`] wrapper that uses `tokio::task::spawn_blocking`
    //! to run NDI operations without blocking the Tokio runtime.
    //!
    //! # Example
    //!
    //! ```no_run
    //! # #[cfg(feature = "tokio")]
    //! # {
    //! use grafton_ndi::{NDI, ReceiverOptionsBuilder, tokio::AsyncReceiver};
    //!
    //! #[tokio::main]
    //! async fn main() -> Result<(), grafton_ndi::Error> {
    //!     let ndi = NDI::new()?;
    //!     // ... obtain source ...
    //!     # let source = grafton_ndi::Source {
    //!     #     name: "Test".into(),
    //!     #     address: grafton_ndi::SourceAddress::None
    //!     # };
    //!
    //!     let options = ReceiverOptionsBuilder::snapshot_preset(source).build();
    //!     let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
    //!     let async_receiver = AsyncReceiver::new(receiver);
    //!
    //!     // Non-blocking async capture
    //!     match async_receiver.capture_video_timeout(std::time::Duration::from_millis(100)).await? {
    //!         Some(frame) => println!("Got frame: {}x{}", frame.width(), frame.height()),
    //!         None => println!("No frame available"),
    //!     }
    //!
    //!     Ok(())
    //! }
    //! # }
    //! ```

    use super::{AsyncReceiverGeneric, TokioRuntime};

    /// Async receiver wrapper for Tokio runtime.
    ///
    /// This is a type alias for the generic async receiver parameterized with
    /// the Tokio runtime. It provides async versions of the [`crate::Receiver`]
    /// methods by running blocking NDI operations on Tokio's blocking thread
    /// pool using `spawn_blocking`.
    ///
    /// # Thread Safety
    ///
    /// The underlying `Receiver` is wrapped in an `Arc` to allow sharing across
    /// async tasks and safe cloning. The NDI SDK receiver is inherently thread-safe.
    ///
    /// # Error Handling
    ///
    /// All methods return [`crate::Result`], converting any task panic or cancellation
    /// into [`crate::Error::SpawnFailed`] rather than propagating the panic.
    pub type AsyncReceiver = AsyncReceiverGeneric<TokioRuntime>;
}

#[cfg(feature = "async-std")]
pub mod async_std {
    //! async-std runtime integration.
    //!
    //! Provides [`AsyncReceiver`] wrapper that uses `async_std::task::spawn_blocking`
    //! to run NDI operations without blocking the async-std runtime.
    //!
    //! # Example
    //!
    //! ```no_run
    //! # #[cfg(feature = "async-std")]
    //! # {
    //! use grafton_ndi::{NDI, ReceiverOptionsBuilder, async_std::AsyncReceiver};
    //!
    //! #[async_std::main]
    //! async fn main() -> Result<(), grafton_ndi::Error> {
    //!     let ndi = NDI::new()?;
    //!     // ... obtain source ...
    //!     # let source = grafton_ndi::Source {
    //!     #     name: "Test".into(),
    //!     #     address: grafton_ndi::SourceAddress::None
    //!     # };
    //!
    //!     let options = ReceiverOptionsBuilder::snapshot_preset(source).build();
    //!     let receiver = grafton_ndi::Receiver::new(&ndi, &options)?;
    //!     let async_receiver = AsyncReceiver::new(receiver);
    //!
    //!     // Non-blocking async capture
    //!     match async_receiver.capture_video_timeout(std::time::Duration::from_millis(100)).await? {
    //!         Some(frame) => println!("Got frame: {}x{}", frame.width(), frame.height()),
    //!         None => println!("No frame available"),
    //!     }
    //!
    //!     Ok(())
    //! }
    //! # }
    //! ```

    use super::{AsyncReceiverGeneric, AsyncStdRuntime};

    /// Async receiver wrapper for async-std runtime.
    ///
    /// This is a type alias for the generic async receiver parameterized with
    /// the async-std runtime. It provides async versions of the [`crate::Receiver`]
    /// methods by running blocking NDI operations on async-std's blocking thread
    /// pool using `spawn_blocking`.
    ///
    /// # Thread Safety
    ///
    /// The underlying `Receiver` is wrapped in an `Arc` to allow sharing across
    /// async tasks and safe cloning. The NDI SDK receiver is inherently thread-safe.
    ///
    /// # Error Handling
    ///
    /// All methods return [`crate::Result`]. Note that async-std's `spawn_blocking`
    /// does not return a `Result`, so spawn failures from this runtime are less
    /// common than with Tokio.
    pub type AsyncReceiver = AsyncReceiverGeneric<AsyncStdRuntime>;
}
