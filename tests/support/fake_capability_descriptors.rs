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
