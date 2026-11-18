//! Select the plain or configured native constructor from the typed options.
//!
//! The configured path takes its capability before constructing settings.
//! Its returned instance carries the configuration it may have retained.

use std::ffi::c_char;

use super::AdvancedConfig;
use crate::{
    routing::{Router, RouterOptions},
    runtime::NativeCreation,
    sys::{
        advanced::{
            native, FinderConfiguration, ReceiverConfiguration, RouterConfiguration,
            SenderConfiguration,
        },
        bindings::*,
        Table,
    },
    Finder, FinderOptions, Ndi, Receiver, ReceiverOptions, Result, Sender, SenderMode,
    SenderOptions,
};

/// An object whose SDK creation function has an Advanced counterpart that
/// also takes a JSON configuration.
pub(crate) trait Configurable: Sized {
    /// What the object is created from.
    type Options;
    /// The native creation settings built from the options.
    type Settings;
    /// The native handle.
    type Instance;

    fn config(options: &Self::Options) -> Option<&AdvancedConfig>;
    fn create_plain(ndi: &Ndi, options: &Self::Options) -> Result<Self>;
    fn create_configured(
        ndi: &Ndi,
        options: &Self::Options,
        config: Option<&AdvancedConfig>,
    ) -> Result<Self>;

    fn create_from_options(ndi: &Ndi, options: &Self::Options) -> Result<Self> {
        match Self::config(options) {
            Some(config) => Self::create_configured(ndi, options, Some(config)),
            None => Self::create_plain(ndi, options),
        }
    }

    /// The object's fresh-instance creation path with its own settings.
    ///
    /// # Safety
    ///
    /// `create` returns null or a live object of `ndi`'s runtime, made from
    /// the settings it was handed, that nothing else owns.
    unsafe fn create_with(
        ndi: &Ndi,
        options: &Self::Options,
        create: impl FnOnce(&Self::Settings) -> NativeCreation<Self::Instance>,
    ) -> Result<Self>;

    /// Create the object with `config` through `create`, the entry point
    /// of table `C` that takes the settings and a configuration.
    ///
    /// The table is taken first, so a runtime or a build without that entry
    /// point is refused with
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// before anything is built or created, whatever the options.
    ///
    /// # Safety
    ///
    /// `create` returns what that entry point returned for the settings and
    /// the configuration it was handed.
    unsafe fn configured<C: Table>(
        ndi: &Ndi,
        options: &Self::Options,
        config: Option<&AdvancedConfig>,
        create: impl FnOnce(
            &C,
            &Self::Settings,
            *const c_char,
        ) -> crate::sys::table::Called<Self::Instance>,
    ) -> Result<Self> {
        let table = ndi.table::<C>()?;
        // SAFETY: the caller promises a fresh instance from these settings.
        // The returned creation result pairs it with the lent allocation.
        unsafe {
            Self::create_with(ndi, options, |settings| {
                let (call, config) =
                    AdvancedConfig::lend(config, |text| create(table, settings, text));
                NativeCreation { call, config }
            })
        }
    }
}

/// Implement the shared selection path for each constructor pair.
macro_rules! configurable {
    (
        $Object:ty $(where $Mode:ident: $Bound:path)? => ($Options:ty, $Settings:ty) -> $Instance:ty,
        $Table:ident::$create:ident
    ) => {
        impl$(<$Mode: $Bound>)? Configurable for $Object {
            type Options = $Options;
            type Settings = $Settings;
            type Instance = $Instance;

            fn config(options: &$Options) -> Option<&AdvancedConfig> {
                options.configured_config()
            }

            fn create_plain(ndi: &Ndi, options: &$Options) -> Result<Self> {
                Self::create_plain(ndi, options)
            }

            fn create_configured(
                ndi: &Ndi,
                options: &$Options,
                config: Option<&AdvancedConfig>,
            ) -> Result<Self> {
                // SAFETY: the closure returns this SDK constructor's fresh
                // instance for the supplied settings and configuration.
                unsafe {
                    Self::configured(
                        ndi,
                        options,
                        config,
                        |table: &$Table, _settings, _text| {
                            native!($Table, table => table.named().$create(_settings, _text))
                        },
                    )
                }
            }

            unsafe fn create_with(
                ndi: &Ndi,
                options: &$Options,
                create: impl FnOnce(&$Settings) -> NativeCreation<$Instance>,
            ) -> Result<Self> {
                // SAFETY: forwarded to the caller's fresh-instance obligation.
                unsafe { Self::create(ndi, options, |_, settings| create(settings)) }
            }
        }
    };
}

configurable!(
    Finder => (FinderOptions, NDIlib_find_create_t) -> NDIlib_find_instance_t,
    FinderConfiguration::NDIlib_find_create_v3
);
configurable!(
    Sender<M> where M: SenderMode => (SenderOptions, NDIlib_send_create_t) -> NDIlib_send_instance_t,
    SenderConfiguration::NDIlib_send_create_v2
);
configurable!(
    Receiver<M> where M: crate::receiver::CreatableMode => (ReceiverOptions, NDIlib_recv_create_v3_t) -> NDIlib_recv_instance_t,
    ReceiverConfiguration::NDIlib_recv_create_v4
);
configurable!(
    Router => (RouterOptions, NDIlib_routing_create_t) -> NDIlib_routing_instance_t,
    RouterConfiguration::NDIlib_routing_create_v2
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capabilities::RuntimeFeature,
        discovery_server::{
            AdvertiserOptions, ListenerOptions, RecvAdvertiser, RecvListener, SendAdvertiser,
            SendListener,
        },
        Error, Source,
    };

    /// Without the Advanced SDK each constructor given a configuration is
    /// refused before anything native is created, whichever gate refuses it
    /// first.
    #[test]
    fn configured_creation_is_refused_without_the_advanced_sdk() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        if ndi
            .require_feature(RuntimeFeature::AdvancedSenderConfiguration)
            .is_ok()
        {
            // Covered against a real Advanced runtime by the integration tests.
            return;
        }
        let config = AdvancedConfig::parse("{}").unwrap();
        let refused = |error: Option<Error>| {
            assert!(
                matches!(error, Some(Error::UnsupportedCapability { .. })),
                "{error:?}"
            );
        };
        let finder = FinderOptions::new().config(config.clone());
        refused(Finder::new(&ndi, &finder).err());
        let sender = SenderOptions::new("configured").config(config.clone());
        refused(Sender::new(&ndi, &sender).err());
        let receiver = ReceiverOptions::new(Source::new("").unwrap()).config(config.clone());
        refused(Receiver::new(&ndi, &receiver).err());
        let router = RouterOptions::new("configured").config(config.clone());
        refused(Router::new(&ndi, &router).err());
        let advertiser = AdvertiserOptions::default()
            .servers(["127.0.0.1"])
            .config(config.clone());
        refused(SendAdvertiser::new(&ndi, &advertiser).err());
        refused(RecvAdvertiser::new(&ndi, &advertiser).err());
        let listener = ListenerOptions::default()
            .server("127.0.0.1")
            .config(config.clone());
        refused(SendListener::new(&ndi, &listener).err());
        refused(RecvListener::new(&ndi, &listener).err());
        // A listener that names more than one server is refused too, but only
        // once the configured entry point has been found.
        let listeners = ListenerOptions::default().server("a,b").config(config);
        refused(SendListener::new(&ndi, &listeners).err());
    }
}
