//! The typed entry points of the selected NDI implementation.
//!
//! [`Core`] is required: a module that lacks any of its functions cannot be
//! selected. Every other table belongs to one optional capability, resolves
//! all-or-nothing, and is handed out only by [`Table::of`], which applies the
//! capability's gate with it. A wrapper holding a table can therefore call
//! everything in it without a further check.

use std::ffi::CStr;

use super::{
    bindings::*,
    loader::Runtime,
    table::{native_table, Resolver},
};
use crate::{
    capabilities::{
        Capabilities, CapabilityStatus, CapabilityUnavailable, RuntimeFeature, RuntimeInfo,
    },
    Error, LoadRefusal, Result,
};

native_table! {
    /// Entry points every supported NDI 6 runtime exports. A module that lacks any
    /// of them cannot be selected.
    pub(crate) struct Core from crate::sys::declarations {
        fn NDIlib_initialize() -> bool;
        fn NDIlib_destroy();
        fn NDIlib_version() -> *const ::std::os::raw::c_char;
        fn NDIlib_is_supported_CPU() -> bool;
        fn NDIlib_find_create_v2(p_create_settings: *const NDIlib_find_create_t) -> NDIlib_find_instance_t;
        fn NDIlib_find_destroy(p_instance: NDIlib_find_instance_t);
        fn NDIlib_find_get_current_sources(p_instance: NDIlib_find_instance_t, p_no_sources: *mut u32) -> *const NDIlib_source_t;
        fn NDIlib_find_wait_for_sources(p_instance: NDIlib_find_instance_t, timeout_in_ms: u32) -> bool;
        fn NDIlib_recv_create_v3(p_create_settings: *const NDIlib_recv_create_v3_t) -> NDIlib_recv_instance_t;
        fn NDIlib_recv_destroy(p_instance: NDIlib_recv_instance_t);
        fn NDIlib_recv_connect(p_instance: NDIlib_recv_instance_t, p_src: *const NDIlib_source_t);
        fn NDIlib_recv_capture_v3(p_instance: NDIlib_recv_instance_t, p_video_data: *mut NDIlib_video_frame_v2_t, p_audio_data: *mut NDIlib_audio_frame_v3_t, p_metadata: *mut NDIlib_metadata_frame_t, timeout_in_ms: u32) -> NDIlib_frame_type_e;
        fn NDIlib_recv_free_video_v2(p_instance: NDIlib_recv_instance_t, p_video_data: *const NDIlib_video_frame_v2_t);
        fn NDIlib_recv_free_audio_v3(p_instance: NDIlib_recv_instance_t, p_audio_data: *const NDIlib_audio_frame_v3_t);
        fn NDIlib_recv_free_metadata(p_instance: NDIlib_recv_instance_t, p_metadata: *const NDIlib_metadata_frame_t);
        fn NDIlib_recv_send_metadata(p_instance: NDIlib_recv_instance_t, p_metadata: *const NDIlib_metadata_frame_t) -> bool;
        fn NDIlib_recv_set_tally(p_instance: NDIlib_recv_instance_t, p_tally: *const NDIlib_tally_t) -> bool;
        fn NDIlib_recv_get_performance(p_instance: NDIlib_recv_instance_t, p_total: *mut NDIlib_recv_performance_t, p_dropped: *mut NDIlib_recv_performance_t);
        fn NDIlib_recv_get_queue(p_instance: NDIlib_recv_instance_t, p_total: *mut NDIlib_recv_queue_t);
        fn NDIlib_recv_get_no_connections(p_instance: NDIlib_recv_instance_t) -> ::std::os::raw::c_int;
        fn NDIlib_recv_ptz_is_supported(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_ptz_zoom(p_instance: NDIlib_recv_instance_t, zoom_value: f32) -> bool;
        fn NDIlib_recv_ptz_zoom_speed(p_instance: NDIlib_recv_instance_t, zoom_speed: f32) -> bool;
        fn NDIlib_recv_ptz_pan_tilt(p_instance: NDIlib_recv_instance_t, pan_value: f32, tilt_value: f32) -> bool;
        fn NDIlib_recv_ptz_pan_tilt_speed(p_instance: NDIlib_recv_instance_t, pan_speed: f32, tilt_speed: f32) -> bool;
        fn NDIlib_recv_ptz_store_preset(p_instance: NDIlib_recv_instance_t, preset_no: ::std::os::raw::c_int) -> bool;
        fn NDIlib_recv_ptz_recall_preset(p_instance: NDIlib_recv_instance_t, preset_no: ::std::os::raw::c_int, speed: f32) -> bool;
        fn NDIlib_recv_ptz_auto_focus(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_ptz_focus(p_instance: NDIlib_recv_instance_t, focus_value: f32) -> bool;
        fn NDIlib_recv_ptz_focus_speed(p_instance: NDIlib_recv_instance_t, focus_speed: f32) -> bool;
        fn NDIlib_recv_ptz_white_balance_auto(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_ptz_white_balance_indoor(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_ptz_white_balance_outdoor(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_ptz_white_balance_oneshot(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_ptz_white_balance_manual(p_instance: NDIlib_recv_instance_t, red: f32, blue: f32) -> bool;
        fn NDIlib_recv_ptz_exposure_auto(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_ptz_exposure_manual(p_instance: NDIlib_recv_instance_t, exposure_level: f32) -> bool;
        fn NDIlib_recv_ptz_exposure_manual_v2(p_instance: NDIlib_recv_instance_t, iris: f32, gain: f32, shutter_speed: f32) -> bool;
        fn NDIlib_send_create(p_create_settings: *const NDIlib_send_create_t) -> NDIlib_send_instance_t;
        fn NDIlib_send_destroy(p_instance: NDIlib_send_instance_t);
        fn NDIlib_send_send_video_v2(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t);
        fn NDIlib_send_send_video_async_v2(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t);
        fn NDIlib_send_send_audio_v3(p_instance: NDIlib_send_instance_t, p_audio_data: *const NDIlib_audio_frame_v3_t);
        fn NDIlib_send_send_metadata(p_instance: NDIlib_send_instance_t, p_metadata: *const NDIlib_metadata_frame_t);
        fn NDIlib_send_get_tally(p_instance: NDIlib_send_instance_t, p_tally: *mut NDIlib_tally_t, timeout_in_ms: u32) -> bool;
        fn NDIlib_send_get_no_connections(p_instance: NDIlib_send_instance_t, timeout_in_ms: u32) -> ::std::os::raw::c_int;
        fn NDIlib_send_clear_connection_metadata(p_instance: NDIlib_send_instance_t);
        fn NDIlib_send_add_connection_metadata(p_instance: NDIlib_send_instance_t, p_metadata: *const NDIlib_metadata_frame_t);
        fn NDIlib_send_set_failover(p_instance: NDIlib_send_instance_t, p_failover_source: *const NDIlib_source_t);
        fn NDIlib_send_get_source_name(p_instance: NDIlib_send_instance_t) -> *const NDIlib_source_t;
        fn NDIlib_framesync_create(p_receiver: NDIlib_recv_instance_t) -> NDIlib_framesync_instance_t;
        fn NDIlib_framesync_destroy(p_instance: NDIlib_framesync_instance_t);
        fn NDIlib_framesync_capture_audio_v2(p_instance: NDIlib_framesync_instance_t, p_audio_data: *mut NDIlib_audio_frame_v3_t, sample_rate: ::std::os::raw::c_int, no_channels: ::std::os::raw::c_int, no_samples: ::std::os::raw::c_int);
        fn NDIlib_framesync_free_audio_v2(p_instance: NDIlib_framesync_instance_t, p_audio_data: *mut NDIlib_audio_frame_v3_t);
        fn NDIlib_framesync_audio_queue_depth(p_instance: NDIlib_framesync_instance_t) -> ::std::os::raw::c_int;
        fn NDIlib_framesync_capture_video(p_instance: NDIlib_framesync_instance_t, p_video_data: *mut NDIlib_video_frame_v2_t, field_type: NDIlib_frame_format_type_e);
        fn NDIlib_framesync_free_video(p_instance: NDIlib_framesync_instance_t, p_video_data: *mut NDIlib_video_frame_v2_t);
    }
}

native_table! {
    /// Release of strings the receiver API returns.
    ///
    /// This is not a capability of its own: it comes with each capability
    /// whose entry points return such a string.
    pub(crate) struct ReceiverStrings from crate::sys::declarations {
        fn NDIlib_recv_free_string(p_instance: NDIlib_recv_instance_t, p_string: *const ::std::os::raw::c_char);
    }
}

/// The entry points of one optional capability.
///
/// This is the only way to an optional table, for the core SDK and the
/// Advanced SDK alike, so a table can never be used without the gate of the
/// capability it belongs to, or be reported under another capability.
pub(crate) trait Table: Sized + 'static {
    /// The entry points resolved from `runtime`.
    ///
    /// # Errors
    ///
    /// As for [`Self::within`].
    fn of(runtime: &'static Runtime) -> Result<&'static Self> {
        Self::within(runtime.evidence())
    }

    /// The entry points `evidence` holds, once the gate of the
    /// [`RuntimeFeature`] the table belongs to allows them. The table is
    /// refused, with the same error, whenever [`Evidence::require`] refuses
    /// that feature.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] for the table's feature, when
    /// the runtime lacks an entry point, is too old for the capability, or
    /// (for the Advanced SDK) is not an Advanced SDK runtime, or the build
    /// has no declarations for it.
    fn within(evidence: Evidence<'_>) -> Result<&'_ Self>;
}

// Version-only enum values have no entry points of their own. These private
// proofs use the same gate as tables, so descriptor builders cannot bypass it.
macro_rules! version_gates {
    ($($gate:ident),+ $(,)?) => {$(
        pub(crate) struct $gate(());
        impl Table for $gate {
            fn within(evidence: Evidence<'_>) -> Result<&'_ Self> {
                evidence.require(RuntimeFeature::$gate)?;
                static PROOF: $gate = $gate(());
                Ok(&PROOF)
            }
        }
    )+};
}
version_gates!(Planar444Video, ReceiveBest444, ReceiveVideoOnly);

/// What the gate of an optional capability decides from: the tables resolved
/// from one implementation, its identity, and the capabilities derived from
/// them.
#[derive(Clone, Copy)]
pub(crate) struct Evidence<'a> {
    pub(crate) api: &'a Api,
    #[cfg(feature = "advanced")]
    pub(crate) advanced: &'a super::advanced::AdvancedApi,
    pub(crate) info: &'a RuntimeInfo,
    pub(crate) capabilities: &'a Capabilities,
}

/// The tables of an optional capability, or the first entry point the
/// selected implementation lacks.
struct Optional<T>(std::result::Result<T, &'static CStr>);

/// What a capability's field of [`Api`] holds, and how it is resolved: its
/// table, together with a support table when its entry points need one.
macro_rules! slot {
    (type $Table:ident) => {
        $Table
    };
    (type $Table:ident $Support:ident) => {
        ($Table, $Support)
    };
    (load $resolver:ident $Table:ident) => {
        $Table::load($resolver)?
    };
    (load $resolver:ident $Table:ident $Support:ident) => {
        match ($Table::load($resolver)?, $Support::load($resolver)?) {
            (Ok(table), Ok(support)) => Ok((table, support)),
            (Err(symbol), _) | (_, Err(symbol)) => Err(symbol),
        }
    };
}

/// Define the optional capabilities of the core SDK.
///
/// Each entry states, once, everything about one capability: the table of
/// its entry points, the field of [`Api`] that holds it, and the
/// [`RuntimeFeature`] it is gated and reported under. `+ Support` adds a
/// table the capability cannot be used without; the two are then resolved
/// and handed out together, as a pair.
macro_rules! optional_tables {
    ($(
        $(#[$meta:meta])*
        struct $Table:ident $(+ $Support:ident)? in $field:ident: $feature:ident {
            $(fn $name:ident($($argument:ident: $type:ty),* $(,)?) $(-> $return:ty)?;)+
        }
    )+) => {
        $(
            native_table! {
                $(#[$meta])*
                pub(crate) struct $Table from crate::sys::declarations {
                    $(fn $name($($argument: $type),*) $(-> $return)?;)+
                }
            }

            impl Table for slot!(type $Table $($Support)?) {
                fn within(evidence: Evidence<'_>) -> Result<&'_ Self> {
                    evidence.capability(RuntimeFeature::$feature, &evidence.api.$field.0)
                }
            }
        )+

        /// Every table resolved from one implementation.
        pub(crate) struct Api {
            pub(crate) core: Core,
            $($field: Optional<slot!(type $Table $($Support)?)>,)+
        }

        impl Api {
            /// Resolve every table from `resolver`.
            ///
            /// # Errors
            ///
            /// Fails when a [`Core`] entry point is missing or any entry point
            /// resolves outside the selected implementation.
            pub(crate) fn load(resolver: &dyn Resolver) -> std::result::Result<Self, LoadRefusal> {
                let core = Core::load(resolver)?.map_err(|symbol| {
                    LoadRefusal::MissingEntryPoint { symbol: symbol.to_string_lossy().into_owned(), required: true, detail: None }
                })?;
                Ok(Self {
                    core,
                    $($field: Optional(slot!(load resolver $Table $($Support)?)),)+
                })
            }

            /// The first entry point behind `feature` that the implementation
            /// does not export. `None` when it exports them all, and for a
            /// feature with no table here: a core feature gated only by its
            /// version or its requirements, or one that belongs to the
            /// Advanced SDK.
            pub(crate) fn missing(&self, feature: RuntimeFeature) -> Option<&'static CStr> {
                // A core feature with no table here, no minimum version and no
                // requirement would be reported available on every runtime.
                // The crate does not compile unless each such feature says it
                // is `ungated`, and no other one does.
                const _: () = {
                    let mut index = 0;
                    while index < RuntimeFeature::UNIT.len() {
                        let feature = RuntimeFeature::UNIT[index];
                        let gated = matches!(feature, $(RuntimeFeature::$feature)|+)
                            || feature.minimum_version().is_some()
                            || !feature.requirements().is_empty();
                        assert!(
                            feature.is_advanced() || gated != feature.is_ungated(),
                            "a core capability without a table, `since` or `requires` must be \
                             declared `ungated`, and only such a one may be"
                        );
                        index += 1;
                    }
                };
                match feature {
                    $(RuntimeFeature::$feature => self.$field.0.as_ref().err().copied(),)+
                    _ => None,
                }
            }
        }
    };
}

optional_tables! {
    /// Independent native audio and video receive delays.
    struct ReceiveDelay in receive_delay: ReceiveDelay {
        fn NDIlib_recv_set_video_delay(p_instance: NDIlib_recv_instance_t, delay_in_ms: ::std::os::raw::c_int) -> bool;
        fn NDIlib_recv_get_video_delay(p_instance: NDIlib_recv_instance_t) -> ::std::os::raw::c_int;
        fn NDIlib_recv_set_audio_delay(p_instance: NDIlib_recv_instance_t, delay_in_ms: ::std::os::raw::c_int) -> bool;
        fn NDIlib_recv_get_audio_delay(p_instance: NDIlib_recv_instance_t) -> ::std::os::raw::c_int;
    }

    /// Metadata a receiver replays to each new connection.
    struct ReceiverConnectionMetadata in receiver_connection_metadata: ReceiverConnectionMetadata {
        fn NDIlib_recv_add_connection_metadata(p_instance: NDIlib_recv_instance_t, p_metadata: *const NDIlib_metadata_frame_t);
        fn NDIlib_recv_clear_connection_metadata(p_instance: NDIlib_recv_instance_t);
    }

    /// The connected source's web-control URL.
    struct ReceiverWebControl + ReceiverStrings in receiver_web_control: ReceiverWebControl {
        fn NDIlib_recv_get_web_control(p_instance: NDIlib_recv_instance_t) -> *const ::std::os::raw::c_char;
    }

    /// Notifications of the connected source's name.
    struct ReceiverSourceName + ReceiverStrings in receiver_source_name: ConnectedSourceName {
        fn NDIlib_recv_get_source_name(p_instance: NDIlib_recv_instance_t, p_source_name: *mut *const ::std::os::raw::c_char, timeout_in_ms: u32) -> bool;
    }

    /// Metadata received by a sender from its connections.
    struct SenderMetadata in sender_metadata: SenderMetadata {
        fn NDIlib_send_capture(p_instance: NDIlib_send_instance_t, p_metadata: *mut NDIlib_metadata_frame_t, timeout_in_ms: u32) -> NDIlib_frame_type_e;
        fn NDIlib_send_free_metadata(p_instance: NDIlib_send_instance_t, p_metadata: *const NDIlib_metadata_frame_t);
    }

    /// The SDK's current timestamp.
    struct Timestamp in timestamp: SdkTimestamp {
        fn NDIlib_util_get_timestamp() -> i64;
    }

    /// Source routing.
    struct Routing in routing: Routing {
        fn NDIlib_routing_create(p_create_settings: *const NDIlib_routing_create_t) -> NDIlib_routing_instance_t;
        fn NDIlib_routing_destroy(p_instance: NDIlib_routing_instance_t);
        fn NDIlib_routing_change(p_instance: NDIlib_routing_instance_t, p_source: *const NDIlib_source_t) -> bool;
        fn NDIlib_routing_clear(p_instance: NDIlib_routing_instance_t) -> bool;
        fn NDIlib_routing_get_no_connections(p_instance: NDIlib_routing_instance_t, timeout_in_ms: u32) -> ::std::os::raw::c_int;
        fn NDIlib_routing_get_source_name(p_instance: NDIlib_routing_instance_t) -> *const NDIlib_source_t;
    }

    /// Discovery Server registration of senders.
    struct SenderAdvertising in sender_advertising: SenderAdvertising {
        fn NDIlib_send_advertiser_create(p_create_settings: *const NDIlib_send_advertiser_create_t) -> NDIlib_send_advertiser_instance_t;
        fn NDIlib_send_advertiser_destroy(p_instance: NDIlib_send_advertiser_instance_t);
        fn NDIlib_send_advertiser_add_sender(p_instance: NDIlib_send_advertiser_instance_t, p_sender: NDIlib_send_instance_t, allow_monitoring: bool) -> bool;
        fn NDIlib_send_advertiser_del_sender(p_instance: NDIlib_send_advertiser_instance_t, p_sender: NDIlib_send_instance_t) -> bool;
    }

    /// Discovery Server registration of receivers.
    struct ReceiverAdvertising in receiver_advertising: ReceiverAdvertising {
        fn NDIlib_recv_advertiser_create(p_create_settings: *const NDIlib_recv_advertiser_create_t) -> NDIlib_recv_advertiser_instance_t;
        fn NDIlib_recv_advertiser_destroy(p_instance: NDIlib_recv_advertiser_instance_t);
        fn NDIlib_recv_advertiser_add_receiver(p_instance: NDIlib_recv_advertiser_instance_t, p_receiver: NDIlib_recv_instance_t, allow_controlling: bool, allow_monitoring: bool, p_input_group_name: *const ::std::os::raw::c_char) -> bool;
        fn NDIlib_recv_advertiser_del_receiver(p_instance: NDIlib_recv_advertiser_instance_t, p_receiver: NDIlib_recv_instance_t) -> bool;
    }

    /// Discovery Server sender snapshots and events.
    struct SenderDiscovery in sender_discovery: SenderDiscovery {
        fn NDIlib_send_listener_create(p_create_settings: *const NDIlib_send_listener_create_t) -> NDIlib_send_listener_instance_t;
        fn NDIlib_send_listener_destroy(p_instance: NDIlib_send_listener_instance_t);
        fn NDIlib_send_listener_is_connected(p_instance: NDIlib_send_listener_instance_t) -> bool;
        fn NDIlib_send_listener_get_server_url(p_instance: NDIlib_send_listener_instance_t) -> *const ::std::os::raw::c_char;
        fn NDIlib_send_listener_get_senders(p_instance: NDIlib_send_listener_instance_t, p_num_senders: *mut u32) -> *const NDIlib_sender_t;
        fn NDIlib_send_listener_wait_for_senders(p_instance: NDIlib_send_listener_instance_t, timeout_in_ms: u32) -> bool;
        fn NDIlib_send_listener_subscribe_events(p_instance: NDIlib_send_listener_instance_t, p_sender_uuid: *const ::std::os::raw::c_char);
        fn NDIlib_send_listener_unsubscribe_events(p_instance: NDIlib_send_listener_instance_t, p_sender_uuid: *const ::std::os::raw::c_char);
        fn NDIlib_send_listener_get_events(p_instance: NDIlib_send_listener_instance_t, p_num_events: *mut u32, timeout_in_ms: u32) -> *const NDIlib_send_listener_event;
        fn NDIlib_send_listener_free_events(p_instance: NDIlib_send_listener_instance_t, p_events: *const NDIlib_send_listener_event);
    }

    /// Discovery Server receiver snapshots, events and connection requests.
    struct ReceiverDiscovery in receiver_discovery: ReceiverDiscovery {
        fn NDIlib_recv_listener_create(p_create_settings: *const NDIlib_recv_listener_create_t) -> NDIlib_recv_listener_instance_t;
        fn NDIlib_recv_listener_destroy(p_instance: NDIlib_recv_listener_instance_t);
        fn NDIlib_recv_listener_is_connected(p_instance: NDIlib_recv_listener_instance_t) -> bool;
        fn NDIlib_recv_listener_get_server_url(p_instance: NDIlib_recv_listener_instance_t) -> *const ::std::os::raw::c_char;
        fn NDIlib_recv_listener_get_receivers(p_instance: NDIlib_recv_listener_instance_t, p_num_receivers: *mut u32) -> *const NDIlib_receiver_t;
        fn NDIlib_recv_listener_wait_for_receivers(p_instance: NDIlib_recv_listener_instance_t, timeout_in_ms: u32) -> bool;
        fn NDIlib_recv_listener_subscribe_events(p_instance: NDIlib_recv_listener_instance_t, p_receiver_uuid: *const ::std::os::raw::c_char);
        fn NDIlib_recv_listener_unsubscribe_events(p_instance: NDIlib_recv_listener_instance_t, p_receiver_uuid: *const ::std::os::raw::c_char);
        fn NDIlib_recv_listener_get_events(p_instance: NDIlib_recv_listener_instance_t, p_num_events: *mut u32, timeout_in_ms: u32) -> *const NDIlib_recv_listener_event;
        fn NDIlib_recv_listener_free_events(p_instance: NDIlib_recv_listener_instance_t, p_events: *const NDIlib_recv_listener_event);
        fn NDIlib_recv_listener_send_connect(p_instance: NDIlib_recv_listener_instance_t, p_receiver_uuid: *const ::std::os::raw::c_char, p_source_name: *const ::std::os::raw::c_char) -> bool;
    }

    /// Conversions between planar and interleaved audio.
    struct InterleavedAudio in interleaved_audio: InterleavedAudioConversion {
        fn NDIlib_util_audio_to_interleaved_16s_v3(p_src: *const NDIlib_audio_frame_v3_t, p_dst: *mut NDIlib_audio_frame_interleaved_16s_t) -> bool;
        fn NDIlib_util_audio_from_interleaved_16s_v3(p_src: *const NDIlib_audio_frame_interleaved_16s_t, p_dst: *mut NDIlib_audio_frame_v3_t) -> bool;
        fn NDIlib_util_audio_to_interleaved_32s_v3(p_src: *const NDIlib_audio_frame_v3_t, p_dst: *mut NDIlib_audio_frame_interleaved_32s_t) -> bool;
        fn NDIlib_util_audio_from_interleaved_32s_v3(p_src: *const NDIlib_audio_frame_interleaved_32s_t, p_dst: *mut NDIlib_audio_frame_v3_t) -> bool;
        fn NDIlib_util_audio_to_interleaved_32f_v3(p_src: *const NDIlib_audio_frame_v3_t, p_dst: *mut NDIlib_audio_frame_interleaved_32f_t) -> bool;
        fn NDIlib_util_audio_from_interleaved_32f_v3(p_src: *const NDIlib_audio_frame_interleaved_32f_t, p_dst: *mut NDIlib_audio_frame_v3_t) -> bool;
    }

    /// Direct submission of interleaved audio.
    struct InterleavedSend in interleaved_send: InterleavedAudioSend {
        fn NDIlib_util_send_send_audio_interleaved_16s(p_instance: NDIlib_send_instance_t, p_audio_data: *const NDIlib_audio_frame_interleaved_16s_t);
        fn NDIlib_util_send_send_audio_interleaved_32s(p_instance: NDIlib_send_instance_t, p_audio_data: *const NDIlib_audio_frame_interleaved_32s_t);
        fn NDIlib_util_send_send_audio_interleaved_32f(p_instance: NDIlib_send_instance_t, p_audio_data: *const NDIlib_audio_frame_interleaved_32f_t);
    }

    /// Conversions between the V210 and P216 video layouts.
    struct VideoConversion in video_conversion: VideoConversion {
        fn NDIlib_util_V210_to_P216(p_src_v210: *const NDIlib_video_frame_v2_t, p_dst_p216: *mut NDIlib_video_frame_v2_t);
        fn NDIlib_util_P216_to_V210(p_src_p216: *const NDIlib_video_frame_v2_t, p_dst_v210: *mut NDIlib_video_frame_v2_t);
    }
}

impl Runtime {
    /// What the gates of this implementation's capabilities decide from.
    pub(crate) fn evidence(&self) -> Evidence<'_> {
        Evidence {
            api: &self.api,
            #[cfg(feature = "advanced")]
            advanced: &self.advanced,
            info: &self.info,
            capabilities: &self.capabilities,
        }
    }

    /// Reject a capability the selected implementation cannot provide.
    ///
    /// # Errors
    ///
    /// As for [`Evidence::require`].
    pub(crate) fn require(&self, feature: RuntimeFeature) -> Result<()> {
        self.evidence().require(feature)
    }
}

impl<'a> Evidence<'a> {
    /// Reject a capability the implementation cannot provide.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] for `feature`, with the
    /// reason its status gives.
    pub(crate) fn require(self, feature: RuntimeFeature) -> Result<()> {
        match self.capabilities.status(feature) {
            CapabilityStatus::Available => Ok(()),
            CapabilityStatus::Unavailable(reason) => Err(self.unavailable(feature, reason)),
        }
    }

    /// The table behind `feature`, once its status allows it.
    ///
    /// A capability's status includes any gate its table cannot express, such
    /// as a version or an edition, so it is always applied together with the
    /// table, and both are reported as `feature`.
    pub(in crate::sys) fn capability<T>(
        self,
        feature: RuntimeFeature,
        table: &'a std::result::Result<T, &'static CStr>,
    ) -> Result<&'a T> {
        self.require(feature)?;
        // The status is derived from the same entry points, so this is
        // reached only by a table that needs one its status does not check.
        table
            .as_ref()
            .map_err(|symbol| Error::UnsupportedCapability {
                feature,
                reason: CapabilityUnavailable::SymbolUnavailable,
                detail: symbol.to_string_lossy().into_owned(),
            })
    }

    /// The error for `feature`, whose status is `reason`, with the evidence
    /// for it.
    pub(in crate::sys) fn unavailable(
        self,
        feature: RuntimeFeature,
        reason: CapabilityUnavailable,
    ) -> Error {
        // A status copied from a capability that `feature` requires is
        // reported with that capability's evidence.
        let evidence = self.capabilities.refused_by(feature);
        let detail = match reason {
            CapabilityUnavailable::RuntimeTooOld | CapabilityUnavailable::RuntimeVersionUnknown => {
                evidence.minimum_version().map(|minimum| {
                    format!(
                        "it needs NDI {minimum} or later and the runtime reports {:?}",
                        self.info.raw_version
                    )
                })
            }
            // The feature's table names what it lacks: an entry point the
            // runtime does not export or, in a build without its Advanced
            // declarations, a function the build does not declare.
            CapabilityUnavailable::SymbolUnavailable
            | CapabilityUnavailable::WrongEdition
            | CapabilityUnavailable::BuildAbiUnavailable => self
                .missing(evidence)
                .map(|symbol| symbol.to_string_lossy().into_owned()),
        };
        Error::UnsupportedCapability {
            feature,
            reason,
            detail: detail.unwrap_or_default(),
        }
    }

    /// The first function of `feature`'s table that did not resolve.
    fn missing(self, feature: RuntimeFeature) -> Option<&'static CStr> {
        #[cfg(feature = "advanced")]
        if let Some(symbol) = self.advanced.missing(feature) {
            return Some(symbol);
        }
        self.api.missing(feature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::table::ExportsWhere;

    #[test]
    fn a_capability_is_exported_only_with_every_table_it_needs() {
        let complete = Api::load(&ExportsWhere(|_| true)).unwrap();
        assert_eq!(complete.missing(RuntimeFeature::Routing), None);
        assert_eq!(complete.missing(RuntimeFeature::ReceiverWebControl), None);
        // A version gate has no entry points of its own.
        assert_eq!(complete.missing(RuntimeFeature::Planar444Video), None);

        // The release function is part of each capability that returns a
        // string, and of no other.
        let api = Api::load(&ExportsWhere(|name| name != "NDIlib_recv_free_string")).unwrap();
        for feature in [
            RuntimeFeature::ReceiverWebControl,
            RuntimeFeature::ConnectedSourceName,
        ] {
            assert_eq!(
                api.missing(feature),
                Some(c"NDIlib_recv_free_string"),
                "{feature:?}"
            );
        }
        assert_eq!(api.missing(RuntimeFeature::ReceiveDelay), None);

        let api = Api::load(&ExportsWhere(|name| name != "NDIlib_recv_get_web_control")).unwrap();
        assert_eq!(
            api.missing(RuntimeFeature::ReceiverWebControl),
            Some(c"NDIlib_recv_get_web_control")
        );
        assert_eq!(api.missing(RuntimeFeature::ConnectedSourceName), None);
    }
}
