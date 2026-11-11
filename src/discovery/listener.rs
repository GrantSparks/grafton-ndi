use crate::{
    sys::{self, bindings::*},
    to_ms_checked, Error, Result, NDI,
};
use std::{
    ffi::{c_char, CString},
    marker::PhantomData,
    ptr,
    rc::Rc,
    time::Duration,
};

/// The operations the sender and receiver listener tables have in common.
trait ListenerApi: Sized + 'static {
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
        $Table:ty, $field:ident, $capability:literal,
        $Instance:ty, $Entry:ty, $Create:ident,
        $create:ident, $destroy:ident, $is_connected:ident, $server_url:ident, $entries:ident,
        $wait:ident, $subscribe:ident, $unsubscribe:ident, $events:ident, $free_events:ident
    ) => {
        impl ListenerApi for $Table {
            type Instance = $Instance;
            type Entry = $Entry;

            fn table(ndi: &NDI) -> Result<&'static Self> {
                ndi.api().$field.require($capability)
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
                            capability: $capability.into(),
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
    "sender discovery",
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
    "receiver discovery",
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

/// One Discovery Server endpoint. Redundancy lists belong to AdvertiserOptions.
#[derive(Debug, Clone, Default)]
pub struct ListenerOptions {
    server: Option<String>,
}
impl ListenerOptions {
    /// Configure a single endpoint. A comma-separated list is rejected.
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
/// An opaque advertised identifier; no UUID spelling is assumed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AdvertisedId(String);
impl AdvertisedId {
    /// Validate an identifier for sending subscription or control requests.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        CString::new(value.as_str())?;
        Ok(Self(value))
    }
    /// Exact identifier returned by the server.
    pub fn as_str(&self) -> &str {
        &self.0
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
/// An event's original identifier, name, and value. Unknown names are preserved.
#[derive(Debug, Clone)]
pub struct ListenerEvent {
    id: Option<AdvertisedId>,
    name: Option<String>,
    value: Option<String>,
}
impl ListenerEvent {
    /// Originating endpoint, if supplied by the server.
    pub fn id(&self) -> Option<&AdvertisedId> {
        self.id.as_ref()
    }
    /// Exact event name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    /// Exact event value.
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
/// An owned sender snapshot, independent of subsequent listener calls.
#[derive(Debug, Clone)]
pub struct AdvertisedSender {
    id: Option<AdvertisedId>,
    name: Option<String>,
    metadata: Option<String>,
    address: Option<String>,
    port: i32,
    groups: Vec<Option<String>>,
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
    /// The sender's network port.
    pub fn port(&self) -> i32 {
        self.port
    }
    /// The discovery groups the sender belongs to.
    pub fn groups(&self) -> &[Option<String>] {
        &self.groups
    }
    /// Whether this listener is subscribed to the sender's events.
    pub fn events_subscribed(&self) -> bool {
        self.subscribed
    }
}
/// An owned receiver snapshot. Missing native strings remain None.
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
    /// The identifier of the source the receiver is connected to.
    pub fn input_id(&self) -> Option<&AdvertisedId> {
        self.input_id.as_ref()
    }
    /// The name of the source the receiver is connected to.
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

// Bound copied snapshots as a whole, not merely each individual field.
struct Budget(usize);
impl Budget {
    fn new() -> Self {
        Self(16 * 1024 * 1024)
    }
    fn take(&mut self, len: usize) -> Result<()> {
        self.0 = self.0.checked_sub(len).ok_or_else(|| {
            Error::InvalidFrame("Discovery Server snapshot exceeds 16 MiB copy budget".into())
        })?;
        Ok(())
    }
    unsafe fn string(&mut self, value: *const c_char) -> Result<Option<String>> {
        if value.is_null() {
            return Ok(None);
        }
        // SDK contract supplies a readable NUL-terminated string. Scan to the
        // terminator without constructing a slice over bytes beyond it.
        let limit = self.0.min(4 * 1024 * 1024);
        for len in 0..limit {
            if unsafe { *value.add(len) } == 0 {
                self.take(len + 1)?;
                let bytes = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), len) };
                return std::str::from_utf8(bytes)
                    .map(|s| Some(s.to_owned()))
                    .map_err(|e| Error::InvalidUtf8(e.to_string()));
            }
        }
        Err(Error::InvalidFrame(
            "Discovery Server string exceeds bounded copy limit".into(),
        ))
    }
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
        Ok(unsafe { std::slice::from_raw_parts(value, count) })
    }
}

struct Listener<A: ListenerApi> {
    instance: A::Instance,
    api: &'static A,
    _url: Option<CString>,
    _config: Option<CString>,
    _ndi: NDI,
    _thread: PhantomData<Rc<()>>,
}
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
            Error::InitializationFailed("Discovery Server listener creation failed".into())
        })?;
        Ok(Self {
            instance,
            api,
            _url: url,
            _config: config,
            _ndi: ndi.clone(),
            _thread: PhantomData,
        })
    }
    fn is_connected(&self) -> bool {
        // SAFETY: the listener instance is live for this borrow.
        unsafe { self.api.is_connected(self.instance) }
    }
    fn server_url(&self) -> Result<Option<String>> {
        // SAFETY: the SDK returns null or a NUL-terminated string it owns.
        unsafe { Budget::new().string(self.api.server_url(self.instance)) }
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
    fn subscription(&mut self, id: &AdvertisedId, subscribe: bool) -> Result<()> {
        let id = CString::new(id.as_str())?;
        // SAFETY: the listener instance is live and `id` outlives the call.
        unsafe {
            if subscribe {
                self.api.subscribe(self.instance, id.as_ptr());
            } else {
                self.api.unsubscribe(self.instance, id.as_ptr());
            }
        };
        Ok(())
    }
    fn capture_events(&mut self, wait: Wait) -> Result<Vec<ListenerEvent>> {
        struct Batch<'a, A: ListenerApi> {
            listener: &'a Listener<A>,
            events: *const NDIlib_listener_event,
        }
        impl<A: ListenerApi> Drop for Batch<'_, A> {
            fn drop(&mut self) {
                if !self.events.is_null() {
                    // SAFETY: the batch came from this listener and is freed once.
                    unsafe {
                        self.listener
                            .api
                            .free_events(self.listener.instance, self.events)
                    };
                }
            }
        }
        let timeout = wait.millis()?;
        let mut count = 0;
        // SAFETY: the listener instance is live for this borrow.
        let events = unsafe { self.api.events(self.instance, &mut count, timeout) };
        let batch = Batch {
            listener: self,
            events,
        };
        let mut budget = Budget::new();
        // SAFETY: the batch stays allocated until `batch` is dropped below.
        let raw = unsafe { budget.slice(batch.events, count)? };
        raw.iter()
            .map(|event| unsafe {
                Ok(ListenerEvent {
                    id: budget.string(event.p_uuid)?.map(AdvertisedId),
                    name: budget.string(event.p_name)?,
                    value: budget.string(event.p_value)?,
                })
            })
            .collect()
    }
}

