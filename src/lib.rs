//! High-performance Rust bindings for the NDI® 6 SDK (Network Device Interface).
//!
//! This crate provides safe, idiomatic Rust bindings for the NDI SDK, enabling
//! real-time, low-latency video/audio streaming over IP networks. NDI is widely
//! used in broadcast, live production, and video conferencing applications.
//!
//! # Quick Start
//!
//! ```no_run
//! use grafton_ndi::{NDI, Finder, Find};
//!
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! // Initialize the NDI runtime
//! let ndi = NDI::new()?;
//!
//! // Find sources on the network
//! let finder = Finder::builder().show_local_sources(true).build();
//! let find = Find::new(&ndi, &finder)?;
//!
//! // Discover sources
//! find.wait_for_sources(5000);
//! let sources = find.get_sources(0)?;
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
//! Use [`Find`] to discover NDI sources on the network. Sources can be filtered
//! by groups and additional IP addresses can be specified for discovery.
//!
//! ## Receiving
//!
//! The [`Receiver`] type handles receiving video, audio, and metadata from NDI
//! sources. It supports various color formats and bandwidth modes.
//!
//! ## Sending
//!
//! Use [`SendInstance`] to transmit video, audio, and metadata as an NDI source.
//! Senders can be configured with clock settings and group assignments.
//!
//! # Thread Safety
//!
//! All primary types ([`Find`], [`Receiver`], [`SendInstance`]) implement `Send + Sync`
//! as the underlying NDI SDK is thread-safe. However, for optimal performance,
//! minimize cross-thread operations and maintain thread affinity where possible.
//!
//! # Performance
//!
//! - **Zero-copy**: Frame data directly references NDI's buffers when possible
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
#![allow(clippy::wildcard_imports)] // We use wildcard imports for FFI bindings
#![allow(clippy::must_use_candidate)] // Too many false positives
#![allow(clippy::missing_errors_doc)] // Error types are self-documenting

// Internal modules
mod error;
mod ndi_lib;

// Public modules
pub mod finder;
pub mod frames;
pub mod receiver;
pub mod runtime;
pub mod sender;

// Re-export main types from modules
pub use error::*;
pub use finder::{Find, Finder, FinderBuilder, Source, SourceAddress};
pub use frames::{
    AudioFrame, AudioFrameBuilder, AudioType, FourCCVideoType, FrameFormatType, LineStrideOrSize,
    MetadataFrame, VideoFrame, VideoFrameBuilder,
};
pub use receiver::{
    FrameType, Receiver, ReceiverBuilder, Recv, RecvBandwidth, RecvColorFormat, RecvStatus, Tally,
};
pub use runtime::NDI;
pub use sender::{
    AsyncVideoToken, SendInstance, SendOptions, SendOptionsBuilder, VideoFrameBorrowed,
};

// Tests
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
