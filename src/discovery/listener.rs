//! Snapshots of, events from, and connection requests to what a Discovery
//! Server advertises.

use super::{Retained, ServerApi, ServerObject};
use crate::{
    finder::{SourceName, SourceText},
    sdk_copy::{
        bounded_sdk_text, copy_lossy, CopyBudget, SpanError, COPY_LIMIT, MAX_SDK_STRING_BYTES,
    },
    sys::{self, bindings::*},
    to_ms_checked, Error, Ndi, ResourceKind, Result,
};
use std::{
    borrow::Cow,
    collections::BTreeSet,
    ffi::{c_char, CStr, CString},
    fmt, ptr,
    time::Duration,
};

/// The operations the sender and receiver listener tables have in common.
trait ListenerApi: ServerApi {
    type Entry;
    type Id: Identifier;
    /// # Safety
    ///
    /// Each method requires `instance` to be a live listener created by this
    /// table; pointer arguments must be valid for the SDK's documented use.
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
        $Table:ty, $Entry:ty, $Id:ty,
        is_connected = $is_connected:ident,
        server_url = $server_url:ident,
        entries = $entries:ident,
        wait = $wait:ident,
        subscribe = $subscribe:ident,
        unsubscribe = $unsubscribe:ident,
        events = $events:ident,
        free_events = $free_events:ident
    ) => {
        impl ListenerApi for $Table {
            type Entry = $Entry;
            type Id = $Id;
            unsafe fn is_connected(&self, instance: Self::Instance) -> bool {
                self.$is_connected(instance.0)
            }
            unsafe fn server_url(&self, instance: Self::Instance) -> *const c_char {
                self.$server_url(instance.0)
            }
            unsafe fn entries(
                &self,
                instance: Self::Instance,
                count: &mut u32,
            ) -> *const Self::Entry {
                self.$entries(instance.0, count)
            }
            unsafe fn wait_for_entries(&self, instance: Self::Instance, timeout_ms: u32) -> bool {
                self.$wait(instance.0, timeout_ms)
            }
            unsafe fn subscribe(&self, instance: Self::Instance, id: *const c_char) {
                self.$subscribe(instance.0, id)
            }
            unsafe fn unsubscribe(&self, instance: Self::Instance, id: *const c_char) {
                self.$unsubscribe(instance.0, id)
            }
            unsafe fn events(
                &self,
                instance: Self::Instance,
                count: &mut u32,
                timeout_ms: u32,
            ) -> *const NDIlib_listener_event {
                self.$events(instance.0, count, timeout_ms)
            }
            unsafe fn free_events(
                &self,
                instance: Self::Instance,
                events: *const NDIlib_listener_event,
            ) {
                self.$free_events(instance.0, events)
            }
        }
    };
}

listener_api!(
    sys::SenderDiscovery,
    NDIlib_sender_t,
    SenderId,
    is_connected = NDIlib_send_listener_is_connected,
    server_url = NDIlib_send_listener_get_server_url,
    entries = NDIlib_send_listener_get_senders,
    wait = NDIlib_send_listener_wait_for_senders,
    subscribe = NDIlib_send_listener_subscribe_events,
    unsubscribe = NDIlib_send_listener_unsubscribe_events,
    events = NDIlib_send_listener_get_events,
    free_events = NDIlib_send_listener_free_events
);
listener_api!(
    sys::ReceiverDiscovery,
    NDIlib_receiver_t,
    ReceiverId,
    is_connected = NDIlib_recv_listener_is_connected,
    server_url = NDIlib_recv_listener_get_server_url,
    entries = NDIlib_recv_listener_get_receivers,
    wait = NDIlib_recv_listener_wait_for_receivers,
    subscribe = NDIlib_recv_listener_subscribe_events,
    unsubscribe = NDIlib_recv_listener_unsubscribe_events,
    events = NDIlib_recv_listener_get_events,
    free_events = NDIlib_recv_listener_free_events
);

