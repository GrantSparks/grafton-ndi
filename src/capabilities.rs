//! Facts about the selected native SDK, separate from hardware and license availability.

#![warn(missing_docs)]

use std::{
    fmt,
    path::{Path, PathBuf},
};

use crate::sys::{AdvancedEntryPoints, Api};

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
    /// Metadata a receiver replays to every new connection.
    ReceiverConnectionMetadata,
    /// The connected source's web-control URL.
    ReceiverWebControl,
    /// Notifications of the connected source's actual name.
    ConnectedSourceName,
    /// Metadata a sender captures from its connected receivers.
    SenderMetadata,
    /// The SDK current-timestamp operation.
    SdkTimestamp,
    /// Conversion between planar and interleaved audio.
    InterleavedAudioConversion,
    /// Direct submission of interleaved audio to a sender.
    InterleavedAudioSend,
    /// Conversion between the V210 and P216 video layouts.
    VideoConversion,
    /// Source routing, including its create/destroy and query operations.
    Routing,
    /// Discovery Server sender registration and removal. Needs NDI 6.4.0 or
    /// later: earlier runtimes can crash when an advertiser is destroyed.
    SenderAdvertising,
    /// Discovery Server receiver registration and removal. Needs NDI 6.4.0 or
    /// later: earlier runtimes can crash when an advertiser is destroyed.
    ReceiverAdvertising,
    /// Sender snapshots, subscriptions and owned event batches. Needs NDI
    /// 6.4.0 or later: earlier runtimes can crash when a listener is destroyed.
    SenderDiscovery,
    /// Receiver snapshots, subscriptions, event batches and connection control.
    /// Needs NDI 6.4.0 or later: earlier runtimes can crash when a listener is
    /// destroyed.
    ReceiverDiscovery,
    /// Creating a sender with Advanced JSON configuration.
    AdvancedSenderConfiguration,
    /// Creating a receiver with Advanced JSON configuration.
    AdvancedReceiverConfiguration,
}

