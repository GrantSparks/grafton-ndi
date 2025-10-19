//! Async runtime integration for Tokio and async-std.
//!
//! This module provides async wrappers around the synchronous NDI receiver API,
//! allowing integration with async Rust applications using Tokio or async-std runtimes.
//!
//! The NDI SDK operations are inherently synchronous and blocking, so these wrappers
//! use `spawn_blocking` internally to run NDI operations on a thread pool without
//! blocking the async runtime.
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
//!     let receiver = ReceiverOptionsBuilder::snapshot_preset(source)
//!         .build(&ndi)?;
//!
//!     let async_receiver = AsyncReceiver::new(receiver);
//!
//!     // Capture frame asynchronously without blocking the runtime
//!     let frame = async_receiver.capture_video_blocking(5000).await?;
//!     println!("Captured {}x{} frame", frame.width, frame.height);
//!
//!     Ok(())
//! }
//! # }
//! ```

use std::sync::Arc;

use crate::{
    frames::{AudioFrame, MetadataFrame, VideoFrame},
    Receiver, Result,
};

#[cfg(feature = "tokio")]
pub mod tokio {
    //! Tokio async runtime integration.
    //!
    //! Provides `AsyncReceiver` wrapper that uses `tokio::task::spawn_blocking`
    //! to run NDI operations without blocking the Tokio runtime.

    use super::*;

    /// Async receiver wrapper for Tokio runtime.
    ///
    /// This wrapper provides async versions of the `Receiver` methods by running
    /// blocking NDI operations on Tokio's blocking thread pool using `spawn_blocking`.
    ///
    /// # Thread Safety
    ///
    /// The underlying `Receiver` is wrapped in an `Arc` to allow sharing across
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
    ///     let receiver = ReceiverOptionsBuilder::snapshot_preset(source).build(&ndi)?;
    ///     let async_receiver = AsyncReceiver::new(receiver);
    ///
    ///     // Non-blocking async capture
    ///     match async_receiver.capture_video(100).await? {
    ///         Some(frame) => println!("Got frame: {}x{}", frame.width, frame.height),
    ///         None => println!("No frame available"),
    ///     }
    ///
    ///     Ok(())
    /// }
    /// # }
    /// ```
    pub struct AsyncReceiver {
        inner: Arc<Receiver>,
    }

    impl AsyncReceiver {
        /// Create a new async receiver wrapper.
        ///
        /// The receiver is wrapped in an `Arc` to allow sharing across async tasks.
        pub fn new(receiver: Receiver) -> Self {
            Self {
                inner: Arc::new(receiver),
            }
        }

        /// Async version of `Receiver::capture_video`.
        ///
        /// Attempts to capture a video frame without blocking the async runtime.
        /// Uses `tokio::task::spawn_blocking` internally.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout in milliseconds for the capture attempt
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured a video frame
        /// * `Ok(None)` - No frame available within timeout
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_video(&self, timeout_ms: u32) -> Result<Option<VideoFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::tokio::task::spawn_blocking(move || receiver.capture_video(timeout_ms))
                .await
                .expect("Blocking task panicked")
        }

