use crate::{
    dispatch::require_optional,
    receiver::{ReceiverCore, ReceiverMode},
    sender::SenderResource,
    Error, Receiver, Result, Sender, NDI,
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::{c_char, c_void, CString},
    ptr,
    rc::Rc,
    sync::Arc,
};

type Instance = *mut c_void;
#[repr(C)]
struct Create {
    url: *const c_char,
}
type New = unsafe extern "C" fn(*const Create) -> Instance;
type Destroy = unsafe extern "C" fn(Instance);
type SendAdd = unsafe extern "C" fn(Instance, crate::ndi_lib::NDIlib_send_instance_t, bool) -> bool;
type RecvAdd = unsafe extern "C" fn(
    Instance,
    crate::ndi_lib::NDIlib_recv_instance_t,
    bool,
    bool,
    *const c_char,
) -> bool;
type SendDel = unsafe extern "C" fn(Instance, crate::ndi_lib::NDIlib_send_instance_t) -> bool;
type RecvDel = unsafe extern "C" fn(Instance, crate::ndi_lib::NDIlib_recv_instance_t) -> bool;

/// Discovery Server endpoints. Comma-separated redundancy lists are accepted.
#[derive(Debug, Clone, Default)]
pub struct AdvertiserOptions {
    servers: Option<String>,
}
impl AdvertiserOptions {
    /// Use explicit endpoints instead of the SDK configuration.
    pub fn servers(mut self, servers: impl Into<String>) -> Self {
        self.servers = Some(servers.into());
        self
    }
}
/// Sender monitoring is disabled unless explicitly enabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct SendAdvertisement {
    monitoring: bool,
}
impl SendAdvertisement {
    /// Permit remote monitoring of this sender.
    pub fn allow_monitoring(mut self, enabled: bool) -> Self {
        self.monitoring = enabled;
        self
    }
}
/// Receiver monitoring and remote source control default to disabled.
#[derive(Debug, Clone, Default)]
pub struct RecvAdvertisement {
    monitoring: bool,
    controlling: bool,
    input_group: Option<String>,
}
impl RecvAdvertisement {
    /// Permit remote monitoring.
    pub fn allow_monitoring(mut self, enabled: bool) -> Self {
        self.monitoring = enabled;
        self
    }
    /// Permit Discovery Server clients to change this receiver's source.
    pub fn allow_controlling(mut self, enabled: bool) -> Self {
        self.controlling = enabled;
        self
    }
    /// Name the receiver's input group.
    pub fn input_group(mut self, group: impl Into<String>) -> Self {
        self.input_group = Some(group.into());
        self
    }
}

struct SendCore {
    instance: Instance,
    destroy: Destroy,
    add: SendAdd,
    del: SendDel,
    entries: BTreeMap<u64, Arc<SenderResource>>,
    next: u64,
    _url: Option<CString>,
    _config: Option<CString>,
    _ndi: NDI,
}
struct RecvEntry {
    resource: Arc<ReceiverCore>,
    _input: Option<CString>,
}
struct RecvCore {
    instance: Instance,
    destroy: Destroy,
    add: RecvAdd,
    del: RecvDel,
    entries: BTreeMap<u64, RecvEntry>,
    next: u64,
    _url: Option<CString>,
    _config: Option<CString>,
    _ndi: NDI,
}
impl SendCore {
    fn remove(&mut self, id: u64) -> bool {
        if let Some(owner) = self.entries.remove(&id) {
            // False means already absent, not an unconfirmed removal.
            unsafe { (self.del)(self.instance, owner.instance) }
        } else {
            false
        }
    }
}
impl RecvCore {
    fn remove(&mut self, id: u64) -> bool {
        if let Some(owner) = self.entries.remove(&id) {
            unsafe { (self.del)(self.instance, owner.resource.instance) }
        } else {
            false
        }
    }
}
impl Drop for SendCore {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.instance) };
    }
}
impl Drop for RecvCore {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.instance) };
    }
}

