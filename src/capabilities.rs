//! Facts about the selected native SDK, separate from hardware and license availability.

#![warn(missing_docs)]

use std::{
    ffi::CStr,
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
        let mut components = token.split('.');
        let parse = |text: &str| {
            if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                None
            } else {
                text.parse().ok()
            }
        };
        let version = Self {
            major: parse(components.next()?)?,
            minor: parse(components.next()?)?,
            patch: parse(components.next()?)?,
            build: components.next().map_or(Some(0), parse)?,
        };
        components.next().is_none().then_some(version)
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
/// the `advanced` SDK and the name it is reported under, then any of:
///
/// - `since`, the first runtime version it may be used with;
/// - `requires`, the capabilities whose tables its operations also take, each
///   listed before it. Once its own gate passes, the capability reports the
///   status, and the evidence, of the first of them that is unavailable, so
///   its operations are refused under the capability itself rather than under
///   the one they reach later;
/// - `ungated`, with the reason, for a core capability that every runtime
///   provides.
///
/// A core capability is gated by its table in `optional_tables!`, by `since`
/// or by `requires`. One with none of them would be reported available on
/// every runtime, so it fails to compile unless it says `ungated`, and only
/// such a one may (see `Api::missing`).
///
/// The enum, its `name`, `minimum_version`, `requirements` and `is_advanced`,
/// [`RuntimeFeature::UNIT`] and the place [`Capabilities`] keeps each status
/// are all generated from the list, so none of them can leave a capability
/// out. The variant with a field, `Passthrough`, is written out here.
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
    (@ungated $edition:ident) => {
        false
    };
    (@ungated core $why:literal) => {
        true
    };
    (@ungated advanced $why:literal) => {
        compile_error!("an Advanced capability is always gated by its table")
    };
    ($(
        $(#[$meta:meta])*
        $edition:ident $Variant:ident as $name:literal
            $(since $minimum:ident)?
            $(requires $($required:ident),+)?
            $(ungated $why:literal)?;
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
        /// build with the `advanced` feature whose SDK headers declare it. A
        /// capability whose operations go on to use another one's functions, such
        /// as [`AdvancedRouterConfiguration`](Self::AdvancedRouterConfiguration)
        /// with [`Routing`](Self::Routing), is also unavailable whenever that one
        /// is: once its own requirements are met, it reports that one's reason,
        /// and its operations are refused with that one's evidence.
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

        // A capability requires only capabilities listed before it, so no
        // requirement is circular and `Capabilities::detect` finds each
        // requirement's status already derived.
        $($($(
            const _: () = assert!(
                (Index::$required as usize) < (Index::$Variant as usize),
                concat!(
                    stringify!($Variant), " requires ", stringify!($required),
                    ", which must be listed before it"
                ),
            );
        )+)?)+

        /// Where [`Capabilities`] finds a feature's status.
        enum Slot {
            /// At this index of the statuses it keeps.
            Unit(usize),
            /// Derived from the configured receiver's, for a passthrough preset.
            #[cfg(feature = "advanced")]
            Passthrough(PassthroughPreset),
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
                    Self::Passthrough(preset) => Slot::Passthrough(preset),
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
            /// those objects that were fixed in 6.3.2 and again in 6.4.0. An object
            /// created with Advanced configuration is destroyed the same way, so its
            /// capability requires the core one and reports that one's version gate.
            pub(crate) const fn minimum_version(self) -> Option<SdkVersion> {
                match self {
                    $(Self::$Variant => runtime_features!(@minimum $($minimum)?),)+
                    #[cfg(feature = "advanced")]
                    Self::Passthrough(_) => None,
                }
            }

            /// The capabilities whose tables the capability's operations also
            /// take, in the order they are checked. The capability is
            /// unavailable whenever one of them is, and then reports the status
            /// of the first one that is.
            pub(crate) const fn requirements(self) -> &'static [Self] {
                match self {
                    $(Self::$Variant => &[$($(Self::$required),+)?],)+
                    #[cfg(feature = "advanced")]
                    Self::Passthrough(_) => &[],
                }
            }

            /// Whether the capability is declared `ungated`: a core capability
            /// without a table, a minimum version or a requirement, which every
            /// runtime provides.
            pub(crate) const fn is_ungated(self) -> bool {
                match self {
                    $(Self::$Variant => runtime_features!(@ungated $edition $($why)?),)+
                    #[cfg(feature = "advanced")]
                    Self::Passthrough(_) => false,
                }
            }

            /// Whether the capability belongs to the Advanced SDK.
            ///
            /// Each such capability is gated by a table of Advanced entry points,
            /// which states, with the build's declarations, what the capability
            /// needs.
            pub(crate) const fn is_advanced(self) -> bool {
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
    /// Creating a router with Advanced JSON configuration. Needs
    /// [`Routing`](Self::Routing), whose functions the router goes on to use.
    advanced AdvancedRouterConfiguration as "Advanced router configuration"
        requires Routing;
    /// Creating a Discovery Server sender advertiser with Advanced JSON
    /// configuration. Needs [`SenderAdvertising`](Self::SenderAdvertising),
    /// whose functions the advertiser goes on to use, and so NDI 6.4.0 or
    /// later.
    advanced AdvancedSenderAdvertiserConfiguration as "Advanced sender advertiser configuration"
        requires SenderAdvertising;
    /// Creating a Discovery Server receiver advertiser with Advanced JSON
    /// configuration. Needs
    /// [`ReceiverAdvertising`](Self::ReceiverAdvertising), whose functions
    /// the advertiser goes on to use, and so NDI 6.4.0 or later.
    advanced AdvancedReceiverAdvertiserConfiguration as "Advanced receiver advertiser configuration"
        requires ReceiverAdvertising;
    /// Creating a Discovery Server sender listener with Advanced JSON
    /// configuration. Needs [`SenderDiscovery`](Self::SenderDiscovery), whose
    /// functions the listener goes on to use, and so NDI 6.4.0 or later.
    advanced AdvancedSenderListenerConfiguration as "Advanced sender listener configuration"
        requires SenderDiscovery;
    /// Creating a Discovery Server receiver listener with Advanced JSON
    /// configuration. Needs [`ReceiverDiscovery`](Self::ReceiverDiscovery),
    /// whose functions the listener goes on to use, and so NDI 6.4.0 or later.
    advanced AdvancedReceiverListenerConfiguration as "Advanced receiver listener configuration"
        requires ReceiverDiscovery;
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
            Self::SymbolUnavailable => "the selected library does not export a required function",
            Self::BuildAbiUnavailable => {
                "this build was made without the Advanced SDK declarations"
            }
            Self::WrongEdition => "the selected runtime is not an Advanced SDK runtime",
        })
    }
}

