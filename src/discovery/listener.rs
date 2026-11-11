use crate::{dispatch::require_optional, to_ms_checked, Error, Result, NDI};
use std::{
    ffi::{c_char, c_void, CString},
    marker::PhantomData,
    ptr,
    rc::Rc,
    time::Duration,
};

type Instance = *mut c_void;
#[repr(C)]
struct Create {
    url: *const c_char,
}
#[repr(C)]
struct RawEvent {
    uuid: *const c_char,
    name: *const c_char,
    value: *const c_char,
}
#[repr(C)]
struct RawSender {
    uuid: *const c_char,
    name: *const c_char,
    metadata: *const c_char,
    address: *const c_char,
    port: i32,
    groups: *const *const c_char,
    num_groups: u32,
    subscribed: bool,
}
#[repr(C)]
struct RawReceiver {
    uuid: *const c_char,
    name: *const c_char,
    input_uuid: *const c_char,
    input_name: *const c_char,
    address: *const c_char,
    streams: *const u32,
    num_streams: u32,
    commands: *const u32,
    num_commands: u32,
    subscribed: bool,
}
type New = unsafe extern "C" fn(*const Create) -> Instance;
type Destroy = unsafe extern "C" fn(Instance);
type Connected = unsafe extern "C" fn(Instance) -> bool;
type Url = unsafe extern "C" fn(Instance) -> *const c_char;
type Snapshot<S> = unsafe extern "C" fn(Instance, *mut u32) -> *const S;
type WaitFn = unsafe extern "C" fn(Instance, u32) -> bool;
type Subscribe = unsafe extern "C" fn(Instance, *const c_char);
type Events = unsafe extern "C" fn(Instance, *mut u32, u32) -> *const RawEvent;
type FreeEvents = unsafe extern "C" fn(Instance, *const RawEvent);
type Connect = unsafe extern "C" fn(Instance, *const c_char, *const c_char) -> bool;

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
    None,
    Metadata,
    Video,
    Audio,
    Unknown(u32),
}
impl From<u32> for ReceiverStream {
    fn from(v: u32) -> Self {
        match v {
            0 => Self::None,
            1 => Self::Metadata,
            2 => Self::Video,
            3 => Self::Audio,
            v => Self::Unknown(v),
        }
    }
}
/// A remote command a receiver advertises that it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReceiverCommand {
    None,
    Connect,
    Unknown(u32),
}
impl From<u32> for ReceiverCommand {
    fn from(v: u32) -> Self {
        match v {
            0 => Self::None,
            1 => Self::Connect,
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
    pub fn id(&self) -> Option<&AdvertisedId> {
        self.id.as_ref()
    }
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    pub fn metadata(&self) -> Option<&str> {
        self.metadata.as_deref()
    }
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }
    pub fn port(&self) -> i32 {
        self.port
    }
    pub fn groups(&self) -> &[Option<String>] {
        &self.groups
    }
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
    pub fn id(&self) -> Option<&AdvertisedId> {
        self.id.as_ref()
    }
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    pub fn input_id(&self) -> Option<&AdvertisedId> {
        self.input_id.as_ref()
    }
    pub fn input_name(&self) -> Option<&str> {
        self.input_name.as_deref()
    }
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }
    pub fn streams(&self) -> &[ReceiverStream] {
        &self.streams
    }
    pub fn commands(&self) -> &[ReceiverCommand] {
        &self.commands
    }
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

struct Listener<S> {
    instance: Instance,
    destroy: Destroy,
    connected: Connected,
    url: Url,
    snapshot: Snapshot<S>,
    wait: WaitFn,
    subscribe: Subscribe,
    unsubscribe: Subscribe,
    events: Events,
    free: FreeEvents,
    _url: Option<CString>,
    _config: Option<CString>,
    _ndi: NDI,
    _thread: PhantomData<Rc<()>>,
}
impl<S> Drop for Listener<S> {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.instance) };
    }
}
impl<S> Listener<S> {
    fn server_url(&self) -> Result<Option<String>> {
        unsafe { Budget::new().string((self.url)(self.instance)) }
    }
    fn subscription(&mut self, id: &AdvertisedId, subscribe: bool) -> Result<()> {
        let id = CString::new(id.as_str())?;
        unsafe {
            if subscribe {
                (self.subscribe)(self.instance, id.as_ptr());
            } else {
                (self.unsubscribe)(self.instance, id.as_ptr());
            }
        };
        Ok(())
    }
    fn capture_events(&mut self, wait: Wait) -> Result<Vec<ListenerEvent>> {
        let mut count = 0;
        let events = unsafe { (self.events)(self.instance, &mut count, wait.millis()?) };
        struct Guard {
            instance: Instance,
            events: *const RawEvent,
            free: FreeEvents,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                if !self.events.is_null() {
                    unsafe { (self.free)(self.instance, self.events) };
                }
            }
        }
        let _guard = Guard {
            instance: self.instance,
            events,
            free: self.free,
        };
        let mut budget = Budget::new();
        let raw = unsafe { budget.slice(events, count)? };
        raw.iter()
            .map(|event| unsafe {
                Ok(ListenerEvent {
                    id: budget.string(event.uuid)?.map(AdvertisedId),
                    name: budget.string(event.name)?,
                    value: budget.string(event.value)?,
                })
            })
            .collect()
    }
}

