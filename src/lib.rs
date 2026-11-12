//! Rust bindings for the NDI® 6 SDK (Network Device Interface).
//!
//! This crate wraps the NDI SDK in Rust ownership types for discovering
//! sources and for sending and receiving video, audio and metadata over IP
//! networks. Building it needs no NDI SDK: the NDI runtime library is loaded
//! when a runtime is selected.
//!
//! # Supported NDI versions and platforms
//!
//! The crate works with NDI 6 runtimes from the Standard and the Advanced SDK,
//! and needs Rust 1.88 or later. Version 3.0 was tested against the Standard
//! 6.1.1 and the Standard and Advanced 6.3.2 and 6.4.0.2 runtimes on Linux
//! x86_64, and its bindings are generated from the 6.4.0.2 headers. Other NDI 6
//! releases have not been tested. A runtime provides only what its version
//! offers: an operation it cannot perform returns
//! [`Error::UnsupportedCapability`], and [`NDI::capabilities`] reports each
//! capability in advance.
//!
//! Windows, macOS, Linux and Android load the NDI runtime library when a
//! runtime is selected, searching next to the executable, then
//! `NDI_RUNTIME_DIR_V6`, then the platform loader's own path; see the
//! [`runtime`] module. A library that cannot be found is reported as
//! [`Error::RuntimeLoadFailed`]. The selected library stays selected, and
//! loaded, for the life of the process, and [`RuntimeOptions`] can name an
//! exact library or SDK edition for that selection. NDI itself starts and
//! stops with the [`NDI`] handles.
//!
//! iOS, tvOS and visionOS targets link the static archive of the NDI SDK for
//! Apple instead, selected from `NDI_SDK_DIR` at build time. With `file-io`,
//! the File archive replaces the core archive when the SDK supplies one.
//!
//! The [README](https://github.com/GrantSparks/grafton-ndi#supported-ndi-versions-and-platforms)
//! lists the Rust targets, what each NDI version provides, and how each target
//! was checked.
//!
//! # Quick Start
//!
//! ```no_run
//! use grafton_ndi::{NDI, FinderOptions, Finder};
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! // Select the default NDI runtime and start it
//! let ndi = NDI::new()?;
//!
//! // Find sources on the network
//! let options = FinderOptions::builder().show_local_sources(true).build();
//! let finder = Finder::new(&ndi, &options)?;
//!
//! // Discover sources
//! let sources = finder.find_sources(Duration::from_secs(5))?;
//!
//! for source in sources {
//!     println!("Found: {}", source);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Core Concepts
//!
//! ## Runtime Management
//!
//! Using NDI takes two steps. [`Runtime::select`] loads the NDI runtime library
//! and pins it for the process without starting NDI; the [`Runtime`] it returns
//! describes the library. [`Runtime::initialize`] starts NDI and returns an
//! [`NDI`] handle. [`NDI::new`] does both with the default options. Create a
//! handle before constructing SDK resources; those resources retain their own
//! handles, and NDI stops when the last one is dropped. The [`runtime`] module
//! describes where the runtime library is loaded from and how
//! [`RuntimeOptions`] selects one.
//!
//! ## Source Discovery
//!
//! Use [`Finder`] to discover NDI sources on the network. Sources can be filtered
//! by groups and additional IP addresses can be specified for discovery. See
//! the [`finder`] module.
//!
//! [`SourceCache`] remembers which source belongs to a host, so repeated
//! lookups of a known device skip discovery. A lookup takes the [`NDI`] handle;
//! the cached source descriptors need none.
//!
//! ```no_run
//! use grafton_ndi::{SourceCache, NDI};
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! let ndi = NDI::new()?;
//! let cache = SourceCache::new();
//! let camera = cache.find_by_host(&ndi, "192.168.1.100", Duration::from_secs(5))?;
//! println!("Found: {camera}");
//! # Ok(())
//! # }
//! ```
//!
//! ## Receiving
//!
//! The [`Receiver`] type handles receiving video, audio, and metadata from NDI
//! sources. It supports various color formats and bandwidth modes. See the
//! [`receiver`] module, and [`framesync`] for clock-corrected pull capture.
//!
//! ## Sending
//!
//! Use [`Sender`] to transmit video, audio, and metadata as an NDI source.
//! Senders can be configured with clock settings and group assignments. See
//! the [`sender`] and [`frames`] modules.
//!
//! ## Optional capabilities
//!
//! NDI runtimes differ in what they provide. [`NDI::capabilities`] reports
//! each optional capability of the selected runtime, without assuming
//! hardware or license availability, and an operation whose capability is
//! missing returns [`Error::UnsupportedCapability`]; the rest of the API keeps
//! working. See the [`capabilities`] module.
//!
//! ## Further modules and features
//!
//! - [`routing`]: a source that redirects its receivers to another source.
//! - [`discovery`]: registration with, and listing of, an NDI Discovery Server.
//! - [`utilities`] and [`video_conversion`]: checked audio and video
//!   conversions.
//! - `advanced` (Cargo feature `advanced`): NDI Advanced SDK operations. They
//!   need the Advanced SDK headers at build time (`NDI_SDK_DIR` or a default
//!   installation); without them the same API compiles and reports that it is
//!   unavailable.
//! - `file` (Cargo feature `file-io`): recording and playback through the NDI
//!   File API.
//! - `tokio` and `smol` (Cargo features of the same names): receivers for the
//!   named async runtime.
//! - PNG, JPEG and data-URL encoding of video frames, with the default
//!   `image-encoding` feature.
//!
//! # Thread Safety
//!
//! All primary types ([`Finder`], [`Receiver`], [`Sender`]) implement `Send + Sync`
//! with internal synchronization where required by the SDK. Borrowed receiver
//! frame references support scoped thread transfer; FrameSync references remain
//! tied to their owner and cannot be shared or transferred across threads.
//! Immutable [`VideoPlane`], [`VideoRows`], and [`VideoPlanes`] views are
//! `Send + Sync` from either capture family. Scoped workers may read those
//! borrowed pixels while the frame guard stays on its permitted freeing thread.
//!
//! ## Async Sending
//!
//! [`Sender::send_video_async`] takes ownership of a [`VideoFrame`] without
//! copying its pixel or metadata allocations. The next submission returns the
//! previous frame after SDK synchronization; finish recovers the final frame.
//!
//! ```no_run
//! # use grafton_ndi::{NDI, SenderOptions, VideoFrame, PixelFormat};
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! # let ndi = NDI::new()?;
//! # let mut sender = grafton_ndi::Sender::new(&ndi, &SenderOptions::builder("Test").build())?;
//! let first = VideoFrame::builder().resolution(1920, 1080).pixel_format(PixelFormat::BGRA).build()?;
//! let second = VideoFrame::builder().resolution(1920, 1080).pixel_format(PixelFormat::BGRA).build()?;
//! assert!(sender.send_video_async(first).is_none());
//! let mut reusable = sender.send_video_async(second).unwrap();
//! reusable.data_mut().fill(0);
//! let final_frame = sender.finish_video_async().unwrap();
//! # Ok(())
//! # }
//! ```
//!
//! [`Sender::send_video_async_copy`] explicitly copies a borrowed buffer into
//! an owned frame, then uses the same pipeline and returns the previous frame.
//! Source buffers can be reused immediately. Both paths use default SDK
//! synchronization, with no SDK completion callback registration. Optional Rust
//! completion handlers run on the calling
//! thread after synchronization, outside internal locks.
//! The optional `advanced::QueuedSender` API provides a separate bounded queue
//! using native completion callbacks when the selected runtime supplies them.
//!
//! Note: Only video supports async sending in the NDI SDK. Audio and metadata are always synchronous.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::missing_errors_doc)]

