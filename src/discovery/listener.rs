//! Snapshots of, events from, and connection requests to what a Discovery
//! Server advertises.

use super::{not_created, Handle};
use crate::{
    frames::{bounded_sdk_text, MAX_SDK_STRING_BYTES},
    sdk_copy::{CopyBudget, CopyError, COPY_LIMIT},
    sys::{self, bindings::*},
    to_ms_checked, Error, Ndi, ResourceKind, Result,
};
use std::{
    borrow::Cow,
    ffi::{c_char, CStr, CString},
    fmt, ptr,
    time::Duration,
};

/// The operations the sender and receiver listener tables have in common.
trait ListenerApi: sys::Table + Sync {
    /// The native listener handle.
    type Instance: Handle;
    /// The native creation settings.
    type Settings;
    /// One element of the native membership snapshot.
    type Entry;
    /// What identifies one of the peers this listener lists.
    type Id: Identifier;
    /// The Advanced table of the entry point that also takes a configuration.
    #[cfg(feature = "advanced")]
    type Configured: sys::Table;
    /// That entry point's name.
    #[cfg(feature = "advanced")]
    const CONFIGURED: &'static str;

    /// The settings for a listener of the Discovery Server at `url`, or of the
    /// one the SDK is configured with when `url` is null.
    fn settings(url: *const c_char) -> Self::Settings;

    /// Create a listener. Returns null when the SDK creates none.
    ///
    /// # Safety
    ///
    /// The URL of `settings` must be null or a NUL-terminated string that
    /// outlives the call.
    unsafe fn create(&self, settings: &Self::Settings) -> Self::Instance;

    /// Create a listener with a per-instance configuration.
    ///
    /// # Safety
    ///
    /// `settings` must satisfy [`Self::create`]'s contract, and `config` must
    /// point to a NUL-terminated configuration string that outlives the call.
    #[cfg(feature = "advanced")]
    unsafe fn create_configured(
        table: &'static Self::Configured,
        settings: *const Self::Settings,
        config: *const c_char,
    ) -> Self::Instance;

    /// # Safety
    ///
    /// Each method requires `instance` to be a live listener created by this
    /// table; pointer arguments must be valid for the SDK's documented use.
    unsafe fn destroy(&self, instance: Self::Instance);
    unsafe fn is_connected(&self, instance: Self::Instance) -> bool;
    unsafe fn server_url(&self, instance: Self::Instance) -> *const c_char;
    unsafe fn entries(&self, instance: Self::Instance, count: &mut u32) -> *const Self::Entry;
    unsafe fn wait_for_entries(&self, instance: Self::Instance, timeout_ms: u32) -> bool;
    unsafe fn subscribe(&self, instance: Self::Instance, id: *const c_char);
    unsafe fn unsubscribe(&self, instance: Self::Instance, id: *const c_char);
    unsafe fn events(
        &self,
        instance: Self::Instance,
        count: &mut u32,
        timeout_ms: u32,
    ) -> *const NDIlib_listener_event;
    unsafe fn free_events(&self, instance: Self::Instance, events: *const NDIlib_listener_event);
}

macro_rules! listener_api {
    (
        $Table:ty, $Instance:ty, $Entry:ty, $Id:ty, $Settings:ident,
        $create:ident, $destroy:ident, $is_connected:ident, $server_url:ident, $entries:ident,
        $wait:ident, $subscribe:ident, $unsubscribe:ident, $events:ident, $free_events:ident,
        $Configured:ident::$create_configured:ident
    ) => {
        impl ListenerApi for $Table {
            type Instance = $Instance;
            type Settings = $Settings;
            type Entry = $Entry;
            type Id = $Id;
            #[cfg(feature = "advanced")]
            type Configured = sys::advanced::$Configured;
            #[cfg(feature = "advanced")]
            const CONFIGURED: &'static str = stringify!($create_configured);

            fn settings(url: *const c_char) -> Self::Settings {
                $Settings { p_url_address: url }
            }
            unsafe fn create(&self, settings: &Self::Settings) -> Self::Instance {
                self.$create(settings)
            }
            #[cfg(feature = "advanced")]
            sys::advanced::declared! { $Configured {
                unsafe fn create_configured(
                    table: &'static Self::Configured,
                    settings: *const Self::Settings,
                    config: *const c_char,
                ) -> Self::Instance {
                    // SAFETY: forwarded to the caller's obligations.
                    unsafe { table.$create_configured(settings, config) }
                }
            } else {
                unsafe fn create_configured(
                    table: &'static Self::Configured,
                    _: *const Self::Settings,
                    _: *const c_char,
                ) -> Self::Instance {
                    match *table {}
                }
            }}
            unsafe fn destroy(&self, instance: Self::Instance) {
                self.$destroy(instance)
            }
            unsafe fn is_connected(&self, instance: Self::Instance) -> bool {
                self.$is_connected(instance)
            }
            unsafe fn server_url(&self, instance: Self::Instance) -> *const c_char {
                self.$server_url(instance)
            }
            unsafe fn entries(
                &self,
                instance: Self::Instance,
                count: &mut u32,
            ) -> *const Self::Entry {
                self.$entries(instance, count)
            }
            unsafe fn wait_for_entries(&self, instance: Self::Instance, timeout_ms: u32) -> bool {
                self.$wait(instance, timeout_ms)
            }
            unsafe fn subscribe(&self, instance: Self::Instance, id: *const c_char) {
                self.$subscribe(instance, id)
            }
            unsafe fn unsubscribe(&self, instance: Self::Instance, id: *const c_char) {
                self.$unsubscribe(instance, id)
            }
            unsafe fn events(
                &self,
                instance: Self::Instance,
                count: &mut u32,
                timeout_ms: u32,
            ) -> *const NDIlib_listener_event {
                self.$events(instance, count, timeout_ms)
            }
            unsafe fn free_events(
                &self,
                instance: Self::Instance,
                events: *const NDIlib_listener_event,
            ) {
                self.$free_events(instance, events)
            }
        }
    };
}

