//! Facts about the selected native SDK, separate from hardware and license availability.

use std::path::{Path, PathBuf};

use crate::sys::{AdvancedEntryPoints, Api};

/// The first runtime version with the NDI 6.4 pixel formats and receiver modes.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SdkEdition {
    /// The standard NDI SDK.
    Standard,
    /// The NDI Advanced SDK. This does not assert a license entitlement.
    Advanced,
}

/// How the native SDK implementation is supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SdkLinkage {
    /// An explicitly loaded shared library, retained for the process lifetime.
    Shared,
    /// The SDK implementation is linked into the application.
    Static,
}

/// A parsed SDK version, distinct from its build timestamp and raw version text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

/// Identity of the implementation actually selected, rather than just build headers.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// A capability whose absence must be checked before passing newer inputs to NDI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeFeature {
    /// Submitting and receiving the NDI 6.4 planar 4:4:4 formats.
    Planar444Video,
    /// The Best444 receiver color mode.
    ReceiveBest444,
    /// Video-only highest and lowest receiver bandwidth modes.
    ReceiveVideoOnly,
    /// Independent native audio/video receiver delays.
    ReceiveDelay,
    /// The SDK current-timestamp operation.
    SdkTimestamp,
    /// Source routing, including its create/destroy and query operations.
    Routing,
    /// Discovery Server sender registration and removal.
    SenderAdvertising,
    /// Discovery Server receiver registration and removal.
    ReceiverAdvertising,
    /// Sender snapshots, subscriptions and owned event batches.
    SenderDiscovery,
    /// Receiver snapshots, subscriptions, event batches and connection control.
    ReceiverDiscovery,
    /// Creating a sender with Advanced JSON configuration.
    AdvancedSenderConfiguration,
    /// Creating a receiver with Advanced JSON configuration.
    AdvancedReceiverConfiguration,
}

impl RuntimeFeature {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Planar444Video => "planar 4:4:4 video",
            Self::ReceiveBest444 => "Best444 receiver mode",
            Self::ReceiveVideoOnly => "video-only receiver bandwidth",
            Self::ReceiveDelay => "receiver audio/video delay",
            Self::SdkTimestamp => "SDK timestamp",
            Self::Routing => "source routing",
            Self::SenderAdvertising => "sender advertising",
            Self::ReceiverAdvertising => "receiver advertising",
            Self::SenderDiscovery => "sender discovery",
            Self::ReceiverDiscovery => "receiver discovery",
            Self::AdvancedSenderConfiguration => "Advanced sender configuration",
            Self::AdvancedReceiverConfiguration => "Advanced receiver configuration",
        }
    }
}

/// Why a capability cannot safely be used in this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Availability established from the selected implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CapabilityStatus {
    /// The wrapper has the required ABI/semantic evidence. Hardware and licenses are separate.
    Available,
    /// The operation must be rejected before calling the SDK.
    Unavailable(CapabilityUnavailable),
}

/// A snapshot of core SDK capability evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    planar_444_video: CapabilityStatus,
    receive_best_444: CapabilityStatus,
    receive_video_only: CapabilityStatus,
    receive_delay: CapabilityStatus,
    sdk_timestamp: CapabilityStatus,
    routing: CapabilityStatus,
    sender_advertising: CapabilityStatus,
    receiver_advertising: CapabilityStatus,
    sender_discovery: CapabilityStatus,
    receiver_discovery: CapabilityStatus,
    advanced_sender_configuration: CapabilityStatus,
    advanced_receiver_configuration: CapabilityStatus,
}

