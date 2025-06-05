//! Frame types for video, audio, and metadata.

use std::{
    borrow::Cow,
    ffi::{CStr, CString},
    fmt,
    marker::PhantomData,
    os::raw::c_char,
    ptr, slice,
};

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{ndi_lib::*, receiver::Receiver, Error, Result};

/// Video pixel format identifiers (FourCC codes).
///
/// These represent the various pixel formats supported by NDI for video frames.
/// The most common formats are BGRA/RGBA for full quality and UYVY for bandwidth-efficient streaming.
///
/// # Examples
///
/// ```
/// use grafton_ndi::FourCCVideoType;
///
/// // For maximum compatibility and quality
/// let format = FourCCVideoType::BGRA;
///
/// // For bandwidth-efficient streaming
/// let format = FourCCVideoType::UYVY;
/// ```
#[derive(Debug, TryFromPrimitive, IntoPrimitive, Clone, Copy)]
#[repr(u32)]
pub enum FourCCVideoType {
    /// YCbCr 4:2:2 format (16 bits per pixel) - bandwidth efficient.
    UYVY = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVY as _,
    /// YCbCr 4:2:2 with alpha channel (24 bits per pixel).
    UYVA = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVA as _,
    /// 16-bit YCbCr 4:2:2 format.
    P216 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_P216 as _,
    /// 16-bit YCbCr 4:2:2 with alpha.
    PA16 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_PA16 as _,
    /// Planar YCbCr 4:2:0 format (12 bits per pixel).
    YV12 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_YV12 as _,
    /// Planar YCbCr 4:2:0 format (12 bits per pixel).
    I420 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_I420 as _,
    /// Semi-planar YCbCr 4:2:0 format (12 bits per pixel).
    NV12 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NV12 as _,
    /// Blue-Green-Red-Alpha format (32 bits per pixel) - full quality.
    BGRA = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA as _,
    /// Blue-Green-Red with padding (32 bits per pixel).
    BGRX = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRX as _,
    /// Red-Green-Blue-Alpha format (32 bits per pixel) - full quality.
    RGBA = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBA as _,
    /// Red-Green-Blue with padding (32 bits per pixel).
    RGBX = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBX as _,
    Max = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_max as _,
}

impl From<FourCCVideoType> for i32 {
    fn from(value: FourCCVideoType) -> Self {
        let u32_value: u32 = value.into();
        u32_value as i32
    }
}

#[derive(Debug, TryFromPrimitive, IntoPrimitive, Clone, Copy)]
#[repr(u32)]
pub enum FrameFormatType {
    Progressive = NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive as _,
    Interlaced = NDIlib_frame_format_type_e_NDIlib_frame_format_type_interleaved as _,
    Field0 = NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_0 as _,
    Field1 = NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_1 as _,
    Max = NDIlib_frame_format_type_e_NDIlib_frame_format_type_max as _,
}

impl From<FrameFormatType> for i32 {
    fn from(value: FrameFormatType) -> Self {
        let u32_value: u32 = value.into();
        u32_value as i32
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

pub struct VideoFrame<'rx> {
    pub width: i32,
    pub height: i32,
    pub fourcc: FourCCVideoType,
    pub frame_rate_n: i32,
    pub frame_rate_d: i32,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: FrameFormatType,
    pub timecode: i64,
    pub data: Cow<'rx, [u8]>,
    pub line_stride_or_size: LineStrideOrSize,
    pub metadata: Option<CString>,
    pub timestamp: i64,
    pub(crate) recv_instance: Option<NDIlib_recv_instance_t>,
    // Store original SDK data pointer for proper freeing
    pub(crate) original_p_data: Option<*mut u8>,
    pub(crate) _origin: PhantomData<&'rx Receiver<'rx>>,
}

impl fmt::Debug for VideoFrame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width)
            .field("height", &self.height)
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

