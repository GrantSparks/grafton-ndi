//! Facts about the selected native SDK, separate from hardware and license availability.

use std::path::{Path, PathBuf};

#[allow(clippy::type_complexity)]
mod generated {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/api/capabilities.rs"));
}

type MinimumVersion = (u16, u16, u16);
type Requirements = (&'static [&'static str], Option<MinimumVersion>);

pub(crate) fn requirements(id: &str) -> Option<Requirements> {
    generated::CAPABILITY_REQUIREMENTS
        .iter()
        .find(|entry| entry.0 == id)
        .map(|entry| (entry.2, entry.3))
}

/// The native SDK edition selected for this process.
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

    // Observed SDK strings end in a version, after platform and build date/time.
    // Reject arbitrary embedded numbers and future major versions for semantic gates.
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

    pub(crate) fn supports_semantics(self, id: &str) -> bool {
        let Some((_, Some((major, minor, patch)))) = requirements(id) else {
            return false;
        };
        self.major == u32::from(major)
            && (self.minor, self.patch) >= (u32::from(minor), u32::from(patch))
    }
}

/// Identity of the implementation actually selected, rather than just build headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub(crate) edition: SdkEdition,
    pub(crate) edition_verified: bool,
    pub(crate) linkage: SdkLinkage,
    pub(crate) library_path: Option<PathBuf>,
    pub(crate) raw_version: String,
    pub(crate) version: Option<SdkVersion>,
}

impl RuntimeInfo {
    /// Selected SDK edition. Check [`Self::edition_verified`] before using this
    /// as evidence for edition-dependent companion libraries or ABI semantics.
    pub fn edition(&self) -> SdkEdition {
        self.edition
    }
    /// Whether the selected edition has positive ABI/artifact evidence.
    ///
    /// Older Advanced libraries with recognizable filenames but no current
    /// edition markers may still supply the legacy API. They report `false`;
    /// the selection label alone must not enable edition-dependent features.
    /// Neither result establishes a license entitlement.
    pub fn edition_verified(&self) -> bool {
        self.edition_verified
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
    fn registry_id(self) -> &'static str {
        match self {
            Self::Planar444Video => "planar_444",
            Self::ReceiveBest444 => "best_444",
            Self::ReceiveVideoOnly => "video_only_receive",
            Self::ReceiveDelay => "receive_delay",
            Self::SdkTimestamp => "sdk_timestamp",
            Self::Routing => "routing",
            Self::SenderAdvertising => "send_advertiser",
            Self::ReceiverAdvertising => "recv_advertiser",
            Self::SenderDiscovery => "send_listener",
            Self::ReceiverDiscovery => "recv_listener",
            Self::AdvancedSenderConfiguration => "send_create_v2",
            Self::AdvancedReceiverConfiguration => "recv_create_v4",
        }
    }

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
    /// This capability is not part of the selected runtime version.
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
    pub(crate) version: Option<SdkVersion>,
    pub(crate) receive_delay: bool,
    pub(crate) timestamp: bool,
    pub(crate) groups: [CapabilityStatus; 7],
}

impl Capabilities {
    /// Query a capability without changing native state.
    pub fn status(&self, feature: RuntimeFeature) -> CapabilityStatus {
        match feature {
            RuntimeFeature::Routing => self.groups[0],
            RuntimeFeature::SenderAdvertising => self.groups[1],
            RuntimeFeature::ReceiverAdvertising => self.groups[2],
            RuntimeFeature::SenderDiscovery => self.groups[3],
            RuntimeFeature::ReceiverDiscovery => self.groups[4],
            RuntimeFeature::AdvancedSenderConfiguration => self.groups[5],
            RuntimeFeature::AdvancedReceiverConfiguration => self.groups[6],
            RuntimeFeature::ReceiveDelay => {
                if self.receive_delay {
                    CapabilityStatus::Available
                } else {
                    CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable)
                }
            }
            RuntimeFeature::SdkTimestamp => {
                if self.timestamp {
                    CapabilityStatus::Available
                } else {
                    CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable)
                }
            }
            _ => match self.version {
                Some(v) if v.supports_semantics(feature.registry_id()) => {
                    CapabilityStatus::Available
                }
                Some(v)
                    if requirements(feature.registry_id())
                        .and_then(|(_, minimum)| minimum)
                        .is_some_and(|minimum| v.major() <= u32::from(minimum.0)) =>
                {
                    CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeTooOld)
                }
                Some(_) => {
                    CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeVersionUnknown)
                }
                None => CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeVersionUnknown),
            },
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
        assert!(v.supports_semantics("planar_444"));
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
        assert!(!SdkVersion::parse("7.0.0")
            .unwrap()
            .supports_semantics("planar_444"));
    }
    #[test]
    fn public_features_have_reviewed_descriptors() {
        for feature in [
            RuntimeFeature::Planar444Video,
            RuntimeFeature::ReceiveBest444,
            RuntimeFeature::ReceiveVideoOnly,
            RuntimeFeature::ReceiveDelay,
            RuntimeFeature::SdkTimestamp,
            RuntimeFeature::Routing,
            RuntimeFeature::SenderAdvertising,
            RuntimeFeature::ReceiverAdvertising,
            RuntimeFeature::SenderDiscovery,
            RuntimeFeature::ReceiverDiscovery,
            RuntimeFeature::AdvancedSenderConfiguration,
            RuntimeFeature::AdvancedReceiverConfiguration,
        ] {
            assert!(requirements(feature.registry_id()).is_some(), "{feature:?}");
        }
    }

    #[test]
    fn unknown_identity_does_not_claim_enum_semantics() {
        let c = Capabilities {
            version: None,
            receive_delay: true,
            timestamp: false,
            groups: [CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable); 7],
        };
        assert!(c.supports(RuntimeFeature::ReceiveDelay));
        assert_eq!(
            c.status(RuntimeFeature::ReceiveBest444),
            CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeVersionUnknown)
        );
        assert!(!c.supports(RuntimeFeature::SdkTimestamp));
    }
}