/// A thread-affine sender monitor. Registrations retain native sender ownership.
pub struct SendAdvertiser {
    core: Rc<RefCell<SendCore>>,
}
/// A thread-affine receiver monitor/controller.
pub struct RecvAdvertiser {
    core: Rc<RefCell<RecvCore>>,
}
/// One registration; dropping removes it. Forgetting it may leak but cannot dangle.
/// A registration intentionally keeps its sender alive after the Sender is dropped.
pub struct SendRegistration {
    core: Rc<RefCell<SendCore>>,
    id: Option<u64>,
}
/// One receiver registration, retaining the receiver even after its facade is dropped.
pub struct RecvRegistration {
    core: Rc<RefCell<RecvCore>>,
    id: Option<u64>,
}
impl SendRegistration {
    /// Remove this registration; false means it was already absent in the SDK.
    pub fn unregister(mut self) -> Result<bool> {
        Ok(self
            .core
            .borrow_mut()
            .remove(self.id.take().expect("live registration")))
    }
}
impl RecvRegistration {
    /// Remove this registration; false means it was already absent in the SDK.
    pub fn unregister(mut self) -> Result<bool> {
        Ok(self
            .core
            .borrow_mut()
            .remove(self.id.take().expect("live registration")))
    }
}
impl Drop for SendRegistration {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.core.borrow_mut().remove(id);
        }
    }
}
impl Drop for RecvRegistration {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.core.borrow_mut().remove(id);
        }
    }
}

