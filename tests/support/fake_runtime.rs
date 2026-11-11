//! A test-owned stand-in for an NDI runtime library.
//!
//! The loader and ownership tests compile this file into a shared library with
//! `rustc` and load it through the crate. It includes the crate's vendored
//! bindings, so every exported function is checked against the declaration the
//! crate's own tables are checked against. It performs no discovery and no network I/O.
//!
//! Variants are selected with `--cfg`:
//!
//! * `fake_objects` — senders, receivers, routers and advertisers are real
//!   allocations whose lifecycle is written to the file named by
//!   `GRAFTON_FAKE_NDI_LOG`, and the optional sender-metadata, routing and
//!   advertiser functions are exported. Registrations log the flags they were
//!   given, and a receiver's input-group name is read only when its
//!   registration ends;
//! * `fake_discovery` — export the sender and receiver listener functions over
//!   a fixed membership whose identifiers and text are partly not UTF-8, and
//!   log every identifier passed back, byte for byte. An event batch holds
//!   `GRAFTON_FAKE_EVENT_COUNT` events (two by default);
//! * `fake_advanced` — also export the Advanced SDK entry points;
//! * `fake_receiver_v4` — also export only `NDIlib_recv_create_v4`, as a newer
//!   Standard runtime might;
//! * `fake_omit_required` — omit one required function.
//!
//! Like the SDK, advertisers and listeners are created only for a Discovery
//! Server address; this runtime has no configuration to supply one.
//!
//! `FAKE_NDI_VERSION` in the compiler's environment replaces the version text.
//!
//! Two variables are read each time the function they affect is called, so a
//! test can change the outcome while the library is loaded:
//! `GRAFTON_FAKE_NDI_INITIALIZE=fail` makes initialization fail, and
//! `GRAFTON_FAKE_NDI_CPU=unsupported` makes the runtime report a CPU it
//! cannot run on.

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
#[cfg(any(fake_objects, fake_discovery))]
use std::ffi::CStr;
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

