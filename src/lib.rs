#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]

mod ndi_lib;

use std::{
    ffi::{CStr, CString},
    os::raw::c_char,
    ptr,
};

use ndi_lib::{
    true_, NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
    NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_max, NDIlib_FourCC_video_type_e,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRX,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_I420,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NV12,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_P216,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_PA16,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBA,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBX,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVA,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVY,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_YV12,
    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_max, NDIlib_audio_frame_v3_t,
    NDIlib_audio_frame_v3_t__bindgen_ty_1, NDIlib_destroy, NDIlib_find_create_t,
    NDIlib_find_create_v2, NDIlib_find_destroy, NDIlib_find_get_sources, NDIlib_find_instance_t,
    NDIlib_find_wait_for_sources, NDIlib_frame_format_type_e,
    NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_0,
    NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_1,
    NDIlib_frame_format_type_e_NDIlib_frame_format_type_interleaved,
    NDIlib_frame_format_type_e_NDIlib_frame_format_type_max,
    NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive,
    NDIlib_frame_type_e_NDIlib_frame_type_audio, NDIlib_frame_type_e_NDIlib_frame_type_metadata,
    NDIlib_frame_type_e_NDIlib_frame_type_none, NDIlib_frame_type_e_NDIlib_frame_type_video,
    NDIlib_initialize, NDIlib_is_supported_CPU, NDIlib_metadata_frame_t, NDIlib_recv_bandwidth_e,
    NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_audio_only,
    NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
    NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
    NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_max,
    NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_metadata_only, NDIlib_recv_capture_v3,
    NDIlib_recv_color_format_e, NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA,
    NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA_flipped,
    NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA,
    NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_BGRA,
    NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_RGBA,
    NDIlib_recv_color_format_e_NDIlib_recv_color_format_best,
    NDIlib_recv_color_format_e_NDIlib_recv_color_format_fastest,
    NDIlib_recv_color_format_e_NDIlib_recv_color_format_max, NDIlib_recv_connect,
    NDIlib_recv_create_v3, NDIlib_recv_create_v3_t, NDIlib_recv_destroy, NDIlib_recv_free_audio_v3,
    NDIlib_recv_free_metadata, NDIlib_recv_free_video_v2, NDIlib_recv_instance_t,
    NDIlib_recv_ptz_auto_focus, NDIlib_recv_ptz_exposure_auto, NDIlib_recv_ptz_exposure_manual,
    NDIlib_recv_ptz_exposure_manual_v2, NDIlib_recv_ptz_focus, NDIlib_recv_ptz_focus_speed,
    NDIlib_recv_ptz_is_supported, NDIlib_recv_ptz_pan_tilt, NDIlib_recv_ptz_pan_tilt_speed,
    NDIlib_recv_ptz_recall_preset, NDIlib_recv_ptz_store_preset,
    NDIlib_recv_ptz_white_balance_auto, NDIlib_recv_ptz_white_balance_indoor,
    NDIlib_recv_ptz_white_balance_manual, NDIlib_recv_ptz_white_balance_oneshot,
    NDIlib_recv_ptz_white_balance_outdoor, NDIlib_recv_ptz_zoom, NDIlib_recv_ptz_zoom_speed,
    NDIlib_send_add_connection_metadata, NDIlib_send_capture,
    NDIlib_send_clear_connection_metadata, NDIlib_send_create, NDIlib_send_create_t,
    NDIlib_send_destroy, NDIlib_send_free_metadata, NDIlib_send_get_no_connections,
    NDIlib_send_get_source_name, NDIlib_send_get_tally, NDIlib_send_instance_t,
    NDIlib_send_send_audio_v3, NDIlib_send_send_metadata, NDIlib_send_send_video_async_v2,
    NDIlib_send_send_video_v2, NDIlib_send_set_failover, NDIlib_source_t,
    NDIlib_source_t__bindgen_ty_1, NDIlib_tally_t, NDIlib_version, NDIlib_video_frame_v2_t,
    NDIlib_video_frame_v2_t__bindgen_ty_1,
};

pub struct NDI {
    initialized: bool,
}

