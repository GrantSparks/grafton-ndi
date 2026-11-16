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
//! finder.watch().wait_for_change(Duration::from_secs(2))?;
//! if let Some(camera) = finder.current_sources()?.first() {
//!     router.set_route(camera)?;
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

use crate::{
    source_retention::RetainedSources,
    sys::{self, bindings::*},
    to_ms_checked, Ndi, Result, Source,
};
use std::{ffi::CString, fmt, num::NonZeroU32, ptr, time::Duration};

/// A router's advertised name and optional discovery groups.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouterOptions {
    name: String,
    groups: Option<Vec<String>>,
    #[cfg(feature = "advanced")]
    config: Option<crate::advanced::AdvancedConfig>,
}
impl RouterOptions {
    /// Replace the advertised routing-source name, preserving other settings.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Inherit discovery groups from the NDI configuration.
    #[must_use]
    pub fn clear_groups(mut self) -> Self {
        self.groups = None;
        self
    }

    /// Remove the per-instance Advanced configuration override.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn clear_config(mut self) -> Self {
        self.config = None;
        self
    }

    /// Requested advertised name.
    #[must_use]
    pub fn configured_name(&self) -> &str {
        &self.name
    }

    /// Requested discovery groups, or `None` to inherit the NDI configuration.
    #[must_use]
    pub fn configured_groups(&self) -> Option<&[String]> {
        self.groups.as_deref()
    }

    /// The stored Advanced configuration handle, if any.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn configured_config(&self) -> Option<&crate::advanced::AdvancedConfig> {
        self.config.as_ref()
    }

    /// Configure a named routing source. Creating the Router advertises it.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            groups: None,
            #[cfg(feature = "advanced")]
            config: None,
        }
    }
    /// Set the discovery groups the router is advertised in instead of those
    /// of the NDI configuration. Creation refuses empty elements and elements
    /// containing a comma or NUL.
    #[must_use]
    pub fn groups(mut self, groups: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.groups = Some(groups.into_iter().map(Into::into).collect());
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

#[derive(Clone, Copy)]
struct RoutingInstance(NDIlib_routing_instance_t);

// SAFETY: routing handles have no thread affinity. Router owns this handle
// exclusively and is not Sync, so its native calls and destruction cannot overlap.
unsafe impl Send for RoutingInstance {}

/// A native routing source. Dropping it removes its advertisement.
///
/// # Threading
///
/// A router is [`Send`] but not [`Sync`]: it may be moved to another thread,
/// and is used by one thread at a time. To keep one in state that several
/// threads share, put it in a [`Mutex`](std::sync::Mutex), which is `Sync`
/// because the router is `Send`.
pub struct Router {
    instance: RoutingInstance,
    routing: &'static sys::Routing,
    _name: CString,
    _groups: Option<CString>,
    // The SDK may retain both an effective route and its last refused request.
    sources: RetainedSources,
    #[cfg(feature = "advanced")]
    #[cfg_attr(
        not(ndi_has_advanced_router_connection_metadata),
        expect(
            dead_code,
            reason = "only native router metadata operations use this storage"
        )
    )]
    pub(crate) connection_metadata: crate::connection_metadata::ConnectionMetadata,
    #[cfg(feature = "advanced")]
    _config: Option<crate::advanced::AdvancedConfig>,
    _ndi: Ndi,
}

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
    /// Returns [`crate::Error::UnsupportedCapability`] when the selected runtime has no
    /// [routing support](crate::capabilities::RuntimeFeature::Routing), or the
    /// options carry an Advanced configuration that the runtime or this build
    /// cannot take; [`crate::Error::InvalidCString`] when the name contains a
    /// NUL byte; [`crate::Error::InvalidInput`] when a group is empty or contains
    /// a comma or NUL; and [`crate::Error::NativeOperationFailed`] when the SDK creates no
    /// source.
    #[doc(alias = "NDIlib_routing_create")]
    #[doc(alias = "NDIlib_routing_create_v2")]
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
                #[cfg(feature = "advanced")]
                None,
                |routing, settings| routing.named().NDIlib_routing_create(settings),
            )
        }
    }

    /// The one creation path: create a router through `create`, which is handed the routing table and the native
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
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(
            &'static sys::Routing,
            &NDIlib_routing_create_t,
        ) -> sys::table::Called<NDIlib_routing_instance_t>,
    ) -> Result<Self> {
        // A router with a configuration gets here only once its own
        // capability, which requires routing, has been allowed, so it is
        // never refused for `Routing`.
        let routing = ndi.table::<sys::Routing>()?;
        let name = CString::new(options.name.as_str())?;
        let groups = crate::finder::list_cstring(options.groups.as_deref(), "groups")?;
        let instance = create(
            routing,
            &NDIlib_routing_create_t {
                p_ndi_name: name.as_ptr(),
                p_groups: groups.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
            },
        )
        .created()?
        .as_ptr();
        Ok(Self {
            instance: RoutingInstance(instance),
            routing,
            _name: name,
            _groups: groups,
            sources: RetainedSources::default(),
            #[cfg(feature = "advanced")]
            connection_metadata: crate::connection_metadata::ConnectionMetadata::default(),
            #[cfg(feature = "advanced")]
            _config: config,
            _ndi: ndi.clone(),
        })
    }

    /// The native router, for calls through an Advanced table.
    #[cfg(all(feature = "advanced", ndi_has_advanced_router_connection_metadata))]
    pub(crate) fn native_instance(&self) -> NDIlib_routing_instance_t {
        self.instance.0
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
    /// Returns [`crate::Error::NativeOperationFailed`] when the SDK rejects
    /// the change.
    #[doc(alias = "NDIlib_routing_change")]
    pub fn set_route(&mut self, source: &Source) -> Result<()> {
        self.sources.replace_if(Some(source), |source| {
            // SAFETY: this router is live and the retention helper owns both
            // the effective and replacement descriptors through the call.
            unsafe {
                self.routing
                    .named()
                    .NDIlib_routing_change(self.instance.0, source)
            }
        })
    }
    /// Stop routing to any source. Receivers stay connected to the router.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::NativeOperationFailed`] when the SDK rejects the change.
    #[doc(alias = "NDIlib_routing_clear")]
    pub fn clear_route(&mut self) -> Result<()> {
        self.sources.replace_if(None, |_| {
            // SAFETY: this router is live and the retention helper keeps both
            // previously retained descriptors alive until clear returns.
            unsafe { self.routing.named().NDIlib_routing_clear(self.instance.0) }
        })
    }
    /// Count the receivers currently connected to this router without waiting.
    /// This count does not establish progress of their media streams.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::UnexpectedSdkResult`] when the SDK reports a
    /// negative count.
    #[doc(alias = "NDIlib_routing_get_no_connections")]
    pub fn connection_count(&self) -> Result<u32> {
        self.connection_count_timeout(0)
    }

    /// Wait up to `timeout` for at least one receiver to connect.
    ///
    /// Returns the connected count, or `None` when the timeout expires.
    /// A zero timeout checks the current count without waiting.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InvalidInput`] when `timeout` exceeds
    /// [`crate::MAX_TIMEOUT`], and [`crate::Error::UnexpectedSdkResult`] when
    /// the SDK reports a negative count.
    #[doc(alias = "NDIlib_routing_get_no_connections")]
    pub fn wait_for_connection(&self, timeout: Duration) -> Result<Option<NonZeroU32>> {
        self.connection_count_timeout(to_ms_checked(timeout)?)
            .map(NonZeroU32::new)
    }

    fn connection_count_timeout(&self, timeout_ms: u32) -> Result<u32> {
        // SAFETY: the router instance is live for this borrow.
        let count = unsafe {
            self.routing
                .named()
                .NDIlib_routing_get_no_connections(self.instance.0, timeout_ms)
        };
        // The SDK count is i32, so every nonnegative value fits u32.
        crate::receiver::native_count(count).map(|count| count as u32)
    }
    /// The source descriptor this router is advertised under, which receivers
    /// connect to.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::NativeOperationFailed`] when the SDK supplies no
    /// descriptor, or one that names neither a source nor an address, and
    /// [`crate::Error::UnexpectedSdkResult`] when its name or address is longer than
    /// the 4 MiB one SDK string may be.
    #[doc(alias = "NDIlib_routing_get_source_name")]
    pub fn source(&self) -> Result<Source> {
        // SAFETY: the router is live, and the descriptor the SDK returns stays
        // valid until the router is destroyed, after this copy completes.
        unsafe {
            Source::try_from_raw(
                self.routing
                    .named()
                    .NDIlib_routing_get_source_name(self.instance.0),
            )
        }
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        // SAFETY: the instance was created by this table and is destroyed once.
        unsafe { self.routing.NDIlib_routing_destroy(self.instance.0) };
    }
}
