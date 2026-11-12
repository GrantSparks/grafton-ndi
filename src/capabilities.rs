//! Facts about the selected native SDK, separate from hardware and license availability.

#![warn(missing_docs)]

use std::{
    fmt,
    path::{Path, PathBuf},
};

use crate::sys::Api;
#[cfg(feature = "advanced")]
use crate::{advanced::PassthroughPreset, sys::advanced::AdvancedApi};

/// NDI 6.4.0: the first runtime with the planar 4:4:4 formats and newer
/// receiver modes, and the first whose Discovery Server advertisers and
/// listeners can be destroyed safely.
const NDI_6_4: SdkVersion = SdkVersion {
    major: 6,
    minor: 4,
    patch: 0,
    build: 0,
};

/// The edition of a native SDK implementation.
///
/// The edition of the selected implementation is detected from what it exports,
/// never taken from its file name or from the caller's request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SdkEdition {
    /// The standard NDI SDK.
    Standard,
    /// The NDI Advanced SDK. This does not assert a license entitlement.
    Advanced,
}

/// How the native SDK implementation is supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SdkLinkage {
    /// An explicitly loaded shared library, retained for the process lifetime.
    Shared,
    /// The SDK implementation is linked into the application.
    Static,
}

/// A parsed SDK version, distinct from its build timestamp and raw version text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SdkVersion {
    major: u32,
    minor: u32,
    patch: u32,
    build: u32,
}

impl SdkVersion {
    /// Major version.
    pub fn major(self) -> u32 {
        self.major
    }
    /// Minor version.
    pub fn minor(self) -> u32 {
        self.minor
    }
    /// Patch version.
    pub fn patch(self) -> u32 {
        self.patch
    }
    /// SDK build component, or zero for a three-component version.
    pub fn build(self) -> u32 {
        self.build
    }

    /// Parse the final token of the SDK's version text.
    ///
    /// Observed strings end in the version after a platform name and a build
    /// date, so numbers elsewhere in the text are never mistaken for it.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let token = text.split_whitespace().last()?;
        let components: Vec<_> = token.split('.').collect();
        if !(3..=4).contains(&components.len())
            || components
                .iter()
                .any(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        Some(Self {
            major: components[0].parse().ok()?,
            minor: components[1].parse().ok()?,
            patch: components[2].parse().ok()?,
            build: if components.len() == 4 {
                components[3].parse().ok()?
            } else {
                0
            },
        })
    }
}

impl fmt::Display for SdkVersion {
    /// Formats all four components, for example `6.4.0.2`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            major,
            minor,
            patch,
            build,
        } = self;
        write!(f, "{major}.{minor}.{patch}.{build}")
    }
}

/// Identity of the implementation actually selected, rather than just build headers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RuntimeInfo {
    pub(crate) edition: SdkEdition,
    pub(crate) linkage: SdkLinkage,
    pub(crate) library_path: Option<PathBuf>,
    pub(crate) raw_version: String,
    pub(crate) version: Option<SdkVersion>,
}

impl RuntimeInfo {
    /// The edition detected from the entry points the implementation exports.
    ///
    /// This does not establish a license entitlement.
    pub fn edition(&self) -> SdkEdition {
        self.edition
    }

    /// Whether the implementation is shared or statically linked.
    pub fn linkage(&self) -> SdkLinkage {
        self.linkage
    }
    /// Actual loaded module path, when using a shared implementation.
    pub fn library_path(&self) -> Option<&Path> {
        self.library_path.as_deref()
    }
    /// Unmodified native version text, including any build timestamp.
    pub fn raw_version(&self) -> &str {
        &self.raw_version
    }
    /// Parsed final version token, when recognized.
    pub fn version(&self) -> Option<SdkVersion> {
        self.version
    }
}

