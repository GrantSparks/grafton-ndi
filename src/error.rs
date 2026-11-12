//! Error types for the grafton-ndi library.

use thiserror::Error;

use std::{ffi::NulError, fmt, time::Duration};

use crate::capabilities::CapabilityUnavailable;

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

    /// An optional operation is unavailable in this build or with the
    /// selected runtime.
    ///
    /// For a capability a [`RuntimeFeature`] describes, `reason` is the one
    /// [`Capabilities::status`] reports for it, so a caller can branch on it
    /// without reading text. The condition lasts for the life of the process:
    /// the runtime is selected once, and a build that lacks a declaration has
    /// to be rebuilt, with the `advanced` feature and with `NDI_SDK_DIR`
    /// naming an NDI Advanced SDK whose headers declare it.
    ///
    /// [`RuntimeFeature`]: crate::capabilities::RuntimeFeature
    /// [`Capabilities::status`]: crate::capabilities::Capabilities::status
    #[error("NDI capability {capability} is unavailable: {}", Unavailability(.reason, .detail))]
    UnsupportedCapability {
        /// The capability's name. For one a [`RuntimeFeature`] describes, it
        /// is the same whichever operation the capability gates.
        ///
        /// [`RuntimeFeature`]: crate::capabilities::RuntimeFeature
        capability: &'static str,
        /// Why it is unavailable.
        reason: CapabilityUnavailable,
        /// The evidence: the entry point the runtime lacks (for
        /// [`SymbolUnavailable`] and [`WrongEdition`]), the version it reports
        /// against the version required (for [`RuntimeTooOld`] and
        /// [`RuntimeVersionUnknown`]), or the declaration this build lacks (for
        /// [`BuildAbiUnavailable`]).
        ///
        /// Empty when there is no evidence beyond `reason`: for example, for a
        /// runtime that exports the capability's entry point but is not an
        /// Advanced SDK runtime, or a build without the `advanced` feature.
        ///
        /// [`SymbolUnavailable`]: CapabilityUnavailable::SymbolUnavailable
        /// [`WrongEdition`]: CapabilityUnavailable::WrongEdition
        /// [`RuntimeTooOld`]: CapabilityUnavailable::RuntimeTooOld
        /// [`RuntimeVersionUnknown`]: CapabilityUnavailable::RuntimeVersionUnknown
        /// [`BuildAbiUnavailable`]: CapabilityUnavailable::BuildAbiUnavailable
        detail: String,
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
    /// Includes the number of capture attempts and the time since the caller's
    /// timeout budget started. At least one attempt is made, even when the
    /// budget is zero or already spent; further retries are not guaranteed.
    /// For async capture, the budget starts on the future's first poll, so
    /// `elapsed` includes time spent waiting for a blocking-pool thread.
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
        /// Time since the caller's timeout budget started
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

    /// A Discovery Server listener did not send a request, because no receiver
    /// it lists accepts it.
    ///
    /// Unlike [`Error::UnsupportedCapability`], this says nothing about the
    /// build or the runtime. A listener lists nothing until it has connected
    /// to its server, so a request made right after it was created can be
    /// refused with `advertised` false and succeed later.
    #[error("The Discovery Server listener sent no request: {}", request_refusal(.advertised))]
    ReceiverRequestNotAccepted {
        /// Whether a receiver with the identifier is listed. When it is not, it
        /// may not have registered with the server yet, and a later request can
        /// succeed; when it is, the receiver does not accept this command.
        advertised: bool,
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

/// Why a capability is unavailable, followed by its evidence when there is any.
struct Unavailability<'a>(&'a CapabilityUnavailable, &'a str);

impl fmt::Display for Unavailability<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(reason, detail) = *self;
        if detail.is_empty() {
            return write!(f, "{reason}");
        }
        match reason {
            CapabilityUnavailable::WrongEdition => {
                write!(f, "{reason}: it does not export {detail}")
            }
            CapabilityUnavailable::BuildAbiUnavailable => {
                write!(f, "{reason}: it does not declare {detail}")
            }
            CapabilityUnavailable::SymbolUnavailable
            | CapabilityUnavailable::RuntimeTooOld
            | CapabilityUnavailable::RuntimeVersionUnknown => write!(f, "{reason}: {detail}"),
        }
    }
}

/// Why a listener refused to send a receiver request.
fn request_refusal(advertised: &bool) -> &'static str {
    if *advertised {
        "the receiver it lists does not accept this request"
    } else {
        "it lists no receiver with this identifier, perhaps because it has not connected \
         or the receiver has not registered yet"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unsupported(reason: CapabilityUnavailable, detail: &str) -> String {
        Error::UnsupportedCapability {
            capability: "source routing",
            reason,
            detail: detail.into(),
        }
        .to_string()
    }

    #[test]
    fn capability_errors_name_their_evidence_and_never_end_in_empty_punctuation() {
        use CapabilityUnavailable::*;
        let prefix = "NDI capability source routing is unavailable: ";
        for reason in [
            RuntimeVersionUnknown,
            RuntimeTooOld,
            SymbolUnavailable,
            BuildAbiUnavailable,
            WrongEdition,
        ] {
            assert_eq!(unsupported(reason, ""), format!("{prefix}{reason}"));
        }
        assert_eq!(
            unsupported(SymbolUnavailable, "NDIlib_routing_create"),
            format!("{prefix}the selected runtime does not export a required function: NDIlib_routing_create")
        );
        assert_eq!(
            unsupported(WrongEdition, "NDIlib_routing_create_v2"),
            format!("{prefix}the selected runtime is not an Advanced SDK runtime: it does not export NDIlib_routing_create_v2")
        );
        assert_eq!(
            unsupported(BuildAbiUnavailable, "NDIlib_routing_create_v2"),
            format!("{prefix}this build was made without the Advanced SDK declarations: it does not declare NDIlib_routing_create_v2")
        );
        assert_eq!(
            unsupported(
                RuntimeTooOld,
                "it needs NDI 6.4.0.0 or later and the runtime reports \"6.3.2.0\""
            ),
            format!("{prefix}the selected runtime is too old: it needs NDI 6.4.0.0 or later and the runtime reports \"6.3.2.0\"")
        );
    }

    #[test]
    fn a_refused_receiver_request_says_whether_the_receiver_is_listed() {
        let listed = Error::ReceiverRequestNotAccepted { advertised: true }.to_string();
        let unlisted = Error::ReceiverRequestNotAccepted { advertised: false }.to_string();
        assert!(listed.ends_with("the receiver it lists does not accept this request"));
        assert!(unlisted.contains("no receiver with this identifier"));
        assert!(!listed.contains("capability") && !unlisted.contains("capability"));
    }
}
