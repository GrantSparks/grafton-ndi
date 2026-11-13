//! Registration of senders and receivers with a Discovery Server.

use super::{not_created, Handle};
use crate::{
    receiver::{ReceiverCore, ReceiverMode},
    sender::SenderResource,
    sys::{self, bindings::*},
    Error, Ndi, Receiver, Rejected, Result, Sender,
};
use std::{
    collections::BTreeMap,
    ffi::{c_char, CString},
    fmt, ptr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

/// The Discovery Server an advertiser registers with.
///
/// Without explicit servers the SDK uses the Discovery Server from the NDI
/// configuration. If that names none either, the advertiser cannot be created.
#[derive(Debug, Clone, Default)]
pub struct AdvertiserOptions {
    servers: Option<String>,
    #[cfg(feature = "advanced")]
    config: Option<crate::advanced::AdvancedConfig>,
}
impl AdvertiserOptions {
    /// Requested server endpoints, or `None` to inherit the NDI configuration.
    #[must_use]
    pub fn configured_servers(&self) -> Option<&str> {
        self.servers.as_deref()
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
    /// Use explicit endpoints instead of the SDK configuration.
    ///
    /// Each endpoint is a host name or IP address, optionally followed by a
    /// colon and a port (5959 when omitted). Several endpoints separated by
    /// commas register with redundant servers.
    #[must_use]
    pub fn servers(mut self, servers: impl Into<String>) -> Self {
        self.servers = Some(servers.into());
        self
    }
    /// Create the advertiser with this per-instance Advanced SDK
    /// configuration, which it keeps for as long as it lives.
    ///
    /// [`SendAdvertiser::new`] and [`RecvAdvertiser::new`] then need an
    /// Advanced runtime, and a build with the Advanced SDK.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn config(mut self, config: crate::advanced::AdvancedConfig) -> Self {
        self.config = Some(config);
        self
    }
}
/// How a sender is advertised. Monitoring is disabled unless explicitly enabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct SendAdvertisement {
    monitoring: bool,
}
impl SendAdvertisement {
    /// Whether remote monitoring was requested.
    #[must_use]
    pub fn configured_monitoring(&self) -> bool {
        self.monitoring
    }

    /// The default advertisement: no remote monitoring.
    pub fn new() -> Self {
        Self::default()
    }
    /// Permit remote monitoring of this sender.
    #[must_use]
    pub fn allow_monitoring(mut self, enabled: bool) -> Self {
        self.monitoring = enabled;
        self
    }
}
/// How a receiver is advertised. Monitoring and remote source control are
/// disabled unless explicitly enabled.
#[derive(Debug, Clone, Default)]
pub struct RecvAdvertisement {
    monitoring: bool,
    controlling: bool,
    input_group: Option<String>,
}
impl RecvAdvertisement {
    /// Whether remote monitoring was requested.
    #[must_use]
    pub fn configured_monitoring(&self) -> bool {
        self.monitoring
    }

    /// Whether remote source control was requested.
    #[must_use]
    pub fn configured_controlling(&self) -> bool {
        self.controlling
    }

    /// Requested input group, if any.
    #[must_use]
    pub fn configured_input_group(&self) -> Option<&str> {
        self.input_group.as_deref()
    }

    /// The default advertisement: no remote monitoring or source control, and
    /// no input group.
    pub fn new() -> Self {
        Self::default()
    }
    /// Permit remote monitoring.
    #[must_use]
    pub fn allow_monitoring(mut self, enabled: bool) -> Self {
        self.monitoring = enabled;
        self
    }
    /// Permit Discovery Server clients to change this receiver's source.
    #[must_use]
    pub fn allow_controlling(mut self, enabled: bool) -> Self {
        self.controlling = enabled;
        self
    }
    /// Name the receiver's input group. Receivers that share a group name, such
    /// as the video and audio receivers of one input, are presented together.
    #[must_use]
    pub fn input_group(mut self, group: impl Into<String>) -> Self {
        self.input_group = Some(group.into());
        self
    }
}

/// The Advanced entry point that creates an advertiser of table `A` from its
/// settings and a JSON configuration, returning null when the SDK creates none.
#[cfg(feature = "advanced")]
type ConfiguredEntry<A> = unsafe extern "C" fn(
    *const <A as AdvertiserApi>::Settings,
    *const c_char,
) -> <A as AdvertiserApi>::Instance;

