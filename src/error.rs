use std::ffi::NulError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to initialize the NDI runtime: {0}")]
    InitializationFailed(String),
    #[error("Encountered a null pointer in function: {0}")]
    NullPointer(String),
    #[error("Invalid UTF-8 string in data: {0}")]
    InvalidUtf8(String),
    #[error("Invalid CString: {0}")]
    InvalidCString(#[from] NulError),
    #[error("Failed to capture frame: {0}")]
    CaptureFailed(String),
    #[error("Invalid frame data: {0}")]
    InvalidFrame(String),
}
