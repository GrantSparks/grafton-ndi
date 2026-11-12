//! Creating objects with a per-instance Advanced SDK configuration.
//!
//! Every object the SDK can also create with a JSON configuration (the
//! finder, sender, receiver and router, and the Discovery Server advertisers
//! and listeners) has one creation path: an unsafe `create` function beside
//! its `new`. It builds the native settings from the options, hands them to
//! the SDK entry point it is given, turns a null result into the object's
//! creation error, and constructs the native owner holding the configuration
//! it is given, in a field that exists only with the `advanced` feature. `new`
//! gives it the SDK's plain entry point and no configuration.
//!
//! This module adds the configured half once, for all of them.
//! [`Configurable`] is that creation path as generic code sees it, and
//! [`Configurable::configured`] is the one place a configuration reaches the
//! SDK: it takes the configured entry point's table before anything is built
//! or created, then hands the SDK the configuration's own buffer and the owner
//! a clone of it to keep. `configurable!` implements the trait for each object
//! and defines its `new_with_config` on that path.

use std::ffi::c_char;

use super::AdvancedConfig;
use crate::{
    discovery::{
        AdvertiserOptions, ListenerOptions, RecvAdvertiser, RecvListener, SendAdvertiser,
        SendListener,
    },
    routing::{Router, RouterOptions},
    sys::{
        advanced::{
            native, FinderConfiguration, ReceiverAdvertiserConfiguration, ReceiverConfiguration,
            ReceiverListenerConfiguration, RouterConfiguration, SenderAdvertiserConfiguration,
            SenderConfiguration, SenderListenerConfiguration,
        },
        bindings::*,
        Table,
    },
    Finder, FinderOptions, Receiver, ReceiverOptions, Result, Sender, SenderOptions, NDI,
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

    /// Run the object's creation path with `create` as its entry point, which
    /// a creation error names as `operation`, and keep `config` for as long
    /// as the object lives.
    ///
    /// # Safety
    ///
    /// `create` returns null or a live object of `ndi`'s runtime, made from
    /// the settings it was handed, that nothing else owns.
    unsafe fn create_with(
        ndi: &NDI,
        options: &Self::Options,
        operation: &'static str,
        config: AdvancedConfig,
        create: impl FnOnce(&Self::Settings) -> Self::Instance,
    ) -> Result<Self>;

    /// Create the object with `config` through `create`, the entry point
    /// `operation` of table `C` that takes the settings and a configuration.
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
        ndi: &NDI,
        options: &Self::Options,
        operation: &'static str,
        config: &AdvancedConfig,
        create: impl FnOnce(&'static C, &Self::Settings, *const c_char) -> Self::Instance,
    ) -> Result<Self> {
        let table = ndi.table::<C>()?;
        let text = config.as_ptr();
        // SAFETY: the SDK is handed `text`, the buffer that every clone of
        // `config` shares, and the object is handed one of those clones to
        // keep. The buffer neither moves nor is freed while a clone lives, and
        // every owner destroys its native object before it releases the
        // clone, so the text stays readable for as long as the SDK may read
        // it, whether it copies the text or keeps the pointer. What `create`
        // returns is the caller's obligation.
        unsafe {
            Self::create_with(ndi, options, operation, config.clone(), |settings| {
                create(table, settings, text)
            })
        }
    }
}

/// Implement [`Configurable`] for `$Object` on its creation path, and define
/// its `new_with_config` on the entry point `$create` of table `$Table`.
macro_rules! configurable {
    (
        $(#[$doc:meta])*
        $Object:ident($Options:ty, $Settings:ty) -> $Instance:ty,
        $Table:ident::$create:ident
    ) => {
        impl Configurable for $Object {
            type Options = $Options;
            type Settings = $Settings;
            type Instance = $Instance;

            unsafe fn create_with(
                ndi: &NDI,
                options: &$Options,
                operation: &'static str,
                config: AdvancedConfig,
                create: impl FnOnce(&$Settings) -> $Instance,
            ) -> Result<Self> {
                // SAFETY: forwarded to the caller's obligation.
                unsafe {
                    Self::create(ndi, options, operation, Some(config), |_, settings| {
                        create(settings)
                    })
                }
            }
        }

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
                // SAFETY: the closure returns what the entry point returned.
                unsafe {
                    Self::configured(
                        ndi,
                        options,
                        stringify!($create),
                        config,
                        |table: &'static $Table, settings, text| {
                            native!($Table, table => table.$create(settings, text))
                        },
                    )
                }
            }
        }
    };
}

