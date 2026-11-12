//! Redirect source selection without receiving or forwarding media in Rust.
//!
//! A [`Router`] is an NDI source that carries no media of its own. Receivers
//! connect to it by name and are redirected to whichever source it currently
//! routes to, so one process can switch what many receivers see without
//! touching a frame.
//!
//! ```no_run
//! use grafton_ndi::{routing::{Router, RouterOptions}, Finder, FinderOptions, Ndi};
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! let ndi = Ndi::new()?;
//! let mut router = Router::new(&ndi, &RouterOptions::new("Program"))?;
//!
//! let finder = Finder::new(&ndi, &FinderOptions::default())?;
//! finder.wait_for_sources(Duration::from_secs(2))?;
//! if let Some(camera) = finder.current_sources()?.first() {
//!     router.change(camera)?;
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

use crate::{
    sys::{self, bindings::*},
    to_ms_checked, Error, Ndi, Result, Source,
};
use std::{ffi::CString, fmt, ptr, time::Duration};

/// A router's advertised name and optional discovery groups.
#[derive(Debug, Clone)]
pub struct RouterOptions {
    name: String,
    groups: Option<String>,
    #[cfg(feature = "advanced")]
    config: Option<crate::advanced::AdvancedConfig>,
}
impl RouterOptions {
    /// Configure a named routing source. Creating the Router advertises it.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            groups: None,
            #[cfg(feature = "advanced")]
            config: None,
        }
    }
    /// Set the comma-separated discovery groups the router is advertised in,
    /// instead of those of the NDI configuration.
    #[must_use]
    pub fn groups(mut self, groups: impl Into<String>) -> Self {
        self.groups = Some(groups.into());
        self
    }
    /// Create the router with this per-instance Advanced SDK configuration,
    /// which it keeps for as long as it lives.
    ///
    /// [`Router::new`] then needs an Advanced runtime, and a build with the
    /// Advanced SDK.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn config(mut self, config: crate::advanced::AdvancedConfig) -> Self {
        self.config = Some(config);
        self
    }
}

/// A native routing source. Dropping it removes its advertisement.
///
/// # Threading
///
/// A router is [`Send`] but not [`Sync`]: it may be moved to another thread,
/// and is used by one thread at a time.
pub struct Router {
    instance: NDIlib_routing_instance_t,
    routing: &'static sys::Routing,
    _name: CString,
    _groups: Option<CString>,
    #[cfg(feature = "advanced")]
    _config: Option<crate::advanced::AdvancedConfig>,
    _ndi: Ndi,
}

// SAFETY: the SDK attaches no routing instance to the thread that created it;
// its objects may be used and destroyed from any thread as long as calls on one
// object do not overlap. A router has exactly one owner and is not `Sync`, so
// its calls cannot overlap, and everything else it holds is `Send`.
unsafe impl Send for Router {}

const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Router>();
};

impl fmt::Debug for Router {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Router")
            .field("name", &self._name)
            .finish_non_exhaustive()
    }
}

impl Router {
    /// Create and advertise a routing source that routes to nothing yet.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] when the selected runtime has no
    /// [routing support](crate::capabilities::RuntimeFeature::Routing), or the
    /// options carry an Advanced configuration that the runtime or this build
    /// cannot take; [`Error::InvalidCString`] when the name or groups contain a
    /// NUL byte; and [`Error::NativeOperationFailed`] when the SDK creates no
    /// source.
    pub fn new(ndi: &Ndi, options: &RouterOptions) -> Result<Self> {
        #[cfg(feature = "advanced")]
        if let Some(config) = &options.config {
            return Self::new_configured(ndi, options, config);
        }
        // SAFETY: the SDK's router creation returns null or a fresh router
        // that nothing else owns, and the settings outlive the call.
        unsafe {
            Self::create(
                ndi,
                options,
                "NDIlib_routing_create",
                #[cfg(feature = "advanced")]
                None,
                |routing, settings| routing.NDIlib_routing_create(settings),
            )
        }
    }

    /// The one creation path: create a router through `create`, the entry
    /// point `operation`, which is handed the routing table and the native
    /// settings for `options`, and keep `config` for as long as the router
    /// lives.
    ///
    /// # Safety
    ///
    /// `create` returns null or a live router of `ndi`'s runtime, made from
    /// the settings it was handed, that nothing else owns.
    pub(crate) unsafe fn create(
        ndi: &Ndi,
        options: &RouterOptions,
        operation: &'static str,
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(
            &'static sys::Routing,
            &NDIlib_routing_create_t,
        ) -> NDIlib_routing_instance_t,
    ) -> Result<Self> {
        let routing = ndi.table::<sys::Routing>()?;
        let name = CString::new(options.name.as_str())?;
        let groups = options.groups.as_deref().map(CString::new).transpose()?;
        let instance = create(
            routing,
            &NDIlib_routing_create_t {
                p_ndi_name: name.as_ptr(),
                p_groups: groups.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
            },
        );
        if instance.is_null() {
            return Err(Error::NativeOperationFailed { operation });
        }
        Ok(Self {
            instance,
            routing,
            _name: name,
            _groups: groups,
            #[cfg(feature = "advanced")]
            _config: config,
            _ndi: ndi.clone(),
        })
    }

    /// The native router, for calls through an Advanced table.
    #[cfg(feature = "advanced")]
    pub(crate) fn native_instance(&self) -> NDIlib_routing_instance_t {
        self.instance
    }

    /// The runtime this router was created through.
    #[cfg(feature = "advanced")]
    pub(crate) fn runtime(&self) -> &Ndi {
        &self._ndi
    }

    /// Route connected and future receivers to `source`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] when the source name or address
    /// contains a NUL byte, and [`Error::NativeOperationFailed`] when the SDK
    /// rejects the change.
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
    /// Stop routing to any source. Receivers stay connected to the router.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NativeOperationFailed`] when the SDK rejects the change.
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
    /// Count the receivers connected to this router.
    ///
    /// A zero `timeout` reads the current count; a longer one waits up to that
    /// long for the first connection.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when `timeout` exceeds
    /// [`crate::MAX_TIMEOUT`], and [`Error::UnexpectedSdkResult`] when the SDK
    /// reports a negative count.
    pub fn connection_count(&self, timeout: Duration) -> Result<u32> {
        let timeout = to_ms_checked(timeout)?;
        // SAFETY: the router instance is live for this borrow.
        let count = unsafe {
            self.routing
                .NDIlib_routing_get_no_connections(self.instance, timeout)
        };
        u32::try_from(count).map_err(|_| {
            Error::UnexpectedSdkResult(format!(
                "NDIlib_routing_get_no_connections returned a negative count: {count}"
            ))
        })
    }
    /// The source descriptor this router is advertised under, which receivers
    /// connect to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NativeOperationFailed`] when the SDK supplies no
    /// descriptor, or one that names neither a source nor an address, and
    /// [`Error::UnexpectedSdkResult`] when its name or address is longer than
    /// the 4 MiB one SDK string may be.
    pub fn source(&self) -> Result<Source> {
        // SAFETY: the router is live, and the descriptor the SDK returns stays
        // valid until the router is destroyed, after this copy completes.
        unsafe {
            Source::try_from_raw(
                self.routing.NDIlib_routing_get_source_name(self.instance),
                "NDIlib_routing_get_source_name",
            )
        }
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        // SAFETY: the instance was created by this table and is destroyed once.
        unsafe { self.routing.NDIlib_routing_destroy(self.instance) };
    }
}