/// A string argument as the bytes it holds, or `null`.
#[cfg(any(fake_objects, fake_discovery))]
unsafe fn bytes(text: *const c_char) -> String {
    if text.is_null() {
        return "null".into();
    }
    unsafe { CStr::from_ptr(text) }
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether creation settings name a Discovery Server, which the SDK needs
/// before it creates an advertiser or a listener.
#[cfg(any(fake_objects, fake_discovery))]
macro_rules! names_a_server {
    ($settings:expr) => {
        !$settings.is_null() && !unsafe { (*$settings).p_url_address }.is_null()
    };
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
        std::env::var_os("GRAFTON_FAKE_NDI_INITIALIZE").is_none_or(|outcome| outcome != "fail")
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
        std::env::var_os("GRAFTON_FAKE_NDI_CPU").is_none_or(|cpu| cpu != "unsupported")
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

    /// A receiver advertiser. It keeps the input-group name of each
    /// registration as the pointer it was given and reads it only when the
    /// registration ends, as an implementation that does not copy the name
    /// would: a name released early is then read after its release.
    struct RecvAdvertiser {
        groups: Vec<(NDIlib_recv_instance_t, *const c_char)>,
    }

    /// A routing source and the descriptor handed out for it.
    struct Router {
        name: CString,
        source: NDIlib_source_t,
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
            if !names_a_server!(p_create_settings) {
                return std::ptr::null_mut();
            }
            log("send-advertiser-create");
            handle()
        }

        fn NDIlib_send_advertiser_destroy(p_instance: NDIlib_send_advertiser_instance_t) {
            log("send-advertiser-destroy");
            unsafe { release(p_instance) };
        }

        fn NDIlib_send_advertiser_add_sender(p_instance: NDIlib_send_advertiser_instance_t, p_sender: NDIlib_send_instance_t, allow_monitoring: bool) -> bool {
            log(&format!("send-advertiser-add monitoring={allow_monitoring}"));
            true
        }

        fn NDIlib_send_advertiser_del_sender(p_instance: NDIlib_send_advertiser_instance_t, p_sender: NDIlib_send_instance_t) -> bool {
            log("send-advertiser-remove");
            true
        }

        fn NDIlib_recv_advertiser_create(p_create_settings: *const NDIlib_recv_advertiser_create_t) -> NDIlib_recv_advertiser_instance_t {
            if !names_a_server!(p_create_settings) {
                return std::ptr::null_mut();
            }
            log("recv-advertiser-create");
            Box::into_raw(Box::new(RecvAdvertiser { groups: Vec::new() })).cast()
        }

        fn NDIlib_recv_advertiser_destroy(p_instance: NDIlib_recv_advertiser_instance_t) {
            log("recv-advertiser-destroy");
            drop(unsafe { Box::from_raw(p_instance.cast::<RecvAdvertiser>()) });
        }

        fn NDIlib_recv_advertiser_add_receiver(p_instance: NDIlib_recv_advertiser_instance_t, p_receiver: NDIlib_recv_instance_t, allow_controlling: bool, allow_monitoring: bool, p_input_group_name: *const c_char) -> bool {
            log(&format!("recv-advertiser-add controlling={allow_controlling} monitoring={allow_monitoring}"));
            let advertiser = unsafe { &mut *p_instance.cast::<RecvAdvertiser>() };
            advertiser.groups.push((p_receiver, p_input_group_name));
            true
        }

        fn NDIlib_recv_advertiser_del_receiver(p_instance: NDIlib_recv_advertiser_instance_t, p_receiver: NDIlib_recv_instance_t) -> bool {
            let advertiser = unsafe { &mut *p_instance.cast::<RecvAdvertiser>() };
            let Some(index) = advertiser.groups.iter().position(|(receiver, _)| *receiver == p_receiver) else {
                return false;
            };
            let (_, group) = advertiser.groups.swap_remove(index);
            log(&format!("recv-advertiser-remove group={}", unsafe { bytes(group) }));
            true
        }

        fn NDIlib_routing_create(p_create_settings: *const NDIlib_routing_create_t) -> NDIlib_routing_instance_t {
            log("router-create");
            let name = unsafe { CStr::from_ptr((*p_create_settings).p_ndi_name) }.to_owned();
            // The name's heap buffer does not move when the router is boxed.
            let source = NDIlib_source_t {
                p_ndi_name: name.as_ptr(),
                ..unsafe { std::mem::zeroed() }
            };
            Box::into_raw(Box::new(Router { name, source })).cast()
        }

        fn NDIlib_routing_destroy(p_instance: NDIlib_routing_instance_t) {
            log("router-destroy");
            drop(unsafe { Box::from_raw(p_instance.cast::<Router>()) });
        }

        fn NDIlib_routing_change(p_instance: NDIlib_routing_instance_t, p_source: *const NDIlib_source_t) -> bool {
            let name = unsafe { CStr::from_ptr((*p_source).p_ndi_name) };
            log(&format!("router-change {}", name.to_string_lossy()));
            true
        }

        fn NDIlib_routing_clear(p_instance: NDIlib_routing_instance_t) -> bool {
            log("router-clear");
            true
        }

        fn NDIlib_routing_get_no_connections(p_instance: NDIlib_routing_instance_t, timeout_in_ms: u32) -> ::std::os::raw::c_int {
            0
        }

        fn NDIlib_routing_get_source_name(p_instance: NDIlib_routing_instance_t) -> *const NDIlib_source_t {
            let router = unsafe { &*p_instance.cast::<Router>() };
            debug_assert_eq!(router.source.p_ndi_name, router.name.as_ptr());
            &router.source
        }
    }
}

#[cfg(fake_discovery)]
mod discovery {
    use super::*;
    use std::ptr;

    // What the fixed membership advertises. Text marked malformed is not UTF-8,
    // as a misbehaving peer might send it; the tests know these exact bytes.
    const SENDER_ID: &CStr = c"sender-\xff\xfe";
    const SENDER_NAME: &CStr = c"STUDIO (Caf\xe9)";
    const SENDER_GROUP: &CStr = c"gro\xffup";
    const RECEIVER_ID: &CStr = c"receiver-\xfe\xff";
    const RECEIVER_NAME: &CStr = c"Monitor \xe9";
    const INPUT_ID: &CStr = c"input-\xfd";
    const EVENT_VALUE: &CStr = c"<tally on=\"\xff\"/>";

    /// The events most recently handed out, which stay allocated until freed.
    type Batch = Vec<NDIlib_listener_event>;

    struct SendListener {
        senders: [NDIlib_sender_t; 2],
        _groups: Box<[*const c_char; 3]>,
        batch: Batch,
    }

    struct RecvListener {
        receivers: [NDIlib_receiver_t; 2],
        _streams: Box<[NDIlib_receiver_type_e; 3]>,
        _commands: Box<[NDIlib_receiver_command_e; 2]>,
        batch: Batch,
    }

