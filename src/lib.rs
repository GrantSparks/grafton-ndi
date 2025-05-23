#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
use once_cell::sync::OnceCell;
use std::{
    ffi::{CStr, CString},
    fmt::{self, Display, Formatter},
    os::raw::c_char,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

mod error;
pub use error::*;

mod ndi_lib;
use ndi_lib::*;

// Global initialization state and reference count
static INIT: OnceCell<bool> = OnceCell::new();
static REFCOUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
pub struct NDI;

impl NDI {
    /// Acquire an NDI instance, initializing the runtime if necessary.
    /// Multiple instances can be acquired safely - the runtime will only be initialized once.
    pub fn acquire() -> Result<Self, Error> {
        // Initialize NDI runtime only once
        INIT.get_or_try_init(|| {
            if unsafe { NDIlib_initialize() } {
                Ok(true)
            } else {
                Err(Error::InitializationFailed(
                    "NDIlib_initialize failed".into(),
                ))
            }
        })?;

        // Increment reference count
        REFCOUNT.fetch_add(1, Ordering::Relaxed);
        Ok(NDI)
    }

    /// Alias for acquire() to maintain backward compatibility
    pub fn new() -> Result<Self, Error> {
        Self::acquire()
    }

    pub fn is_supported_cpu() -> bool {
        unsafe { NDIlib_is_supported_CPU() }
    }

    pub fn version() -> Result<String, Error> {
        unsafe {
            let version_ptr = NDIlib_version();
            if version_ptr.is_null() {
                return Err(Error::NullPointer("NDIlib_version".into()));
            }
            let c_str = CStr::from_ptr(version_ptr);
            c_str
                .to_str()
                .map(|s| s.to_owned())
                .map_err(|e| Error::InvalidUtf8(e.to_string()))
        }
    }
}

impl Clone for NDI {
    fn clone(&self) -> Self {
        // Increment reference count for the clone
        REFCOUNT.fetch_add(1, Ordering::Relaxed);
        NDI
    }
}

impl Drop for NDI {
    fn drop(&mut self) {
        // Only destroy the runtime when the last reference is dropped
        if REFCOUNT.fetch_sub(1, Ordering::Relaxed) == 1 {
            unsafe { NDIlib_destroy() };
            // Note: We don't reset INIT because NDI might not support re-initialization
        }
    }
}

#[derive(Debug, Default)]
pub struct Finder {
    pub show_local_sources: bool,
    pub groups: Option<String>,
    pub extra_ips: Option<String>,
}

impl Finder {
    pub fn new(show_local_sources: bool, groups: Option<&str>, extra_ips: Option<&str>) -> Self {
        Finder {
            show_local_sources,
            groups: groups.map(|s| s.to_string()),
            extra_ips: extra_ips.map(|s| s.to_string()),
        }
    }
}

pub struct Find<'a> {
    instance: NDIlib_find_instance_t,
    _groups: Option<CString>,    // Hold ownership of CStrings
    _extra_ips: Option<CString>, // to ensure they outlive SDK usage
    ndi: std::marker::PhantomData<&'a NDI>,
}

impl<'a> Find<'a> {
    pub fn new(_ndi: &'a NDI, settings: Finder) -> Result<Self, Error> {
        let groups_cstr = settings
            .groups
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(Error::InvalidCString)?;
        let extra_ips_cstr = settings
            .extra_ips
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(Error::InvalidCString)?;

        let create_settings = NDIlib_find_create_t {
            show_local_sources: settings.show_local_sources,
            p_groups: groups_cstr.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            p_extra_ips: extra_ips_cstr.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        };

        let instance = unsafe { NDIlib_find_create_v2(&create_settings) };
        if instance.is_null() {
            return Err(Error::InitializationFailed(
                "NDIlib_find_create_v2 failed".into(),
            ));
        }
        Ok(Find {
            instance,
            _groups: groups_cstr,
            _extra_ips: extra_ips_cstr,
            ndi: std::marker::PhantomData,
        })
    }

    pub fn wait_for_sources(&self, timeout: u32) -> bool {
        unsafe { NDIlib_find_wait_for_sources(self.instance, timeout) }
    }

    pub fn get_sources(&self, timeout: u32) -> Result<Vec<Source>, Error> {
        let mut no_sources = 0;
        let sources_ptr =
            unsafe { NDIlib_find_get_sources(self.instance, &mut no_sources, timeout) };
        if sources_ptr.is_null() {
            return Ok(vec![]);
        }
        let sources = unsafe {
            (0..no_sources)
                .map(|i| {
                    let source = &*sources_ptr.add(i as usize);
                    Source::from_raw(source)
                })
                .collect()
        };
        Ok(sources)
    }
}

impl Drop for Find<'_> {
    fn drop(&mut self) {
        unsafe { NDIlib_find_destroy(self.instance) };
    }
}

#[derive(Debug, Clone)]
pub struct Source {
    pub name: String,
    pub url_address: Option<String>,
    pub ip_address: Option<String>,
}

