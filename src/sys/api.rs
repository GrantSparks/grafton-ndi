//! The typed entry points of the selected NDI implementation.
//!
//! [`Core`] is required: a module that lacks any of its functions cannot be
//! selected. Every other table is one independently available capability and
//! resolves all-or-nothing, so a wrapper holding a table can call everything in
//! it without a further check. An optional table is handed out only by
//! [`Runtime::capability`], which applies the capability's status with it.

use std::ffi::CStr;

use super::{
    bindings::*,
    loader::Runtime,
    table::{native_table, Resolver},
};
use crate::{
    capabilities::{CapabilityStatus, CapabilityUnavailable, RuntimeFeature},
    Error, Result,
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
    /// Independent native audio and video receive delays.
    pub(crate) struct ReceiveDelay from crate::sys::declarations {
        fn NDIlib_recv_set_video_delay(p_instance: NDIlib_recv_instance_t, delay_in_ms: ::std::os::raw::c_int) -> bool;
        fn NDIlib_recv_get_video_delay(p_instance: NDIlib_recv_instance_t) -> ::std::os::raw::c_int;
        fn NDIlib_recv_set_audio_delay(p_instance: NDIlib_recv_instance_t, delay_in_ms: ::std::os::raw::c_int) -> bool;
        fn NDIlib_recv_get_audio_delay(p_instance: NDIlib_recv_instance_t) -> ::std::os::raw::c_int;
    }
}

native_table! {
    /// Metadata a receiver replays to each new connection.
    pub(crate) struct ReceiverConnectionMetadata from crate::sys::declarations {
        fn NDIlib_recv_add_connection_metadata(p_instance: NDIlib_recv_instance_t, p_metadata: *const NDIlib_metadata_frame_t);
        fn NDIlib_recv_clear_connection_metadata(p_instance: NDIlib_recv_instance_t);
    }
}

native_table! {
    /// Release of strings the receiver API returns.
    pub(crate) struct ReceiverStrings from crate::sys::declarations {
        fn NDIlib_recv_free_string(p_instance: NDIlib_recv_instance_t, p_string: *const ::std::os::raw::c_char);
    }
}

native_table! {
    /// The connected source's web-control URL.
    pub(crate) struct ReceiverWebControl from crate::sys::declarations {
        fn NDIlib_recv_get_web_control(p_instance: NDIlib_recv_instance_t) -> *const ::std::os::raw::c_char;
    }
}

native_table! {
    /// Notifications of the connected source's name.
    pub(crate) struct ReceiverSourceName from crate::sys::declarations {
        fn NDIlib_recv_get_source_name(p_instance: NDIlib_recv_instance_t, p_source_name: *mut *const ::std::os::raw::c_char, timeout_in_ms: u32) -> bool;
    }
}

native_table! {
    /// Metadata received by a sender from its connections.
    pub(crate) struct SenderMetadata from crate::sys::declarations {
        fn NDIlib_send_capture(p_instance: NDIlib_send_instance_t, p_metadata: *mut NDIlib_metadata_frame_t, timeout_in_ms: u32) -> NDIlib_frame_type_e;
        fn NDIlib_send_free_metadata(p_instance: NDIlib_send_instance_t, p_metadata: *const NDIlib_metadata_frame_t);
    }
}

native_table! {
    /// The SDK's current timestamp.
    pub(crate) struct Timestamp from crate::sys::declarations {
        fn NDIlib_util_get_timestamp() -> i64;
    }
}

native_table! {
    /// Source routing.
    pub(crate) struct Routing from crate::sys::declarations {
        fn NDIlib_routing_create(p_create_settings: *const NDIlib_routing_create_t) -> NDIlib_routing_instance_t;
        fn NDIlib_routing_destroy(p_instance: NDIlib_routing_instance_t);
        fn NDIlib_routing_change(p_instance: NDIlib_routing_instance_t, p_source: *const NDIlib_source_t) -> bool;
        fn NDIlib_routing_clear(p_instance: NDIlib_routing_instance_t) -> bool;
        fn NDIlib_routing_get_no_connections(p_instance: NDIlib_routing_instance_t, timeout_in_ms: u32) -> ::std::os::raw::c_int;
        fn NDIlib_routing_get_source_name(p_instance: NDIlib_routing_instance_t) -> *const NDIlib_source_t;
    }
}

native_table! {
    /// Discovery Server registration of senders.
    pub(crate) struct SenderAdvertising from crate::sys::declarations {
        fn NDIlib_send_advertiser_create(p_create_settings: *const NDIlib_send_advertiser_create_t) -> NDIlib_send_advertiser_instance_t;
        fn NDIlib_send_advertiser_destroy(p_instance: NDIlib_send_advertiser_instance_t);
        fn NDIlib_send_advertiser_add_sender(p_instance: NDIlib_send_advertiser_instance_t, p_sender: NDIlib_send_instance_t, allow_monitoring: bool) -> bool;
        fn NDIlib_send_advertiser_del_sender(p_instance: NDIlib_send_advertiser_instance_t, p_sender: NDIlib_send_instance_t) -> bool;
    }
}

