//! Snapshots of, events from, and connection requests to what a Discovery
//! Server advertises.

use crate::{
    capabilities::RuntimeFeature,
    sys::{self, bindings::*},
    to_ms_checked, Error, Result, NDI,
};
use std::{
    borrow::Cow,
    ffi::{c_char, CStr, CString},
    fmt, ptr,
    time::Duration,
};

/// The operations the sender and receiver listener tables have in common.
trait ListenerApi: Sized + Sync + 'static {
    type Instance: Copy;
    /// One element of the native membership snapshot.
    type Entry;

    /// The table from the selected runtime, or an unsupported-capability error.
    fn table(ndi: &NDI) -> Result<&'static Self>;

    /// Create a listener for `url`, with Advanced JSON `config` when given.
    /// Returns `None` when the SDK creates no instance.
    ///
    /// # Safety
    ///
    /// `url` and `config` must each be null or a NUL-terminated string that
    /// outlives the call.
    unsafe fn create(
        &self,
        ndi: &NDI,
        url: *const c_char,
        config: Option<*const c_char>,
    ) -> Result<Option<Self::Instance>>;

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
        $Table:ty, $field:ident, $feature:ident,
        $Instance:ty, $Entry:ty, $Create:ident,
        $create:ident, $destroy:ident, $is_connected:ident, $server_url:ident, $entries:ident,
        $wait:ident, $subscribe:ident, $unsubscribe:ident, $events:ident, $free_events:ident
    ) => {
        impl ListenerApi for $Table {
            type Instance = $Instance;
            type Entry = $Entry;

            fn table(ndi: &NDI) -> Result<&'static Self> {
                ndi.capability(RuntimeFeature::$feature, &ndi.api().$field)
            }

            unsafe fn create(
                &self,
                ndi: &NDI,
                url: *const c_char,
                config: Option<*const c_char>,
            ) -> Result<Option<Self::Instance>> {
                let settings = $Create { p_url_address: url };
                let instance = match config {
                    #[cfg(feature = "advanced")]
                    Some(config) => {
                        type Configured =
                            unsafe extern "C" fn(*const $Create, *const c_char) -> $Instance;
                        // SAFETY: `Configured` is the SDK's signature for the
                        // `_ex` variant of this table's create function.
                        let configured: Configured = unsafe {
                            crate::advanced::ffi::required(
                                ndi,
                                concat!(stringify!($create), "_ex\0").as_bytes(),
                            )?
                        };
                        // SAFETY: the caller guarantees both strings are valid.
                        unsafe { configured(&settings, config) }
                    }
                    #[cfg(not(feature = "advanced"))]
                    Some(_) => {
                        let _ = ndi;
                        return Err(Error::UnsupportedCapability {
                            capability: RuntimeFeature::$feature.name().into(),
                            reason: "Advanced configuration needs the `advanced` feature".into(),
                        });
                    }
                    // SAFETY: the caller guarantees `url` is valid.
                    None => unsafe { self.$create(&settings) },
                };
                Ok((!instance.is_null()).then_some(instance))
            }

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
    sender_discovery,
    SenderDiscovery,
    NDIlib_send_listener_instance_t,
    NDIlib_sender_t,
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
    NDIlib_send_listener_free_events
);
listener_api!(
    sys::ReceiverDiscovery,
    receiver_discovery,
    ReceiverDiscovery,
    NDIlib_recv_listener_instance_t,
    NDIlib_receiver_t,
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
    NDIlib_recv_listener_free_events
);

/// The Discovery Server a listener connects to.
///
/// Without an explicit server the SDK uses the Discovery Server from its own
/// configuration.
#[derive(Debug, Clone, Default)]
pub struct ListenerOptions {
    server: Option<String>,
}
impl ListenerOptions {
    /// Connect to this endpoint: a host name or IP address, optionally followed
    /// by a colon and a port (5959 when omitted).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] for a comma-separated list, of
    /// which a listener would use only the first entry, and
    /// [`Error::InvalidCString`] when the endpoint contains a NUL byte.
    pub fn server(mut self, server: impl Into<String>) -> Result<Self> {
        let server = server.into();
        if server.contains(',') {
            return Err(Error::InvalidConfiguration(
                "a listener accepts one Discovery Server endpoint".into(),
            ));
        }
        CString::new(server.as_str())?;
        self.server = Some(server);
        Ok(self)
    }
}