impl RuntimeFeature {
    /// The one name the capability is reported under.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Planar444Video => "planar 4:4:4 video",
            Self::ReceiveBest444 => "Best444 receiver mode",
            Self::ReceiveVideoOnly => "video-only receiver bandwidth",
            Self::ReceiveDelay => "receiver audio/video delay",
            Self::ReceiverConnectionMetadata => "receiver connection metadata",
            Self::ReceiverWebControl => "receiver web control",
            Self::ConnectedSourceName => "connected source name",
            Self::SenderMetadata => "sender metadata",
            Self::SdkTimestamp => "SDK timestamp",
            Self::InterleavedAudioConversion => "interleaved audio conversion",
            Self::InterleavedAudioSend => "interleaved audio submission",
            Self::VideoConversion => "V210/P216 conversion",
            Self::Routing => "source routing",
            Self::SenderAdvertising => "sender advertising",
            Self::ReceiverAdvertising => "receiver advertising",
            Self::SenderDiscovery => "sender discovery",
            Self::ReceiverDiscovery => "receiver discovery",
            Self::AdvancedSenderConfiguration => "Advanced sender configuration",
            Self::AdvancedReceiverConfiguration => "Advanced receiver configuration",
        }
    }

    /// The first runtime version the capability may be used with, when the
    /// functions a runtime exports are not evidence enough.
    ///
    /// The 6.4 formats and receiver modes are enum values an older runtime
    /// would misread. The advertiser and listener functions exist in older
    /// runtimes, but the vendor's change log records crashes when destroying
    /// those objects that were fixed in 6.3.2 and again in 6.4.0.
    pub(crate) fn minimum_version(self) -> Option<SdkVersion> {
        match self {
            Self::Planar444Video
            | Self::ReceiveBest444
            | Self::ReceiveVideoOnly
            | Self::SenderAdvertising
            | Self::ReceiverAdvertising
            | Self::SenderDiscovery
            | Self::ReceiverDiscovery => Some(NDI_6_4),
            Self::ReceiveDelay
            | Self::ReceiverConnectionMetadata
            | Self::ReceiverWebControl
            | Self::ConnectedSourceName
            | Self::SenderMetadata
            | Self::SdkTimestamp
            | Self::InterleavedAudioConversion
            | Self::InterleavedAudioSend
            | Self::VideoConversion
            | Self::Routing
            | Self::AdvancedSenderConfiguration
            | Self::AdvancedReceiverConfiguration => None,
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
    receiver_connection_metadata: CapabilityStatus,
    receiver_web_control: CapabilityStatus,
    connected_source_name: CapabilityStatus,
    sender_metadata: CapabilityStatus,
    sdk_timestamp: CapabilityStatus,
    interleaved_audio_conversion: CapabilityStatus,
    interleaved_audio_send: CapabilityStatus,
    video_conversion: CapabilityStatus,
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
        // The order is the one an Advanced table that is its own gate reports
        // in, so that every Advanced capability is refused alike.
        let advanced_entry = |feature: RuntimeFeature, available: bool| {
            // The table the feature gates states which declarations it needs.
            #[cfg(feature = "advanced")]
            let declared = crate::sys::advanced::AdvancedApi::declares(feature);
            #[cfg(not(feature = "advanced"))]
            let declared = false;
            if !declared {
                CapabilityStatus::Unavailable(CapabilityUnavailable::BuildAbiUnavailable)
            } else if edition != SdkEdition::Advanced {
                CapabilityStatus::Unavailable(CapabilityUnavailable::WrongEdition)
            } else {
                exported(feature, available)
            }
        };
        let status = |feature: RuntimeFeature| match feature {
            RuntimeFeature::AdvancedSenderConfiguration => {
                advanced_entry(feature, advanced.sender_configuration)
            }
            RuntimeFeature::AdvancedReceiverConfiguration => {
                advanced_entry(feature, advanced.receiver_configuration)
            }
            // Which entry points a feature needs is stated with its table. A
            // feature without any of its own is only a version gate.
            _ => exported(feature, api.missing(feature).is_none()),
        };
        Self {
            planar_444_video: status(RuntimeFeature::Planar444Video),
            receive_best_444: status(RuntimeFeature::ReceiveBest444),
            receive_video_only: status(RuntimeFeature::ReceiveVideoOnly),
            receive_delay: status(RuntimeFeature::ReceiveDelay),
            receiver_connection_metadata: status(RuntimeFeature::ReceiverConnectionMetadata),
            receiver_web_control: status(RuntimeFeature::ReceiverWebControl),
            connected_source_name: status(RuntimeFeature::ConnectedSourceName),
            sender_metadata: status(RuntimeFeature::SenderMetadata),
            sdk_timestamp: status(RuntimeFeature::SdkTimestamp),
            interleaved_audio_conversion: status(RuntimeFeature::InterleavedAudioConversion),
            interleaved_audio_send: status(RuntimeFeature::InterleavedAudioSend),
            video_conversion: status(RuntimeFeature::VideoConversion),
            routing: status(RuntimeFeature::Routing),
            sender_advertising: status(RuntimeFeature::SenderAdvertising),
            receiver_advertising: status(RuntimeFeature::ReceiverAdvertising),
            sender_discovery: status(RuntimeFeature::SenderDiscovery),
            receiver_discovery: status(RuntimeFeature::ReceiverDiscovery),
            advanced_sender_configuration: status(RuntimeFeature::AdvancedSenderConfiguration),
            advanced_receiver_configuration: status(RuntimeFeature::AdvancedReceiverConfiguration),
        }
    }

    /// Query a capability without changing native state.
    pub fn status(&self, feature: RuntimeFeature) -> CapabilityStatus {
        match feature {
            RuntimeFeature::Planar444Video => self.planar_444_video,
            RuntimeFeature::ReceiveBest444 => self.receive_best_444,
            RuntimeFeature::ReceiveVideoOnly => self.receive_video_only,
            RuntimeFeature::ReceiveDelay => self.receive_delay,
            RuntimeFeature::ReceiverConnectionMetadata => self.receiver_connection_metadata,
            RuntimeFeature::ReceiverWebControl => self.receiver_web_control,
            RuntimeFeature::ConnectedSourceName => self.connected_source_name,
            RuntimeFeature::SenderMetadata => self.sender_metadata,
            RuntimeFeature::SdkTimestamp => self.sdk_timestamp,
            RuntimeFeature::InterleavedAudioConversion => self.interleaved_audio_conversion,
            RuntimeFeature::InterleavedAudioSend => self.interleaved_audio_send,
            RuntimeFeature::VideoConversion => self.video_conversion,
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

    #[test]
    fn advanced_configuration_needs_the_build_abi_and_the_advanced_edition() {
        let features = [
            RuntimeFeature::AdvancedSenderConfiguration,
            RuntimeFeature::AdvancedReceiverConfiguration,
        ];
        #[cfg(feature = "advanced")]
        let built = features.map(crate::sys::advanced::AdvancedApi::declares);
        #[cfg(not(feature = "advanced"))]
        let built = [false, false];
        let standard = detect(|_| true, Some("6.4.0.2"), SdkEdition::Standard);
        let advanced = detect(|_| true, Some("6.4.0.2"), SdkEdition::Advanced);
        for (feature, built) in features.into_iter().zip(built) {
            if built {
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

    /// `NDI::require_feature` and the table an Advanced configuration feature
    /// gates are one gate: for every reason the loader can give, both accept
    /// the feature or both refuse it with the same error.
    #[cfg(feature = "advanced")]
    #[test]
    fn advanced_configuration_tables_and_their_features_agree() {
        use crate::sys::{
            advanced::{declared, AdvancedApi, ReceiverConfiguration, SenderConfiguration},
            table::ExportsWhere,
            Evidence, Table,
        };
        use CapabilityUnavailable::{BuildAbiUnavailable, SymbolUnavailable, WrongEdition};

        const SENDER: &str = "NDIlib_send_create_v2";
        const RECEIVER: &str = "NDIlib_recv_create_v4";
        type Outcome = std::result::Result<(), (&'static str, CapabilityUnavailable, String)>;
        fn outcome<T>(result: crate::Result<T>) -> Outcome {
            match result {
                Ok(_) => Ok(()),
                Err(crate::Error::UnsupportedCapability {
                    capability,
                    reason,
                    detail,
                }) => Err((capability, reason, detail)),
                Err(other) => panic!("unexpected error: {other}"),
            }
        }

        let features = [
            (RuntimeFeature::AdvancedSenderConfiguration, SENDER),
            (RuntimeFeature::AdvancedReceiverConfiguration, RECEIVER),
        ];
        // Each feature states the family it gates once, in the table registry.
        assert_eq!(
            features.map(|(feature, _)| AdvancedApi::declares(feature)),
            [
                declared! { SenderConfiguration { true } else { false } },
                declared! { ReceiverConfiguration { true } else { false } },
            ]
        );
        // A shared runtime is an Advanced runtime when it exports both entry
        // points; a linked archive has the edition the build chose, and may
        // lack one of them.
        let scenarios: [(&[&str], SdkEdition); 6] = [
            (&[SENDER, RECEIVER], SdkEdition::Advanced),
            (&[], SdkEdition::Standard),
            (&[SENDER], SdkEdition::Standard),
            (&[RECEIVER], SdkEdition::Standard),
            (&[SENDER], SdkEdition::Advanced),
            (&[RECEIVER], SdkEdition::Advanced),
        ];
        let mut reached = Vec::new();
        for (exported, edition) in scenarios {
            let resolver = ExportsWhere(|name: &str| {
                ![SENDER, RECEIVER].contains(&name) || exported.contains(&name)
            });
            let api = Api::load(&resolver).unwrap();
            let advanced = AdvancedApi::load(&resolver).unwrap();
            let info = RuntimeInfo {
                edition,
                linkage: SdkLinkage::Shared,
                library_path: None,
                raw_version: "NDI SDK TEST 6.4.0.2".into(),
                version: SdkVersion::parse("6.4.0.2"),
            };
            let capabilities = Capabilities::detect(
                &api,
                info.version,
                edition,
                AdvancedEntryPoints {
                    sender_configuration: exported.contains(&SENDER),
                    receiver_configuration: exported.contains(&RECEIVER),
                },
            );
            let evidence = Evidence {
                api: &api,
                advanced: &advanced,
                info: &info,
                capabilities: &capabilities,
            };
            let tables = [
                outcome(SenderConfiguration::within(evidence)),
                outcome(ReceiverConfiguration::within(evidence)),
            ];
            for ((feature, entry_point), table) in features.into_iter().zip(tables) {
                let context = format!("{feature:?} with {exported:?} exported by {edition:?}");
                let status = capabilities.status(feature);
                let required = outcome(evidence.require(feature));
                assert_eq!(table, required, "{context}");
                let expected = match status {
                    CapabilityStatus::Available => Ok(()),
                    CapabilityStatus::Unavailable(reason) => {
                        // The evidence is the entry point the runtime lacks,
                        // or the declaration the build lacks; a runtime that
                        // exports it but is not Advanced has none to give.
                        let lacks =
                            reason == BuildAbiUnavailable || !exported.contains(&entry_point);
                        let detail = if lacks { entry_point } else { "" };
                        Err((feature.name(), reason, detail.to_owned()))
                    }
                };
                assert_eq!(required, expected, "{context}");
                reached.push((feature, status));
            }
        }
        for (feature, _) in features {
            let expected: &[_] = if AdvancedApi::declares(feature) {
                &[
                    CapabilityStatus::Available,
                    CapabilityStatus::Unavailable(WrongEdition),
                    CapabilityStatus::Unavailable(SymbolUnavailable),
                ]
            } else {
                &[CapabilityStatus::Unavailable(BuildAbiUnavailable)]
            };
            for status in expected {
                assert!(
                    reached.contains(&(feature, *status)),
                    "{feature:?}: {status:?}"
                );
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