listener_api!(
    sys::SenderDiscovery,
    NDIlib_send_listener_instance_t,
    NDIlib_sender_t,
    SenderId,
    NDIlib_send_listener_create_t,
    NDIlib_send_listener_create,
    NDIlib_send_listener_destroy,
    NDIlib_send_listener_is_connected,
    NDIlib_send_listener_get_server_url,
    NDIlib_send_listener_get_senders,
    NDIlib_send_listener_wait_for_senders,
    NDIlib_send_listener_subscribe_events,
    NDIlib_send_listener_unsubscribe_events,
    NDIlib_send_listener_get_events,
    NDIlib_send_listener_free_events,
    SenderListenerConfiguration::NDIlib_send_listener_create_ex
);
listener_api!(
    sys::ReceiverDiscovery,
    NDIlib_recv_listener_instance_t,
    NDIlib_receiver_t,
    ReceiverId,
    NDIlib_recv_listener_create_t,
    NDIlib_recv_listener_create,
    NDIlib_recv_listener_destroy,
    NDIlib_recv_listener_is_connected,
    NDIlib_recv_listener_get_server_url,
    NDIlib_recv_listener_get_receivers,
    NDIlib_recv_listener_wait_for_receivers,
    NDIlib_recv_listener_subscribe_events,
    NDIlib_recv_listener_unsubscribe_events,
    NDIlib_recv_listener_get_events,
    NDIlib_recv_listener_free_events,
    ReceiverListenerConfiguration::NDIlib_recv_listener_create_ex
);

/// The Discovery Server a listener connects to.
///
/// Without an explicit server the SDK uses the Discovery Server from the NDI
/// configuration. If that names none either, the listener cannot be created.
#[derive(Debug, Clone, Default)]
pub struct ListenerOptions {
    server: Option<String>,
    #[cfg(feature = "advanced")]
    config: Option<crate::advanced::AdvancedConfig>,
}
impl ListenerOptions {
    /// Inherit the Discovery Server endpoint from the NDI configuration.
    #[must_use]
    pub fn clear_server(mut self) -> Self {
        self.server = None;
        self
    }

    /// Remove the per-instance Advanced configuration override.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn clear_config(mut self) -> Self {
        self.config = None;
        self
    }

    /// Requested server endpoint, or `None` to inherit the NDI configuration.
    #[must_use]
    pub fn configured_server(&self) -> Option<&str> {
        self.server.as_deref()
    }

    /// The stored Advanced configuration handle, if any.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn configured_config(&self) -> Option<&crate::advanced::AdvancedConfig> {
        self.config.as_ref()
    }

    /// The default options: the Discovery Server of the NDI configuration.
    pub fn new() -> Self {
        Self::default()
    }
    /// Connect to this endpoint: a host name or IP address, optionally followed
    /// by a colon and a port (5959 when omitted).
    ///
    /// One endpoint only: the listener constructors refuse a comma-separated
    /// list, of which a listener would use only the first entry.
    #[must_use]
    pub fn server(mut self, server: impl Into<String>) -> Self {
        self.server = Some(server.into());
        self
    }
    /// Create the listener with this per-instance Advanced SDK configuration,
    /// which it keeps for as long as it lives.
    ///
    /// [`SendListener::new`] and [`RecvListener::new`] then need an Advanced
    /// runtime, and a build with the Advanced SDK.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn config(mut self, config: crate::advanced::AdvancedConfig) -> Self {
        self.config = Some(config);
        self
    }
}

/// An identifier a Discovery Server assigned, kept byte for byte.
///
/// Each kind of identifier is its own type, so one cannot be passed where
/// another is meant.
trait Identifier: Sized {
    /// The identifier with exactly the bytes the server supplied.
    fn from_exact(bytes: CString) -> Self;
    /// Those bytes, as the SDK takes them back.
    fn as_c_str(&self) -> &CStr;
}