/// A thread-affine sender listener. Use owned snapshots across thread boundaries.
pub struct SendListener {
    inner: Listener<RawSender>,
}
/// A thread-affine receiver listener. Remote control is always explicit.
pub struct RecvListener {
    inner: Listener<RawReceiver>,
    connect: Connect,
    latest: Vec<AdvertisedReceiver>,
}

macro_rules! create_listener {
    ($ndi:expr, $options:expr, $config:expr, $prefix:literal, $snapshot:literal, $wait:literal) => {{
        let url = $options.server.as_deref().map(CString::new).transpose()?;
        let config: Option<CString> = $config;
        unsafe {
            let create: New = require_optional(
                concat!($prefix, "create\0").as_bytes(),
                "Discovery Server listener",
            )?;
            let destroy = require_optional(
                concat!($prefix, "destroy\0").as_bytes(),
                "Discovery Server listener",
            )?;
            let connected = require_optional(
                concat!($prefix, "is_connected\0").as_bytes(),
                "Discovery Server listener",
            )?;
            let get_url = require_optional(
                concat!($prefix, "get_server_url\0").as_bytes(),
                "Discovery Server listener",
            )?;
            let snapshot = require_optional(
                concat!($prefix, $snapshot, "\0").as_bytes(),
                "Discovery Server listener",
            )?;
            let wait = require_optional(
                concat!($prefix, $wait, "\0").as_bytes(),
                "Discovery Server listener",
            )?;
            let subscribe = require_optional(
                concat!($prefix, "subscribe_events\0").as_bytes(),
                "Discovery Server listener",
            )?;
            let unsubscribe = require_optional(
                concat!($prefix, "unsubscribe_events\0").as_bytes(),
                "Discovery Server listener",
            )?;
            let events = require_optional(
                concat!($prefix, "get_events\0").as_bytes(),
                "Discovery Server listener",
            )?;
            let free = require_optional(
                concat!($prefix, "free_events\0").as_bytes(),
                "Discovery Server listener",
            )?;
            let raw = Create {
                url: url.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
            };
            #[cfg(feature = "advanced")]
            let instance = if let Some(config) = &config {
                type Configured = unsafe extern "C" fn(*const Create, *const c_char) -> Instance;
                let configured: Configured = crate::advanced::ffi::required(
                    $ndi,
                    concat!($prefix, "create_ex\0").as_bytes(),
                )?;
                configured(&raw, config.as_ptr())
            } else {
                create(&raw)
            };
            #[cfg(not(feature = "advanced"))]
            let instance = create(&raw);
            if instance.is_null() {
                return Err(Error::InitializationFailed(
                    "Discovery Server listener creation failed".into(),
                ));
            }
            Listener {
                instance,
                destroy,
                connected,
                url: get_url,
                snapshot,
                wait,
                subscribe,
                unsubscribe,
                events,
                free,
                _url: url,
                _config: config,
                _ndi: $ndi.clone(),
                _thread: PhantomData,
            }
        }
    }};
}
macro_rules! listener_methods {
    () => {
        /// Whether the native listener is connected to its configured server.
        pub fn is_connected(&self) -> bool {
            unsafe { (self.inner.connected)(self.inner.instance) }
        }
        /// Copy the configured server endpoint.
        pub fn server_url(&self) -> Result<Option<String>> {
            self.inner.server_url()
        }
        /// Wait for a membership change; zero polls immediately.
        pub fn wait_for_changes(&mut self, timeout: Duration) -> Result<bool> {
            Ok(unsafe { (self.inner.wait)(self.inner.instance, to_ms_checked(timeout)?) })
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
            inner: create_listener!(
                ndi,
                options,
                None,
                "NDIlib_send_listener_",
                "get_senders",
                "wait_for_senders"
            ),
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
            inner: create_listener!(
                ndi,
                options,
                config,
                "NDIlib_send_listener_",
                "get_senders",
                "wait_for_senders"
            ),
        })
    }
    listener_methods!();
    /// Copy the complete current sender snapshot before native storage expires.
    pub fn snapshot(&mut self) -> Result<Vec<AdvertisedSender>> {
        let mut count = 0;
        let values = unsafe { (self.inner.snapshot)(self.inner.instance, &mut count) };
        let mut budget = Budget::new();
        let raw = unsafe { budget.slice(values, count)? };
        raw.iter()
            .map(|s| unsafe {
                let groups = budget
                    .slice(s.groups, s.num_groups)?
                    .iter()
                    .map(|v| budget.string(*v))
                    .collect::<Result<_>>()?;
                Ok(AdvertisedSender {
                    id: budget.string(s.uuid)?.map(AdvertisedId),
                    name: budget.string(s.name)?,
                    metadata: budget.string(s.metadata)?,
                    address: budget.string(s.address)?,
                    port: s.port,
                    groups,
                    subscribed: s.subscribed,
                })
            })
            .collect()
    }
}
impl RecvListener {
    /// Create a receiver listener with explicit remote-control methods.
    pub fn new(ndi: &NDI, options: &ListenerOptions) -> Result<Self> {
        let connect = unsafe {
            require_optional(
                b"NDIlib_recv_listener_send_connect\0",
                "Discovery Server receiver control",
            )?
        };
        Ok(Self {
            inner: create_listener!(
                ndi,
                options,
                None,
                "NDIlib_recv_listener_",
                "get_receivers",
                "wait_for_receivers"
            ),
            connect,
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
        let connect = unsafe {
            require_optional(
                b"NDIlib_recv_listener_send_connect\0",
                "Discovery Server receiver control",
            )?
        };
        let config = Some(CString::new(config.expose_json())?);
        Ok(Self {
            inner: create_listener!(
                ndi,
                options,
                config,
                "NDIlib_recv_listener_",
                "get_receivers",
                "wait_for_receivers"
            ),
            connect,
            latest: Vec::new(),
        })
    }
    listener_methods!();
    /// Copy the complete receiver snapshot, retaining command capabilities for checks.
    pub fn snapshot(&mut self) -> Result<Vec<AdvertisedReceiver>> {
        let mut count = 0;
        let values = unsafe { (self.inner.snapshot)(self.inner.instance, &mut count) };
        let mut budget = Budget::new();
        let raw = unsafe { budget.slice(values, count)? };
        let copied: Result<Vec<_>> = raw
            .iter()
            .map(|s| unsafe {
                let streams = budget
                    .slice(s.streams, s.num_streams)?
                    .iter()
                    .copied()
                    .map(ReceiverStream::from)
                    .collect();
                let commands = budget
                    .slice(s.commands, s.num_commands)?
                    .iter()
                    .copied()
                    .map(ReceiverCommand::from)
                    .collect();
                Ok(AdvertisedReceiver {
                    id: budget.string(s.uuid)?.map(AdvertisedId),
                    name: budget.string(s.name)?,
                    input_id: budget.string(s.input_uuid)?.map(AdvertisedId),
                    input_name: budget.string(s.input_name)?,
                    address: budget.string(s.address)?,
                    streams,
                    commands,
                    subscribed: s.subscribed,
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
        if unsafe {
            (self.connect)(
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

#[cfg(all(test, ndi_has_discovery_header))]
#[path = "listener_abi_tests.rs"]
mod sdk_abi_tests;