impl NDI {
    pub fn new() -> Result<Self, &'static str> {
        if true_ == 1 && Self::initialize() {
            Ok(NDI { initialized: true })
        } else {
            Err("Failed to initialize NDI library")
        }
    }

    pub fn version() -> &'static str {
        unsafe {
            CStr::from_ptr(Self::get_version())
                .to_str()
                .expect("Invalid UTF-8 string")
        }
    }

    pub fn is_supported_cpu() -> bool {
        unsafe { NDIlib_is_supported_CPU() }
    }

    fn initialize() -> bool {
        unsafe { NDIlib_initialize() }
    }

    fn get_version() -> *const c_char {
        unsafe { NDIlib_version() }
    }
}

impl Drop for NDI {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                NDIlib_destroy();
            }
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Source {
    pub name: String,
    pub url_address: Option<String>,
    pub ip_address: Option<String>,
}

impl Source {
    fn from_raw(raw: &NDIlib_source_t) -> Self {
        let name = unsafe {
            CStr::from_ptr(raw.p_ndi_name)
                .to_string_lossy()
                .into_owned()
        };
        let url_address = unsafe {
            if !raw.__bindgen_anon_1.p_url_address.is_null() {
                Some(
                    CStr::from_ptr(raw.__bindgen_anon_1.p_url_address)
                        .to_string_lossy()
                        .into_owned(),
                )
            } else {
                None
            }
        };
        let ip_address = unsafe {
            if !raw.__bindgen_anon_1.p_ip_address.is_null() {
                Some(
                    CStr::from_ptr(raw.__bindgen_anon_1.p_ip_address)
                        .to_string_lossy()
                        .into_owned(),
                )
            } else {
                None
            }
        };

        Source {
            name,
            url_address,
            ip_address,
        }
    }

    fn to_raw(&self) -> NDIlib_source_t {
        NDIlib_source_t {
            p_ndi_name: CString::new(self.name.clone()).unwrap().into_raw(),
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_url_address: match &self.url_address {
                    Some(url) => CString::new(url.clone()).unwrap().into_raw(),
                    None => ptr::null(),
                },
            },
        }
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct Finder {
    show_local_sources: bool,
    p_groups: *const std::os::raw::c_char,
    p_extra_ips: *const std::os::raw::c_char,
}

impl Default for Finder {
    fn default() -> Self {
        Finder {
            show_local_sources: false,
            p_groups: std::ptr::null(),
            p_extra_ips: std::ptr::null(),
        }
    }
}

impl Finder {
    pub fn new(show_local_sources: bool, groups: Option<&str>, extra_ips: Option<&str>) -> Self {
        Finder {
            show_local_sources,
            p_groups: groups.map_or(std::ptr::null(), |g| {
                CString::new(g).unwrap().into_raw() as *const std::os::raw::c_char
            }),
            p_extra_ips: extra_ips.map_or(std::ptr::null(), |ip| {
                CString::new(ip).unwrap().into_raw() as *const std::os::raw::c_char
            }),
        }
    }
}

pub struct Find {
    instance: NDIlib_find_instance_t,
}

impl Find {
    pub fn new(create: Finder) -> Result<Self, &'static str> {
        let create_t = NDIlib_find_create_t {
            show_local_sources: create.show_local_sources,
            p_groups: create.p_groups,
            p_extra_ips: create.p_extra_ips,
        };
        let instance = unsafe { NDIlib_find_create_v2(&create_t) };
        if instance.is_null() {
            Err("Failed to create NDI find instance")
        } else {
            Ok(Find { instance })
        }
    }

    pub fn wait_for_sources(&self, timeout_ms: u32) -> bool {
        unsafe { NDIlib_find_wait_for_sources(self.instance, timeout_ms) }
    }

    pub fn get_sources(&self, timeout_ms: u32) -> Vec<Source> {
        let mut source_count = 0;
        let sources =
            unsafe { NDIlib_find_get_sources(self.instance, &mut source_count, timeout_ms) };
        if sources.is_null() {
            vec![]
        } else {
            let raw_sources = unsafe { std::slice::from_raw_parts(sources, source_count as usize) };
            raw_sources.iter().map(Source::from_raw).collect()
        }
    }
}

