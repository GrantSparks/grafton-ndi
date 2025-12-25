//! Error types for the grafton-ndi library.

use thiserror::Error;

use std::{ffi::NulError, io, time::Duration};

/// The main error type for NDI operations.
///
/// This enum represents all possible errors that can occur when using the
/// grafton-ndi library. It provides detailed error messages and automatic
/// conversion from common error types.
#[derive(Debug, Error)]
pub enum Error {
    /// NDI runtime initialization failed.
    ///
    /// This typically occurs when the NDI SDK is not installed or cannot be loaded.
    #[error("Failed to initialize the NDI runtime: {0}")]
    InitializationFailed(String),

    /// A null pointer was returned by the NDI SDK.
    ///
    /// Indicates an internal NDI error or invalid operation.
    #[error("Encountered a null pointer in function: {0}")]
    NullPointer(String),

    /// Invalid UTF-8 data in a string from the NDI SDK.
    #[error("Invalid UTF-8 string in data: {0}")]
    InvalidUtf8(String),

    /// Failed to create a C string due to null bytes.
    #[error("Invalid CString: {0}")]
    InvalidCString(#[from] NulError),

    /// Frame capture operation failed.
    #[error("Failed to capture frame: {0}")]
    CaptureFailed(String),

    /// Frame data is invalid or corrupted.
    #[error("Invalid frame data: {0}")]
    InvalidFrame(String),

    /// PTZ (Pan-Tilt-Zoom) camera control command failed.
    #[error("PTZ command failed: {0}")]
    PtzCommandFailed(String),

    /// Configuration parameters are invalid.
    ///
    /// This can occur when builder validation fails or conflicting options are set.
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// I/O operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),

    /// Operation timed out with generic context.
    ///
    /// This is a general timeout error. For frame capture timeouts with retry information,
    /// see [`Error::FrameTimeout`].
    #[error("Operation timed out: {0}")]
    Timeout(String),

    /// Frame capture timed out after multiple retry attempts.
    ///
    /// This error includes detailed information about the retry attempts and total elapsed time,
    /// making it easier to diagnose frame capture issues and distinguish them from other timeout scenarios.
    ///
    /// # Example
    ///
    /// ```
    /// # use grafton_ndi::Error;
    /// # use std::time::Duration;
    /// match some_operation() {
    ///     Err(Error::FrameTimeout { attempts, elapsed }) => {
    ///         eprintln!("Frame timeout after {} attempts in {:?}", attempts, elapsed);
    ///         // Could implement retry logic based on attempts count
    ///     }
    ///     Err(e) => eprintln!("Other error: {}", e),
    ///     Ok(_) => println!("Success"),
    /// }
    /// # fn some_operation() -> Result<(), Error> { Ok(()) }
    /// ```
    #[error("Frame capture timed out after {attempts} attempts ({elapsed:?})")]
    FrameTimeout {
        /// Number of capture attempts made before timing out
        attempts: usize,
        /// Total elapsed time during capture attempts
        elapsed: Duration,
    },

    /// NDI source became unavailable during operation.
    ///
    /// This error indicates that an NDI source that was previously available has gone offline
    /// or become unreachable. This is different from [`Error::NoSourcesFound`] which indicates
    /// that no matching sources were ever discovered.
    ///
    /// # Example
    ///
    /// ```
    /// # use grafton_ndi::Error;
    /// match some_operation() {
    ///     Err(Error::SourceUnavailable { source_name }) => {
    ///         eprintln!("Lost connection to source: {}", source_name);
    ///         // Could attempt to reconnect or switch to backup source
    ///     }
    ///     Err(e) => eprintln!("Other error: {}", e),
    ///     Ok(_) => println!("Success"),
    /// }
    /// # fn some_operation() -> Result<(), Error> { Ok(()) }
    /// ```
    #[error("NDI source became unavailable: {source_name}")]
    SourceUnavailable {
        /// Name or identifier of the source that became unavailable
        source_name: String,
    },

    /// Receiver disconnected from its NDI source.
    ///
    /// This error indicates that an active receiver lost its connection to the source.
    /// The reason field provides additional context about why the disconnection occurred.
    ///
    /// # Example
    ///
    /// ```
    /// # use grafton_ndi::Error;
    /// match some_operation() {
    ///     Err(Error::Disconnected { reason }) => {
    ///         eprintln!("Receiver disconnected: {}", reason);
    ///         // Could implement automatic reconnection logic
    ///     }
    ///     Err(e) => eprintln!("Other error: {}", e),
    ///     Ok(_) => println!("Success"),
    /// }
    /// # fn some_operation() -> Result<(), Error> { Ok(()) }
    /// ```
    #[error("Receiver disconnected from source: {reason}")]
    Disconnected {
        /// Reason for the disconnection
        reason: String,
    },

    /// No NDI sources found matching the specified criteria.
    ///
    /// This error is returned when source discovery completes but no sources match
    /// the requested criteria (e.g., host/IP filter, group filter).
    ///
    /// # Example
    ///
    /// ```
    /// # use grafton_ndi::Error;
    /// match some_operation() {
    ///     Err(Error::NoSourcesFound { criteria }) => {
    ///         eprintln!("No sources found matching: {}", criteria);
    ///         // Could widen search criteria or wait longer
    ///     }
    ///     Err(e) => eprintln!("Other error: {}", e),
    ///     Ok(_) => println!("Success"),
    /// }
    /// # fn some_operation() -> Result<(), Error> { Ok(()) }
    /// ```
    #[error("No NDI sources found matching: {criteria}")]
    NoSourcesFound {
        /// The search criteria that yielded no results
        criteria: String,
    },

    /// Failed to spawn a blocking task on the async runtime.
    ///
    /// This error occurs when the async runtime fails to spawn a blocking task,
    /// typically due to task cancellation or runtime shutdown. Unlike the previous
    /// behavior which panicked on tokio `JoinError`, this error allows callers to
    /// handle the failure gracefully.
    ///
    /// # Example
    ///
    /// ```
    /// use grafton_ndi::Error;
    ///
    /// fn handle_error(err: Error) {
    ///     match err {
    ///         Error::SpawnFailed(reason) => {
    ///             eprintln!("Async spawn failed: {}", reason);
    ///             // Could retry or gracefully shutdown
    ///         }
    ///         e => eprintln!("Other error: {}", e),
    ///     }
    /// }
    /// ```
    #[error("Failed to spawn blocking task: {0}")]
    SpawnFailed(String),
}
