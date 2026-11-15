//! Registration of senders and receivers with a Discovery Server.

use super::{ServerApi, ServerObject};
use crate::{
    receiver::{ReceiverCore, ReceiverMode},
    sender::SenderResource,
    sys, Error, Ndi, Receiver, Rejected, Result, Sender,
};
use std::{
    collections::BTreeMap,
    ffi::CString,
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
    /// Inherit Discovery Server endpoints from the NDI configuration.
    #[must_use]
    pub fn clear_servers(mut self) -> Self {
        self.servers = None;
        self
    }

    /// Remove the per-instance Advanced configuration override.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn clear_config(mut self) -> Self {
        self.config = None;
        self
    }

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
    /// Remove the explicitly requested receiver input group.
    #[must_use]
    pub fn clear_input_group(mut self) -> Self {
        self.input_group = None;
        self
    }

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

/// Registration operations added to the shared Discovery Server lifecycle.
trait AdvertiserApi: ServerApi {
    type Member: Send;
    type Advertisement;
    const MEMBER: &'static str;
    const REGISTER: &'static str;
    fn same_member(a: &Self::Member, b: &Self::Member) -> bool;
    /// # Safety
    /// The advertiser and member are live; the member retains its native handle
    /// and all advertisement strings until removal or advertiser destruction.
    unsafe fn add(
        &self,
        instance: Self::Instance,
        member: &Self::Member,
        options: &Self::Advertisement,
    ) -> bool;
    /// # Safety
    /// The advertiser and member are live and belong to this runtime.
    unsafe fn remove(&self, instance: Self::Instance, member: &Self::Member) -> bool;
}

impl AdvertiserApi for sys::SenderAdvertising {
    type Member = Arc<SenderResource>;
    type Advertisement = SendAdvertisement;
    const MEMBER: &'static str = "sender";
    const REGISTER: &'static str = "register sender";
    fn same_member(a: &Self::Member, b: &Self::Member) -> bool {
        Arc::ptr_eq(a, b)
    }
    unsafe fn add(
        &self,
        instance: Self::Instance,
        member: &Self::Member,
        options: &Self::Advertisement,
    ) -> bool {
        // SAFETY: forwarded to the caller's live-owner obligation.
        unsafe {
            self.NDIlib_send_advertiser_add_sender(
                instance.0,
                member.instance(),
                options.monitoring,
            )
        }
    }
    unsafe fn remove(&self, instance: Self::Instance, member: &Self::Member) -> bool {
        // SAFETY: forwarded to the caller's live-owner obligation.
        unsafe { self.NDIlib_send_advertiser_del_sender(instance.0, member.instance()) }
    }
}

/// A registered receiver and the input-group name the SDK was given for it.
struct ReceiverMember {
    resource: Arc<ReceiverCore>,
    _input_group: Option<CString>,
}

impl AdvertiserApi for sys::ReceiverAdvertising {
    type Member = ReceiverMember;
    type Advertisement = RecvAdvertisement;
    const MEMBER: &'static str = "receiver";
    const REGISTER: &'static str = "register receiver";
    fn same_member(a: &Self::Member, b: &Self::Member) -> bool {
        Arc::ptr_eq(&a.resource, &b.resource)
    }
    unsafe fn add(
        &self,
        instance: Self::Instance,
        member: &Self::Member,
        options: &Self::Advertisement,
    ) -> bool {
        // SAFETY: forwarded to the caller's live-owner and retained-name obligation.
        unsafe {
            self.NDIlib_recv_advertiser_add_receiver(
                instance.0,
                member.resource.instance.0,
                options.controlling,
                options.monitoring,
                member
                    ._input_group
                    .as_ref()
                    .map_or(ptr::null(), |s| s.as_ptr()),
            )
        }
    }
    unsafe fn remove(&self, instance: Self::Instance, member: &Self::Member) -> bool {
        // SAFETY: forwarded to the caller's live-owner obligation.
        unsafe { self.NDIlib_recv_advertiser_del_receiver(instance.0, member.resource.instance.0) }
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
    server: ServerObject<A>,
    members: BTreeMap<u64, A::Member>,
}

const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Advertiser<sys::SenderAdvertising>>();
    assert_send::<Advertiser<sys::ReceiverAdvertising>>();
};

impl<A: AdvertiserApi> Advertiser<A>
where
    A: ServerApi<Options = AdvertiserOptions>,
{
    fn new(ndi: &Ndi, options: &AdvertiserOptions) -> Result<Self> {
        Ok(Self {
            server: ServerObject::new(ndi, options)?,
            members: BTreeMap::new(),
        })
    }
}

impl<A: AdvertiserApi> Advertiser<A> {
    fn register(&mut self, member: A::Member, options: &A::Advertisement) -> Result<u64> {
        if self
            .members
            .values()
            .any(|existing| A::same_member(existing, &member))
        {
            return Err(Error::InvalidInput(crate::InputDefect::AlreadyRegistered {
                member: A::MEMBER,
            }));
        }
        // SAFETY: the member owns its native object and advertisement strings;
        // successful registration retains that value until the SDK releases it.
        if !unsafe {
            self.server
                .api
                .add(self.server.instance(), &member, options)
        } {
            return Err(Error::NativeOperationFailed {
                operation: A::REGISTER,
            });
        }
        Ok(self.retain(member))
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
                Error::InvalidInput(crate::InputDefect::ForeignRegistration),
                registration,
            ));
        };
        // SAFETY: the advertiser is live for this borrow.
        if !unsafe { self.server.api.remove(self.server.instance(), member) } {
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

impl<A: AdvertiserApi> Drop for Advertiser<A> {
    fn drop(&mut self) {
        // End registrations, destroy their native advertiser, then release the
        // retained members. Explicit destruction makes this independent of fields.
        for member in self.members.values() {
            // SAFETY: the advertiser and every member remain live through this call.
            unsafe { self.server.api.remove(self.server.instance(), member) };
        }
        self.server.destroy();
        self.members.clear();
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
        self.inner
            .register(sender.native_owner(), &options)
            .map(SendRegistration)
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
        self.inner
            .register(
                ReceiverMember {
                    resource: Arc::clone(&receiver.core),
                    _input_group: input_group,
                },
                &options,
            )
            .map(RecvRegistration)
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