        /// Async version of `Receiver::capture_video_with_retry`.
        ///
        /// Captures video with automatic retry logic without blocking the async runtime.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout for each capture attempt in milliseconds
        /// * `max_attempts` - Maximum number of retry attempts
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured a video frame
        /// * `Ok(None)` - No frame available after all retry attempts
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_video_with_retry(
            &self,
            timeout_ms: u32,
            max_attempts: usize,
        ) -> Result<Option<VideoFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::tokio::task::spawn_blocking(move || {
                receiver.capture_video_with_retry(timeout_ms, max_attempts)
            })
            .await
            .expect("Blocking task panicked")
        }

        /// Async version of `Receiver::capture_video_blocking`.
        ///
        /// Blocks until a frame is received or timeout expires, without blocking
        /// the async runtime. This is the recommended method for reliable frame capture.
        ///
        /// # Arguments
        ///
        /// * `total_timeout_ms` - Total time to wait for a frame in milliseconds
        ///
        /// # Returns
        ///
        /// * `Ok(frame)` - Successfully captured a video frame
        /// * `Err(Error::FrameTimeout)` - No frame received within timeout (includes retry details)
        /// * `Err(_)` - Another error occurred during capture
        ///
        /// # Example
        ///
        /// ```no_run
        /// # #[cfg(feature = "tokio")]
        /// # {
        /// # use grafton_ndi::{NDI, ReceiverOptionsBuilder, tokio::AsyncReceiver};
        /// # #[tokio::main]
        /// # async fn main() -> Result<(), grafton_ndi::Error> {
        /// # let ndi = NDI::new()?;
        /// # let source = grafton_ndi::Source {
        /// #     name: "Test".into(),
        /// #     address: grafton_ndi::SourceAddress::None
        /// # };
        /// # let receiver = ReceiverOptionsBuilder::snapshot_preset(source).build(&ndi)?;
        /// let async_receiver = AsyncReceiver::new(receiver);
        /// let frame = async_receiver.capture_video_blocking(5000).await?;
        /// println!("Captured {}x{} frame", frame.width, frame.height);
        /// # Ok(())
        /// # }
        /// # }
        /// ```
        pub async fn capture_video_blocking(&self, total_timeout_ms: u32) -> Result<VideoFrame> {
            let receiver = Arc::clone(&self.inner);
            ::tokio::task::spawn_blocking(move || receiver.capture_video_blocking(total_timeout_ms))
                .await
                .expect("Blocking task panicked")
        }

        /// Async version of `Receiver::capture_audio`.
        ///
        /// Attempts to capture an audio frame without blocking the async runtime.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout in milliseconds for the capture attempt
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured an audio frame
        /// * `Ok(None)` - No frame available within timeout
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_audio(&self, timeout_ms: u32) -> Result<Option<AudioFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::tokio::task::spawn_blocking(move || receiver.capture_audio(timeout_ms))
                .await
                .expect("Blocking task panicked")
        }

        /// Async version of `Receiver::capture_audio_with_retry`.
        ///
        /// Captures audio with automatic retry logic without blocking the async runtime.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout for each capture attempt in milliseconds
        /// * `max_attempts` - Maximum number of retry attempts
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured an audio frame
        /// * `Ok(None)` - No frame available after all retry attempts
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_audio_with_retry(
            &self,
            timeout_ms: u32,
            max_attempts: usize,
        ) -> Result<Option<AudioFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::tokio::task::spawn_blocking(move || {
                receiver.capture_audio_with_retry(timeout_ms, max_attempts)
            })
            .await
            .expect("Blocking task panicked")
        }

        /// Async version of `Receiver::capture_audio_blocking`.
        ///
        /// Blocks until an audio frame is received or timeout expires, without blocking
        /// the async runtime.
        ///
        /// # Arguments
        ///
        /// * `total_timeout_ms` - Total time to wait for a frame in milliseconds
        ///
        /// # Returns
        ///
        /// * `Ok(frame)` - Successfully captured an audio frame
        /// * `Err(Error::FrameTimeout)` - No frame received within timeout (includes retry details)
        /// * `Err(_)` - Another error occurred during capture
        pub async fn capture_audio_blocking(&self, total_timeout_ms: u32) -> Result<AudioFrame> {
            let receiver = Arc::clone(&self.inner);
            ::tokio::task::spawn_blocking(move || receiver.capture_audio_blocking(total_timeout_ms))
                .await
                .expect("Blocking task panicked")
        }

        /// Async version of `Receiver::capture_metadata`.
        ///
        /// Attempts to capture a metadata frame without blocking the async runtime.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout in milliseconds for the capture attempt
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured a metadata frame
        /// * `Ok(None)` - No frame available within timeout
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_metadata(&self, timeout_ms: u32) -> Result<Option<MetadataFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::tokio::task::spawn_blocking(move || receiver.capture_metadata(timeout_ms))
                .await
                .expect("Blocking task panicked")
        }

        /// Async version of `Receiver::capture_metadata_with_retry`.
        ///
        /// Captures metadata with automatic retry logic without blocking the async runtime.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout for each capture attempt in milliseconds
        /// * `max_attempts` - Maximum number of retry attempts
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured a metadata frame
        /// * `Ok(None)` - No frame available after all retry attempts
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_metadata_with_retry(
            &self,
            timeout_ms: u32,
            max_attempts: usize,
        ) -> Result<Option<MetadataFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::tokio::task::spawn_blocking(move || {
                receiver.capture_metadata_with_retry(timeout_ms, max_attempts)
            })
            .await
            .expect("Blocking task panicked")
        }

        /// Async version of `Receiver::capture_metadata_blocking`.
        ///
        /// Blocks until a metadata frame is received or timeout expires, without blocking
        /// the async runtime.
        ///
        /// # Arguments
        ///
        /// * `total_timeout_ms` - Total time to wait for a frame in milliseconds
        ///
        /// # Returns
        ///
        /// * `Ok(frame)` - Successfully captured a metadata frame
        /// * `Err(Error::FrameTimeout)` - No frame received within timeout (includes retry details)
        /// * `Err(_)` - Another error occurred during capture
        pub async fn capture_metadata_blocking(
            &self,
            total_timeout_ms: u32,
        ) -> Result<MetadataFrame> {
            let receiver = Arc::clone(&self.inner);
            ::tokio::task::spawn_blocking(move || {
                receiver.capture_metadata_blocking(total_timeout_ms)
            })
            .await
            .expect("Blocking task panicked")
        }
    }

    impl Clone for AsyncReceiver {
        fn clone(&self) -> Self {
            Self {
                inner: Arc::clone(&self.inner),
            }
        }
    }
}

