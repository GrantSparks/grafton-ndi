//! Discovery Server monitoring and explicitly enabled remote control.
//!
//! A Discovery Server keeps a registry of senders and receivers that goes
//! beyond what a [`Finder`](crate::Finder) reports: who is on the network, what
//! each receiver is connected to, and events they choose to publish. This
//! module has the two sides of that registry:
//!
//! * [`SendAdvertiser`] and [`RecvAdvertiser`] register this process's senders
//!   and receivers;
//! * [`SendListener`] and [`RecvListener`] list what is registered, subscribe
//!   to events, and — for receivers that allow it — request a source change.
//!
//! All four need a Discovery Server and an NDI 6.4.0 or later runtime; earlier
//! runtimes can crash when these objects are destroyed, so their constructors
//! return [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
//! there. [`NDI::capabilities`](crate::NDI::capabilities) reports each one.
//!
//! The native owners are [`Send`] but not [`Sync`]. Snapshots and events are
//! plain owned data.
//!
//! # Text from the network
//!
//! Names, metadata and event values are whatever a peer advertised. They are
//! decoded lossily, replacing bytes that are not UTF-8 with U+FFFD, so one
//! malformed peer cannot make a snapshot fail. An [`AdvertisedId`] is the
//! exception: it keeps the server's exact bytes, so handing it back in a
//! subscription or a connection request always names the same peer.
//!
//! # Example
//!
//! ```no_run
//! use grafton_ndi::{
//!     discovery::{ListenerOptions, RecvListener},
//!     NDI,
//! };
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! let ndi = NDI::new()?;
//! let options = ListenerOptions::default().server("127.0.0.1")?;
//! let mut listener = RecvListener::new(&ndi, &options)?;
//! listener.wait_for_changes(Duration::from_secs(2))?;
//! for receiver in listener.snapshot()? {
//!     println!("{:?} at {:?}", receiver.name(), receiver.address());
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod advertiser;
mod listener;
pub use advertiser::{
    AdvertiserOptions, RecvAdvertisement, RecvAdvertiser, RecvRegistration, SendAdvertisement,
    SendAdvertiser, SendRegistration,
};
pub use listener::{
    AdvertisedId, AdvertisedReceiver, AdvertisedSender, ListenerEvent, ListenerOptions,
    ReceiverCommand, ReceiverStream, RecvListener, SendListener, Wait,
};
