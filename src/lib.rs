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
//! finder.wait_for_sources(5000);
//! let sources = finder.get_sources(0)?;
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
//! The [`NDI`] struct manages the NDI runtime lifecycle. It must be created before
//! any other NDI operations and should be kept alive for the duration of your
//! application's NDI usage.
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
//! as the underlying NDI SDK is thread-safe. However, for optimal performance,
//! minimize cross-thread operations and maintain thread affinity where possible.
//!
//! ## Zero-Copy Async Sending
//!
//! The library provides zero-copy async video sending using `NDIlib_send_send_video_async_v2`.
//! The completion callback notifies when the buffer can be reused:
//!
//! ```no_run
//! # use grafton_ndi::{NDI, SenderOptions, BorrowedVideoFrame, FourCCVideoType};
//! # let ndi = NDI::new().unwrap();
//! # let sender = grafton_ndi::Sender::new(&ndi, &SenderOptions::builder("Test").build().unwrap()).unwrap();
//! // Register callback to know when buffer is released
//! sender.on_async_video_done(|len| println!("Buffer released: {} bytes", len));
//!
//! let buffer = vec![0u8; 1920 * 1080 * 4];
//! let frame = BorrowedVideoFrame::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);
//! let token = sender.send_video_async(&frame);
//! // Buffer is now owned by NDI - cannot be modified until callback fires
//! // The AsyncVideoToken must be kept alive to track the operation
//! ```
//!
//! Note: Only video supports async sending in the NDI SDK. Audio and metadata are always synchronous.
//!
//! # Performance
//!
//! - **Zero-copy**: Frame data directly references NDI's buffers when possible
//! - **Lock-free async**: Atomic operations for minimal overhead in hot paths
//! - **Bandwidth control**: Multiple quality levels for different use cases
//! - **Hardware acceleration**: Automatically uses GPU acceleration when available
//!
//! # Platform Support
//!
//! - **Windows**: Full support, tested on Windows 10/11
//! - **Linux**: Full support, tested on Ubuntu 20.04+
//! - **macOS**: Experimental support with limited testing

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::missing_errors_doc)]

// Internal modules
mod error;
mod ndi_lib;

// Public modules
pub mod finder;
pub mod frames;
pub mod receiver;
pub mod runtime;
pub mod sender;

// Re-exports
pub use {
    error::*,
    finder::{Finder, FinderOptions, FinderOptionsBuilder, Source, SourceAddress},
    frames::{
        AudioFrame, AudioFrameBuilder, AudioType, FourCCVideoType, FrameFormatType,
        LineStrideOrSize, MetadataFrame, VideoFrame, VideoFrameBuilder,
    },
    receiver::{
        FrameType, Receiver, ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions,
        ReceiverOptionsBuilder, ReceiverStatus, Tally,
    },
    runtime::NDI,
    sender::{AsyncVideoToken, BorrowedVideoFrame, Sender, SenderOptions, SenderOptionsBuilder},
};

/// Alias for Result with our Error type
pub type Result<T> = std::result::Result<T, crate::error::Error>;

// Tests
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
