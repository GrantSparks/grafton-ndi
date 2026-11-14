//! Advanced tables checked against the declarations present in the installed SDK.
//!
//! The shared registry drives both build-time availability and runtime loading.
//! Missing declaration families have uninhabited stand-ins; wrappers preserve
//! their public API and return a typed capability error before reaching them.

use super::{bindings::NDIlib_recv_color_format_e, table::Resolver, Evidence, Table};
use crate::{
    advanced::PassthroughPreset,
    capabilities::{CapabilityUnavailable, RuntimeFeature},
    Result,
};
use std::ffi::CStr;

#[cfg(ndi_has_advanced_header)]
// The shared registry's selected families determine which generated ABI names
// it uses. Partial headers can leave either glob (or native_table) unused.
#[allow(unused_imports)]
use super::{advanced_bindings::*, bindings::*, table::native_table};

include!(concat!(env!("OUT_DIR"), "/ndi_advanced_selection.rs"));
pub(crate) use declared;

// Stored types follow their family, rather than the presence of an umbrella
// header: an earlier Advanced SDK may have neither AV sync nor genlock.
#[cfg(ndi_has_advanced_av_sync)]
pub(crate) use super::advanced_bindings::NDIlib_avsync_instance_t as AvSyncHandle;
#[cfg(ndi_has_advanced_scatter_send)]
pub(crate) use super::advanced_bindings::NDIlib_frame_scatter_t as FrameScatter;
#[cfg(ndi_has_advanced_genlock)]
pub(crate) use super::advanced_bindings::NDIlib_genlock_instance_t as GenlockHandle;

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

/// Define the Advanced SDK's tables.
///
/// Each entry states, once, everything about one capability: the table of its
/// entry points, the field of [`AdvancedApi`] that holds it, and the
/// [`RuntimeFeature`] it is gated and reported under. A table is handed out
/// under its feature's status, as a core table is, so that
/// `Ndi::require_feature` and the table always agree.
macro_rules! advanced_tables {
    ($(
        $(#[$meta:meta])*
        struct $Table:ident in $field:ident: $feature:ident {
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
                    evidence.capability(RuntimeFeature::$feature, &evidence.advanced.$field)
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
            /// family is not declared, one this build does not declare. A
            /// passthrough preset's receiver is the configured receiver, after
            /// the color format constant this build may not declare. `None`
            /// when everything resolved, and for a core feature.
            pub(crate) fn missing(&self, feature: RuntimeFeature) -> Option<&'static CStr> {
                match feature {
                    $(RuntimeFeature::$feature => self.$field.as_ref().err().copied(),)+
                    RuntimeFeature::Passthrough(preset) => passthrough_format(preset)
                        .err()
                        .or_else(|| self.missing(RuntimeFeature::AdvancedReceiverConfiguration)),
                    _ => None,
                }
            }

            /// Whether this build declares what `feature` needs: the table it
            /// gates and, for a passthrough preset, the color format constant
            /// that selects it. `false` for a core feature.
            pub(crate) fn declares(feature: RuntimeFeature) -> bool {
                match feature {
                    $(RuntimeFeature::$feature => declared! { $Table { true } else { false } },)+
                    RuntimeFeature::Passthrough(preset) => {
                        passthrough_format(preset).is_ok()
                            && Self::declares(RuntimeFeature::AdvancedReceiverConfiguration)
                    }
                    _ => false,
                }
            }
        }

        /// Each Advanced table's feature, exported names and gate, for tests
        /// that hold every gate to the feature it reports.
        #[cfg(test)]
        pub(crate) const GATES: &[(
            RuntimeFeature,
            &[&CStr],
            fn(Evidence<'_>) -> Result<()>,
        )] = &[$((
            RuntimeFeature::$feature,
            $Table::SYMBOLS,
            |evidence| $Table::within(evidence).map(|_| ()),
        ),)+];

        #[cfg(all(test, ndi_has_advanced_header))]
        const TABLES: &[(&str, &[&CStr])] = &[$((stringify!($Table), $Table::SYMBOLS),)+];
    };
}

include!("advanced_tables.rs");

/// Declares every passthrough preset with the extended receiver color format
/// that selects it: `passthrough_format`, and the test list
/// `PassthroughPreset::ALL`. The match in `passthrough_format` is
/// exhaustive, so the compiler refuses a list that misses a preset, and a
/// preset listed twice is an unreachable pattern.
macro_rules! passthrough_presets {
    ($($Preset:ident => $constant:ident,)+) => {
        impl PassthroughPreset {
            /// Every preset, in the order listed, for tests that cover each one.
            #[cfg(test)]
            pub(crate) const ALL: [Self; [$(stringify!($Preset)),+].len()] =
                [$(Self::$Preset),+];
        }

        /// The receiver color format a packet receiver is created with to pass
        /// through `preset`, or the header's name for the constant that gives it
        /// when this build does not declare it.
        pub(crate) fn passthrough_format(
            preset: PassthroughPreset,
        ) -> std::result::Result<NDIlib_recv_color_format_e, &'static CStr> {
            let format: std::result::Result<_, &'static CStr> = match preset {
                $(PassthroughPreset::$Preset => declared! { $constant {
                    // The extended color formats take the target's C enum
                    // type; the creation settings carry the same bits.
                    Ok(NDIlib_recv_color_format_e::from_ne_bytes(
                        super::advanced_bindings::$constant.to_ne_bytes(),
                    ))
                } else {
                    Err(super::table::symbol_name(concat!(stringify!($constant), "\0")))
                } },)+
            };
            // The headers declare each constant without its enumeration's
            // name, which the generated binding prefixes it with.
            format.map_err(|binding| {
                binding
                    .to_bytes_with_nul()
                    .strip_prefix(b"NDIlib_recv_color_format_ex_e_")
                    .and_then(|name| CStr::from_bytes_with_nul(name).ok())
                    .unwrap_or(binding)
            })
        }
    };
}

passthrough_presets! {
    SpeedHqOnly => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v1,
    SpeedHq => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v2,
    SpeedHqBest => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v2_best,
    H264 => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v3,
    H264Best => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v3_best,
    H264WithAudio => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v3_with_audio,
    Hevc => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v4,
    HevcBest => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v4_best,
    HevcWithAudio => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v4_with_audio,
    Alpha => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v5,
    AlphaBest => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v5_best,
    AlphaWithAudio => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v5_with_audio,
    Protected => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v6,
    ProtectedBest => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v6_best,
    ProtectedWithAudio => NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v6_with_audio,
}

impl Evidence<'_> {
    /// The receiver color format that selects `preset`, once the preset's
    /// gate allows it. That gate includes the configured receiver's, so the
    /// receiver's table is then available too.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// for [`RuntimeFeature::Passthrough`], whenever [`Self::require`]
    /// refuses it.
    pub(crate) fn passthrough_format(
        self,
        preset: PassthroughPreset,
    ) -> Result<NDIlib_recv_color_format_e> {
        let feature = RuntimeFeature::Passthrough(preset);
        self.require(feature)?;
        // The preset's status is available only when its constant is
        // declared, so this is reached only if that rule ever changes.
        passthrough_format(preset)
            .map_err(|_| self.unavailable(feature, CapabilityUnavailable::BuildAbiUnavailable))
    }
}

#[cfg(all(test, ndi_has_advanced_header))]
#[path = "advanced_coverage_tests.rs"]
mod coverage_tests;