impl SendAdvertiser {
    /// Connect to the configured Discovery Server without registering sources.
    pub fn new(ndi: &NDI, options: &AdvertiserOptions) -> Result<Self> {
        Self::create(ndi, options, None)
    }
    /// Create with Advanced configuration retained through native destruction.
    #[cfg(feature = "advanced")]
    pub fn new_with_config(
        ndi: &NDI,
        options: &AdvertiserOptions,
        config: &crate::advanced::AdvancedConfig,
    ) -> Result<Self> {
        Self::create(ndi, options, Some(CString::new(config.expose_json())?))
    }
    fn create(ndi: &NDI, options: &AdvertiserOptions, config: Option<CString>) -> Result<Self> {
        let url = options.servers.as_deref().map(CString::new).transpose()?;
        unsafe {
            let create: New =
                require_optional(b"NDIlib_send_advertiser_create\0", "sender advertising")?;
            let destroy =
                require_optional(b"NDIlib_send_advertiser_destroy\0", "sender advertising")?;
            let add =
                require_optional(b"NDIlib_send_advertiser_add_sender\0", "sender advertising")?;
            let del =
                require_optional(b"NDIlib_send_advertiser_del_sender\0", "sender advertising")?;
            let raw = Create {
                url: url.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
            };
            #[cfg(feature = "advanced")]
            let instance = if let Some(config) = &config {
                type Configured = unsafe extern "C" fn(*const Create, *const c_char) -> Instance;
                let configured: Configured =
                    crate::advanced::ffi::required(ndi, b"NDIlib_send_advertiser_create_ex\0")?;
                configured(&raw, config.as_ptr())
            } else {
                create(&raw)
            };
            #[cfg(not(feature = "advanced"))]
            let instance = create(&raw);
            if instance.is_null() {
                return Err(Error::InitializationFailed(
                    "sender advertiser creation failed".into(),
                ));
            }
            Ok(Self {
                core: Rc::new(RefCell::new(SendCore {
                    instance,
                    destroy,
                    add,
                    del,
                    entries: BTreeMap::new(),
                    next: 1,
                    _url: url,
                    _config: config,
                    _ndi: ndi.clone(),
                })),
            })
        }
    }
    /// Register without borrowing the Sender for the registration lifetime.
    /// Owned async sending remains available while monitoring is registered.
    pub fn register(
        &self,
        sender: &Sender,
        options: SendAdvertisement,
    ) -> Result<SendRegistration> {
        let mut core = self.core.borrow_mut();
        let owner = sender.native_owner();
        if core
            .entries
            .values()
            .any(|entry| Arc::ptr_eq(entry, &owner))
        {
            return Err(Error::InvalidConfiguration(
                "sender is already registered".into(),
            ));
        }
        let id = core.next;
        core.next = id.checked_add(1).ok_or_else(|| {
            Error::InvalidConfiguration("advertisement identifiers exhausted".into())
        })?;
        if !unsafe { (core.add)(core.instance, owner.instance, options.monitoring) } {
            return Err(Error::NativeOperationFailed {
                operation: "register sender",
            });
        }
        core.entries.insert(id, owner);
        Ok(SendRegistration {
            core: Rc::clone(&self.core),
            id: Some(id),
        })
    }
}
impl RecvAdvertiser {
    /// Connect to the Discovery Server without enabling monitoring or control.
    pub fn new(ndi: &NDI, options: &AdvertiserOptions) -> Result<Self> {
        Self::create(ndi, options, None)
    }
    /// Create with Advanced configuration retained through native destruction.
    #[cfg(feature = "advanced")]
    pub fn new_with_config(
        ndi: &NDI,
        options: &AdvertiserOptions,
        config: &crate::advanced::AdvancedConfig,
    ) -> Result<Self> {
        Self::create(ndi, options, Some(CString::new(config.expose_json())?))
    }
    fn create(ndi: &NDI, options: &AdvertiserOptions, config: Option<CString>) -> Result<Self> {
        let url = options.servers.as_deref().map(CString::new).transpose()?;
        unsafe {
            let create: New =
                require_optional(b"NDIlib_recv_advertiser_create\0", "receiver advertising")?;
            let destroy =
                require_optional(b"NDIlib_recv_advertiser_destroy\0", "receiver advertising")?;
            let add = require_optional(
                b"NDIlib_recv_advertiser_add_receiver\0",
                "receiver advertising",
            )?;
            let del = require_optional(
                b"NDIlib_recv_advertiser_del_receiver\0",
                "receiver advertising",
            )?;
            let raw = Create {
                url: url.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
            };
            #[cfg(feature = "advanced")]
            let instance = if let Some(config) = &config {
                type Configured = unsafe extern "C" fn(*const Create, *const c_char) -> Instance;
                let configured: Configured =
                    crate::advanced::ffi::required(ndi, b"NDIlib_recv_advertiser_create_ex\0")?;
                configured(&raw, config.as_ptr())
            } else {
                create(&raw)
            };
            #[cfg(not(feature = "advanced"))]
            let instance = create(&raw);
            if instance.is_null() {
                return Err(Error::InitializationFailed(
                    "receiver advertiser creation failed".into(),
                ));
            }
            Ok(Self {
                core: Rc::new(RefCell::new(RecvCore {
                    instance,
                    destroy,
                    add,
                    del,
                    entries: BTreeMap::new(),
                    next: 1,
                    _url: url,
                    _config: config,
                    _ndi: ndi.clone(),
                })),
            })
        }
    }
    /// Register a receiver in any mode. This exposes no raw capture capability.
    pub fn register<M: ReceiverMode>(
        &self,
        receiver: &Receiver<M>,
        options: RecvAdvertisement,
    ) -> Result<RecvRegistration> {
        let input = options
            .input_group
            .as_deref()
            .map(CString::new)
            .transpose()?;
        let mut core = self.core.borrow_mut();
        let resource = Arc::clone(&receiver.core);
        if core
            .entries
            .values()
            .any(|entry| Arc::ptr_eq(&entry.resource, &resource))
        {
            return Err(Error::InvalidConfiguration(
                "receiver is already registered".into(),
            ));
        }
        let id = core.next;
        core.next = id.checked_add(1).ok_or_else(|| {
            Error::InvalidConfiguration("advertisement identifiers exhausted".into())
        })?;
        if !unsafe {
            (core.add)(
                core.instance,
                resource.instance,
                options.controlling,
                options.monitoring,
                input.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
            )
        } {
            return Err(Error::NativeOperationFailed {
                operation: "register receiver",
            });
        }
        core.entries.insert(
            id,
            RecvEntry {
                resource,
                _input: input,
            },
        );
        Ok(RecvRegistration {
            core: Rc::clone(&self.core),
            id: Some(id),
        })
    }
}

#[cfg(all(test, ndi_has_discovery_header))]
#[path = "advertiser_abi_tests.rs"]
mod sdk_abi_tests;