/// A thread-affine sender listener. Use owned snapshots across thread boundaries.
pub struct SendListener {
    inner: Listener<sys::SenderDiscovery>,
}
/// A thread-affine receiver listener. Remote control is always explicit.
pub struct RecvListener {
    inner: Listener<sys::ReceiverDiscovery>,
    latest: Vec<AdvertisedReceiver>,
}

macro_rules! listener_methods {
    () => {
        /// Whether the native listener is connected to its configured server.
        pub fn is_connected(&self) -> bool {
            self.inner.is_connected()
        }
        /// Copy the configured server endpoint.
        pub fn server_url(&self) -> Result<Option<String>> {
            self.inner.server_url()
        }
        /// Wait for a membership change; zero polls immediately.
        pub fn wait_for_changes(&mut self, timeout: Duration) -> Result<bool> {
            self.inner.wait_for_changes(timeout)
        }
        /// Issue a subscription request. Snapshots report whether it is active.
        pub fn subscribe(&mut self, id: &AdvertisedId) -> Result<()> {
            self.inner.subscription(id, true)
        }
        /// Issue an unsubscribe request; this does not acknowledge remote delivery.
        pub fn unsubscribe(&mut self, id: &AdvertisedId) -> Result<()> {
            self.inner.subscription(id, false)
        }
        /// Copy an event batch, freeing the native batch even if decoding fails.
        pub fn capture_events(&mut self, wait: Wait) -> Result<Vec<ListenerEvent>> {
            self.inner.capture_events(wait)
        }
    };
}
impl SendListener {
    /// Create a listener; all required functions are checked before native creation.
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
    /// Copy the complete current sender snapshot before native storage expires.
    pub fn snapshot(&mut self) -> Result<Vec<AdvertisedSender>> {
        let mut budget = Budget::new();
        let raw = self.inner.entries(&mut budget)?;
        raw.iter()
            .map(|s| unsafe {
                let groups = budget
                    .slice(s.p_groups.cast_const(), s.num_groups)?
                    .iter()
                    .map(|v| budget.string(*v))
                    .collect::<Result<_>>()?;
                Ok(AdvertisedSender {
                    id: budget.string(s.p_uuid)?.map(AdvertisedId),
                    name: budget.string(s.p_name)?,
                    metadata: budget.string(s.p_metadata)?,
                    address: budget.string(s.p_address)?,
                    port: s.port,
                    groups,
                    subscribed: s.events_subscribed,
                })
            })
            .collect()
    }
}
impl RecvListener {
    /// Create a receiver listener with explicit remote-control methods.
    pub fn new(ndi: &NDI, options: &ListenerOptions) -> Result<Self> {
        Ok(Self {
            inner: Listener::create(ndi, options, None)?,
            latest: Vec::new(),
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
            latest: Vec::new(),
        })
    }
    listener_methods!();
    /// Copy the complete receiver snapshot, retaining command capabilities for checks.
    pub fn snapshot(&mut self) -> Result<Vec<AdvertisedReceiver>> {
        let mut budget = Budget::new();
        let raw = self.inner.entries(&mut budget)?;
        let copied: Result<Vec<_>> = raw
            .iter()
            .map(|s| unsafe {
                let streams = budget
                    .slice(s.p_streams.cast_const(), s.num_streams)?
                    .iter()
                    .copied()
                    .map(ReceiverStream::from)
                    .collect();
                let commands = budget
                    .slice(s.p_commands.cast_const(), s.num_commands)?
                    .iter()
                    .copied()
                    .map(ReceiverCommand::from)
                    .collect();
                Ok(AdvertisedReceiver {
                    id: budget.string(s.p_uuid)?.map(AdvertisedId),
                    name: budget.string(s.p_name)?,
                    input_id: budget.string(s.p_input_uuid)?.map(AdvertisedId),
                    input_name: budget.string(s.p_input_name)?,
                    address: budget.string(s.p_address)?,
                    streams,
                    commands,
                    subscribed: s.events_subscribed,
                })
            })
            .collect();
        self.latest = copied?;
        Ok(self.latest.clone())
    }
    /// Request a source change. The latest snapshot must advertise Connect support.
    pub fn request_connect(&mut self, id: &AdvertisedId, source: &str) -> Result<()> {
        self.request(id, Some(source))
    }
    /// Explicitly request disconnection from the current source.
    pub fn request_disconnect(&mut self, id: &AdvertisedId) -> Result<()> {
        self.request(id, None)
    }
    fn request(&mut self, id: &AdvertisedId, source: Option<&str>) -> Result<()> {
        // The SDK requires a receiver advertising this command. Refreshing also
        // avoids using stale application data as an invented acknowledgement.
        let snapshots = self.snapshot()?;
        if !snapshots
            .iter()
            .any(|s| s.id.as_ref() == Some(id) && s.commands.contains(&ReceiverCommand::Connect))
        {
            return Err(Error::UnsupportedCapability {
                capability: "receiver source control".into(),
                reason: "receiver is absent or does not advertise the Connect command".into(),
            });
        }
        let id = CString::new(id.as_str())?;
        let source = source.map(CString::new).transpose()?;
        // SAFETY: the listener instance is live and both strings outlive the call.
        if unsafe {
            self.inner.api.NDIlib_recv_listener_send_connect(
                self.inner.instance,
                id.as_ptr(),
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
        let empty = CString::new("").unwrap();
        let invalid = [0xffu8, 0];
        let mut budget = Budget::new();
        unsafe {
            assert_eq!(budget.string(ptr::null()).unwrap(), None);
            assert_eq!(budget.string(empty.as_ptr()).unwrap(), Some(String::new()));
            assert!(budget.string(invalid.as_ptr().cast()).is_err());
            assert!(budget.slice::<u32>(ptr::null(), 1).is_err());
            assert!(budget.slice::<u32>(ptr::null(), 0).unwrap().is_empty());
        }
        assert_eq!(ReceiverStream::from(3), ReceiverStream::Audio);
        assert_eq!(ReceiverStream::from(999), ReceiverStream::Unknown(999));
        assert!(ListenerOptions::default().server("a,b").is_err());
    }
}
