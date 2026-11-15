//! The crate's one error type.

#[cfg(test)]
mod allocation_tests;
mod defects;
#[cfg(test)]
mod defects_tests;
pub use crate::sys::table::NativeFunction;
pub use defects::*;

mod loading;
pub use loading::{
    LibraryKind, LoadAttempt, LoadFailure, LoadOrigin, LoadRefusal, SelectionConflict,
    SelectionFailure,
};

use thiserror::Error;

use std::ffi::NulError;

use crate::capabilities::{CapabilityRefusal, RuntimeFeature};

/// Why an async blocking operation could not run.
///
/// Panics are resumed on the awaiting task rather than reported as spawn
/// failures. This discriminator does not depend on diagnostic text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum SpawnFailure {
    /// The Tokio operation was polled outside a Tokio runtime.
    #[error("no Tokio runtime is entered")]
    NoRuntime,
    /// The runtime cancelled the blocking task, or its abandoned closure never started.
    #[error("the blocking task was cancelled")]
    TaskCancelled,
}

/// Why the selected native runtime could not initialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum InitializationFailure {
    /// The SDK refused initialization on a supported CPU.
    #[error("NDIlib_initialize returned false on a CPU the runtime reports as supported")]
    NotStarted,
    /// Another runtime handle would overflow the reference count.
    #[error("NDI runtime reference count exhausted")]
    HandleCountExhausted,
}

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
    /// [`LoadFailure`] records the library, attempted locations, their origins
    /// and typed [`LoadRefusal`] causes. [`LoadFailure::loaded_any`] distinguishes
    /// code that loaded and was refused from locations that could not be opened.
    /// `Display` retains the diagnostic text for every attempt.
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
    /// points of one family is reported as `Error::FileFamilyUnavailable`
    /// instead, by every operation that needs that family.
    ///
    /// [`Runtime::select`]: crate::Runtime::select
    #[error("Failed to load an NDI library: {0}")]
    RuntimeLoadFailed(LoadFailure),

    /// An operation needs a capability this build or the selected runtime
    /// does not provide.
    ///
    /// `refusal` is the same typed evidence returned by
    /// [`Capabilities::status`](crate::Capabilities::status). Every operation
    /// is refused before reaching the SDK, and [`Ndi::require_feature`](crate::Ndi::require_feature)
    /// reports the same value in advance. This condition lasts for the process
    /// lifetime: selecting another runtime or supplying missing build declarations
    /// requires a new process or build.
    #[error("NDI capability {} is unavailable: {refusal}", .feature.name())]
    UnsupportedCapability {
        /// The capability, the same whichever operation it gates.
        feature: RuntimeFeature,
        /// Why it is unavailable, with the evidence recorded during detection.
        refusal: CapabilityRefusal,
    },

    /// The selected File library lacks an entry point of the required family.
    #[cfg(feature = "file-io")]
    #[error("NDI capability {} is unavailable: the selected library does not export a required function: {}", .family.name(), .missing.to_string_lossy())]
    FileFamilyUnavailable {
        /// The family the operation needs.
        family: crate::file::FileFamily,
        /// The first required entry point the selected library does not export.
        missing: &'static std::ffi::CStr,
    },

    /// Runtime or File library selection conflicts with the process's native
    /// libraries or a selection already in progress.
    ///
    /// [`SelectionFailure`] identifies the [`SelectionConflict`], library and
    /// selected, requested or foreign module paths without parsing text.
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
    RuntimeConflict(SelectionFailure),

    /// The native SDK did not perform an operation.
    ///
    /// It returned the failure value its header gives the operation: `false`
    /// from an operation that reports success, or null from one that creates
    /// an object (a finder, receiver, sender, router, FrameSync, AV sync or
    /// genlock instance, or a File player or recorder) or returns a source
    /// descriptor; or an internal-error status from AV synchronization.
    /// A source descriptor with neither a name nor an address is
    /// reported the same way. `operation` names the SDK function. A Discovery Server advertiser or listener the SDK does
    /// not create is reported as [`Error::InvalidInput`] instead, because the
    /// SDK refuses only the address it was given.
    ///
    /// The SDK gives no further reason. The File library may report one as
    /// an event.
    #[error("The NDI SDK did not perform {operation}")]
    NativeOperationFailed {
        /// The SDK function that failed.
        operation: crate::NativeFunction,
    },

    /// NDI runtime initialization failed, with its [`InitializationFailure`].
    ///
    /// Returned only for the runtime itself: the SDK refused to start, or the
    /// runtime handle reference count was exhausted. A finder, receiver,
    /// sender or other object the SDK does not create is reported as
    /// [`Error::NativeOperationFailed`], naming the creating function. A
    /// runtime library that cannot be loaded is reported as
    /// [`Error::RuntimeLoadFailed`] instead, and one that does not start
    /// because of the CPU as [`Error::UnsupportedCpu`].
    #[error("Failed to initialize the NDI runtime: {0}")]
    InitializationFailed(InitializationFailure),

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

    /// Text the SDK delivered is not UTF-8; the original [`std::str::Utf8Error`] retains its offsets.
    ///
    /// Reported for standalone received metadata frames and strings a receiver
    /// returns, such as its source's web-control URL. Unreadable per-frame
    /// video/audio metadata is withheld without discarding the media; inspect
    /// [`crate::FrameMetadataStatus`] instead. Text a peer advertises through a Discovery Server is
    /// decoded lossily instead.
    #[error("Invalid UTF-8 string in data: {0}")]
    InvalidUtf8(std::str::Utf8Error),

    /// Text passed to the SDK contains a NUL byte, which a C string cannot
    /// hold.
    #[error("Invalid CString: {0}")]
    InvalidCString(#[from] NulError),

    /// A receiver reported the SDK error frame: its connection was lost.
    #[error("Failed to capture frame: the SDK returned an error frame: the connection was lost")]
    ConnectionLost,

    /// A frame, or a value describing media, is invalid; [`FrameDefect`] identifies the cause.
    /// Shared layout checks use [`FrameDefect::Layout`] with a [`LayoutDefect`].
    ///
    /// * A frame the SDK delivered (video, audio, metadata or a compressed
    ///   packet) fails validation: its dimensions, format, strides, sizes or
    ///   pointers are inconsistent, or a standalone metadata frame has no
    ///   terminator within the limit.
    /// * A frame, packet or buffer being built, converted or sent does not
    ///   match its format: a buffer is too small, a dimension, stride or rate
    ///   is out of range, or a format cannot be used for the operation. This
    ///   includes a compressed frame the SDK would discard instead of
    ///   delivering it.
    ///
    /// Unreadable optional video/audio metadata is withheld instead of failing
    /// capture; [`crate::FrameMetadataStatus`] reports its condition.
    ///
    /// A value that describes no one frame, such as a compressed video format
    /// or an allocation, AV sync, clock or remote-control value of the
    /// `advanced` module, is [`Error::InvalidInput`] when it is out of range.
    #[error("Invalid frame data: {0}")]
    InvalidFrame(FrameDefect),

    /// A value passed to the crate cannot be used; [`InputDefect`] identifies the cause.
    /// Standalone aspect ratios, frame rates, compressed video formats and
    /// allocation strides use [`InputDefect::Layout`] with a [`LayoutDefect`].
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
    InvalidInput(InputDefect),

    /// A bounded queue is full. The same call can succeed once queued work
    /// has completed, with nothing else changed; the [`QueueLimit`] says
    /// which queue and limit.
    ///
    /// * `advanced::QueuedSender::try_submit`, when the frame would take the
    ///   queue past its frame limit ([`QueueLimit::Frames`]) or the retained
    ///   frames leave insufficient byte capacity ([`QueueLimit::Bytes`]). A
    ///   frame exceeding the entire byte limit gets [`Error::InvalidInput`].
    ///   The frame comes back in the `Rejected`;
    ///   collect completed frames and submit it again.
    /// * An operation of an async File handle, when the handle's command
    ///   queue is full ([`QueueLimit::FileCommands`]). A request to stop is
    ///   refused only when another one already waits beyond the full queue.
    /// * `file::AsyncFilePlayer::probe`, when the probes waiting for the
    ///   process's probe thread fill its queue ([`QueueLimit::FileProbes`]).
    #[error("Queue full: {0}")]
    QueueFull(QueueLimit),

    /// A value the SDK returned, other than a frame, breaks the SDK's
    /// documented contract, or owned storage could not be reserved for it.
    /// [`SdkResultDefect`] identifies that contract breach.
    ///
    /// For example a negative connection count, a receive delay outside the
    /// range the SDK accepts, a string without a terminator within its limit,
    /// a Discovery Server snapshot or event batch that is malformed, a
    /// send-completion callback for a frame the SDK
    /// was not handed or had already returned, negative encoder guidance, an
    /// unknown AV sync status, an AV sync format change to a sample rate or
    /// channel count that is not positive, or a File SDK result that cannot
    /// be represented. Malformed
    /// frames are reported as [`Error::InvalidFrame`], and text that is not
    /// UTF-8 as [`Error::InvalidUtf8`]. Ordinary resource-budget ceilings are
    /// [`Error::ResourceLimit`] instead.
    #[error("{prefix}{cause}", prefix = sdk_result_prefix(.0), cause = .0)]
    UnexpectedSdkResult(SdkResultDefect),

    /// Discovery data exceeded a per-copy or whole-discovery resource limit.
    /// No partial result is returned. `resource` identifies the exhausted
    /// count or byte budget; `limit` is its maximum.
    #[error("NDI resource limit exceeded: {resource} (limit {limit})")]
    ResourceLimit {
        /// The resource whose budget was exhausted.
        resource: ResourceKind,
        /// Maximum number of entries or bytes for that resource.
        limit: usize,
    },

    /// A caller-defined discovery operation finished without a matching source.
    ///
    /// This convenience error is constructed by callers when an empty discovery
    /// result is an application error. Finder selection returns `Ok(None)` and
    /// source enumeration returns an empty vector when nothing matches; neither
    /// constructs this error automatically.
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

    /// An async blocking operation could not run.
    ///
    /// [`SpawnFailure`] distinguishes a missing Tokio runtime context from a
    /// cancelled blocking task. A panic in the operation or its callback is
    /// resumed on the awaiting task with its original payload, after SDK frame
    /// guards are released on the worker. Dropping the future discards its
    /// result and any panic payload; it does not deliver an error to that caller.
    ///
    /// # Example
    ///
    /// ```
    /// use grafton_ndi::{Error, SpawnFailure};
    /// fn handle_error(error: Error) {
    ///     match error {
    ///         Error::SpawnFailed(SpawnFailure::NoRuntime) => {
    ///             eprintln!("Await this operation inside a Tokio runtime");
    ///         }
    ///         Error::SpawnFailed(SpawnFailure::TaskCancelled) => {
    ///             eprintln!("The runtime cancelled the blocking task");
    ///         }
    ///         error => eprintln!("{error}"),
    ///     }
    /// }
    /// ```
    #[error("Could not run blocking task: {0}")]
    SpawnFailed(SpawnFailure),

    /// A File API operation is not permitted in the current state of its
    /// recorder or async handle.
    ///
    /// The [`StateError`](crate::file::StateError) says which state forbids
    /// it, and [`StateError::is_retryable`](crate::file::StateError::is_retryable)
    /// whether the same call can succeed later without anything else changing.
    #[cfg(feature = "file-io")]
    #[error("Invalid File API state: {0}")]
    InvalidState(crate::file::StateError),

    /// An operating-system operation failed.
    ///
    /// * File API: resolving a relative path against the current directory,
    ///   which may be unavailable.
    /// * File API: checking, before a recording starts or rolls over, that
    ///   the directory of its first file exists and can be written to, and
    ///   that an exact file name does not exist yet
    ///   ([`AlreadyExists`](std::io::ErrorKind::AlreadyExists)).
    /// * File API: starting the thread of an async File handle or probe, or
    ///   the timer thread that async event waits share.
    /// * Image encoding: the writer given to a frame's `encode_image_to`
    ///   failed. The error is the writer's own.
    #[cfg(any(feature = "file-io", feature = "image-encoding"))]
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

// The existing largest payload is NulError. Selection diagnostics must not
// enlarge the error returned by hot capture/send paths (40 bytes on x86-64).
const _: () = assert!(
    std::mem::size_of::<Error>()
        <= std::mem::size_of::<NulError>() + std::mem::align_of::<NulError>()
);

// Preserve the compact nested causes on every 32-bit and 64-bit target,
// including SDK allocator paths that return a LayoutDefect without allocating.
#[cfg(any(target_pointer_width = "32", target_pointer_width = "64"))]
const _: () = {
    use std::mem::size_of;
    assert!(size_of::<FrameDefect>() == 4 * size_of::<usize>());
    assert!(size_of::<InputDefect>() == 4 * size_of::<usize>());
    assert!(
        size_of::<LayoutDefect>()
            == if cfg!(target_pointer_width = "64") {
                24
            } else {
                16
            }
    );
};

/// The resource budget exhausted by a discovery operation.
///
/// Each variant specifies both the scope and the unit of the `limit` field in
/// [`Error::ResourceLimit`]. Byte budgets cover native data inspected and owned
/// storage retained, as documented by the operation. Match this value rather
/// than diagnostic text; additional budgets may be introduced in 3.x.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum ResourceKind {
    /// Source descriptors in one plain or Advanced Finder snapshot; the limit
    /// is a number of sources.
    #[error("Finder snapshot sources")]
    FinderSnapshotSources,
    /// Aggregate bytes scanned or copied for one plain or Advanced Finder
    /// snapshot; the limit is in bytes.
    #[error("Finder snapshot bytes")]
    FinderSnapshotBytes,
    /// Distinct sources retained over one Finder discovery window; the limit
    /// is a number of sources.
    #[error("Finder discovery sources")]
    FinderDiscoverySources,
    /// Aggregate owned storage retained over one Finder discovery window;
    /// the limit is in bytes.
    #[error("Finder discovery bytes")]
    FinderDiscoveryBytes,
    /// Aggregate bytes scanned or copied for one Discovery Server snapshot or
    /// event batch, including nested arrays and strings; the limit is in bytes.
    #[error("Discovery Server snapshot or event batch bytes")]
    DiscoveryServerCopyBytes,
}