/// The Discovery Server a listener connects to.
///
/// Without an explicit server the SDK uses the Discovery Server from the NDI
/// configuration. If that names none either, the listener cannot be created.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
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
trait Identifier: Sized + Clone + Ord {
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
    name: Option<SourceText>,
    metadata: Option<String>,
    address: Option<SourceText>,
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
        self.name.as_ref().map(SourceText::as_str)
    }
    /// The connection metadata the sender advertises.
    pub fn metadata(&self) -> Option<&str> {
        self.metadata.as_deref()
    }
    /// The sender's network address.
    pub fn address(&self) -> Option<&str> {
        self.address.as_ref().map(SourceText::as_str)
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
    /// Convert this advertised sender into a connection target.
    ///
    /// Returns `None` when the server supplied no source name. An address is
    /// included only when both the address and a valid port were advertised.
    /// IPv6 literals are enclosed in brackets before their port is appended.
    pub fn to_source(&self) -> Option<crate::Source> {
        let mut source = crate::Source::from_text(self.name.as_ref()?.clone(), None);
        if let (Some(address), Some(port)) = (&self.address, self.port) {
            let endpoint = address.with_port(port);
            source = source.with_address(crate::SourceAddress::from_text(endpoint));
        }
        Some(source)
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

/// Compare only the prefix that could match, including its exact terminator.
/// No native string, including one compared with a longer caller-supplied ID,
/// is read beyond the SDK string bound.
///
/// # Safety
/// A non-null `value` must remain readable byte by byte until the first NUL,
/// mismatching byte, or `MAX_SDK_STRING_BYTES`, whichever comes first.
unsafe fn receiver_id_matches(value: *const c_char, id: &ReceiverId) -> bool {
    if value.is_null() {
        return false;
    }
    for (offset, &expected) in id
        .as_c_str()
        .to_bytes_with_nul()
        .iter()
        .take(MAX_SDK_STRING_BYTES)
        .enumerate()
    {
        // SAFETY: the comparison stops at the first mismatch or NUL, and the
        // iterator caps every native read at the SDK string bound.
        let byte = unsafe { value.cast::<u8>().add(offset).read() };
        if byte != expected {
            return false;
        }
        if byte == 0 {
            return true;
        }
    }
    false
}

/// Validate one receiver's connection-request precondition without copying
/// the registry or inspecting fields unrelated to its ID and commands.
///
/// # Safety
/// Every non-null UUID must satisfy `receiver_id_matches`'s readable bound.
/// A matched entry's non-null commands pointer must hold its stated number
/// of initialized commands. All of this storage must remain unchanged for
/// the call. Other pointer fields are never read.
pub(crate) unsafe fn require_receiver_connect(
    receivers: &[NDIlib_receiver_t],
    id: &ReceiverId,
    budget: &mut CopyBudget,
) -> Result<()> {
    let Some(receiver) = receivers.iter().find(|receiver| {
        // SAFETY: the caller keeps each UUID's bounded prefix readable.
        unsafe { receiver_id_matches(receiver.p_uuid, id) }
    }) else {
        return Err(Error::ReceiverRequestNotAccepted { advertised: false });
    };
    // SAFETY: the caller keeps the matched command array readable. Preflight
    // validates its pointer, count, span and budget before constructing a slice.
    let commands =
        match unsafe { budget.slice(receiver.p_commands.cast_const(), receiver.num_commands) } {
            Ok(commands) => commands,
            Err(SpanError::Array(_)) => {
                return Err(Error::ReceiverRequestNotAccepted { advertised: true });
            }
            Err(SpanError::Failed(error)) => return Err(error),
        };
    if commands.contains(&NDIlib_receiver_command_e_NDIlib_receiver_command_connect) {
        Ok(())
    } else {
        Err(Error::ReceiverRequestNotAccepted { advertised: true })
    }
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
    Some(copy_lossy(bytes))
}

/// A listener adds membership operations to the shared lifecycle owner.
struct Listener<A: ListenerApi>(ServerObject<A, BTreeSet<A::Id>>);

// SAFETY: the default set is empty. Subscription changes retain each ID until
// its own listener unsubscribes or is destroyed; no pre-destroy calls are needed.
unsafe impl<A: ListenerApi> Retained<A> for BTreeSet<A::Id> {}

const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Listener<sys::SenderDiscovery>>();
    assert_send::<Listener<sys::ReceiverDiscovery>>();
};

impl<A: ListenerApi> Listener<A>
where
    A: ServerApi<Options = ListenerOptions>,
{
    fn new(ndi: &Ndi, options: &ListenerOptions) -> Result<Self> {
        ServerObject::new(ndi, options).map(Self)
    }
}

impl<A: ListenerApi> Listener<A> {
    fn is_connected(&self) -> bool {
        // SAFETY: the listener instance is live for this borrow.
        unsafe { self.0.api.is_connected(self.0.instance()) }
    }
    fn server_url(&self) -> Option<String> {
        // SAFETY: the listener instance is live for this borrow.
        let url = unsafe { self.0.api.server_url(self.0.instance()) };
        // SAFETY: Processing.NDI.{Send,Recv}Listener.h only documents that the
        // result may be null; it promises neither a lifetime nor a terminator.
        // The bounded endpoint copy completes within this borrow, and this
        // Send, !Sync owner prevents another listener call during that copy.
        // We assume a non-null getter result stays readable through its first
        // NUL or the SDK string bound for that interval. The real-server getter
        // check in tests/discovery_server.rs exercises that SDK assumption.
        unsafe { endpoint(url) }
    }
    fn wait_for_change(&mut self, timeout: Duration) -> Result<bool> {
        let timeout = to_ms_checked(timeout)?;
        // SAFETY: the listener instance is live for this borrow.
        Ok(unsafe { self.0.api.wait_for_entries(self.0.instance(), timeout) })
    }
    /// The native membership snapshot, valid until the next call on this listener.
    fn entries<'a>(&'a mut self, budget: &mut CopyBudget) -> Result<&'a [A::Entry]> {
        let mut count = 0;
        // SAFETY: the exclusive borrow keeps the SDK-owned array unchanged for
        // `'a`; the budget validates the pointer and count before use.
        Ok(unsafe { budget.slice(self.0.api.entries(self.0.instance(), &mut count), count) }?)
    }
    fn subscription(&mut self, id: &A::Id, subscribe: bool) {
        if subscribe {
            if !self.0.retained().contains(id) {
                self.0.retained_mut().insert(id.clone());
            }
            let retained = self
                .0
                .retained()
                .get(id)
                .expect("subscribed identifier was retained");
            // SAFETY: the listener and its stable identifier allocation stay
            // live until unsubscribe returns or after native destruction.
            unsafe {
                self.0
                    .api
                    .subscribe(self.0.instance(), retained.as_c_str().as_ptr())
            };
        } else {
            let retained = self.0.retained().get(id).unwrap_or(id);
            // SAFETY: a retained identifier stays live through unsubscribe;
            // an identifier never subscribed is borrowed only for this call.
            unsafe {
                self.0
                    .api
                    .unsubscribe(self.0.instance(), retained.as_c_str().as_ptr())
            };
            self.0.retained_mut().remove(id);
        }
    }
    fn next_events(&mut self, timeout: Duration) -> Result<Vec<ListenerEvent<A::Id>>> {
        struct Batch<'a, A: ListenerApi> {
            listener: &'a Listener<A>,
            events: *const NDIlib_listener_event,
        }
        impl<A: ListenerApi> Drop for Batch<'_, A> {
            fn drop(&mut self) {
                // SAFETY: the batch came from this listener and is freed once.
                unsafe {
                    self.listener
                        .0
                        .api
                        .free_events(self.listener.0.instance(), self.events)
                };
            }
        }
        let timeout = to_ms_checked(timeout)?;
        let mut count = 0;
        // SAFETY: the listener instance is live for this borrow.
        let events = unsafe { self.0.api.events(self.0.instance(), &mut count, timeout) };
        let mut budget = copy_budget();
        if events.is_null() {
            // No event arrived only if the count agrees. A null batch with
            // a positive count is malformed, as a membership snapshot is.
            // SAFETY: null is permitted and checked without dereferencing.
            unsafe { budget.slice(events, count) }?;
            return Ok(Vec::new());
        }
        let batch = Batch {
            listener: self,
            events,
        };
        // SAFETY: the batch stays allocated until `batch` is dropped below.
        let raw = unsafe { budget.slice(batch.events, count) }?;
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
        f.debug_struct("SendListener")
            .field("server", &self.inner.0._url)
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for RecvListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvListener")
            .field("server", &self.inner.0._url)
            .finish_non_exhaustive()
    }
}