#[cfg(feature = "async-std")]
pub mod async_std {
    //! async-std runtime integration.
    //!
    //! Provides `AsyncReceiver` wrapper that uses `async_std::task::spawn_blocking`
    //! to run NDI operations without blocking the async-std runtime.

    use super::*;

    /// Async receiver wrapper for async-std runtime.
    ///
    /// This wrapper provides async versions of the `Receiver` methods by running
    /// blocking NDI operations on async-std's blocking thread pool using `spawn_blocking`.
    ///
    /// # Thread Safety
    ///
    /// The underlying `Receiver` is wrapped in an `Arc` to allow sharing across
    /// async tasks and safe cloning. The NDI SDK receiver is inherently thread-safe.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "async-std")]
    /// # {
    /// use grafton_ndi::{NDI, ReceiverOptionsBuilder, async_std::AsyncReceiver};
    ///
    /// #[async_std::main]
    /// async fn main() -> Result<(), grafton_ndi::Error> {
    ///     let ndi = NDI::new()?;
    ///     // ... obtain source ...
    ///     # let source = grafton_ndi::Source {
    ///     #     name: "Test".into(),
    ///     #     address: grafton_ndi::SourceAddress::None
    ///     # };
    ///
    ///     let receiver = ReceiverOptionsBuilder::snapshot_preset(source).build(&ndi)?;
    ///     let async_receiver = AsyncReceiver::new(receiver);
    ///
    ///     // Non-blocking async capture
    ///     match async_receiver.capture_video(100).await? {
    ///         Some(frame) => println!("Got frame: {}x{}", frame.width, frame.height),
    ///         None => println!("No frame available"),
    ///     }
    ///
    ///     Ok(())
    /// }
    /// # }
    /// ```
    pub struct AsyncReceiver {
        inner: Arc<Receiver>,
    }

    impl AsyncReceiver {
        /// Create a new async receiver wrapper.
        ///
        /// The receiver is wrapped in an `Arc` to allow sharing across async tasks.
        pub fn new(receiver: Receiver) -> Self {
            Self {
                inner: Arc::new(receiver),
            }
        }

        /// Async version of `Receiver::capture_video`.
        ///
        /// Attempts to capture a video frame without blocking the async runtime.
        /// Uses `async_std::task::spawn_blocking` internally.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout in milliseconds for the capture attempt
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured a video frame
        /// * `Ok(None)` - No frame available within timeout
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_video(&self, timeout_ms: u32) -> Result<Option<VideoFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::async_std::task::spawn_blocking(move || receiver.capture_video(timeout_ms)).await
        }