/// Define [`RuntimeFeature`] from one entry for each variant without a field.
///
/// The list below is the only place a capability is declared. Each entry
/// states its documentation, its variant, whether it belongs to the `core` or
/// the `advanced` SDK, the name it is reported under and, after `since`, the
/// first runtime version it may be used with. The enum, its `name`,
/// `minimum_version` and `is_advanced`, [`RuntimeFeature::UNIT`] and the place
/// [`Capabilities`] keeps each status are all generated from it, so none of
/// them can leave a capability out. `Passthrough`, the one variant with a
/// field, is written out here.
macro_rules! runtime_features {
    (@advanced core) => {
        false
    };
    (@advanced advanced) => {
        true
    };
    (@minimum) => {
        None
    };
    (@minimum $minimum:ident) => {
        Some($minimum)
    };
    ($(
        $(#[$meta:meta])*
        $edition:ident $Variant:ident as $name:literal $(since $minimum:ident)?;
    )+) => {
        /// A capability that this build or the selected runtime may lack.
        ///
        /// [`Capabilities::status`] reports each one in advance, and every operation
        /// that needs one that is unavailable fails with
        /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
        /// naming it, before anything reaches the SDK.
        ///
        /// A core capability depends on the functions the runtime exports and, for
        /// some, on its version: an older runtime would misread newer input values, or
        /// cannot destroy an object safely. Every capability from
        /// [`AdvancedFinderConfiguration`](Self::AdvancedFinderConfiguration) on
        /// belongs to the Advanced SDK: it also needs an Advanced SDK runtime and a
        /// build with the `advanced` feature whose SDK headers declare it.
        /// Availability says nothing about hardware or licenses.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum RuntimeFeature {
            $(
                $(#[$meta])*
                $Variant,
            )+
            /// Receiving compressed packets with this preset.
            ///
            /// A packet receiver is created as a receiver with Advanced JSON
            /// configuration is, so this needs everything
            /// [`AdvancedReceiverConfiguration`](Self::AdvancedReceiverConfiguration)
            /// does, and also that this build's headers declare the receiver color
            /// format that selects the preset.
            #[cfg(feature = "advanced")]
            Passthrough(PassthroughPreset),
        }

        /// The variants of [`RuntimeFeature`] without a field, numbered in
        /// declaration order from zero.
        enum Index {
            $($Variant,)+
        }

        /// Where [`Capabilities`] finds a feature's status.
        enum Slot {
            /// At this index of the statuses it keeps.
            Unit(usize),
            /// Derived from the configured receiver's, for a passthrough preset.
            #[cfg(feature = "advanced")]
            Passthrough,
        }

        impl RuntimeFeature {
            /// Every variant without a field, in declaration order: the
            /// capabilities whose statuses [`Capabilities`] keeps.
            pub(crate) const UNIT: [Self; [$(Index::$Variant),+].len()] = [$(Self::$Variant),+];

            /// Where [`Capabilities`] finds the capability's status. The
            /// variant at each index of [`Self::UNIT`] is kept at that index.
            fn slot(self) -> Slot {
                match self {
                    $(Self::$Variant => Slot::Unit(Index::$Variant as usize),)+
                    #[cfg(feature = "advanced")]
                    Self::Passthrough(_) => Slot::Passthrough,
                }
            }

            /// The one name the capability is reported under.
            pub(crate) fn name(self) -> &'static str {
                match self {
                    $(Self::$Variant => $name,)+
                    #[cfg(feature = "advanced")]
                    Self::Passthrough(_) => "compressed passthrough preset",
                }
            }

            /// The first runtime version the capability may be used with, when the
            /// functions a runtime exports are not evidence enough.
            ///
            /// The 6.4 formats and receiver modes are enum values an older runtime
            /// would misread. The advertiser and listener functions exist in older
            /// runtimes, but the vendor's change log records crashes when destroying
            /// those objects that were fixed in 6.3.2 and again in 6.4.0; an object
            /// created with Advanced configuration is destroyed the same way.
            pub(crate) fn minimum_version(self) -> Option<SdkVersion> {
                match self {
                    $(Self::$Variant => runtime_features!(@minimum $($minimum)?),)+
                    #[cfg(feature = "advanced")]
                    Self::Passthrough(_) => None,
                }
            }

            /// Whether the capability belongs to the Advanced SDK.
            ///
            /// Each such capability is gated by a table of Advanced entry points,
            /// which states, with the build's declarations, what the capability
            /// needs.
            pub(crate) fn is_advanced(self) -> bool {
                match self {
                    $(Self::$Variant => runtime_features!(@advanced $edition),)+
                    #[cfg(feature = "advanced")]
                    Self::Passthrough(_) => true,
                }
            }
        }
    };
}

