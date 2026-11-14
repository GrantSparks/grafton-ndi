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


fn media_metadata(kind: &str, valid: &'static std::ffi::CStr) -> *const std::ffi::c_char {
    static UNTERMINATED: [u8; 4 * 1024 * 1024] = [b'x'; 4 * 1024 * 1024];
    if kind.ends_with("invalid-metadata") {
        c"\xff".as_ptr()
    } else if kind.ends_with("unterminated-metadata") {
        UNTERMINATED.as_ptr().cast()
    } else {
        valid.as_ptr()
    }
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
            gate if gate.starts_with("gate-") && !p_video_data.is_null() => {
                // One full-resolution row per plane fits the same allocation
                // the ordinary video fixtures free. Initialize its entire
                // storage so these capability fixtures add no padding concern.
                let pixels = Box::new([MaybeUninit::<u8>::new(0); 64]);
                let (format, stride) = if gate.starts_with("gate-nb4g") {
                    (NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NB4G, 16)
                } else if gate.starts_with("gate-na4g") {
                    (NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NA4G, 16)
                } else if gate.starts_with("gate-nb48") {
                    (NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NB48, 8)
                } else if gate.starts_with("gate-na48") {
                    (NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NA48, 8)
                } else {
                    panic!("unknown gated fixture {gate}");
                };
                unsafe { *p_video_data = NDIlib_video_frame_v2_t {
                    xres: 4, yres: 1, FourCC: format,
                    frame_rate_N: 30, frame_rate_D: 1, picture_aspect_ratio: 4.0,
                    frame_format_type: NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive,
                    timecode: 1234, timestamp: 5678,
                    p_data: Box::into_raw(pixels).cast(),
                    __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 { line_stride_in_bytes: stride },
                    p_metadata: media_metadata(&kind, c"<video/>"),
                }; }
                NDIlib_frame_type_e_NDIlib_frame_type_video
            }
            "packet-video" | "bad-packet-video" if !p_video_data.is_null() => {
                let mut bytes = Box::new([MaybeUninit::<u8>::uninit(); 64]);
                let mut packet = [0u8; 48];
                packet[0..4].copy_from_slice(&44i32.to_ne_bytes());
                packet[4..8].copy_from_slice(b"H264");
                packet[32..36].copy_from_slice(&1u32.to_ne_bytes());
                packet[36..40].copy_from_slice(&3u32.to_ne_bytes());
                packet[40..44].copy_from_slice(&1u32.to_ne_bytes());
                packet[44..48].copy_from_slice(&[1,2,3,4]);
                for (byte,value) in bytes.iter_mut().zip(packet) { byte.write(value); }
                let data = Box::into_raw(bytes).cast::<u8>();
                log(&format!("video-data:{}", data as usize));
                unsafe { *p_video_data = NDIlib_video_frame_v2_t {
                    xres: 16, yres: 16, FourCC: u32::from_le_bytes(*b"H264"),
                    frame_rate_N: 30, frame_rate_D: 1, p_data: data,
                    timecode: 1234, timestamp: 5678, p_metadata: c"<packet/>".as_ptr(),
                    __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                        data_size_in_bytes: if kind == "bad-packet-video" { 1 } else { 48 },
                    }, ..Default::default()
                }};
                NDIlib_frame_type_e_NDIlib_frame_type_video
            }
            "packet-audio" if !p_audio_data.is_null() => {
                let mut bytes = Box::new([MaybeUninit::<f32>::uninit(); 16]);
                bytes[0].write(f32::from_ne_bytes([1,2,3,4]));
                let data = Box::into_raw(bytes).cast::<u8>();
                log(&format!("audio-data:{}", data as usize));
                unsafe { *p_audio_data = NDIlib_audio_frame_v3_t {
                    sample_rate: 48_000, no_channels: 2, no_samples: 960,
                    FourCC: u32::from_le_bytes(*b"Opus"), p_data: data,
                    timecode: 2345, timestamp: 6789, p_metadata: c"<packet/>".as_ptr(),
                    __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 { data_size_in_bytes: 4 },
                }};
                NDIlib_frame_type_e_NDIlib_frame_type_audio
            }
            "video" | "planar" | "alpha" | "bad-video" | "video-invalid-metadata" | "video-unterminated-metadata" if !p_video_data.is_null() => {
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
                    p_metadata: media_metadata(&kind, c"<video/>"), timestamp: 5678,
                }};
                NDIlib_frame_type_e_NDIlib_frame_type_video
            }
            "audio" | "bad-audio" | "audio-invalid-metadata" | "audio-unterminated-metadata" if !p_audio_data.is_null() => {
                let mut samples = Box::new([MaybeUninit::<f32>::uninit(); 16]);
                for (index, value) in [(0, 0.25), (1, 0.5), (4, -0.25), (5, -0.5)] { samples[index].write(value); }
                let data = Box::into_raw(samples).cast::<u8>();
                log(&format!("audio-data:{}", data as usize));
                unsafe { *p_audio_data = NDIlib_audio_frame_v3_t {
                    sample_rate: 48_000, no_channels: 2, no_samples: if kind == "bad-audio" { -1 } else { 2 },
                    timecode: 2345, FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
                    p_data: data,
                    __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 { channel_stride_in_bytes: 16 },
                    p_metadata: media_metadata(&kind, c"<audio/>"), timestamp: 6789,
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
