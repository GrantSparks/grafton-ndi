//! Error types for the grafton-ndi library.

use std::ffi::NulError;
use std::io;
use thiserror::Error;

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
}