configurable!(
    /// Create a finder with a per-instance Advanced SDK configuration.
    Finder(FinderOptions, NDIlib_find_create_t) -> NDIlib_find_instance_t,
    FinderConfiguration::NDIlib_find_create_v3
);
configurable!(
    /// Create a sender with a per-instance Advanced SDK configuration.
    Sender(SenderOptions, NDIlib_send_create_t) -> NDIlib_send_instance_t,
    SenderConfiguration::NDIlib_send_create_v2
);
configurable!(
    /// Create a receiver with a per-instance Advanced SDK configuration.
    Receiver(ReceiverOptions, NDIlib_recv_create_v3_t) -> NDIlib_recv_instance_t,
    ReceiverConfiguration::NDIlib_recv_create_v4
);
configurable!(
    /// Create and advertise a routing source with a per-instance Advanced SDK
    /// configuration.
    Router(RouterOptions, NDIlib_routing_create_t) -> NDIlib_routing_instance_t,
    RouterConfiguration::NDIlib_routing_create_v2
);
configurable!(
    /// Connect to the Discovery Server with a per-instance Advanced SDK
    /// configuration, without registering any sender.
    SendAdvertiser(AdvertiserOptions, NDIlib_send_advertiser_create_t)
        -> NDIlib_send_advertiser_instance_t,
    SenderAdvertiserConfiguration::NDIlib_send_advertiser_create_ex
);
configurable!(
    /// Connect to the Discovery Server with a per-instance Advanced SDK
    /// configuration, without registering any receiver.
    RecvAdvertiser(AdvertiserOptions, NDIlib_recv_advertiser_create_t)
        -> NDIlib_recv_advertiser_instance_t,
    ReceiverAdvertiserConfiguration::NDIlib_recv_advertiser_create_ex
);
configurable!(
    /// Connect to the Discovery Server with a per-instance Advanced SDK
    /// configuration.
    SendListener(ListenerOptions, NDIlib_send_listener_create_t)
        -> NDIlib_send_listener_instance_t,
    SenderListenerConfiguration::NDIlib_send_listener_create_ex
);
configurable!(
    /// Connect to the Discovery Server with a per-instance Advanced SDK
    /// configuration.
    RecvListener(ListenerOptions, NDIlib_recv_listener_create_t)
        -> NDIlib_recv_listener_instance_t,
    ReceiverListenerConfiguration::NDIlib_recv_listener_create_ex
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{capabilities::RuntimeFeature, Error, Source};

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
        refused(Finder::new_with_config(&ndi, &FinderOptions::default(), &config).err());
        let sender = SenderOptions::builder("configured").build();
        refused(Sender::new_with_config(&ndi, &sender, &config).err());
        let receiver = ReceiverOptions::builder(Source::default()).build();
        refused(Receiver::new_with_config(&ndi, &receiver, &config).err());
        refused(Router::new_with_config(&ndi, &RouterOptions::new("configured"), &config).err());
        let advertiser = AdvertiserOptions::default().servers("127.0.0.1");
        refused(SendAdvertiser::new_with_config(&ndi, &advertiser, &config).err());
        refused(RecvAdvertiser::new_with_config(&ndi, &advertiser, &config).err());
        let listener = ListenerOptions::default().server("127.0.0.1").unwrap();
        refused(SendListener::new_with_config(&ndi, &listener, &config).err());
        refused(RecvListener::new_with_config(&ndi, &listener, &config).err());
    }
}
