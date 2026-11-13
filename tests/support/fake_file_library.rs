//! A test-owned stand-in for the NDI File library.
//!
//! The File loader tests compile this file into a shared library with `rustc`,
//! link it against a fake NDI runtime, and load it through the crate. Like the
//! vendor's library it imports entry points of the NDI runtime, which the
//! platform loader binds when the library is loaded, and names that runtime as
//! a dependency. It includes the crate's vendored bindings, so every exported
//! function is checked against the declaration the crate's own tables are
//! checked against.
//!
//! A probe succeeds and reports, as the file's track count, the build number of
//! the runtime the library's imports are bound to, so a test can tell which
//! runtime that is. Every other function does nothing and returns zero, false
//! or null.
//!
//! `--cfg fake_omit_recording_entry` omits one function of the recording family.

#![allow(non_snake_case, unused_variables, clippy::missing_safety_doc)]

#[rustfmt::skip]
#[path = "../../src/sys/bindings.rs"]
#[allow(non_upper_case_globals, non_camel_case_types, dead_code, unused_imports)]
mod bindings;
#[rustfmt::skip]
#[path = "../../src/sys/declarations.rs"]
#[allow(dead_code)]
mod declarations;

use bindings::*;
use std::ffi::{c_char, CStr};

/// Export functions under their SDK names, each checked against the vendored
/// declaration of the same name.
macro_rules! export {
    ($(fn $name:ident($($argument:ident: $type:ty),* $(,)?) $(-> $return:ty)? $body:block)+) => {$(
        #[no_mangle]
        pub unsafe extern "C" fn $name($($argument: $type),*) $(-> $return)? $body
        const _: unsafe extern "C" fn($($type),*) $(-> $return)? = declarations::$name;
    )+};
}

/// Export functions that do nothing and return zero, false or null.
macro_rules! stub {
    ($(fn $name:ident($($argument:ident: $type:ty),* $(,)?) $(-> $return:ty)?;)+) => {
        export! {$(
            fn $name($($argument: $type),*) $(-> $return)? {
                // SAFETY: every stubbed return type is an integer, a boolean or
                // a raw pointer, for which all-zero bytes are a valid value.
                unsafe { std::mem::zeroed() }
            }
        )+}
    };
}

/// The build number of the NDI runtime this library's imports are bound to:
/// the last component of the version that runtime reports.
fn bound_runtime_build() -> u32 {
    // SAFETY: the query takes no arguments and returns a static string. The
    // call goes through this library's own import of the runtime.
    let version = unsafe { CStr::from_ptr(declarations::NDIlib_version()) };
    version
        .to_str()
        .ok()
        .and_then(|text| text.rsplit('.').next())
        .and_then(|build| build.parse().ok())
        .unwrap_or(0)
}

export! {
    fn NDIlib_file_player_probe_file(p_file_path: *const c_char, p_out_info: *mut NDIlib_file_player_file_info_t) -> bool {
        // SAFETY: the caller passes storage for one structure.
        unsafe { (*p_out_info).track_count = bound_runtime_build() };
        true
    }
}