        /// Async version of `Receiver::capture_video_with_retry`.
        ///
        /// Captures video with automatic retry logic without blocking the async runtime.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout for each capture attempt in milliseconds
        /// * `max_attempts` - Maximum number of retry attempts
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured a video frame
        /// * `Ok(None)` - No frame available after all retry attempts
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_video_with_retry(
            &self,
            timeout_ms: u32,
            max_attempts: usize,
        ) -> Result<Option<VideoFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::async_std::task::spawn_blocking(move || {
                receiver.capture_video_with_retry(timeout_ms, max_attempts)
            })
            .await
        }

        /// Async version of `Receiver::capture_video_blocking`.
        ///
        /// Blocks until a frame is received or timeout expires, without blocking
        /// the async runtime. This is the recommended method for reliable frame capture.
        ///
        /// # Arguments
        ///
        /// * `total_timeout_ms` - Total time to wait for a frame in milliseconds
        ///
        /// # Returns
        ///
        /// * `Ok(frame)` - Successfully captured a video frame
        /// * `Err(Error::FrameTimeout)` - No frame received within timeout (includes retry details)
        /// * `Err(_)` - Another error occurred during capture
        ///
        /// # Example
        ///
        /// ```no_run
        /// # #[cfg(feature = "async-std")]
        /// # {
        /// # use grafton_ndi::{NDI, ReceiverOptionsBuilder, async_std::AsyncReceiver};
        /// # #[async_std::main]
        /// # async fn main() -> Result<(), grafton_ndi::Error> {
        /// # let ndi = NDI::new()?;
        /// # let source = grafton_ndi::Source {
        /// #     name: "Test".into(),
        /// #     address: grafton_ndi::SourceAddress::None
        /// # };
        /// # let receiver = ReceiverOptionsBuilder::snapshot_preset(source).build(&ndi)?;
        /// let async_receiver = AsyncReceiver::new(receiver);
        /// let frame = async_receiver.capture_video_blocking(5000).await?;
        /// println!("Captured {}x{} frame", frame.width, frame.height);
        /// # Ok(())
        /// # }
        /// # }
        /// ```
        pub async fn capture_video_blocking(&self, total_timeout_ms: u32) -> Result<VideoFrame> {
            let receiver = Arc::clone(&self.inner);
            ::async_std::task::spawn_blocking(move || {
                receiver.capture_video_blocking(total_timeout_ms)
            })
            .await
        }

        /// Async version of `Receiver::capture_audio`.
        ///
        /// Attempts to capture an audio frame without blocking the async runtime.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout in milliseconds for the capture attempt
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured an audio frame
        /// * `Ok(None)` - No frame available within timeout
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_audio(&self, timeout_ms: u32) -> Result<Option<AudioFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::async_std::task::spawn_blocking(move || receiver.capture_audio(timeout_ms)).await
        }

        /// Async version of `Receiver::capture_audio_with_retry`.
        ///
        /// Captures audio with automatic retry logic without blocking the async runtime.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout for each capture attempt in milliseconds
        /// * `max_attempts` - Maximum number of retry attempts
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured an audio frame
        /// * `Ok(None)` - No frame available after all retry attempts
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_audio_with_retry(
            &self,
            timeout_ms: u32,
            max_attempts: usize,
        ) -> Result<Option<AudioFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::async_std::task::spawn_blocking(move || {
                receiver.capture_audio_with_retry(timeout_ms, max_attempts)
            })
            .await
        }

        /// Async version of `Receiver::capture_audio_blocking`.
        ///
        /// Blocks until an audio frame is received or timeout expires, without blocking
        /// the async runtime.
        ///
        /// # Arguments
        ///
        /// * `total_timeout_ms` - Total time to wait for a frame in milliseconds
        ///
        /// # Returns
        ///
        /// * `Ok(frame)` - Successfully captured an audio frame
        /// * `Err(Error::FrameTimeout)` - No frame received within timeout (includes retry details)
        /// * `Err(_)` - Another error occurred during capture
        pub async fn capture_audio_blocking(&self, total_timeout_ms: u32) -> Result<AudioFrame> {
            let receiver = Arc::clone(&self.inner);
            ::async_std::task::spawn_blocking(move || {
                receiver.capture_audio_blocking(total_timeout_ms)
            })
            .await
        }

        /// Async version of `Receiver::capture_metadata`.
        ///
        /// Attempts to capture a metadata frame without blocking the async runtime.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout in milliseconds for the capture attempt
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured a metadata frame
        /// * `Ok(None)` - No frame available within timeout
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_metadata(&self, timeout_ms: u32) -> Result<Option<MetadataFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::async_std::task::spawn_blocking(move || receiver.capture_metadata(timeout_ms)).await
        }

        /// Async version of `Receiver::capture_metadata_with_retry`.
        ///
        /// Captures metadata with automatic retry logic without blocking the async runtime.
        ///
        /// # Arguments
        ///
        /// * `timeout_ms` - Timeout for each capture attempt in milliseconds
        /// * `max_attempts` - Maximum number of retry attempts
        ///
        /// # Returns
        ///
        /// * `Ok(Some(frame))` - Successfully captured a metadata frame
        /// * `Ok(None)` - No frame available after all retry attempts
        /// * `Err(_)` - An error occurred during capture
        pub async fn capture_metadata_with_retry(
            &self,
            timeout_ms: u32,
            max_attempts: usize,
        ) -> Result<Option<MetadataFrame>> {
            let receiver = Arc::clone(&self.inner);
            ::async_std::task::spawn_blocking(move || {
                receiver.capture_metadata_with_retry(timeout_ms, max_attempts)
            })
            .await
        }

        /// Async version of `Receiver::capture_metadata_blocking`.
        ///
        /// Blocks until a metadata frame is received or timeout expires, without blocking
        /// the async runtime.
        ///
        /// # Arguments
        ///
        /// * `total_timeout_ms` - Total time to wait for a frame in milliseconds
        ///
        /// # Returns
        ///
        /// * `Ok(frame)` - Successfully captured a metadata frame
        /// * `Err(Error::FrameTimeout)` - No frame received within timeout (includes retry details)
        /// * `Err(_)` - Another error occurred during capture
        pub async fn capture_metadata_blocking(
            &self,
            total_timeout_ms: u32,
        ) -> Result<MetadataFrame> {
            let receiver = Arc::clone(&self.inner);
            ::async_std::task::spawn_blocking(move || {
                receiver.capture_metadata_blocking(total_timeout_ms)
            })
            .await
        }
    }

    impl Clone for AsyncReceiver {
        fn clone(&self) -> Self {
            Self {
                inner: Arc::clone(&self.inner),
            }
        }
    }
}