macro_rules! identifier {
    ($(#[$doc:meta])* $Id:ident) => {
        $(#[$doc])*
        ///
        /// The identifier is opaque: no UUID spelling is assumed, and it keeps
        /// exactly the bytes the server supplied, so it always names the same
        /// peer even when those bytes are not valid UTF-8.
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $Id(CString);

        impl $Id {
            /// Make an identifier from bytes obtained elsewhere, such as a
            /// saved selection.
            ///
            /// # Errors
            ///
            /// Returns [`Error::InvalidCString`] when the bytes contain a NUL.
            pub fn new(value: impl Into<Vec<u8>>) -> Result<Self> {
                Ok(Self(CString::new(value)?))
            }
            /// The exact bytes the server supplied.
            pub fn as_bytes(&self) -> &[u8] {
                self.0.as_bytes()
            }
            /// The identifier as text for display, with any bytes that are not
            /// UTF-8 replaced by U+FFFD. Use the identifier itself, not this
            /// text, to name the peer again.
            pub fn to_string_lossy(&self) -> Cow<'_, str> {
                self.0.to_string_lossy()
            }
        }

        impl fmt::Display for $Id {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.to_string_lossy())
            }
        }

        impl Identifier for $Id {
            fn from_exact(bytes: CString) -> Self {
                Self(bytes)
            }
            fn as_c_str(&self) -> &CStr {
                &self.0
            }
        }
    };
}

identifier!(
    /// The identifier a Discovery Server gives an advertised sender.
    ///
    /// [`SendListener`] reports it and takes it back to subscribe to the
    /// sender's events.
    SenderId
);
identifier!(
    /// The identifier a Discovery Server gives an advertised receiver.
    ///
    /// [`RecvListener`] reports it and takes it back to subscribe to the
    /// receiver's events and to ask the receiver to change source.
    ReceiverId
);
identifier!(
    /// The identifier a Discovery Server gives a receiver's input group.
    ///
    /// Receivers advertised with the same
    /// [input group](super::RecvAdvertisement::input_group) report the same
    /// identifier. It names a group, not a receiver, so no operation takes it.
    InputGroupId
);

/// An event a subscribed sender or receiver reported.
///
/// `Id` is the kind of peer the event came from: [`SenderId`] for the events
/// of a [`SendListener`], [`ReceiverId`] for those of a [`RecvListener`].
///
/// Event names and values are defined by the peer and are preserved as sent,
/// apart from the lossy decoding described on [`ListenerEvent::name`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ListenerEvent<Id> {
    id: Option<Id>,
    name: Option<String>,
    value: Option<String>,
}
impl<Id> ListenerEvent<Id> {
    /// The peer the event came from, when the server supplied it.
    pub fn id(&self) -> Option<&Id> {
        self.id.as_ref()
    }
    /// The event's name. Bytes that are not UTF-8 are replaced by U+FFFD.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    /// The event's value. Bytes that are not UTF-8 are replaced by U+FFFD.
    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }
}

/// A receiver stream kind. These are individual values, not bit flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReceiverStream {
    /// The receiver accepts metadata.
    Metadata,
    /// The receiver accepts video.
    Video,
    /// The receiver accepts audio.
    Audio,
    /// A stream kind this crate does not name, with its native value.
    Unknown(u32),
}
impl ReceiverStream {
    /// The stream kind the SDK's `value` names, or `None` for the SDK's
    /// "none", which only terminates the list a receiver advertises.
    fn from_sdk(value: NDIlib_receiver_type_e) -> Option<Self> {
        match value {
            sys::bindings::NDIlib_receiver_type_e_NDIlib_receiver_type_none => None,
            sys::bindings::NDIlib_receiver_type_e_NDIlib_receiver_type_metadata => {
                Some(Self::Metadata)
            }
            sys::bindings::NDIlib_receiver_type_e_NDIlib_receiver_type_video => Some(Self::Video),
            sys::bindings::NDIlib_receiver_type_e_NDIlib_receiver_type_audio => Some(Self::Audio),
            value => Some(Self::Unknown(value)),
        }
    }
}
/// A remote command a receiver advertises that it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReceiverCommand {
    /// The receiver accepts requests to connect to another source.
    Connect,
    /// A command this crate does not name, with its native value.
    Unknown(u32),
}
impl ReceiverCommand {
    /// The command the SDK's `value` names, or `None` for the SDK's "none",
    /// which only terminates the list a receiver advertises.
    fn from_sdk(value: NDIlib_receiver_command_e) -> Option<Self> {
        match value {
            sys::bindings::NDIlib_receiver_command_e_NDIlib_receiver_command_none => None,
            sys::bindings::NDIlib_receiver_command_e_NDIlib_receiver_command_connect => {
                Some(Self::Connect)
            }
            value => Some(Self::Unknown(value)),
        }
    }
}