stub! {
    fn NDIlib_file_v1_load() -> *const NDIlib_file_v1;

    fn NDIlib_file_player_create(p_create_settings: *const NDIlib_file_player_create_t) -> NDIlib_file_player_instance_t;
    fn NDIlib_file_player_destroy(p_instance: NDIlib_file_player_instance_t);
    fn NDIlib_file_player_start(p_instance: NDIlib_file_player_instance_t, p_file_path: *const c_char) -> bool;
    fn NDIlib_file_player_stop(p_instance: NDIlib_file_player_instance_t);
    fn NDIlib_file_player_is_running(p_instance: NDIlib_file_player_instance_t) -> bool;
    fn NDIlib_file_player_capture_event(p_instance: NDIlib_file_player_instance_t, p_out_event: *mut NDIlib_file_player_event_t, timeout_in_ms: u32) -> NDIlib_file_player_event_type_e;
    fn NDIlib_file_player_get_dropped_event_count(p_instance: NDIlib_file_player_instance_t) -> u64;
    fn NDIlib_file_player_get_stats(p_instance: NDIlib_file_player_instance_t, p_out_stats: *mut NDIlib_file_player_stats_t) -> bool;
    fn NDIlib_file_player_seek_to_time(p_instance: NDIlib_file_player_instance_t, time_100ns: u64) -> bool;
    fn NDIlib_file_player_seek_to_frame(p_instance: NDIlib_file_player_instance_t, frame_index: u64) -> bool;
    fn NDIlib_file_player_set_paused(p_instance: NDIlib_file_player_instance_t, paused: bool);
    fn NDIlib_file_player_get_paused(p_instance: NDIlib_file_player_instance_t) -> bool;
    fn NDIlib_file_player_set_loop(p_instance: NDIlib_file_player_instance_t, loop_: bool);
    fn NDIlib_file_player_get_loop(p_instance: NDIlib_file_player_instance_t) -> bool;
    fn NDIlib_file_player_get_current_duration(p_instance: NDIlib_file_player_instance_t, p_out_duration: *mut u64, p_out_is_final: *mut bool) -> bool;
    fn NDIlib_file_player_get_current_position(p_instance: NDIlib_file_player_instance_t, p_out_position: *mut u64) -> bool;
    fn NDIlib_file_player_get_current_source_timecode(p_instance: NDIlib_file_player_instance_t, p_out_timecode_100ns: *mut i64) -> bool;
    fn NDIlib_file_player_get_source_format(p_instance: NDIlib_file_player_instance_t, p_out_format: *mut NDIlib_file_source_format_t) -> bool;
    fn NDIlib_file_player_get_advertised_source(p_instance: NDIlib_file_player_instance_t) -> *const NDIlib_source_t;

    fn NDIlib_file_recorder_create(p_create_settings: *const NDIlib_file_recorder_create_t) -> NDIlib_file_recorder_instance_t;
    fn NDIlib_file_recorder_destroy(p_instance: NDIlib_file_recorder_instance_t);
    fn NDIlib_file_recorder_start(p_instance: NDIlib_file_recorder_instance_t, p_output_prefix: *const c_char) -> bool;
    fn NDIlib_file_recorder_start_with_timecode(p_instance: NDIlib_file_recorder_instance_t, p_output_prefix: *const c_char, start_timecode_100ns: i64) -> bool;
    fn NDIlib_file_recorder_capture_event(p_instance: NDIlib_file_recorder_instance_t, p_out_event: *mut NDIlib_file_recorder_event_t, timeout_in_ms: u32) -> NDIlib_file_recorder_event_type_e;
    fn NDIlib_file_recorder_get_dropped_event_count(p_instance: NDIlib_file_recorder_instance_t) -> u64;
    fn NDIlib_file_recorder_stop(p_instance: NDIlib_file_recorder_instance_t);
    fn NDIlib_file_recorder_inject_metadata(p_instance: NDIlib_file_recorder_instance_t, p_metadata: *const NDIlib_metadata_frame_t) -> bool;
    fn NDIlib_file_recorder_set_source(p_instance: NDIlib_file_recorder_instance_t, p_source: *const NDIlib_file_recorder_source_t) -> bool;
    fn NDIlib_file_recorder_get_source(p_instance: NDIlib_file_recorder_instance_t, p_out_source: *mut NDIlib_file_recorder_source_t) -> bool;
    fn NDIlib_file_recorder_is_recording(p_instance: NDIlib_file_recorder_instance_t) -> bool;
    fn NDIlib_file_recorder_get_current_file(p_instance: NDIlib_file_recorder_instance_t, p_out_path: *mut *const c_char) -> bool;
    fn NDIlib_file_recorder_get_stats(p_instance: NDIlib_file_recorder_instance_t, p_out_stats: *mut NDIlib_file_recorder_stats_t) -> bool;
    fn NDIlib_file_recorder_get_source_format(p_instance: NDIlib_file_recorder_instance_t, p_out_format: *mut NDIlib_file_source_format_t) -> bool;
}

#[cfg(not(fake_omit_recording_entry))]
stub! {
    fn NDIlib_file_recorder_rollover(p_instance: NDIlib_file_recorder_instance_t, p_new_output_prefix: *const c_char) -> bool;
}