    /// Fill `batch` with events and hand it out, or hand out nothing when the
    /// caller does not wait. Events alternate between one from `uuid` and one
    /// with no fields.
    unsafe fn events(
        batch: &mut Batch,
        uuid: &'static CStr,
        p_num_events: *mut u32,
        timeout_in_ms: u32,
    ) -> *const NDIlib_listener_event {
        if timeout_in_ms == 0 {
            return ptr::null();
        }
        let count = std::env::var("GRAFTON_FAKE_EVENT_COUNT").map_or(2, |count| {
            count
                .parse::<u32>()
                .expect("GRAFTON_FAKE_EVENT_COUNT is a count")
        });
        *batch = (0..count)
            .map(|index| {
                if index % 2 == 0 {
                    NDIlib_listener_event {
                        p_uuid: uuid.as_ptr(),
                        p_name: c"tally".as_ptr(),
                        p_value: EVENT_VALUE.as_ptr(),
                    }
                } else {
                    NDIlib_listener_event {
                        p_uuid: ptr::null(),
                        p_name: ptr::null(),
                        p_value: ptr::null(),
                    }
                }
            })
            .collect();
        unsafe { *p_num_events = count };
        batch.as_ptr()
    }

    /// Release the batch `p_events` was handed out from.
    unsafe fn free_events(batch: &mut Batch, p_events: *const NDIlib_listener_event) {
        assert_eq!(batch.as_ptr(), p_events, "freed a batch not handed out");
        *batch = Batch::new();
    }

