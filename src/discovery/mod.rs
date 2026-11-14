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
//! All four need an NDI 6.4.0 or later runtime, so their constructors return
//! [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
//! before that. [`Ndi::capabilities`](crate::Ndi::capabilities) reports each
//! one. A 6.3.2 runtime exports their functions, but destroying any of them
//! while it is connected to a server aborts the process, and no order of
//! calls avoids it.
//!
//! # SDK defects that remain in 6.4
//!
//! Four defects of the NDI 6.4.0.2 runtime involve these objects. The crate
//! cannot prevent them, but none needs more than a little care to avoid:
//!
//! * Moving one sender or receiver from one advertiser to another on the same
//!   server within a few tens of milliseconds can crash a listener on that
//!   server, in any process. Wait a second between unregistering and
//!   registering again, or drop the first advertiser before registering with
//!   the second.
//! * A [`RecvListener::unsubscribe`] can permanently block the connect of the
//!   receiver it names, in this process or another, when that receiver
//!   connects at the same moment. To stop receiving a receiver's events, drop
//!   the listener; this crate's listeners never unsubscribe on their own.
//! * Destroying a [`SendAdvertiser`] can block forever, about once in a
//!   million destroys under heavy load.
//! * A listener that does not find its server on its first attempt, such as
//!   one created before the server appeared, connects on a later attempt but
//!   then never lists the senders or receivers registered before it
//!   connected: only those registered afterwards. Create listeners once their
//!   server is up, or replace a listener that took ten seconds or more to
//!   report `is_connected`, the interval at which the runtime tries again.
//!
//! See [Discovery Server deployment guidance](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/troubleshooting.md#discovery-server)
//! when validating an application with its selected runtime and network.
//!
//! # Threads
//!
//! The native owners are [`Send`] but not [`Sync`]: each can move to another
//! thread, and one thread at a time uses it. Snapshots and events are plain
//! owned data. To keep an owner in state that several threads share, put it in
//! a [`Mutex`](std::sync::Mutex). The owner is `Send`, so the mutex is `Sync`,
//! and the methods that change an owner take it exclusively in any case:
//!
//! ```no_run
//! use std::sync::{Arc, Mutex};
//! use grafton_ndi::{
//!     discovery::{AdvertiserOptions, SendAdvertisement, SendAdvertiser},
//!     Ndi, Sender, SenderOptions,
//! };
//!
//! /// What every request handler of a server shares.
//! struct Shared {
//!     advertiser: Mutex<SendAdvertiser>,
//! }
//!
//! # fn main() -> grafton_ndi::Result<()> {
//! let ndi = Ndi::new()?;
//! let shared = Arc::new(Shared {
//!     advertiser: Mutex::new(SendAdvertiser::new(&ndi, &AdvertiserOptions::new())?),
//! });
//! let sender = Sender::new(&ndi, &SenderOptions::new("Camera 1"))?;
//! let worker = std::thread::spawn({
//!     let shared = Arc::clone(&shared);
//!     move || {
//!         let mut advertiser = shared.advertiser.lock().unwrap();
//!         advertiser.register(&sender, SendAdvertisement::new())
//!     }
//! });
//! let _registration = worker.join().unwrap()?;
//! # Ok(())
//! # }
//! ```
//!
//! # Connecting
//!
//! Each object is created for a Discovery Server address: the one given in its
//! options, or else the one in the NDI configuration. Creation fails only when
//! there is no address or the SDK cannot parse it. It does not wait for the
//! server, and a server that cannot be reached is not an error: the object
//! keeps trying in the background for as long as it lives, and starts working
//! within about ten seconds of the server appearing, as the 6.4.0.2 runtime
//! tries again every ten seconds. A listener that connects only on such a
//! later attempt misses earlier registrations; see the defects above.
//!
//! A listener reports its connection through `is_connected`, and its snapshots
//! are empty until it connects. An advertiser reports nothing; whether its
//! registrations arrived shows in a listener on the same server.
//! Registry connectivity and membership do not establish media progress;
//! monitor the receiver's expected-media counter separately.
//!
//! # Text from the network
//!
//! Names, metadata and event values are whatever a peer advertised. They are
//! decoded lossily, replacing bytes that are not UTF-8 with U+FFFD, within the
//! resource limits below. Identifiers are the exception:
//! a [`SenderId`], [`ReceiverId`] or [`InputGroupId`] keeps the server's exact
//! bytes, so handing one back in a subscription or a connection request always
//! names the same peer. Each kind is its own type, so a sender's identifier
//! cannot be used where a receiver's is meant.
//!
//! Each snapshot or event batch has one 16 MiB budget covering native arrays
//! and strings scanned plus owned Rust storage reserved, including UTF-8
//! replacement bytes and identifier terminators. Array storage is reserved
//! for every reported element, even when null fields or list terminators are
//! omitted from the result. A single native string must be shorter than 4 MiB.
//! Exceeding the aggregate copy budget returns
//! [`Error::ResourceLimit`](crate::Error::ResourceLimit), identifying
//! [`ResourceKind::DiscoveryServerCopyBytes`](crate::ResourceKind::DiscoveryServerCopyBytes),
//! before reserving the offending output. Malformed descriptors, strings that
//! exceed their individual bound, and allocation failures return
//! [`Error::UnexpectedSdkResult`](crate::Error::UnexpectedSdkResult). A rejected
//! event batch is freed and its events are lost.
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
