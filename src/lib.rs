#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

use std::{
    ffi::{CStr, CString},
    os::raw::c_char,
    ptr,
};

pub struct NDIlib {
    _private: (),
}

impl NDIlib {
    pub fn initialize() -> bool {
        unsafe { NDIlib_initialize() }
    }

    pub fn destroy() {
        unsafe { NDIlib_destroy() }
    }

    pub fn version() -> &'static str {
        unsafe {
            CStr::from_ptr(NDIlib_version())
                .to_str()
                .expect("Invalid UTF-8 string")
        }
    }

    pub fn is_supported_cpu() -> bool {
        unsafe { NDIlib_is_supported_CPU() }
    }
}

pub struct NDIlibFindInstance {
    ptr: NDIlib_find_instance_t,
}

impl NDIlibFindInstance {
    pub fn new(show_local_sources: bool, groups: Option<&str>, extra_ips: Option<&str>) -> Self {
        let groups = groups.map_or(ptr::null(), |s| s.as_ptr() as *const c_char);
        let extra_ips = extra_ips.map_or(ptr::null(), |s| s.as_ptr() as *const c_char);

        let settings = NDIlib_find_create_t {
            show_local_sources,
            p_groups: groups,
            p_extra_ips: extra_ips,
        };

        let ptr = unsafe { NDIlib_find_create_v2(&settings) };
        Self { ptr }
    }

    pub fn wait_for_sources(&self, timeout_ms: u32) -> bool {
        unsafe { NDIlib_find_wait_for_sources(self.ptr, timeout_ms) }
    }

    pub fn get_sources(&self, timeout_ms: u32) -> Vec<NDIlibSource> {
        let mut no_sources: u32 = 0;
        let sources_ptr = unsafe { NDIlib_find_get_sources(self.ptr, &mut no_sources, timeout_ms) };

        if sources_ptr.is_null() || no_sources == 0 {
            return Vec::new();
        }

        let sources = unsafe { std::slice::from_raw_parts(sources_ptr, no_sources as usize) };
        sources.iter().map(NDIlibSource::from_raw).collect()
    }

    pub fn is_initialized(&self) -> bool {
        !self.ptr.is_null()
    }
}

