//! Error types for the grafton-ndi library.

use thiserror::Error;

use std::{ffi::NulError, time::Duration};

/// The main error type for NDI operations.
///
/// This enum represents all possible errors that can occur when using the
/// grafton-ndi library. It provides detailed error messages and automatic
/// conversion from common error types.
///
/// This enum is marked `#[non_exhaustive]` so that new error variants can be
/// added as the NDI SDK surface evolves without constituting a breaking change.
/// Match arms should include a wildcard `_` pattern.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// No usable NDI runtime library could be loaded.
    ///
    /// The message names every location tried and why each was rejected: the
    /// file was absent, the operating system could not load it, or it lacks a
    /// required entry point. Loading is retryable.
    #[error("Failed to load the NDI runtime: {0}")]
    RuntimeLoadFailed(String),

    /// An optional operation is unavailable in this build or selected runtime.
    #[error("NDI capability {capability} is unavailable: {reason}")]
    UnsupportedCapability {
        /// Requested capability.
        capability: String,
        /// Build, module, target, or symbol evidence explaining unavailability.
        reason: String,
    },

    /// The request cannot be met by the runtime already selected for this
    /// process: it names a different library, or requires the Advanced SDK when
    /// the selected runtime is not one.
    #[error("Conflicting NDI runtime selection: {0}")]
    RuntimeConflict(String),

    /// Construction requires proof from an initialized compatible runtime.
    #[error("{feature} requires an initialized compatible runtime; use {operation}")]
    RuntimeRequired {
        /// Functionality requiring runtime validation.
        feature: &'static str,
        /// Runtime-aware constructor to use.
        operation: &'static str,
    },

    /// The native SDK rejected an operation after capability validation.
    ///
    /// This includes creating an object: a finder, receiver, sender or other
    /// object the SDK does not create is reported with `operation` naming the
    /// creating function.
    #[error("The NDI SDK rejected {operation}")]
    NativeOperationFailed {
        /// Native operation or public action that failed.
        operation: &'static str,
    },

    /// NDI runtime initialization failed.
    ///
    /// Returned only for the runtime itself: the SDK refused to start, or the
    /// runtime handle reference count was exhausted. A finder, receiver,
    /// sender or other object the SDK does not create is reported as
    /// [`Error::NativeOperationFailed`], naming the creating function. A
    /// runtime library that cannot be loaded is reported as
    /// [`Error::RuntimeLoadFailed`] instead, and one that does not start
    /// because of the CPU as [`Error::UnsupportedCpu`].
    #[error("Failed to initialize the NDI runtime: {0}")]
    InitializationFailed(String),

    /// The selected NDI runtime cannot run on this CPU.
    ///
    /// The runtime did not start and reports that the CPU lacks instructions
    /// it requires. Which instructions those are depends on the SDK version
    /// and the platform. [`Runtime::is_supported_cpu`](crate::Runtime::is_supported_cpu)
    /// answers the same question without starting NDI.
    #[error("The selected NDI runtime does not support this CPU")]
    UnsupportedCpu,

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

    /// Configuration parameters are invalid.
    ///
    /// This can occur when builder validation fails or conflicting options are set.
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// The capture wait budget expired without a frame.
    ///
    /// Includes the number of capture attempts and elapsed time in the retry
    /// loop. The attempt count can be zero or one; retries are not guaranteed.
    /// For async capture, `elapsed`
    /// excludes time queued before that loop begins.
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

    /// An async blocking operation could not run or finish normally.
    ///
    /// Covers a missing Tokio runtime context, task cancellation or join failure,
    /// and a panic caught while executing a blocking operation. Dropping a future
    /// discards its result; cancellation does not deliver an error to that caller.
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
