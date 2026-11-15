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
//! options, or else the one in the NDI configuration. Creation can fail for
//! an unavailable capability, a NUL byte in the endpoint, or an address the
//! SDK cannot use; listeners also reject multiple endpoints. Each constructor
//! lists its errors. Creation does not wait for the server, and a server that
//! cannot be reached is not an error: the object
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
//! before reserving the offending output. An entry or event with a string
//! beyond its individual bound, or an entry with a malformed nested array,
//! is omitted while the other entries are returned. Rejected string scans
//! remain charged, so several malformed entries can still exhaust the budget.
//! A malformed top-level array or allocation failure returns
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
fn not_created(object: &'static str, server: Option<&str>) -> crate::Error {
    crate::Error::InvalidInput(match server {
        Some(server) => {
            crate::InputDefect::DiscoveryAddressRejected(Box::new(crate::DiscoveryAddressFailure {
                object,
                address: server.into(),
            }))
        }
        None => crate::InputDefect::DiscoveryAddressMissing { object },
    })
}

/// A native Discovery Server identity. Handles move between threads, while
/// their owners determine every other auto trait and serialize native calls.
trait Handle: Copy + Send {
    fn is_null(self) -> bool;
}

trait ServerOptions {
    fn server(&self) -> Option<&str>;
    #[cfg(feature = "advanced")]
    fn config(&self) -> Option<&crate::advanced::AdvancedConfig>;
}

impl ServerOptions for AdvertiserOptions {
    fn server(&self) -> Option<&str> {
        self.configured_servers()
    }
    #[cfg(feature = "advanced")]
    fn config(&self) -> Option<&crate::advanced::AdvancedConfig> {
        self.configured_config()
    }
}

impl ServerOptions for ListenerOptions {
    fn server(&self) -> Option<&str> {
        self.configured_server()
    }
    #[cfg(feature = "advanced")]
    fn config(&self) -> Option<&crate::advanced::AdvancedConfig> {
        self.configured_config()
    }
}

/// The shared native lifecycle of all four Discovery Server object kinds.
trait ServerApi: crate::sys::Table + Sync {
    type Instance: Handle;
    type Settings;
    type Options: ServerOptions;
    const OBJECT: &'static str;
    const SINGLE_ENDPOINT: bool;
    #[cfg(feature = "advanced")]
    type Configured: crate::sys::Table;
    #[cfg(feature = "advanced")]
    const CONFIGURED: &'static str;

    fn settings(url: *const std::ffi::c_char) -> Self::Settings;
    /// # Safety
    /// Settings point to a null URL or a terminated string kept alive through
    /// native destruction. The result is null or a fresh, exclusively owned object.
    unsafe fn create(&self, settings: &Self::Settings) -> Self::Instance;
    /// # Safety
    /// As for `create`; configuration is terminated and also remains live
    /// through destruction, and `table` belongs to the same runtime.
    #[cfg(feature = "advanced")]
    unsafe fn create_configured(
        table: &'static Self::Configured,
        settings: *const Self::Settings,
        config: *const std::ffi::c_char,
    ) -> Self::Instance;
    /// # Safety
    /// The instance was created through this table, remains live, and is never
    /// used after this call. No native call on the instance overlaps destruction.
    unsafe fn destroy(&self, instance: Self::Instance);
}

macro_rules! server_api {
    ($Table:ty, $Handle:ident, $Raw:ty, $Settings:ident, $Options:ty,
     $object:literal, $single:literal, $create:ident, $destroy:ident,
     $Configured:ident::$configured:ident) => {
        #[derive(Clone, Copy)]
        struct $Handle($Raw);
        // SAFETY: Discovery Server objects have no thread affinity. Their
        // ServerObject has one owner and this raw handle withholds Sync, so
        // membership calls and destruction cannot overlap. Ownership of Rust
        // state remains checked separately by the compiler.
        unsafe impl Send for $Handle {}
        impl Handle for $Handle {
            fn is_null(self) -> bool { self.0.is_null() }
        }
        impl ServerApi for $Table {
            type Instance = $Handle;
            type Settings = $Settings;
            type Options = $Options;
            const OBJECT: &'static str = $object;
            const SINGLE_ENDPOINT: bool = $single;
            #[cfg(feature = "advanced")]
            type Configured = crate::sys::advanced::$Configured;
            #[cfg(feature = "advanced")]
            const CONFIGURED: &'static str = stringify!($configured);
            fn settings(url: *const std::ffi::c_char) -> Self::Settings {
                $Settings { p_url_address: url }
            }
            unsafe fn create(&self, settings: &Self::Settings) -> Self::Instance {
                // SAFETY: forwarded to the caller's lifecycle contract.
                $Handle(unsafe { self.$create(settings) })
            }
            #[cfg(feature = "advanced")]
            unsafe fn create_configured(
                table: &'static Self::Configured,
                _settings: *const Self::Settings,
                _config: *const std::ffi::c_char,
            ) -> Self::Instance {
                crate::sys::advanced::native!($Configured, table => {
                    // SAFETY: forwarded to the caller's lifecycle contract.
                    $Handle(unsafe { table.$configured(_settings, _config) })
                })
            }
            unsafe fn destroy(&self, instance: Self::Instance) {
                // SAFETY: forwarded to the caller's lifecycle contract.
                unsafe { self.$destroy(instance.0) }
            }
        }
    };
}

