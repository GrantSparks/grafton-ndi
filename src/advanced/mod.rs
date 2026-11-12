//! NDI Advanced SDK operations.
//!
//! * Per-instance JSON configuration of senders, receivers, finders, routers
//!   and Discovery Server advertisers and listeners ([`AdvancedConfig`]),
//!   given to each through the `config` setter of its options.
//! * Compressed passthrough: receiving codec packets ([`PacketReceiver`]) and
//!   sending them ([`CompressedSender`]), including scatter-gather submission.
//! * Encoder guidance, keyframe demand and stream selection on the ordinary
//!   [`Sender`](crate::Sender) and [`Receiver`](crate::Receiver).
//! * Audio matched to video timestamps ([`AvSync`]) and reference-locked
//!   clocks ([`Genlock`]).
//! * Remote keyboard, mouse and touch control ([`KvmSession`]).
//! * Application-supplied memory for received frames ([`AllocatorService`]).
//! * Asynchronous sending with completion notification ([`QueuedSender`]).
//!
//! # Requirements
//!
//! The Advanced SDK headers may not be redistributed, so this module needs
//! three things, in this order:
//!
//! 1. the `advanced` Cargo feature;
//! 2. an installed NDI Advanced SDK when the crate is built (`NDI_SDK_DIR`, or
//!    a default installation directory), from which the native declarations
//!    are generated on the build machine;
//! 3. an Advanced SDK runtime selected when the program runs.
//!
//! The API is identical whether or not the build found the Advanced SDK.
//! Without it, and against a Standard runtime, every native operation returns
//! [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability) with
//! the reason; the data types in this module (packets, envelopes, options)
//! work everywhere. Core operations are unaffected in every case.
//! An older installed Advanced SDK disables only the declaration families or
//! receive presets it lacks. The remaining operations keep their native ABI
//! checks and still require the matching runtime exports.
//!
//! Enabling this module neither selects an SDK edition nor grants a license
//! entitlement.
//!
//! # Ownership
//!
//! An operation that takes ownership of a receiver, a sender or a frame hands
//! it back inside [`Rejected`](crate::Rejected) when it fails.

#![warn(missing_docs)]
// An operation that fails hands its input back inside the error, so these
// errors are as large as the frames and objects they return. Boxing them
// would add an allocation to the very paths (back-pressure, validation) that
// exist so that nothing is lost.
#![allow(clippy::result_large_err)]
// A build without the Advanced SDK declarations compiles no `native!` body,
// which leaves the parameters, imports and helpers those bodies use without a
// reader. Every build that has the declarations enforces these lints.
#![cfg_attr(
    not(ndi_has_complete_advanced_header),
    allow(
        dead_code,
        unreachable_code,
        unused_imports,
        unused_mut,
        unused_variables
    )
)]

mod aliasable;
mod allocators;
mod config;
mod configured;
mod controls;
mod descriptors;
mod finder;
mod kvm;
mod packets;
mod queued;
mod receive;
mod routing;
mod send;
mod sync;

pub use allocators::*;
pub use config::AdvancedConfig;
pub(crate) use configured::Configurable;
pub use controls::*;
pub use finder::ExtendedSource;
pub use kvm::*;
pub use packets::*;
pub use queued::*;
pub use receive::*;
pub use send::*;
pub use sync::*;

/// [`Error::InvalidFrame`](crate::Error::InvalidFrame) with `message`: a frame
/// or packet that is malformed, or that cannot be built or sent.
fn invalid_frame(message: impl Into<String>) -> crate::Error {
    crate::Error::InvalidFrame(message.into())
}

/// [`Error::InvalidInput`](crate::Error::InvalidInput) with `message`: a value
/// this module was given that it cannot use, whatever frame it is for.
fn invalid_input(message: impl Into<String>) -> crate::Error {
    crate::Error::InvalidInput(message.into())
}

/// `value` when it is in `1..=i32::MAX`, the range of the SDK's signed
/// quantities: dimensions, sample rates, channel and sample counts.
fn sdk_quantity(value: impl Into<i64>) -> Option<u32> {
    u32::try_from(value.into())
        .ok()
        .filter(|value| (1..=i32::MAX.unsigned_abs()).contains(value))
}
