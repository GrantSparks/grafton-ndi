//! A test-owned stand-in for an NDI runtime library.
//!
//! The loader and ownership tests compile this file into a shared library with
//! `rustc` and load it through the crate. It includes the crate's vendored
//! bindings, so every exported function is checked against the declaration the
//! crate's own tables are checked against. It performs no discovery and no network I/O.
//!
//! Variants are selected with `--cfg`:
//!
//! * `fake_objects` — senders, receivers and advertisers are real allocations
//!   whose lifecycle is written to the file named by `GRAFTON_FAKE_NDI_LOG`,
//!   and the optional sender-metadata and advertiser functions are exported;
//! * `fake_advanced` — also export the Advanced SDK entry points;
//! * `fake_receiver_v4` — also export only `NDIlib_recv_create_v4`, as a newer
//!   Standard runtime might;
//! * `fake_omit_required` — omit one required function.
//!
//! `FAKE_NDI_VERSION` in the compiler's environment replaces the version text.

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
#[cfg(fake_objects)]
use std::ffi::c_void;
use std::{
    ffi::{c_char, CString},
    io::Write,
    sync::OnceLock,
};

/// Append one lifecycle event to the log the test named.
fn log(operation: &str) {
    let Some(path) = std::env::var_os("GRAFTON_FAKE_NDI_LOG") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{operation}");
    }
}

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

export! {
    fn NDIlib_initialize() -> bool {
        log("initialize");
        true
    }

    fn NDIlib_destroy() {
        log("destroy");
    }

    fn NDIlib_version() -> *const c_char {
        static VERSION: OnceLock<CString> = OnceLock::new();
        VERSION
            .get_or_init(|| {
                let version = option_env!("FAKE_NDI_VERSION").unwrap_or("NDI SDK TEST 6.3.2.0");
                CString::new(version).expect("version text has no interior NUL")
            })
            .as_ptr()
    }

    fn NDIlib_is_supported_CPU() -> bool {
        true
    }
}

stub! {
    fn NDIlib_find_create_v2(p_create_settings: *const NDIlib_find_create_t) -> NDIlib_find_instance_t;
    fn NDIlib_find_destroy(p_instance: NDIlib_find_instance_t);
    fn NDIlib_find_get_current_sources(p_instance: NDIlib_find_instance_t, p_no_sources: *mut u32) -> *const NDIlib_source_t;
    fn NDIlib_find_wait_for_sources(p_instance: NDIlib_find_instance_t, timeout_in_ms: u32) -> bool;
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
    fn NDIlib_send_send_video_v2(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t);
    fn NDIlib_send_send_audio_v3(p_instance: NDIlib_send_instance_t, p_audio_data: *const NDIlib_audio_frame_v3_t);
    fn NDIlib_send_send_metadata(p_instance: NDIlib_send_instance_t, p_metadata: *const NDIlib_metadata_frame_t);
    fn NDIlib_send_get_tally(p_instance: NDIlib_send_instance_t, p_tally: *mut NDIlib_tally_t, timeout_in_ms: u32) -> bool;
    fn NDIlib_send_get_no_connections(p_instance: NDIlib_send_instance_t, timeout_in_ms: u32) -> ::std::os::raw::c_int;
    fn NDIlib_send_clear_connection_metadata(p_instance: NDIlib_send_instance_t);
    fn NDIlib_send_add_connection_metadata(p_instance: NDIlib_send_instance_t, p_metadata: *const NDIlib_metadata_frame_t);
    fn NDIlib_send_get_source_name(p_instance: NDIlib_send_instance_t) -> *const NDIlib_source_t;
    fn NDIlib_framesync_create(p_receiver: NDIlib_recv_instance_t) -> NDIlib_framesync_instance_t;
    fn NDIlib_framesync_destroy(p_instance: NDIlib_framesync_instance_t);
    fn NDIlib_framesync_capture_audio_v2(p_instance: NDIlib_framesync_instance_t, p_audio_data: *mut NDIlib_audio_frame_v3_t, sample_rate: ::std::os::raw::c_int, no_channels: ::std::os::raw::c_int, no_samples: ::std::os::raw::c_int);
    fn NDIlib_framesync_free_audio_v2(p_instance: NDIlib_framesync_instance_t, p_audio_data: *mut NDIlib_audio_frame_v3_t);
    fn NDIlib_framesync_audio_queue_depth(p_instance: NDIlib_framesync_instance_t) -> ::std::os::raw::c_int;
    fn NDIlib_framesync_capture_video(p_instance: NDIlib_framesync_instance_t, p_video_data: *mut NDIlib_video_frame_v2_t, field_type: NDIlib_frame_format_type_e);
    fn NDIlib_framesync_free_video(p_instance: NDIlib_framesync_instance_t, p_video_data: *mut NDIlib_video_frame_v2_t);
}