mod capture;
mod error;
mod recv_guard;
mod rejected;
mod sys;

pub mod framesync;

pub mod finder;
pub mod frames;
pub mod receiver;
pub mod runtime;
pub mod sender;
mod timestamp;
pub mod utilities;
pub mod video_conversion;

pub mod capabilities;
pub mod discovery;
pub mod routing;

#[cfg(feature = "advanced")]
pub mod advanced;

#[cfg(feature = "file-io")]
pub mod file;

#[cfg(any(feature = "tokio", feature = "smol"))]
mod async_runtime;

#[cfg(feature = "tokio")]
pub use async_runtime::tokio;

#[cfg(feature = "smol")]
pub use async_runtime::smol;

pub use {
    capabilities::{
        Capabilities, CapabilityStatus, CapabilityUnavailable, RuntimeFeature, RuntimeInfo,
        SdkEdition, SdkLinkage, SdkVersion,
    },
    capture::{AudioKind, CaptureKind, MetadataKind, VideoKind},
    error::*,
    finder::{Finder, FinderOptions, FinderOptionsBuilder, Source, SourceAddress, SourceCache},
    frames::{
        AudioFormat, AudioFrame, AudioFrameBuilder, AudioFrameRef, AudioLayout, FormatCategory,
        MetadataFrame, MetadataFrameRef, PixelFormat, PixelFormatInfo, ScanType, VideoFrame,
        VideoFrameBuilder, VideoFrameRef, VideoPlane, VideoPlanes, VideoRows,
    },
    framesync::{FrameSync, FrameSyncAudioRef, FrameSyncAudioRequest, FrameSyncVideoRef},
    receiver::{
        Capture, ConnectionStats, FrameSyncAttached, RawCapture, Receiver, ReceiverBandwidth,
        ReceiverColorFormat, ReceiverEvent, ReceiverMode, ReceiverOptions, ReceiverOptionsBuilder,
        ReceiverStatus, SourceNameChange, Tally,
    },
    runtime::{ProcessLifetimeRuntime, Runtime, RuntimeOptions, NDI},
    sender::{
        BorrowedVideoFrame, BorrowedVideoFrameBuilder, Sender, SenderOptions, SenderOptionsBuilder,
    },
};

pub use rejected::Rejected;
pub use timestamp::SdkTimestamp;

#[cfg(feature = "image-encoding")]
pub use frames::ImageFormat;

/// Alias for Result with our Error type
pub type Result<T> = std::result::Result<T, crate::error::Error>;

/// Maximum timeout duration supported by the NDI SDK (~49.7 days).
///
/// The NDI SDK uses `u32` milliseconds for timeout values, which limits the maximum
/// timeout to approximately 49.7 days. Attempting to use a larger `Duration` will
/// result in an error.
pub const MAX_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(u32::MAX as u64);

/// Converts a `Duration` to milliseconds for FFI, checking for overflow.
///
/// # Errors
///
/// Returns [`Error::InvalidConfiguration`] if the duration exceeds [`MAX_TIMEOUT`].
///
pub(crate) fn to_ms_checked(d: std::time::Duration) -> Result<u32> {
    if d > MAX_TIMEOUT {
        Err(Error::InvalidConfiguration(format!(
            "timeout {:?} exceeds MAX_TIMEOUT (~49.7 days)",
            d
        )))
    } else {
        Ok(d.as_millis() as u32)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
mod test_alloc;

// Shared with the integration tests, which use helpers the unit tests do not.
#[cfg(test)]
#[allow(dead_code)]
mod test_support;