/// The evidence that prevents a capability from being used.
///
/// Detection records this value once. Capability queries and operation errors
/// carry that same value, including when another capability requires it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CapabilityRefusal {
    /// The selected library does not export this required entry point.
    SymbolUnavailable(&'static CStr),
    /// This build's SDK declarations do not contain this entry point or constant.
    BuildAbiUnavailable(&'static CStr),
    /// The runtime is not Advanced, optionally naming an entry point it lacks.
    WrongEdition(Option<&'static CStr>),
    /// The runtime predates the required semantic version.
    RuntimeTooOld {
        /// The first version that may safely implement this capability.
        required: SdkVersion,
    },
    /// The runtime version cannot establish the required semantics.
    RuntimeVersionUnknown {
        /// The first version that may safely implement this capability.
        required: SdkVersion,
    },
}

impl CapabilityRefusal {
    /// The category of this refusal, without discarding its typed evidence.
    pub const fn reason(self) -> CapabilityUnavailable {
        match self {
            Self::SymbolUnavailable(_) => CapabilityUnavailable::SymbolUnavailable,
            Self::BuildAbiUnavailable(_) => CapabilityUnavailable::BuildAbiUnavailable,
            Self::WrongEdition(_) => CapabilityUnavailable::WrongEdition,
            Self::RuntimeTooOld { .. } => CapabilityUnavailable::RuntimeTooOld,
            Self::RuntimeVersionUnknown { .. } => CapabilityUnavailable::RuntimeVersionUnknown,
        }
    }
}

impl fmt::Display for CapabilityRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.reason())?;
        match self {
            Self::SymbolUnavailable(symbol) => write!(f, ": {}", symbol.to_string_lossy()),
            Self::BuildAbiUnavailable(symbol) => {
                write!(f, ": it does not declare {}", symbol.to_string_lossy())
            }
            Self::WrongEdition(Some(symbol)) => {
                write!(f, ": it does not export {}", symbol.to_string_lossy())
            }
            Self::WrongEdition(None) => Ok(()),
            Self::RuntimeTooOld { required } | Self::RuntimeVersionUnknown { required } => {
                write!(f, ": it needs NDI {required} or later")
            }
        }
    }
}