native_table! {
    /// Discovery Server registration of receivers.
    pub(crate) struct ReceiverAdvertising from crate::sys::declarations {
        fn NDIlib_recv_advertiser_create(p_create_settings: *const NDIlib_recv_advertiser_create_t) -> NDIlib_recv_advertiser_instance_t;
        fn NDIlib_recv_advertiser_destroy(p_instance: NDIlib_recv_advertiser_instance_t);
        fn NDIlib_recv_advertiser_add_receiver(p_instance: NDIlib_recv_advertiser_instance_t, p_receiver: NDIlib_recv_instance_t, allow_controlling: bool, allow_monitoring: bool, p_input_group_name: *const ::std::os::raw::c_char) -> bool;
        fn NDIlib_recv_advertiser_del_receiver(p_instance: NDIlib_recv_advertiser_instance_t, p_receiver: NDIlib_recv_instance_t) -> bool;
    }
}

native_table! {
    /// Discovery Server sender snapshots and events.
    pub(crate) struct SenderDiscovery from crate::sys::declarations {
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
}

native_table! {
    /// Discovery Server receiver snapshots, events and connection requests.
    pub(crate) struct ReceiverDiscovery from crate::sys::declarations {
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
}

native_table! {
    /// Conversions between planar and interleaved audio.
    pub(crate) struct InterleavedAudio from crate::sys::declarations {
        fn NDIlib_util_audio_to_interleaved_16s_v3(p_src: *const NDIlib_audio_frame_v3_t, p_dst: *mut NDIlib_audio_frame_interleaved_16s_t) -> bool;
        fn NDIlib_util_audio_from_interleaved_16s_v3(p_src: *const NDIlib_audio_frame_interleaved_16s_t, p_dst: *mut NDIlib_audio_frame_v3_t) -> bool;
        fn NDIlib_util_audio_to_interleaved_32s_v3(p_src: *const NDIlib_audio_frame_v3_t, p_dst: *mut NDIlib_audio_frame_interleaved_32s_t) -> bool;
        fn NDIlib_util_audio_from_interleaved_32s_v3(p_src: *const NDIlib_audio_frame_interleaved_32s_t, p_dst: *mut NDIlib_audio_frame_v3_t) -> bool;
        fn NDIlib_util_audio_to_interleaved_32f_v3(p_src: *const NDIlib_audio_frame_v3_t, p_dst: *mut NDIlib_audio_frame_interleaved_32f_t) -> bool;
        fn NDIlib_util_audio_from_interleaved_32f_v3(p_src: *const NDIlib_audio_frame_interleaved_32f_t, p_dst: *mut NDIlib_audio_frame_v3_t) -> bool;
    }
}

native_table! {
    /// Direct submission of interleaved audio.
    pub(crate) struct InterleavedSend from crate::sys::declarations {
        fn NDIlib_util_send_send_audio_interleaved_16s(p_instance: NDIlib_send_instance_t, p_audio_data: *const NDIlib_audio_frame_interleaved_16s_t);
        fn NDIlib_util_send_send_audio_interleaved_32s(p_instance: NDIlib_send_instance_t, p_audio_data: *const NDIlib_audio_frame_interleaved_32s_t);
        fn NDIlib_util_send_send_audio_interleaved_32f(p_instance: NDIlib_send_instance_t, p_audio_data: *const NDIlib_audio_frame_interleaved_32f_t);
    }
}

native_table! {
    /// Conversions between the V210 and P216 video layouts.
    pub(crate) struct VideoConversion from crate::sys::declarations {
        fn NDIlib_util_V210_to_P216(p_src_v210: *const NDIlib_video_frame_v2_t, p_dst_p216: *mut NDIlib_video_frame_v2_t);
        fn NDIlib_util_P216_to_V210(p_src_p216: *const NDIlib_video_frame_v2_t, p_dst_v210: *mut NDIlib_video_frame_v2_t);
    }
}

/// An optional table, or the first entry point the selected implementation lacks.
pub(crate) struct Optional<T>(std::result::Result<T, &'static CStr>);

impl<T> Optional<T> {
    /// Whether every entry point resolved.
    pub(crate) fn is_available(&self) -> bool {
        self.0.is_ok()
    }
}

impl Runtime {
    /// Reject a capability the selected implementation cannot provide.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] under the capability's name.
    pub(crate) fn require(&self, feature: RuntimeFeature) -> Result<()> {
        match self.capabilities.status(feature) {
            CapabilityStatus::Available => Ok(()),
            CapabilityStatus::Unavailable(reason) => Err(self.unavailable(feature, reason)),
        }
    }

    /// The table of entry points behind an optional capability.
    ///
    /// Nothing else hands out an optional table. A capability's status, which
    /// includes any version gate its exported functions cannot express, is
    /// therefore always applied together with the table, and both failures are
    /// reported under the one name `feature` carries.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] under the capability's name.
    pub(crate) fn capability<T>(
        &self,
        feature: RuntimeFeature,
        table: &'static Optional<T>,
    ) -> Result<&'static T> {
        match (self.capabilities.status(feature), &table.0) {
            (CapabilityStatus::Available, Ok(table)) => Ok(table),
            // The table knows which entry point is missing; the status only
            // knows that one is.
            (_, Err(symbol)) => Err(Error::UnsupportedCapability {
                capability: feature.name().into(),
                reason: format!(
                    "the selected runtime does not export {}",
                    symbol.to_string_lossy()
                ),
            }),
            (CapabilityStatus::Unavailable(reason), Ok(_)) => {
                Err(self.unavailable(feature, reason))
            }
        }
    }

    /// The error for a capability whose status is `reason`.
    fn unavailable(&self, feature: RuntimeFeature, reason: CapabilityUnavailable) -> Error {
        let reason = match (reason, feature.minimum_version()) {
            (
                CapabilityUnavailable::RuntimeTooOld | CapabilityUnavailable::RuntimeVersionUnknown,
                Some(minimum),
            ) => format!(
                "{reason}: it needs NDI {minimum} or later and the runtime reports {:?}",
                self.info.raw_version
            ),
            _ => reason.to_string(),
        };
        Error::UnsupportedCapability {
            capability: feature.name().into(),
            reason,
        }
    }
}

/// Every table resolved from one implementation.
pub(crate) struct Api {
    pub(crate) core: Core,
    pub(crate) receive_delay: Optional<ReceiveDelay>,
    pub(crate) receiver_connection_metadata: Optional<ReceiverConnectionMetadata>,
    pub(crate) receiver_strings: Optional<ReceiverStrings>,
    pub(crate) receiver_web_control: Optional<ReceiverWebControl>,
    pub(crate) receiver_source_name: Optional<ReceiverSourceName>,
    pub(crate) sender_metadata: Optional<SenderMetadata>,
    pub(crate) timestamp: Optional<Timestamp>,
    pub(crate) routing: Optional<Routing>,
    pub(crate) sender_advertising: Optional<SenderAdvertising>,
    pub(crate) receiver_advertising: Optional<ReceiverAdvertising>,
    pub(crate) sender_discovery: Optional<SenderDiscovery>,
    pub(crate) receiver_discovery: Optional<ReceiverDiscovery>,
    pub(crate) interleaved_audio: Optional<InterleavedAudio>,
    pub(crate) interleaved_send: Optional<InterleavedSend>,
    pub(crate) video_conversion: Optional<VideoConversion>,
}

impl Api {
    /// Resolve every table from `resolver`.
    ///
    /// # Errors
    ///
    /// Fails when a [`Core`] entry point is missing or any entry point resolves
    /// outside the selected implementation.
    pub(crate) fn load(resolver: &dyn Resolver) -> Result<Self> {
        let core = Core::load(resolver)?.map_err(|symbol| {
            Error::RuntimeLoadFailed(format!(
                "it does not export the required entry point {}",
                symbol.to_string_lossy()
            ))
        })?;
        Ok(Self {
            core,
            receive_delay: Optional(ReceiveDelay::load(resolver)?),
            receiver_connection_metadata: Optional(ReceiverConnectionMetadata::load(resolver)?),
            receiver_strings: Optional(ReceiverStrings::load(resolver)?),
            receiver_web_control: Optional(ReceiverWebControl::load(resolver)?),
            receiver_source_name: Optional(ReceiverSourceName::load(resolver)?),
            sender_metadata: Optional(SenderMetadata::load(resolver)?),
            timestamp: Optional(Timestamp::load(resolver)?),
            routing: Optional(Routing::load(resolver)?),
            sender_advertising: Optional(SenderAdvertising::load(resolver)?),
            receiver_advertising: Optional(ReceiverAdvertising::load(resolver)?),
            sender_discovery: Optional(SenderDiscovery::load(resolver)?),
            receiver_discovery: Optional(ReceiverDiscovery::load(resolver)?),
            interleaved_audio: Optional(InterleavedAudio::load(resolver)?),
            interleaved_send: Optional(InterleavedSend::load(resolver)?),
            video_conversion: Optional(VideoConversion::load(resolver)?),
        })
    }
}
