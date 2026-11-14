//! Clocked capture and remote-control boundaries without a native SDK.
use super::*;

struct SyncState {
    receiver: NDIlib_recv_instance_t,
}

fn refused() -> bool {
    std::env::var_os("GRAFTON_FAKE_CONTROL_RESULT").is_some_and(|result| result == "refuse")
}

unsafe fn state(instance: NDIlib_framesync_instance_t) -> &'static SyncState {
    unsafe { &*instance.cast::<SyncState>() }
}

export! {
    fn NDIlib_recv_set_tally(receiver: NDIlib_recv_instance_t, tally: *const NDIlib_tally_t) -> bool {
        let tally = unsafe { &*tally };
        log(&format!("tally:{receiver:p}:{}:{}", tally.on_program, tally.on_preview));
        true
    }
    fn NDIlib_framesync_create(receiver: NDIlib_recv_instance_t) -> NDIlib_framesync_instance_t {
        let instance = Box::into_raw(Box::new(SyncState { receiver })).cast();
        log(&format!("fs-create:{receiver:p}:{instance:p}"));
        instance
    }
    fn NDIlib_framesync_destroy(instance: NDIlib_framesync_instance_t) {
        log(&format!("fs-destroy:{instance:p}"));
        drop(unsafe { Box::from_raw(instance.cast::<SyncState>()) });
    }
    fn NDIlib_framesync_capture_video(instance: NDIlib_framesync_instance_t, frame: *mut NDIlib_video_frame_v2_t, field: NDIlib_frame_format_type_e) {
        static PIXELS: [u8; 32] = [37; 32];
        let receiver = unsafe { state(instance).receiver };
        log(&format!("fs-video:{receiver:p}:{instance:p}:{field}"));
        unsafe { *frame = NDIlib_video_frame_v2_t {
            xres: 4, yres: 2,
            FourCC: NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA,
            frame_rate_N: 30, frame_rate_D: 1,
            picture_aspect_ratio: 1.25,
            frame_format_type: field,
            timecode: 1234,
            p_data: PIXELS.as_ptr().cast_mut(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 { line_stride_in_bytes: 16 },
            p_metadata: c"<clock/>".as_ptr(),
            timestamp: 5678,
        }; }
    }
    fn NDIlib_framesync_free_video(instance: NDIlib_framesync_instance_t, _frame: *mut NDIlib_video_frame_v2_t) {
        log(&format!("fs-video-free:{instance:p}"));
    }
    fn NDIlib_framesync_capture_audio_v2(instance: NDIlib_framesync_instance_t, frame: *mut NDIlib_audio_frame_v3_t, rate: i32, channels: i32, samples: i32) {
        let receiver = unsafe { state(instance).receiver };
        log(&format!("fs-audio:{receiver:p}:{instance:p}:{rate}:{channels}:{samples}"));
        let query = samples == 0;
        let data = if query { std::ptr::null_mut() } else {
            Box::into_raw(vec![0.25f32; (channels * samples) as usize].into_boxed_slice()).cast::<u8>()
        };
        unsafe { *frame = NDIlib_audio_frame_v3_t {
            sample_rate: if query { 48_000 } else { rate },
            no_channels: if query { 2 } else { channels },
            no_samples: samples,
            timecode: 4321,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 { channel_stride_in_bytes: samples * 4 },
            p_metadata: std::ptr::null(),
            timestamp: 8765,
        }; }
    }
    fn NDIlib_framesync_free_audio_v2(instance: NDIlib_framesync_instance_t, frame: *mut NDIlib_audio_frame_v3_t) {
        log(&format!("fs-audio-free:{instance:p}"));
        let frame = unsafe { &*frame };
        if !frame.p_data.is_null() {
            let samples = std::ptr::slice_from_raw_parts_mut(frame.p_data.cast::<f32>(), (frame.no_channels * frame.no_samples) as usize);
            drop(unsafe { Box::from_raw(samples) });
        }
    }
    fn NDIlib_framesync_audio_queue_depth(_instance: NDIlib_framesync_instance_t) -> i32 { 42 }
}

// These functions are declared only by installed Advanced headers. The crate's
// native tables check their exact signatures against those headers in builds
// that can exercise this fixture's success paths.
macro_rules! advanced {
    ($(fn $name:ident($($argument:ident: $type:ty),* $(,)?) -> $return:ty $body:block)+) => {$(
        #[no_mangle]
        pub unsafe extern "C" fn $name($($argument: $type),*) -> $return $body
    )+};
}

advanced! {
    fn NDIlib_recv_set_bandwidth(receiver: NDIlib_recv_instance_t, bandwidth: NDIlib_recv_bandwidth_e) -> bool {
        log(&format!("tier:{receiver:p}:{bandwidth}"));
        !refused()
    }
    fn NDIlib_recv_set_enabled_streams(receiver: NDIlib_recv_instance_t, video: bool, audio: bool, metadata: bool) -> bool {
        log(&format!("streams:{receiver:p}:{video}:{audio}:{metadata}"));
        !refused()
    }
    fn NDIlib_recv_kvm_send_keyboard_press(receiver: NDIlib_recv_instance_t, symbol: i32) -> bool {
        log(&format!("key-down:{receiver:p}:{symbol}")); !refused()
    }
    fn NDIlib_recv_kvm_send_keyboard_release(receiver: NDIlib_recv_instance_t, symbol: i32) -> bool {
        log(&format!("key-up:{receiver:p}:{symbol}")); !refused()
    }
    fn NDIlib_recv_kvm_send_mouse_position(receiver: NDIlib_recv_instance_t, position: *const f32) -> bool {
        let xy = unsafe { std::slice::from_raw_parts(position, 2) };
        log(&format!("position:{receiver:p}:{}:{}", xy[0], xy[1])); !refused()
    }
    fn NDIlib_recv_kvm_send_clipboard_contents(receiver: NDIlib_recv_instance_t, _text: *const c_char) -> bool {
        log(&format!("clipboard:{receiver:p}")); !refused()
    }
    fn NDIlib_recv_kvm_send_touch_positions(receiver: NDIlib_recv_instance_t, count: i32, _positions: *const f32) -> bool {
        log(&format!("touch:{receiver:p}:{count}")); !refused()
    }
    fn NDIlib_recv_kvm_send_vertical_mouse_wheel(_receiver: NDIlib_recv_instance_t, _units: f32) -> bool { !refused() }
    fn NDIlib_recv_kvm_send_horizontal_mouse_wheel(_receiver: NDIlib_recv_instance_t, _units: f32) -> bool { !refused() }
}

macro_rules! button {
    ($($name:ident => $label:literal;)+) => {$(
        advanced! {
            fn $name(receiver: NDIlib_recv_instance_t) -> bool {
                log(&format!(concat!($label, ":{:p}"), receiver)); !refused()
            }
        }
    )+};
}
button! {
    NDIlib_recv_kvm_is_supported => "kvm-supported";
    NDIlib_recv_kvm_send_left_mouse_click => "left-down";
    NDIlib_recv_kvm_send_middle_mouse_click => "middle-down";
    NDIlib_recv_kvm_send_right_mouse_click => "right-down";
    NDIlib_recv_kvm_send_left_mouse_release => "left-up";
    NDIlib_recv_kvm_send_middle_mouse_release => "middle-up";
    NDIlib_recv_kvm_send_right_mouse_release => "right-up";
    NDIlib_recv_kvm_send_touch_finish => "touch-up";
}