/// Availability established from the selected implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CapabilityStatus {
    /// The wrapper has the required ABI/semantic evidence. Hardware and licenses are separate.
    Available,
    /// The operation must be rejected before calling the SDK, for this recorded evidence.
    Unavailable(CapabilityRefusal),
}

impl CapabilityStatus {
    /// The refusal category, when the capability is unavailable.
    pub const fn reason(self) -> Option<CapabilityUnavailable> {
        match self {
            Self::Available => None,
            Self::Unavailable(refusal) => Some(refusal.reason()),
        }
    }

    fn from_decision(decision: std::result::Result<(), CapabilityRefusal>) -> Self {
        match decision {
            Ok(()) => Self::Available,
            Err(refusal) => Self::Unavailable(refusal),
        }
    }
}

// The registry names the missing declaration even in builds whose public API
// omits Advanced wrappers. No ABI types from those headers are needed here.
#[cfg(not(feature = "advanced"))]
fn absent_advanced_abi(feature: RuntimeFeature) -> Option<&'static CStr> {
    macro_rules! advanced_tables {
        ($( $(#[$meta:meta])* struct $Table:ident in $field:ident: $Feature:ident {
            fn $first:ident($($argument:ident: $type:ty),* $(,)?) $(-> $return:ty)?;
            $($rest:tt)*
        })+) => {
            match feature {
                $(RuntimeFeature::$Feature => Some(crate::sys::table::symbol_name(
                    concat!(stringify!($first), "\0"),
                )),)+
                _ => None,
            }
        };
    }
    include!("sys/advanced_tables.rs")
}

/// What the selected implementation, together with this build, provides:
/// one recorded decision for every [`RuntimeFeature`].
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Capabilities {
    decisions: [std::result::Result<(), CapabilityRefusal>; RuntimeFeature::UNIT.len()],
    #[cfg(feature = "advanced")]
    presets:
        [std::result::Result<crate::sys::bindings::NDIlib_recv_color_format_e, CapabilityRefusal>;
            PassthroughPreset::ALL.len()],
}

impl fmt::Debug for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(RuntimeFeature::UNIT.map(|feature| (feature, self.status(feature))))
            .finish()
    }
}

impl Capabilities {
    /// Decide each capability once, preserving its first refusal and evidence.
    pub(crate) fn detect(
        api: &Api,
        #[cfg(feature = "advanced")] advanced: &AdvancedApi,
        version: Option<SdkVersion>,
        edition: SdkEdition,
    ) -> Self {
        let versioned = |feature: RuntimeFeature| match (feature.minimum_version(), version) {
            (None, _) => Ok(()),
            (Some(required), Some(version)) if version >= required => Ok(()),
            (Some(required), Some(_)) => Err(CapabilityRefusal::RuntimeTooOld { required }),
            (Some(required), None) => Err(CapabilityRefusal::RuntimeVersionUnknown { required }),
        };
        let own = |feature: RuntimeFeature| {
            if !feature.is_advanced() {
                return api.missing(feature).map_or_else(|| versioned(feature), Err);
            }
            #[cfg(feature = "advanced")]
            let (declared, missing) = (AdvancedApi::declares(feature), advanced.missing(feature));
            #[cfg(not(feature = "advanced"))]
            let (declared, missing) = (
                false,
                absent_advanced_abi(feature).map(CapabilityRefusal::BuildAbiUnavailable),
            );
            if !declared {
                // Missing declarations are recorded by the same table registry
                // that declares the feature, including a build without wrappers.
                return Err(missing.expect("an undeclared Advanced table names its missing ABI"));
            }
            if edition != SdkEdition::Advanced {
                return Err(CapabilityRefusal::WrongEdition(missing.and_then(
                    |refusal| match refusal {
                        CapabilityRefusal::SymbolUnavailable(symbol) => Some(symbol),
                        _ => None,
                    },
                )));
            }
            missing.map_or_else(|| versioned(feature), Err)
        };
        let mut capabilities = Self {
            decisions: [Ok(()); RuntimeFeature::UNIT.len()],
            #[cfg(feature = "advanced")]
            presets: [Ok(0); PassthroughPreset::ALL.len()],
        };
        for (index, feature) in RuntimeFeature::UNIT.into_iter().enumerate() {
            capabilities.decisions[index] = own(feature).and_then(|()| {
                feature
                    .requirements()
                    .iter()
                    .try_for_each(|&required| capabilities.decision(required))
            });
        }
        #[cfg(feature = "advanced")]
        {
            // A preset first needs its own constant, then exactly the configured
            // receiver's decision. Its usable format is retained beside it.
            let receiver = capabilities.decision(RuntimeFeature::AdvancedReceiverConfiguration);
            capabilities.presets = PassthroughPreset::ALL.map(|preset| {
                crate::sys::advanced::passthrough_format(preset)
                    .map_err(CapabilityRefusal::BuildAbiUnavailable)
                    .and_then(|format| receiver.map(|()| format))
            });
        }
        capabilities
    }