impl Default for VideoFrame<'_> {
    fn default() -> Self {
        VideoFrame::builder()
            .resolution(1920, 1080)
            .fourcc(FourCCVideoType::BGRA)
            .frame_rate(60, 1)
            .aspect_ratio(16.0 / 9.0)
            .format(FrameFormatType::Interlaced)
            .build()
            .expect("Default VideoFrame should always succeed")
    }
}

impl<'rx> VideoFrame<'rx> {
    pub fn to_raw(&self) -> NDIlib_video_frame_v2_t {
        NDIlib_video_frame_v2_t {
            xres: self.width,
            yres: self.height,
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

    /// Creates a `VideoFrame` from a raw NDI video frame with owned data.
    ///
    /// # Safety
    ///
    /// This function assumes the given `NDIlib_video_frame_v2_t` is valid and correctly allocated.
    /// This method copies the data, so the VideoFrame owns its data and can outlive the source.
    pub unsafe fn from_raw(
        c_frame: &NDIlib_video_frame_v2_t,
        recv_instance: Option<NDIlib_recv_instance_t>,
    ) -> Result<VideoFrame<'static>> {
        if c_frame.p_data.is_null() {
            return Err(Error::InvalidFrame(
                "Video frame has null data pointer".into(),
            ));
        }

        #[allow(clippy::unnecessary_cast)] // Required for Windows where FourCC is i32
        let fourcc =
            FourCCVideoType::try_from(c_frame.FourCC as u32).unwrap_or(FourCCVideoType::Max);

        // Determine data size based on whether we have line_stride or data_size_in_bytes
        // The NDI SDK uses a union here: line_stride_in_bytes for uncompressed formats,
        // data_size_in_bytes for compressed formats.
        let data_size_in_bytes = c_frame.__bindgen_anon_1.data_size_in_bytes;
        let line_stride = c_frame.__bindgen_anon_1.line_stride_in_bytes;

        // Determine if this is an uncompressed format
        let is_uncompressed = is_uncompressed_format(fourcc);

        let (data_size, line_stride_or_size) =
            if is_uncompressed && line_stride > 0 && c_frame.yres > 0 {
                // Uncompressed format: use line_stride * height
                let calculated_size = (line_stride as usize) * (c_frame.yres as usize);
                if calculated_size > 0 && calculated_size <= (100 * 1024 * 1024) {
                    // Reasonable size for uncompressed video (< 100MB per frame)
                    (
                        calculated_size,
                        LineStrideOrSize {
                            line_stride_in_bytes: line_stride,
                        },
                    )
                } else {
                    return Err(Error::InvalidFrame(format!(
                        "Invalid calculated size {} for uncompressed format",
                        calculated_size
                    )));
                }
            } else if !is_uncompressed && data_size_in_bytes > 0 {
                // Compressed format: use the explicit data size
                (
                    data_size_in_bytes as usize,
                    LineStrideOrSize { data_size_in_bytes },
                )
            } else if data_size_in_bytes > 0 {
                // Fallback: use data_size_in_bytes if available
                (
                    data_size_in_bytes as usize,
                    LineStrideOrSize { data_size_in_bytes },
                )
            } else {
                // Neither field is valid - this is an error
                return Err(Error::InvalidFrame(
                    "Video frame has neither valid line_stride_in_bytes nor data_size_in_bytes"
                        .into(),
                ));
            };

        if data_size == 0 {
            return Err(Error::InvalidFrame("Video frame has zero size".into()));
        }

        // For zero-copy: just borrow the data slice from the SDK
        let (data, original_p_data) = if recv_instance.is_some() {
            // We're receiving - don't copy, just borrow
            let slice = slice::from_raw_parts(c_frame.p_data, data_size);
            (Cow::Borrowed(slice), Some(c_frame.p_data))
        } else {
            // Not from receive - make a copy for ownership
            let slice = slice::from_raw_parts(c_frame.p_data, data_size);
            (Cow::Owned(slice.to_vec()), None)
        };

        let metadata = if c_frame.p_metadata.is_null() {
            None
        } else {
            Some(CString::from(CStr::from_ptr(c_frame.p_metadata)))
        };

        Ok(VideoFrame {
            width: c_frame.xres,
            height: c_frame.yres,
            fourcc,
            frame_rate_n: c_frame.frame_rate_N,
            frame_rate_d: c_frame.frame_rate_D,
            picture_aspect_ratio: c_frame.picture_aspect_ratio,
            #[allow(clippy::unnecessary_cast)] // Required for Windows where frame_format_type is i32
            frame_format_type: FrameFormatType::try_from(c_frame.frame_format_type as u32)
                .unwrap_or(FrameFormatType::Max),
            timecode: c_frame.timecode,
            data,
            line_stride_or_size,
            metadata,
            timestamp: c_frame.timestamp,
            recv_instance,
            original_p_data,
            _origin: PhantomData,
        })
    }

    /// Create a builder for configuring a video frame
    pub fn builder() -> VideoFrameBuilder<'rx> {
        VideoFrameBuilder::new()
    }
}

