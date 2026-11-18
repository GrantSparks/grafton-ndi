//! NDI Advanced SDK operations.
//!
//! * Per-instance JSON configuration through the `config` setter of
//!   supported options. See [`AdvancedConfig`] for the configurable objects.
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

mod aliasable;
mod allocators;
mod config;
mod configured;
mod controls;
mod descriptors;
mod finder;
mod kvm;
#[cfg(ndi_has_advanced_kvm)]
pub(crate) use kvm::TouchPoints;
mod kvm_receiver;
mod packets;
mod queued;
mod receive;
mod reserved_map;
mod routing;
mod send;
mod send_slots;
mod sync;

/// Receiver mode held by [`PacketReceiver`], [`AllocatedReceiver<PacketAttached>`]
/// and their [`KvmReceiver`] views. Packet capture reserves video, audio, metadata
/// and event capture for the packet owner and its KVM view.
#[derive(Debug)]
pub enum PacketAttached {}

/// Receiver mode held by [`AvSync`], which reserves audio for synchronization.
/// Video, metadata and common controls remain available through its receiver.
/// Query capabilities, [`crate::Receiver::wait_for_source_name_change`] and
/// connection statistics because synchronization may consume notifications.
#[derive(Debug)]
pub enum AvSyncAttached {}

/// Sender mode held exclusively by [`CompressedSender`].
/// Video submission belongs to the wrapper; audio goes through its sender.
#[derive(Debug)]
pub enum CompressedAttached {}

/// Sender mode held exclusively by [`QueuedSender`].
/// It submits video and sends both uncompressed and compressed audio.
#[derive(Debug)]
pub enum QueueAttached {}

/// Sealed sender modes that send compressed audio: compressed and queued.
/// Their constructors require an Advanced runtime.
pub trait CompressedAudioSendMode: crate::sender::SenderMode {}
pub use crate::framesync::capture::FrameSyncCapture;
pub use allocators::*;
pub use config::AdvancedConfig;
pub(crate) use config::RetainedConfig;
pub(crate) use configured::Configurable;
pub use controls::*;
pub use finder::ExtendedSource;
pub use kvm::*;
pub use kvm_receiver::KvmReceiver;
pub use packets::*;
pub(crate) use queued::QueueLoans;
pub use queued::*;
pub use receive::*;
#[cfg(ndi_has_advanced_scatter_send)]
pub(crate) use send::ScatterVideoLoan;
pub use send::*;
pub(crate) use send::{PendingStreams, PendingVideo, ScatterSendable};
pub use sync::*;

/// [`Error::InvalidFrame`](crate::Error::InvalidFrame) with its typed `cause`: a frame
/// or packet that is malformed, or that cannot be built or sent.
fn invalid_frame(cause: crate::FrameDefect) -> crate::Error {
    crate::Error::InvalidFrame(cause)
}

/// [`Error::InvalidInput`](crate::Error::InvalidInput) with its typed `cause`: a value
/// this module was given that it cannot use, whatever frame it is for.
fn invalid_input(cause: crate::InputDefect) -> crate::Error {
    crate::Error::InvalidInput(cause)
}