/// Which bounded queue was full, carried by [`Error::QueueFull`].
///
/// Its `Display` text is for diagnostics; match on the variant rather than
/// the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum QueueLimit {
    /// An Advanced `QueuedSender` already holds as many frames as its
    /// `QueueOptions::max_frames` allows.
    #[error("queued video frame limit reached")]
    Frames,
    /// The frames an Advanced `QueuedSender` retains leave too little room
    /// within `QueueOptions::max_bytes` for this frame. The frame itself fits
    /// the limit, so collecting completions makes room.
    #[error("queued video byte limit reached")]
    Bytes,
    /// An async File handle already has as many commands waiting as its
    /// `AsyncFileOptions::command_capacity` allows, and, for a request to
    /// stop, another request to stop waits beyond them.
    #[error("File command queue is full")]
    FileCommands,
    /// The 64 async File probes that may wait for the process's probe thread
    /// are already waiting.
    #[error("File probe queue is full")]
    FileProbes,
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

    #[test]
    fn diagnostic_payloads_have_the_common_error_traits() {
        fn error<T: std::error::Error + std::fmt::Debug + Clone + PartialEq>() {}
        fn value<T: Copy + Eq + std::hash::Hash>() {}

        error::<SelectionFailure>();
        error::<LoadFailure>();
        error::<LoadRefusal>();
        error::<DiscoveryAddressFailure>();
        error::<InputDefect>();
        error::<FrameDefect>();
        error::<LayoutDefect>();
        error::<SdkResultDefect>();
        error::<CapabilityRefusal>();
        error::<crate::CapabilityUnavailable>();
        error::<QueueLimit>();
        error::<ResourceKind>();
        error::<SelectionConflict>();
        error::<LibraryKind>();
        error::<ColorMetadataIssue>();
        error::<FrameRateDefect>();
        error::<VideoDimensionsDefect>();
        error::<AudioLengthDefect>();
        error::<CompressedAudioDefect>();
        error::<PtzDefect>();
        error::<SpawnFailure>();
        error::<InitializationFailure>();

        value::<SdkResultDefect>();
        value::<CapabilityRefusal>();
        value::<crate::CapabilityUnavailable>();
        value::<QueueLimit>();
        value::<ResourceKind>();
        value::<SelectionConflict>();
        value::<LibraryKind>();
        value::<ColorMetadataIssue>();
        value::<FrameRateDefect>();
        value::<VideoDimensionsDefect>();
        value::<AudioLengthDefect>();
        value::<CompressedAudioDefect>();
        value::<SpawnFailure>();
        value::<InitializationFailure>();
    }

    #[test]
    fn capability_errors_display_the_recorded_evidence() {
        let required = RuntimeFeature::Planar444Video.minimum_version().unwrap();
        for refusal in [
            CapabilityRefusal::SymbolUnavailable(c"NDIlib_routing_create"),
            CapabilityRefusal::BuildAbiUnavailable(c"NDIlib_routing_create_v2"),
            CapabilityRefusal::WrongEdition(Some(c"NDIlib_routing_create_v2")),
            CapabilityRefusal::WrongEdition(None),
            CapabilityRefusal::RuntimeTooOld { required },
            CapabilityRefusal::RuntimeVersionUnknown { required },
        ] {
            let error = Error::UnsupportedCapability {
                feature: RuntimeFeature::Routing,
                refusal,
            };
            assert_eq!(
                error.to_string(),
                format!("NDI capability source routing is unavailable: {refusal}")
            );
            assert!(!error.to_string().ends_with(": "));
        }
    }

    #[cfg(feature = "file-io")]
    #[test]
    fn file_family_errors_name_the_family_and_missing_entry_point() {
        let error = Error::FileFamilyUnavailable {
            family: crate::file::FileFamily::Recording,
            missing: c"NDIlib_file_recorder_rollover",
        };
        assert_eq!(error.to_string(), "NDI capability file recording is unavailable: the selected library does not export a required function: NDIlib_file_recorder_rollover");
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
            (QueueLimit::FileProbes, "File probe queue is full"),
        ] {
            assert_eq!(
                Error::QueueFull(limit).to_string(),
                format!("Queue full: {text}")
            );
        }
    }
}

