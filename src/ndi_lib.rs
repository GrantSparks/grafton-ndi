#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]
#![allow(unused_imports)]

pub(crate) mod raw {
    include!(concat!(env!("OUT_DIR"), "/ndi_lib.rs"));
}
pub use raw::*;
// Explicit imports shadow generated extern declarations, so every legacy call
// uses the selected dispatch table without changing the safe wrapper's behavior.
pub(crate) use crate::dispatch::wrappers::{
    NDIlib_destroy, NDIlib_find_create_v2, NDIlib_find_destroy, NDIlib_find_get_current_sources,
    NDIlib_find_wait_for_sources, NDIlib_framesync_audio_queue_depth,
    NDIlib_framesync_capture_audio_v2, NDIlib_framesync_capture_video, NDIlib_framesync_create,
    NDIlib_framesync_destroy, NDIlib_framesync_free_audio_v2, NDIlib_framesync_free_video,
    NDIlib_initialize, NDIlib_is_supported_CPU, NDIlib_recv_capture_v3, NDIlib_recv_connect,
    NDIlib_recv_create_v3, NDIlib_recv_destroy, NDIlib_recv_free_audio_v3,
    NDIlib_recv_free_metadata, NDIlib_recv_free_video_v2, NDIlib_recv_get_no_connections,
    NDIlib_recv_get_performance, NDIlib_recv_get_queue, NDIlib_recv_ptz_auto_focus,
    NDIlib_recv_ptz_exposure_auto, NDIlib_recv_ptz_exposure_manual,
    NDIlib_recv_ptz_exposure_manual_v2, NDIlib_recv_ptz_focus, NDIlib_recv_ptz_focus_speed,
    NDIlib_recv_ptz_is_supported, NDIlib_recv_ptz_pan_tilt, NDIlib_recv_ptz_pan_tilt_speed,
    NDIlib_recv_ptz_recall_preset, NDIlib_recv_ptz_store_preset,
    NDIlib_recv_ptz_white_balance_auto, NDIlib_recv_ptz_white_balance_indoor,
    NDIlib_recv_ptz_white_balance_manual, NDIlib_recv_ptz_white_balance_oneshot,
    NDIlib_recv_ptz_white_balance_outdoor, NDIlib_recv_ptz_zoom, NDIlib_recv_ptz_zoom_speed,
    NDIlib_recv_send_metadata, NDIlib_recv_set_tally, NDIlib_send_add_connection_metadata,
    NDIlib_send_clear_connection_metadata, NDIlib_send_create, NDIlib_send_destroy,
    NDIlib_send_get_no_connections, NDIlib_send_get_source_name, NDIlib_send_get_tally,
    NDIlib_send_send_audio_v3, NDIlib_send_send_metadata, NDIlib_send_send_video_async_v2,
    NDIlib_send_send_video_v2, NDIlib_send_set_failover, NDIlib_version,
};