runtime_features! {
    /// Submitting and receiving the NDI 6.4 planar 4:4:4 formats.
    core Planar444Video as "planar 4:4:4 video" since NDI_6_4;
    /// The Best444 receiver color mode.
    core ReceiveBest444 as "Best444 receiver mode" since NDI_6_4;
    /// Video-only highest and lowest receiver bandwidth modes.
    core ReceiveVideoOnly as "video-only receiver bandwidth" since NDI_6_4;
    /// Independent native audio/video receiver delays.
    core ReceiveDelay as "receiver audio/video delay";
    /// Metadata a receiver replays to every new connection.
    core ReceiverConnectionMetadata as "receiver connection metadata";
    /// The connected source's web-control URL.
    core ReceiverWebControl as "receiver web control";
    /// Notifications of the connected source's actual name.
    core ConnectedSourceName as "connected source name";
    /// Metadata a sender captures from its connected receivers.
    core SenderMetadata as "sender metadata";
    /// The SDK current-timestamp operation.
    core SdkTimestamp as "SDK timestamp";
    /// Conversion between planar and interleaved audio.
    core InterleavedAudioConversion as "interleaved audio conversion";
    /// Direct submission of interleaved audio to a sender.
    core InterleavedAudioSend as "interleaved audio submission";
    /// Conversion between the V210 and P216 video layouts.
    core VideoConversion as "V210/P216 conversion";
    /// Source routing, including its create/destroy and query operations.
    core Routing as "source routing";
    /// Discovery Server sender registration and removal. Needs NDI 6.4.0 or
    /// later: earlier runtimes can crash when an advertiser is destroyed.
    core SenderAdvertising as "sender advertising" since NDI_6_4;
    /// Discovery Server receiver registration and removal. Needs NDI 6.4.0 or
    /// later: earlier runtimes can crash when an advertiser is destroyed.
    core ReceiverAdvertising as "receiver advertising" since NDI_6_4;
    /// Sender snapshots, subscriptions and owned event batches. Needs NDI
    /// 6.4.0 or later: earlier runtimes can crash when a listener is destroyed.
    core SenderDiscovery as "sender discovery" since NDI_6_4;
    /// Receiver snapshots, subscriptions, event batches and connection control.
    /// Needs NDI 6.4.0 or later: earlier runtimes can crash when a listener is
    /// destroyed.
    core ReceiverDiscovery as "receiver discovery" since NDI_6_4;
    /// Creating a finder with Advanced JSON configuration.
    advanced AdvancedFinderConfiguration as "Advanced finder configuration";
    /// Creating a sender with Advanced JSON configuration.
    advanced AdvancedSenderConfiguration as "Advanced sender configuration";
    /// Creating a receiver with Advanced JSON configuration.
    advanced AdvancedReceiverConfiguration as "Advanced receiver configuration";
    /// Creating a router with Advanced JSON configuration.
    advanced AdvancedRouterConfiguration as "Advanced router configuration";
    /// Creating a Discovery Server sender advertiser with Advanced JSON
    /// configuration. Needs NDI 6.4.0 or later, as
    /// [`SenderAdvertising`](Self::SenderAdvertising) does, whose functions
    /// the advertiser goes on to use.
    advanced AdvancedSenderAdvertiserConfiguration as "Advanced sender advertiser configuration" since NDI_6_4;
    /// Creating a Discovery Server receiver advertiser with Advanced JSON
    /// configuration. Needs NDI 6.4.0 or later, as
    /// [`ReceiverAdvertising`](Self::ReceiverAdvertising) does, whose
    /// functions the advertiser goes on to use.
    advanced AdvancedReceiverAdvertiserConfiguration as "Advanced receiver advertiser configuration" since NDI_6_4;
    /// Creating a Discovery Server sender listener with Advanced JSON
    /// configuration. Needs NDI 6.4.0 or later, as
    /// [`SenderDiscovery`](Self::SenderDiscovery) does, whose functions the
    /// listener goes on to use.
    advanced AdvancedSenderListenerConfiguration as "Advanced sender listener configuration" since NDI_6_4;
    /// Creating a Discovery Server receiver listener with Advanced JSON
    /// configuration. Needs NDI 6.4.0 or later, as
    /// [`ReceiverDiscovery`](Self::ReceiverDiscovery) does, whose functions
    /// the listener goes on to use.
    advanced AdvancedReceiverListenerConfiguration as "Advanced receiver listener configuration" since NDI_6_4;
    /// Source snapshots that carry each source's metadata.
    advanced SourceMetadata as "source metadata";
    /// Metadata a router replays to each new connection.
    advanced RouterConnectionMetadata as "router connection metadata";
    /// A sender's own contribution to its tally state.
    advanced SenderTally as "sender tally override";
    /// The target frame size, bit rate and quality for an encoder feeding a
    /// sender.
    advanced EncoderGuidance as "encoder guidance";
    /// Encoder quality together with the formats the connected receivers
    /// accept.
    advanced EncoderQualityPlanning as "encoder quality planning";
    /// Whether the receivers connected to a sender need a keyframe, and
    /// waiting for one to ask for it.
    advanced KeyframeDemand as "keyframe demand";
    /// A receiver's request for a keyframe from its source.
    advanced KeyframeRequest as "keyframe request";
    /// Sending compressed frames held in several blocks.
    advanced ScatterSend as "scatter sending";
    /// A receiver's choice between its source's program and preview streams.
    advanced ReceiverStreamTier as "receiver stream tier";
    /// A receiver's choice of which of its source's streams it receives.
    advanced ReceiverStreamSelection as "receiver stream selection";
    /// Application-supplied memory for the frames a receiver delivers.
    advanced ReceiveAllocators as "custom receive allocators";
    /// Notification that the SDK has finished with an asynchronously sent
    /// video frame.
    advanced SendCompletion as "asynchronous send completion";
    /// Matching a receiver's audio to its video timestamps.
    advanced AvSync as "AV synchronization";
    /// Clocks locked to a reference source.
    advanced Genlock as "genlock";
    /// Keyboard, mouse, touch and clipboard control of a source.
    advanced Kvm as "KVM control";
}