/// The identifier a Discovery Server gives a sender, a receiver or a receiver's
/// input group.
///
/// An identifier is opaque: no UUID spelling is assumed, and it keeps exactly
/// the bytes the server supplied. Passing it back in a subscription or a
/// connection request therefore always names the same peer, even when those
/// bytes are not valid UTF-8.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AdvertisedId(CString);
impl AdvertisedId {
    /// Make an identifier from bytes obtained elsewhere, such as a saved
    /// selection.
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
    /// The identifier as text for display, with any bytes that are not UTF-8
    /// replaced by U+FFFD. Use the identifier itself, not this text, to name
    /// the peer again.
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        self.0.to_string_lossy()
    }
}
impl fmt::Display for AdvertisedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_lossy())
    }
}

/// A finite event wait or an explicit indefinite wait.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum Wait {
    /// A finite budget, rounded upward to whole milliseconds.
    For(Duration),
    /// Wait until an event arrives; this is not cancellable.
    Forever,
}
impl Wait {
    fn millis(self) -> Result<u32> {
        match self {
            Self::Forever => Ok(u32::MAX),
            Self::For(value) => {
                let millis = value.as_nanos().div_ceil(1_000_000);
                if millis >= u128::from(u32::MAX) {
                    return Err(Error::InvalidConfiguration(
                        "finite event timeout must be less than UINT32_MAX milliseconds".into(),
                    ));
                }
                Ok(millis as u32)
            }
        }
    }
}