macro_rules! listener_methods {
    ($Id:ty) => {
        /// Whether the listener is connected to its Discovery Server right
        /// now. A listener that is not connected keeps trying. This reports
        /// registry connectivity, not progress of any receiver media.
        #[doc(alias = "NDIlib_send_listener_is_connected")]
        #[doc(alias = "NDIlib_recv_listener_is_connected")]
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
        #[doc(alias = "NDIlib_send_listener_get_server_url")]
        #[doc(alias = "NDIlib_recv_listener_get_server_url")]
        pub fn server_url(&self) -> Option<String> {
            self.inner.server_url()
        }
        /// Wait for the advertised membership to change; a zero timeout polls.
        ///
        /// # Errors
        ///
        /// Returns [`Error::InvalidInput`] when `timeout` exceeds
        /// [`crate::MAX_TIMEOUT`].
        #[doc(alias = "NDIlib_send_listener_wait_for_senders")]
        #[doc(alias = "NDIlib_recv_listener_wait_for_receivers")]
        pub fn wait_for_change(&mut self, timeout: Duration) -> Result<bool> {
            self.inner.wait_for_change(timeout)
        }
        /// Ask to receive the events of the peer `id` names. The next snapshot
        /// reports whether the subscription is active.
        ///
        /// The listener keeps its own copy of the identifier until
        /// [`unsubscribe`](Self::unsubscribe) returns or the listener is dropped.
        #[doc(alias = "NDIlib_send_listener_subscribe_events")]
        #[doc(alias = "NDIlib_recv_listener_subscribe_events")]
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
        #[doc(alias = "NDIlib_send_listener_unsubscribe_events")]
        #[doc(alias = "NDIlib_recv_listener_unsubscribe_events")]
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
        /// malformed, or [`Error::AllocationFailed`] when storage cannot be
        /// reserved. The SDK has already
        /// handed a rejected batch over, so it is freed and its events are lost.
        #[doc(alias = "NDIlib_send_listener_get_events")]
        #[doc(alias = "NDIlib_recv_listener_get_events")]
        pub fn next_events(&mut self, timeout: Duration) -> Result<Vec<ListenerEvent<$Id>>> {
            self.inner.next_events(timeout)
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
    #[doc(alias = "NDIlib_send_listener_create")]
    #[doc(alias = "NDIlib_send_listener_create_ex")]
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
    /// array is malformed. [`Error::AllocationFailed`] when storage cannot be
    /// reserved.
    #[doc(alias = "NDIlib_send_listener_get_senders")]
    pub fn current_senders(&mut self) -> Result<Vec<AdvertisedSender>> {
        let mut budget = copy_budget();
        let raw = self.inner.entries(&mut budget)?;
        budget.collect_entries(raw, |sender, budget| {
            // SAFETY: the snapshot and everything it points to stay valid
            // until the next call on this listener, after this copy.
            unsafe {
                let groups = budget.slice(sender.p_groups.cast_const(), sender.num_groups)?;
                let groups = budget.collect(groups, |group, budget| Ok(budget.text(*group)?))?;
                Ok(AdvertisedSender {
                    id: budget.id(sender.p_uuid)?.map(SenderId::from_exact),
                    name: budget.source_text(sender.p_name)?,
                    metadata: budget.text(sender.p_metadata)?,
                    address: budget.source_text(sender.p_address)?,
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
    #[doc(alias = "NDIlib_recv_listener_create")]
    #[doc(alias = "NDIlib_recv_listener_create_ex")]
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
    /// array is malformed. [`Error::AllocationFailed`] when storage cannot be
    /// reserved.
    #[doc(alias = "NDIlib_recv_listener_get_receivers")]
    pub fn current_receivers(&mut self) -> Result<Vec<AdvertisedReceiver>> {
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
    /// `source_name`.
    ///
    /// The request checks the listener's current borrowed membership, comparing
    /// exact identifiers and reading only the matched receiver's commands. It
    /// is sent only when that receiver advertises [`ReceiverCommand::Connect`].
    /// Malformed names, input groups, addresses or streams do not prevent a
    /// request. Success means it was sent, not that the receiver acted on it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when `source_name` is empty, and
    /// [`Error::InvalidCString`] when it contains a NUL byte, before reading
    /// listener membership;
    /// [`Error::ReceiverRequestNotAccepted`] when no receiver the listener
    /// lists has this identifier (`advertised` is false: the listener may not
    /// have connected yet, the receiver may not be registered, or its entry
    /// may have an unreadable identifier; a later request can succeed) or the
    /// receiver does not accept connection requests
    /// (`advertised` is true, including a malformed command array);
    /// [`Error::NativeOperationFailed`] when the SDK cannot send the request;
    /// [`Error::UnexpectedSdkResult`] for a malformed top-level membership
    /// array; and [`Error::ResourceLimit`] when that array and the target's
    /// commands exceed the 16 MiB inspection budget. Unrelated fields are
    /// neither scanned nor copied and cannot spend that budget.
    #[doc(alias = "NDIlib_recv_listener_send_connect")]
    pub fn request_connect(&mut self, id: &ReceiverId, source_name: &str) -> Result<()> {
        self.request(id, Some(SourceName::checked(source_name)?))
    }
    /// Ask the receiver `id` names to disconnect from its current source.
    ///
    /// This is the connection request without a source, so it too is sent
    /// only to a receiver that advertises [`ReceiverCommand::Connect`].
    /// It compares exact identifiers and reads only the matched command array;
    /// unrelated fields are neither scanned nor copied.
    ///
    /// # Errors
    ///
    /// As for [`Self::request_connect`], without source-name validation.
    #[doc(alias = "NDIlib_recv_listener_send_connect")]
    pub fn request_disconnect(&mut self, id: &ReceiverId) -> Result<()> {
        self.request(id, None)
    }
    fn request(&mut self, id: &ReceiverId, source: Option<SourceName<'_>>) -> Result<()> {
        let source = source.map(SourceName::into_c_string);
        // The SDK accepts this command only for a receiver that advertises it.
        // Read current membership rather than trust what the caller last saw.
        let mut budget = copy_budget();
        let receivers = self.inner.entries(&mut budget)?;
        // SAFETY: the exclusive listener borrow keeps its SDK-owned entries,
        // UUIDs and matched commands unchanged until this inspection finishes.
        unsafe { require_receiver_connect(receivers, id, &mut budget)? };
        // SAFETY: the listener instance is live and both strings outlive the call.
        unsafe {
            self.inner.0.api.named().NDIlib_recv_listener_send_connect(
                self.inner.0.instance().0,
                id.as_c_str().as_ptr(),
                source.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            )
        }
        .performed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_sources_use_name_and_only_complete_addresses() {
        let mut sender = AdvertisedSender {
            id: None,
            name: Some(SourceText::new("Studio (Camera)".into()).unwrap()),
            metadata: None,
            address: Some(SourceText::new("192.0.2.10".into()).unwrap()),
            port: Some(5960),
            groups: vec![],
            subscribed: false,
        };
        let source = sender.to_source().unwrap();
        assert_eq!(source.name(), "Studio (Camera)");
        assert_eq!(source.address().unwrap().as_str(), "192.0.2.10:5960");
        for (address, endpoint) in [
            ("192.0.2.10", "192.0.2.10:5960"),
            ("camera.local", "camera.local:5960"),
            ("2001:db8::10", "[2001:db8::10]:5960"),
            ("[2001:db8::10]", "[2001:db8::10]:5960"),
        ] {
            sender.address = Some(SourceText::new(address.into()).unwrap());
            let source = sender.to_source().unwrap();
            let actual = source.address().unwrap();
            assert_eq!(actual.as_str(), endpoint);
            assert_eq!(actual.port(), Some(5960));
        }
        sender.port = None;
        assert!(sender.to_source().unwrap().address().is_none());
        sender.port = Some(5960);
        sender.address = None;
        assert!(sender.to_source().unwrap().address().is_none());
        sender.name = None;
        assert!(sender.to_source().is_none());
    }

    #[test]
    fn receiver_request_lookup_uses_exact_ids_and_only_the_matched_commands() {
        let mut commands = [
            999,
            NDIlib_receiver_command_e_NDIlib_receiver_command_connect,
            NDIlib_receiver_command_e_NDIlib_receiver_command_none,
        ];
        let target = ReceiverId::new(b"receiver-\xff".to_vec()).unwrap();
        let matching = NDIlib_receiver_t {
            p_uuid: c"receiver-\xff".as_ptr(),
            // Unrelated fields are intentionally not usable as a snapshot.
            p_name: ptr::dangling(),
            p_input_uuid: ptr::dangling(),
            p_input_name: ptr::dangling(),
            p_address: ptr::dangling(),
            num_streams: 1,
            p_commands: commands.as_mut_ptr(),
            num_commands: commands.len() as u32,
            ..Default::default()
        };
        let unrelated = NDIlib_receiver_t {
            p_uuid: c"receiver-\xff-longer".as_ptr(),
            p_commands: ptr::null_mut(),
            num_commands: 1,
            ..Default::default()
        };
        // SAFETY: UUIDs and the matched commands remain live and unchanged;
        // malformed unrelated fields and unmatched commands are not read.
        unsafe {
            require_receiver_connect(&[unrelated, matching], &target, &mut copy_budget()).unwrap();
            for id in [c"receiver-\xff-longer", c"receiver", c"", c"other"] {
                let receiver = NDIlib_receiver_t {
                    p_uuid: id.as_ptr(),
                    ..matching
                };
                assert!(matches!(
                    require_receiver_connect(&[receiver], &target, &mut copy_budget()),
                    Err(Error::ReceiverRequestNotAccepted { advertised: false })
                ));
            }
            for (pointer, count) in [
                (ptr::null_mut(), 0),
                (ptr::null_mut(), 1),
                (commands.as_mut_ptr(), 1),
            ] {
                let receiver = NDIlib_receiver_t {
                    p_commands: pointer,
                    num_commands: count,
                    ..matching
                };
                assert!(matches!(
                    require_receiver_connect(&[receiver], &target, &mut copy_budget()),
                    Err(Error::ReceiverRequestNotAccepted { advertised: true })
                ));
            }
            assert!(matches!(
                require_receiver_connect(&[], &target, &mut copy_budget()),
                Err(Error::ReceiverRequestNotAccepted { advertised: false })
            ));
            let no_id = NDIlib_receiver_t {
                p_uuid: ptr::null(),
                ..matching
            };
            assert!(matches!(
                require_receiver_connect(&[no_id], &target, &mut copy_budget()),
                Err(Error::ReceiverRequestNotAccepted { advertised: false })
            ));
        }
    }

    #[test]
    fn receiver_request_lookup_does_not_copy_or_budget_unrelated_strings() {
        let text = CString::new(vec![b'x'; MAX_SDK_STRING_BYTES - 1]).unwrap();
        let unrelated = NDIlib_receiver_t {
            p_uuid: c"unrelated".as_ptr(),
            p_name: text.as_ptr(),
            p_input_uuid: text.as_ptr(),
            p_input_name: text.as_ptr(),
            p_address: text.as_ptr(),
            num_streams: 1,
            num_commands: 1,
            ..Default::default()
        };
        let mut receivers = vec![unrelated; 1_000];
        let mut connect = [NDIlib_receiver_command_e_NDIlib_receiver_command_connect];
        receivers.push(NDIlib_receiver_t {
            p_uuid: c"target".as_ptr(),
            p_commands: connect.as_mut_ptr(),
            num_commands: 1,
            ..unrelated
        });
        // Even one unrelated entry cannot be copied within the shared
        // snapshot budget once its four large text fields are included.
        let mut budget = copy_budget();
        // SAFETY: `text` is a live terminated string for every attempted copy.
        unsafe {
            budget.text(text.as_ptr()).unwrap();
            budget.text(text.as_ptr()).unwrap();
            assert!(matches!(
                budget.text(text.as_ptr()),
                Err(crate::sdk_copy::TextError::Failed(Error::ResourceLimit {
                    resource: ResourceKind::DiscoveryServerCopyBytes,
                    ..
                }))
            ));
        }
        let target = ReceiverId::new("target").unwrap();
        let (result, allocations) = crate::test_alloc::allocation_count(|| {
            // SAFETY: all UUIDs and the target's commands remain readable.
            // The malformed unrelated arrays are never inspected.
            unsafe { require_receiver_connect(&receivers, &target, &mut copy_budget()) }
        });
        result.unwrap();
        assert_eq!(allocations, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn receiver_id_comparison_stops_at_nul_mismatch_and_the_native_string_cap() {
        struct Guarded {
            mapping: *mut libc::c_void,
            mapped: usize,
            data: *mut u8,
        }
        impl Guarded {
            fn new(bytes: &[u8]) -> Self {
                // SAFETY: the private mapping owns its initialized prefix and
                // inaccessible last page until Drop releases it.
                unsafe {
                    let page = usize::try_from(libc::sysconf(libc::_SC_PAGESIZE)).unwrap();
                    let body = bytes.len().div_ceil(page) * page;
                    let mapped = body + page;
                    let mapping = libc::mmap(
                        ptr::null_mut(),
                        mapped,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        -1,
                        0,
                    );
                    assert_ne!(mapping, libc::MAP_FAILED);
                    let guard = mapping.cast::<u8>().add(body);
                    assert_eq!(libc::mprotect(guard.cast(), page, libc::PROT_NONE), 0);
                    let data = guard.sub(bytes.len());
                    ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
                    Self {
                        mapping,
                        mapped,
                        data,
                    }
                }
            }
        }
        impl Drop for Guarded {
            fn drop(&mut self) {
                // SAFETY: this is the mapping allocated in new, with no live borrows.
                assert_eq!(unsafe { libc::munmap(self.mapping, self.mapped) }, 0);
            }
        }
        let target = ReceiverId::new("target").unwrap();
        for bytes in [b"t\0".as_slice(), b"\0", b"u", b"target\0", b"targetx"] {
            let native = Guarded::new(bytes);
            // SAFETY: the bytewise comparison's required prefix is readable:
            // it must stop at NUL, mismatch, or the target's exact terminator.
            assert_eq!(
                unsafe { receiver_id_matches(native.data.cast(), &target) },
                bytes == b"target\0"
            );
        }
        let bytes = vec![b'x'; MAX_SDK_STRING_BYTES];
        let native = Guarded::new(&bytes);
        let long_target = ReceiverId::new(vec![b'x'; MAX_SDK_STRING_BYTES + 10]).unwrap();
        // SAFETY: the complete native scan cap is readable; its next byte is
        // intentionally inaccessible even though the caller's ID is longer.
        assert!(!unsafe { receiver_id_matches(native.data.cast(), &long_target) });
        let mut terminated = bytes;
        *terminated.last_mut().unwrap() = 0;
        let native = Guarded::new(&terminated);
        let longest_target = ReceiverId::new(vec![b'x'; MAX_SDK_STRING_BYTES - 1]).unwrap();
        // SAFETY: the longest permitted native string includes its final NUL.
        assert!(unsafe { receiver_id_matches(native.data.cast(), &longest_target) });
    }

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