/// Builder for configuring a VideoFrame with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct VideoFrameBuilder<'rx> {
    width: Option<i32>,
    height: Option<i32>,
    fourcc: Option<FourCCVideoType>,
    frame_rate_n: Option<i32>,
    frame_rate_d: Option<i32>,
    picture_aspect_ratio: Option<f32>,
    frame_format_type: Option<FrameFormatType>,
    timecode: Option<i64>,
    metadata: Option<String>,
    timestamp: Option<i64>,
    _phantom: PhantomData<&'rx ()>,
}

impl<'rx> VideoFrameBuilder<'rx> {
    /// Create a new builder with no fields set
    pub fn new() -> Self {
        Self {
            width: None,
            height: None,
            fourcc: None,
            frame_rate_n: None,
            frame_rate_d: None,
            picture_aspect_ratio: None,
            frame_format_type: None,
            timecode: None,
            metadata: None,
            timestamp: None,
            _phantom: PhantomData,
        }
    }

    /// Set the video resolution
    #[must_use]
    pub fn resolution(mut self, width: i32, height: i32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// Set the pixel format
    #[must_use]
    pub fn fourcc(mut self, fourcc: FourCCVideoType) -> Self {
        self.fourcc = Some(fourcc);
        self
    }

    /// Set the frame rate as a fraction (e.g., 30000/1001 for 29.97fps)
    #[must_use]
    pub fn frame_rate(mut self, numerator: i32, denominator: i32) -> Self {
        self.frame_rate_n = Some(numerator);
        self.frame_rate_d = Some(denominator);
        self
    }

    /// Set the picture aspect ratio
    #[must_use]
    pub fn aspect_ratio(mut self, ratio: f32) -> Self {
        self.picture_aspect_ratio = Some(ratio);
        self
    }

    /// Set the frame format type (progressive, interlaced, etc.)
    #[must_use]
    pub fn format(mut self, format: FrameFormatType) -> Self {
        self.frame_format_type = Some(format);
        self
    }

    /// Set the timecode
    #[must_use]
    pub fn timecode(mut self, tc: i64) -> Self {
        self.timecode = Some(tc);
        self
    }

    /// Set metadata
    #[must_use]
    pub fn metadata<S: Into<String>>(mut self, meta: S) -> Self {
        self.metadata = Some(meta.into());
        self
    }

    /// Set the timestamp
    #[must_use]
    pub fn timestamp(mut self, ts: i64) -> Self {
        self.timestamp = Some(ts);
        self
    }

    /// Build the VideoFrame
    pub fn build(self) -> Result<VideoFrame<'rx>> {
        let width = self.width.unwrap_or(1920);
        let height = self.height.unwrap_or(1080);
        let fourcc = self.fourcc.unwrap_or(FourCCVideoType::BGRA);
        let frame_rate_n = self.frame_rate_n.unwrap_or(60);
        let frame_rate_d = self.frame_rate_d.unwrap_or(1);
        let picture_aspect_ratio = self.picture_aspect_ratio.unwrap_or(16.0 / 9.0);
        let frame_format_type = self
            .frame_format_type
            .unwrap_or(FrameFormatType::Progressive);

        // Calculate stride and buffer size
        let stride = calculate_line_stride(fourcc, width);
        let buffer_size = calculate_buffer_size(fourcc, width, height);
        let data = vec![0u8; buffer_size];

        let mut frame = VideoFrame {
            width,
            height,
            fourcc,
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio,
            frame_format_type,
            timecode: self.timecode.unwrap_or(0),
            data: Cow::Owned(data),
            line_stride_or_size: LineStrideOrSize {
                line_stride_in_bytes: stride,
            },
            metadata: None,
            timestamp: self.timestamp.unwrap_or(0),
            recv_instance: None,
            original_p_data: None,
            _origin: PhantomData,
        };

        if let Some(meta) = self.metadata {
            frame.metadata = Some(CString::new(meta).map_err(Error::InvalidCString)?);
        }

        Ok(frame)
    }
}