/// What the sender and receiver advertiser tables have in common.
trait AdvertiserApi: sys::Table + Sync {
    /// The native advertiser handle.
    type Instance: Handle;
    /// The native creation settings.
    type Settings;
    /// The native owner a registration keeps alive.
    type Member;
    /// The Advanced table of the entry point that also takes a configuration.
    #[cfg(feature = "advanced")]
    type Configured: sys::Table;
    /// That entry point's name.
    #[cfg(feature = "advanced")]
    const CONFIGURED: &'static str;

    /// The settings for an advertiser of the Discovery Server at `url`, or of
    /// the one the SDK is configured with when `url` is null.
    fn settings(url: *const c_char) -> Self::Settings;

    /// Create an advertiser. Returns null when the SDK creates none.
    ///
    /// # Safety
    ///
    /// The URL of `settings` must be null or a NUL-terminated string that
    /// outlives the call.
    unsafe fn create(&self, settings: &Self::Settings) -> Self::Instance;

    /// The entry point of `table` that also takes a configuration.
    #[cfg(feature = "advanced")]
    fn configured_entry(table: &'static Self::Configured) -> ConfiguredEntry<Self>;

    /// Destroy the advertiser.
    ///
    /// # Safety
    ///
    /// `instance` must be a live advertiser created through this table, and it
    /// must not be used again.
    unsafe fn destroy(&self, instance: Self::Instance);

    /// Stop advertising `member`. Returns false, having removed nothing, when
    /// the SDK did not know it.
    ///
    /// # Safety
    ///
    /// `instance` must be a live advertiser created through this table.
    unsafe fn remove(&self, instance: Self::Instance, member: &Self::Member) -> bool;
}

impl AdvertiserApi for sys::SenderAdvertising {
    type Instance = NDIlib_send_advertiser_instance_t;
    type Settings = NDIlib_send_advertiser_create_t;
    type Member = Arc<SenderResource>;
    #[cfg(feature = "advanced")]
    type Configured = sys::advanced::SenderAdvertiserConfiguration;
    #[cfg(feature = "advanced")]
    const CONFIGURED: &'static str = "NDIlib_send_advertiser_create_ex";

    fn settings(url: *const c_char) -> Self::Settings {
        NDIlib_send_advertiser_create_t { p_url_address: url }
    }
    unsafe fn create(&self, settings: &Self::Settings) -> Self::Instance {
        // SAFETY: forwarded to the caller's obligations.
        unsafe { self.NDIlib_send_advertiser_create(settings) }
    }
    #[cfg(feature = "advanced")]
    fn configured_entry(table: &'static Self::Configured) -> ConfiguredEntry<Self> {
        sys::advanced::native!(SenderAdvertiserConfiguration, table => table.NDIlib_send_advertiser_create_ex)
    }
    unsafe fn destroy(&self, instance: Self::Instance) {
        // SAFETY: forwarded to the caller's obligations.
        unsafe { self.NDIlib_send_advertiser_destroy(instance) }
    }
    unsafe fn remove(&self, instance: Self::Instance, member: &Self::Member) -> bool {
        // SAFETY: the caller supplies a live advertiser, and `member` owns a
        // live sender.
        unsafe { self.NDIlib_send_advertiser_del_sender(instance, member.instance()) }
    }
}

/// A registered receiver and the input-group name the SDK was given for it.
struct ReceiverMember {
    resource: Arc<ReceiverCore>,
    _input_group: Option<CString>,
}

impl AdvertiserApi for sys::ReceiverAdvertising {
    type Instance = NDIlib_recv_advertiser_instance_t;
    type Settings = NDIlib_recv_advertiser_create_t;
    type Member = ReceiverMember;
    #[cfg(feature = "advanced")]
    type Configured = sys::advanced::ReceiverAdvertiserConfiguration;
    #[cfg(feature = "advanced")]
    const CONFIGURED: &'static str = "NDIlib_recv_advertiser_create_ex";

