//! Advanced tables checked against the declarations present in the installed SDK.
//!
//! The shared registry drives both build-time availability and runtime loading.
//! Missing declaration families have uninhabited stand-ins; wrappers preserve
//! their public API and return a typed capability error before reaching them.

#[cfg(feature = "advanced")]
use super::bindings::NDIlib_recv_color_format_e;
use super::{table::Resolver, Evidence, Table};
#[cfg(feature = "advanced")]
use crate::{advanced::PassthroughPreset, capabilities::Capability};
use crate::{
    capabilities::{Capabilities, CapabilityRefusal, RuntimeFeature},
    LoadRefusal, Result,
};
use std::ffi::CStr;

#[cfg(ndi_has_advanced_header)]
// The shared registry's selected families determine which generated ABI names
// it uses. Partial headers can leave either glob (or native_table) unused.
#[allow(unused_imports)]
use super::{advanced_bindings::*, bindings::*, table::native_table};

include!(concat!(env!("OUT_DIR"), "/ndi_advanced_selection.rs"));
#[cfg(feature = "advanced")]
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
#[cfg(feature = "advanced")]
macro_rules! native {
    ($Family:ident, $table:expr, uses($($binding:ident),* $(,)?) => $body:expr) => {
        $crate::sys::advanced::declared! { $Family {{
            let _: &$crate::sys::advanced::$Family = &$table;
            $body
        }} else {{
            let _ = ($(&$binding,)*);
            match *$table {}
        }}}
    };
    ($Family:ident, $table:expr => $body:expr) => {
        $crate::sys::advanced::declared! { $Family {{
            let _: &$crate::sys::advanced::$Family = &$table;
            $body
        }} else { match *$table {} }}
    };
}
#[cfg(feature = "advanced")]
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

                    fn load(_: &dyn Resolver) -> std::result::Result<std::result::Result<Self, &'static CStr>, LoadRefusal> {
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
            $($field: std::result::Result<$Table, CapabilityRefusal>,)+
        }

        /// Resolution refusals only; these slots cannot produce gate evidence.
        pub(crate) struct ResolvedAdvancedApi {
            $($field: std::result::Result<$Table, CapabilityRefusal>,)+
        }

        // Every Advanced capability owns exactly one table, in every build.
        // Missing and duplicate registry entries are selection-time defects
        // rejected here, rather than panics while detecting capabilities.
        const _: () = {
            let mut index = 0;
            while index < RuntimeFeature::UNIT.len() {
                let feature = RuntimeFeature::UNIT[index];
                let counts = [$(matches!(feature, RuntimeFeature::$feature) as usize),+];
                let mut table = 0;
                let mut count = 0;
                while table < counts.len() {
                    count += counts[table];
                    table += 1;
                }
                assert!(
                    count == feature.is_advanced() as usize,
                    "every Advanced capability must have exactly one Advanced table"
                );
                index += 1;
            }
        };

        impl AdvancedApi {
            pub(in crate::sys) fn load(resolver: &dyn Resolver) -> std::result::Result<ResolvedAdvancedApi, LoadRefusal> {
                Ok(ResolvedAdvancedApi { $($field: $Table::load(resolver)?.map_err(|symbol| {
                    declared! { $Table { CapabilityRefusal::SymbolUnavailable(symbol) } else { CapabilityRefusal::BuildAbiUnavailable(symbol) } }
                }),)+ })
            }
        }

        impl ResolvedAdvancedApi {
            /// Construct each table only under its final recorded decision.
            pub(in crate::sys) fn seal(self, capabilities: &Capabilities) -> AdvancedApi {
                AdvancedApi {
                    $($field: capabilities.decision(RuntimeFeature::$feature).and(self.$field),)+
                }
            }

            /// The first function of the table gated by `feature` that did not
            /// resolve: one the implementation does not export or, when its
            /// family is not declared, one this build does not declare.
            /// `None` when everything resolved, or for a feature without a
            /// table in this registry.
            pub(crate) fn missing(&self, feature: RuntimeFeature) -> Option<CapabilityRefusal> {
                match feature {
                    $(RuntimeFeature::$feature => self.$field.as_ref().err().copied(),)+
                    _ => None,
                }
            }

            /// The declaration decision, before edition and symbol decisions.
            /// Non-Advanced features require no declarations from this registry.
            pub(crate) fn declarations(feature: RuntimeFeature) -> std::result::Result<(), CapabilityRefusal> {
                match feature {
                    $(RuntimeFeature::$feature => declared! { $Table { Ok(()) } else {
                        Err(CapabilityRefusal::BuildAbiUnavailable($Table::UNDECLARED))
                    } },)+
                    _ => Ok(()),
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
#[cfg(feature = "advanced")]
macro_rules! passthrough_presets {
    ($($Preset:ident => $constant:ident,)+) => {
        impl PassthroughPreset {
            /// Every preset, in the order listed, for tests that cover each one.
            pub(crate) const ALL: [Self; [$(stringify!($Preset)),+].len()] =
                [$(Self::$Preset),+];

            pub(crate) fn index(self) -> usize {
                enum Index { $($Preset,)+ }
                match self { $(Self::$Preset => Index::$Preset as usize,)+ }
            }
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

#[cfg(feature = "advanced")]
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

#[cfg(feature = "advanced")]
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
        self.capabilities
            .passthrough_format(preset)
            .map_err(|refusal| crate::Error::UnsupportedCapability {
                capability: Capability::Runtime(RuntimeFeature::Passthrough(preset)),
                refusal,
            })
    }
}

#[cfg(all(test, ndi_has_advanced_header))]
#[path = "advanced_coverage_tests.rs"]
mod coverage_tests;
