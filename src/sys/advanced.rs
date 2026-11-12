//! Advanced tables checked against the declarations present in the installed SDK.
//!
//! The shared registry drives both build-time availability and runtime loading.
//! Missing declaration families have uninhabited stand-ins; wrappers preserve
//! their public API and return a typed capability error before reaching them.

use super::{table::Resolver, Evidence, Table};
use crate::{
    capabilities::{CapabilityUnavailable, RuntimeFeature, SdkEdition},
    Error, Result,
};
use std::ffi::CStr;

#[cfg(ndi_has_advanced_header)]
#[allow(unused_imports)]
use super::{advanced_bindings::*, bindings::*, table::native_table};

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

/// The error for an Advanced table, reported under `capability`, that did not
/// resolve for want of `symbol`: a function this build does not declare when
/// the family is not `declared`, and otherwise one the runtime does not export.
///
/// The reasons are tried in the order [`Capabilities`] derives them in.
///
/// [`Capabilities`]: crate::capabilities::Capabilities
fn unresolved(
    capability: &'static str,
    edition: SdkEdition,
    declared: bool,
    symbol: &CStr,
) -> Error {
    let reason = if !declared {
        CapabilityUnavailable::BuildAbiUnavailable
    } else if edition == SdkEdition::Advanced {
        CapabilityUnavailable::SymbolUnavailable
    } else {
        CapabilityUnavailable::WrongEdition
    };
    Error::UnsupportedCapability {
        capability,
        reason,
        detail: symbol.to_string_lossy().into_owned(),
    }
}

/// How an `advanced_tables!` entry is gated.
///
/// An entry names either the [`RuntimeFeature`] it is gated and reported
/// under, as an `optional_tables!` entry does, or the name it is reported
/// under when the table alone is its gate.
macro_rules! gate {
    // Handed out under the feature's status, as a core table is, so that
    // `NDI::require_feature` and the table always agree.
    (of $Table:ident in $field:ident: $feature:ident, $evidence:ident) => {
        $evidence.capability(RuntimeFeature::$feature, &$evidence.advanced.$field)
    };
    (of $Table:ident in $field:ident: $capability:literal, $evidence:ident) => {
        $evidence.advanced.$field.as_ref().map_err(|symbol| {
            unresolved(
                $capability,
                $evidence.info.edition(),
                declared! { $Table { true } else { false } },
                symbol,
            )
        })
    };
    (missing $field:ident: $feature:ident, $api:ident, $requested:ident) => {
        if $requested == RuntimeFeature::$feature {
            return $api.$field.as_ref().err().copied();
        }
    };
    (missing $field:ident: $capability:literal, $api:ident, $requested:ident) => {};
    (declares $Table:ident: $feature:ident, $requested:ident) => {
        if $requested == RuntimeFeature::$feature {
            return declared! { $Table { true } else { false } };
        }
    };
    (declares $Table:ident: $capability:literal, $requested:ident) => {};
}

macro_rules! advanced_tables {
    ($(
        $(#[$meta:meta])*
        struct $Table:ident in $field:ident: $gate:tt {
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

                    /// The first of the family's functions this build does
                    /// not declare. Some function is undeclared, or the family
                    /// would be, so the search ends within the table.
                    const UNDECLARED: &'static CStr = {
                        let declared = [$(declared! { $name { true } else { false } }),+];
                        let mut index = 0;
                        while declared[index] {
                            index += 1;
                        }
                        Self::SYMBOLS[index]
                    };

                    fn load(_: &dyn Resolver) -> Result<std::result::Result<Self, &'static CStr>> {
                        Ok(Err(Self::UNDECLARED))
                    }
                }
            }}

            impl Table for $Table {
                fn within(evidence: Evidence<'_>) -> Result<&'_ Self> {
                    gate!(of $Table in $field: $gate, evidence)
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

            /// The first function of the table gated by `feature` that did not
            /// resolve: one the implementation does not export or, when its
            /// family is not declared, one this build does not declare. `None`
            /// when the table resolved, and for a feature that gates no table
            /// here.
            pub(crate) fn missing(&self, feature: RuntimeFeature) -> Option<&'static CStr> {
                $(gate!(missing $field: $gate, self, feature);)+
                None
            }

            /// Whether this build declares the table gated by `feature`.
            /// `false` for a feature that gates no table here.
            pub(crate) fn declares(feature: RuntimeFeature) -> bool {
                $(gate!(declares $Table: $gate, feature);)+
                false
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