/// Why a capability cannot safely be used in this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CapabilityUnavailable {
    /// The SDK version text is unrecognized, so new enum semantics cannot be assumed.
    RuntimeVersionUnknown,
    /// The selected runtime predates this capability.
    RuntimeTooOld,
    /// At least one required native function is absent.
    SymbolUnavailable,
    /// This build lacks the locally generated ABI required by the optional feature.
    BuildAbiUnavailable,
    /// The selected native SDK edition does not implement this API family.
    WrongEdition,
}

impl fmt::Display for CapabilityUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RuntimeVersionUnknown => "the selected runtime's version is not recognized",
            Self::RuntimeTooOld => "the selected runtime is too old",
            Self::SymbolUnavailable => "the selected runtime does not export a required function",
            Self::BuildAbiUnavailable => {
                "this build was made without the Advanced SDK declarations"
            }
            Self::WrongEdition => "the selected runtime is not an Advanced SDK runtime",
        })
    }
}

/// Availability established from the selected implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CapabilityStatus {
    /// The wrapper has the required ABI/semantic evidence. Hardware and licenses are separate.
    Available,
    /// The operation must be rejected before calling the SDK.
    Unavailable(CapabilityUnavailable),
}

/// What the selected implementation, together with this build, provides:
/// one [`CapabilityStatus`] for every [`RuntimeFeature`].
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Capabilities {
    /// The status of each variant without a field, at its index in
    /// [`RuntimeFeature::UNIT`].
    statuses: [CapabilityStatus; RuntimeFeature::UNIT.len()],
}

/// Each capability's status, keyed by the capability.
impl fmt::Debug for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(RuntimeFeature::UNIT.iter().zip(&self.statuses))
            .finish()
    }
}