impl Default for VideoFrameBuilder<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VideoFrame<'_> {
    fn drop(&mut self) {
        // If this frame originated from a Recv instance and we have the original SDK pointer, free it
        if let (Some(recv_instance), Some(original_p_data)) =
            (self.recv_instance, self.original_p_data)
        {
            // Create a raw frame with the original SDK pointer for NDI to free
            let raw_frame = NDIlib_video_frame_v2_t {
                xres: self.width,
                yres: self.height,
                FourCC: self.fourcc.into(),
                frame_rate_N: self.frame_rate_n,
                frame_rate_D: self.frame_rate_d,
                picture_aspect_ratio: self.picture_aspect_ratio,
                frame_format_type: self.frame_format_type.into(),
                timecode: self.timecode,
                p_data: original_p_data,
                __bindgen_anon_1: self.line_stride_or_size.into(),
                p_metadata: match &self.metadata {
                    Some(meta) => meta.as_ptr(),
                    None => ptr::null(),
                },
                timestamp: self.timestamp,
            };
            unsafe {
                NDIlib_recv_free_video_v2(recv_instance, &raw_frame);
            }
        }
    }
}

#[derive(Debug)]
pub struct AudioFrame<'rx> {
    pub sample_rate: i32,
    pub num_channels: i32,
    pub num_samples: i32,
    pub timecode: i64,
    pub fourcc: AudioType,
    data: Cow<'rx, [f32]>,
    pub channel_stride_in_bytes: i32,
    pub metadata: Option<CString>,
    pub timestamp: i64,
    pub(crate) recv_instance: Option<NDIlib_recv_instance_t>,
    // Store original SDK data pointer for proper freeing
    pub(crate) original_p_data: Option<*mut u8>,
    pub(crate) _origin: PhantomData<&'rx Receiver<'rx>>,
}