impl Drop for Find {
    fn drop(&mut self) {
        unsafe {
            NDIlib_find_destroy(self.instance);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum FourCCVideoType {
    UYVY,
    UYVA,
    P216,
    PA16,
    YV12,
    I420,
    NV12,
    BGRA,
    BGRX,
    RGBA,
    RGBX,
    Max,
}

impl From<FourCCVideoType> for NDIlib_FourCC_video_type_e {
    fn from(fourcc: FourCCVideoType) -> Self {
        match fourcc {
            FourCCVideoType::UYVY => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVY,
            FourCCVideoType::UYVA => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVA,
            FourCCVideoType::P216 => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_P216,
            FourCCVideoType::PA16 => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_PA16,
            FourCCVideoType::YV12 => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_YV12,
            FourCCVideoType::I420 => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_I420,
            FourCCVideoType::NV12 => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NV12,
            FourCCVideoType::BGRA => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA,
            FourCCVideoType::BGRX => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRX,
            FourCCVideoType::RGBA => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBA,
            FourCCVideoType::RGBX => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBX,
            FourCCVideoType::Max => NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_max,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum FrameFormatType {
    Progressive,
    Interlaced,
    Field0,
    Field1,
    Max,
}

impl From<FrameFormatType> for NDIlib_frame_format_type_e {
    fn from(format: FrameFormatType) -> Self {
        match format {
            FrameFormatType::Progressive => {
                NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive
            }
            FrameFormatType::Interlaced => {
                NDIlib_frame_format_type_e_NDIlib_frame_format_type_interleaved
            }
            FrameFormatType::Field0 => NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_0,
            FrameFormatType::Field1 => NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_1,
            FrameFormatType::Max => NDIlib_frame_format_type_e_NDIlib_frame_format_type_max,
        }
    }
}

#[repr(C)]
pub union LineStrideOrSize {
    pub line_stride_in_bytes: i32,
    pub data_size_in_bytes: i32,
}

#[repr(C)]
pub struct VideoFrame {
    pub xres: i32,
    pub yres: i32,
    pub fourcc: FourCCVideoType,
    pub frame_rate_n: i32,
    pub frame_rate_d: i32,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: FrameFormatType,
    pub timecode: i64,
    pub p_data: *mut u8,
    pub line_stride_or_size: LineStrideOrSize, // Union
    pub p_metadata: *const ::std::os::raw::c_char,
    pub timestamp: i64,
}

impl VideoFrame {
    pub fn new() -> Self {
        VideoFrame {
            xres: 0,
            yres: 0,
            fourcc: FourCCVideoType::RGBA,
            frame_rate_n: 0,
            frame_rate_d: 0,
            picture_aspect_ratio: 0.0,
            frame_format_type: FrameFormatType::Interlaced,
            timecode: 0,
            p_data: ptr::null_mut(),
            line_stride_or_size: LineStrideOrSize {
                line_stride_in_bytes: 0,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        }
    }

    pub(crate) fn to_raw(&self) -> NDIlib_video_frame_v2_t {
        NDIlib_video_frame_v2_t {
            xres: self.xres,
            yres: self.yres,
            FourCC: self.fourcc.into(),
            frame_rate_N: self.frame_rate_n,
            frame_rate_D: self.frame_rate_d,
            picture_aspect_ratio: self.picture_aspect_ratio,
            frame_format_type: self.frame_format_type.into(),
            timecode: self.timecode,
            p_data: self.p_data,
            __bindgen_anon_1: unsafe {
                NDIlib_video_frame_v2_t__bindgen_ty_1 {
                    line_stride_in_bytes: self.line_stride_or_size.line_stride_in_bytes,
                }
            },
            p_metadata: self.p_metadata,
            timestamp: self.timestamp,
        }
    }

    pub(crate) fn from_raw(raw: NDIlib_video_frame_v2_t) -> Self {
        VideoFrame {
            xres: raw.xres,
            yres: raw.yres,
            fourcc: match raw.FourCC {
                NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVY => FourCCVideoType::UYVY,
                NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVA => FourCCVideoType::UYVA,
                NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_P216 => FourCCVideoType::P216,
                NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_PA16 => FourCCVideoType::PA16,
                NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_YV12 => FourCCVideoType::YV12,
                NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_I420 => FourCCVideoType::I420,
                NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NV12 => FourCCVideoType::NV12,
                NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA => FourCCVideoType::BGRA,
                NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRX => FourCCVideoType::BGRX,
                NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBA => FourCCVideoType::RGBA,
                NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBX => FourCCVideoType::RGBX,
                _ => FourCCVideoType::Max,
            },
            frame_rate_n: raw.frame_rate_N,
            frame_rate_d: raw.frame_rate_D,
            picture_aspect_ratio: raw.picture_aspect_ratio,
            frame_format_type: match raw.frame_format_type {
                NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive => {
                    FrameFormatType::Progressive
                }
                NDIlib_frame_format_type_e_NDIlib_frame_format_type_interleaved => {
                    FrameFormatType::Interlaced
                }
                NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_0 => {
                    FrameFormatType::Field0
                }
                NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_1 => {
                    FrameFormatType::Field1
                }
                _ => FrameFormatType::Max,
            },
            timecode: raw.timecode,
            p_data: raw.p_data,
            line_stride_or_size: unsafe {
                LineStrideOrSize {
                    line_stride_in_bytes: raw.__bindgen_anon_1.line_stride_in_bytes,
                }
            },
            p_metadata: raw.p_metadata,
            timestamp: raw.timestamp,
        }
    }
}

impl Default for VideoFrame {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AudioFrame {
    pub sample_rate: i32,
    pub no_channels: i32,
    pub no_samples: i32,
    pub timecode: i64,
    pub fourcc: AudioType,
    pub data: Vec<u8>,
    pub channel_stride_in_bytes: i32,
    pub metadata: Option<String>,
    pub timestamp: i64,
}

impl AudioFrame {
    pub fn new() -> Self {
        AudioFrame {
            sample_rate: 0,
            no_channels: 0,
            no_samples: 0,
            timecode: 0,
            fourcc: AudioType::Max,
            data: Vec::new(),
            channel_stride_in_bytes: 0,
            metadata: None,
            timestamp: 0,
        }
    }

    pub(crate) fn to_raw(&self) -> NDIlib_audio_frame_v3_t {
        NDIlib_audio_frame_v3_t {
            sample_rate: self.sample_rate,
            no_channels: self.no_channels,
            no_samples: self.no_samples,
            timecode: self.timecode,
            FourCC: self.fourcc.into(),
            p_data: self.data.as_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: self.channel_stride_in_bytes,
            },
            p_metadata: match &self.metadata {
                Some(metadata) => CString::new(metadata.clone()).unwrap().into_raw(),
                None => ptr::null(),
            },
            timestamp: self.timestamp,
        }
    }

    pub(crate) fn from_raw(raw: NDIlib_audio_frame_v3_t) -> Self {
        let metadata = if raw.p_metadata.is_null() {
            None
        } else {
            unsafe {
                Some(
                    CString::from_raw(raw.p_metadata as *mut c_char)
                        .into_string()
                        .unwrap(),
                )
            }
        };

        AudioFrame {
            sample_rate: raw.sample_rate,
            no_channels: raw.no_channels,
            no_samples: raw.no_samples,
            timecode: raw.timecode,
            fourcc: raw.FourCC.into(),
            data: unsafe {
                Vec::from_raw_parts(
                    raw.p_data,
                    raw.__bindgen_anon_1.data_size_in_bytes as usize,
                    raw.__bindgen_anon_1.data_size_in_bytes as usize,
                )
            },
            channel_stride_in_bytes: unsafe { raw.__bindgen_anon_1.channel_stride_in_bytes },
            metadata,
            timestamp: raw.timestamp,
        }
    }
}

impl Default for AudioFrame {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioType {
    FLTP,
    Max,
}

impl From<i32> for AudioType {
    fn from(value: i32) -> Self {
        if let NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP = value {
            AudioType::FLTP
        } else {
            AudioType::Max
        }
    }
}

impl From<AudioType> for i32 {
    fn from(audio_type: AudioType) -> Self {
        match audio_type {
            AudioType::FLTP => NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            AudioType::Max => NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_max,
        }
    }
}

pub struct MetadataFrame {
    length: i32,
    timecode: i64,
    p_data: *mut c_char,
}

impl MetadataFrame {
    pub fn new() -> Self {
        MetadataFrame {
            length: 0,
            timecode: 0,
            p_data: ptr::null_mut(),
        }
    }

    pub(crate) fn to_raw(&self) -> NDIlib_metadata_frame_t {
        NDIlib_metadata_frame_t {
            length: self.length,
            timecode: self.timecode,
            p_data: self.p_data,
        }
    }

    pub(crate) fn from_raw(raw: NDIlib_metadata_frame_t) -> Self {
        MetadataFrame {
            length: raw.length,
            timecode: raw.timecode,
            p_data: raw.p_data,
        }
    }
}

impl Default for MetadataFrame {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RecvColorFormat {
    BGRX_BGRA,
    UYVY_BGRA,
    RGBX_RGBA,
    UYVY_RGBA,
    Fastest,
    Best,
    BGRX_BGRA_Flipped,
    Max,
}

impl From<RecvColorFormat> for NDIlib_recv_color_format_e {
    fn from(format: RecvColorFormat) -> Self {
        match format {
            RecvColorFormat::BGRX_BGRA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA
            }
            RecvColorFormat::UYVY_BGRA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_BGRA
            }
            RecvColorFormat::RGBX_RGBA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA
            }
            RecvColorFormat::UYVY_RGBA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_RGBA
            }
            RecvColorFormat::Fastest => NDIlib_recv_color_format_e_NDIlib_recv_color_format_fastest,
            RecvColorFormat::Best => NDIlib_recv_color_format_e_NDIlib_recv_color_format_best,
            RecvColorFormat::BGRX_BGRA_Flipped => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA_flipped
            }
            RecvColorFormat::Max => NDIlib_recv_color_format_e_NDIlib_recv_color_format_max,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RecvBandwidth {
    MetadataOnly,
    AudioOnly,
    Lowest,
    Highest,
    Max,
}

impl From<RecvBandwidth> for NDIlib_recv_bandwidth_e {
    fn from(bandwidth: RecvBandwidth) -> Self {
        match bandwidth {
            RecvBandwidth::MetadataOnly => {
                NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_metadata_only
            }
            RecvBandwidth::AudioOnly => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_audio_only,
            RecvBandwidth::Lowest => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
            RecvBandwidth::Highest => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
            RecvBandwidth::Max => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_max,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Receiver {
    pub source_to_connect_to: Source,
    pub color_format: RecvColorFormat,
    pub bandwidth: RecvBandwidth,
    pub allow_video_fields: bool,
    pub ndi_recv_name: Option<String>,
}

impl Default for Receiver {
    fn default() -> Self {
        Receiver {
            source_to_connect_to: Source {
                name: String::new(),
                url_address: None,
                ip_address: None,
            },
            color_format: RecvColorFormat::BGRX_BGRA,
            bandwidth: RecvBandwidth::Highest,
            allow_video_fields: true,
            ndi_recv_name: None,
        }
    }
}

impl Receiver {
    pub fn new(
        source_to_connect_to: Source,
        color_format: RecvColorFormat,
        bandwidth: RecvBandwidth,
        allow_video_fields: bool,
        ndi_recv_name: Option<String>,
    ) -> Self {
        Receiver {
            source_to_connect_to,
            color_format,
            bandwidth,
            allow_video_fields,
            ndi_recv_name,
        }
    }

    pub(crate) fn to_raw(&self) -> NDIlib_recv_create_v3_t {
        NDIlib_recv_create_v3_t {
            source_to_connect_to: self.source_to_connect_to.to_raw(),
            color_format: self.color_format.into(),
            bandwidth: self.bandwidth.into(),
            allow_video_fields: self.allow_video_fields,
            p_ndi_recv_name: match &self.ndi_recv_name {
                Some(name) => CString::new(name.clone()).unwrap().into_raw(),
                None => ptr::null(),
            },
        }
    }
}

pub struct Recv {
    instance: NDIlib_recv_instance_t,
}

impl Recv {
    pub fn new(create: Receiver) -> Result<Self, &'static str> {
        let create_t = create.to_raw();
        let instance = unsafe { NDIlib_recv_create_v3(&create_t) };
        if instance.is_null() {
            Err("Failed to create NDI recv instance")
        } else {
            unsafe { NDIlib_recv_connect(instance, &create_t.source_to_connect_to) };

            Ok(Recv { instance })
        }
    }

    pub fn capture(&self, timeout_ms: u32) -> Result<FrameType, &'static str> {
        let video_frame = VideoFrame::new();
        let audio_frame = AudioFrame::new();
        let metadata_frame = MetadataFrame::new();

        // Create raw structs
        let mut raw_video_frame = video_frame.to_raw();
        let mut raw_audio_frame = audio_frame.to_raw();
        let mut raw_metadata_frame = metadata_frame.to_raw();

        // Call the function with pointers to the raw structs
        let frame_type = unsafe {
            NDIlib_recv_capture_v3(
                self.instance,
                &mut raw_video_frame as *mut _,
                &mut raw_audio_frame as *mut _,
                &mut raw_metadata_frame as *mut _,
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_video => {
                Ok(FrameType::Video(VideoFrame::from_raw(raw_video_frame)))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_audio => {
                Ok(FrameType::Audio(AudioFrame::from_raw(raw_audio_frame)))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => Ok(FrameType::Metadata(
                MetadataFrame::from_raw(raw_metadata_frame),
            )),
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(FrameType::None),
            _ => Err("Failed to capture frame"),
        }
    }

    pub fn free_video(&self, video_frame: &VideoFrame) {
        unsafe {
            NDIlib_recv_free_video_v2(self.instance, &video_frame.to_raw());
        }
    }

    pub fn free_audio(&self, audio_frame: &AudioFrame) {
        unsafe {
            NDIlib_recv_free_audio_v3(self.instance, &audio_frame.to_raw());
        }
    }

    pub fn free_metadata(&self, metadata_frame: &MetadataFrame) {
        unsafe {
            NDIlib_recv_free_metadata(self.instance, &metadata_frame.to_raw());
        }
    }

    pub fn ptz_is_supported(&self) -> bool {
        unsafe { NDIlib_recv_ptz_is_supported(self.instance) }
    }

    pub fn ptz_recall_preset(&self, preset: u32, speed: f32) -> bool {
        unsafe { NDIlib_recv_ptz_recall_preset(self.instance, preset as i32, speed) }
    }

    pub fn ptz_zoom(&self, zoom_value: f32) -> bool {
        unsafe { NDIlib_recv_ptz_zoom(self.instance, zoom_value) }
    }

    pub fn ptz_zoom_speed(&self, zoom_speed: f32) -> bool {
        unsafe { NDIlib_recv_ptz_zoom_speed(self.instance, zoom_speed) }
    }

    pub fn ptz_pan_tilt(&self, pan_value: f32, tilt_value: f32) -> bool {
        unsafe { NDIlib_recv_ptz_pan_tilt(self.instance, pan_value, tilt_value) }
    }

    pub fn ptz_pan_tilt_speed(&self, pan_speed: f32, tilt_speed: f32) -> bool {
        unsafe { NDIlib_recv_ptz_pan_tilt_speed(self.instance, pan_speed, tilt_speed) }
    }

    pub fn ptz_store_preset(&self, preset_no: i32) -> bool {
        unsafe { NDIlib_recv_ptz_store_preset(self.instance, preset_no) }
    }

    pub fn ptz_auto_focus(&self) -> bool {
        unsafe { NDIlib_recv_ptz_auto_focus(self.instance) }
    }

    pub fn ptz_focus(&self, focus_value: f32) -> bool {
        unsafe { NDIlib_recv_ptz_focus(self.instance, focus_value) }
    }

    pub fn ptz_focus_speed(&self, focus_speed: f32) -> bool {
        unsafe { NDIlib_recv_ptz_focus_speed(self.instance, focus_speed) }
    }

    pub fn ptz_white_balance_auto(&self) -> bool {
        unsafe { NDIlib_recv_ptz_white_balance_auto(self.instance) }
    }

    pub fn ptz_white_balance_indoor(&self) -> bool {
        unsafe { NDIlib_recv_ptz_white_balance_indoor(self.instance) }
    }

    pub fn ptz_white_balance_outdoor(&self) -> bool {
        unsafe { NDIlib_recv_ptz_white_balance_outdoor(self.instance) }
    }

    pub fn ptz_white_balance_oneshot(&self) -> bool {
        unsafe { NDIlib_recv_ptz_white_balance_oneshot(self.instance) }
    }

    pub fn ptz_white_balance_manual(&self, red: f32, blue: f32) -> bool {
        unsafe { NDIlib_recv_ptz_white_balance_manual(self.instance, red, blue) }
    }

    pub fn ptz_exposure_auto(&self) -> bool {
        unsafe { NDIlib_recv_ptz_exposure_auto(self.instance) }
    }

    pub fn ptz_exposure_manual(&self, exposure_level: f32) -> bool {
        unsafe { NDIlib_recv_ptz_exposure_manual(self.instance, exposure_level) }
    }

    pub fn ptz_exposure_manual_v2(&self, iris: f32, gain: f32, shutter_speed: f32) -> bool {
        unsafe { NDIlib_recv_ptz_exposure_manual_v2(self.instance, iris, gain, shutter_speed) }
    }
}

impl Drop for Recv {
    fn drop(&mut self) {
        unsafe {
            NDIlib_recv_destroy(self.instance);
        }
    }
}

// Enum to represent different frame types
pub enum FrameType {
    Video(VideoFrame),
    Audio(AudioFrame),
    Metadata(MetadataFrame),
    None,
}

#[derive(Debug, Clone)]
pub struct Tally {
    pub on_program: bool,
    pub on_preview: bool,
}

impl Tally {
    pub fn new(on_program: bool, on_preview: bool) -> Self {
        Tally {
            on_program,
            on_preview,
        }
    }

    pub(crate) fn to_raw(&self) -> NDIlib_tally_t {
        NDIlib_tally_t {
            on_program: self.on_program,
            on_preview: self.on_preview,
        }
    }
}

#[derive(Debug)]
pub struct Send {
    instance: NDIlib_send_instance_t,
}

impl Send {
    pub fn new(create_settings: Sender) -> Result<Self, &'static str> {
        let c_settings = NDIlib_send_create_t {
            p_ndi_name: CString::new(create_settings.name).unwrap().into_raw(),
            p_groups: match create_settings.groups {
                Some(ref groups) => CString::new(groups.clone()).unwrap().into_raw(),
                None => ptr::null(),
            },
            clock_video: create_settings.clock_video,
            clock_audio: create_settings.clock_audio,
        };

        let instance = unsafe { NDIlib_send_create(&c_settings) };
        if instance.is_null() {
            Err("Failed to create NDI send instance")
        } else {
            Ok(Send { instance })
        }
    }

    pub fn send_video(&self, video_frame: &VideoFrame) {
        unsafe {
            NDIlib_send_send_video_v2(self.instance, &video_frame.to_raw());
        }
    }

    pub fn send_video_async(&self, video_frame: &VideoFrame) {
        unsafe {
            NDIlib_send_send_video_async_v2(self.instance, &video_frame.to_raw());
        }
    }

    pub fn send_audio(&self, audio_frame: &AudioFrame) {
        unsafe {
            NDIlib_send_send_audio_v3(self.instance, &audio_frame.to_raw());
        }
    }

    pub fn send_metadata(&self, metadata_frame: &MetadataFrame) {
        unsafe {
            NDIlib_send_send_metadata(self.instance, &metadata_frame.to_raw());
        }
    }

    pub fn capture(&self, timeout_ms: u32) -> Result<FrameType, &'static str> {
        let metadata_frame = MetadataFrame::new();
        let frame_type =
            unsafe { NDIlib_send_capture(self.instance, &mut metadata_frame.to_raw(), timeout_ms) };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => Ok(FrameType::Metadata(
                MetadataFrame::from_raw(metadata_frame.to_raw()),
            )),
            _ => Err("Failed to capture frame"),
        }
    }

    pub fn free_metadata(&self, metadata_frame: &MetadataFrame) {
        unsafe {
            NDIlib_send_free_metadata(self.instance, &metadata_frame.to_raw());
        }
    }

    pub fn get_tally(&self, tally: &mut Tally, timeout_ms: u32) -> bool {
        unsafe { NDIlib_send_get_tally(self.instance, &mut tally.to_raw(), timeout_ms) }
    }

    pub fn get_no_connections(&self, timeout_ms: u32) -> i32 {
        unsafe { NDIlib_send_get_no_connections(self.instance, timeout_ms) }
    }

    pub fn clear_connection_metadata(&self) {
        unsafe { NDIlib_send_clear_connection_metadata(self.instance) }
    }

    pub fn add_connection_metadata(&self, metadata_frame: &MetadataFrame) {
        unsafe { NDIlib_send_add_connection_metadata(self.instance, &metadata_frame.to_raw()) }
    }

    pub fn set_failover(&self, source: &Source) {
        unsafe { NDIlib_send_set_failover(self.instance, &source.to_raw()) }
    }

    pub fn get_source_name(&self) -> Source {
        let source_ptr = unsafe { NDIlib_send_get_source_name(self.instance) };
        Source::from_raw(unsafe { &*source_ptr })
    }
}

impl Drop for Send {
    fn drop(&mut self) {
        unsafe {
            NDIlib_send_destroy(self.instance);
        }
    }
}

#[derive(Debug)]
pub struct Sender {
    pub name: String,
    pub groups: Option<String>,
    pub clock_video: bool,
    pub clock_audio: bool,
}
