//! Redirect source selection without receiving or forwarding media in Rust.

use crate::{
    sys::{self, bindings::*},
    to_ms_checked, Error, Result, Source, NDI,
};
use std::{ffi::CString, marker::PhantomData, ptr, rc::Rc, time::Duration};

/// A router's advertised name and optional discovery groups.
#[derive(Debug, Clone)]
pub struct RouterOptions {
    name: String,
    groups: Option<String>,
}
impl RouterOptions {
    /// Configure a named routing source. Creating the Router advertises it.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            groups: None,
        }
    }
    /// Select discovery groups; omission uses the SDK default.
    pub fn groups(mut self, groups: impl Into<String>) -> Self {
        self.groups = Some(groups.into());
        self
    }
    /// Configured source name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A thread-affine native routing source. Destruction removes its advertisement.
pub struct Router {
    instance: NDIlib_routing_instance_t,
    routing: &'static sys::Routing,
    _name: CString,
    _groups: Option<CString>,
    _config: Option<CString>,
    _ndi: NDI,
    _thread: PhantomData<Rc<()>>,
}
impl Router {
    /// Create and advertise a routing source after validating the complete API group.
    pub fn new(ndi: &NDI, options: &RouterOptions) -> Result<Self> {
        Self::create(ndi, options, None)
    }
    /// Create a routing source with retained Advanced JSON configuration.
    #[cfg(feature = "advanced")]
    pub fn new_with_config(
        ndi: &NDI,
        options: &RouterOptions,
        config: &crate::advanced::AdvancedConfig,
    ) -> Result<Self> {
        Self::create(ndi, options, Some(CString::new(config.expose_json())?))
    }
    fn create(ndi: &NDI, options: &RouterOptions, config: Option<CString>) -> Result<Self> {
        let routing = ndi.api().routing.require("source routing")?;
        let name = CString::new(options.name.as_str())?;
        let groups = options.groups.as_deref().map(CString::new).transpose()?;
        let raw = NDIlib_routing_create_t {
            p_ndi_name: name.as_ptr(),
            p_groups: groups.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
        };
        #[cfg(feature = "advanced")]
        let instance = if let Some(config) = &config {
            type Configured = unsafe extern "C" fn(
                *const NDIlib_routing_create_t,
                *const std::ffi::c_char,
            ) -> NDIlib_routing_instance_t;
            // SAFETY: `Configured` is the SDK's signature for this function.
            let configured: Configured =
                unsafe { crate::advanced::ffi::required(ndi, b"NDIlib_routing_create_v2\0")? };
            // SAFETY: `raw` and `config` borrow C strings that outlive the call.
            unsafe { configured(&raw, config.as_ptr()) }
        } else {
            // SAFETY: `raw` borrows C strings that outlive the call.
            unsafe { routing.NDIlib_routing_create(&raw) }
        };
        // SAFETY: `raw` borrows C strings that outlive the call.
        #[cfg(not(feature = "advanced"))]
        let instance = unsafe { routing.NDIlib_routing_create(&raw) };
        if instance.is_null() {
            return Err(Error::InitializationFailed(
                "routing source creation failed".into(),
            ));
        }
        Ok(Self {
            instance,
            routing,
            _name: name,
            _groups: groups,
            _config: config,
            _ndi: ndi.clone(),
            _thread: PhantomData,
        })
    }
    /// Select a source for connected receivers.
    pub fn change(&mut self, source: &Source) -> Result<()> {
        let raw = source.to_raw()?;
        // SAFETY: the router is live and `raw` owns the strings it points to.
        if unsafe { self.routing.NDIlib_routing_change(self.instance, &raw.raw) } {
            Ok(())
        } else {
            Err(Error::NativeOperationFailed {
                operation: "routing change",
            })
        }
    }
    /// Disconnect the current route.
    pub fn clear(&mut self) -> Result<()> {
        // SAFETY: the router instance is live for this borrow.
        if unsafe { self.routing.NDIlib_routing_clear(self.instance) } {
            Ok(())
        } else {
            Err(Error::NativeOperationFailed {
                operation: "routing clear",
            })
        }
    }
    /// Count receivers; a nonzero timeout waits for connections.
    pub fn connection_count(&self, timeout: Duration) -> Result<u32> {
        let timeout = to_ms_checked(timeout)?;
        // SAFETY: the router instance is live for this borrow.
        let count = unsafe {
            self.routing
                .NDIlib_routing_get_no_connections(self.instance, timeout)
        };
        u32::try_from(count).map_err(|_| Error::NativeOperationFailed {
            operation: "routing connection count",
        })
    }
    /// Copy the router's advertised source descriptor.
    pub fn source(&self) -> Result<Source> {
        // SAFETY: the router is live, and the descriptor the SDK returns stays
        // valid until the next call on it, after this copy completes.
        unsafe { Source::try_from_raw(self.routing.NDIlib_routing_get_source_name(self.instance)) }
    }
    /// Send metadata immediately and on each new downstream connection.
    #[cfg(feature = "advanced")]
    pub fn add_connection_metadata(&self, metadata: &crate::MetadataFrame) -> Result<()> {
        type Add = unsafe extern "C" fn(NDIlib_routing_instance_t, *const NDIlib_metadata_frame_t);
        let add: Add = unsafe {
            crate::advanced::ffi::required(&self._ndi, b"NDIlib_routing_add_connection_metadata\0")?
        };
        unsafe { add(self.instance, &metadata.to_raw()) };
        Ok(())
    }

    /// Clear the stored connection metadata.
    #[cfg(feature = "advanced")]
    pub fn clear_connection_metadata(&self) -> Result<()> {
        type ClearMetadata = unsafe extern "C" fn(NDIlib_routing_instance_t);
        let clear: ClearMetadata = unsafe {
            crate::advanced::ffi::required(
                &self._ndi,
                b"NDIlib_routing_clear_connection_metadata\0",
            )?
        };
        unsafe { clear(self.instance) };
        Ok(())
    }
}
impl Drop for Router {
    fn drop(&mut self) {
        // SAFETY: the instance was created by this table and is destroyed once.
        unsafe { self.routing.NDIlib_routing_destroy(self.instance) };
    }
}