#[cfg(test)]
mod initialization_failure_tests {
    use super::*;

    #[test]
    fn initialization_causes_preserve_their_diagnostics() {
        for (cause, expected) in [
            (
                InitializationFailure::NotStarted,
                "NDIlib_initialize returned false on a CPU the runtime reports as supported",
            ),
            (
                InitializationFailure::HandleCountExhausted,
                "NDI runtime reference count exhausted",
            ),
        ] {
            assert_eq!(cause.to_string(), expected);
        }
    }

    #[test]
    fn utf8_errors_keep_the_offset_and_incomplete_sequence() {
        let bytes = [b'a', 0xe2, 0x82];
        let cause = std::str::from_utf8(std::hint::black_box(&bytes)).unwrap_err();
        let error = Error::InvalidUtf8(cause);
        assert!(
            matches!(error, Error::InvalidUtf8(found) if found.valid_up_to() == 1 && found.error_len().is_none())
        );
        assert_eq!(
            error.to_string(),
            format!("Invalid UTF-8 string in data: {cause}")
        );
    }
}

// Preserve the sender capture diagnostic while exposing its typed SDK breach.
fn sdk_result_prefix(cause: &SdkResultDefect) -> &'static str {
    match cause {
        SdkResultDefect::UnexpectedFrameType { .. } => "Failed to capture frame: ",
        _ => "Unexpected NDI SDK result: ",
    }
}