    pub(crate) fn decision(
        &self,
        feature: RuntimeFeature,
    ) -> std::result::Result<(), CapabilityRefusal> {
        match feature.slot() {
            Slot::Unit(index) => self.decisions[index],
            #[cfg(feature = "advanced")]
            Slot::Passthrough(preset) => self.presets[preset.index()].map(|_| ()),
        }
    }

    /// Query a capability without changing native state.
    pub fn status(&self, feature: RuntimeFeature) -> CapabilityStatus {
        CapabilityStatus::from_decision(self.decision(feature))
    }

    /// Whether the wrapper has enough evidence to attempt the operation.
    pub fn supports(&self, feature: RuntimeFeature) -> bool {
        self.decision(feature).is_ok()
    }

    #[cfg(feature = "advanced")]
    pub(crate) fn passthrough_format(
        &self,
        preset: PassthroughPreset,
    ) -> std::result::Result<crate::sys::bindings::NDIlib_recv_color_format_e, CapabilityRefusal>
    {
        self.presets[preset.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        sys::{Evidence, Table},
        Error,
    };

    struct Fixture {
        api: Api,
        #[cfg(feature = "advanced")]
        advanced: AdvancedApi,
        capabilities: Capabilities,
    }

    impl Fixture {
        fn new(
            exported: impl Fn(&str) -> bool,
            version: Option<&str>,
            edition: SdkEdition,
        ) -> Self {
            let resolver = crate::sys::table::ExportsWhere(exported);
            let mut api = Api::load(&resolver).unwrap();
            #[cfg(feature = "advanced")]
            let mut advanced = AdvancedApi::load(&resolver).unwrap();
            let version = version.map(|text| SdkVersion::parse(text).unwrap());
            #[cfg(feature = "advanced")]
            let capabilities = Capabilities::detect(&api, &advanced, version, edition);
            #[cfg(not(feature = "advanced"))]
            let capabilities = Capabilities::detect(&api, version, edition);
            api.apply_capabilities(&capabilities);
            #[cfg(feature = "advanced")]
            advanced.apply_capabilities(&capabilities);
            Self {
                api,
                #[cfg(feature = "advanced")]
                advanced,
                capabilities,
            }
        }

        fn evidence(&self) -> Evidence<'_> {
            Evidence {
                api: &self.api,
                #[cfg(feature = "advanced")]
                advanced: &self.advanced,
                capabilities: &self.capabilities,
            }
        }
    }

    fn every_feature() -> Vec<RuntimeFeature> {
        let features = RuntimeFeature::UNIT.into_iter();
        #[cfg(feature = "advanced")]
        let features = features.chain(PassthroughPreset::ALL.map(RuntimeFeature::Passthrough));
        features.collect()
    }

