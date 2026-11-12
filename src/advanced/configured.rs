//! Creating objects with a per-instance Advanced SDK configuration.
//!
//! Every object the SDK can also create with a JSON configuration (the
//! finder, sender, receiver and router, and the Discovery Server advertisers
//! and listeners) takes it from its options, whose `config` setter exists
//! with the `advanced` feature. Each object has one creation path: an unsafe
//! `create` function beside its `new`. It builds the native settings from the
//! options, hands them to the SDK entry point it is given, turns a null result
//! into the object's creation error, and constructs the native owner holding
//! the configuration it is given, in a field that exists only with the
//! `advanced` feature.
//!
//! This module adds the configured half once, for all of them.
//! [`Configurable`] is that creation path as generic code sees it, and
//! [`Configurable::configured`] is the one place a configuration reaches the
//! SDK: it takes the configured entry point's table before anything is built
//! or created, then hands the SDK the configuration's own buffer and the owner
//! a clone of it to keep. `configurable!` implements the trait for the finder,
//! sender, receiver and router, and defines each one's `new_configured` on
//! that path; the object's `new` calls `new_configured` when its options carry
//! a configuration, and otherwise gives the creation path the SDK's plain
//! entry point and no configuration. The advertisers and listeners share one
//! generic owner each, which implements the trait once, beside its creation
//! path, and makes the same choice in its own `new`.
//!
//! Taking the configured table applies its capability's gate, which includes
//! the gates of the capabilities it requires: the router's includes source
//! routing, and each Discovery Server object's includes the core capability
//! whose functions it goes on to use. A configured object is therefore refused
//! under its own capability before its creation path takes any other table.

use std::ffi::c_char;

use super::AdvancedConfig;
use crate::{
    routing::{Router, RouterOptions},
    sys::{
        advanced::{
            native, FinderConfiguration, ReceiverConfiguration, RouterConfiguration,
            SenderConfiguration,
        },
        bindings::*,
        Table,
    },
    Finder, FinderOptions, Ndi, Receiver, ReceiverOptions, Result, Sender, SenderOptions,
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
        ndi: &Ndi,
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
        ndi: &Ndi,
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
/// its `new_configured` on the entry point `$create` of table `$Table`.
macro_rules! configurable {
    (
        $Object:ident($Options:ty, $Settings:ty) -> $Instance:ty,
        $Table:ident::$create:ident
    ) => {
        impl Configurable for $Object {
            type Options = $Options;
            type Settings = $Settings;
            type Instance = $Instance;

            unsafe fn create_with(
                ndi: &Ndi,
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
            /// Create the object through its configured entry point with
            /// `config`, which it keeps for as long as it lives: what `new`
            /// does for options that carry a configuration. Without an
            /// Advanced runtime, or in a build without the Advanced SDK, it is
            /// refused with `Error::UnsupportedCapability`.
            pub(crate) fn new_configured(
                ndi: &Ndi,
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
    Finder(FinderOptions, NDIlib_find_create_t) -> NDIlib_find_instance_t,
    FinderConfiguration::NDIlib_find_create_v3
);
configurable!(
    Sender(SenderOptions, NDIlib_send_create_t) -> NDIlib_send_instance_t,
    SenderConfiguration::NDIlib_send_create_v2
);
configurable!(
    Receiver(ReceiverOptions, NDIlib_recv_create_v3_t) -> NDIlib_recv_instance_t,
    ReceiverConfiguration::NDIlib_recv_create_v4
);
configurable!(
    Router(RouterOptions, NDIlib_routing_create_t) -> NDIlib_routing_instance_t,
    RouterConfiguration::NDIlib_routing_create_v2
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capabilities::RuntimeFeature,
        discovery::{
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
        let receiver = ReceiverOptions::new(Source::new("")).config(config.clone());
        refused(Receiver::new(&ndi, &receiver).err());
        let router = RouterOptions::new("configured").config(config.clone());
        refused(Router::new(&ndi, &router).err());
        let advertiser = AdvertiserOptions::default()
            .servers("127.0.0.1")
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
