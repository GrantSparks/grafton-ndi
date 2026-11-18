//! Observe descriptors after their runtime authorization, without encoding.
use super::*;

export! {
    fn NDIlib_send_send_video_v2(_sender: NDIlib_send_instance_t, frame: *const NDIlib_video_frame_v2_t) {
        let frame = unsafe { &*frame };
        log(&format!("video-send:{}", frame.FourCC));
    }
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_avsync_create(_receiver: NDIlib_recv_instance_t) -> *mut std::ffi::c_void {
    log("avsync-create");
    Box::into_raw(Box::new(0u8)).cast()
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_avsync_destroy(instance: *mut std::ffi::c_void) {
    log("avsync-destroy");
    drop(unsafe { Box::from_raw(instance.cast::<u8>()) });
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_avsync_synchronize(
    _instance: *mut std::ffi::c_void,
    video: *const NDIlib_video_frame_v2_t,
    _audio: *mut NDIlib_audio_frame_v3_t,
) -> i32 {
    let frame = unsafe { &*video };
    log(&format!("avsync-synchronize:{}:{}:{}", frame.FourCC, frame.timestamp, frame.p_metadata.is_null()));
    -1 // NDIlib_avsync_ret_no_audio_stream_received.
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_avsync_free_audio(
    _instance: *mut std::ffi::c_void,
    _audio: *mut NDIlib_audio_frame_v3_t,
) {
    log("avsync-free-audio");
}

#[cfg(fake_advanced)]
struct GenlockState {
    source: *const NDIlib_source_t,
    config: *const std::ffi::c_char,
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_genlock_create(
    source: *const NDIlib_source_t,
    config: *const std::ffi::c_char,
) -> *mut std::ffi::c_void {
    log("genlock-create");
    #[cfg(fake_config_lending)]
    log_configuration("genlock", config);
    Box::into_raw(Box::new(GenlockState { source, config })).cast()
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_genlock_destroy(instance: *mut std::ffi::c_void) {
    let state = unsafe { Box::from_raw(instance.cast::<GenlockState>()) };
    let source = unsafe { &*state.source };
    let name = unsafe { std::ffi::CStr::from_ptr(source.p_ndi_name) };
    let config = if state.config.is_null() {
        "<null>".into()
    } else {
        unsafe { std::ffi::CStr::from_ptr(state.config) }.to_string_lossy()
    };
    log(&format!("genlock-destroy:{}:{config}", name.to_string_lossy()));
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_genlock_connect(
    instance: *mut std::ffi::c_void,
    source: *const NDIlib_source_t,
) {
    unsafe { (*instance.cast::<GenlockState>()).source = source };
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_genlock_is_active(_instance: *mut std::ffi::c_void) -> bool { false }

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_genlock_wait_video(
    _instance: *mut std::ffi::c_void, _frame: *const NDIlib_video_frame_v2_t,
) -> bool { false }

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_genlock_wait_audio(
    _instance: *mut std::ffi::c_void, _frame: *const NDIlib_audio_frame_v3_t,
) -> bool { false }

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_recv_set_video_allocator(
    _receiver: NDIlib_recv_instance_t,
    _opaque: *mut std::ffi::c_void,
    allocate: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *mut NDIlib_video_frame_v2_t) -> bool>,
    free: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *const NDIlib_video_frame_v2_t) -> bool>,
) {
    assert_eq!(allocate.is_some(), free.is_some());
    log(if allocate.is_some() { "video-allocator-install" } else { "video-allocator-reset" });
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_recv_set_audio_allocator(
    _receiver: NDIlib_recv_instance_t,
    _opaque: *mut std::ffi::c_void,
    allocate: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *mut NDIlib_audio_frame_v3_t) -> bool>,
    free: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *const NDIlib_audio_frame_v3_t) -> bool>,
) {
    assert_eq!(allocate.is_some(), free.is_some());
    log(if allocate.is_some() { "audio-allocator-install" } else { "audio-allocator-reset" });
}