    fn settings(url: *const c_char) -> Self::Settings {
        NDIlib_recv_advertiser_create_t { p_url_address: url }
    }
    unsafe fn create(&self, settings: &Self::Settings) -> Self::Instance {
        // SAFETY: forwarded to the caller's obligations.
        unsafe { self.NDIlib_recv_advertiser_create(settings) }
    }
    #[cfg(feature = "advanced")]
    fn configured_entry(table: &'static Self::Configured) -> ConfiguredEntry<Self> {
        sys::advanced::native!(ReceiverAdvertiserConfiguration, table => table.NDIlib_recv_advertiser_create_ex)
    }
    unsafe fn destroy(&self, instance: Self::Instance) {
        // SAFETY: forwarded to the caller's obligations.
        unsafe { self.NDIlib_recv_advertiser_destroy(instance) }
    }
    unsafe fn remove(&self, instance: Self::Instance, member: &Self::Member) -> bool {
        // SAFETY: the caller supplies a live advertiser, and `member` owns a
        // live receiver.
        unsafe { self.NDIlib_recv_advertiser_del_receiver(instance, member.resource.instance) }
    }
}

/// Source of registration identifiers. They are unique across advertisers, so
/// a registration can only ever name an entry of the advertiser that issued it.
static NEXT_REGISTRATION: AtomicU64 = AtomicU64::new(1);

/// A native advertiser and the objects registered with it.
///
/// The advertiser is the single owner of its registrations: each one keeps its
/// sender or receiver alive until it is removed or the advertiser is dropped.
struct Advertiser<A: AdvertiserApi> {
    instance: A::Instance,
    api: &'static A,
    members: BTreeMap<u64, A::Member>,
    _url: Option<CString>,
    #[cfg(feature = "advanced")]
    _config: Option<crate::advanced::AdvancedConfig>,
    _ndi: Ndi,
}

// SAFETY: the SDK attaches no advertiser to the thread that created it; its
// objects may be used and destroyed from any thread as long as calls on one
// object do not overlap. An advertiser has exactly one owner and is not `Sync`,
// so its calls cannot overlap. The registered objects it may destroy are `Send`,
// and everything else it holds is too.
unsafe impl<A: AdvertiserApi> Send for Advertiser<A> where A::Member: Send {}

impl<A: AdvertiserApi> Advertiser<A> {
    /// Connect to the Discovery Server `options` name, through the Advanced
    /// entry point when the options carry a configuration.
    fn new(ndi: &Ndi, options: &AdvertiserOptions) -> Result<Self> {
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
                        A::configured_entry(table)(settings, text)
                    },
                )
            };
        }
        // SAFETY: the SDK's advertiser creation returns null or a fresh
        // advertiser that nothing else owns, and the settings outlive the call.
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

    /// The one creation path of both advertisers, with and without a
    /// configuration: create the native advertiser through `create`, which
    /// is handed the table and the native settings for `options`, and keep
    /// `config` for as long as the advertiser lives.
    ///
    /// # Safety
    ///
    /// `create` returns null or a live advertiser of `ndi`'s runtime, made
    /// from the settings it was handed, that nothing else owns.
    unsafe fn create(
        ndi: &Ndi,
        options: &AdvertiserOptions,
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(&'static A, &A::Settings) -> A::Instance,
    ) -> Result<Self> {
        // An advertiser with a configuration gets here only once its own
        // capability, which requires this table's, has been allowed, so it
        // is never refused for this table.
        let api = ndi.table::<A>()?;
        let url = options.servers.as_deref().map(CString::new).transpose()?;
        let settings = A::settings(url.as_ref().map_or(ptr::null(), |v| v.as_ptr()));
        let instance = create(api, &settings);
        if instance.is_null() {
            return Err(not_created("advertiser", options.servers.as_deref()));
        }
        Ok(Self {
            instance,
            api,
            members: BTreeMap::new(),
            _url: url,
            #[cfg(feature = "advanced")]
            _config: config,
            _ndi: ndi.clone(),
        })
    }

    /// Keep a newly registered `member` alive and return its identifier.
    fn retain(&mut self, member: A::Member) -> u64 {
        let id = NEXT_REGISTRATION.fetch_add(1, Ordering::Relaxed);
        self.members.insert(id, member);
        id
    }

    /// End the registration `id`, which the token `registration` names, as
    /// the native `operation`.
    ///
    /// # Errors
    ///
    /// Hands `registration` back, changing nothing: with
    /// [`Error::InvalidInput`] when another advertiser issued it, since the
    /// token is the only way to end that registration, and with
    /// [`Error::NativeOperationFailed`] when the SDK removes nothing because
    /// it does not know the registered object.
    fn release<R>(
        &mut self,
        id: u64,
        registration: R,
        operation: &'static str,
    ) -> std::result::Result<(), Rejected<R>> {
        let Some(member) = self.members.get(&id) else {
            return Err(Rejected::new(
                Error::InvalidInput("the registration was issued by another advertiser".into()),
                registration,
            ));
        };
        // SAFETY: the advertiser is live for this borrow.
        if !unsafe { self.api.remove(self.instance, member) } {
            return Err(Rejected::new(
                Error::NativeOperationFailed { operation },
                registration,
            ));
        }
        // The SDK has forgotten the object, so it may be released.
        self.members.remove(&id);
        Ok(())
    }
}