impl<'rx> AudioFrame<'rx> {
    pub(crate) fn to_raw(&self) -> NDIlib_audio_frame_v3_t {
        NDIlib_audio_frame_v3_t {
            sample_rate: self.sample_rate,
            no_channels: self.num_channels,
            no_samples: self.num_samples,
            timecode: self.timecode,
            FourCC: self.fourcc.into(),
            p_data: self.data.as_ptr() as *mut f32 as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: self.channel_stride_in_bytes,
            },
            p_metadata: self.metadata.as_ref().map_or(ptr::null(), |m| m.as_ptr()),
            timestamp: self.timestamp,
        }
    }

    pub(crate) fn from_raw(
        raw: NDIlib_audio_frame_v3_t,
        recv_instance: Option<NDIlib_recv_instance_t>,
    ) -> Result<AudioFrame<'static>> {
        if raw.p_data.is_null() {
            return Err(Error::InvalidFrame(
                "Audio frame has null data pointer".into(),
            ));
        }

        if raw.sample_rate <= 0 {
            return Err(Error::InvalidFrame(format!(
                "Invalid sample rate: {}",
                raw.sample_rate
            )));
        }

        if raw.no_channels <= 0 {
            return Err(Error::InvalidFrame(format!(
                "Invalid number of channels: {}",
                raw.no_channels
            )));
        }

        if raw.no_samples <= 0 {
            return Err(Error::InvalidFrame(format!(
                "Invalid number of samples: {}",
                raw.no_samples
            )));
        }

        let sample_count = (raw.no_samples * raw.no_channels) as usize;

        if sample_count == 0 {
            return Err(Error::InvalidFrame(
                "Calculated audio sample count is zero".into(),
            ));
        }

        // For zero-copy: just borrow the data slice from the SDK
        let (data, original_p_data) = if recv_instance.is_some() {
            // We're receiving - don't copy, just borrow
            let slice = unsafe { slice::from_raw_parts(raw.p_data as *const f32, sample_count) };
            (Cow::Borrowed(slice), Some(raw.p_data))
        } else {
            // Not from receive - make a copy for ownership
            let slice = unsafe { slice::from_raw_parts(raw.p_data as *const f32, sample_count) };
            (Cow::Owned(slice.to_vec()), None)
        };

        let metadata = if raw.p_metadata.is_null() {
            None
        } else {
            // Copy the string, don't take ownership - SDK will free the original
            Some(unsafe { CString::from(CStr::from_ptr(raw.p_metadata)) })
        };

        Ok(AudioFrame {
            sample_rate: raw.sample_rate,
            num_channels: raw.no_channels,
            num_samples: raw.no_samples,
            timecode: raw.timecode,
            fourcc: match raw.FourCC {
                NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP => AudioType::FLTP,
                _ => AudioType::Max,
            },
            data,
            channel_stride_in_bytes: unsafe { raw.__bindgen_anon_1.channel_stride_in_bytes },
            metadata,
            timestamp: raw.timestamp,
            recv_instance,
            original_p_data,
            _origin: PhantomData,
        })
    }

    /// Create a builder for configuring an audio frame
    pub fn builder() -> AudioFrameBuilder<'rx> {
        AudioFrameBuilder::new()
    }

    /// Get audio data as 32-bit floats
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// Get audio data for a specific channel (if planar format)
    pub fn channel_data(&self, channel: usize) -> Option<Vec<f32>> {
        if channel >= self.num_channels as usize {
            return None;
        }

        let samples_per_channel = self.num_samples as usize;

        if self.channel_stride_in_bytes == 0 {
            // Interleaved format: extract samples for the requested channel
            let channels = self.num_channels as usize;
            let channel_data: Vec<f32> = self
                .data
                .iter()
                .skip(channel)
                .step_by(channels)
                .copied()
                .collect();
            Some(channel_data)
        } else {
            // Planar format: channel data is contiguous
            let stride_in_samples = self.channel_stride_in_bytes as usize / 4; // f32 = 4 bytes
            let start = channel * stride_in_samples;
            let end = start + samples_per_channel;

            if end <= self.data.len() {
                Some(self.data[start..end].to_vec())
            } else {
                None
            }
        }
    }
}

/// Builder for configuring an AudioFrame with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct AudioFrameBuilder<'rx> {
    sample_rate: Option<i32>,
    num_channels: Option<i32>,
    num_samples: Option<i32>,
    timecode: Option<i64>,
    fourcc: Option<AudioType>,
    data: Option<Vec<f32>>,
    metadata: Option<String>,
    timestamp: Option<i64>,
    _phantom: PhantomData<&'rx ()>,
}

impl<'rx> AudioFrameBuilder<'rx> {
    /// Create a new builder with no fields set
    pub fn new() -> Self {
        Self {
            sample_rate: None,
            num_channels: None,
            num_samples: None,
            timecode: None,
            fourcc: None,
            data: None,
            metadata: None,
            timestamp: None,
            _phantom: PhantomData,
        }
    }

    /// Set the sample rate
    #[must_use]
    pub fn sample_rate(mut self, rate: i32) -> Self {
        self.sample_rate = Some(rate);
        self
    }

