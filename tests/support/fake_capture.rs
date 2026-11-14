//! Scripted all-slot capture boundary. Padding is deliberately uninitialized;
//! the wrapper may inspect/copy only the active regions initialized below.
use super::*;
use std::{mem::MaybeUninit, sync::Mutex};

fn next() -> String {
    static SCRIPT: Mutex<(String, usize)> = Mutex::new((String::new(), 0));
    let text = std::env::var("GRAFTON_FAKE_CAPTURE_SCRIPT").unwrap_or_default();
    let mut script = SCRIPT.lock().unwrap();
    if script.0 != text {
        *script = (text, 0);
    }
    // A distinct prefix lets the test restart an otherwise identical script.
    let item = script
        .0
        .split_once(':')
        .map_or("", |(_, items)| items)
        .split(',')
        .nth(script.1)
        .unwrap_or("none")
        .to_owned();
    script.1 += 1;
    item
}

export! {
    fn NDIlib_recv_capture_v3(_p_instance: NDIlib_recv_instance_t, p_video_data: *mut NDIlib_video_frame_v2_t, p_audio_data: *mut NDIlib_audio_frame_v3_t, p_metadata: *mut NDIlib_metadata_frame_t, timeout_in_ms: u32) -> NDIlib_frame_type_e {
        let kind = next();
        #[cfg(fake_receiver_controls)]
        log(&format!("capture-instance:{_p_instance:p}"));
        log(&format!("capture:{kind}:{}{}{}:{timeout_in_ms}",
            u8::from(!p_video_data.is_null()), u8::from(!p_audio_data.is_null()), u8::from(!p_metadata.is_null())));
        if let Ok(delay) = std::env::var("GRAFTON_FAKE_CAPTURE_DELAY_MS").unwrap_or_default().parse::<u64>() {
            std::thread::sleep(std::time::Duration::from_millis(delay.min(timeout_in_ms.into())));
        }
        match kind.as_str() {
            "video" | "planar" | "alpha" | "bad-video" if !p_video_data.is_null() => {
                let mut pixels = Box::new([MaybeUninit::<u8>::uninit(); 64]);
                let (format, height, stride, planes): (_, _, _, &[(usize, usize, usize, usize)]) = match kind.as_str() {
                    "planar" => (NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_I420, 4, 8, &[(0, 8, 4, 4), (32, 4, 2, 2), (40, 4, 2, 2)]),
                    "alpha" => (NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVA, 2, 16, &[(0, 16, 8, 2), (32, 8, 4, 2)]),
                    _ => (NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA, 2, 24, &[(0, 24, 16, 2)]),
                };
                for &(offset, row_stride, active, rows) in planes {
                    for row in 0..rows {
                        for pixel in &mut pixels[offset + row * row_stride..offset + row * row_stride + active] {
                            pixel.write(37);
                        }
                    }
                }
                let data = Box::into_raw(pixels).cast::<u8>();
                log(&format!("video-data:{}", data as usize));
                unsafe { *p_video_data = NDIlib_video_frame_v2_t {
                    xres: if kind == "bad-video" { -1 } else { 4 }, yres: height, FourCC: format,
                    frame_rate_N: 30_000, frame_rate_D: 1001,
                    picture_aspect_ratio: 1.25,
                    frame_format_type: NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_1,
                    timecode: 1234, p_data: data,
                    __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 { line_stride_in_bytes: stride },
                    p_metadata: c"<video/>".as_ptr(), timestamp: 5678,
                }};
                NDIlib_frame_type_e_NDIlib_frame_type_video
            }
            "audio" | "bad-audio" if !p_audio_data.is_null() => {
                let mut samples = Box::new([MaybeUninit::<f32>::uninit(); 16]);
                for (index, value) in [(0, 0.25), (1, 0.5), (4, -0.25), (5, -0.5)] { samples[index].write(value); }
                let data = Box::into_raw(samples).cast::<u8>();
                log(&format!("audio-data:{}", data as usize));
                unsafe { *p_audio_data = NDIlib_audio_frame_v3_t {
                    sample_rate: 48_000, no_channels: 2, no_samples: if kind == "bad-audio" { -1 } else { 2 },
                    timecode: 2345, FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
                    p_data: data,
                    __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 { channel_stride_in_bytes: 16 },
                    p_metadata: c"<audio/>".as_ptr(), timestamp: 6789,
                }};
                NDIlib_frame_type_e_NDIlib_frame_type_audio
            }
            "metadata" | "bad-metadata" if !p_metadata.is_null() => {
                let data = CString::new("<metadata/>").unwrap().into_raw();
                log(&format!("metadata-data:{}", data as usize));
                unsafe { *p_metadata = NDIlib_metadata_frame_t {
                    length: if kind == "bad-metadata" { -1 } else { 0 }, timecode: 3456,
                    p_data: data,
                }};
                NDIlib_frame_type_e_NDIlib_frame_type_metadata
            }
            "status" => NDIlib_frame_type_e_NDIlib_frame_type_status_change,
            "source" => NDIlib_frame_type_e_NDIlib_frame_type_source_change,
            "unknown" => 987,
            "error" => NDIlib_frame_type_e_NDIlib_frame_type_error,
            _ => NDIlib_frame_type_e_NDIlib_frame_type_none,
        }
    }

    fn NDIlib_recv_free_video_v2(_p_instance: NDIlib_recv_instance_t, p_video_data: *const NDIlib_video_frame_v2_t) {
        log("free-video");
        unsafe { drop(Box::from_raw((*p_video_data).p_data.cast::<[MaybeUninit<u8>; 64]>())); }
    }
    fn NDIlib_recv_free_audio_v3(_p_instance: NDIlib_recv_instance_t, p_audio_data: *const NDIlib_audio_frame_v3_t) {
        log("free-audio");
        unsafe { drop(Box::from_raw((*p_audio_data).p_data.cast::<[MaybeUninit<f32>; 16]>())); }
    }
    fn NDIlib_recv_free_metadata(_p_instance: NDIlib_recv_instance_t, p_metadata: *const NDIlib_metadata_frame_t) {
        log("free-metadata");
        unsafe { drop(CString::from_raw((*p_metadata).p_data)); }
    }
}
