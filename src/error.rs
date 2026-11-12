//! The crate's one error type.

use thiserror::Error;

use std::{ffi::NulError, fmt};

use crate::capabilities::{CapabilityUnavailable, RuntimeFeature};

/// Every error this crate reports, from the core API, the `advanced` module
/// and the File API alike.
///
/// Each variant says when it occurs. Its text is for diagnostics; match on
/// the variant and its fields rather than on the text.
///
/// This enum is marked `#[non_exhaustive]` so that new error variants can be
/// added as the NDI SDK surface evolves without constituting a breaking change.
/// Match arms should include a wildcard `_` pattern.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// No usable NDI library could be selected.
    ///
    /// * Selecting the NDI runtime ([`Runtime::select`] and every constructor
    ///   that selects one): no usable runtime library was found. The message
    ///   names every location tried and why each was rejected: the file was
    ///   absent, the operating system could not load it, or it lacks a
    ///   required entry point.
    /// * Opening the File API (`file::FileApi::open`): no usable File library
    ///   was found, with every location tried and the reason for each. A
    ///   library that provides no family of File entry points is not usable.
    ///
    /// Nothing is selected, so the call can be repeated, for example once the
    /// library is installed. A selected File library that lacks the entry
    /// points of one family is reported as [`Error::UnsupportedCapability`]
    /// instead, by every operation that needs that family.
    ///
    /// [`Runtime::select`]: crate::Runtime::select
    #[error("Failed to load an NDI library: {0}")]
    RuntimeLoadFailed(String),

    /// An operation needs a capability this build or the selected runtime
    /// does not provide.
    ///
    /// `feature` is the capability, and `reason` is the one
    /// [`Capabilities::status`] reports for it, so a caller can branch on
    /// either without reading text. Every operation that needs the capability
    /// is refused this way before anything reaches the SDK, and
    /// [`Ndi::require_feature`] reports the same error in advance. A family
    /// of the NDI File library (`RuntimeFeature::File`) is refused by the File
    /// API operations that need it, when the selected File library does not
    /// export one of its entry points, and `file::FileApi::status` reports its
    /// status in advance. The condition lasts for the life of the process:
    /// the runtime and the File library are each selected once, and a build
    /// that lacks a declaration has to be rebuilt, with the `advanced` feature
    /// and with `NDI_SDK_DIR` naming an NDI Advanced SDK whose headers declare
    /// it.
    ///
    /// [`Capabilities::status`]: crate::capabilities::Capabilities::status
    /// [`Ndi::require_feature`]: crate::Ndi::require_feature
    #[error(
        "NDI capability {} is unavailable: {}",
        .feature.name(),
        Unavailability(.reason, .detail)
    )]
    UnsupportedCapability {
        /// The capability, the same whichever operation it gates.
        feature: RuntimeFeature,
        /// Why it is unavailable.
        reason: CapabilityUnavailable,
        /// The evidence: the entry point the runtime, or for a File family
        /// the File library, lacks (for [`SymbolUnavailable`] and
        /// [`WrongEdition`]), the version the runtime reports
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

    /// The request cannot be met by the implementation already selected for
    /// this process, which cannot be replaced.
    ///
    /// * The runtime options name a library file other than the selected one,
    ///   ask for the Advanced SDK when the selected runtime is not one, or
    ///   name any file when the SDK is linked into the program.
    /// * Loading a native library re-entered runtime selection on the same
    ///   thread.
    /// * The File API options name a File library other than the selected one,
    ///   or any library when the File library is linked into the program.
    /// * The File library cannot accompany the selected runtime: on Linux,
    ///   macOS and Android it needs the Advanced SDK runtime, and it is
    ///   refused while the process's global symbol scope holds an NDI runtime
    ///   other than the selected one, which it would bind to.
    #[error("Conflicting NDI runtime selection: {0}")]
    RuntimeConflict(String),

    /// The native SDK did not perform an operation.
    ///
    /// It returned the failure value its header gives the operation: `false`
    /// from an operation that reports success, or null from one that creates
    /// an object (a finder, receiver, sender, router, FrameSync, AV sync or
    /// genlock instance, or a File player or recorder) or returns a source
    /// descriptor. A source descriptor with neither a name nor an address is
    /// reported the same way. `operation` names the SDK function or the File
    /// API operation. A Discovery Server advertiser or listener the SDK does
    /// not create is reported as [`Error::InvalidInput`] instead, because the
    /// SDK refuses only the address it was given.
    ///
    /// The SDK gives no further reason. The File library may report one as
    /// an event.
    #[error("The NDI SDK did not perform {operation}")]
    NativeOperationFailed {
        /// The SDK function, or the File API operation, that failed.
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

    /// The SDK sent nothing because nothing is connected.
    ///
    /// A receiver sends metadata upstream to the source it is connected to,
    /// and `Processing.NDI.Recv.h` documents a refusal as meaning that it is
    /// connected to none. A later attempt can succeed once it has connected.
    #[error("Not connected: the NDI SDK sent nothing")]
    NotConnected,

    /// Text the SDK delivered is not UTF-8.
    ///
    /// Reported for the metadata of a captured frame, a received metadata
    /// frame, and the strings a receiver returns, such as its source's
    /// web-control URL. Text a peer advertises through a Discovery Server is
    /// decoded lossily instead.
    #[error("Invalid UTF-8 string in data: {0}")]
    InvalidUtf8(String),

    /// Text passed to the SDK contains a NUL byte, which a C string cannot
    /// hold.
    #[error("Invalid CString: {0}")]
    InvalidCString(#[from] NulError),

    /// A capture reported a failure instead of a frame or an event.
    ///
    /// A receiver's capture returned the SDK's error frame type, which
    /// `Processing.NDI.Recv.h` documents as a lost connection; or a sender's
    /// metadata capture returned a frame type other than metadata or none.
    #[error("Failed to capture frame: {0}")]
    CaptureFailed(String),

    /// A frame, or a value describing media, is invalid.
    ///
    /// * A frame the SDK delivered (video, audio, metadata or a compressed
    ///   packet) fails validation: its dimensions, format, strides, sizes or
    ///   pointers are inconsistent, or its metadata has no terminator within
    ///   the limit.
    /// * A frame, packet or buffer being built, converted or sent does not
    ///   match its format: a buffer is too small, a dimension, stride or rate
    ///   is out of range, or a format cannot be used for the operation. This
    ///   includes a compressed frame the SDK would discard instead of
    ///   delivering it.
    ///
    /// A value that describes no one frame, such as a compressed video format
    /// or an allocation, AV sync, clock or remote-control value of the
    /// `advanced` module, is [`Error::InvalidInput`] when it is out of range.
    #[error("Invalid frame data: {0}")]
    InvalidFrame(String),

    /// A value passed to the crate cannot be used.
    ///
    /// It is out of range, malformed, or conflicts with another setting: for
    /// example a timeout above [`MAX_TIMEOUT`](crate::MAX_TIMEOUT), a relative
    /// library path, a receive delay above two seconds, a PTZ value outside
    /// its range, malformed Advanced configuration JSON, more than one
    /// Discovery Server for a listener, allocator limits that cannot all be
    /// reached, a registration handed to an advertiser that did not issue it,
    /// a compressed video format, an allocation stride or buffer, an AV sync
    /// or clock quantity or a remote control value out of the range the SDK
    /// accepts, or a File API name, path or option the File library cannot
    /// take. The message says which value and why, and nothing reaches the
    /// SDK, with one exception: the SDK creates a Discovery Server advertiser
    /// or listener for any address it can parse, so when it creates none, the
    /// address it was given, or the lack of one, is reported here.
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// A bounded queue is full. The same call can succeed once queued work
    /// has completed, with nothing else changed; the [`QueueLimit`] says
    /// which queue and limit.
    ///
    /// * `advanced::QueuedSender::try_submit`, when the frame would take the
    ///   queue past its frame limit ([`QueueLimit::Frames`]) or its byte limit
    ///   ([`QueueLimit::Bytes`]). The frame comes back in the `Rejected`;
    ///   collect completed frames and submit it again.
    /// * Every operation of an async File handle except a request to stop,
    ///   when the handle's command queue is full
    ///   ([`QueueLimit::FileCommands`]).
    #[error("Queue full: {0}")]
    QueueFull(QueueLimit),

    /// A value the SDK returned, other than a frame, breaks the SDK's
    /// documented contract, or exceeds a limit this crate places on what it
    /// copies.
    ///
    /// For example a negative connection count, a receive delay outside the
    /// range the SDK accepts, a string without a terminator within its limit,
    /// a Discovery Server snapshot or event batch that is malformed or larger
    /// than one copy may be, a send-completion callback for a frame the SDK
    /// was not handed or had already returned, negative encoder guidance, an
    /// AV sync format change to a sample rate or channel count that is not
    /// positive, or a File SDK result that cannot be represented. Malformed
    /// frames are reported as [`Error::InvalidFrame`], and text that is not
    /// UTF-8 as [`Error::InvalidUtf8`].
    #[error("Unexpected NDI SDK result: {0}")]
    UnexpectedSdkResult(String),

    /// Source discovery finished without a source that matches.
    ///
    /// Returned by the finder's host lookups when no source on the requested
    /// host was discovered within the wait.
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

    /// A File API operation is not permitted in the current state of its
    /// recorder or async handle.
    ///
    /// The [`StateError`](crate::file::StateError) says which state forbids
    /// it, and [`StateError::is_retryable`](crate::file::StateError::is_retryable)
    /// whether the same call can succeed later without anything else changing.
    #[cfg(feature = "file-io")]
    #[error("Invalid File API state: {0}")]
    InvalidState(crate::file::StateError),

    /// An operating-system operation of the File API failed.
    ///
    /// * Resolving a relative path against the current directory, which may
    ///   be unavailable.
    /// * Checking, before a recording starts or rolls over, that the
    ///   directory of its first file exists and can be written to, and that an
    ///   exact file name does not exist yet
    ///   ([`AlreadyExists`](std::io::ErrorKind::AlreadyExists)).
    /// * Starting the thread of an async File handle or probe, or the timer
    ///   thread that async event waits share.
    #[cfg(feature = "file-io")]
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Which bounded queue was full, carried by [`Error::QueueFull`].
///
/// Its `Display` text is for diagnostics; match on the variant rather than
/// the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum QueueLimit {
    /// An Advanced `QueuedSender` already holds as many frames as its
    /// `QueueOptions::max_frames` allows.
    Frames,
    /// The frame would take an Advanced `QueuedSender` past its
    /// `QueueOptions::max_bytes`.
    Bytes,
    /// An async File handle already has as many commands waiting as its
    /// `AsyncFileOptions::command_capacity` allows.
    FileCommands,
}

impl fmt::Display for QueueLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Frames => "queued video frame limit reached",
            Self::Bytes => "queued video byte limit reached",
            Self::FileCommands => "File command queue is full",
        })
    }
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
            #[cfg(feature = "file-io")]
            CapabilityUnavailable::SeparateLibrary => write!(f, "{reason}: {detail}"),
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
            feature: RuntimeFeature::Routing,
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
            #[cfg(feature = "file-io")]
            SeparateLibrary,
        ] {
            assert_eq!(unsupported(reason, ""), format!("{prefix}{reason}"));
        }
        assert_eq!(
            unsupported(SymbolUnavailable, "NDIlib_routing_create"),
            format!("{prefix}the selected library does not export a required function: NDIlib_routing_create")
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

    #[test]
    fn a_full_queue_names_its_limit() {
        for (limit, text) in [
            (QueueLimit::Frames, "queued video frame limit reached"),
            (QueueLimit::Bytes, "queued video byte limit reached"),
            (QueueLimit::FileCommands, "File command queue is full"),
        ] {
            assert_eq!(
                Error::QueueFull(limit).to_string(),
                format!("Queue full: {text}")
            );
        }
    }
}