    /// Set the number of audio channels
    #[must_use]
    pub fn channels(mut self, channels: i32) -> Self {
        self.num_channels = Some(channels);
        self
    }

    /// Set the number of samples
    #[must_use]
    pub fn samples(mut self, samples: i32) -> Self {
        self.num_samples = Some(samples);
        self
    }

    /// Set the timecode
    #[must_use]
    pub fn timecode(mut self, tc: i64) -> Self {
        self.timecode = Some(tc);
        self
    }

    /// Set the audio format
    #[must_use]
    pub fn format(mut self, format: AudioType) -> Self {
        self.fourcc = Some(format);
        self
    }

    /// Set the audio data as 32-bit floats
    #[must_use]
    pub fn data(mut self, data: Vec<f32>) -> Self {
        self.data = Some(data);
        self
    }

    /// Set metadata
    #[must_use]
    pub fn metadata<S: Into<String>>(mut self, meta: S) -> Self {
        self.metadata = Some(meta.into());
        self
    }

    /// Set the timestamp
    #[must_use]
    pub fn timestamp(mut self, ts: i64) -> Self {
        self.timestamp = Some(ts);
        self
    }

    /// Build the AudioFrame
    pub fn build(self) -> Result<AudioFrame<'rx>> {
        let sample_rate = self.sample_rate.unwrap_or(48000);
        let num_channels = self.num_channels.unwrap_or(2);
        let num_samples = self.num_samples.unwrap_or(1024);
        let fourcc = self.fourcc.unwrap_or(AudioType::FLTP);

        let data = if let Some(data) = self.data {
            data
        } else {
            // Calculate default buffer size for f32 samples
            let sample_count = (num_samples * num_channels) as usize;
            vec![0.0f32; sample_count]
        };

        let metadata_cstring = self
            .metadata
            .map(|m| CString::new(m).map_err(Error::InvalidCString))
            .transpose()?;

        Ok(AudioFrame {
            sample_rate,
            num_channels,
            num_samples,
            timecode: self.timecode.unwrap_or(0),
            fourcc,
            data: Cow::Owned(data),
            channel_stride_in_bytes: 0, // 0 indicates interleaved format
            metadata: metadata_cstring,
            timestamp: self.timestamp.unwrap_or(0),
            recv_instance: None,
            original_p_data: None,
            _origin: PhantomData,
        })
    }
}

impl Default for AudioFrameBuilder<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for AudioFrame<'_> {
    fn default() -> Self {
        AudioFrame::builder()
            .build()
            .expect("Default AudioFrame should always succeed")
    }
}

impl Drop for AudioFrame<'_> {
    fn drop(&mut self) {
        // If this frame originated from a Recv instance and we have the original SDK pointer, free it
        if let (Some(recv_instance), Some(original_p_data)) =
            (self.recv_instance, self.original_p_data)
        {
            // Create a raw frame with the original SDK pointer for NDI to free
            let raw_frame = NDIlib_audio_frame_v3_t {
                sample_rate: self.sample_rate,
                no_channels: self.num_channels,
                no_samples: self.num_samples,
                timecode: self.timecode,
                FourCC: self.fourcc.into(),
                p_data: original_p_data,
                __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                    channel_stride_in_bytes: self.channel_stride_in_bytes,
                },
                p_metadata: self.metadata.as_ref().map_or(ptr::null(), |m| m.as_ptr()),
                timestamp: self.timestamp,
            };
            unsafe {
                NDIlib_recv_free_audio_v3(recv_instance, &raw_frame);
            }
        }
    }
}

#[derive(Debug, TryFromPrimitive, IntoPrimitive, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AudioType {
    FLTP = NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP as _,
    Max = NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_max as _,
}

impl From<AudioType> for i32 {
    fn from(value: AudioType) -> Self {
        let u32_value: u32 = value.into();
        u32_value as i32
    }
}