#[cfg(feature = "advanced")]
impl<A: AdvertiserApi> crate::advanced::Configurable for Advertiser<A> {
    type Options = AdvertiserOptions;
    type Settings = A::Settings;
    type Instance = A::Instance;

    /// An advertiser the SDK does not create is reported by the server it
    /// was given, so the entry point's name is not needed.
    unsafe fn create_with(
        ndi: &Ndi,
        options: &AdvertiserOptions,
        _operation: &'static str,
        config: crate::advanced::AdvancedConfig,
        create: impl FnOnce(&A::Settings) -> A::Instance,
    ) -> Result<Self> {
        // SAFETY: forwarded to the caller's obligation.
        unsafe { Self::create(ndi, options, Some(config), |_, settings| create(settings)) }
    }
}

impl<A: AdvertiserApi> Drop for Advertiser<A> {
    fn drop(&mut self) {
        // The SDK's documented teardown order: stop advertising every object,
        // destroy the advertiser, and only then release the objects, which
        // happens when the fields drop after this body.
        for member in self.members.values() {
            // SAFETY: the advertiser is live until the call below.
            unsafe { self.api.remove(self.instance, member) };
        }
        // SAFETY: this is the advertiser's only owner, and it is not used again.
        unsafe { self.api.destroy(self.instance) };
    }
}

/// Advertises senders to a Discovery Server so they can be monitored.
///
/// This is separate from the discovery every sender already performs: a
/// registration here makes a sender visible to [`SendListener`]s, not to
/// finders or receivers.
///
/// # Connection
///
/// An advertiser connects in the background and keeps trying for as long as
/// it lives. It is created, and accepts registrations, whether or not its
/// server can be reached, and delivers them once the server answers, which
/// can take several seconds after the server appears. It has no way to report
/// whether it is connected. To find out, use a [`SendListener`] on the same
/// server: [`is_connected`](super::SendListener::is_connected) reports the
/// connection, and a [snapshot](super::SendListener::snapshot) shows whether
/// the registration arrived.
///
/// # Ownership
///
/// The advertiser owns its registrations. Registering a sender keeps the native
/// sender alive until the registration is [removed](Self::unregister) or the
/// advertiser is dropped, even if every [`Sender`] handle is dropped first.
/// Dropping the advertiser removes every registration, destroys the native
/// advertiser, and then releases the senders it was keeping alive.
///
/// # Threading
///
/// An advertiser is [`Send`] but not [`Sync`]: it may be moved to another
/// thread, and is used by one thread at a time. Every method takes it
/// exclusively, so `Sync` would give a shared reference nothing to do today.
/// It is withheld, as from the listeners, because the SDK does not say
/// whether calls on one advertiser may overlap, so that a shared query such
/// as the listeners' connection state can be added without that guarantee.
///
/// [`SendListener`]: super::SendListener
pub struct SendAdvertiser {
    inner: Advertiser<sys::SenderAdvertising>,
}
/// Advertises receivers to a Discovery Server so they can be monitored and,
/// when permitted, told which source to connect to.
///
/// # Connection
///
/// An advertiser connects in the background and keeps trying for as long as
/// it lives. It is created, and accepts registrations, whether or not its
/// server can be reached, and delivers them once the server answers, which
/// can take several seconds after the server appears. It has no way to report
/// whether it is connected. To find out, use a [`RecvListener`] on the same
/// server: [`is_connected`](super::RecvListener::is_connected) reports the
/// connection, and a [snapshot](super::RecvListener::snapshot) shows whether
/// the registration arrived.
///
/// # Ownership
///
/// The advertiser owns its registrations. Registering a receiver keeps the
/// native receiver alive until the registration is [removed](Self::unregister)
/// or the advertiser is dropped, even if every [`Receiver`] handle is dropped
/// first. Dropping the advertiser removes every registration, destroys the
/// native advertiser, and then releases the receivers it was keeping alive.
///
/// # Threading
///
/// An advertiser is [`Send`] but not [`Sync`]: it may be moved to another
/// thread, and is used by one thread at a time. Every method takes it
/// exclusively, so `Sync` would give a shared reference nothing to do today.
/// It is withheld, as from the listeners, because the SDK does not say
/// whether calls on one advertiser may overlap, so that a shared query such
/// as the listeners' connection state can be added without that guarantee.
///
/// [`RecvListener`]: super::RecvListener
pub struct RecvAdvertiser {
    inner: Advertiser<sys::ReceiverAdvertising>,
}

