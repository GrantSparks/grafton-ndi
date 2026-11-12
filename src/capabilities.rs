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
    /// Creating a finder with Advanced JSON configuration.
    AdvancedFinderConfiguration,
    /// Creating a sender with Advanced JSON configuration.
    AdvancedSenderConfiguration,
    /// Creating a receiver with Advanced JSON configuration.
    AdvancedReceiverConfiguration,
    /// Creating a router with Advanced JSON configuration.
    AdvancedRouterConfiguration,
    /// Creating a Discovery Server sender advertiser with Advanced JSON
    /// configuration. Needs NDI 6.4.0 or later, as
    /// [`SenderAdvertising`](Self::SenderAdvertising) does, whose functions
    /// the advertiser goes on to use.
    AdvancedSenderAdvertiserConfiguration,
    /// Creating a Discovery Server receiver advertiser with Advanced JSON
    /// configuration. Needs NDI 6.4.0 or later, as
    /// [`ReceiverAdvertising`](Self::ReceiverAdvertising) does, whose
    /// functions the advertiser goes on to use.
    AdvancedReceiverAdvertiserConfiguration,
    /// Creating a Discovery Server sender listener with Advanced JSON
    /// configuration. Needs NDI 6.4.0 or later, as
    /// [`SenderDiscovery`](Self::SenderDiscovery) does, whose functions the
    /// listener goes on to use.
    AdvancedSenderListenerConfiguration,
    /// Creating a Discovery Server receiver listener with Advanced JSON
    /// configuration. Needs NDI 6.4.0 or later, as
    /// [`ReceiverDiscovery`](Self::ReceiverDiscovery) does, whose functions
    /// the listener goes on to use.
    AdvancedReceiverListenerConfiguration,
    /// Source snapshots that carry each source's metadata.
    SourceMetadata,
    /// Metadata a router replays to each new connection.
    RouterConnectionMetadata,
    /// A sender's own contribution to its tally state.
    SenderTally,
    /// The target frame size, bit rate and quality for an encoder feeding a
    /// sender.
    EncoderGuidance,
    /// Encoder quality together with the formats the connected receivers
    /// accept.
    EncoderQualityPlanning,
    /// Whether the receivers connected to a sender need a keyframe, and
    /// waiting for one to ask for it.
    KeyframeDemand,
    /// A receiver's request for a keyframe from its source.
    KeyframeRequest,
    /// Sending compressed frames held in several blocks.
    ScatterSend,
    /// A receiver's choice between its source's program and preview streams.
    ReceiverStreamTier,
    /// A receiver's choice of which of its source's streams it receives.
    ReceiverStreamSelection,
    /// Application-supplied memory for the frames a receiver delivers.
    ReceiveAllocators,
    /// Notification that the SDK has finished with an asynchronously sent
    /// video frame.
    SendCompletion,
    /// Matching a receiver's audio to its video timestamps.
    AvSync,
    /// Clocks locked to a reference source.
    Genlock,
    /// Keyboard, mouse, touch and clipboard control of a source.
    Kvm,
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
            Self::AdvancedFinderConfiguration => "Advanced finder configuration",
            Self::AdvancedSenderConfiguration => "Advanced sender configuration",
            Self::AdvancedReceiverConfiguration => "Advanced receiver configuration",
            Self::AdvancedRouterConfiguration => "Advanced router configuration",
            Self::AdvancedSenderAdvertiserConfiguration => {
                "Advanced sender advertiser configuration"
            }
            Self::AdvancedReceiverAdvertiserConfiguration => {
                "Advanced receiver advertiser configuration"
            }
            Self::AdvancedSenderListenerConfiguration => "Advanced sender listener configuration",
            Self::AdvancedReceiverListenerConfiguration => {
                "Advanced receiver listener configuration"
            }
            Self::SourceMetadata => "source metadata",
            Self::RouterConnectionMetadata => "router connection metadata",
            Self::SenderTally => "sender tally override",
            Self::EncoderGuidance => "encoder guidance",
            Self::EncoderQualityPlanning => "encoder quality planning",
            Self::KeyframeDemand => "keyframe demand",
            Self::KeyframeRequest => "keyframe request",
            Self::ScatterSend => "scatter sending",
            Self::ReceiverStreamTier => "receiver stream tier",
            Self::ReceiverStreamSelection => "receiver stream selection",
            Self::ReceiveAllocators => "custom receive allocators",
            Self::SendCompletion => "asynchronous send completion",
            Self::AvSync => "AV synchronization",
            Self::Genlock => "genlock",
            Self::Kvm => "KVM control",
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
            Self::Planar444Video
            | Self::ReceiveBest444
            | Self::ReceiveVideoOnly
            | Self::SenderAdvertising
            | Self::ReceiverAdvertising
            | Self::SenderDiscovery
            | Self::ReceiverDiscovery
            | Self::AdvancedSenderAdvertiserConfiguration
            | Self::AdvancedReceiverAdvertiserConfiguration
            | Self::AdvancedSenderListenerConfiguration
            | Self::AdvancedReceiverListenerConfiguration => Some(NDI_6_4),
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
            | Self::AdvancedFinderConfiguration
            | Self::AdvancedSenderConfiguration
            | Self::AdvancedReceiverConfiguration
            | Self::AdvancedRouterConfiguration
            | Self::SourceMetadata
            | Self::RouterConnectionMetadata
            | Self::SenderTally
            | Self::EncoderGuidance
            | Self::EncoderQualityPlanning
            | Self::KeyframeDemand
            | Self::KeyframeRequest
            | Self::ScatterSend
            | Self::ReceiverStreamTier
            | Self::ReceiverStreamSelection
            | Self::ReceiveAllocators
            | Self::SendCompletion
            | Self::AvSync
            | Self::Genlock
            | Self::Kvm => None,
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
            Self::Planar444Video
            | Self::ReceiveBest444
            | Self::ReceiveVideoOnly
            | Self::ReceiveDelay
            | Self::ReceiverConnectionMetadata
            | Self::ReceiverWebControl
            | Self::ConnectedSourceName
            | Self::SenderMetadata
            | Self::SdkTimestamp
            | Self::InterleavedAudioConversion
            | Self::InterleavedAudioSend
            | Self::VideoConversion
            | Self::Routing
            | Self::SenderAdvertising
            | Self::ReceiverAdvertising
            | Self::SenderDiscovery
            | Self::ReceiverDiscovery => false,
            Self::AdvancedFinderConfiguration
            | Self::AdvancedSenderConfiguration
            | Self::AdvancedReceiverConfiguration
            | Self::AdvancedRouterConfiguration
            | Self::AdvancedSenderAdvertiserConfiguration
            | Self::AdvancedReceiverAdvertiserConfiguration
            | Self::AdvancedSenderListenerConfiguration
            | Self::AdvancedReceiverListenerConfiguration
            | Self::SourceMetadata
            | Self::RouterConnectionMetadata
            | Self::SenderTally
            | Self::EncoderGuidance
            | Self::EncoderQualityPlanning
            | Self::KeyframeDemand
            | Self::KeyframeRequest
            | Self::ScatterSend
            | Self::ReceiverStreamTier
            | Self::ReceiverStreamSelection
            | Self::ReceiveAllocators
            | Self::SendCompletion
            | Self::AvSync
            | Self::Genlock
            | Self::Kvm => true,
            #[cfg(feature = "advanced")]
            Self::Passthrough(_) => true,
        }
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    advanced_finder_configuration: CapabilityStatus,
    advanced_sender_configuration: CapabilityStatus,
    advanced_receiver_configuration: CapabilityStatus,
    advanced_router_configuration: CapabilityStatus,
    advanced_sender_advertiser_configuration: CapabilityStatus,
    advanced_receiver_advertiser_configuration: CapabilityStatus,
    advanced_sender_listener_configuration: CapabilityStatus,
    advanced_receiver_listener_configuration: CapabilityStatus,
    source_metadata: CapabilityStatus,
    router_connection_metadata: CapabilityStatus,
    sender_tally: CapabilityStatus,
    encoder_guidance: CapabilityStatus,
    encoder_quality_planning: CapabilityStatus,
    keyframe_demand: CapabilityStatus,
    keyframe_request: CapabilityStatus,
    scatter_send: CapabilityStatus,
    receiver_stream_tier: CapabilityStatus,
    receiver_stream_selection: CapabilityStatus,
    receive_allocators: CapabilityStatus,
    send_completion: CapabilityStatus,
    av_sync: CapabilityStatus,
    genlock: CapabilityStatus,
    kvm: CapabilityStatus,
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
            advanced_finder_configuration: status(RuntimeFeature::AdvancedFinderConfiguration),
            advanced_sender_configuration: status(RuntimeFeature::AdvancedSenderConfiguration),
            advanced_receiver_configuration: status(RuntimeFeature::AdvancedReceiverConfiguration),
            advanced_router_configuration: status(RuntimeFeature::AdvancedRouterConfiguration),
            advanced_sender_advertiser_configuration: status(
                RuntimeFeature::AdvancedSenderAdvertiserConfiguration,
            ),
            advanced_receiver_advertiser_configuration: status(
                RuntimeFeature::AdvancedReceiverAdvertiserConfiguration,
            ),
            advanced_sender_listener_configuration: status(
                RuntimeFeature::AdvancedSenderListenerConfiguration,
            ),
            advanced_receiver_listener_configuration: status(
                RuntimeFeature::AdvancedReceiverListenerConfiguration,
            ),
            source_metadata: status(RuntimeFeature::SourceMetadata),
            router_connection_metadata: status(RuntimeFeature::RouterConnectionMetadata),
            sender_tally: status(RuntimeFeature::SenderTally),
            encoder_guidance: status(RuntimeFeature::EncoderGuidance),
            encoder_quality_planning: status(RuntimeFeature::EncoderQualityPlanning),
            keyframe_demand: status(RuntimeFeature::KeyframeDemand),
            keyframe_request: status(RuntimeFeature::KeyframeRequest),
            scatter_send: status(RuntimeFeature::ScatterSend),
            receiver_stream_tier: status(RuntimeFeature::ReceiverStreamTier),
            receiver_stream_selection: status(RuntimeFeature::ReceiverStreamSelection),
            receive_allocators: status(RuntimeFeature::ReceiveAllocators),
            send_completion: status(RuntimeFeature::SendCompletion),
            av_sync: status(RuntimeFeature::AvSync),
            genlock: status(RuntimeFeature::Genlock),
            kvm: status(RuntimeFeature::Kvm),
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
            RuntimeFeature::AdvancedFinderConfiguration => self.advanced_finder_configuration,
            RuntimeFeature::AdvancedSenderConfiguration => self.advanced_sender_configuration,
            RuntimeFeature::AdvancedReceiverConfiguration => self.advanced_receiver_configuration,
            RuntimeFeature::AdvancedRouterConfiguration => self.advanced_router_configuration,
            RuntimeFeature::AdvancedSenderAdvertiserConfiguration => {
                self.advanced_sender_advertiser_configuration
            }
            RuntimeFeature::AdvancedReceiverAdvertiserConfiguration => {
                self.advanced_receiver_advertiser_configuration
            }
            RuntimeFeature::AdvancedSenderListenerConfiguration => {
                self.advanced_sender_listener_configuration
            }
            RuntimeFeature::AdvancedReceiverListenerConfiguration => {
                self.advanced_receiver_listener_configuration
            }
            RuntimeFeature::SourceMetadata => self.source_metadata,
            RuntimeFeature::RouterConnectionMetadata => self.router_connection_metadata,
            RuntimeFeature::SenderTally => self.sender_tally,
            RuntimeFeature::EncoderGuidance => self.encoder_guidance,
            RuntimeFeature::EncoderQualityPlanning => self.encoder_quality_planning,
            RuntimeFeature::KeyframeDemand => self.keyframe_demand,
            RuntimeFeature::KeyframeRequest => self.keyframe_request,
            RuntimeFeature::ScatterSend => self.scatter_send,
            RuntimeFeature::ReceiverStreamTier => self.receiver_stream_tier,
            RuntimeFeature::ReceiverStreamSelection => self.receiver_stream_selection,
            RuntimeFeature::ReceiveAllocators => self.receive_allocators,
            RuntimeFeature::SendCompletion => self.send_completion,
            RuntimeFeature::AvSync => self.av_sync,
            RuntimeFeature::Genlock => self.genlock,
            RuntimeFeature::Kvm => self.kvm,
            // A preset adds a declaration to the configured receiver it is
            // created as; the rest of its status is that receiver's.
            #[cfg(feature = "advanced")]
            RuntimeFeature::Passthrough(_) => {
                if AdvancedApi::declares(feature) {
                    self.advanced_receiver_configuration
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

    /// Every passthrough preset.
    #[cfg(feature = "advanced")]
    const PRESETS: [PassthroughPreset; 15] = {
        use PassthroughPreset::*;
        [
            SpeedHqOnly,
            SpeedHq,
            SpeedHqBest,
            H264,
            H264Best,
            H264WithAudio,
            Hevc,
            HevcBest,
            HevcWithAudio,
            Alpha,
            AlphaBest,
            AlphaWithAudio,
            Protected,
            ProtectedBest,
            ProtectedWithAudio,
        ]
    };

    /// Every feature, with each passthrough preset in builds that have them.
    fn every_feature() -> Vec<RuntimeFeature> {
        use RuntimeFeature::*;
        let features = vec![
            Planar444Video,
            ReceiveBest444,
            ReceiveVideoOnly,
            ReceiveDelay,
            ReceiverConnectionMetadata,
            ReceiverWebControl,
            ConnectedSourceName,
            SenderMetadata,
            SdkTimestamp,
            InterleavedAudioConversion,
            InterleavedAudioSend,
            VideoConversion,
            Routing,
            SenderAdvertising,
            ReceiverAdvertising,
            SenderDiscovery,
            ReceiverDiscovery,
            AdvancedFinderConfiguration,
            AdvancedSenderConfiguration,
            AdvancedReceiverConfiguration,
            AdvancedRouterConfiguration,
            AdvancedSenderAdvertiserConfiguration,
            AdvancedReceiverAdvertiserConfiguration,
            AdvancedSenderListenerConfiguration,
            AdvancedReceiverListenerConfiguration,
            SourceMetadata,
            RouterConnectionMetadata,
            SenderTally,
            EncoderGuidance,
            EncoderQualityPlanning,
            KeyframeDemand,
            KeyframeRequest,
            ScatterSend,
            ReceiverStreamTier,
            ReceiverStreamSelection,
            ReceiveAllocators,
            SendCompletion,
            AvSync,
            Genlock,
            Kvm,
        ];
        #[cfg(feature = "advanced")]
        let features = features
            .into_iter()
            .chain(PRESETS.map(Passthrough))
            .collect();
        features
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
        for preset in PRESETS {
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