/// Calculate the line stride (bytes per row) for a given video format
pub(crate) fn calculate_line_stride(fourcc: FourCCVideoType, width: i32) -> i32 {
    match fourcc {
        FourCCVideoType::BGRA
        | FourCCVideoType::BGRX
        | FourCCVideoType::RGBA
        | FourCCVideoType::RGBX => width * 4, // 32 bpp = 4 bytes per pixel
        FourCCVideoType::UYVY => width * 2, // 16 bpp = 2 bytes per pixel
        FourCCVideoType::YV12 | FourCCVideoType::I420 | FourCCVideoType::NV12 => width, // Y plane stride for planar formats
        FourCCVideoType::UYVA => width * 3, // 24 bpp = 3 bytes per pixel
        FourCCVideoType::P216 | FourCCVideoType::PA16 => width * 4, // 32 bpp = 4 bytes per pixel
        _ => width * 4,                     // Default to 32 bpp
    }
}

/// Calculate the total buffer size needed for a video frame
fn calculate_buffer_size(fourcc: FourCCVideoType, width: i32, height: i32) -> usize {
    match fourcc {
        FourCCVideoType::BGRA
        | FourCCVideoType::BGRX
        | FourCCVideoType::RGBA
        | FourCCVideoType::RGBX => (height * width * 4) as usize, // 32 bpp
        FourCCVideoType::UYVY => (height * width * 2) as usize, // 16 bpp
        FourCCVideoType::YV12 | FourCCVideoType::I420 | FourCCVideoType::NV12 => {
            // Planar 4:2:0 formats: Y plane is full res, U/V planes are quarter size
            let y_size = (width * height) as usize;
            let uv_size = ((width / 2) * (height / 2)) as usize;
            y_size + 2 * uv_size
        }
        FourCCVideoType::UYVA => (height * width * 3) as usize, // 24 bpp
        FourCCVideoType::P216 | FourCCVideoType::PA16 => (height * width * 4) as usize, // 32 bpp
        _ => (height * width * 4) as usize,                     // Default to 32 bpp
    }
}

/// Check if a video format is uncompressed
fn is_uncompressed_format(fourcc: FourCCVideoType) -> bool {
    matches!(
        fourcc,
        FourCCVideoType::BGRA
            | FourCCVideoType::BGRX
            | FourCCVideoType::RGBA
            | FourCCVideoType::RGBX
            | FourCCVideoType::UYVY
            | FourCCVideoType::UYVA
            | FourCCVideoType::YV12
            | FourCCVideoType::I420
            | FourCCVideoType::NV12
            | FourCCVideoType::P216
            | FourCCVideoType::PA16
    )
}

#[derive(Debug, Clone)]
pub struct MetadataFrame {
    pub data: String, // Owned metadata (typically XML)
    pub timecode: i64,
}

impl MetadataFrame {
    pub fn new() -> Self {
        MetadataFrame {
            data: String::new(),
            timecode: 0,
        }
    }

    pub fn with_data(data: String, timecode: i64) -> Self {
        MetadataFrame { data, timecode }
    }

    /// Convert to raw format for sending
    pub(crate) fn to_raw(&self) -> Result<(CString, NDIlib_metadata_frame_t)> {
        let c_data = CString::new(self.data.clone()).map_err(Error::InvalidCString)?;
        let raw = NDIlib_metadata_frame_t {
            length: c_data.as_bytes().len() as i32,
            timecode: self.timecode,
            p_data: c_data.as_ptr() as *mut c_char,
        };
        Ok((c_data, raw))
    }

    /// Create from raw NDI metadata frame (copies the data)
    pub(crate) fn from_raw(raw: &NDIlib_metadata_frame_t) -> Self {
        let data = if raw.p_data.is_null() {
            String::new()
        } else {
            unsafe {
                let c_str = CStr::from_ptr(raw.p_data);
                c_str.to_string_lossy().into_owned()
            }
        };
        MetadataFrame {
            data,
            timecode: raw.timecode,
        }
    }
}

impl Default for MetadataFrame {
    fn default() -> Self {
        Self::new()
    }
}