impl Capabilities {
    /// Derive the capabilities of an implementation from the tables it
    /// resolved, its parsed version and its detected edition.
    ///
    /// This is the one rule every capability is gated by: an operation is
    /// refused exactly when the status derived here, or for a passthrough
    /// preset from the configured receiver's here, is unavailable.
    pub(crate) fn detect(
        api: &Api,
        #[cfg(feature = "advanced")] advanced: &AdvancedApi,
        version: Option<SdkVersion>,
        edition: SdkEdition,
    ) -> Self {
        // The feature's version gate.
        let versioned = |feature: RuntimeFeature| match (feature.minimum_version(), version) {
            (None, _) => CapabilityStatus::Available,
            (Some(minimum), Some(version)) if version >= minimum => CapabilityStatus::Available,
            (Some(_), Some(_)) => {
                CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeTooOld)
            }
            (Some(_), None) => {
                CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeVersionUnknown)
            }
        };
        // A missing function is reported first: it is the more specific reason,
        // and no version makes up for it.
        let exported = |feature: RuntimeFeature, available: bool| {
            if available {
                versioned(feature)
            } else {
                CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable)
            }
        };
        let status = |feature: RuntimeFeature| {
            // Which entry points a feature needs is stated with its table. A
            // core feature without any of its own is only a version gate.
            if !feature.is_advanced() {
                return exported(feature, api.missing(feature).is_none());
            }
            // An Advanced feature's table also states which declarations it
            // needs. A build without them is refused first, then a runtime of
            // the wrong edition, then one that lacks an entry point.
            #[cfg(feature = "advanced")]
            let (declared, resolved) = (
                AdvancedApi::declares(feature),
                advanced.missing(feature).is_none(),
            );
            #[cfg(not(feature = "advanced"))]
            let (declared, resolved) = (false, false);
            if !declared {
                CapabilityStatus::Unavailable(CapabilityUnavailable::BuildAbiUnavailable)
            } else if edition != SdkEdition::Advanced {
                CapabilityStatus::Unavailable(CapabilityUnavailable::WrongEdition)
            } else {
                exported(feature, resolved)
            }
        };
        Self {
            statuses: RuntimeFeature::UNIT.map(status),
        }
    }

    /// Query a capability without changing native state.
    pub fn status(&self, feature: RuntimeFeature) -> CapabilityStatus {
        match feature.slot() {
            Slot::Unit(index) => self.statuses[index],
            // A preset adds a declaration to the configured receiver it is
            // created as; the rest of its status is that receiver's.
            #[cfg(feature = "advanced")]
            Slot::Passthrough => {
                if AdvancedApi::declares(feature) {
                    self.status(RuntimeFeature::AdvancedReceiverConfiguration)
                } else {
                    CapabilityStatus::Unavailable(CapabilityUnavailable::BuildAbiUnavailable)
                }
            }
        }
    }

    /// Whether the wrapper has enough evidence to attempt the operation.
    pub fn supports(&self, feature: RuntimeFeature) -> bool {
        self.status(feature) == CapabilityStatus::Available
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_final_version_not_build_time() {
        let v = SdkVersion::parse("NDI SDK LINUX 10:14:14 Nov 6 2026 6.4.0.2").unwrap();
        assert_eq!((v.major(), v.minor(), v.patch(), v.build()), (6, 4, 0, 2));
        for s in [
            "",
            "NDI 6.4",
            "NDI 2026-11-06",
            "NDI 6.4.0.2 trailing",
            "6..4.0",
            "6.4.0.-1",
        ] {
            assert!(SdkVersion::parse(s).is_none(), "{s}");
        }
    }

    /// What an implementation that exports the names `exported` accepts,
    /// with version text `version` and edition `edition`, resolved and
    /// gated as the loader does it.
    struct Fixture {
        api: Api,
        #[cfg(feature = "advanced")]
        advanced: AdvancedApi,
        info: RuntimeInfo,
        capabilities: Capabilities,
    }

    impl Fixture {
        fn new(
            exported: impl Fn(&str) -> bool,
            version: Option<&str>,
            edition: SdkEdition,
        ) -> Self {
            let resolver = crate::sys::table::ExportsWhere(exported);
            let api = Api::load(&resolver).unwrap();
            #[cfg(feature = "advanced")]
            let advanced = AdvancedApi::load(&resolver).unwrap();
            let version = version.map(|text| SdkVersion::parse(text).unwrap());
            #[cfg(feature = "advanced")]
            let capabilities = Capabilities::detect(&api, &advanced, version, edition);
            #[cfg(not(feature = "advanced"))]
            let capabilities = Capabilities::detect(&api, version, edition);
            Self {
                api,
                #[cfg(feature = "advanced")]
                advanced,
                info: RuntimeInfo {
                    edition,
                    linkage: SdkLinkage::Shared,
                    library_path: None,
                    raw_version: version
                        .map_or_else(|| "NDI SDK TEST".into(), |v| format!("NDI SDK TEST {v}")),
                    version,
                },
                capabilities,
            }
        }

        fn evidence(&self) -> crate::sys::Evidence<'_> {
            crate::sys::Evidence {
                api: &self.api,
                #[cfg(feature = "advanced")]
                advanced: &self.advanced,
                info: &self.info,
                capabilities: &self.capabilities,
            }
        }
    }

    fn detect(
        exported: impl Fn(&str) -> bool,
        version: Option<&str>,
        edition: SdkEdition,
    ) -> Capabilities {
        Fixture::new(exported, version, edition).capabilities
    }

    /// Every feature, with each passthrough preset in builds that have them.
    fn every_feature() -> Vec<RuntimeFeature> {
        let features = RuntimeFeature::UNIT.into_iter();
        #[cfg(feature = "advanced")]
        let features = features.chain(PassthroughPreset::ALL.map(RuntimeFeature::Passthrough));
        features.collect()
    }

    /// Whether this build declares what `feature` needs.
    fn declared(feature: RuntimeFeature) -> bool {
        #[cfg(feature = "advanced")]
        let declared = AdvancedApi::declares(feature);
        #[cfg(not(feature = "advanced"))]
        let declared = false;
        !feature.is_advanced() || declared
    }

    /// A refusal's feature, reason and detail.
    type Refusal = (RuntimeFeature, CapabilityUnavailable, String);

    fn refusal<T>(result: crate::Result<T>) -> std::result::Result<(), Refusal> {
        match result {
            Ok(_) => Ok(()),
            Err(crate::Error::UnsupportedCapability {
                feature,
                reason,
                detail,
            }) => Err((feature, reason, detail)),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn every_feature_reports_a_status_that_require_feature_repeats() {
        let names: std::collections::BTreeSet<_> = every_feature()
            .into_iter()
            .map(RuntimeFeature::name)
            .collect();
        assert!(!names.contains(""));
        for (version, edition) in [
            (Some("6.4.0.2"), SdkEdition::Advanced),
            (Some("6.4.0.2"), SdkEdition::Standard),
            (Some("6.3.2.0"), SdkEdition::Advanced),
            (None, SdkEdition::Advanced),
        ] {
            let fixture = Fixture::new(|_| true, version, edition);
            for feature in every_feature() {
                let status = fixture.capabilities.status(feature);
                let context = format!("{feature:?} with {version:?} {edition:?}");
                match refusal(fixture.evidence().require(feature)) {
                    Ok(()) => assert_eq!(status, CapabilityStatus::Available, "{context}"),
                    Err((refused, reason, _)) => {
                        assert_eq!(refused, feature, "{context}");
                        assert_eq!(status, CapabilityStatus::Unavailable(reason), "{context}");
                    }
                }
                // An implementation that exports everything, has the latest
                // version and the Advanced edition provides every feature this
                // build declares.
                let expected = if !declared(feature) {
                    CapabilityStatus::Unavailable(CapabilityUnavailable::BuildAbiUnavailable)
                } else if feature.is_advanced() && edition != SdkEdition::Advanced {
                    CapabilityStatus::Unavailable(CapabilityUnavailable::WrongEdition)
                } else {
                    match (feature.minimum_version(), version) {
                        (Some(_), None) => CapabilityStatus::Unavailable(
                            CapabilityUnavailable::RuntimeVersionUnknown,
                        ),
                        (Some(_), Some("6.3.2.0")) => {
                            CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeTooOld)
                        }
                        _ => CapabilityStatus::Available,
                    }
                };
                assert_eq!(status, expected, "{context}");
            }
        }
    }

    /// The generated list holds each feature once, at the index its status is
    /// kept at, and reports each under a name no other feature has.
    #[test]
    fn every_feature_is_listed_once_under_its_own_name() {
        use std::collections::HashSet;

        let features = every_feature();
        let distinct: HashSet<_> = features.iter().copied().collect();
        assert_eq!(distinct.len(), features.len());
        for (index, feature) in RuntimeFeature::UNIT.into_iter().enumerate() {
            assert!(
                matches!(feature.slot(), Slot::Unit(at) if at == index),
                "{feature:?}"
            );
        }

        // Every passthrough preset is reported under the one name.
        let names: HashSet<_> = RuntimeFeature::UNIT
            .into_iter()
            .map(RuntimeFeature::name)
            .collect();
        assert_eq!(names.len(), RuntimeFeature::UNIT.len());
        #[cfg(feature = "advanced")]
        for preset in PassthroughPreset::ALL {
            let name = RuntimeFeature::Passthrough(preset).name();
            assert!(!names.contains(name), "{preset:?}: {name}");
        }

        // As the enum's documentation says, every capability from
        // `AdvancedFinderConfiguration` on belongs to the Advanced SDK.
        let core = RuntimeFeature::UNIT
            .iter()
            .take_while(|feature| !feature.is_advanced())
            .count();
        assert_eq!(
            RuntimeFeature::UNIT[core],
            RuntimeFeature::AdvancedFinderConfiguration
        );
        assert!(RuntimeFeature::UNIT[core..]
            .iter()
            .all(|feature| feature.is_advanced()));
    }

    #[test]
    fn debug_output_names_each_feature_with_its_status() {
        let capabilities = detect(
            |name| !name.starts_with("NDIlib_routing_"),
            Some("6.4.0.2"),
            SdkEdition::Standard,
        );
        let debug = format!("{capabilities:?}");
        assert!(
            debug.contains("Routing: Unavailable(SymbolUnavailable)"),
            "{debug}"
        );
        assert!(debug.contains("ReceiveDelay: Available"), "{debug}");
    }

    #[test]
    fn unknown_identity_does_not_claim_enum_semantics() {
        let capabilities = detect(
            |name| name != "NDIlib_util_get_timestamp",
            None,
            SdkEdition::Standard,
        );
        assert!(capabilities.supports(RuntimeFeature::ReceiveDelay));
        assert_eq!(
            capabilities.status(RuntimeFeature::ReceiveBest444),
            CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeVersionUnknown)
        );
        assert_eq!(
            capabilities.status(RuntimeFeature::SdkTimestamp),
            CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable)
        );
    }

    #[test]
    fn each_optional_table_gates_only_its_own_feature() {
        let capabilities = detect(
            |name| !name.starts_with("NDIlib_routing_"),
            Some("6.4.0.2"),
            SdkEdition::Standard,
        );
        assert_eq!(
            capabilities.status(RuntimeFeature::Routing),
            CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable)
        );
        for feature in [
            RuntimeFeature::Planar444Video,
            RuntimeFeature::ReceiveBest444,
            RuntimeFeature::ReceiveVideoOnly,
            RuntimeFeature::ReceiveDelay,
            RuntimeFeature::ReceiverConnectionMetadata,
            RuntimeFeature::ReceiverWebControl,
            RuntimeFeature::ConnectedSourceName,
            RuntimeFeature::SenderMetadata,
            RuntimeFeature::SdkTimestamp,
            RuntimeFeature::InterleavedAudioConversion,
            RuntimeFeature::InterleavedAudioSend,
            RuntimeFeature::VideoConversion,
        ]
        .into_iter()
        .chain(DISCOVERY_SERVER)
        {
            assert!(capabilities.supports(feature), "{feature:?}");
        }
    }

    #[test]
    fn older_runtimes_report_version_gated_features_as_too_old() {
        let capabilities = detect(|_| true, Some("6.3.2.0"), SdkEdition::Standard);
        assert_eq!(
            capabilities.status(RuntimeFeature::Planar444Video),
            CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeTooOld)
        );
    }

    /// The capabilities whose objects talk to a Discovery Server.
    const DISCOVERY_SERVER: [RuntimeFeature; 4] = [
        RuntimeFeature::SenderAdvertising,
        RuntimeFeature::ReceiverAdvertising,
        RuntimeFeature::SenderDiscovery,
        RuntimeFeature::ReceiverDiscovery,
    ];

    #[test]
    fn discovery_server_objects_need_their_functions_and_ndi_6_4() {
        let unavailable = |reason| CapabilityStatus::Unavailable(reason);
        let too_old = detect(|_| true, Some("6.3.2.0"), SdkEdition::Standard);
        let unknown = detect(|_| true, None, SdkEdition::Standard);
        let first_fixed = detect(|_| true, Some("6.4.0.0"), SdkEdition::Standard);
        let without_functions = detect(
            |name| !name.contains("_advertiser_") && !name.contains("_listener_"),
            Some("6.3.2.0"),
            SdkEdition::Standard,
        );
        for feature in DISCOVERY_SERVER {
            assert_eq!(
                too_old.status(feature),
                unavailable(CapabilityUnavailable::RuntimeTooOld),
                "{feature:?}"
            );
            assert_eq!(
                unknown.status(feature),
                unavailable(CapabilityUnavailable::RuntimeVersionUnknown),
                "{feature:?}"
            );
            assert!(first_fixed.supports(feature), "{feature:?}");
            // A missing function is the more specific reason, whatever the version.
            assert_eq!(
                without_functions.status(feature),
                unavailable(CapabilityUnavailable::SymbolUnavailable),
                "{feature:?}"
            );
        }
        // Routing has no such defect: its functions are evidence enough.
        assert!(too_old.supports(RuntimeFeature::Routing));
        assert!(unknown.supports(RuntimeFeature::Routing));
    }

    #[test]
    fn capabilities_returning_receiver_strings_need_the_release_function() {
        let capabilities = detect(
            |name| name != "NDIlib_recv_free_string",
            Some("6.4.0.2"),
            SdkEdition::Standard,
        );
        for feature in [
            RuntimeFeature::ReceiverWebControl,
            RuntimeFeature::ConnectedSourceName,
        ] {
            assert_eq!(
                capabilities.status(feature),
                CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable),
                "{feature:?}"
            );
        }
        assert!(capabilities.supports(RuntimeFeature::ReceiverConnectionMetadata));
    }

    #[test]
    fn versions_and_reasons_have_readable_text() {
        assert_eq!(NDI_6_4.to_string(), "6.4.0.0");
        assert_eq!(
            SdkVersion::parse("NDI SDK 6.4.0.2").unwrap().to_string(),
            "6.4.0.2"
        );
        assert_eq!(
            CapabilityUnavailable::RuntimeTooOld.to_string(),
            "the selected runtime is too old"
        );
    }

    /// Every Advanced table's gate and `Ndi::require_feature` are one gate:
    /// for every reason a status can give, both accept the table's feature or
    /// both refuse it, reporting that feature with the same reason and
    /// evidence.
    #[cfg(feature = "advanced")]
    #[test]
    fn every_advanced_gate_reports_its_own_feature_and_agrees_with_its_status() {
        use crate::sys::advanced::GATES;
        use CapabilityUnavailable::*;

        // One table for each Advanced feature other than a preset, and none
        // for a core feature.
        let gated: Vec<_> = GATES.iter().map(|(feature, ..)| *feature).collect();
        for feature in every_feature() {
            let tables = gated.iter().filter(|gated| **gated == feature).count();
            let expected =
                feature.is_advanced() && !matches!(feature, RuntimeFeature::Passthrough(_));
            assert_eq!(tables, usize::from(expected), "{feature:?}");
        }

        for &(feature, symbols, gate) in GATES {
            let entry = symbols[0].to_str().unwrap();
            // Every combination of edition, the table's first entry point and
            // the version: a shared runtime is Advanced only when it exports
            // the edition's entry points, but a linked archive has the edition
            // the build chose, whatever it lacks.
            let mut reached = Vec::new();
            for (lacks_entry, edition, version) in [
                (false, SdkEdition::Advanced, "6.4.0.2"),
                (false, SdkEdition::Standard, "6.4.0.2"),
                (true, SdkEdition::Advanced, "6.4.0.2"),
                (true, SdkEdition::Standard, "6.4.0.2"),
                (false, SdkEdition::Advanced, "6.3.2.0"),
            ] {
                let fixture =
                    Fixture::new(|name| !lacks_entry || name != entry, Some(version), edition);
                let evidence = fixture.evidence();
                let context =
                    format!("{feature:?} lacking {entry}: {lacks_entry}, {edition:?} {version}");
                let required = refusal(evidence.require(feature));
                assert_eq!(refusal(gate(evidence)), required, "{context}");
                let status = fixture.capabilities.status(feature);
                reached.push(status);
                let expected = match status {
                    CapabilityStatus::Available => Ok(()),
                    CapabilityStatus::Unavailable(reason) => {
                        let detail = match reason {
                            // The declaration the build lacks is one of the table's.
                            BuildAbiUnavailable => {
                                let Err((_, _, detail)) = &required else {
                                    panic!("{context}: accepted without declarations");
                                };
                                assert!(symbols.iter().any(|symbol| symbol.to_str() == Ok(detail.as_str())), "{context}: {detail}");
                                detail.clone()
                            }
                            // The entry point the runtime lacks; a runtime that
                            // exports it but is not Advanced has none to give.
                            SymbolUnavailable | WrongEdition if lacks_entry => entry.to_owned(),
                            SymbolUnavailable | WrongEdition => String::new(),
                            RuntimeTooOld | RuntimeVersionUnknown => format!(
                                "it needs NDI 6.4.0.0 or later and the runtime reports \"NDI SDK TEST {version}\""
                            ),
                        };
                        Err((feature, reason, detail))
                    }
                };
                assert_eq!(required, expected, "{context}");
            }
            let expected: &[_] = if declared(feature) {
                &[
                    CapabilityStatus::Available,
                    CapabilityStatus::Unavailable(WrongEdition),
                    CapabilityStatus::Unavailable(SymbolUnavailable),
                ]
            } else {
                &[CapabilityStatus::Unavailable(BuildAbiUnavailable)]
            };
            for status in expected {
                assert!(reached.contains(status), "{feature:?}: {status:?}");
            }
            // Only the Discovery Server objects have a minimum version.
            let too_old = CapabilityStatus::Unavailable(RuntimeTooOld);
            assert_eq!(
                reached.contains(&too_old),
                declared(feature) && feature.minimum_version().is_some(),
                "{feature:?}"
            );
        }
    }

    /// A passthrough preset is gated as the configured receiver it is created
    /// as, after the color format that selects it, and is reported as itself.
    #[cfg(feature = "advanced")]
    #[test]
    fn every_preset_gate_reports_its_own_preset() {
        use CapabilityUnavailable::*;
        const RECEIVER: &str = "NDIlib_recv_create_v4";
        for preset in PassthroughPreset::ALL {
            let feature = RuntimeFeature::Passthrough(preset);
            let format = crate::sys::advanced::passthrough_format(preset);
            for (lacks_receiver, edition) in [
                (false, SdkEdition::Advanced),
                (false, SdkEdition::Standard),
                (true, SdkEdition::Advanced),
            ] {
                let fixture = Fixture::new(
                    |name| !lacks_receiver || name != RECEIVER,
                    Some("6.4.0.2"),
                    edition,
                );
                let evidence = fixture.evidence();
                let context =
                    format!("{preset:?} lacking {RECEIVER}: {lacks_receiver}, {edition:?}");
                let gated = evidence.passthrough_format(preset);
                assert_eq!(
                    gated.as_ref().ok(),
                    format
                        .as_ref()
                        .ok()
                        .filter(|_| fixture.capabilities.supports(feature)),
                    "{context}"
                );
                let required = refusal(evidence.require(feature));
                assert_eq!(refusal(gated), required, "{context}");
                let receiver = fixture
                    .capabilities
                    .status(RuntimeFeature::AdvancedReceiverConfiguration);
                let expected = match (format, receiver) {
                    // The constant this build lacks, by the header's name for it.
                    (Err(constant), _) => {
                        let constant = constant.to_str().unwrap();
                        assert!(
                            constant.starts_with("NDIlib_recv_color_format_ex_compressed_v"),
                            "{constant}"
                        );
                        Err((feature, BuildAbiUnavailable, constant.to_owned()))
                    }
                    (Ok(_), CapabilityStatus::Available) => Ok(()),
                    // Otherwise, whatever the configured receiver lacks.
                    (Ok(_), CapabilityStatus::Unavailable(_)) => {
                        refusal(evidence.require(RuntimeFeature::AdvancedReceiverConfiguration))
                            .map_err(|(_, reason, detail)| (feature, reason, detail))
                    }
                };
                assert_eq!(required, expected, "{context}");
            }
        }
    }

    #[cfg(feature = "advanced")]
    #[test]
    fn only_advanced_features_are_gated_by_advanced_tables() {
        // With nothing exported, every Advanced table names what it lacks.
        let nothing = AdvancedApi::load(&crate::sys::table::ExportsWhere(|_| false)).unwrap();
        for feature in every_feature() {
            assert_eq!(
                nothing.missing(feature).is_some(),
                feature.is_advanced(),
                "{feature:?}"
            );
        }
    }

    #[test]
    fn version_gates_admit_every_later_release() {
        assert!(SdkVersion::parse("6.3.2.0").unwrap() < NDI_6_4);
        for later in ["6.4.0", "6.4.0.2", "6.5.1", "7.0.0"] {
            assert!(SdkVersion::parse(later).unwrap() >= NDI_6_4, "{later}");
        }
    }
}