use crate::sys::{self, bindings::*};
server_api!(
    sys::SenderAdvertising,
    SendAdvertiserInstance,
    NDIlib_send_advertiser_instance_t,
    NDIlib_send_advertiser_create_t,
    AdvertiserOptions,
    "advertiser",
    false,
    NDIlib_send_advertiser_create,
    NDIlib_send_advertiser_destroy,
    SenderAdvertiserConfiguration::NDIlib_send_advertiser_create_ex
);
server_api!(
    sys::ReceiverAdvertising,
    RecvAdvertiserInstance,
    NDIlib_recv_advertiser_instance_t,
    NDIlib_recv_advertiser_create_t,
    AdvertiserOptions,
    "advertiser",
    false,
    NDIlib_recv_advertiser_create,
    NDIlib_recv_advertiser_destroy,
    ReceiverAdvertiserConfiguration::NDIlib_recv_advertiser_create_ex
);
server_api!(
    sys::SenderDiscovery,
    SendListenerInstance,
    NDIlib_send_listener_instance_t,
    NDIlib_send_listener_create_t,
    ListenerOptions,
    "listener",
    true,
    NDIlib_send_listener_create,
    NDIlib_send_listener_destroy,
    SenderListenerConfiguration::NDIlib_send_listener_create_ex
);
server_api!(
    sys::ReceiverDiscovery,
    RecvListenerInstance,
    NDIlib_recv_listener_instance_t,
    NDIlib_recv_listener_create_t,
    ListenerOptions,
    "listener",
    true,
    NDIlib_recv_listener_create,
    NDIlib_recv_listener_destroy,
    ReceiverListenerConfiguration::NDIlib_recv_listener_create_ex
);

/// Own every value the SDK may retain until its object is destroyed.
struct ServerObject<A: ServerApi> {
    instance: Option<A::Instance>,
    api: &'static A,
    _url: Option<std::ffi::CString>,
    #[cfg(feature = "advanced")]
    _config: Option<crate::advanced::AdvancedConfig>,
    _ndi: crate::Ndi,
}

impl<A: ServerApi> ServerObject<A> {
    fn new(ndi: &crate::Ndi, options: &A::Options) -> crate::Result<Self> {
        #[cfg(feature = "advanced")]
        if let Some(config) = options.config() {
            use crate::advanced::Configurable;
            // SAFETY: the configured entry point obeys the shared lifecycle.
            return unsafe {
                Self::configured(
                    ndi,
                    options,
                    A::CONFIGURED,
                    config,
                    |table: &'static A::Configured, settings, text| {
                        A::create_configured(table, settings, text)
                    },
                )
            };
        }
        // SAFETY: the plain entry point obeys the shared lifecycle.
        unsafe {
            Self::create(
                ndi,
                options,
                #[cfg(feature = "advanced")]
                None,
                |api, settings| api.create(settings),
            )
        }
    }

    /// # Safety
    /// `create` obeys ServerApi's fresh-instance and settings lifetime contract.
    unsafe fn create(
        ndi: &crate::Ndi,
        options: &A::Options,
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(&'static A, &A::Settings) -> A::Instance,
    ) -> crate::Result<Self> {
        // Configured gates run first and include this table's own capability.
        let api = ndi.table::<A>()?;
        let server = options.server();
        if A::SINGLE_ENDPOINT && server.is_some_and(|s| s.contains(',')) {
            return Err(crate::Error::InvalidInput(
                crate::InputDefect::MultipleDiscoveryServers,
            ));
        }
        let url = server.map(std::ffi::CString::new).transpose()?;
        let settings = A::settings(url.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()));
        let instance = create(api, &settings);
        if instance.is_null() {
            return Err(not_created(A::OBJECT, server));
        }
        Ok(Self {
            instance: Some(instance),
            api,
            _url: url,
            #[cfg(feature = "advanced")]
            _config: config,
            _ndi: ndi.clone(),
        })
    }

    fn instance(&self) -> A::Instance {
        // Only teardown consumes the handle, after the owner's last operation.
        self.instance.expect("live Discovery Server object")
    }

    fn destroy(&mut self) {
        if let Some(instance) = self.instance.take() {
            // SAFETY: this owner destroys its object once, while all retained
            // creation storage and the selected runtime are still alive.
            unsafe { self.api.destroy(instance) };
        }
    }
}

#[cfg(feature = "advanced")]
impl<A: ServerApi> crate::advanced::Configurable for ServerObject<A> {
    type Options = A::Options;
    type Settings = A::Settings;
    type Instance = A::Instance;
    unsafe fn create_with(
        ndi: &crate::Ndi,
        options: &Self::Options,
        _operation: &'static str,
        config: crate::advanced::AdvancedConfig,
        create: impl FnOnce(&Self::Settings) -> Self::Instance,
    ) -> crate::Result<Self> {
        // SAFETY: forwarded to the caller's fresh-instance obligation.
        unsafe { Self::create(ndi, options, Some(config), |_, settings| create(settings)) }
    }
}

impl<A: ServerApi> Drop for ServerObject<A> {
    fn drop(&mut self) {
        self.destroy();
    }
}
