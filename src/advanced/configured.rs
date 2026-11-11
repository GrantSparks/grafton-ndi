//! Creating routers, advertisers and listeners with a per-instance Advanced
//! SDK configuration.

use std::ffi::{c_char, CString};

use super::AdvancedConfig;
use crate::{
    discovery::{
        AdvertiserOptions, ListenerOptions, RecvAdvertiser, RecvListener, SendAdvertiser,
        SendListener,
    },
    routing::{Router, RouterOptions},
    sys::{
        advanced::{
            native, ReceiverAdvertiserConfiguration, ReceiverListenerConfiguration,
            RouterConfiguration, SenderAdvertiserConfiguration, SenderListenerConfiguration,
        },
        Table,
    },
    Result, NDI,
};

/// An object whose creation function has an Advanced SDK counterpart that
/// also takes a JSON configuration.
pub(crate) trait Configurable: Sized {
    /// What the object is created from.
    type Options;
    /// The native creation settings.
    type Settings;
    /// The native handle.
    type Instance;

    /// Create the object through `create`, which is handed the native settings
    /// for `options`, and keep `config` for as long as the object lives.
    ///
    /// The object's own capability gate is applied before `create` runs.
    ///
    /// # Safety
    ///
    /// `create` returns null or a live object of `ndi`'s runtime, made from
    /// the settings it was handed, that nothing else owns.
    unsafe fn create_with(
        ndi: &NDI,
        options: &Self::Options,
        config: CString,
        create: impl FnOnce(&Self::Settings) -> Result<Self::Instance>,
    ) -> Result<Self>;
}

/// Create a `T` with `config` through `create`, the entry point of table `C`
/// that takes the object's settings and a configuration.
///
/// # Safety
///
/// `create` returns what the SDK's configured creation function for `T`
/// returned for the settings and the configuration it was handed.
unsafe fn configured<T: Configurable, C: Table>(
    ndi: &NDI,
    options: &T::Options,
    config: &AdvancedConfig,
    create: impl FnOnce(&'static C, &T::Settings, *const c_char) -> T::Instance,
) -> Result<T> {
    // The object keeps the exact allocation given to the SDK, in case the SDK
    // keeps the pointer rather than copying the text.
    let configuration = config.to_c_string();
    let text = configuration.as_ptr();
    // SAFETY: forwarded to the caller's obligation. `text` points into
    // `configuration`, whose buffer does not move when the object takes it.
    unsafe {
        T::create_with(ndi, options, configuration, |settings| {
            Ok(create(ndi.table::<C>()?, settings, text))
        })
    }
}

/// Define `new_with_config` for an object created by `$create` of `$Table`.
macro_rules! new_with_config {
    ($(#[$doc:meta])* $Object:ty, $Options:ty, $Table:ty, $create:ident) => {
        impl $Object {
            $(#[$doc])*
            ///
            /// The object keeps the configuration for as long as it lives.
            ///
            /// # Errors
            ///
            /// The errors of [`new`](Self::new), and
            /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
            /// without an Advanced runtime (or in a build without the Advanced
            /// SDK).
            pub fn new_with_config(
                ndi: &NDI,
                options: &$Options,
                config: &AdvancedConfig,
            ) -> Result<Self> {
                // SAFETY: the closure returns what the SDK's configured
                // creation function returned, and the settings and the
                // configuration it is handed outlive that call.
                unsafe {
                    configured(ndi, options, config, |table: &'static $Table, settings, text| {
                        native!(table => table.$create(settings, text))
                    })
                }
            }
        }
    };
}

new_with_config!(
    /// Create and advertise a routing source with a per-instance Advanced SDK
    /// configuration.
    Router,
    RouterOptions,
    RouterConfiguration,
    NDIlib_routing_create_v2
);
new_with_config!(
    /// Connect to the Discovery Server with a per-instance Advanced SDK
    /// configuration, without registering any sender.
    SendAdvertiser,
    AdvertiserOptions,
    SenderAdvertiserConfiguration,
    NDIlib_send_advertiser_create_ex
);
new_with_config!(
    /// Connect to the Discovery Server with a per-instance Advanced SDK
    /// configuration, without registering any receiver.
    RecvAdvertiser,
    AdvertiserOptions,
    ReceiverAdvertiserConfiguration,
    NDIlib_recv_advertiser_create_ex
);
new_with_config!(
    /// Connect to the Discovery Server with a per-instance Advanced SDK
    /// configuration.
    SendListener,
    ListenerOptions,
    SenderListenerConfiguration,
    NDIlib_send_listener_create_ex
);
new_with_config!(
    /// Connect to the Discovery Server with a per-instance Advanced SDK
    /// configuration.
    RecvListener,
    ListenerOptions,
    ReceiverListenerConfiguration,
    NDIlib_recv_listener_create_ex
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{capabilities::RuntimeFeature, Error};

    /// Without the Advanced SDK each constructor is refused before anything
    /// native is created, whichever gate refuses it first.
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
        refused(Router::new_with_config(&ndi, &RouterOptions::new("configured"), &config).err());
        let advertiser = AdvertiserOptions::default().servers("127.0.0.1");
        refused(SendAdvertiser::new_with_config(&ndi, &advertiser, &config).err());
        refused(RecvAdvertiser::new_with_config(&ndi, &advertiser, &config).err());
        let listener = ListenerOptions::default().server("127.0.0.1").unwrap();
        refused(SendListener::new_with_config(&ndi, &listener, &config).err());
        refused(RecvListener::new_with_config(&ndi, &listener, &config).err());
    }
}