const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<SendAdvertiser>();
    assert_send::<RecvAdvertiser>();
};

/// Names one sender registration for [`SendAdvertiser::unregister`].
///
/// The registration belongs to the advertiser that issued it, and only that
/// advertiser can end it. Dropping this value changes nothing: the sender
/// stays advertised, and alive, until the registration is removed or the
/// advertiser is dropped.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct SendRegistration(u64);
/// Names one receiver registration for [`RecvAdvertiser::unregister`].
///
/// The registration belongs to the advertiser that issued it, and only that
/// advertiser can end it. Dropping this value changes nothing: the receiver
/// stays advertised, and alive, until the registration is removed or the
/// advertiser is dropped.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct RecvRegistration(u64);

impl fmt::Debug for SendAdvertiser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendAdvertiser")
            .field("registrations", &self.inner.members.len())
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for RecvAdvertiser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvAdvertiser")
            .field("registrations", &self.inner.members.len())
            .finish_non_exhaustive()
    }
}

impl SendAdvertiser {
    /// Create an advertiser for the Discovery Server `options` name, without
    /// registering any sender.
    ///
    /// Success does not mean the server was reached; see
    /// [Connection](Self#connection).
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] when the selected runtime
    /// cannot provide [sender advertising], which needs NDI 6.4.0 or later, or
    /// the options carry an Advanced configuration that the runtime or this
    /// build cannot take; [`Error::InvalidCString`] when the server list
    /// contains a NUL byte; and [`Error::InvalidInput`] when the SDK creates
    /// no advertiser. It creates none for an address it cannot parse, and none
    /// when `options` name no server and the NDI configuration supplies none.
    ///
    /// [sender advertising]: crate::capabilities::RuntimeFeature::SenderAdvertising
    pub fn new(ndi: &Ndi, options: &AdvertiserOptions) -> Result<Self> {
        Ok(Self {
            inner: Advertiser::new(ndi, options)?,
        })
    }
    /// Advertise `sender`, keeping the native sender alive for as long as the
    /// registration exists.
    ///
    /// The sender is not borrowed: it stays fully usable, and may even be
    /// dropped, while it is registered.
    ///
    /// Only an ordinary sender, in the [`RawSend`](crate::RawSend) mode, can
    /// be registered. The Advanced `CompressedSender` and `QueuedSender` lend
    /// their sender in another [`SenderMode`](crate::SenderMode), which this
    /// method does not take: they free the frames or completion state the SDK
    /// uses as soon as their native sender is destroyed, and a registration
    /// would keep it alive past that.
    ///
    /// Registering a sender within a few tens of milliseconds of removing it
    /// from another advertiser on the same server can crash a listener on
    /// that server, through an SDK defect of NDI 6.3.2 and 6.4.0.2. Wait a
    /// second, or drop the other advertiser first. See the
    /// [module documentation](super#sdk-defects-that-remain-in-64).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when this advertiser already
    /// advertises the sender, and [`Error::NativeOperationFailed`] when the SDK
    /// refuses the registration.
    pub fn register(
        &mut self,
        sender: &Sender,
        options: SendAdvertisement,
    ) -> Result<SendRegistration> {
        let owner = sender.native_owner();
        if self
            .inner
            .members
            .values()
            .any(|member| Arc::ptr_eq(member, &owner))
        {
            return Err(Error::InvalidInput("sender is already registered".into()));
        }
        // SAFETY: the advertiser is live for this borrow and `owner` keeps the
        // sender alive for as long as the registration exists.
        if !unsafe {
            self.inner.api.NDIlib_send_advertiser_add_sender(
                self.inner.instance,
                owner.instance(),
                options.monitoring,
            )
        } {
            return Err(Error::NativeOperationFailed {
                operation: "register sender",
            });
        }
        Ok(SendRegistration(self.inner.retain(owner)))
    }
    /// Stop advertising a sender and release this advertiser's hold on it.
    ///
    /// # Errors
    ///
    /// A failure changes nothing and hands the registration back inside
    /// [`Rejected`]. Only the advertiser that issued a registration can end
    /// it: another one fails with [`Error::InvalidInput`], and the
    /// registration can still be given to the advertiser it belongs to. The
    /// call fails with [`Error::NativeOperationFailed`] when the SDK removes
    /// nothing because it does not know the sender; the registration then
    /// stays in effect, and keeps the sender alive, until it is removed or
    /// the advertiser is dropped.
    pub fn unregister(
        &mut self,
        registration: SendRegistration,
    ) -> std::result::Result<(), Rejected<SendRegistration>> {
        self.inner
            .release(registration.0, registration, "unregister sender")
    }
}
impl RecvAdvertiser {
    /// Create an advertiser for the Discovery Server `options` name, without
    /// registering any receiver.
    ///
    /// Success does not mean the server was reached; see
    /// [Connection](Self#connection).
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] when the selected runtime
    /// cannot provide [receiver advertising], which needs NDI 6.4.0 or later,
    /// or the options carry an Advanced configuration that the runtime or this
    /// build cannot take; [`Error::InvalidCString`] when the server list
    /// contains a NUL byte; and [`Error::InvalidInput`] when the SDK creates
    /// no advertiser. It creates none for an address it cannot parse, and none
    /// when `options` name no server and the NDI configuration supplies none.
    ///
    /// [receiver advertising]: crate::capabilities::RuntimeFeature::ReceiverAdvertising
    pub fn new(ndi: &Ndi, options: &AdvertiserOptions) -> Result<Self> {
        Ok(Self {
            inner: Advertiser::new(ndi, options)?,
        })
    }
    /// Advertise `receiver`, in any mode, keeping the native receiver alive for
    /// as long as the registration exists.
    ///
    /// The receiver is not borrowed: it stays fully usable, and may even be
    /// dropped, while it is registered.
    ///
    /// Two SDK defects of NDI 6.3.2 and 6.4.0.2 concern a registered receiver.
    /// Registering it within a few tens of milliseconds of removing it from
    /// another advertiser on the same server can crash a listener on that
    /// server; wait a second, or drop the other advertiser first. And a
    /// listener anywhere that unsubscribes from it while it connects can block
    /// that connect for good. See the
    /// [module documentation](super#sdk-defects-that-remain-in-64).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] when the input-group name contains a
    /// NUL byte, [`Error::InvalidInput`] when this advertiser already
    /// advertises the receiver, and [`Error::NativeOperationFailed`] when the
    /// SDK refuses the registration.
    pub fn register<M: ReceiverMode>(
        &mut self,
        receiver: &Receiver<M>,
        options: RecvAdvertisement,
    ) -> Result<RecvRegistration> {
        let input_group = options
            .input_group
            .as_deref()
            .map(CString::new)
            .transpose()?;
        let resource = Arc::clone(&receiver.core);
        if self
            .inner
            .members
            .values()
            .any(|member| Arc::ptr_eq(&member.resource, &resource))
        {
            return Err(Error::InvalidInput("receiver is already registered".into()));
        }
        // SAFETY: the advertiser is live for this borrow, `resource` keeps the
        // receiver alive for as long as the registration exists, and the
        // registration retains `input_group` for the same time.
        if !unsafe {
            self.inner.api.NDIlib_recv_advertiser_add_receiver(
                self.inner.instance,
                resource.instance,
                options.controlling,
                options.monitoring,
                input_group.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
            )
        } {
            return Err(Error::NativeOperationFailed {
                operation: "register receiver",
            });
        }
        Ok(RecvRegistration(self.inner.retain(ReceiverMember {
            resource,
            _input_group: input_group,
        })))
    }
    /// Stop advertising a receiver and release this advertiser's hold on it.
    ///
    /// # Errors
    ///
    /// A failure changes nothing and hands the registration back inside
    /// [`Rejected`]. Only the advertiser that issued a registration can end
    /// it: another one fails with [`Error::InvalidInput`], and the
    /// registration can still be given to the advertiser it belongs to. The
    /// call fails with [`Error::NativeOperationFailed`] when the SDK removes
    /// nothing because it does not know the receiver; the registration then
    /// stays in effect, and keeps the receiver alive, until it is removed or
    /// the advertiser is dropped.
    pub fn unregister(
        &mut self,
        registration: RecvRegistration,
    ) -> std::result::Result<(), Rejected<RecvRegistration>> {
        self.inner
            .release(registration.0, registration, "unregister receiver")
    }
}
