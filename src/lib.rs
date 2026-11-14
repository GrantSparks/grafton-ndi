//! Discover NDI sources and send and receive video, audio and metadata.
//!
//! Supports NDI 6 with optional async adapters, image encoding, Advanced SDK
//! operations, and recording/playback. Default desktop and Android builds need
//! no SDK at build time. Advanced native operations need Advanced headers and
//! libclang; Apple mobile targets link an SDK archive.
//!
//! # Quick Start
//!
//! ```no_run
//! use grafton_ndi::{Ndi, FinderOptions, Finder};
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! // Select the default NDI runtime and start it
//! let ndi = Ndi::new()?;
//!
//! // Find sources on the network
//! let finder = Finder::new(&ndi, &FinderOptions::new())?;
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
//! # Choose a workflow
//!
//! - [`runtime`]: runtime selection, initialization and lifetime.
//! - [`finder`]: source discovery and caching.
//! - [`receiver`] and [`sender`]: capture and publish media.
//! - [`frames`] and [`framesync`]: frame layouts and clock-driven capture.
//! - [`capabilities`]: check optional runtime operations.
//! - [`routing`] and [`discovery`]: redirect connections and use Discovery Servers.
//! - [`audio_conversion`] and [`video_conversion`]: checked media conversions.
//!
//! Cargo features enable `asynchronous` (`tokio` or `smol`), `advanced`,
//! `file` (`file-io`), and image encoders (`image-encoding`). None is on by default.
//!
//! See the [user guides](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/README.md#guides-and-examples)
//! for setup, platform support, examples, limitations and migration instructions.

#![warn(clippy::return_self_not_must_use)]
// Android has no native thread-locals, so std expands `thread_local!` through
// its OS-key implementation, which wraps even a `const { .. }` initializer in
// an ordinary function; clippy then asks for the `const` the test-only
// thread-locals already have. Plain `allow`: an expectation would go unfulfilled
// in an Android build without tests.
#![cfg_attr(
    all(test, target_os = "android"),
    allow(
        clippy::missing_const_for_thread_local,
        reason = "false positive: the OS-key thread_local! expansion hides the const initializer"
    )
)]

mod capture;
mod color_info;
mod error;
mod media;
mod pixels;
mod rejected;
mod sdk_copy;
mod sys;
mod time;
mod timeout;

pub mod framesync;

pub mod audio_conversion;
pub mod finder;
pub mod frames;
pub mod receiver;
pub mod runtime;
pub mod sender;
pub mod video_conversion;

pub mod capabilities;
pub mod discovery;
pub mod routing;

#[cfg(feature = "advanced")]
pub mod advanced;

#[cfg(feature = "file-io")]
pub mod file;

#[cfg(any(feature = "tokio", feature = "smol"))]
pub mod asynchronous;

#[cfg(feature = "image-encoding")]
mod image;

pub use {
    capabilities::{
        Capabilities, CapabilityStatus, CapabilityUnavailable, RuntimeFeature, RuntimeInfo,
        SdkEdition, SdkLinkage, SdkVersion,
    },
    capture::{AudioKind, CaptureKind, MetadataKind, VideoKind},
    error::*,
    finder::{Finder, FinderOptions, Source, SourceAddress, SourceCache},
    frames::{
        AudioChannels, AudioFormat, AudioFrame, AudioFrameBuilder, AudioFrameRef, AudioLayout,
        AudioSource, BorrowedAudioFrame, BorrowedAudioFrameBuilder, FormatCategory, MetadataFrame,
        MetadataFrameRef, MetadataSource, PixelFormat, PixelFormatInfo, ScanType, VideoFrame,
        VideoFrameBuilder, VideoFrameRef, VideoPlane, VideoPlanes, VideoRows, VideoSource,
    },
    framesync::{
        AudioInputFormat, FrameSync, FrameSyncAudioRef, FrameSyncAudioRequest, FrameSyncCapture,
        FrameSyncVideoRef,
    },
    receiver::{
        Capture, CaptureMode, ConnectionStats, FrameSyncAttached, MetadataMode, Ptz, RawCapture,
        Receiver, ReceiverBandwidth, ReceiverColorFormat, ReceiverEvent, ReceiverItem,
        ReceiverItemRef, ReceiverMode, ReceiverOptions, ReceiverStatus, SettingChange,
        SourceNameChange, Tally, VideoMode,
    },
    runtime::{Ndi, ProcessLifetimeRuntime, Runtime, RuntimeOptions},
    sender::{
        AudioSendMode, BorrowedVideoFrame, BorrowedVideoFrameBuilder, RawSend, Sender, SenderMode,
        SenderOptions,
    },
};

#[cfg(feature = "advanced")]
pub use sender::{CompressedAttached, QueueAttached};

pub use color_info::{ColorInfo, ColorPrimaries, TransferFunction, YuvMatrix};
pub use media::{FourCc, FrameRate, MediaTypes, StreamTier};
pub use rejected::Rejected;
pub use time::{SdkTimestamp, Timecode};

#[cfg(feature = "image-encoding")]
pub use image::{
    AlphaMode, ColorPolicy, Downscale, HdrPolicy, ImageFormat, ImageOptions, ImageWorkspace,
    PngCompression, SampleDepth, YuvRange,
};

/// Alias for Result with our Error type
pub type Result<T> = std::result::Result<T, crate::error::Error>;

/// The largest finite timeout: `u32::MAX - 1` milliseconds (~49.7 days).
///
/// The NDI SDK takes every timeout as a `uint32_t` count of milliseconds and
/// reserves the largest value, 0xFFFFFFFF, for a wait without a limit (see
/// the event functions in `Processing.NDI.RecvListener.h`). A finite wait is
/// therefore never passed to the SDK as `u32::MAX`. A longer timeout is
/// refused with [`Error::InvalidInput`].
///
/// A timeout is rounded up to whole milliseconds, so a non-zero wait shorter
/// than a millisecond becomes one millisecond, and only [`Duration::ZERO`]
/// polls without blocking.
///
/// [`Duration::ZERO`]: std::time::Duration::ZERO
pub const MAX_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(u32::MAX as u64 - 1);

pub(crate) use timeout::to_ms_checked;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
mod test_alloc;

#[cfg(test)]
mod test_support;
