//! High-performance Rust bindings for the NDI® 6 SDK (Network Device Interface).
//!
//! This crate provides safe, idiomatic Rust bindings for the NDI SDK, enabling
//! real-time, low-latency video/audio streaming over IP networks. NDI is widely
//! used in broadcast, live production, and video conferencing applications.
//!
//! # Quick Start
//!
//! ```no_run
//! use grafton_ndi::{NDI, FinderOptions, Finder};
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! // Initialize the NDI runtime
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
//! The [`NDI`] struct manages the NDI runtime lifecycle. Create a handle before
//! constructing SDK resources; those resources retain their own runtime
//! references. [`SourceCache`] acquires the runtime on cache misses, while its
//! cached owned source descriptors need no live runtime handle.
//!
//! ## Source Discovery
//!
//! Use [`Finder`] to discover NDI sources on the network. Sources can be filtered
//! by groups and additional IP addresses can be specified for discovery.
//!
//! ## Receiving
//!
//! The [`Receiver`] type handles receiving video, audio, and metadata from NDI
//! sources. It supports various color formats and bandwidth modes.
//!
//! ## Sending
//!
//! Use [`Sender`] to transmit video, audio, and metadata as an NDI source.
//! Senders can be configured with clock settings and group assignments.
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
//!
//! # Performance
//!
//! - **Borrowed receive buffers**: Read SDK-owned frames without wrapper pixel copies
//! - **Async completion**: Owned frames are recovered after SDK synchronization
//! - **Bandwidth control**: Multiple quality levels for different use cases
//! - **SDK processing**: Encoding and acceleration depend on the installed NDI SDK
//!
//! # Runtime and platform support
//!
//! Building this crate needs no NDI SDK. Desktop platforms and Android load the
//! NDI runtime library when the first [`NDI`] handle is created, searching next
//! to the executable, then `NDI_RUNTIME_DIR_V6`, then the platform loader's own
//! path; see the [`runtime`] module. One successfully loaded module stays
//! selected for the process, and [`RuntimeOptions`] can name an exact library
//! or SDK edition before that first load. [`NDI::new`] preserves normal
//! initialize/destroy cycles.
//!
//! Functions a runtime does not export never prevent the rest of the API from
//! working: [`NDI::capabilities`] reports each optional capability, without
//! assuming hardware or license availability, and the affected operations
//! return [`Error::UnsupportedCapability`].
//!
//! The `advanced` and `file-io` features add optional APIs. The Advanced
//! wrappers need the Advanced SDK headers at build time (`NDI_SDK_DIR` or a
//! default installation); without them the same API compiles and reports that
//! it is unavailable. `advanced_sdk` remains a no-op compatibility flag.
//!
//! Apple mobile targets link the vendor's static archive instead, selected from
//! `NDI_SDK_DIR` at build time, with the File archive as the sole provider when
//! it embeds the NDI core.

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
        LineStrideOrSize, MetadataFrame, MetadataFrameRef, PixelFormat, PixelFormatInfo, ScanType,
        VideoFrame, VideoFrameBuilder, VideoFrameRef, VideoPlane, VideoPlanes, VideoRows,
    },
    framesync::{FrameSync, FrameSyncAudioRef, FrameSyncAudioRequest, FrameSyncVideoRef},
    receiver::{
        Capture, ConnectionStats, FrameSyncAttached, RawCapture, Receiver, ReceiverBandwidth,
        ReceiverColorFormat, ReceiverEvent, ReceiverMode, ReceiverOptions, ReceiverOptionsBuilder,
        ReceiverStatus, SourceNameChange, Tally,
    },
    runtime::{ProcessLifetimeRuntime, RuntimeOptions, NDI},
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