impl Capabilities {
    /// Derive the capabilities of an implementation from the tables it
    /// resolved, its parsed version and its detected edition.
    pub(crate) fn detect(
        api: &Api,
        version: Option<SdkVersion>,
        edition: SdkEdition,
        advanced: AdvancedEntryPoints,
    ) -> Self {
        let exported = |available: bool| {
            if available {
                CapabilityStatus::Available
            } else {
                CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable)
            }
        };
        let since = |minimum: SdkVersion| match version {
            Some(version) if version >= minimum => CapabilityStatus::Available,
            Some(_) => CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeTooOld),
            None => CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeVersionUnknown),
        };
        let advanced_entry = |available: bool| {
            if !cfg!(all(feature = "advanced", ndi_has_advanced_header)) {
                CapabilityStatus::Unavailable(CapabilityUnavailable::BuildAbiUnavailable)
            } else if edition != SdkEdition::Advanced {
                CapabilityStatus::Unavailable(CapabilityUnavailable::WrongEdition)
            } else {
                exported(available)
            }
        };
        Self {
            planar_444_video: since(NDI_6_4),
            receive_best_444: since(NDI_6_4),
            receive_video_only: since(NDI_6_4),
            receive_delay: exported(api.receive_delay.is_available()),
            sdk_timestamp: exported(api.timestamp.is_available()),
            routing: exported(api.routing.is_available()),
            sender_advertising: exported(api.sender_advertising.is_available()),
            receiver_advertising: exported(api.receiver_advertising.is_available()),
            sender_discovery: exported(api.sender_discovery.is_available()),
            receiver_discovery: exported(api.receiver_discovery.is_available()),
            advanced_sender_configuration: advanced_entry(advanced.sender_configuration),
            advanced_receiver_configuration: advanced_entry(advanced.receiver_configuration),
        }
    }

    /// Query a capability without changing native state.
    pub fn status(&self, feature: RuntimeFeature) -> CapabilityStatus {
        match feature {
            RuntimeFeature::Planar444Video => self.planar_444_video,
            RuntimeFeature::ReceiveBest444 => self.receive_best_444,
            RuntimeFeature::ReceiveVideoOnly => self.receive_video_only,
            RuntimeFeature::ReceiveDelay => self.receive_delay,
            RuntimeFeature::SdkTimestamp => self.sdk_timestamp,
            RuntimeFeature::Routing => self.routing,
            RuntimeFeature::SenderAdvertising => self.sender_advertising,
            RuntimeFeature::ReceiverAdvertising => self.receiver_advertising,
            RuntimeFeature::SenderDiscovery => self.sender_discovery,
            RuntimeFeature::ReceiverDiscovery => self.receiver_discovery,
            RuntimeFeature::AdvancedSenderConfiguration => self.advanced_sender_configuration,
            RuntimeFeature::AdvancedReceiverConfiguration => self.advanced_receiver_configuration,
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

    fn detect(
        exported: impl Fn(&str) -> bool,
        version: Option<&str>,
        edition: SdkEdition,
    ) -> Capabilities {
        let api = Api::load(&crate::sys::table::ExportsWhere(exported)).unwrap();
        Capabilities::detect(
            &api,
            version.map(|text| SdkVersion::parse(text).unwrap()),
            edition,
            AdvancedEntryPoints {
                sender_configuration: edition == SdkEdition::Advanced,
                receiver_configuration: edition == SdkEdition::Advanced,
            },
        )
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
            RuntimeFeature::SdkTimestamp,
            RuntimeFeature::SenderAdvertising,
            RuntimeFeature::ReceiverAdvertising,
            RuntimeFeature::SenderDiscovery,
            RuntimeFeature::ReceiverDiscovery,
        ] {
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

    #[test]
    fn advanced_configuration_needs_the_build_abi_and_the_advanced_edition() {
        let features = [
            RuntimeFeature::AdvancedSenderConfiguration,
            RuntimeFeature::AdvancedReceiverConfiguration,
        ];
        let standard = detect(|_| true, Some("6.4.0.2"), SdkEdition::Standard);
        let advanced = detect(|_| true, Some("6.4.0.2"), SdkEdition::Advanced);
        for feature in features {
            if cfg!(all(feature = "advanced", ndi_has_advanced_header)) {
                assert_eq!(
                    standard.status(feature),
                    CapabilityStatus::Unavailable(CapabilityUnavailable::WrongEdition)
                );
                assert!(advanced.supports(feature));
            } else {
                for capabilities in [&standard, &advanced] {
                    assert_eq!(
                        capabilities.status(feature),
                        CapabilityStatus::Unavailable(CapabilityUnavailable::BuildAbiUnavailable)
                    );
                }
            }
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