/// An event a subscribed sender or receiver reported.
///
/// Event names and values are defined by the peer and are preserved as sent,
/// apart from the lossy decoding described on [`ListenerEvent::name`].
#[derive(Debug, Clone)]
pub struct ListenerEvent {
    id: Option<AdvertisedId>,
    name: Option<String>,
    value: Option<String>,
}
impl ListenerEvent {
    /// The peer the event came from, when the server supplied it.
    pub fn id(&self) -> Option<&AdvertisedId> {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReceiverStream {
    /// No stream.
    None,
    /// The receiver accepts metadata.
    Metadata,
    /// The receiver accepts video.
    Video,
    /// The receiver accepts audio.
    Audio,
    /// A stream kind this crate does not name, with its native value.
    Unknown(u32),
}
impl From<u32> for ReceiverStream {
    fn from(v: u32) -> Self {
        match v {
            NDIlib_receiver_type_e_NDIlib_receiver_type_none => Self::None,
            NDIlib_receiver_type_e_NDIlib_receiver_type_metadata => Self::Metadata,
            NDIlib_receiver_type_e_NDIlib_receiver_type_video => Self::Video,
            NDIlib_receiver_type_e_NDIlib_receiver_type_audio => Self::Audio,
            v => Self::Unknown(v),
        }
    }
}
/// A remote command a receiver advertises that it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReceiverCommand {
    /// No command.
    None,
    /// The receiver accepts requests to connect to another source.
    Connect,
    /// A command this crate does not name, with its native value.
    Unknown(u32),
}
impl From<u32> for ReceiverCommand {
    fn from(v: u32) -> Self {
        match v {
            NDIlib_receiver_command_e_NDIlib_receiver_command_none => Self::None,
            NDIlib_receiver_command_e_NDIlib_receiver_command_connect => Self::Connect,
            v => Self::Unknown(v),
        }
    }
}

/// An owned description of one advertised sender, independent of later
/// listener calls.
///
/// Every string is whatever the sender advertised. Text that is not valid
/// UTF-8 is decoded with U+FFFD in place of the offending bytes; only the
/// [identifier](Self::id) keeps its exact bytes.
#[derive(Debug, Clone)]
pub struct AdvertisedSender {
    id: Option<AdvertisedId>,
    name: Option<String>,
    metadata: Option<String>,
    address: Option<String>,
    port: Option<u16>,
    groups: Vec<String>,
    subscribed: bool,
}
impl AdvertisedSender {
    /// The server's identifier for this sender.
    pub fn id(&self) -> Option<&AdvertisedId> {
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
#[derive(Debug, Clone)]
pub struct AdvertisedReceiver {
    id: Option<AdvertisedId>,
    name: Option<String>,
    input_id: Option<AdvertisedId>,
    input_name: Option<String>,
    address: Option<String>,
    streams: Vec<ReceiverStream>,
    commands: Vec<ReceiverCommand>,
    subscribed: bool,
}
impl AdvertisedReceiver {
    /// The server's identifier for this receiver.
    pub fn id(&self) -> Option<&AdvertisedId> {
        self.id.as_ref()
    }
    /// The receiver's name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    /// The identifier of the input group the receiver belongs to.
    pub fn input_id(&self) -> Option<&AdvertisedId> {
        self.input_id.as_ref()
    }
    /// The name of the input group the receiver belongs to.
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

/// The most one snapshot or event batch copies out of the SDK.
const COPY_LIMIT: usize = 16 * 1024 * 1024;
/// The longest single string copied out of the SDK.
const STRING_LIMIT: usize = 4 * 1024 * 1024;

/// Bounds what one snapshot or event batch copies, as a whole rather than field
/// by field: how much there is to copy is decided by peers on the network.
struct Budget(usize);
impl Budget {
    fn new() -> Self {
        Self(COPY_LIMIT)
    }
    fn take(&mut self, len: usize) -> Result<()> {
        self.0 = self.0.checked_sub(len).ok_or_else(|| {
            Error::InvalidFrame("Discovery Server data exceeds the 16 MiB copy limit".into())
        })?;
        Ok(())
    }
    /// Borrow a string the SDK supplied, or `None` for a null pointer.
    ///
    /// # Safety
    ///
    /// A non-null `value` must be readable up to and including its NUL
    /// terminator for `'a`.
    unsafe fn c_str<'a>(&mut self, value: *const c_char) -> Result<Option<&'a CStr>> {
        if value.is_null() {
            return Ok(None);
        }
        // Find the terminator first, so that no reference ever covers bytes
        // beyond the limit.
        let limit = self.0.min(STRING_LIMIT);
        // SAFETY: every byte before the terminator is readable.
        let len = (0..limit)
            .find(|&len| unsafe { *value.add(len) } == 0)
            .ok_or_else(|| {
                Error::InvalidFrame("Discovery Server string exceeds the copy limit".into())
            })?;
        self.take(len + 1)?;
        // SAFETY: the terminator was found, and the caller keeps the string
        // readable for `'a`.
        Ok(Some(unsafe { CStr::from_ptr(value) }))
    }
    /// Copy text for display. Bytes that are not UTF-8 become U+FFFD, so
    /// nothing a peer advertises can make a snapshot or event undecodable.
    ///
    /// # Safety
    ///
    /// As for [`Self::c_str`].
    unsafe fn text(&mut self, value: *const c_char) -> Result<Option<String>> {
        // SAFETY: forwarded to the caller's obligations.
        let text = unsafe { self.c_str(value)? };
        Ok(text.map(|text| text.to_string_lossy().into_owned()))
    }
    /// Copy an identifier byte for byte, so it can be passed back unchanged.
    ///
    /// # Safety
    ///
    /// As for [`Self::c_str`].
    unsafe fn id(&mut self, value: *const c_char) -> Result<Option<AdvertisedId>> {
        // SAFETY: forwarded to the caller's obligations.
        let id = unsafe { self.c_str(value)? };
        Ok(id.map(|id| AdvertisedId(id.to_owned())))
    }
    /// Borrow an array the SDK supplied.
    ///
    /// # Safety
    ///
    /// A non-null `value` must point to `count` initialized elements that stay
    /// readable for `'a`.
    unsafe fn slice<'a, T>(&mut self, value: *const T, count: u32) -> Result<&'a [T]> {
        let count = count as usize;
        if count == 0 {
            return Ok(&[]);
        }
        if value.is_null() || count > 65_536 {
            return Err(Error::InvalidFrame(
                "invalid Discovery Server array pointer or count".into(),
            ));
        }
        let bytes = count
            .checked_mul(std::mem::size_of::<T>())
            .filter(|n| *n <= isize::MAX as usize)
            .ok_or_else(|| Error::InvalidFrame("Discovery Server array size overflow".into()))?;
        self.take(bytes)?;
        // SAFETY: the pointer is non-null and the caller vouches for `count`
        // elements, whose total size was just checked.
        Ok(unsafe { std::slice::from_raw_parts(value, count) })
    }
}

/// A native listener and the strings its creation settings point to.
struct Listener<A: ListenerApi> {
    instance: A::Instance,
    api: &'static A,
    _url: Option<CString>,
    _config: Option<CString>,
    _ndi: NDI,
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
    /// Resolve the complete table, then create the native listener.
    fn create(ndi: &NDI, options: &ListenerOptions, config: Option<CString>) -> Result<Self> {
        let api = A::table(ndi)?;
        let url = options.server.as_deref().map(CString::new).transpose()?;
        // SAFETY: both strings are owned by this call and retained by the
        // listener for as long as the native instance exists.
        let instance = unsafe {
            api.create(
                ndi,
                url.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
                config.as_ref().map(|config| config.as_ptr()),
            )?
        }
        .ok_or_else(|| {
            Error::InitializationFailed(
                "the SDK created no listener; it needs a reachable Discovery Server".into(),
            )
        })?;
        Ok(Self {
            instance,
            api,
            _url: url,
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
        if url.is_null() {
            return None;
        }
        // SAFETY: a non-null result is a NUL-terminated string the SDK keeps
        // for the listener's lifetime.
        let url = unsafe { CStr::from_ptr(url) };
        Some(url.to_string_lossy().into_owned())
    }
    fn wait_for_changes(&mut self, timeout: Duration) -> Result<bool> {
        let timeout = to_ms_checked(timeout)?;
        // SAFETY: the listener instance is live for this borrow.
        Ok(unsafe { self.api.wait_for_entries(self.instance, timeout) })
    }
    /// The native membership snapshot, valid until the next call on this listener.
    fn entries<'a>(&'a mut self, budget: &mut Budget) -> Result<&'a [A::Entry]> {
        let mut count = 0;
        // SAFETY: the exclusive borrow keeps the SDK-owned array unchanged for
        // `'a`; the budget validates the pointer and count before use.
        unsafe { budget.slice(self.api.entries(self.instance, &mut count), count) }
    }
    fn subscription(&mut self, id: &AdvertisedId, subscribe: bool) {
        // SAFETY: the listener instance is live and `id` outlives the call.
        unsafe {
            if subscribe {
                self.api.subscribe(self.instance, id.0.as_ptr());
            } else {
                self.api.unsubscribe(self.instance, id.0.as_ptr());
            }
        };
    }
    fn capture_events(&mut self, wait: Wait) -> Result<Vec<ListenerEvent>> {
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
        let timeout = wait.millis()?;
        let mut count = 0;
        // SAFETY: the listener instance is live for this borrow.
        let events = unsafe { self.api.events(self.instance, &mut count, timeout) };
        if events.is_null() {
            // No event arrived within the wait.
            return Ok(Vec::new());
        }
        let batch = Batch {
            listener: self,
            events,
        };
        let mut budget = Budget::new();
        // SAFETY: the batch stays allocated until `batch` is dropped below.
        let raw = unsafe { budget.slice(batch.events, count)? };
        raw.iter()
            .map(|event| {
                // SAFETY: the strings belong to the batch, which is still
                // allocated.
                unsafe {
                    Ok(ListenerEvent {
                        id: budget.id(event.p_uuid)?,
                        name: budget.text(event.p_name)?,
                        value: budget.text(event.p_value)?,
                    })
                }
            })
            .collect()
    }
}

/// Lists the senders registered with a Discovery Server and reports their
/// events.
///
/// # Threading
///
/// A listener is [`Send`] but not [`Sync`]: it may be moved to another thread,
/// and is used by one thread at a time. Snapshots and events are plain owned
/// data and can be shared freely.
pub struct SendListener {
    inner: Listener<sys::SenderDiscovery>,
}
/// Lists the receivers registered with a Discovery Server, reports their
/// events, and asks them to change source. Remote control is always an
/// explicit call.
///
/// # Threading
///
/// A listener is [`Send`] but not [`Sync`]: it may be moved to another thread,
/// and is used by one thread at a time. Snapshots and events are plain owned
/// data and can be shared freely.
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
    () => {
        /// Whether the listener is connected to its Discovery Server.
        pub fn is_connected(&self) -> bool {
            self.inner.is_connected()
        }
        /// The Discovery Server endpoint the listener uses, when the SDK
        /// reports one.
        pub fn server_url(&self) -> Option<String> {
            self.inner.server_url()
        }
        /// Wait for the advertised membership to change; a zero timeout polls.
        ///
        /// # Errors
        ///
        /// Returns [`Error::InvalidConfiguration`] when `timeout` exceeds
        /// [`crate::MAX_TIMEOUT`].
        pub fn wait_for_changes(&mut self, timeout: Duration) -> Result<bool> {
            self.inner.wait_for_changes(timeout)
        }
        /// Ask to receive the events of the peer `id` names. The next snapshot
        /// reports whether the subscription is active.
        pub fn subscribe(&mut self, id: &AdvertisedId) {
            self.inner.subscription(id, true)
        }
        /// Ask to stop receiving the events of the peer `id` names.
        pub fn unsubscribe(&mut self, id: &AdvertisedId) {
            self.inner.subscription(id, false)
        }
        /// Take the events that arrived since the last call, in arrival order,
        /// waiting up to `wait` for the first one. An empty batch means none
        /// arrived in time.
        ///
        /// # Errors
        ///
        /// Returns [`Error::InvalidConfiguration`] for a finite wait the SDK
        /// cannot express, and [`Error::InvalidFrame`] when the batch exceeds
        /// the 16 MiB copy limit. The native batch is released either way.
        pub fn capture_events(&mut self, wait: Wait) -> Result<Vec<ListenerEvent>> {
            self.inner.capture_events(wait)
        }
    };
}
impl SendListener {
    /// Connect to the Discovery Server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] when the selected runtime
    /// cannot provide [sender discovery](RuntimeFeature::SenderDiscovery),
    /// which needs NDI 6.4.0 or later; [`Error::InvalidCString`] when the
    /// endpoint contains a NUL byte; and [`Error::InitializationFailed`] when
    /// the SDK creates no listener, as it does when no Discovery Server is
    /// configured or reachable.
    pub fn new(ndi: &NDI, options: &ListenerOptions) -> Result<Self> {
        Ok(Self {
            inner: Listener::create(ndi, options, None)?,
        })
    }
    /// Create with retained Advanced configuration.
    #[cfg(feature = "advanced")]
    pub fn new_with_config(
        ndi: &NDI,
        options: &ListenerOptions,
        config: &crate::advanced::AdvancedConfig,
    ) -> Result<Self> {
        let config = Some(CString::new(config.expose_json())?);
        Ok(Self {
            inner: Listener::create(ndi, options, config)?,
        })
    }
    listener_methods!();
    /// Copy the senders currently advertised.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] when the SDK's snapshot is malformed or
    /// exceeds the 16 MiB copy limit. Text that is not valid UTF-8 is decoded
    /// lossily and never fails the snapshot.
    pub fn snapshot(&mut self) -> Result<Vec<AdvertisedSender>> {
        let mut budget = Budget::new();
        let raw = self.inner.entries(&mut budget)?;
        raw.iter()
            .map(|sender| {
                // SAFETY: the snapshot and everything it points to stay valid
                // until the next call on this listener, after this copy.
                unsafe {
                    let groups = budget
                        .slice(sender.p_groups.cast_const(), sender.num_groups)?
                        .iter()
                        .filter_map(|group| budget.text(*group).transpose())
                        .collect::<Result<_>>()?;
                    Ok(AdvertisedSender {
                        id: budget.id(sender.p_uuid)?,
                        name: budget.text(sender.p_name)?,
                        metadata: budget.text(sender.p_metadata)?,
                        address: budget.text(sender.p_address)?,
                        port: u16::try_from(sender.port).ok(),
                        groups,
                        subscribed: sender.events_subscribed,
                    })
                }
            })
            .collect()
    }
}
impl RecvListener {
    /// Connect to the Discovery Server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] when the selected runtime
    /// cannot provide [receiver discovery](RuntimeFeature::ReceiverDiscovery),
    /// which needs NDI 6.4.0 or later; [`Error::InvalidCString`] when the
    /// endpoint contains a NUL byte; and [`Error::InitializationFailed`] when
    /// the SDK creates no listener, as it does when no Discovery Server is
    /// configured or reachable.
    pub fn new(ndi: &NDI, options: &ListenerOptions) -> Result<Self> {
        Ok(Self {
            inner: Listener::create(ndi, options, None)?,
        })
    }
    /// Create with retained Advanced configuration.
    #[cfg(feature = "advanced")]
    pub fn new_with_config(
        ndi: &NDI,
        options: &ListenerOptions,
        config: &crate::advanced::AdvancedConfig,
    ) -> Result<Self> {
        let config = Some(CString::new(config.expose_json())?);
        Ok(Self {
            inner: Listener::create(ndi, options, config)?,
        })
    }
    listener_methods!();
    /// Copy the receivers currently advertised.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] when the SDK's snapshot is malformed or
    /// exceeds the 16 MiB copy limit. Text that is not valid UTF-8 is decoded
    /// lossily and never fails the snapshot.
    pub fn snapshot(&mut self) -> Result<Vec<AdvertisedReceiver>> {
        let mut budget = Budget::new();
        let raw = self.inner.entries(&mut budget)?;
        raw.iter()
            .map(|receiver| {
                // SAFETY: the snapshot and everything it points to stay valid
                // until the next call on this listener, after this copy.
                unsafe {
                    let streams = budget
                        .slice(receiver.p_streams.cast_const(), receiver.num_streams)?
                        .iter()
                        .copied()
                        .map(ReceiverStream::from)
                        .collect();
                    let commands = budget
                        .slice(receiver.p_commands.cast_const(), receiver.num_commands)?
                        .iter()
                        .copied()
                        .map(ReceiverCommand::from)
                        .collect();
                    Ok(AdvertisedReceiver {
                        id: budget.id(receiver.p_uuid)?,
                        name: budget.text(receiver.p_name)?,
                        input_id: budget.id(receiver.p_input_uuid)?,
                        input_name: budget.text(receiver.p_input_name)?,
                        address: budget.text(receiver.p_address)?,
                        streams,
                        commands,
                        subscribed: receiver.events_subscribed,
                    })
                }
            })
            .collect()
    }
    /// Ask the receiver `id` names to connect to the NDI source called
    /// `source`.
    ///
    /// The request is sent only to a receiver that currently advertises
    /// [`ReceiverCommand::Connect`]. Success means the request was sent, not
    /// that the receiver acted on it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] when `source` contains a NUL byte,
    /// [`Error::UnsupportedCapability`] when no advertised receiver has this
    /// identifier or the receiver does not accept connection requests,
    /// [`Error::NativeOperationFailed`] when the SDK cannot send the request,
    /// and the errors of [`Self::snapshot`].
    pub fn request_connect(&mut self, id: &AdvertisedId, source: &str) -> Result<()> {
        self.request(id, Some(source))
    }
    /// Ask the receiver `id` names to disconnect from its current source.
    ///
    /// # Errors
    ///
    /// As for [`Self::request_connect`].
    pub fn request_disconnect(&mut self, id: &AdvertisedId) -> Result<()> {
        self.request(id, None)
    }
    fn request(&mut self, id: &AdvertisedId, source: Option<&str>) -> Result<()> {
        let source = source.map(CString::new).transpose()?;
        // The SDK accepts this command only for a receiver that advertises it.
        // Check a fresh snapshot rather than trust what the caller last saw.
        let refusal = match self
            .snapshot()?
            .iter()
            .find(|receiver| receiver.id.as_ref() == Some(id))
        {
            None => Some("no advertised receiver has this identifier"),
            Some(receiver) if !receiver.commands.contains(&ReceiverCommand::Connect) => {
                Some("the receiver does not accept connection requests")
            }
            Some(_) => None,
        };
        if let Some(reason) = refusal {
            return Err(Error::UnsupportedCapability {
                capability: "remote receiver source control".into(),
                reason: reason.into(),
            });
        }
        // SAFETY: the listener instance is live and both strings outlive the call.
        if unsafe {
            self.inner.api.NDIlib_recv_listener_send_connect(
                self.inner.instance,
                id.0.as_ptr(),
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
    fn finite_wait_never_turns_into_infinity() {
        assert_eq!(Wait::For(Duration::ZERO).millis().unwrap(), 0);
        assert_eq!(Wait::For(Duration::from_nanos(1)).millis().unwrap(), 1);
        assert_eq!(
            Wait::For(Duration::from_millis(u32::MAX as u64 - 1))
                .millis()
                .unwrap(),
            u32::MAX - 1
        );
        assert!(
            Wait::For(Duration::from_nanos((u32::MAX as u64 - 1) * 1_000_000 + 1))
                .millis()
                .is_err()
        );
        assert_eq!(Wait::Forever.millis().unwrap(), u32::MAX);
    }
    #[test]
    fn bounded_copy_preserves_null_empty_and_unknown_values() {
        let mut budget = Budget::new();
        // SAFETY: every non-null pointer is a live NUL-terminated string.
        unsafe {
            assert_eq!(budget.text(ptr::null()).unwrap(), None);
            assert_eq!(budget.text(c"".as_ptr()).unwrap(), Some(String::new()));
            assert_eq!(budget.id(ptr::null()).unwrap(), None);
            assert!(budget.slice::<u32>(ptr::null(), 1).is_err());
            assert!(budget.slice::<u32>(ptr::null(), 0).unwrap().is_empty());
        }
        assert_eq!(ReceiverStream::from(3), ReceiverStream::Audio);
        assert_eq!(ReceiverStream::from(999), ReceiverStream::Unknown(999));
        assert!(ListenerOptions::default().server("a,b").is_err());
    }
    #[test]
    fn text_is_decoded_lossily_and_identifiers_keep_their_bytes() {
        let malformed = c"caf\xe9 \xff";
        let mut budget = Budget::new();
        // SAFETY: the pointer is a live NUL-terminated string.
        let (text, id) = unsafe {
            (
                budget.text(malformed.as_ptr()).unwrap().unwrap(),
                budget.id(malformed.as_ptr()).unwrap().unwrap(),
            )
        };
        assert_eq!(text, "caf\u{fffd} \u{fffd}");
        assert_eq!(id.as_bytes(), malformed.to_bytes());
        assert_eq!(id.to_string(), text);
        assert_eq!(id, AdvertisedId::new(malformed.to_bytes()).unwrap());
        assert_ne!(id, AdvertisedId::new(text).unwrap());
        assert!(AdvertisedId::new("a\0b").is_err());
    }
    #[test]
    fn copies_are_charged_to_one_budget() {
        let mut budget = Budget(8);
        // SAFETY: the pointer is a live NUL-terminated string.
        unsafe {
            assert_eq!(
                budget.text(c"abc".as_ptr()).unwrap().as_deref(),
                Some("abc")
            );
            assert_eq!(
                budget.text(c"abc".as_ptr()).unwrap().as_deref(),
                Some("abc")
            );
            assert!(matches!(
                budget.text(c"a".as_ptr()),
                Err(Error::InvalidFrame(_))
            ));
        }
    }
}