    fn refusal<T>(
        result: crate::Result<T>,
    ) -> std::result::Result<(), (RuntimeFeature, CapabilityRefusal)> {
        match result {
            Ok(_) => Ok(()),
            Err(Error::UnsupportedCapability { feature, refusal }) => Err((feature, refusal)),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn parses_final_version_not_build_time() {
        let v = SdkVersion::parse("NDI SDK LINUX 10:14:14 Nov 6 2026 6.4.0.2").unwrap();
        assert_eq!((v.major(), v.minor(), v.patch(), v.build()), (6, 4, 0, 2));
        assert_eq!(v.to_string(), "6.4.0.2");
        for text in [
            "",
            "NDI 6.4",
            "NDI 2026-11-06",
            "NDI 6.4.0.2 trailing",
            "6..4.0",
            "6.4.0.-1",
        ] {
            assert!(SdkVersion::parse(text).is_none(), "{text}");
        }
    }

    #[test]
    fn version_parsing_preserves_components_without_allocating() {
        let (version, allocations) = crate::test_alloc::allocation_count(|| {
            SdkVersion::parse("NDI SDK TEST 4294967295.65536.4294967295.0")
        });
        assert_eq!(allocations, 0);
        let version = version.unwrap();
        assert_eq!(
            (
                version.major(),
                version.minor(),
                version.patch(),
                version.build()
            ),
            (u32::MAX, 65536, u32::MAX, 0)
        );
        for text in ["1.2.3.4.5", "4294967296.2.3", "1.2.3."] {
            assert!(SdkVersion::parse(text).is_none());
        }
    }

    #[test]
    fn every_feature_is_listed_once_under_its_own_name() {
        use std::collections::HashSet;
        let features = every_feature();
        assert_eq!(
            features.iter().copied().collect::<HashSet<_>>().len(),
            features.len()
        );
        assert_eq!(
            RuntimeFeature::UNIT
                .map(RuntimeFeature::name)
                .into_iter()
                .collect::<HashSet<_>>()
                .len(),
            RuntimeFeature::UNIT.len()
        );
        for (index, feature) in RuntimeFeature::UNIT.into_iter().enumerate() {
            assert!(matches!(feature.slot(), Slot::Unit(at) if at == index));
        }
        #[cfg(feature = "advanced")]
        for (index, preset) in PassthroughPreset::ALL.into_iter().enumerate() {
            assert_eq!(preset.index(), index);
            assert_eq!(
                RuntimeFeature::Passthrough(preset).name(),
                "compressed passthrough preset"
            );
        }
    }

    #[test]
    fn every_recorded_refusal_is_the_gate_error_and_display_evidence() {
        for (version, edition) in [
            (Some("6.4.0.2"), SdkEdition::Advanced),
            (Some("6.4.0.2"), SdkEdition::Standard),
            (Some("6.3.2.0"), SdkEdition::Advanced),
            (None, SdkEdition::Advanced),
        ] {
            // Complete exports, no optional exports, and one absent symbol at
            // a time exercise each family and both first-refusal orderings.
            let symbols: Vec<_> = Api::GATES
                .iter()
                .flat_map(|(_, names, _)| names.iter().copied())
                .collect();
            #[cfg(feature = "advanced")]
            let symbols: Vec<_> = symbols
                .into_iter()
                .chain(
                    crate::sys::advanced::GATES
                        .iter()
                        .flat_map(|(_, names, _)| names.iter().copied()),
                )
                .collect();
            for missing in std::iter::once(None).chain(symbols.into_iter().map(Some)) {
                let fixture = Fixture::new(
                    |name| missing.is_none_or(|symbol| symbol.to_bytes() != name.as_bytes()),
                    version,
                    edition,
                );
                let evidence = fixture.evidence();
                for feature in every_feature() {
                    let expected = fixture
                        .capabilities
                        .decision(feature)
                        .map_err(|refusal| (feature, refusal));
                    assert_eq!(
                        refusal(evidence.require(feature)),
                        expected,
                        "{feature:?} {version:?} {edition:?} {missing:?}"
                    );
                    assert_eq!(
                        fixture.capabilities.status(feature),
                        CapabilityStatus::from_decision(expected.map_err(|(_, refusal)| refusal))
                    );
                    if let Err((_, refusal)) = expected {
                        let error = Error::UnsupportedCapability { feature, refusal };
                        assert_eq!(
                            error.to_string(),
                            format!(
                                "NDI capability {} is unavailable: {refusal}",
                                feature.name()
                            )
                        );
                    }
                }
                for &(feature, _, gate) in Api::GATES {
                    assert_eq!(
                        refusal(gate(evidence)),
                        refusal(evidence.require(feature)),
                        "{feature:?}"
                    );
                }
                #[cfg(feature = "advanced")]
                for &(feature, _, gate) in crate::sys::advanced::GATES {
                    assert_eq!(
                        refusal(gate(evidence)),
                        refusal(evidence.require(feature)),
                        "{feature:?}"
                    );
                }
                assert_eq!(
                    refusal(crate::sys::Planar444Video::within(evidence)),
                    refusal(evidence.require(RuntimeFeature::Planar444Video))
                );
                assert_eq!(
                    refusal(crate::sys::ReceiveBest444::within(evidence)),
                    refusal(evidence.require(RuntimeFeature::ReceiveBest444))
                );
                assert_eq!(
                    refusal(crate::sys::ReceiveVideoOnly::within(evidence)),
                    refusal(evidence.require(RuntimeFeature::ReceiveVideoOnly))
                );
                #[cfg(feature = "advanced")]
                for preset in PassthroughPreset::ALL {
                    assert_eq!(
                        refusal(evidence.passthrough_format(preset)),
                        refusal(evidence.require(RuntimeFeature::Passthrough(preset)))
                    );
                }
            }
        }
    }

    #[test]
    fn core_gates_record_symbols_before_versions_and_copy_required_evidence() {
        let old = Fixture::new(|_| true, Some("6.3.2.0"), SdkEdition::Standard);
        let unknown = Fixture::new(|_| true, None, SdkEdition::Standard);
        let current = Fixture::new(|_| true, Some("6.4.0.0"), SdkEdition::Standard);
        for feature in RuntimeFeature::UNIT
            .into_iter()
            .filter(|feature| feature.minimum_version().is_some())
        {
            assert_eq!(
                old.capabilities.decision(feature),
                Err(CapabilityRefusal::RuntimeTooOld { required: NDI_6_4 })
            );
            assert_eq!(
                unknown.capabilities.decision(feature),
                Err(CapabilityRefusal::RuntimeVersionUnknown { required: NDI_6_4 })
            );
            assert!(current.capabilities.supports(feature));
        }
        for &(feature, symbols, _) in Api::GATES {
            let missing = symbols[0];
            let fixture = Fixture::new(
                |name| name.as_bytes() != missing.to_bytes(),
                Some("6.3.2.0"),
                SdkEdition::Standard,
            );
            assert_eq!(
                fixture.capabilities.decision(feature),
                Err(CapabilityRefusal::SymbolUnavailable(missing))
            );
        }
        for feature in [
            RuntimeFeature::ReceiverWebControl,
            RuntimeFeature::ConnectedSourceName,
        ] {
            let fixture = Fixture::new(
                |name| name != "NDIlib_recv_free_string",
                Some("6.4.0.2"),
                SdkEdition::Standard,
            );
            assert_eq!(
                fixture.capabilities.decision(feature),
                Err(CapabilityRefusal::SymbolUnavailable(
                    c"NDIlib_recv_free_string"
                ))
            );
        }
    }

    #[cfg(feature = "advanced")]
    #[test]
    fn advanced_refusal_precedence_and_requirements_preserve_exact_evidence() {
        for &(feature, symbols, _) in crate::sys::advanced::GATES {
            let missing = symbols[0];
            for edition in [SdkEdition::Standard, SdkEdition::Advanced] {
                let fixture = Fixture::new(
                    |name| name.as_bytes() != missing.to_bytes(),
                    Some("6.4.0.2"),
                    edition,
                );
                let result = fixture.capabilities.decision(feature);
                if AdvancedApi::declares(feature) {
                    assert_eq!(
                        result,
                        Err(if edition == SdkEdition::Standard {
                            CapabilityRefusal::WrongEdition(Some(missing))
                        } else {
                            CapabilityRefusal::SymbolUnavailable(missing)
                        })
                    );
                } else {
                    assert!(
                        matches!(result, Err(CapabilityRefusal::BuildAbiUnavailable(symbol)) if symbols.contains(&symbol))
                    );
                }
            }
            for &required in feature.requirements() {
                let fixture = Fixture::new(
                    |name| !name.starts_with("NDIlib_routing_") || name.ends_with("_v2"),
                    Some("6.3.2.0"),
                    SdkEdition::Advanced,
                );
                if AdvancedApi::declares(feature) {
                    assert_eq!(
                        fixture.capabilities.decision(feature),
                        fixture.capabilities.decision(required)
                    );
                }

                // Both fail: the capability's own missing symbol must win
                // over a required family's symbol or version refusal.
                let both_refused = Fixture::new(
                    |name| {
                        name.as_bytes() != missing.to_bytes()
                            && (!name.starts_with("NDIlib_routing_") || name.ends_with("_v2"))
                    },
                    Some("6.1.1.0"),
                    SdkEdition::Advanced,
                );
                assert!(both_refused.capabilities.decision(required).is_err());
                if AdvancedApi::declares(feature) {
                    assert_eq!(
                        refusal(both_refused.evidence().require(feature)),
                        Err((feature, CapabilityRefusal::SymbolUnavailable(missing)))
                    );
                }
            }
        }
        for preset in PassthroughPreset::ALL {
            for edition in [SdkEdition::Standard, SdkEdition::Advanced] {
                let fixture = Fixture::new(
                    |name| name != "NDIlib_recv_create_v4",
                    Some("6.4.0.2"),
                    edition,
                );
                let expected = crate::sys::advanced::passthrough_format(preset)
                    .map_err(CapabilityRefusal::BuildAbiUnavailable)
                    .and_then(|_| {
                        fixture
                            .capabilities
                            .decision(RuntimeFeature::AdvancedReceiverConfiguration)
                    });
                assert_eq!(
                    fixture
                        .capabilities
                        .decision(RuntimeFeature::Passthrough(preset)),
                    expected
                );
            }
        }
    }

    #[cfg(not(feature = "advanced"))]
    #[test]
    fn disabled_advanced_feature_still_names_every_missing_declaration() {
        let fixture = Fixture::new(|_| true, Some("6.4.0.2"), SdkEdition::Advanced);
        for feature in RuntimeFeature::UNIT
            .into_iter()
            .filter(|feature| feature.is_advanced())
        {
            assert_eq!(
                fixture.capabilities.decision(feature),
                Err(CapabilityRefusal::BuildAbiUnavailable(
                    absent_advanced_abi(feature).unwrap()
                ))
            );
        }
    }

    #[test]
    fn capability_status_and_every_gate_allocate_nothing() {
        // Build the fixture and feature list before tracking. Exercise real
        // refusal paths, including declarations, edition, symbols and versions,
        // rather than only constructing the public refusal enum.
        let features = every_feature();
        let mut total_refused = 0;
        for (version, edition) in [
            (Some("6.4.0.2"), SdkEdition::Advanced),
            (Some("6.4.0.2"), SdkEdition::Standard),
            (Some("6.1.1.0"), SdkEdition::Advanced),
            (None, SdkEdition::Advanced),
        ] {
            for missing in [
                None,
                Some("NDIlib_routing_create"),
                Some("NDIlib_recv_create_v4"),
            ] {
                let fixture = Fixture::new(|name| Some(name) != missing, version, edition);
                let (refused, allocations) = crate::test_alloc::allocation_count(|| {
                    let evidence = fixture.evidence();
                    let mut refused = 0;
                    for &feature in &features {
                        refused += usize::from(evidence.require(feature).is_err());
                        let _ = fixture.capabilities.status(feature);
                    }
                    for &(_, _, gate) in Api::GATES {
                        let _ = gate(evidence);
                    }
                    let _ = crate::sys::Planar444Video::within(evidence);
                    let _ = crate::sys::ReceiveBest444::within(evidence);
                    let _ = crate::sys::ReceiveVideoOnly::within(evidence);
                    #[cfg(feature = "advanced")]
                    {
                        for &(_, _, gate) in crate::sys::advanced::GATES {
                            let _ = gate(evidence);
                        }
                        for preset in PassthroughPreset::ALL {
                            let _ = evidence.passthrough_format(preset);
                        }
                    }
                    refused
                });
                assert_eq!(allocations, 0, "{version:?} {edition:?} {missing:?}");
                total_refused += refused;
            }
        }
        assert!(total_refused > 0);
    }
}
