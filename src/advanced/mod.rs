//! Optional Advanced SDK operations, resolved from the selected runtime.
//!
//! Enabling this module does not select an SDK edition or grant an entitlement.
//! Missing entry points return an error while existing core operations remain usable.
//! Configuration strings remain owned until the corresponding native object closes.

mod config;
mod controls;
pub(crate) mod ffi;
mod kvm;
mod packets;

pub use config::AdvancedConfig;
pub use controls::*;
pub use kvm::*;
pub use packets::*;

mod receive;
pub use receive::*;

mod sync;
pub use sync::*;

mod queued;
pub use queued::*;

mod allocators;
pub use allocators::*;

mod send;
pub use send::*;