    export! {
        fn NDIlib_send_listener_create(p_create_settings: *const NDIlib_send_listener_create_t) -> NDIlib_send_listener_instance_t {
            if !names_a_server!(p_create_settings) {
                return ptr::null_mut();
            }
            log("send-listener-create");
            let mut groups = Box::new([c"public".as_ptr(), SENDER_GROUP.as_ptr(), ptr::null()]);
            let senders = [
                NDIlib_sender_t {
                    p_uuid: SENDER_ID.as_ptr(),
                    p_name: SENDER_NAME.as_ptr(),
                    p_metadata: c"<ndi_product/>".as_ptr(),
                    p_address: c"192.0.2.10".as_ptr(),
                    port: 5961,
                    p_groups: groups.as_mut_ptr(),
                    num_groups: 2,
                    events_subscribed: false,
                },
                NDIlib_sender_t {
                    p_uuid: c"sender-plain".as_ptr(),
                    p_name: c"STUDIO (Camera 2)".as_ptr(),
                    p_metadata: ptr::null(),
                    p_address: ptr::null(),
                    port: -1,
                    p_groups: ptr::null_mut(),
                    num_groups: 0,
                    events_subscribed: true,
                },
            ];
            Box::into_raw(Box::new(SendListener { senders, _groups: groups, batch: Batch::new() })).cast()
        }

        fn NDIlib_send_listener_destroy(p_instance: NDIlib_send_listener_instance_t) {
            log("send-listener-destroy");
            drop(unsafe { Box::from_raw(p_instance.cast::<SendListener>()) });
        }

        fn NDIlib_send_listener_is_connected(p_instance: NDIlib_send_listener_instance_t) -> bool {
            true
        }

        fn NDIlib_send_listener_get_server_url(p_instance: NDIlib_send_listener_instance_t) -> *const c_char {
            c"192.0.2.1:5959".as_ptr()
        }

        fn NDIlib_send_listener_get_senders(p_instance: NDIlib_send_listener_instance_t, p_num_senders: *mut u32) -> *const NDIlib_sender_t {
            let listener = unsafe { &*p_instance.cast::<SendListener>() };
            unsafe { *p_num_senders = listener.senders.len() as u32 };
            listener.senders.as_ptr()
        }

        fn NDIlib_send_listener_wait_for_senders(p_instance: NDIlib_send_listener_instance_t, timeout_in_ms: u32) -> bool {
            false
        }

        fn NDIlib_send_listener_subscribe_events(p_instance: NDIlib_send_listener_instance_t, p_sender_uuid: *const c_char) {
            log(&format!("send-listener-subscribe {}", unsafe { bytes(p_sender_uuid) }));
        }

        fn NDIlib_send_listener_unsubscribe_events(p_instance: NDIlib_send_listener_instance_t, p_sender_uuid: *const c_char) {
            log(&format!("send-listener-unsubscribe {}", unsafe { bytes(p_sender_uuid) }));
        }

        fn NDIlib_send_listener_get_events(p_instance: NDIlib_send_listener_instance_t, p_num_events: *mut u32, timeout_in_ms: u32) -> *const NDIlib_send_listener_event {
            log(&format!("send-listener-events {timeout_in_ms}"));
            let listener = unsafe { &mut *p_instance.cast::<SendListener>() };
            unsafe { events(&mut listener.batch, SENDER_ID, p_num_events, timeout_in_ms) }
        }

        fn NDIlib_send_listener_free_events(p_instance: NDIlib_send_listener_instance_t, p_events: *const NDIlib_send_listener_event) {
            log("send-listener-free-events");
            let listener = unsafe { &mut *p_instance.cast::<SendListener>() };
            unsafe { free_events(&mut listener.batch, p_events) };
        }

        fn NDIlib_recv_listener_create(p_create_settings: *const NDIlib_recv_listener_create_t) -> NDIlib_recv_listener_instance_t {
            if !names_a_server!(p_create_settings) {
                return ptr::null_mut();
            }
            log("recv-listener-create");
            let mut streams = Box::new([
                NDIlib_receiver_type_e_NDIlib_receiver_type_video,
                NDIlib_receiver_type_e_NDIlib_receiver_type_audio,
                NDIlib_receiver_type_e_NDIlib_receiver_type_none,
            ]);
            let mut commands = Box::new([
                NDIlib_receiver_command_e_NDIlib_receiver_command_connect,
                NDIlib_receiver_command_e_NDIlib_receiver_command_none,
            ]);
            let receivers = [
                NDIlib_receiver_t {
                    p_uuid: RECEIVER_ID.as_ptr(),
                    p_name: RECEIVER_NAME.as_ptr(),
                    p_input_uuid: INPUT_ID.as_ptr(),
                    p_input_name: c"Program".as_ptr(),
                    p_address: c"192.0.2.20".as_ptr(),
                    p_streams: streams.as_mut_ptr(),
                    num_streams: 2,
                    p_commands: commands.as_mut_ptr(),
                    num_commands: 1,
                    events_subscribed: false,
                },
                NDIlib_receiver_t {
                    p_uuid: c"receiver-fixed".as_ptr(),
                    p_name: c"Fixed".as_ptr(),
                    p_input_uuid: ptr::null(),
                    p_input_name: ptr::null(),
                    p_address: ptr::null(),
                    p_streams: ptr::null_mut(),
                    num_streams: 0,
                    p_commands: ptr::null_mut(),
                    num_commands: 0,
                    events_subscribed: false,
                },
            ];
            Box::into_raw(Box::new(RecvListener { receivers, _streams: streams, _commands: commands, batch: Batch::new() })).cast()
        }

        fn NDIlib_recv_listener_destroy(p_instance: NDIlib_recv_listener_instance_t) {
            log("recv-listener-destroy");
            drop(unsafe { Box::from_raw(p_instance.cast::<RecvListener>()) });
        }

        fn NDIlib_recv_listener_is_connected(p_instance: NDIlib_recv_listener_instance_t) -> bool {
            true
        }

        fn NDIlib_recv_listener_get_server_url(p_instance: NDIlib_recv_listener_instance_t) -> *const c_char {
            c"192.0.2.1:5959".as_ptr()
        }

        fn NDIlib_recv_listener_get_receivers(p_instance: NDIlib_recv_listener_instance_t, p_num_receivers: *mut u32) -> *const NDIlib_receiver_t {
            let listener = unsafe { &*p_instance.cast::<RecvListener>() };
            unsafe { *p_num_receivers = listener.receivers.len() as u32 };
            listener.receivers.as_ptr()
        }

        fn NDIlib_recv_listener_wait_for_receivers(p_instance: NDIlib_recv_listener_instance_t, timeout_in_ms: u32) -> bool {
            false
        }

        fn NDIlib_recv_listener_subscribe_events(p_instance: NDIlib_recv_listener_instance_t, p_receiver_uuid: *const c_char) {
            log(&format!("recv-listener-subscribe {}", unsafe { bytes(p_receiver_uuid) }));
        }

        fn NDIlib_recv_listener_unsubscribe_events(p_instance: NDIlib_recv_listener_instance_t, p_receiver_uuid: *const c_char) {
            log(&format!("recv-listener-unsubscribe {}", unsafe { bytes(p_receiver_uuid) }));
        }

        fn NDIlib_recv_listener_get_events(p_instance: NDIlib_recv_listener_instance_t, p_num_events: *mut u32, timeout_in_ms: u32) -> *const NDIlib_recv_listener_event {
            log(&format!("recv-listener-events {timeout_in_ms}"));
            let listener = unsafe { &mut *p_instance.cast::<RecvListener>() };
            unsafe { events(&mut listener.batch, RECEIVER_ID, p_num_events, timeout_in_ms) }
        }

        fn NDIlib_recv_listener_free_events(p_instance: NDIlib_recv_listener_instance_t, p_events: *const NDIlib_recv_listener_event) {
            log("recv-listener-free-events");
            let listener = unsafe { &mut *p_instance.cast::<RecvListener>() };
            unsafe { free_events(&mut listener.batch, p_events) };
        }

        fn NDIlib_recv_listener_send_connect(p_instance: NDIlib_recv_listener_instance_t, p_receiver_uuid: *const c_char, p_source_name: *const c_char) -> bool {
            log(&format!("recv-listener-connect {} {}", unsafe { bytes(p_receiver_uuid) }, unsafe { bytes(p_source_name) }));
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
