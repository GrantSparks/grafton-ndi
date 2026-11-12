//! Advanced tables checked against the declarations present in the installed SDK.
//!
//! The shared registry drives both build-time availability and runtime loading.
//! Missing declaration families have uninhabited stand-ins; wrappers preserve
//! their public API and return a typed capability error before reaching them.

use super::{table::Resolver, Runtime, Table};
use crate::{Error, Result};
use std::ffi::CStr;

#[cfg(ndi_has_advanced_header)]
#[allow(unused_imports)]
use super::{advanced_bindings::*, bindings::*, table::native_table};
#[cfg(ndi_has_advanced_header)]
use crate::capabilities::SdkEdition;

include!(concat!(env!("OUT_DIR"), "/ndi_advanced_selection.rs"));
pub(crate) use declared;

// Stored types follow their family, rather than the presence of an umbrella
// header: an earlier Advanced SDK may have neither AV sync nor genlock.
declared! { AvSync {
    pub(crate) use super::advanced_bindings::NDIlib_avsync_instance_t as AvSyncHandle;
} else {
    pub(crate) type AvSyncHandle = *mut std::ffi::c_void;
}}
declared! { Genlock {
    pub(crate) use super::advanced_bindings::NDIlib_genlock_instance_t as GenlockHandle;
} else {
    pub(crate) type GenlockHandle = *mut std::ffi::c_void;
}}
declared! { ScatterSend {
    pub(crate) use super::advanced_bindings::NDIlib_frame_scatter_t as FrameScatter;
} else {
    pub(crate) enum FrameScatter {}
}}

/// Compile a native expression only when its entire declaration family exists.
/// Otherwise its table is uninhabited, proving the expression unreachable.
macro_rules! native {
    ($Family:ident, $table:expr => $body:expr) => {
        $crate::sys::advanced::declared! { $Family {{
            let _: &'static $crate::sys::advanced::$Family = $table;
            $body
        }} else { match *$table {} }}
    };
}
pub(crate) use native;

#[cfg(ndi_has_advanced_header)]
#[cfg_attr(not(ndi_has_complete_advanced_header), allow(dead_code))]
fn not_exported(capability: &str, runtime: &Runtime, symbol: &CStr) -> Error {
    let symbol = symbol.to_string_lossy();
    let reason = if runtime.info.edition() == SdkEdition::Advanced {
        format!("the selected NDI runtime does not export {symbol}")
    } else {
        format!(
            "the selected NDI runtime is not an Advanced SDK runtime: it does not export {symbol}"
        )
    };
    Error::UnsupportedCapability {
        capability: capability.into(),
        reason,
    }
}

#[cfg(not(ndi_has_complete_advanced_header))]
fn not_built(capability: &str) -> Error {
    Error::UnsupportedCapability {
        capability: capability.into(),
        reason: format!(
            "grafton-ndi was built without the Advanced SDK declarations required for {capability}; \
             rebuild with NDI_SDK_DIR naming an NDI Advanced SDK that declares this API"
        ),
    }
}

macro_rules! advanced_tables {
    ($(
        $(#[$meta:meta])*
        struct $Table:ident in $field:ident: $capability:literal {
            $(fn $name:ident($($argument:ident: $type:ty),* $(,)?) $(-> $return:ty)?;)+
        }
    )+) => {
        $(
            declared! { $Table {
                native_table! {
                    $(#[$meta])*
                    pub(crate) struct $Table from crate::sys::advanced_declarations {
                        $(fn $name($($argument: $type),*) $(-> $return)?;)+
                    }
                }
            } else {
                $(#[$meta])*
                pub(crate) enum $Table {}

                impl $Table {
                    const SYMBOLS: &'static [&'static CStr] = &[
                        $(super::table::symbol_name(concat!(stringify!($name), "\0"))),+
                    ];

                    fn load(_: &dyn Resolver) -> Result<std::result::Result<Self, &'static CStr>> {
                        Ok(Err(Self::SYMBOLS[0]))
                    }
                }
            }}

            impl Table for $Table {
                fn of(runtime: &'static Runtime) -> Result<&'static Self> {
                    runtime.advanced.$field.as_ref().map_err(|_symbol| {
                        declared! { $Table {
                            not_exported($capability, runtime, _symbol)
                        } else {
                            not_built($capability)
                        }}
                    })
                }
            }
        )+

        /// Each family is resolved atomically from the selected implementation.
        pub(crate) struct AdvancedApi {
            $($field: std::result::Result<$Table, &'static CStr>,)+
        }

        impl AdvancedApi {
            pub(crate) fn load(resolver: &dyn Resolver) -> Result<Self> {
                Ok(Self { $($field: $Table::load(resolver)?,)+ })
            }
        }

        #[cfg(all(test, ndi_has_advanced_header))]
        const TABLES: &[(&str, &[&CStr])] = &[$((stringify!($Table), $Table::SYMBOLS),)+];
    };
}

include!("advanced_tables.rs");

#[cfg(all(test, ndi_has_advanced_header))]
#[path = "advanced_coverage_tests.rs"]
mod coverage_tests;