/// An owned description of one advertised sender, independent of later
/// listener calls.
///
/// Every string is whatever the sender advertised. Text that is not valid
/// UTF-8 is decoded with U+FFFD in place of the offending bytes; only the
/// [identifier](Self::id) keeps its exact bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AdvertisedSender {
    id: Option<SenderId>,
    name: Option<String>,
    metadata: Option<String>,
    address: Option<String>,
    port: Option<u16>,
    groups: Vec<String>,
    subscribed: bool,
}
impl AdvertisedSender {
    /// The server's identifier for this sender.
    pub fn id(&self) -> Option<&SenderId> {
        self.id.as_ref()
    }
    /// The sender's source name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    /// The connection metadata the sender advertises.
    pub fn metadata(&self) -> Option<&str> {
        self.metadata.as_deref()
    }
    /// The sender's network address.
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }
    /// The sender's network port, or `None` when the advertised value is not a
    /// port number.
    pub fn port(&self) -> Option<u16> {
        self.port
    }
    /// The discovery groups the sender belongs to.
    pub fn groups(&self) -> &[String] {
        &self.groups
    }
    /// Whether this listener is subscribed to the sender's events.
    pub fn events_subscribed(&self) -> bool {
        self.subscribed
    }
}
/// An owned description of one advertised receiver, independent of later
/// listener calls.
///
/// Every string is whatever the receiver advertised. Text that is not valid
/// UTF-8 is decoded with U+FFFD in place of the offending bytes; only the
/// identifiers keep their exact bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AdvertisedReceiver {
    id: Option<ReceiverId>,
    name: Option<String>,
    input_id: Option<InputGroupId>,
    input_name: Option<String>,
    address: Option<String>,
    streams: Vec<ReceiverStream>,
    commands: Vec<ReceiverCommand>,
    subscribed: bool,
}
impl AdvertisedReceiver {
    /// The server's identifier for this receiver.
    pub fn id(&self) -> Option<&ReceiverId> {
        self.id.as_ref()
    }
    /// The receiver's name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    /// The identifier of the input group the receiver belongs to.
    pub fn input_id(&self) -> Option<&InputGroupId> {
        self.input_id.as_ref()
    }
    /// The name of the input group the receiver belongs to, which is empty
    /// for a receiver advertised without one.
    pub fn input_name(&self) -> Option<&str> {
        self.input_name.as_deref()
    }
    /// The receiver's network address.
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }
    /// The stream kinds the receiver accepts.
    pub fn streams(&self) -> &[ReceiverStream] {
        &self.streams
    }
    /// The remote commands the receiver accepts.
    pub fn commands(&self) -> &[ReceiverCommand] {
        &self.commands
    }
    /// Whether this listener is subscribed to the receiver's events.
    pub fn events_subscribed(&self) -> bool {
        self.subscribed
    }
}

fn copy_budget() -> CopyBudget {
    CopyBudget::new(COPY_LIMIT, ResourceKind::DiscoveryServerCopyBytes)
}