impl Drop for NDIlibFindInstance {
    fn drop(&mut self) {
        unsafe { NDIlib_find_destroy(self.ptr) }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct NDIlibSource {
    pub name: String,
    pub url_address: String,
    pub ip_address: String,
}

impl NDIlibSource {
    pub fn from_raw(raw: &NDIlib_source_t) -> Self {
        let name = unsafe {
            CStr::from_ptr(raw.p_ndi_name)
                .to_string_lossy()
                .into_owned()
        };
        let url_address = unsafe {
            CStr::from_ptr(raw.__bindgen_anon_1.p_url_address)
                .to_string_lossy()
                .into_owned()
        };
        let ip_address = unsafe {
            CStr::from_ptr(raw.__bindgen_anon_1.p_ip_address)
                .to_string_lossy()
                .into_owned()
        };

        NDIlibSource {
            name,
            url_address,
            ip_address,
        }
    }
}

pub struct NDIlibRecvInstance {
    ptr: NDIlib_recv_instance_t,
}

impl NDIlibRecvInstance {
    pub fn new(
        source: &NDIlibSource,
        color_format: NDIlib_recv_color_format_e,
        bandwidth: NDIlib_recv_bandwidth_e,
        allow_video_fields: bool,
    ) -> Self {
        let source_to_connect_to = NDIlib_source_t {
            p_ndi_name: source.name.as_ptr() as *const c_char,
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_url_address: source.url_address.as_ptr() as *const c_char,
            },
        };

        let settings = NDIlib_recv_create_v3_t {
            source_to_connect_to,
            color_format,
            bandwidth,
            allow_video_fields,
            p_ndi_recv_name: ptr::null(),
        };

        let ptr = unsafe { NDIlib_recv_create_v3(&settings) };
        Self { ptr }
    }

    pub fn capture(&self, timeout_ms: u32) -> Option<NDIlibFrame> {
        let mut video_frame = NDIlib_video_frame_v2_t {
            xres: 0,
            yres: 0,
            FourCC: 0,
            frame_rate_N: 0,
            frame_rate_D: 0,
            picture_aspect_ratio: 0.0,
            frame_format_type: 0,
            timecode: 0,
            p_data: ptr::null_mut(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 0,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };
        let mut audio_frame = NDIlib_audio_frame_v2_t {
            sample_rate: 0,
            no_channels: 0,
            no_samples: 0,
            timecode: 0,
            p_data: ptr::null_mut(),
            channel_stride_in_bytes: 0,
            p_metadata: ptr::null(),
            timestamp: 0,
        };
        let mut metadata_frame = NDIlib_metadata_frame_t {
            length: 0,
            timecode: 0,
            p_data: ptr::null_mut(),
        };

        let frame_type = unsafe {
            NDIlib_recv_capture_v2(
                self.ptr,
                &mut video_frame,
                &mut audio_frame,
                &mut metadata_frame,
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_video => Some(NDIlibFrame::Video(video_frame)),
            NDIlib_frame_type_e_NDIlib_frame_type_audio => Some(NDIlibFrame::Audio(audio_frame)),
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                Some(NDIlibFrame::Metadata(metadata_frame))
            }
            _ => None,
        }
    }

    pub fn free_video(&self, video_frame: &NDIlib_video_frame_v2_t) {
        unsafe { NDIlib_recv_free_video_v2(self.ptr, video_frame) }
    }

    pub fn free_audio(&self, audio_frame: &NDIlib_audio_frame_v2_t) {
        unsafe { NDIlib_recv_free_audio_v2(self.ptr, audio_frame) }
    }

    pub fn free_metadata(&self, metadata_frame: &NDIlib_metadata_frame_t) {
        unsafe { NDIlib_recv_free_metadata(self.ptr, metadata_frame) }
    }

    pub fn is_initialized(&self) -> bool {
        !self.ptr.is_null()
    }
}

impl Drop for NDIlibRecvInstance {
    fn drop(&mut self) {
        unsafe { NDIlib_recv_destroy(self.ptr) }
    }
}

pub enum NDIlibFrame {
    Video(NDIlib_video_frame_v2_t),
    Audio(NDIlib_audio_frame_v2_t),
    Metadata(NDIlib_metadata_frame_t),
}

pub struct NDIlibSendInstance {
    ptr: NDIlib_send_instance_t,
}

impl NDIlibSendInstance {
    pub fn new(name: &str, groups: Option<&str>, clock_video: bool, clock_audio: bool) -> Self {
        let name_cstr = CString::new(name).unwrap();
        let groups_cstr = groups.map(|s| CString::new(s).unwrap());

        let settings = NDIlib_send_create_t {
            p_ndi_name: name_cstr.as_ptr(),
            p_groups: groups_cstr.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            clock_video,
            clock_audio,
        };

        let ptr = unsafe { NDIlib_send_create(&settings) };
        Self { ptr }
    }

    pub fn send_video(&self, video_frame: &NDIlib_video_frame_v2_t) {
        unsafe { NDIlib_send_send_video_v2(self.ptr, video_frame) }
    }

    pub fn send_video_async(&self, video_frame: &NDIlib_video_frame_v2_t) {
        unsafe { NDIlib_send_send_video_async_v2(self.ptr, video_frame) }
    }

    pub fn send_audio(&self, audio_frame: &NDIlib_audio_frame_v2_t) {
        unsafe { NDIlib_send_send_audio_v2(self.ptr, audio_frame) }
    }

    pub fn send_metadata(&self, metadata_frame: &NDIlib_metadata_frame_t) {
        unsafe { NDIlib_send_send_metadata(self.ptr, metadata_frame) }
    }

    pub fn capture_metadata(&self, timeout_ms: u32) -> Option<NDIlib_metadata_frame_t> {
        let mut metadata_frame = NDIlib_metadata_frame_t {
            length: 0,
            timecode: 0,
            p_data: ptr::null_mut(),
        };

        let frame_type = unsafe { NDIlib_send_capture(self.ptr, &mut metadata_frame, timeout_ms) };

        if frame_type == NDIlib_frame_type_e_NDIlib_frame_type_metadata {
            Some(metadata_frame)
        } else {
            None
        }
    }

    pub fn free_metadata(&self, metadata_frame: &NDIlib_metadata_frame_t) {
        unsafe { NDIlib_send_free_metadata(self.ptr, metadata_frame) }
    }
}

impl Drop for NDIlibSendInstance {
    fn drop(&mut self) {
        unsafe { NDIlib_send_destroy(self.ptr) }
    }
}