#[cfg(not(fake_omit_required))]
stub! {
    fn NDIlib_send_set_failover(p_instance: NDIlib_send_instance_t, p_failover_source: *const NDIlib_source_t);
}

#[cfg(not(fake_objects))]
stub! {
    fn NDIlib_recv_create_v3(p_create_settings: *const NDIlib_recv_create_v3_t) -> NDIlib_recv_instance_t;
    fn NDIlib_recv_destroy(p_instance: NDIlib_recv_instance_t);
    fn NDIlib_send_create(p_create_settings: *const NDIlib_send_create_t) -> NDIlib_send_instance_t;
    fn NDIlib_send_destroy(p_instance: NDIlib_send_instance_t);
    fn NDIlib_send_send_video_async_v2(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t);
}

/// The completion callback the Advanced SDK invokes for an async video frame.
#[cfg(fake_objects)]
type Completion = unsafe extern "C" fn(*mut c_void, *const NDIlib_video_frame_v2_t);

#[cfg(fake_objects)]
mod objects {
    use super::*;

    /// The most frames the fake retains before treating a test as runaway.
    const PENDING_LIMIT: usize = 128;

    pub(super) struct Sender {
        pub(super) opaque: *mut c_void,
        pub(super) completion: Option<Completion>,
        pending: Vec<NDIlib_video_frame_v2_t>,
    }

    impl Sender {
        /// Report every retained frame as complete, most recent first.
        unsafe fn complete(&mut self) {
            let Some(completion) = self.completion else {
                return;
            };
            while let Some(frame) = self.pending.pop() {
                log("sender-completion");
                unsafe { completion(self.opaque, &frame) };
            }
        }
    }

    /// Allocate a distinct non-null handle of pointer type `T`.
    fn handle<T>() -> *mut T {
        Box::into_raw(Box::new(0u8)).cast()
    }

    /// Release a handle allocated by [`handle`].
    unsafe fn release<T>(instance: *mut T) {
        drop(unsafe { Box::from_raw(instance.cast::<u8>()) });
    }