/// Copy the Discovery Server endpoint a listener reports: `None` for a null
/// pointer, or for a string with no terminator within
/// [`MAX_SDK_STRING_BYTES`], and bytes that are not UTF-8 replaced with
/// U+FFFD.
///
/// # Safety
///
/// A non-null `url` must be readable byte by byte until either its first NUL
/// or [`MAX_SDK_STRING_BYTES`] bytes have been read.
unsafe fn endpoint(url: *const c_char) -> Option<String> {
    if url.is_null() {
        return None;
    }
    // SAFETY: forwarded to the caller's obligation for a non-null pointer.
    let bytes = unsafe { bounded_sdk_text(url, MAX_SDK_STRING_BYTES) }?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// A native listener and the strings its creation settings point to.
struct Listener<A: ListenerApi> {
    instance: A::Instance,
    api: &'static A,
    _url: Option<CString>,
    #[cfg(feature = "advanced")]
    _config: Option<crate::advanced::AdvancedConfig>,
    _ndi: Ndi,
}

// SAFETY: the SDK attaches no listener to the thread that created it; its
// objects may be used and destroyed from any thread as long as calls on one
// object do not overlap, which the SDK requires of membership queries in
// particular. A listener has exactly one owner and is not `Sync`, so its calls
// cannot overlap, and everything else it holds is `Send`.
unsafe impl<A: ListenerApi> Send for Listener<A> {}

impl<A: ListenerApi> Drop for Listener<A> {
    fn drop(&mut self) {
        // SAFETY: this listener is the instance's only owner.
        unsafe { self.api.destroy(self.instance) };
    }
}
impl<A: ListenerApi> Listener<A> {
    /// Connect to the Discovery Server `options` name, through the Advanced
    /// entry point when the options carry a configuration.
    fn new(ndi: &Ndi, options: &ListenerOptions) -> Result<Self> {
        #[cfg(feature = "advanced")]
        if let Some(config) = &options.config {
            use crate::advanced::Configurable;
            // SAFETY: the closure returns what the configured entry point
            // returned for the settings and the configuration it was handed.
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
        // SAFETY: the SDK's listener creation returns null or a fresh listener
        // that nothing else owns, and the settings outlive the call.
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

    /// The one creation path of both listeners, with and without a
    /// configuration: create the native listener through `create`, which is
    /// handed the table and the native settings for `options`, and keep
    /// `config` for as long as the listener lives.
    ///
    /// # Safety
    ///
    /// `create` returns null or a live listener of `ndi`'s runtime, made from
    /// the settings it was handed, that nothing else owns.
    unsafe fn create(
        ndi: &Ndi,
        options: &ListenerOptions,
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(&'static A, &A::Settings) -> A::Instance,
    ) -> Result<Self> {
        // A listener with a configuration gets here only once its own
        // capability, which requires this table's, has been allowed, so it
        // is never refused for this table.
        let api = ndi.table::<A>()?;
        if options
            .server
            .as_deref()
            .is_some_and(|server| server.contains(','))
        {
            return Err(Error::InvalidInput(
                "a listener accepts one Discovery Server endpoint".into(),
            ));
        }
        let url = options.server.as_deref().map(CString::new).transpose()?;
        let settings = A::settings(url.as_ref().map_or(ptr::null(), |v| v.as_ptr()));
        let instance = create(api, &settings);
        if instance.is_null() {
            return Err(not_created("listener", options.server.as_deref()));
        }
        Ok(Self {
            instance,
            api,
            _url: url,
            #[cfg(feature = "advanced")]
            _config: config,
            _ndi: ndi.clone(),
        })
    }

    fn is_connected(&self) -> bool {
        // SAFETY: the listener instance is live for this borrow.
        unsafe { self.api.is_connected(self.instance) }
    }
    fn server_url(&self) -> Option<String> {
        // SAFETY: the listener instance is live for this borrow.
        let url = unsafe { self.api.server_url(self.instance) };
        // SAFETY: a non-null result is a NUL-terminated string the SDK keeps
        // for the listener's lifetime, which this borrow is within.
        unsafe { endpoint(url) }
    }
    fn wait_for_changes(&mut self, timeout: Duration) -> Result<bool> {
        let timeout = to_ms_checked(timeout)?;
        // SAFETY: the listener instance is live for this borrow.
        Ok(unsafe { self.api.wait_for_entries(self.instance, timeout) })
    }
    /// The native membership snapshot, valid until the next call on this listener.
    fn entries<'a>(&'a mut self, budget: &mut CopyBudget) -> Result<&'a [A::Entry]> {
        let mut count = 0;
        // SAFETY: the exclusive borrow keeps the SDK-owned array unchanged for
        // `'a`; the budget validates the pointer and count before use.
        unsafe { budget.slice(self.api.entries(self.instance, &mut count), count) }
            .map_err(CopyError::into_error)
    }
    fn subscription(&mut self, id: &A::Id, subscribe: bool) {
        let id = id.as_c_str().as_ptr();
        // SAFETY: the listener instance is live and `id` outlives the call.
        unsafe {
            if subscribe {
                self.api.subscribe(self.instance, id);
            } else {
                self.api.unsubscribe(self.instance, id);
            }
        };
    }
    fn capture_events(&mut self, timeout: Duration) -> Result<Vec<ListenerEvent<A::Id>>> {
        struct Batch<'a, A: ListenerApi> {
            listener: &'a Listener<A>,
            events: *const NDIlib_listener_event,
        }
        impl<A: ListenerApi> Drop for Batch<'_, A> {
            fn drop(&mut self) {
                // SAFETY: the batch came from this listener and is freed once.
                unsafe {
                    self.listener
                        .api
                        .free_events(self.listener.instance, self.events)
                };
            }
        }
        let timeout = to_ms_checked(timeout)?;
        let mut count = 0;
        // SAFETY: the listener instance is live for this borrow.
        let events = unsafe { self.api.events(self.instance, &mut count, timeout) };
        let mut budget = copy_budget();
        if events.is_null() {
            // No event arrived only if the count agrees. A null batch with
            // a positive count is malformed, as a membership snapshot is.
            // SAFETY: null is permitted and checked without dereferencing.
            unsafe { budget.slice(events, count) }.map_err(CopyError::into_error)?;
            return Ok(Vec::new());
        }
        let batch = Batch {
            listener: self,
            events,
        };
        // SAFETY: the batch stays allocated until `batch` is dropped below.
        let raw = unsafe { budget.slice(batch.events, count) }.map_err(CopyError::into_error)?;
        budget.collect_entries(raw, |event, budget| {
            // SAFETY: the strings belong to the batch, which is still
            // allocated.
            unsafe {
                Ok(ListenerEvent {
                    id: budget.id(event.p_uuid)?.map(A::Id::from_exact),
                    name: budget.text(event.p_name)?,
                    value: budget.text(event.p_value)?,
                })
            }
        })
    }
}

#[cfg(feature = "advanced")]
impl<A: ListenerApi> crate::advanced::Configurable for Listener<A> {
    type Options = ListenerOptions;
    type Settings = A::Settings;
    type Instance = A::Instance;

    /// A listener the SDK does not create is reported by the server it was
    /// given, so the entry point's name is not needed.
    unsafe fn create_with(
        ndi: &Ndi,
        options: &ListenerOptions,
        _operation: &'static str,
        config: crate::advanced::AdvancedConfig,
        create: impl FnOnce(&A::Settings) -> A::Instance,
    ) -> Result<Self> {
        // SAFETY: forwarded to the caller's obligation.
        unsafe { Self::create(ndi, options, Some(config), |_, settings| create(settings)) }
    }
}

