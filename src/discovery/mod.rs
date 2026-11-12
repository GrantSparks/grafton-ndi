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
//! All four need an NDI 6.4.0 or later runtime; earlier runtimes can crash
//! when these objects are destroyed, so their constructors return
//! [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
//! there. [`Ndi::capabilities`](crate::Ndi::capabilities) reports each one.
//!
//! The native owners are [`Send`] but not [`Sync`]. Snapshots and events are
//! plain owned data.
//!
//! # Connecting
//!
//! Each object is created for a Discovery Server address: the one given in its
//! options, or else the one in the NDI configuration. Creation fails only when
//! there is no address or the SDK cannot parse it. It does not wait for the
//! server, and a server that cannot be reached is not an error: the object
//! keeps trying in the background for as long as it lives, and starts working
//! a few seconds after the server appears.
//!
//! A listener reports its connection through `is_connected`, and its snapshots
//! are empty until it connects. An advertiser reports nothing; whether its
//! registrations arrived shows in a listener on the same server.
//!
//! # Text from the network
//!
//! Names, metadata and event values are whatever a peer advertised. They are
//! decoded lossily, replacing bytes that are not UTF-8 with U+FFFD, so one
//! malformed peer cannot make a snapshot fail. Identifiers are the exception:
//! a [`SenderId`], [`ReceiverId`] or [`InputGroupId`] keeps the server's exact
//! bytes, so handing one back in a subscription or a connection request always
//! names the same peer. Each kind is its own type, so a sender's identifier
//! cannot be used where a receiver's is meant.
//!
//! # Example
//!
//! ```no_run
//! use grafton_ndi::{
//!     discovery::{ListenerOptions, RecvListener},
//!     Ndi,
//! };
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! let ndi = Ndi::new()?;
//! let options = ListenerOptions::default().server("127.0.0.1");
//! let mut listener = RecvListener::new(&ndi, &options)?;
//! // The listener connects in the background; give it time to hear from the
//! // server before reading the registry.
//! listener.wait_for_changes(Duration::from_secs(2))?;
//! if !listener.is_connected() {
//!     println!("no answer from the Discovery Server yet");
//! }
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
    AdvertisedReceiver, AdvertisedSender, InputGroupId, ListenerEvent, ListenerOptions,
    ReceiverCommand, ReceiverId, ReceiverStream, RecvListener, SendListener, SenderId,
};

/// The error for an advertiser or listener the SDK did not create.
///
/// The SDK creates these objects for any address it can parse, reachable or
/// not, so a refusal is about the configuration: `server` is the address that
/// was given.
fn not_created(object: &str, server: Option<&str>) -> crate::Error {
    crate::Error::InvalidInput(match server {
        Some(server) => format!(
            "the SDK created no {object}: it does not accept {server:?} as a Discovery Server \
             address"
        ),
        None => format!(
            "the SDK created no {object}: no Discovery Server was named and the NDI \
             configuration supplies none"
        ),
    })
}

/// A native object handle, which is null when the SDK created nothing.
trait Handle: Copy {
    fn is_null(self) -> bool;
}

impl<T> Handle for *mut T {
    fn is_null(self) -> bool {
        <*mut T>::is_null(self)
    }
}