#[cfg(test)]
mod full_diagnostic_tests {
    use super::*;

    #[test]
    fn error_groups_preserve_their_complete_diagnostic_prefixes() {
        for (error, expected) in [
            (Error::InvalidFrame(FrameDefect::Layout(crate::LayoutDefect::AudioRate(0.into()))), "Invalid frame data: Invalid sample rate: 0"),
            (Error::InvalidInput(InputDefect::RelativeLibraryPath), "Invalid input: an explicit NDI library path must be absolute"),
            (Error::UnexpectedSdkResult(SdkResultDefect::NegativeCount {operation: crate::sys::table::for_test((), c"NDIlib_send_get_no_connections").function(),value:(-1).into()}), "Unexpected NDI SDK result: NDIlib_send_get_no_connections returned a negative count: -1"),
            (Error::UnexpectedSdkResult(SdkResultDefect::UnexpectedFrameType {operation: crate::sys::table::for_test((), c"NDIlib_send_capture").function(),kind:17}), "Failed to capture frame: NDIlib_send_capture returned 17"),
            (Error::ConnectionLost, "Failed to capture frame: the SDK returned an error frame: the connection was lost"),
            (Error::InitializationFailed(InitializationFailure::NotStarted), "Failed to initialize the NDI runtime: NDIlib_initialize returned false on a CPU the runtime reports as supported"),
            (Error::InitializationFailed(InitializationFailure::HandleCountExhausted), "Failed to initialize the NDI runtime: NDI runtime reference count exhausted"),
        ] {
            assert_eq!(error.to_string(), expected);
        }
    }
}