/// Lists the senders registered with a Discovery Server and reports their
/// events.
///
/// # Connection
///
/// A listener connects in the background and keeps trying for as long as it
/// lives. It is created whether or not its server can be reached; until the
/// server answers, which can take several seconds after it appears,
/// [`is_connected`](Self::is_connected) is `false` and snapshots are empty.
///
/// # Threading
///
/// A listener is [`Send`] but not [`Sync`]: it may be moved to another thread,
/// and is used by one thread at a time. [`is_connected`](Self::is_connected)
/// and [`server_url`](Self::server_url) take a shared reference, and the SDK
/// documents neither as safe to call on one listener from several threads at
/// once; it forbids overlapping membership queries on one listener. Snapshots
/// and events are plain owned data and can be shared freely.
pub struct SendListener {
    inner: Listener<sys::SenderDiscovery>,
}
/// Lists the receivers registered with a Discovery Server, reports their
/// events, and asks them to change source. Remote control is always an
/// explicit call.
///
/// # Connection
///
/// A listener connects in the background and keeps trying for as long as it
/// lives. It is created whether or not its server can be reached; until the
/// server answers, which can take several seconds after it appears,
/// [`is_connected`](Self::is_connected) is `false` and snapshots are empty.
///
/// # Threading
///
/// A listener is [`Send`] but not [`Sync`]: it may be moved to another thread,
/// and is used by one thread at a time. [`is_connected`](Self::is_connected)
/// and [`server_url`](Self::server_url) take a shared reference, and the SDK
/// documents neither as safe to call on one listener from several threads at
/// once; it forbids overlapping membership queries on one listener. Snapshots
/// and events are plain owned data and can be shared freely.
pub struct RecvListener {
    inner: Listener<sys::ReceiverDiscovery>,
}

const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<SendListener>();
    assert_send::<RecvListener>();
};

impl fmt::Debug for SendListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendListener").finish_non_exhaustive()
    }
}
impl fmt::Debug for RecvListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvListener").finish_non_exhaustive()
    }
}