    export! {
        fn NDIlib_recv_create_v3(p_create_settings: *const NDIlib_recv_create_v3_t) -> NDIlib_recv_instance_t {
            log("receiver-create");
            handle()
        }

        fn NDIlib_recv_destroy(p_instance: NDIlib_recv_instance_t) {
            log("receiver-destroy");
            unsafe { release(p_instance) };
        }

        fn NDIlib_send_create(p_create_settings: *const NDIlib_send_create_t) -> NDIlib_send_instance_t {
            log("sender-create");
            Box::into_raw(Box::new(Sender {
                opaque: std::ptr::null_mut(),
                completion: None,
                pending: Vec::new(),
            }))
            .cast()
        }

        fn NDIlib_send_destroy(p_instance: NDIlib_send_instance_t) {
            let mut sender = unsafe { Box::from_raw(p_instance.cast::<Sender>()) };
            log("sender-destroy-begin");
            unsafe { sender.complete() };
            log("sender-destroy");
        }

        fn NDIlib_send_send_video_async_v2(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t) {
            let sender = unsafe { &mut *p_instance.cast::<Sender>() };
            if p_video_data.is_null() {
                log("sender-flush");
                unsafe { sender.complete() };
                return;
            }
            log("sender-submit");
            let Some(completion) = sender.completion else {
                return;
            };
            if std::env::var_os("GRAFTON_FAKE_CALLBACK_MODE").is_some_and(|mode| mode == "immediate") {
                log("sender-completion");
                unsafe { completion(sender.opaque, p_video_data) };
                // The submitting call must still own its header and pixels.
                let _first_byte = unsafe { (*p_video_data).p_data.read_volatile() };
                log("sender-submit-return");
            } else {
                assert!(sender.pending.len() < PENDING_LIMIT, "runaway async submissions");
                sender.pending.push(unsafe { *p_video_data });
            }
        }

        fn NDIlib_send_capture(p_instance: NDIlib_send_instance_t, p_metadata: *mut NDIlib_metadata_frame_t, timeout_in_ms: u32) -> NDIlib_frame_type_e {
            // Deliberately not UTF-8, so the crate must free it on rejection.
            let text = Box::into_raw(Box::new([0xffu8, 0]));
            unsafe {
                (*p_metadata).length = 2;
                (*p_metadata).timecode = 123;
                (*p_metadata).p_data = text.cast();
            }
            log("sender-metadata-capture");
            NDIlib_frame_type_e_NDIlib_frame_type_metadata
        }

        fn NDIlib_send_free_metadata(p_instance: NDIlib_send_instance_t, p_metadata: *const NDIlib_metadata_frame_t) {
            log("sender-metadata-free");
            drop(unsafe { Box::from_raw((*p_metadata).p_data.cast::<[u8; 2]>()) });
        }

        fn NDIlib_send_advertiser_create(p_create_settings: *const NDIlib_send_advertiser_create_t) -> NDIlib_send_advertiser_instance_t {
            log("send-advertiser-create");
            handle()
        }

        fn NDIlib_send_advertiser_destroy(p_instance: NDIlib_send_advertiser_instance_t) {
            log("send-advertiser-destroy");
            unsafe { release(p_instance) };
        }

        fn NDIlib_send_advertiser_add_sender(p_instance: NDIlib_send_advertiser_instance_t, p_sender: NDIlib_send_instance_t, allow_monitoring: bool) -> bool {
            log(if allow_monitoring { "send-advertiser-add-monitoring" } else { "send-advertiser-add" });
            true
        }

        fn NDIlib_send_advertiser_del_sender(p_instance: NDIlib_send_advertiser_instance_t, p_sender: NDIlib_send_instance_t) -> bool {
            log("send-advertiser-remove");
            true
        }

        fn NDIlib_recv_advertiser_create(p_create_settings: *const NDIlib_recv_advertiser_create_t) -> NDIlib_recv_advertiser_instance_t {
            log("recv-advertiser-create");
            handle()
        }

        fn NDIlib_recv_advertiser_destroy(p_instance: NDIlib_recv_advertiser_instance_t) {
            log("recv-advertiser-destroy");
            unsafe { release(p_instance) };
        }

        fn NDIlib_recv_advertiser_add_receiver(p_instance: NDIlib_recv_advertiser_instance_t, p_receiver: NDIlib_recv_instance_t, allow_controlling: bool, allow_monitoring: bool, p_input_group_name: *const c_char) -> bool {
            log("recv-advertiser-add");
            true
        }

        fn NDIlib_recv_advertiser_del_receiver(p_instance: NDIlib_recv_advertiser_instance_t, p_receiver: NDIlib_recv_instance_t) -> bool {
            log("recv-advertiser-remove");
            true
        }
    }
}

// The Advanced SDK entry points are declared in headers that are not vendored,
// so these definitions state the SDK's signatures themselves.

#[cfg(all(fake_objects, any(fake_advanced, fake_receiver_v4)))]
use objects::NDIlib_recv_create_v3;
#[cfg(all(fake_objects, fake_advanced))]
use objects::NDIlib_send_create;

#[cfg(any(fake_advanced, fake_receiver_v4))]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_recv_create_v4(
    p_create_settings: *const NDIlib_recv_create_v3_t,
    p_config_data: *const c_char,
) -> NDIlib_recv_instance_t {
    unsafe { NDIlib_recv_create_v3(p_create_settings) }
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_send_create_v2(
    p_create_settings: *const NDIlib_send_create_t,
    p_config_data: *const c_char,
) -> NDIlib_send_instance_t {
    unsafe { NDIlib_send_create(p_create_settings) }
}

#[cfg(all(fake_advanced, fake_objects))]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_send_set_video_async_completion(
    p_instance: NDIlib_send_instance_t,
    p_opaque: *mut c_void,
    p_deallocator: Option<Completion>,
) {
    let sender = unsafe { &mut *p_instance.cast::<objects::Sender>() };
    sender.opaque = p_opaque;
    sender.completion = p_deallocator;
}
