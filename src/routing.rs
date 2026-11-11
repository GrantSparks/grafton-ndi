//! Redirect source selection without receiving or forwarding media in Rust.

use crate::{
    dispatch::require_optional, ndi_lib::NDIlib_source_t, to_ms_checked, Error, Result, Source, NDI,
};
use std::{
    ffi::{c_void, CString},
    marker::PhantomData,
    ptr,
    rc::Rc,
    time::Duration,
};

type Instance = *mut c_void;
#[repr(C)]
struct Create {
    name: *const std::ffi::c_char,
    groups: *const std::ffi::c_char,
}
type New = unsafe extern "C" fn(*const Create) -> Instance;
type Destroy = unsafe extern "C" fn(Instance);
type Change = unsafe extern "C" fn(Instance, *const NDIlib_source_t) -> bool;
type Clear = unsafe extern "C" fn(Instance) -> bool;
type Count = unsafe extern "C" fn(Instance, u32) -> i32;
type GetSource = unsafe extern "C" fn(Instance) -> *const NDIlib_source_t;

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
    instance: Instance,
    destroy: Destroy,
    change: Change,
    clear: Clear,
    count: Count,
    get_source: GetSource,
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
        let name = CString::new(options.name.as_str())?;
        let groups = options.groups.as_deref().map(CString::new).transpose()?;
        // SAFETY: These reviewed signatures match Processing.NDI.Routing.h; the
        // process-selected module is pinned and the full group resolves first.
        unsafe {
            let create: New = require_optional(b"NDIlib_routing_create\0", "routing")?;
            let destroy = require_optional(b"NDIlib_routing_destroy\0", "routing")?;
            let change = require_optional(b"NDIlib_routing_change\0", "routing")?;
            let clear = require_optional(b"NDIlib_routing_clear\0", "routing")?;
            let count = require_optional(b"NDIlib_routing_get_no_connections\0", "routing")?;
            let get_source = require_optional(b"NDIlib_routing_get_source_name\0", "routing")?;
            let raw = Create {
                name: name.as_ptr(),
                groups: groups.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
            };
            #[cfg(feature = "advanced")]
            let instance = if let Some(config) = &config {
                type Configured =
                    unsafe extern "C" fn(*const Create, *const std::ffi::c_char) -> Instance;
                let configured: Configured =
                    crate::advanced::ffi::required(ndi, b"NDIlib_routing_create_v2\0")?;
                configured(&raw, config.as_ptr())
            } else {
                create(&raw)
            };
            #[cfg(not(feature = "advanced"))]
            let instance = create(&raw);
            if instance.is_null() {
                return Err(Error::InitializationFailed(
                    "routing source creation failed".into(),
                ));
            }
            Ok(Self {
                instance,
                destroy,
                change,
                clear,
                count,
                get_source,
                _name: name,
                _groups: groups,
                _config: config,
                _ndi: ndi.clone(),
                _thread: PhantomData,
            })
        }
    }
    /// Select a source for connected receivers.
    pub fn change(&mut self, source: &Source) -> Result<()> {
        let raw = source.to_raw()?;
        if unsafe { (self.change)(self.instance, &raw.raw) } {
            Ok(())
        } else {
            Err(Error::NativeOperationFailed {
                operation: "routing change",
            })
        }
    }
    /// Disconnect the current route.
    pub fn clear(&mut self) -> Result<()> {
        if unsafe { (self.clear)(self.instance) } {
            Ok(())
        } else {
            Err(Error::NativeOperationFailed {
                operation: "routing clear",
            })
        }
    }
    /// Count receivers; a nonzero timeout waits for connections.
    pub fn connection_count(&self, timeout: Duration) -> Result<u32> {
        let count = unsafe { (self.count)(self.instance, to_ms_checked(timeout)?) };
        u32::try_from(count).map_err(|_| Error::NativeOperationFailed {
            operation: "routing connection count",
        })
    }
    /// Copy the router's advertised source descriptor.
    pub fn source(&self) -> Result<Source> {
        unsafe { Source::try_from_raw((self.get_source)(self.instance)) }
    }
    /// Send metadata immediately and on each new downstream connection.
    #[cfg(feature = "advanced")]
    pub fn add_connection_metadata(&self, metadata: &crate::MetadataFrame) -> Result<()> {
        type Add = unsafe extern "C" fn(Instance, *const crate::ndi_lib::NDIlib_metadata_frame_t);
        let add: Add = unsafe {
            crate::advanced::ffi::required(&self._ndi, b"NDIlib_routing_add_connection_metadata\0")?
        };
        unsafe { add(self.instance, &metadata.to_raw()) };
        Ok(())
    }

    /// Clear the stored connection metadata.
    #[cfg(feature = "advanced")]
    pub fn clear_connection_metadata(&self) -> Result<()> {
        type ClearMetadata = unsafe extern "C" fn(Instance);
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
        unsafe { (self.destroy)(self.instance) };
    }
}