macro_rules! listener_methods {
    ($Id:ty) => {
        /// Whether the listener is connected to its Discovery Server right
        /// now. A listener that is not connected keeps trying. This reports
        /// registry connectivity, not progress of any receiver media.
        pub fn is_connected(&self) -> bool {
            self.inner.is_connected()
        }
        /// The Discovery Server endpoint the listener uses, when the SDK
        /// reports one.
        ///
        /// `None` when the SDK reports none, and also when what it reports
        /// is no string of less than 4 MiB, the most this crate reads from
        /// one SDK string. No endpoint is that long, and the value is for
        /// display only: the listener does not depend on it and the crate
        /// never passes it back to the SDK. So a value that cannot be read
        /// is reported as no value, rather than as an error that every
        /// caller of this getter would have to handle. Bytes that are not
        /// UTF-8 are replaced with U+FFFD, as in the other text a listener
        /// reports.
        pub fn server_url(&self) -> Option<String> {
            self.inner.server_url()
        }
        /// Wait for the advertised membership to change; a zero timeout polls.
        ///
        /// # Errors
        ///
        /// Returns [`Error::InvalidInput`] when `timeout` exceeds
        /// [`crate::MAX_TIMEOUT`].
        pub fn wait_for_changes(&mut self, timeout: Duration) -> Result<bool> {
            self.inner.wait_for_changes(timeout)
        }
        /// Ask to receive the events of the peer `id` names. The next snapshot
        /// reports whether the subscription is active.
        pub fn subscribe(&mut self, id: &$Id) {
            self.inner.subscription(id, true)
        }
        /// Ask to stop receiving the events of the peer `id` names.
        ///
        /// With the NDI 6.3.2 and 6.4.0.2 runtimes, unsubscribing from a
        /// receiver can permanently block that receiver's connect, in this
        /// process or another, when it connects at the same moment: an SDK
        /// defect the crate cannot prevent. Dropping the listener ends its
        /// subscriptions without that risk. See the
        /// [module documentation](super#sdk-defects-that-remain-in-64).
        pub fn unsubscribe(&mut self, id: &$Id) {
            self.inner.subscription(id, false)
        }
        /// Take the events that arrived since the last call, in arrival order,
        /// waiting up to `timeout` for the first one. An empty batch means none
        /// arrived in time; a zero timeout polls.
        ///
        /// An event with a string of 4 MiB or longer is omitted; its scan
        /// still spends the shared budget. Other events remain in order.
        ///
        /// # Errors
        ///
        /// Returns [`Error::InvalidInput`] when `timeout` exceeds
        /// [`crate::MAX_TIMEOUT`], [`Error::ResourceLimit`] when copying the
        /// batch would exceed its 16 MiB budget, and
        /// [`Error::UnexpectedSdkResult`] when the top-level batch array is
        /// malformed or storage cannot be reserved. The SDK has already
        /// handed a rejected batch over, so it is freed and its events are lost.
        pub fn capture_events(&mut self, timeout: Duration) -> Result<Vec<ListenerEvent<$Id>>> {
            self.inner.capture_events(timeout)
        }
    };
}
impl SendListener {
    /// Create a listener for the Discovery Server `options` name.
    ///
    /// Success does not mean the server was reached; see
    /// [Connection](Self#connection).
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] when the selected runtime
    /// cannot provide [sender discovery], which needs NDI 6.4.0 or later, or the
    /// options carry an Advanced configuration that the runtime or this build
    /// cannot take; [`Error::InvalidCString`] when the endpoint contains a NUL
    /// byte; and [`Error::InvalidInput`] when the options name more than one
    /// endpoint, or the SDK creates no listener. It creates none for an
    /// address it cannot parse, and none when `options` name no server and the
    /// NDI configuration supplies none.
    ///
    /// [sender discovery]: crate::capabilities::RuntimeFeature::SenderDiscovery
    pub fn new(ndi: &Ndi, options: &ListenerOptions) -> Result<Self> {
        Ok(Self {
            inner: Listener::new(ndi, options)?,
        })
    }
    listener_methods!(SenderId);
    /// Copy the senders currently advertised. The list is empty while the
    /// listener is not connected.
    ///
    /// An entry with a string of 4 MiB or longer, or a malformed nested
    /// array, is omitted. Its scans still spend the shared budget. Text that
    /// is not valid UTF-8 is decoded lossily; see the [module documentation](super).
    ///
    /// # Errors
    ///
    /// Returns [`Error::ResourceLimit`] when copying would exceed the 16 MiB
    /// budget, and [`Error::UnexpectedSdkResult`] when the top-level snapshot
    /// array is malformed or storage cannot be reserved.
    pub fn snapshot(&mut self) -> Result<Vec<AdvertisedSender>> {
        let mut budget = copy_budget();
        let raw = self.inner.entries(&mut budget)?;
        budget.collect_entries(raw, |sender, budget| {
            // SAFETY: the snapshot and everything it points to stay valid
            // until the next call on this listener, after this copy.
            unsafe {
                let groups = budget.slice(sender.p_groups.cast_const(), sender.num_groups)?;
                let groups = budget.collect(groups, |group, budget| budget.text(*group))?;
                Ok(AdvertisedSender {
                    id: budget.id(sender.p_uuid)?.map(SenderId::from_exact),
                    name: budget.text(sender.p_name)?,
                    metadata: budget.text(sender.p_metadata)?,
                    address: budget.text(sender.p_address)?,
                    port: u16::try_from(sender.port).ok(),
                    groups,
                    subscribed: sender.events_subscribed,
                })
            }
        })
    }
}
impl RecvListener {
    /// Create a listener for the Discovery Server `options` name.
    ///
    /// Success does not mean the server was reached; see
    /// [Connection](Self#connection).
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] when the selected runtime
    /// cannot provide [receiver discovery], which needs NDI 6.4.0 or later, or the
    /// options carry an Advanced configuration that the runtime or this build
    /// cannot take; [`Error::InvalidCString`] when the endpoint contains a NUL
    /// byte; and [`Error::InvalidInput`] when the options name more than one
    /// endpoint, or the SDK creates no listener. It creates none for an
    /// address it cannot parse, and none when `options` name no server and the
    /// NDI configuration supplies none.
    ///
    /// [receiver discovery]: crate::capabilities::RuntimeFeature::ReceiverDiscovery
    pub fn new(ndi: &Ndi, options: &ListenerOptions) -> Result<Self> {
        Ok(Self {
            inner: Listener::new(ndi, options)?,
        })
    }
    listener_methods!(ReceiverId);
    /// Copy the receivers currently advertised. The list is empty while the
    /// listener is not connected.
    ///
    /// An entry with a string of 4 MiB or longer, or a malformed nested
    /// array, is omitted. Its scans still spend the shared budget. Text that
    /// is not valid UTF-8 is decoded lossily; see the [module documentation](super).
    ///
    /// # Errors
    ///
    /// Returns [`Error::ResourceLimit`] when copying would exceed the 16 MiB
    /// budget, and [`Error::UnexpectedSdkResult`] when the top-level snapshot
    /// array is malformed or storage cannot be reserved.
    pub fn snapshot(&mut self) -> Result<Vec<AdvertisedReceiver>> {
        let mut budget = copy_budget();
        let raw = self.inner.entries(&mut budget)?;
        budget.collect_entries(raw, |receiver, budget| {
            // SAFETY: the snapshot and everything it points to stay valid
            // until the next call on this listener, after this copy.
            unsafe {
                let streams =
                    budget.slice(receiver.p_streams.cast_const(), receiver.num_streams)?;
                let streams =
                    budget.collect(streams, |stream, _| Ok(ReceiverStream::from_sdk(*stream)))?;
                let commands =
                    budget.slice(receiver.p_commands.cast_const(), receiver.num_commands)?;
                let commands = budget.collect(commands, |command, _| {
                    Ok(ReceiverCommand::from_sdk(*command))
                })?;
                Ok(AdvertisedReceiver {
                    id: budget.id(receiver.p_uuid)?.map(ReceiverId::from_exact),
                    name: budget.text(receiver.p_name)?,
                    input_id: budget
                        .id(receiver.p_input_uuid)?
                        .map(InputGroupId::from_exact),
                    input_name: budget.text(receiver.p_input_name)?,
                    address: budget.text(receiver.p_address)?,
                    streams,
                    commands,
                    subscribed: receiver.events_subscribed,
                })
            }
        })
    }
    /// Ask the receiver `id` names to connect to the NDI source called
    /// `source`.
    ///
    /// The request is sent only to a receiver that the listener lists, in a
    /// snapshot taken by this call, as advertising
    /// [`ReceiverCommand::Connect`]. Success means the request was sent, not
    /// that the receiver acted on it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] when `source` contains a NUL byte;
    /// [`Error::ReceiverRequestNotAccepted`] when no receiver the listener
    /// lists has this identifier (`advertised` is false: the listener may not
    /// have connected yet, the receiver may not be registered, or its entry
    /// may have been omitted as malformed; a later request can succeed) or the
    /// receiver does not accept connection requests
    /// (`advertised` is true); [`Error::NativeOperationFailed`] when the SDK
    /// cannot send the request; and the errors of [`Self::snapshot`].
    pub fn request_connect(&mut self, id: &ReceiverId, source: &str) -> Result<()> {
        self.request(id, Some(source))
    }
    /// Ask the receiver `id` names to disconnect from its current source.
    ///
    /// This is the connection request without a source, so it too is sent
    /// only to a receiver that advertises [`ReceiverCommand::Connect`].
    ///
    /// # Errors
    ///
    /// As for [`Self::request_connect`], without the NUL check.
    pub fn request_disconnect(&mut self, id: &ReceiverId) -> Result<()> {
        self.request(id, None)
    }
    fn request(&mut self, id: &ReceiverId, source: Option<&str>) -> Result<()> {
        let source = source.map(CString::new).transpose()?;
        // The SDK accepts this command only for a receiver that advertises it.
        // Check a fresh snapshot rather than trust what the caller last saw.
        let receivers = self.snapshot()?;
        let listed = receivers
            .iter()
            .find(|receiver| receiver.id.as_ref() == Some(id));
        if !listed.is_some_and(|receiver| receiver.commands.contains(&ReceiverCommand::Connect)) {
            return Err(Error::ReceiverRequestNotAccepted {
                advertised: listed.is_some(),
            });
        }
        // SAFETY: the listener instance is live and both strings outlive the call.
        if unsafe {
            self.inner.api.NDIlib_recv_listener_send_connect(
                self.inner.instance,
                id.as_c_str().as_ptr(),
                source.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            )
        } {
            Ok(())
        } else {
            Err(Error::NativeOperationFailed {
                operation: "remote receiver connection request",
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn list_terminators_are_dropped_and_unknown_values_kept() {
        assert_eq!(
            ReceiverStream::from_sdk(NDIlib_receiver_type_e_NDIlib_receiver_type_none),
            None
        );
        assert_eq!(
            ReceiverStream::from_sdk(NDIlib_receiver_type_e_NDIlib_receiver_type_audio),
            Some(ReceiverStream::Audio)
        );
        assert_eq!(
            ReceiverStream::from_sdk(999),
            Some(ReceiverStream::Unknown(999))
        );
        assert_eq!(
            ReceiverCommand::from_sdk(NDIlib_receiver_command_e_NDIlib_receiver_command_none),
            None
        );
        assert_eq!(
            ReceiverCommand::from_sdk(NDIlib_receiver_command_e_NDIlib_receiver_command_connect),
            Some(ReceiverCommand::Connect)
        );
        assert_eq!(
            ReceiverCommand::from_sdk(999),
            Some(ReceiverCommand::Unknown(999))
        );
    }
    #[test]
    fn text_is_decoded_lossily_and_identifiers_keep_their_bytes() {
        let malformed = c"caf\xe9 \xff";
        let mut budget = copy_budget();
        // SAFETY: the pointer is a live NUL-terminated string.
        let (text, id) = unsafe {
            (
                budget.text(malformed.as_ptr()).unwrap().unwrap(),
                budget
                    .id(malformed.as_ptr())
                    .unwrap()
                    .map(ReceiverId::from_exact)
                    .unwrap(),
            )
        };
        assert_eq!(text, "caf\u{fffd} \u{fffd}");
        assert_eq!(id.as_bytes(), malformed.to_bytes());
        assert_eq!(id.to_string(), text);
        assert_eq!(id, ReceiverId::new(malformed.to_bytes()).unwrap());
        assert_ne!(id, ReceiverId::new(text).unwrap());
        assert!(ReceiverId::new("a\0b").is_err());
    }
    #[test]
    fn an_endpoint_is_read_only_up_to_the_string_limit() {
        let mut longest = vec![b'x'; MAX_SDK_STRING_BYTES];
        longest[MAX_SDK_STRING_BYTES - 1] = 0;
        // Its terminator lies just past the limit, so the scan never reaches
        // it; nothing past the limit is read.
        let mut too_long = vec![b'x'; MAX_SDK_STRING_BYTES + 1];
        too_long[MAX_SDK_STRING_BYTES] = 0;
        // SAFETY: every non-null pointer is a live NUL-terminated string.
        unsafe {
            assert_eq!(endpoint(ptr::null()), None);
            assert_eq!(
                endpoint(c"192.0.2.1:5959".as_ptr()).as_deref(),
                Some("192.0.2.1:5959")
            );
            assert_eq!(
                endpoint(c"host-\xff:5959".as_ptr()).as_deref(),
                Some("host-\u{fffd}:5959")
            );
            assert_eq!(
                endpoint(longest.as_ptr().cast()).map(|url| url.len()),
                Some(MAX_SDK_STRING_BYTES - 1)
            );
            assert_eq!(endpoint(too_long.as_ptr().cast()), None);
        }
    }
}