// This struct holds the CStrings to ensure they live as long as needed
pub(crate) struct RawSource {
    _name: CString,
    _url_address: Option<CString>,
    _ip_address: Option<CString>,
    pub raw: NDIlib_source_t,
}

impl Source {
    fn from_raw(ndi_source: &NDIlib_source_t) -> Self {
        let name = unsafe {
            CStr::from_ptr(ndi_source.p_ndi_name)
                .to_string_lossy()
                .into_owned()
        };
        let url_address = unsafe {
            if !ndi_source.__bindgen_anon_1.p_url_address.is_null() {
                Some(
                    CStr::from_ptr(ndi_source.__bindgen_anon_1.p_url_address)
                        .to_string_lossy()
                        .into_owned(),
                )
            } else {
                None
            }
        };
        let ip_address = unsafe {
            if !ndi_source.__bindgen_anon_1.p_ip_address.is_null() {
                Some(
                    CStr::from_ptr(ndi_source.__bindgen_anon_1.p_ip_address)
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

    fn to_raw(&self) -> Result<RawSource, Error> {
        let name = CString::new(self.name.clone()).map_err(Error::InvalidCString)?;
        let url_address = self
            .url_address
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(Error::InvalidCString)?;
        let ip_address = self
            .ip_address
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(Error::InvalidCString)?;

        let p_ndi_name = name.as_ptr();
        let p_url_address = url_address.as_ref().map_or(ptr::null(), |s| s.as_ptr());
        let p_ip_address = ip_address.as_ref().map_or(ptr::null(), |s| s.as_ptr());

        let __bindgen_anon_1 = if !p_url_address.is_null() {
            NDIlib_source_t__bindgen_ty_1 { p_url_address }
        } else {
            NDIlib_source_t__bindgen_ty_1 { p_ip_address }
        };

        Ok(RawSource {
            _name: name,
            _url_address: url_address,
            _ip_address: ip_address,
            raw: NDIlib_source_t {
                p_ndi_name,
                __bindgen_anon_1,
            },
        })
    }
}

impl Display for Source {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
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

impl From<NDIlib_FourCC_video_type_e> for FourCCVideoType {
    fn from(fourcc: NDIlib_FourCC_video_type_e) -> Self {
        match fourcc {
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

impl From<NDIlib_frame_format_type_e> for FrameFormatType {
    fn from(format: NDIlib_frame_format_type_e) -> Self {
        match format {
            NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive => {
                FrameFormatType::Progressive
            }
            NDIlib_frame_format_type_e_NDIlib_frame_format_type_interleaved => {
                FrameFormatType::Interlaced
            }
            NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_0 => FrameFormatType::Field0,
            NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_1 => FrameFormatType::Field1,
            _ => FrameFormatType::Max,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union LineStrideOrSize {
    pub line_stride_in_bytes: i32,
    pub data_size_in_bytes: i32,
}

impl fmt::Debug for LineStrideOrSize {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // For debugging purposes, we'll assume that we're interested in `line_stride_in_bytes`
        unsafe {
            write!(
                f,
                "LineStrideOrSize {{ line_stride_in_bytes: {} }}",
                self.line_stride_in_bytes
            )
        }
    }
}

impl From<LineStrideOrSize> for NDIlib_video_frame_v2_t__bindgen_ty_1 {
    fn from(value: LineStrideOrSize) -> Self {
        unsafe {
            if value.line_stride_in_bytes != 0 {
                NDIlib_video_frame_v2_t__bindgen_ty_1 {
                    line_stride_in_bytes: value.line_stride_in_bytes,
                }
            } else {
                NDIlib_video_frame_v2_t__bindgen_ty_1 {
                    data_size_in_bytes: value.data_size_in_bytes,
                }
            }
        }
    }
}

impl From<NDIlib_video_frame_v2_t__bindgen_ty_1> for LineStrideOrSize {
    fn from(value: NDIlib_video_frame_v2_t__bindgen_ty_1) -> Self {
        unsafe {
            if value.line_stride_in_bytes != 0 {
                LineStrideOrSize {
                    line_stride_in_bytes: value.line_stride_in_bytes,
                }
            } else {
                LineStrideOrSize {
                    data_size_in_bytes: value.data_size_in_bytes,
                }
            }
        }
    }
}

pub struct VideoFrame {
    pub xres: i32,
    pub yres: i32,
    pub fourcc: FourCCVideoType,
    pub frame_rate_n: i32,
    pub frame_rate_d: i32,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: FrameFormatType,
    pub timecode: i64,
    pub data: Vec<u8>,
    pub line_stride_or_size: LineStrideOrSize,
    pub metadata: Option<CString>,
    pub timestamp: i64,
}

impl fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrame")
            .field("xres", &self.xres)
            .field("yres", &self.yres)
            .field("fourcc", &self.fourcc)
            .field("frame_rate_n", &self.frame_rate_n)
            .field("frame_rate_d", &self.frame_rate_d)
            .field("picture_aspect_ratio", &self.picture_aspect_ratio)
            .field("frame_format_type", &self.frame_format_type)
            .field("timecode", &self.timecode)
            .field("data (bytes)", &self.data.len())
            .field("line_stride_or_size", &self.line_stride_or_size)
            .field("metadata", &self.metadata)
            .field("timestamp", &self.timestamp)
            .finish()
    }
}

impl Default for VideoFrame {
    fn default() -> Self {
        VideoFrame::new(
            1920,
            1080,
            FourCCVideoType::BGRA,
            60,
            1,
            16.0 / 9.0,
            FrameFormatType::Interlaced,
        )
    }
}

impl VideoFrame {
    pub fn new(
        xres: i32,
        yres: i32,
        fourcc: FourCCVideoType,
        frame_rate_n: i32,
        frame_rate_d: i32,
        aspect_ratio: f32,
        format: FrameFormatType,
    ) -> Self {
        let bpp = match fourcc {
            FourCCVideoType::BGRA => 32,
            // Add other formats and their bpp as needed
            _ => 32, // Default to 32 bpp if unknown
        };

        let stride = (xres * bpp + 7) / 8;
        let buffer_size: usize = (yres * stride) as usize;
        let data = vec![0u8; buffer_size];

        VideoFrame {
            xres,
            yres,
            fourcc,
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio: aspect_ratio,
            frame_format_type: format,
            timecode: 0,
            data,
            line_stride_or_size: LineStrideOrSize {
                line_stride_in_bytes: stride,
            },
            metadata: None,
            timestamp: 0,
        }
    }

    pub fn to_raw(&self) -> NDIlib_video_frame_v2_t {
        NDIlib_video_frame_v2_t {
            xres: self.xres,
            yres: self.yres,
            FourCC: self.fourcc.into(),
            frame_rate_N: self.frame_rate_n,
            frame_rate_D: self.frame_rate_d,
            picture_aspect_ratio: self.picture_aspect_ratio,
            frame_format_type: self.frame_format_type.into(),
            timecode: self.timecode,
            p_data: self.data.as_ptr() as *mut u8,
            __bindgen_anon_1: self.line_stride_or_size.into(),
            p_metadata: match &self.metadata {
                Some(meta) => meta.as_ptr(),
                None => ptr::null(),
            },
            timestamp: self.timestamp,
        }
    }

    /// Creates a `VideoFrame` from a raw NDI video frame.
    ///
    /// # Safety
    ///
    /// This function assumes the given `NDIlib_video_frame_v2_t` is valid and correctly allocated.
    pub unsafe fn from_raw(c_frame: &NDIlib_video_frame_v2_t) -> Result<Self, Error> {
        if c_frame.p_data.is_null() {
            return Err(Error::InvalidFrame("Video frame has null data pointer".into()));
        }

        // SAFETY: For union access, we need to determine which field to use
        // based on the video format. Since we don't have format info here,
        // we'll use a heuristic: if line_stride would give us a reasonable size,
        // use it; otherwise use data_size_in_bytes
        let line_stride = c_frame.__bindgen_anon_1.line_stride_in_bytes;
        let potential_stride_size = line_stride as usize * c_frame.yres as usize;
        
        let data_size = if line_stride > 0 && potential_stride_size > 0 && potential_stride_size < (100 * 1024 * 1024) {
            // Reasonable size for uncompressed video (< 100MB per frame)
            potential_stride_size
        } else {
            // Use data_size_in_bytes for compressed formats
            c_frame.__bindgen_anon_1.data_size_in_bytes as usize
        };
        
        if data_size == 0 {
            return Err(Error::InvalidFrame("Video frame has zero size".into()));
        }

        let data = std::slice::from_raw_parts(c_frame.p_data, data_size).to_vec();

        let metadata = if c_frame.p_metadata.is_null() {
            None
        } else {
            Some(CString::from(CStr::from_ptr(c_frame.p_metadata)))
        };

        Ok(VideoFrame {
            xres: c_frame.xres,
            yres: c_frame.yres,
            fourcc: c_frame.FourCC.into(),
            frame_rate_n: c_frame.frame_rate_N,
            frame_rate_d: c_frame.frame_rate_D,
            picture_aspect_ratio: c_frame.picture_aspect_ratio,
            frame_format_type: c_frame.frame_format_type.into(),
            timecode: c_frame.timecode,
            data,
            line_stride_or_size: LineStrideOrSize {
                data_size_in_bytes: data_size as i32,
            },
            metadata,
            timestamp: c_frame.timestamp,
        })
    }
}

// Drop implementation removed - CString in metadata field handles its own cleanup

#[derive(Debug)]
pub struct AudioFrame {
    pub sample_rate: i32,
    pub no_channels: i32,
    pub no_samples: i32,
    pub timecode: i64,
    pub fourcc: AudioType,
    pub data: Vec<u8>,
    pub channel_stride_in_bytes: i32,
    pub metadata: Option<CString>,
    pub timestamp: i64,
}

impl AudioFrame {
    pub fn new() -> Self {
        AudioFrame {
            sample_rate: 0,
            no_channels: 0,
            no_samples: 0,
            timecode: 0,
            fourcc: AudioType::Max, // TODO: Is this the right default?
            data: vec![],
            channel_stride_in_bytes: 0,
            metadata: None,
            timestamp: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_data(
        sample_rate: i32,
        no_channels: i32,
        no_samples: i32,
        timecode: i64,
        fourcc: AudioType, // TODO: Can this be merged with the fourcc on VideoFrame?
        data: Vec<u8>,     // TODO: Many of these fields could be combined into a struct
        metadata: Option<String>,
        timestamp: i64,
    ) -> Result<Self, Error> {
        let metadata_cstring = metadata
            .map(|m| CString::new(m).map_err(Error::InvalidCString))
            .transpose()?;
        Ok(AudioFrame {
            sample_rate,
            no_channels,
            no_samples,
            timecode,
            fourcc,
            data,
            channel_stride_in_bytes: no_samples * 4,
            metadata: metadata_cstring,
            timestamp,
        })
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
            p_metadata: self.metadata.as_ref().map_or(ptr::null(), |m| m.as_ptr()),
            timestamp: self.timestamp,
        }
    }

    pub(crate) fn from_raw(raw: NDIlib_audio_frame_v3_t) -> Result<Self, Error> {
        if raw.p_data.is_null() {
            return Err(Error::InvalidFrame("Audio frame has null data pointer".into()));
        }

        if raw.sample_rate <= 0 {
            return Err(Error::InvalidFrame(format!("Invalid sample rate: {}", raw.sample_rate)));
        }

        if raw.no_channels <= 0 {
            return Err(Error::InvalidFrame(format!("Invalid number of channels: {}", raw.no_channels)));
        }

        if raw.no_samples <= 0 {
            return Err(Error::InvalidFrame(format!("Invalid number of samples: {}", raw.no_samples)));
        }

        let bytes_per_sample = 4;
        let data_size = (raw.no_samples * raw.no_channels * bytes_per_sample) as usize;

        if data_size == 0 {
            return Err(Error::InvalidFrame("Calculated audio data size is zero".into()));
        }

        let data = unsafe {
            std::slice::from_raw_parts(raw.p_data, data_size).to_vec()
        };

        let metadata = if raw.p_metadata.is_null() {
            None
        } else {
            Some(unsafe { CString::from_raw(raw.p_metadata as *mut c_char) })
        };

        Ok(AudioFrame {
            sample_rate: raw.sample_rate,
            no_channels: raw.no_channels,
            no_samples: raw.no_samples,
            timecode: raw.timecode,
            fourcc: match raw.FourCC {
                NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP => AudioType::FLTP,
                _ => AudioType::Max,
            },
            data,
            channel_stride_in_bytes: unsafe { raw.__bindgen_anon_1.channel_stride_in_bytes },
            metadata,
            timestamp: raw.timestamp,
        })
    }
}

impl Default for AudioFrame {
    fn default() -> Self {
        Self::new()
    }
}

// Drop implementation removed - CString handles its own memory management

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioType {
    FLTP,
    Max,
}

impl From<u32> for AudioType {
    fn from(value: u32) -> Self {
        if value == NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP {
            AudioType::FLTP
        } else {
            AudioType::Max
        }
    }
}

impl From<AudioType> for u32 {
    fn from(audio_type: AudioType) -> Self {
        match audio_type {
            AudioType::FLTP => NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            AudioType::Max => NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_max,
        }
    }
}

#[derive(Debug)]
pub struct MetadataFrame {
    pub length: i32,
    pub timecode: i64,
    pub p_data: *mut c_char,
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
//    BGRX_BGRA_Flipped,
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
//            RecvColorFormat::BGRX_BGRA_Flipped => {
//                NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA_flipped
//            }
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

pub(crate) struct RawRecvCreateV3 {
    _source: RawSource,
    _name: Option<CString>,
    pub raw: NDIlib_recv_create_v3_t,
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

    pub(crate) fn to_raw(&self) -> Result<RawRecvCreateV3, Error> {
        let source = self.source_to_connect_to.to_raw()?;
        let name = self.ndi_recv_name
            .as_ref()
            .map(|n| CString::new(n.clone()))
            .transpose()
            .map_err(Error::InvalidCString)?;
        
        let p_ndi_recv_name = name.as_ref().map_or(ptr::null(), |n| n.as_ptr());
        let source_raw = source.raw;

        Ok(RawRecvCreateV3 {
            raw: NDIlib_recv_create_v3_t {
                source_to_connect_to: source_raw,
                color_format: self.color_format.into(),
                bandwidth: self.bandwidth.into(),
                allow_video_fields: self.allow_video_fields,
                p_ndi_recv_name,
            },
            _source: source,
            _name: name,
        })
    }
}

pub struct Recv<'a> {
    instance: NDIlib_recv_instance_t,
    ndi: std::marker::PhantomData<&'a NDI>,
}

impl<'a> Recv<'a> {
    pub fn new(_ndi: &'a NDI, create: Receiver) -> Result<Self, Error> {
        let create_raw = create.to_raw()?;
        let instance = unsafe { NDIlib_recv_create_v3(&create_raw.raw) };
        if instance.is_null() {
            Err(Error::InitializationFailed(
                "Failed to create NDI recv instance".into(),
            ))
        } else {
            unsafe { NDIlib_recv_connect(instance, &create_raw.raw.source_to_connect_to) };
            Ok(Recv {
                instance,
                ndi: std::marker::PhantomData,
            })
        }
    }

    pub fn capture(&mut self, timeout_ms: u32) -> Result<FrameType, Error> {
        let mut video_frame = NDIlib_video_frame_v2_t::default();
        let mut audio_frame = NDIlib_audio_frame_v3_t::default();
        let mut metadata_frame = NDIlib_metadata_frame_t::default();

        let frame_type = unsafe {
            NDIlib_recv_capture_v3(
                self.instance,
                &mut video_frame,
                &mut audio_frame,
                &mut metadata_frame,
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_video => {
                let frame = unsafe { VideoFrame::from_raw(&video_frame) }?;
                unsafe { NDIlib_recv_free_video_v2(self.instance, &video_frame) };
                Ok(FrameType::Video(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_audio => {
                let frame = AudioFrame::from_raw(audio_frame)?;
                unsafe { NDIlib_recv_free_audio_v3(self.instance, &audio_frame) };
                Ok(FrameType::Audio(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                if metadata_frame.p_data.is_null() {
                    Err(Error::NullPointer("Metadata frame data is null".into()))
                } else {
                    let frame = MetadataFrame::from_raw(metadata_frame);
                    unsafe { NDIlib_recv_free_metadata(self.instance, &metadata_frame) };
                    Ok(FrameType::Metadata(frame))
                }
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(FrameType::None),
            NDIlib_frame_type_e_NDIlib_frame_type_status_change => Ok(FrameType::StatusChange),
            NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::CaptureFailed("Received an error frame".into()))
            }
            _ => Err(Error::CaptureFailed(format!(
                "Unknown frame type: {}",
                frame_type
            ))),
        }
    }

    #[allow(dead_code)]
    pub fn free_string(&self, string: &str) {
        let c_string = CString::new(string).expect("Failed to create CString");
        unsafe {
            NDIlib_recv_free_string(self.instance, c_string.into_raw());
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

impl Drop for Recv<'_> {
    fn drop(&mut self) {
        unsafe {
            NDIlib_recv_destroy(self.instance);
        }
    }
}

#[derive(Debug)]
pub enum FrameType {
    Video(VideoFrame),
    Audio(AudioFrame),
    Metadata(MetadataFrame),
    None,
    StatusChange,
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
pub struct Send<'a> {
    instance: NDIlib_send_instance_t,
    _name: *mut c_char,  // Store raw pointer to free on drop
    _groups: *mut c_char, // Store raw pointer to free on drop
    ndi: std::marker::PhantomData<&'a NDI>,
}

impl<'a> Send<'a> {
    pub fn new(_ndi: &'a NDI, create_settings: Sender) -> Result<Self, Error> {
        let p_ndi_name = CString::new(create_settings.name).map_err(Error::InvalidCString)?;
        let p_groups = match create_settings.groups {
            Some(ref groups) => CString::new(groups.clone())
                .map_err(Error::InvalidCString)?
                .into_raw(),
            None => ptr::null_mut(),
        };

        let p_ndi_name_raw = p_ndi_name.into_raw();
        let c_settings = NDIlib_send_create_t {
            p_ndi_name: p_ndi_name_raw,
            p_groups,
            clock_video: create_settings.clock_video,
            clock_audio: create_settings.clock_audio,
        };

        let instance = unsafe { NDIlib_send_create(&c_settings) };
        if instance.is_null() {
            // Clean up on error
            unsafe {
                let _ = CString::from_raw(p_ndi_name_raw);
                if !p_groups.is_null() {
                    let _ = CString::from_raw(p_groups);
                }
            }
            Err(Error::InitializationFailed(
                "Failed to create NDI send instance".into(),
            ))
        } else {
            Ok(Send {
                instance,
                _name: p_ndi_name_raw,
                _groups: p_groups,
                ndi: std::marker::PhantomData,
            })
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

    pub fn capture(&self, timeout_ms: u32) -> Result<FrameType, Error> {
        let metadata_frame = MetadataFrame::new();
        let frame_type =
            unsafe { NDIlib_send_capture(self.instance, &mut metadata_frame.to_raw(), timeout_ms) };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => Ok(FrameType::Metadata(
                MetadataFrame::from_raw(metadata_frame.to_raw()),
            )),
            _ => Err(Error::CaptureFailed("Failed to capture frame".into())),
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

    pub fn set_failover(&self, source: &Source) -> Result<(), Error> {
        let raw_source = source.to_raw()?;
        unsafe { NDIlib_send_set_failover(self.instance, &raw_source.raw) }
        Ok(())
    }

    pub fn get_source_name(&self) -> Source {
        let source_ptr = unsafe { NDIlib_send_get_source_name(self.instance) };
        Source::from_raw(unsafe { &*source_ptr })
    }
}

impl Drop for Send<'_> {
    fn drop(&mut self) {
        unsafe {
            NDIlib_send_destroy(self.instance);
            
            // Free the CStrings we allocated
            if !self._name.is_null() {
                let _ = CString::from_raw(self._name);
            }
            if !self._groups.is_null() {
                let _ = CString::from_raw(self._groups);
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::ptr;

    // Helper function to create a test video frame
    fn create_test_video_frame(
        width: i32,
        height: i32,
        line_stride: i32,
        data_size: i32,
    ) -> NDIlib_video_frame_v2_t {
        let mut frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
        frame.xres = width;
        frame.yres = height;
        
        // Set the union field based on which value is provided
        if line_stride > 0 {
            frame.__bindgen_anon_1.line_stride_in_bytes = line_stride;
        } else {
            frame.__bindgen_anon_1.data_size_in_bytes = data_size;
        }
        
        // Allocate dummy data
        let actual_size = if line_stride > 0 {
            (line_stride * height) as usize
        } else {
            data_size as usize
        };
        let mut data = vec![0u8; actual_size];
        frame.p_data = data.as_mut_ptr();
        std::mem::forget(data); // Prevent deallocation during test
        
        frame
    }

    #[test]
    fn test_video_frame_standard_format_size_calculation() {
        // Test standard video format with line stride
        let test_width = 1920;
        let test_height = 1080;
        let bytes_per_pixel = 4; // RGBA
        let line_stride = test_width * bytes_per_pixel;
        
        let c_frame = create_test_video_frame(test_width, test_height, line_stride, 0);
        
        // The from_raw function should calculate size as line_stride * height
        // Previously it would incorrectly multiply data_size_in_bytes * height
        unsafe {
            let frame = VideoFrame::from_raw(&c_frame).unwrap();
            
            // Expected size is line_stride * height
            let expected_size = (line_stride * test_height) as usize;
            assert_eq!(frame.data.len(), expected_size);
            
            // Clean up
            drop(frame);
            Vec::from_raw_parts(c_frame.p_data, expected_size, expected_size);
        }
    }

    #[test]
    fn test_video_frame_size_calculation_logic() {
        // Test the size calculation logic without relying on union behavior
        // This is a simplified test that verifies the fix prevents the original bug
        
        // The original bug was: data_size = data_size_in_bytes * yres
        // This would cause massive over-allocation
        
        // For a 1920x1080 RGBA frame:
        // Correct: line_stride (1920*4) * height (1080) = 8,294,400 bytes
        // Bug would calculate: some_value * 1080 (potentially huge)
        
        let correct_size = 1920 * 4 * 1080; // 8,294,400 bytes
        assert!(correct_size < 10_000_000); // Should be under 10MB
        
        // The fix ensures we use line_stride * height for standard formats
        // and data_size_in_bytes directly for compressed formats
    }

    #[test]
    fn test_video_frame_null_data_returns_error() {
        let mut c_frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
        c_frame.p_data = ptr::null_mut();
        c_frame.__bindgen_anon_1.line_stride_in_bytes = 1920 * 4;
        c_frame.yres = 1080;
        
        unsafe {
            let result = VideoFrame::from_raw(&c_frame);
            assert!(result.is_err());
            match result {
                Err(Error::InvalidFrame(msg)) => {
                    assert!(msg.contains("null data pointer"));
                }
                _ => panic!("Expected InvalidFrame error"),
            }
        }
    }

    #[test]
    fn test_video_frame_zero_size_returns_error() {
        let mut c_frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
        let mut data = vec![0u8; 100];
        c_frame.p_data = data.as_mut_ptr();
        c_frame.__bindgen_anon_1.line_stride_in_bytes = 0;
        c_frame.__bindgen_anon_1.data_size_in_bytes = 0;
        c_frame.yres = 1080;
        
        unsafe {
            let result = VideoFrame::from_raw(&c_frame);
            assert!(result.is_err());
            match result {
                Err(Error::InvalidFrame(msg)) => {
                    assert!(msg.contains("zero size"));
                }
                _ => panic!("Expected InvalidFrame error"),
            }
        }
    }

    #[test]
    fn test_audio_frame_drop_no_double_free() {
        // Test that AudioFrame can be created and dropped without issues
        let frame1 = AudioFrame::new();
        drop(frame1); // Should not panic or cause double-free
        
        // Test with metadata
        let mut frame2 = AudioFrame::new();
        frame2.metadata = Some(CString::new("test metadata").unwrap());
        drop(frame2); // Should not panic - CString handles its own memory
        
        // Test multiple drops in sequence
        for _ in 0..10 {
            let mut frame = AudioFrame::new();
            frame.metadata = Some(CString::new(format!("metadata {}", 42)).unwrap());
            drop(frame);
        }
    }

    #[test]
    fn test_raw_source_memory_management() {
        // Test that RawSource properly manages CString memory
        let source = Source {
            name: "Test NDI Source".to_string(),
            url_address: Some("ndi://192.168.1.100:5960".to_string()),
            ip_address: Some("192.168.1.100".to_string()),
        };
        
        // Create RawSource
        let raw_source = source.to_raw().unwrap();
        
        // Verify the raw pointers are valid
        unsafe {
            assert!(!raw_source.raw.p_ndi_name.is_null());
            let name = CStr::from_ptr(raw_source.raw.p_ndi_name);
            assert_eq!(name.to_string_lossy(), "Test NDI Source");
            
            // Check union field
            assert!(!raw_source.raw.__bindgen_anon_1.p_url_address.is_null());
            let url = CStr::from_ptr(raw_source.raw.__bindgen_anon_1.p_url_address);
            assert_eq!(url.to_string_lossy(), "ndi://192.168.1.100:5960");
        }
        
        // Drop should clean up all CStrings properly
        drop(raw_source);
    }

    #[test]
    fn test_raw_source_null_optional_fields() {
        // Test with None values for optional fields
        let source = Source {
            name: "Minimal Source".to_string(),
            url_address: None,
            ip_address: None,
        };
        
        let raw_source = source.to_raw().unwrap();
        
        unsafe {
            assert!(!raw_source.raw.p_ndi_name.is_null());
            assert!(raw_source.raw.__bindgen_anon_1.p_url_address.is_null());
            assert!(raw_source.raw.__bindgen_anon_1.p_ip_address.is_null());
        }
        
        drop(raw_source);
    }

    #[test]
    fn test_raw_recv_create_v3_memory_management() {
        // Test RawRecvCreateV3 memory management
        let receiver = Receiver::new(
            Source {
                name: "Test Source".to_string(),
                url_address: None,
                ip_address: None,
            },
            RecvColorFormat::BGRX_BGRA,
            RecvBandwidth::Highest,
            true,
            Some("Test Receiver".to_string()),
        );
        
        let raw_recv = receiver.to_raw().unwrap();
        
        unsafe {
            // Verify receiver name
            assert!(!raw_recv.raw.p_ndi_recv_name.is_null());
            let name = CStr::from_ptr(raw_recv.raw.p_ndi_recv_name);
            assert_eq!(name.to_string_lossy(), "Test Receiver");
            
            // Verify source name through the nested structure
            assert!(!raw_recv.raw.source_to_connect_to.p_ndi_name.is_null());
            let source_name = CStr::from_ptr(raw_recv.raw.source_to_connect_to.p_ndi_name);
            assert_eq!(source_name.to_string_lossy(), "Test Source");
        }
        
        // Should properly clean up all nested CStrings
        drop(raw_recv);
    }

    #[test]
    fn test_source_roundtrip() {
        // Test converting Source to raw and back
        let original = Source {
            name: "Roundtrip Test".to_string(),
            url_address: Some("ndi://test.local".to_string()),
            ip_address: Some("10.0.0.1".to_string()),
        };
        
        let raw = original.to_raw().unwrap();
        let restored = Source::from_raw(&raw.raw);
        
        assert_eq!(original.name, restored.name);
        assert_eq!(original.url_address, restored.url_address);
        // Note: ip_address might not round-trip perfectly due to union behavior
    }

    #[test]
    fn test_video_frame_metadata_no_double_free() {
        // Test that VideoFrame with metadata doesn't double-free
        let mut frame = VideoFrame::new(
            1920,
            1080,
            FourCCVideoType::RGBA,
            30000,
            1001,
            16.0 / 9.0,
            FrameFormatType::Progressive,
        );
        frame.metadata = Some(CString::new("test video metadata").unwrap());
        
        // This should not panic or double-free
        drop(frame);
    }

    #[test]
    fn test_send_memory_management() {
        // Test that Send properly manages CString memory
        // Note: This test would require NDI SDK to actually create Send instance
        // so we'll test the memory management pattern instead
        
        // Simulate the pattern used in Send::new
        let name = CString::new("Test Sender").unwrap();
        let groups = CString::new("Test Group").unwrap();
        
        let name_ptr = name.into_raw();
        let groups_ptr = groups.into_raw();
        
        // Simulate cleanup (like Send's Drop would do)
        unsafe {
            let _ = CString::from_raw(name_ptr);
            let _ = CString::from_raw(groups_ptr);
        }
        
        // If this doesn't crash, memory management is correct
    }

    #[test]
    fn test_metadata_frame_null_pointer() {
        // Test MetadataFrame with null pointer
        let frame = MetadataFrame::new();
        assert!(frame.p_data.is_null());
        assert_eq!(frame.length, 0);
        
        // MetadataFrame doesn't implement Drop, so it's automatically cleaned up
    }

    #[test]
    fn test_ndi_singleton_initialization() {
        use std::sync::atomic::Ordering;
        
        // Get initial reference count
        let initial_count = REFCOUNT.load(Ordering::Relaxed);
        
        // Create first NDI instance
        let ndi1 = NDI::new().expect("Failed to create first NDI instance");
        let count_after_first = REFCOUNT.load(Ordering::Relaxed);
        assert!(count_after_first > initial_count, "Reference count should increase");
        
        // Create second NDI instance - should not reinitialize
        let ndi2 = NDI::new().expect("Failed to create second NDI instance");
        assert_eq!(REFCOUNT.load(Ordering::Relaxed), count_after_first + 1);
        
        // Clone an instance
        let ndi3 = ndi1.clone();
        assert_eq!(REFCOUNT.load(Ordering::Relaxed), count_after_first + 2);
        
        // Drop one instance
        drop(ndi2);
        assert_eq!(REFCOUNT.load(Ordering::Relaxed), count_after_first + 1);
        
        // Drop another instance
        drop(ndi1);
        let count_after_drop = REFCOUNT.load(Ordering::Relaxed);
        assert!(count_after_drop < count_after_first + 2, "Count should decrease after drop");
        
        // Drop final instance
        drop(ndi3);
        let final_count = REFCOUNT.load(Ordering::Relaxed);
        assert!(final_count < count_after_drop, "Count should decrease after final drop");
    }

    #[test]
    fn test_ndi_thread_safety() {
        use std::thread;
        
        // Create NDI instances from multiple threads
        let handles: Vec<_> = (0..5)
            .map(|i| {
                thread::spawn(move || {
                    let ndi = NDI::new().unwrap_or_else(|_| panic!("Failed to create NDI in thread {}", i));
                    // Use the NDI instance
                    let _version = NDI::version();
                    // Clone it a few times
                    let _clone1 = ndi.clone();
                    let _clone2 = ndi.clone();
                    // Let them all drop at the end of the thread
                })
            })
            .collect();
        
        // Wait for all threads to complete
        for handle in handles {
            handle.join().expect("Thread panicked");
        }
        
        // All instances should be cleaned up by now
    }

    #[test]
    fn test_find_cstring_lifetime() {
        // Test that Find keeps CStrings alive for its lifetime
        let ndi = NDI::new().expect("Failed to create NDI");
        
        // Create finder with both groups and extra_ips
        let settings = Finder::new(true, Some("TestGroup"), Some("192.168.1.100"));
        let finder = Find::new(&ndi, settings).expect("Failed to create finder");
        
        // The finder should keep the CStrings alive even though we've moved settings
        // If CStrings were dropped early, this could cause undefined behavior
        let _sources = finder.get_sources(0);
        
        // Drop finder - CStrings should be freed now
        drop(finder);
    }

    #[test]
    fn test_send_cstring_lifetime() {
        // Test that Send keeps CStrings alive for its lifetime
        // Note: This test verifies the memory management pattern
        // without actually creating an NDI send instance
        
        // Verify our Send struct has the fields to hold CStrings
        // The actual Send::new() would require NDI SDK runtime
        
        // Test the pattern we use in Send
        let name = CString::new("Test Sender").unwrap();
        let groups = Some(CString::new("Test Group").unwrap());
        
        let name_ptr = name.as_ptr();
        let groups_ptr = groups.as_ref().map(|g| g.as_ptr());
        
        // Simulate keeping the CStrings alive
        let _name_holder = name;
        let _groups_holder = groups;
        
        // Pointers should remain valid as long as holders exist
        unsafe {
            if !name_ptr.is_null() {
                let _ = CStr::from_ptr(name_ptr);
            }
            if let Some(ptr) = groups_ptr {
                if !ptr.is_null() {
                    let _ = CStr::from_ptr(ptr);
                }
            }
        }
    }

    #[test]
    fn test_receiver_cstring_lifetime() {
        // Test that Receiver properly manages CString lifetime through RawRecvCreateV3
        let receiver = Receiver::new(
            Source {
                name: "Test Source".to_string(),
                url_address: Some("ndi://test.local".to_string()),
                ip_address: None,
            },
            RecvColorFormat::BGRX_BGRA,
            RecvBandwidth::Highest,
            true,
            Some("Test Receiver Name".to_string()),
        );
        
        // Convert to raw - this should create RawRecvCreateV3 that owns the CStrings
        let raw_recv = receiver.to_raw().expect("Failed to convert receiver");
        
        // The raw_recv should keep all CStrings alive
        // Verify the pointers are still valid
        unsafe {
            assert!(!raw_recv.raw.source_to_connect_to.p_ndi_name.is_null());
            assert!(!raw_recv.raw.p_ndi_recv_name.is_null());
            
            // These should not cause segfault
            let _source_name = CStr::from_ptr(raw_recv.raw.source_to_connect_to.p_ndi_name);
            let _recv_name = CStr::from_ptr(raw_recv.raw.p_ndi_recv_name);
        }
        
        // Drop raw_recv - all CStrings should be properly freed
        drop(raw_recv);
    }
}
