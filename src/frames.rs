//! Frame types for video, audio, and metadata.

use num_enum::{IntoPrimitive, TryFromPrimitive};

use std::{
    ffi::{CStr, CString},
    fmt,
    os::raw::c_char,
    ptr, slice,
};

use crate::{
    ndi_lib::*,
    recv_guard::{RecvAudioGuard, RecvMetadataGuard, RecvVideoGuard},
    Error, Result,
};

/// Video pixel format identifiers (FourCC codes).
///
/// These represent the various pixel formats supported by NDI for video frames.
/// The most common formats are BGRA/RGBA for full quality and UYVY for bandwidth-efficient streaming.
///
/// This enum is marked `#[non_exhaustive]` to allow future NDI SDK versions to add new formats
/// without breaking existing code. Always use a wildcard pattern when matching.
///
/// # Examples
///
/// ```
/// use grafton_ndi::PixelFormat;
///
/// // For maximum compatibility and quality
/// let format = PixelFormat::BGRA;
///
/// // For bandwidth-efficient streaming
/// let format = PixelFormat::UYVY;
///
/// // When matching, always include a wildcard for forward compatibility
/// match format {
///     PixelFormat::BGRA | PixelFormat::RGBA => println!("Full quality RGB"),
///     PixelFormat::UYVY => println!("Compressed YUV"),
///     _ => println!("Other format"),
/// }
/// ```
#[derive(Debug, TryFromPrimitive, IntoPrimitive, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u32)]
pub enum PixelFormat {
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
}

impl From<PixelFormat> for i32 {
    fn from(value: PixelFormat) -> Self {
        let u32_value: u32 = value.into();
        u32_value as i32
    }
}

/// Video scan type (progressive, interlaced, or field-based).
///
/// This enum describes how video frames are scanned/displayed.
/// Most modern content uses Progressive, while legacy broadcast may use Interlaced or field-based formats.
///
/// This enum is marked `#[non_exhaustive]` to allow future NDI SDK versions to add new scan types
/// without breaking existing code. Always use a wildcard pattern when matching.
///
/// # Examples
///
/// ```
/// use grafton_ndi::ScanType;
///
/// let scan = ScanType::Progressive;
///
/// // When matching, always include a wildcard for forward compatibility
/// match scan {
///     ScanType::Progressive => println!("Progressive scan"),
///     ScanType::Interlaced => println!("Interlaced"),
///     _ => println!("Field-based or other"),
/// }
/// ```
#[derive(Debug, TryFromPrimitive, IntoPrimitive, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u32)]
pub enum ScanType {
    /// Progressive scan - full frames rendered sequentially.
    Progressive = NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive as _,
    /// Interlaced scan - alternating even/odd lines.
    Interlaced = NDIlib_frame_format_type_e_NDIlib_frame_format_type_interleaved as _,
    /// Field 0 only (first field of interlaced content).
    Field0 = NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_0 as _,
    /// Field 1 only (second field of interlaced content).
    Field1 = NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_1 as _,
}

impl From<ScanType> for i32 {
    fn from(value: ScanType) -> Self {
        let u32_value: u32 = value.into();
        u32_value as i32
    }
}

/// Line stride or data size for video frames.
///
/// This enum represents the choice between line stride (for uncompressed formats)
/// and total data size (for compressed or opaque formats). The discriminant is
/// determined by the video format (FourCC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStrideOrSize {
    /// Line stride in bytes for uncompressed formats.
    /// This is the number of bytes per row of video data.
    LineStrideBytes(i32),
    /// Total data size in bytes for compressed or opaque formats.
    DataSizeBytes(i32),
}

impl From<LineStrideOrSize> for NDIlib_video_frame_v2_t__bindgen_ty_1 {
    fn from(value: LineStrideOrSize) -> Self {
        // Writing to a union field is safe when the field type implements Copy.
        // We write exactly one field of the union based on the enum variant.
        match value {
            LineStrideOrSize::LineStrideBytes(stride) =>
            {
                #[allow(clippy::field_reassign_with_default)]
                NDIlib_video_frame_v2_t__bindgen_ty_1 {
                    line_stride_in_bytes: stride,
                }
            }
            LineStrideOrSize::DataSizeBytes(size) => NDIlib_video_frame_v2_t__bindgen_ty_1 {
                data_size_in_bytes: size,
            },
        }
    }
}

pub struct VideoFrame {
    pub width: i32,
    pub height: i32,
    pub pixel_format: PixelFormat,
    pub frame_rate_n: i32,
    pub frame_rate_d: i32,
    pub picture_aspect_ratio: f32,
    pub scan_type: ScanType,
    pub timecode: i64,
    pub data: Vec<u8>,
    pub line_stride_or_size: LineStrideOrSize,
    pub metadata: Option<CString>,
    pub timestamp: i64,
}

impl fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixel_format", &self.pixel_format)
            .field("frame_rate_n", &self.frame_rate_n)
            .field("frame_rate_d", &self.frame_rate_d)
            .field("picture_aspect_ratio", &self.picture_aspect_ratio)
            .field("scan_type", &self.scan_type)
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
        VideoFrame::builder()
            .resolution(1920, 1080)
            .pixel_format(PixelFormat::BGRA)
            .frame_rate(60, 1)
            .aspect_ratio(16.0 / 9.0)
            .scan_type(ScanType::Interlaced)
            .build()
            .expect("Default VideoFrame should always succeed")
    }
}

impl VideoFrame {
    pub fn to_raw(&self) -> NDIlib_video_frame_v2_t {
        NDIlib_video_frame_v2_t {
            xres: self.width,
            yres: self.height,
            FourCC: self.pixel_format.into(),
            frame_rate_N: self.frame_rate_n,
            frame_rate_D: self.frame_rate_d,
            picture_aspect_ratio: self.picture_aspect_ratio,
            frame_format_type: self.scan_type.into(),
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

    /// Encode the video frame as PNG bytes.
    ///
    /// This method encodes the frame to PNG format, automatically handling color format
    /// conversion from the NDI frame format (BGRA/RGBA/etc.) to PNG-compatible RGBA.
    ///
    /// # Supported Formats
    ///
    /// - `RGBA` / `RGBX`: Direct encoding (fastest)
    /// - `BGRA` / `BGRX`: Swaps red and blue channels
    /// - Other formats: Returns an error (unsupported for now)
    ///
    /// # Stride Handling
    ///
    /// This method validates that the frame's line stride matches the expected stride for
    /// the pixel format. If the stride doesn't match (indicating row padding), an error
    /// is returned. This prevents corrupted image output.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The frame format is not RGBA/RGBX/BGRA/BGRX
    /// - The line stride doesn't match the expected value (has padding)
    /// - PNG encoding fails
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, Finder, FinderOptions, ReceiverOptions, Receiver, ReceiverColorFormat};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_millis(1000))?;
    /// # let sources = finder.sources(Duration::ZERO)?;
    /// # let options = ReceiverOptions::builder(sources[0].clone())
    /// #     .color(ReceiverColorFormat::RGBX_RGBA)
    /// #     .build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let video_frame = receiver.capture_video(Duration::from_secs(5))?;
    /// let png_bytes = video_frame.encode_png()?;
    /// std::fs::write("frame.png", &png_bytes)?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "image-encoding")]
    pub fn encode_png(&self) -> Result<Vec<u8>> {
        use png::{BitDepth, ColorType, Encoder};

        // Validate format
        let bytes_per_pixel = match self.pixel_format {
            PixelFormat::RGBA | PixelFormat::RGBX => 4,
            PixelFormat::BGRA | PixelFormat::BGRX => 4,
            _ => {
                let pixel_format = self.pixel_format;
                return Err(Error::InvalidFrame(format!(
                    "Unsupported format for PNG encoding: {pixel_format:?}. Only RGBA/RGBX/BGRA/BGRX are supported."
                )));
            }
        };

        // Validate stride
        let expected_stride = self.width * bytes_per_pixel;
        let actual_stride = match self.line_stride_or_size {
            LineStrideOrSize::LineStrideBytes(stride) => stride,
            LineStrideOrSize::DataSizeBytes(_) => {
                return Err(Error::InvalidFrame(
                    "Cannot encode image from compressed/data-size format. Use LineStrideBytes."
                        .into(),
                ));
            }
        };

        if actual_stride != expected_stride {
            return Err(Error::InvalidFrame(format!(
                "Line stride ({actual_stride}) doesn't match width * {bytes_per_pixel} ({expected_stride}). \
                 Row padding is not supported for image encoding."
            )));
        }

        // Handle color format conversion if needed
        let rgba_data: Vec<u8> = match self.pixel_format {
            PixelFormat::RGBA | PixelFormat::RGBX => {
                // Already in correct format, use as-is
                self.data.to_vec()
            }
            PixelFormat::BGRA | PixelFormat::BGRX => {
                // Swap R and B channels (BGRA -> RGBA)
                let mut rgba = self.data.to_vec();
                for chunk in rgba.chunks_exact_mut(4) {
                    chunk.swap(0, 2); // Swap B and R
                }
                rgba
            }
            _ => unreachable!("Format already validated above"),
        };

        // Encode to PNG
        let mut png_data = Vec::new();
        let mut encoder = Encoder::new(&mut png_data, self.width as u32, self.height as u32);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);

        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&rgba_data))
            .map_err(|e| Error::InvalidFrame(format!("PNG encoding failed: {e}")))?;

        Ok(png_data)
    }

    /// Encode the video frame as JPEG bytes with the specified quality.
    ///
    /// This method encodes the frame to JPEG format, automatically handling color format
    /// conversion from the NDI frame format to JPEG-compatible RGB.
    ///
    /// # Arguments
    ///
    /// * `quality` - JPEG quality from 1 (lowest) to 100 (highest). Typical values are 80-95.
    ///
    /// # Supported Formats
    ///
    /// - `RGBA` / `RGBX`: Strips alpha channel
    /// - `BGRA` / `BGRX`: Swaps red/blue and strips alpha
    /// - Other formats: Returns an error (unsupported for now)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The frame format is not RGBA/RGBX/BGRA/BGRX
    /// - The line stride doesn't match the expected value (has padding)
    /// - JPEG encoding fails
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, Finder, FinderOptions, ReceiverOptions, Receiver, ReceiverColorFormat};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_millis(1000))?;
    /// # let sources = finder.sources(Duration::ZERO)?;
    /// # let options = ReceiverOptions::builder(sources[0].clone())
    /// #     .color(ReceiverColorFormat::RGBX_RGBA)
    /// #     .build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let video_frame = receiver.capture_video(Duration::from_secs(5))?;
    /// let jpeg_bytes = video_frame.encode_jpeg(85)?;
    /// std::fs::write("frame.jpg", &jpeg_bytes)?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "image-encoding")]
    pub fn encode_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder};

        // Validate format
        let bytes_per_pixel = match self.pixel_format {
            PixelFormat::RGBA | PixelFormat::RGBX => 4,
            PixelFormat::BGRA | PixelFormat::BGRX => 4,
            _ => {
                let pixel_format = self.pixel_format;
                return Err(Error::InvalidFrame(format!(
                    "Unsupported format for JPEG encoding: {pixel_format:?}. Only RGBA/RGBX/BGRA/BGRX are supported."
                )));
            }
        };

        // Validate stride
        let expected_stride = self.width * bytes_per_pixel;
        let actual_stride = match self.line_stride_or_size {
            LineStrideOrSize::LineStrideBytes(stride) => stride,
            LineStrideOrSize::DataSizeBytes(_) => {
                return Err(Error::InvalidFrame(
                    "Cannot encode image from compressed/data-size format. Use LineStrideBytes."
                        .into(),
                ));
            }
        };

        if actual_stride != expected_stride {
            return Err(Error::InvalidFrame(format!(
                "Line stride ({actual_stride}) doesn't match width * {bytes_per_pixel} ({expected_stride}). \
                 Row padding is not supported for image encoding."
            )));
        }

        // Convert to RGB (JPEG doesn't support alpha channel)
        let rgb_data: Vec<u8> = match self.pixel_format {
            PixelFormat::RGBA | PixelFormat::RGBX => {
                // Strip alpha channel: RGBA -> RGB
                self.data
                    .chunks_exact(4)
                    .flat_map(|chunk| [chunk[0], chunk[1], chunk[2]])
                    .collect()
            }
            PixelFormat::BGRA | PixelFormat::BGRX => {
                // Swap R/B and strip alpha: BGRA -> RGB
                self.data
                    .chunks_exact(4)
                    .flat_map(|chunk| [chunk[2], chunk[1], chunk[0]])
                    .collect()
            }
            _ => unreachable!("Format already validated above"),
        };

        // Encode to JPEG
        let mut jpeg_data = Vec::new();
        let encoder = JpegEncoder::new(&mut jpeg_data, quality);
        encoder
            .encode(
                &rgb_data,
                self.width as u16,
                self.height as u16,
                JpegColorType::Rgb,
            )
            .map_err(|e| Error::InvalidFrame(format!("JPEG encoding failed: {e}")))?;

        Ok(jpeg_data)
    }

    /// Encode the video frame as a base64 data URL for embedding in HTML/JSON.
    ///
    /// This produces a string in the format: `data:image/png;base64,...` or
    /// `data:image/jpeg;base64,...` that can be directly used in HTML `<img>` tags
    /// or stored in JSON.
    ///
    /// # Arguments
    ///
    /// * `format` - The image format to use (PNG or JPEG with quality)
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, Finder, FinderOptions, ReceiverOptions, Receiver, ReceiverColorFormat, ImageFormat};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_millis(1000))?;
    /// # let sources = finder.sources(Duration::ZERO)?;
    /// # let options = ReceiverOptions::builder(sources[0].clone())
    /// #     .color(ReceiverColorFormat::RGBX_RGBA)
    /// #     .build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let video_frame = receiver.capture_video(Duration::from_secs(5))?;
    ///
    /// // As PNG
    /// let data_url = video_frame.encode_data_url(ImageFormat::Png)?;
    /// println!("<img src=\"{}\">", data_url);
    ///
    /// // As JPEG with quality 90
    /// let data_url = video_frame.encode_data_url(ImageFormat::Jpeg(90))?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "image-encoding")]
    pub fn encode_data_url(&self, format: ImageFormat) -> Result<String> {
        use base64::{engine::general_purpose::STANDARD, Engine};

        let (mime_type, image_bytes) = match format {
            ImageFormat::Png => ("image/png", self.encode_png()?),
            ImageFormat::Jpeg(quality) => ("image/jpeg", self.encode_jpeg(quality)?),
        };

        let base64_data = STANDARD.encode(&image_bytes);
        Ok(format!("data:{mime_type};base64,{base64_data}"))
    }

    /// Creates a `VideoFrame` from a raw NDI video frame with owned data.
    ///
    /// # Safety
    ///
    /// This function assumes the given `NDIlib_video_frame_v2_t` is valid and correctly allocated.
    /// This method copies the data, so the VideoFrame owns its data and can outlive the source.
    pub unsafe fn from_raw(c_frame: &NDIlib_video_frame_v2_t) -> Result<VideoFrame> {
        if c_frame.p_data.is_null() {
            return Err(Error::InvalidFrame(
                "Video frame has null data pointer".into(),
            ));
        }

        #[allow(clippy::unnecessary_cast)] // Required for Windows where FourCC is i32
        let pixel_format = PixelFormat::try_from(c_frame.FourCC as u32).map_err(|_| {
            Error::InvalidFrame(format!(
                "Unknown pixel format FourCC: 0x{:08X}",
                c_frame.FourCC
            ))
        })?;

        // Determine data size and LineStrideOrSize based on format.
        // The NDI SDK uses a union here: line_stride_in_bytes for uncompressed formats,
        // data_size_in_bytes for compressed formats.
        // We read ONLY the appropriate field based on the format to avoid UB.
        let is_uncompressed = is_uncompressed_format(pixel_format);

        let (data_size, line_stride_or_size) = if is_uncompressed {
            // Uncompressed format: read ONLY line_stride_in_bytes
            let line_stride = c_frame.__bindgen_anon_1.line_stride_in_bytes;

            if line_stride > 0 && c_frame.yres > 0 && c_frame.xres > 0 {
                // Use the new helper that correctly handles planar 4:2:0 formats
                let calculated_size =
                    uncompressed_buffer_len(pixel_format, line_stride, c_frame.xres, c_frame.yres);
                if calculated_size > 0 && calculated_size <= MAX_VIDEO_BYTES {
                    (
                        calculated_size,
                        LineStrideOrSize::LineStrideBytes(line_stride),
                    )
                } else {
                    return Err(Error::InvalidFrame(format!(
                        "Uncompressed video frame exceeds maximum size: {} bytes > {} bytes",
                        calculated_size, MAX_VIDEO_BYTES
                    )));
                }
            } else {
                return Err(Error::InvalidFrame(
                    "Uncompressed video frame has invalid line_stride_in_bytes".into(),
                ));
            }
        } else {
            // Compressed/unknown format: read ONLY data_size_in_bytes
            let data_size_in_bytes = c_frame.__bindgen_anon_1.data_size_in_bytes;

            if data_size_in_bytes <= 0 {
                return Err(Error::InvalidFrame(
                    "Compressed video frame has invalid data_size_in_bytes".into(),
                ));
            }

            let data_size_usize = usize::try_from(data_size_in_bytes).map_err(|_| {
                Error::InvalidFrame(format!(
                    "Invalid data_size_in_bytes value: {}",
                    data_size_in_bytes
                ))
            })?;

            if data_size_usize > MAX_VIDEO_BYTES {
                return Err(Error::InvalidFrame(format!(
                    "Compressed video frame exceeds maximum size: {} bytes > {} bytes",
                    data_size_usize, MAX_VIDEO_BYTES
                )));
            }

            (
                data_size_usize,
                LineStrideOrSize::DataSizeBytes(data_size_in_bytes),
            )
        };

        if data_size == 0 {
            return Err(Error::InvalidFrame("Video frame has zero size".into()));
        }

        // Always copy data for ownership - we're no longer zero-copy
        let slice = slice::from_raw_parts(c_frame.p_data, data_size);
        let data = slice.to_vec();

        let metadata = if c_frame.p_metadata.is_null() {
            None
        } else {
            Some(CString::from(CStr::from_ptr(c_frame.p_metadata)))
        };

        #[allow(clippy::unnecessary_cast)] // Required for Windows where frame_format_type is i32
        let scan_type = ScanType::try_from(c_frame.frame_format_type as u32).map_err(|_| {
            Error::InvalidFrame(format!(
                "Unknown scan type: 0x{:08X}",
                c_frame.frame_format_type
            ))
        })?;

        Ok(VideoFrame {
            width: c_frame.xres,
            height: c_frame.yres,
            pixel_format,
            frame_rate_n: c_frame.frame_rate_N,
            frame_rate_d: c_frame.frame_rate_D,
            picture_aspect_ratio: c_frame.picture_aspect_ratio,
            scan_type,
            timecode: c_frame.timecode,
            data,
            line_stride_or_size,
            metadata,
            timestamp: c_frame.timestamp,
        })
    }

    /// Create a builder for configuring a video frame
    pub fn builder() -> VideoFrameBuilder {
        VideoFrameBuilder::new()
    }
}

/// Builder for configuring a VideoFrame with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct VideoFrameBuilder {
    width: Option<i32>,
    height: Option<i32>,
    pixel_format: Option<PixelFormat>,
    frame_rate_n: Option<i32>,
    frame_rate_d: Option<i32>,
    picture_aspect_ratio: Option<f32>,
    scan_type: Option<ScanType>,
    timecode: Option<i64>,
    metadata: Option<String>,
    timestamp: Option<i64>,
}

impl VideoFrameBuilder {
    /// Create a new builder with no fields set
    pub fn new() -> Self {
        Self {
            width: None,
            height: None,
            pixel_format: None,
            frame_rate_n: None,
            frame_rate_d: None,
            picture_aspect_ratio: None,
            scan_type: None,
            timecode: None,
            metadata: None,
            timestamp: None,
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
    pub fn pixel_format(mut self, pixel_format: PixelFormat) -> Self {
        self.pixel_format = Some(pixel_format);
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

    /// Set the scan type (progressive, interlaced, etc.)
    #[must_use]
    pub fn scan_type(mut self, scan_type: ScanType) -> Self {
        self.scan_type = Some(scan_type);
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
    pub fn build(self) -> Result<VideoFrame> {
        let width = self.width.unwrap_or(1920);
        let height = self.height.unwrap_or(1080);
        let pixel_format = self.pixel_format.unwrap_or(PixelFormat::BGRA);
        let frame_rate_n = self.frame_rate_n.unwrap_or(60);
        let frame_rate_d = self.frame_rate_d.unwrap_or(1);
        let picture_aspect_ratio = self.picture_aspect_ratio.unwrap_or(16.0 / 9.0);
        let scan_type = self.scan_type.unwrap_or(ScanType::Progressive);

        // Calculate stride and buffer size
        let stride = calculate_line_stride(pixel_format, width);
        let buffer_size = calculate_buffer_size(pixel_format, width, height);
        let data = vec![0u8; buffer_size];

        let mut frame = VideoFrame {
            width,
            height,
            pixel_format,
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio,
            scan_type,
            timecode: self.timecode.unwrap_or(0),
            data: (data),
            line_stride_or_size: LineStrideOrSize::LineStrideBytes(stride),
            metadata: None,
            timestamp: self.timestamp.unwrap_or(0),
        };

        if let Some(meta) = self.metadata {
            frame.metadata = Some(CString::new(meta).map_err(Error::InvalidCString)?);
        }

        Ok(frame)
    }
}

impl Default for VideoFrameBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VideoFrame {
    fn drop(&mut self) {
        // With owned data, we don't need to free SDK pointers anymore
        // The Vec<u8> handles memory cleanup automatically
    }
}

#[derive(Debug)]
pub struct AudioFrame {
    pub sample_rate: i32,
    pub num_channels: i32,
    pub num_samples: i32,
    pub timecode: i64,
    pub format: AudioFormat,
    data: Vec<f32>,
    pub channel_stride_in_bytes: i32,
    pub metadata: Option<CString>,
    pub timestamp: i64,
}

impl AudioFrame {
    pub(crate) fn to_raw(&self) -> NDIlib_audio_frame_v3_t {
        NDIlib_audio_frame_v3_t {
            sample_rate: self.sample_rate,
            no_channels: self.num_channels,
            no_samples: self.num_samples,
            timecode: self.timecode,
            FourCC: self.format.into(),
            p_data: self.data.as_ptr() as *mut f32 as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: self.channel_stride_in_bytes,
            },
            p_metadata: self.metadata.as_ref().map_or(ptr::null(), |m| m.as_ptr()),
            timestamp: self.timestamp,
        }
    }

    pub(crate) fn from_raw(raw: NDIlib_audio_frame_v3_t) -> Result<AudioFrame> {
        if raw.p_data.is_null() {
            return Err(Error::InvalidFrame(
                "Audio frame has null data pointer".into(),
            ));
        }

        if raw.sample_rate <= 0 {
            let sample_rate = raw.sample_rate;
            return Err(Error::InvalidFrame(format!(
                "Invalid sample rate: {sample_rate}"
            )));
        }

        if raw.no_channels <= 0 {
            let no_channels = raw.no_channels;
            return Err(Error::InvalidFrame(format!(
                "Invalid number of channels: {no_channels}"
            )));
        }

        if raw.no_samples <= 0 {
            let no_samples = raw.no_samples;
            return Err(Error::InvalidFrame(format!(
                "Invalid number of samples: {no_samples}"
            )));
        }

        // Use checked math to prevent overflow when computing sample count
        let no_samples = usize::try_from(raw.no_samples).map_err(|_| {
            Error::InvalidFrame(format!("Invalid no_samples value: {}", raw.no_samples))
        })?;

        let no_channels = usize::try_from(raw.no_channels).map_err(|_| {
            Error::InvalidFrame(format!("Invalid no_channels value: {}", raw.no_channels))
        })?;

        let sample_count = no_samples.checked_mul(no_channels).ok_or_else(|| {
            Error::InvalidFrame(format!(
                "Audio sample count overflow: {} samples × {} channels",
                no_samples, no_channels
            ))
        })?;

        // Check total byte size against limit
        let byte_size = sample_count
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| {
                Error::InvalidFrame(format!(
                    "Audio byte size overflow: {} samples × {} bytes",
                    sample_count,
                    std::mem::size_of::<f32>()
                ))
            })?;

        if byte_size > MAX_AUDIO_BYTES {
            return Err(Error::InvalidFrame(format!(
                "Audio frame exceeds maximum size: {} bytes > {} bytes",
                byte_size, MAX_AUDIO_BYTES
            )));
        }

        // Always copy data for ownership - we're no longer zero-copy
        let slice = unsafe { slice::from_raw_parts(raw.p_data as *const f32, sample_count) };
        let data = slice.to_vec();

        let metadata = if raw.p_metadata.is_null() {
            None
        } else {
            // Copy the string, don't take ownership - SDK will free the original
            Some(unsafe { CString::from(CStr::from_ptr(raw.p_metadata)) })
        };

        let format = match raw.FourCC {
            NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP => AudioFormat::FLTP,
            _ => {
                return Err(Error::InvalidFrame(format!(
                    "Unknown audio format FourCC: 0x{:08X}",
                    raw.FourCC
                )))
            }
        };

        Ok(AudioFrame {
            sample_rate: raw.sample_rate,
            num_channels: raw.no_channels,
            num_samples: raw.no_samples,
            timecode: raw.timecode,
            format,
            data,
            channel_stride_in_bytes: unsafe { raw.__bindgen_anon_1.channel_stride_in_bytes },
            metadata,
            timestamp: raw.timestamp,
        })
    }

    /// Create a builder for configuring an audio frame
    pub fn builder() -> AudioFrameBuilder {
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
pub struct AudioFrameBuilder {
    sample_rate: Option<i32>,
    num_channels: Option<i32>,
    num_samples: Option<i32>,
    timecode: Option<i64>,
    format: Option<AudioFormat>,
    data: Option<Vec<f32>>,
    layout: Option<AudioLayout>,
    metadata: Option<String>,
    timestamp: Option<i64>,
}

impl AudioFrameBuilder {
    /// Create a new builder with no fields set
    pub fn new() -> Self {
        Self {
            sample_rate: None,
            num_channels: None,
            num_samples: None,
            timecode: None,
            format: None,
            data: None,
            layout: None,
            metadata: None,
            timestamp: None,
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
    pub fn format(mut self, format: AudioFormat) -> Self {
        self.format = Some(format);
        self
    }

    /// Set the audio data layout (planar or interleaved)
    ///
    /// - **Planar**: All samples for channel 0, then all for channel 1, etc.
    /// - **Interleaved**: Samples from all channels are interleaved.
    ///
    /// Defaults to `AudioLayout::Planar` which is the native format for FLTP.
    ///
    /// # Example
    /// ```
    /// use grafton_ndi::{AudioFrame, AudioLayout};
    ///
    /// // Planar layout (default)
    /// let frame = AudioFrame::builder()
    ///     .channels(2)
    ///     .samples(100)
    ///     .layout(AudioLayout::Planar)
    ///     .build()
    ///     .unwrap();
    /// ```
    #[must_use]
    pub fn layout(mut self, layout: AudioLayout) -> Self {
        self.layout = Some(layout);
        self
    }

    /// Set the audio data as 32-bit floats
    ///
    /// The data layout must match the configured `AudioLayout`:
    /// - **Planar**: `[C0S0, C0S1, ..., C1S0, C1S1, ...]`
    /// - **Interleaved**: `[C0S0, C1S0, C0S1, C1S1, ...]`
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
    ///
    /// Calculates the appropriate `channel_stride_in_bytes` based on the configured layout:
    /// - **Planar** (default): stride = num_samples * 4 (4 bytes per f32 sample)
    /// - **Interleaved**: stride = 0
    pub fn build(self) -> Result<AudioFrame> {
        let sample_rate = self.sample_rate.unwrap_or(48000);
        let num_channels = self.num_channels.unwrap_or(2);
        let num_samples = self.num_samples.unwrap_or(1024);
        let format = self.format.unwrap_or(AudioFormat::FLTP);
        let layout = self.layout.unwrap_or(AudioLayout::Planar);

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

        // Calculate channel_stride_in_bytes based on layout
        // Planar: Each channel has num_samples * sizeof(f32) bytes
        // Interleaved: 0 indicates interleaved format
        let channel_stride_in_bytes = match layout {
            AudioLayout::Planar => num_samples * 4, // 4 bytes per f32
            AudioLayout::Interleaved => 0,
        };

        Ok(AudioFrame {
            sample_rate,
            num_channels,
            num_samples,
            timecode: self.timecode.unwrap_or(0),
            format,
            data,
            channel_stride_in_bytes,
            metadata: metadata_cstring,
            timestamp: self.timestamp.unwrap_or(0),
        })
    }
}

impl Default for AudioFrameBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for AudioFrame {
    fn default() -> Self {
        AudioFrame::builder()
            .build()
            .expect("Default AudioFrame should always succeed")
    }
}

impl Drop for AudioFrame {
    fn drop(&mut self) {
        // With owned data, we don't need to free SDK pointers anymore
        // The Vec<f32> handles memory cleanup automatically
    }
}

/// Audio format identifiers (FourCC codes).
///
/// Currently NDI primarily uses `FLTP` (32-bit floating point planar format).
///
/// This enum is marked `#[non_exhaustive]` to allow future NDI SDK versions to add new audio formats
/// without breaking existing code. Always use a wildcard pattern when matching.
///
/// # Examples
///
/// ```
/// use grafton_ndi::AudioFormat;
///
/// let format = AudioFormat::FLTP;
///
/// // When matching, always include a wildcard for forward compatibility
/// match format {
///     AudioFormat::FLTP => println!("32-bit float planar"),
///     _ => println!("Other format"),
/// }
/// ```
#[derive(Debug, TryFromPrimitive, IntoPrimitive, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u32)]
pub enum AudioFormat {
    /// 32-bit floating point planar audio (FLTP).
    FLTP = NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP as _,
}

/// Audio data layout format
///
/// Determines how multi-channel audio samples are arranged in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioLayout {
    /// Planar format: All samples for channel 0, then all for channel 1, etc.
    ///
    /// Memory layout for 2 channels, 3 samples:
    /// `[C0S0, C0S1, C0S2, C1S0, C1S1, C1S2]`
    ///
    /// This is the native format for FLTP and is efficient for per-channel processing.
    Planar,

    /// Interleaved format: Samples from all channels are interleaved.
    ///
    /// Memory layout for 2 channels, 3 samples:
    /// `[C0S0, C1S0, C0S1, C1S1, C0S2, C1S2]`
    ///
    /// This format alternates between channels for each sample.
    Interleaved,
}

impl From<AudioFormat> for i32 {
    fn from(value: AudioFormat) -> Self {
        let u32_value: u32 = value.into();
        u32_value as i32
    }
}

/// Maximum allowed size for video frame data (100 MiB).
/// Applies to both compressed and uncompressed video frames.
const MAX_VIDEO_BYTES: usize = 100 * 1024 * 1024;

/// Maximum allowed size for audio frame data (64 MiB).
/// Comfortably above typical NDI audio frames while preventing unbounded allocations.
const MAX_AUDIO_BYTES: usize = 64 * 1024 * 1024;

/// Check if a pixel format is planar 4:2:0 (YV12, I420, NV12).
fn is_planar_420(fmt: PixelFormat) -> bool {
    matches!(
        fmt,
        PixelFormat::YV12 | PixelFormat::I420 | PixelFormat::NV12
    )
}

/// Ceiling division by 2 for computing subsampled plane dimensions.
/// For odd values, rounds up (e.g., 1920/2 = 960, 1921/2 = 961).
#[inline]
fn ceil_div2(x: i32) -> i32 {
    (x + 1) / 2
}

/// Calculate the total buffer length for an uncompressed video frame.
///
/// This function computes the correct buffer size based on the pixel format,
/// accounting for planar 4:2:0 formats (YV12/I420/NV12) which require Y + UV planes.
///
/// # Arguments
///
/// * `fmt` - The pixel format
/// * `y_stride` - The Y-plane line stride in bytes (for planar formats) or total line stride (for packed formats)
/// * `width` - Frame width in pixels
/// * `height` - Frame height in pixels
///
/// # Returns
///
/// Total buffer size in bytes needed to hold the frame data.
///
/// # Format-specific calculations
///
/// - **Packed RGB/YUV** (BGRA/BGRX/RGBA/RGBX/UYVY/UYVA/P216/PA16): `y_stride * height`
/// - **Planar 4:2:0 YV12/I420**: `Y + U + V` where:
///   - Y plane: `y_stride * height`
///   - U plane: `(y_stride/2) * ceil(height/2)`
///   - V plane: `(y_stride/2) * ceil(height/2)`
/// - **Semi-planar 4:2:0 NV12**: `Y + UV` where:
///   - Y plane: `y_stride * height`
///   - UV plane: `y_stride * ceil(height/2)`
pub(crate) fn uncompressed_buffer_len(
    fmt: PixelFormat,
    y_stride: i32,
    _width: i32,
    height: i32,
) -> usize {
    if !is_planar_420(fmt) {
        // Packed formats: simple stride * height
        return (y_stride as usize) * (height as usize);
    }

    // Planar 4:2:0 formats need Y + UV planes
    let y_size = (y_stride as usize) * (height as usize);
    let chroma_height = ceil_div2(height) as usize;

    match fmt {
        PixelFormat::YV12 | PixelFormat::I420 => {
            // Planar 4:2:0: Y + U + V
            // U and V planes each have half width and half height (with ceiling for odd dimensions)
            let u_stride = (y_stride / 2) as usize;
            let v_stride = (y_stride / 2) as usize;
            let u_size = u_stride * chroma_height;
            let v_size = v_stride * chroma_height;
            y_size + u_size + v_size
        }
        PixelFormat::NV12 => {
            // Semi-planar 4:2:0: Y + interleaved UV
            // UV plane has full width (contains both U and V interleaved) and half height
            let uv_size = (y_stride as usize) * chroma_height;
            y_size + uv_size
        }
        _ => unreachable!("is_planar_420 check above ensures only YV12/I420/NV12 reach here"),
    }
}

/// Calculate the line stride (bytes per row) for a given video format
pub fn calculate_line_stride(fourcc: PixelFormat, width: i32) -> i32 {
    match fourcc {
        PixelFormat::BGRA | PixelFormat::BGRX | PixelFormat::RGBA | PixelFormat::RGBX => width * 4, // 32 bpp = 4 bytes per pixel
        PixelFormat::UYVY => width * 2, // 16 bpp = 2 bytes per pixel
        PixelFormat::YV12 | PixelFormat::I420 | PixelFormat::NV12 => width, // Y plane stride for planar formats
        PixelFormat::UYVA => width * 3, // 24 bpp = 3 bytes per pixel
        PixelFormat::P216 | PixelFormat::PA16 => width * 4, // 32 bpp = 4 bytes per pixel
    }
}

/// Calculate the total buffer size needed for a video frame.
///
/// This uses the same logic as `uncompressed_buffer_len`, but assumes
/// the stride equals the width (for packed formats) or the Y-plane width
/// (for planar formats), which is appropriate for builder allocation.
fn calculate_buffer_size(fourcc: PixelFormat, width: i32, height: i32) -> usize {
    // For builders, stride is computed from width using calculate_line_stride
    let stride = calculate_line_stride(fourcc, width);
    uncompressed_buffer_len(fourcc, stride, width, height)
}

/// Check if a video format is uncompressed
pub(crate) fn is_uncompressed_format(fourcc: PixelFormat) -> bool {
    matches!(
        fourcc,
        PixelFormat::BGRA
            | PixelFormat::BGRX
            | PixelFormat::RGBA
            | PixelFormat::RGBX
            | PixelFormat::UYVY
            | PixelFormat::UYVA
            | PixelFormat::YV12
            | PixelFormat::I420
            | PixelFormat::NV12
            | PixelFormat::P216
            | PixelFormat::PA16
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

/// Image format specification for encoding video frames.
///
/// Used with [`VideoFrame::encode_data_url`] to specify the desired output format.
///
/// # Examples
///
/// ```
/// use grafton_ndi::ImageFormat;
///
/// // PNG format (lossless)
/// let png = ImageFormat::Png;
///
/// // JPEG with quality 85 (lossy, smaller file size)
/// let jpeg = ImageFormat::Jpeg(85);
/// ```
#[cfg(feature = "image-encoding")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG format (lossless compression)
    Png,
    /// JPEG format with quality setting (1-100, where 100 is highest quality)
    Jpeg(u8),
}

// ============================================================================
// Zero-copy borrowed receive frames
// ============================================================================

/// A zero-copy borrowed video frame.
///
/// This type wraps an RAII guard that owns the NDI frame buffer lifetime,
/// exposing a safe, zero-copy view of the video data. The frame is automatically
/// freed when dropped via `NDIlib_recv_free_video_v2`.
///
/// **Key characteristics:**
/// - Zero allocations: References NDI SDK buffers directly
/// - Zero copies: No memcpy of pixel data
/// - RAII lifetime: Exactly one free per frame, enforced at compile time
/// - Not `Send`: Prevents accidental cross-thread use of FFI buffers
///
/// # Lifetime
///
/// The lifetime parameter `'rx` ties this frame to the `Receiver` that created it.
/// The borrow checker ensures the receiver cannot be dropped while any frame
/// references are alive, preventing use-after-free at compile time with zero runtime cost.
/// The underlying NDI buffer is freed when `VideoFrameRef` is dropped.
///
/// # Performance
///
/// For a 1920×1080 BGRA frame, this eliminates ~8.3 MB of memcpy compared to
/// the owned [`VideoFrame`]. At 60 fps, this saves ~475 MB/s of memory bandwidth.
///
/// # Examples
///
/// ```no_run
/// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, Source, SourceAddress};
/// # use std::time::Duration;
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = NDI::new()?;
/// # let source = Source { name: "Test".into(), address: SourceAddress::None };
/// # let options = ReceiverOptions::builder(source).build();
/// # let receiver = Receiver::new(&ndi, &options)?;
/// // Zero-copy capture (no allocation, no memcpy)
/// if let Some(frame) = receiver.capture_video_ref(Duration::from_millis(1000))? {
///     println!("{}×{} frame, {} bytes", frame.width(), frame.height(), frame.data().len());
///
///     // Process in place - no copy needed
///     let pixels = frame.data();
///
///     // Frame is freed here when `frame` goes out of scope
/// }
/// # Ok(())
/// # }
/// ```
///
/// To convert to an owned frame:
///
/// ```no_run
/// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, Source, SourceAddress};
/// # use std::time::Duration;
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = NDI::new()?;
/// # let source = Source { name: "Test".into(), address: SourceAddress::None };
/// # let options = ReceiverOptions::builder(source).build();
/// # let receiver = Receiver::new(&ndi, &options)?;
/// if let Some(frame_ref) = receiver.capture_video_ref(Duration::from_millis(1000))? {
///     // Convert to owned for storage or cross-thread use
///     let owned = frame_ref.to_owned()?;
///     // owned is now a VideoFrame that can be sent across threads
/// }
/// # Ok(())
/// # }
/// ```
pub struct VideoFrameRef<'rx> {
    guard: RecvVideoGuard<'rx>,
    pixel_format: PixelFormat,
}

impl<'rx> VideoFrameRef<'rx> {
    /// Create a borrowed video frame from an RAII guard.
    ///
    /// Validates the FourCC and returns an error if it's unknown.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI receiver
    /// and contains a frame populated by `NDIlib_recv_capture_v3`.
    pub(crate) unsafe fn new(guard: RecvVideoGuard<'rx>) -> Result<Self> {
        #[allow(clippy::unnecessary_cast)]
        let pixel_format = PixelFormat::try_from(guard.frame().FourCC as u32).map_err(|_| {
            Error::InvalidFrame(format!(
                "Unknown pixel format FourCC: 0x{:08X}",
                guard.frame().FourCC
            ))
        })?;

        Ok(Self {
            guard,
            pixel_format,
        })
    }

    /// Get the frame width in pixels.
    pub fn width(&self) -> i32 {
        self.guard.frame().xres
    }

    /// Get the frame height in pixels.
    pub fn height(&self) -> i32 {
        self.guard.frame().yres
    }

    /// Get the pixel format (FourCC code).
    ///
    /// This is guaranteed to be a valid, supported format since it's validated during construction.
    pub fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    /// Get the frame rate numerator.
    pub fn frame_rate_n(&self) -> i32 {
        self.guard.frame().frame_rate_N
    }

    /// Get the frame rate denominator.
    pub fn frame_rate_d(&self) -> i32 {
        self.guard.frame().frame_rate_D
    }

    /// Get the picture aspect ratio.
    pub fn picture_aspect_ratio(&self) -> f32 {
        self.guard.frame().picture_aspect_ratio
    }

    /// Get the scan type (progressive, interlaced, etc.).
    ///
    /// Returns `ScanType::Progressive` as a fallback if the SDK returns an unknown scan type code.
    pub fn scan_type(&self) -> ScanType {
        #[allow(clippy::unnecessary_cast)]
        ScanType::try_from(self.guard.frame().frame_format_type as u32)
            .unwrap_or(ScanType::Progressive)
    }

    /// Get the timecode.
    pub fn timecode(&self) -> i64 {
        self.guard.frame().timecode
    }

    /// Get the timestamp.
    pub fn timestamp(&self) -> i64 {
        self.guard.frame().timestamp
    }

    /// Get the line stride or data size.
    pub fn line_stride_or_size(&self) -> LineStrideOrSize {
        let is_uncompressed = is_uncompressed_format(self.pixel_format);

        if is_uncompressed {
            let line_stride = unsafe { self.guard.frame().__bindgen_anon_1.line_stride_in_bytes };
            LineStrideOrSize::LineStrideBytes(line_stride)
        } else {
            let data_size = unsafe { self.guard.frame().__bindgen_anon_1.data_size_in_bytes };
            LineStrideOrSize::DataSizeBytes(data_size)
        }
    }

    /// Get the metadata as a `CStr`, if present.
    pub fn metadata(&self) -> Option<&CStr> {
        let p_metadata = self.guard.frame().p_metadata;
        if p_metadata.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(p_metadata) })
        }
    }

    /// Get a zero-copy view of the frame data.
    ///
    /// This returns a slice directly into the NDI SDK's buffer.
    /// No allocation or memcpy is performed.
    ///
    /// For planar 4:2:0 formats (YV12/I420/NV12), this returns the full
    /// buffer including Y and UV planes.
    pub fn data(&self) -> &[u8] {
        let frame = self.guard.frame();

        if frame.p_data.is_null() {
            return &[];
        }

        let is_uncompressed = is_uncompressed_format(self.pixel_format);

        let data_size = if is_uncompressed {
            let line_stride = unsafe { frame.__bindgen_anon_1.line_stride_in_bytes };
            if line_stride > 0 && frame.yres > 0 && frame.xres > 0 {
                // Use the new helper that correctly handles planar 4:2:0 formats
                uncompressed_buffer_len(self.pixel_format, line_stride, frame.xres, frame.yres)
            } else {
                0
            }
        } else {
            let size = unsafe { frame.__bindgen_anon_1.data_size_in_bytes };
            if size > 0 {
                size as usize
            } else {
                0
            }
        };

        if data_size == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(frame.p_data, data_size) }
        }
    }

    /// Convert this borrowed frame to an owned `VideoFrame`.
    ///
    /// This performs a single memcpy of the frame data and metadata,
    /// allowing the frame to outlive the NDI buffer and be sent across threads.
    pub fn to_owned(&self) -> Result<VideoFrame> {
        unsafe { VideoFrame::from_raw(self.guard.frame()) }
    }
}

impl<'rx> fmt::Debug for VideoFrameRef<'rx> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrameRef")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("pixel_format", &self.pixel_format())
            .field("frame_rate_n", &self.frame_rate_n())
            .field("frame_rate_d", &self.frame_rate_d())
            .field("picture_aspect_ratio", &self.picture_aspect_ratio())
            .field("scan_type", &self.scan_type())
            .field("timecode", &self.timecode())
            .field("data (bytes)", &self.data().len())
            .field("line_stride_or_size", &self.line_stride_or_size())
            .field("metadata", &self.metadata())
            .field("timestamp", &self.timestamp())
            .finish()
    }
}

/// A zero-copy borrowed audio frame.
///
/// This type wraps an RAII guard that owns the NDI frame buffer lifetime,
/// exposing a safe, zero-copy view of the audio data. The frame is automatically
/// freed when dropped via `NDIlib_recv_free_audio_v3`.
///
/// **Key characteristics:**
/// - Zero allocations: References NDI SDK buffers directly
/// - Zero copies: No memcpy of audio samples
/// - RAII lifetime: Exactly one free per frame, enforced at compile time
/// - Not `Send`: Prevents accidental cross-thread use of FFI buffers
///
/// # Examples
///
/// ```no_run
/// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, Source, SourceAddress};
/// # use std::time::Duration;
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = NDI::new()?;
/// # let source = Source { name: "Test".into(), address: SourceAddress::None };
/// # let options = ReceiverOptions::builder(source).build();
/// # let receiver = Receiver::new(&ndi, &options)?;
/// // Zero-copy capture
/// if let Some(frame) = receiver.capture_audio_ref(Duration::from_millis(1000))? {
///     println!("{} channels, {} samples", frame.num_channels(), frame.num_samples());
///
///     // Process in place - no copy needed
///     let samples = frame.data();
///
///     // Frame is freed here when `frame` goes out of scope
/// }
/// # Ok(())
/// # }
/// ```
pub struct AudioFrameRef<'rx> {
    guard: RecvAudioGuard<'rx>,
    format: AudioFormat,
}

impl<'rx> AudioFrameRef<'rx> {
    /// Create a borrowed audio frame from an RAII guard.
    ///
    /// Validates the FourCC and returns an error if it's unknown.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI receiver
    /// and contains a frame populated by `NDIlib_recv_capture_v3`.
    pub(crate) unsafe fn new(guard: RecvAudioGuard<'rx>) -> Result<Self> {
        let format = match guard.frame().FourCC {
            NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP => AudioFormat::FLTP,
            _ => {
                return Err(Error::InvalidFrame(format!(
                    "Unknown audio format FourCC: 0x{:08X}",
                    guard.frame().FourCC
                )))
            }
        };

        Ok(Self { guard, format })
    }

    /// Get the sample rate in Hz.
    pub fn sample_rate(&self) -> i32 {
        self.guard.frame().sample_rate
    }

    /// Get the number of audio channels.
    pub fn num_channels(&self) -> i32 {
        self.guard.frame().no_channels
    }

    /// Get the number of samples per channel.
    pub fn num_samples(&self) -> i32 {
        self.guard.frame().no_samples
    }

    /// Get the timecode.
    pub fn timecode(&self) -> i64 {
        self.guard.frame().timecode
    }

    /// Get the timestamp.
    pub fn timestamp(&self) -> i64 {
        self.guard.frame().timestamp
    }

    /// Get the audio format (FourCC code).
    ///
    /// This is guaranteed to be a valid, supported format since it's validated during construction.
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// Get the channel stride in bytes.
    pub fn channel_stride_in_bytes(&self) -> i32 {
        unsafe { self.guard.frame().__bindgen_anon_1.channel_stride_in_bytes }
    }

    /// Get the metadata as a `CStr`, if present.
    pub fn metadata(&self) -> Option<&CStr> {
        let p_metadata = self.guard.frame().p_metadata;
        if p_metadata.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(p_metadata) })
        }
    }

    /// Get a zero-copy view of the audio data as 32-bit floats.
    ///
    /// This returns a slice directly into the NDI SDK's buffer.
    /// No allocation or memcpy is performed.
    pub fn data(&self) -> &[f32] {
        let frame = self.guard.frame();

        if frame.p_data.is_null() {
            return &[];
        }

        let sample_count = (frame.no_samples * frame.no_channels) as usize;
        if sample_count == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(frame.p_data as *const f32, sample_count) }
        }
    }

    /// Convert this borrowed frame to an owned `AudioFrame`.
    ///
    /// This performs a single memcpy of the audio data and metadata,
    /// allowing the frame to outlive the NDI buffer and be sent across threads.
    pub fn to_owned(&self) -> Result<AudioFrame> {
        AudioFrame::from_raw(*self.guard.frame())
    }
}

impl<'rx> fmt::Debug for AudioFrameRef<'rx> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioFrameRef")
            .field("sample_rate", &self.sample_rate())
            .field("num_channels", &self.num_channels())
            .field("num_samples", &self.num_samples())
            .field("timecode", &self.timecode())
            .field("format", &self.format())
            .field("data (samples)", &self.data().len())
            .field("channel_stride_in_bytes", &self.channel_stride_in_bytes())
            .field("metadata", &self.metadata())
            .field("timestamp", &self.timestamp())
            .finish()
    }
}

/// A zero-copy borrowed metadata frame.
///
/// This type wraps an RAII guard that owns the NDI frame buffer lifetime,
/// exposing a safe, zero-copy view of the metadata string. The frame is automatically
/// freed when dropped via `NDIlib_recv_free_metadata`.
///
/// **Key characteristics:**
/// - Zero allocations: References NDI SDK buffers directly
/// - Zero copies: No string duplication
/// - RAII lifetime: Exactly one free per frame, enforced at compile time
/// - Not `Send`: Prevents accidental cross-thread use of FFI buffers
///
/// # Examples
///
/// ```no_run
/// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, Source, SourceAddress};
/// # use std::time::Duration;
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = NDI::new()?;
/// # let source = Source { name: "Test".into(), address: SourceAddress::None };
/// # let options = ReceiverOptions::builder(source).build();
/// # let receiver = Receiver::new(&ndi, &options)?;
/// // Zero-copy capture
/// if let Some(frame) = receiver.capture_metadata_ref(Duration::from_millis(1000))? {
///     println!("Metadata: {}", frame.data().to_string_lossy());
///
///     // Frame is freed here when `frame` goes out of scope
/// }
/// # Ok(())
/// # }
/// ```
pub struct MetadataFrameRef<'rx> {
    guard: RecvMetadataGuard<'rx>,
}

impl<'rx> MetadataFrameRef<'rx> {
    /// Create a borrowed metadata frame from an RAII guard.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI receiver
    /// and contains a frame populated by `NDIlib_recv_capture_v3`.
    pub(crate) unsafe fn new(guard: RecvMetadataGuard<'rx>) -> Self {
        Self { guard }
    }

    /// Get the timecode.
    pub fn timecode(&self) -> i64 {
        self.guard.frame().timecode
    }

    /// Get a zero-copy view of the metadata as a `CStr`.
    ///
    /// This returns a reference directly into the NDI SDK's buffer.
    /// No allocation or string copying is performed.
    ///
    /// Returns an empty `CStr` if the metadata pointer is null.
    pub fn data(&self) -> &CStr {
        let p_data = self.guard.frame().p_data;
        if p_data.is_null() {
            // Return empty CStr for null pointer
            unsafe { CStr::from_bytes_with_nul_unchecked(b"\0") }
        } else {
            unsafe { CStr::from_ptr(p_data) }
        }
    }

    /// Convert this borrowed frame to an owned `MetadataFrame`.
    ///
    /// This performs a string copy, allowing the frame to outlive
    /// the NDI buffer and be sent across threads.
    pub fn to_owned(&self) -> MetadataFrame {
        MetadataFrame::from_raw(self.guard.frame())
    }
}

impl<'rx> fmt::Debug for MetadataFrameRef<'rx> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetadataFrameRef")
            .field("data", &self.data())
            .field("timecode", &self.timecode())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test uncompressed_buffer_len for packed RGB formats (32 bpp)
    #[test]
    fn test_uncompressed_buffer_len_packed_rgb() {
        let formats = [
            PixelFormat::BGRA,
            PixelFormat::BGRX,
            PixelFormat::RGBA,
            PixelFormat::RGBX,
        ];

        for fmt in formats {
            // 1920x1080, stride = 1920 * 4 = 7680
            let len = uncompressed_buffer_len(fmt, 7680, 1920, 1080);
            assert_eq!(len, 7680 * 1080, "Format {:?} even dimensions", fmt);

            // Odd dimensions: 1921x1081
            let len = uncompressed_buffer_len(fmt, 7684, 1921, 1081);
            assert_eq!(len, 7684 * 1081, "Format {:?} odd dimensions", fmt);
        }
    }

    /// Test uncompressed_buffer_len for packed YUV formats
    #[test]
    fn test_uncompressed_buffer_len_packed_yuv() {
        // UYVY: 16 bpp = 2 bytes per pixel
        let len = uncompressed_buffer_len(PixelFormat::UYVY, 3840, 1920, 1080);
        assert_eq!(len, 3840 * 1080);

        // UYVA: 24 bpp = 3 bytes per pixel
        let len = uncompressed_buffer_len(PixelFormat::UYVA, 5760, 1920, 1080);
        assert_eq!(len, 5760 * 1080);

        // P216/PA16: 32 bpp = 4 bytes per pixel
        let len = uncompressed_buffer_len(PixelFormat::P216, 7680, 1920, 1080);
        assert_eq!(len, 7680 * 1080);

        let len = uncompressed_buffer_len(PixelFormat::PA16, 7680, 1920, 1080);
        assert_eq!(len, 7680 * 1080);
    }

    /// Test uncompressed_buffer_len for planar YV12/I420 with even dimensions
    #[test]
    fn test_uncompressed_buffer_len_planar_420_even() {
        // 1920x1080 YV12/I420
        // Y: 1920 * 1080 = 2,073,600
        // U: (1920/2) * (1080/2) = 960 * 540 = 518,400
        // V: (1920/2) * (1080/2) = 960 * 540 = 518,400
        // Total: 2,073,600 + 518,400 + 518,400 = 3,110,400
        let y_stride = 1920;
        let len = uncompressed_buffer_len(PixelFormat::YV12, y_stride, 1920, 1080);
        assert_eq!(len, 3_110_400, "YV12 1920x1080");

        let len = uncompressed_buffer_len(PixelFormat::I420, y_stride, 1920, 1080);
        assert_eq!(len, 3_110_400, "I420 1920x1080");
    }

    /// Test uncompressed_buffer_len for planar YV12/I420 with odd dimensions
    #[test]
    fn test_uncompressed_buffer_len_planar_420_odd() {
        // 1921x1081 YV12/I420 (odd width and height)
        // Y: 1921 * 1081 = 2,076,601
        // U: (1921/2) * ceil(1081/2) = 960 * 541 = 519,360 (using ceil_div2)
        // V: (1921/2) * ceil(1081/2) = 960 * 541 = 519,360
        // Total: 2,076,601 + 519,360 + 519,360 = 3,115,321
        let y_stride = 1921;
        let len = uncompressed_buffer_len(PixelFormat::YV12, y_stride, 1921, 1081);
        assert_eq!(len, 3_115_321, "YV12 1921x1081 (odd dimensions)");

        let len = uncompressed_buffer_len(PixelFormat::I420, y_stride, 1921, 1081);
        assert_eq!(len, 3_115_321, "I420 1921x1081 (odd dimensions)");
    }

    /// Test uncompressed_buffer_len for semi-planar NV12 with even dimensions
    #[test]
    fn test_uncompressed_buffer_len_nv12_even() {
        // 1920x1080 NV12
        // Y: 1920 * 1080 = 2,073,600
        // UV: 1920 * (1080/2) = 1920 * 540 = 1,036,800
        // Total: 2,073,600 + 1,036,800 = 3,110,400
        let y_stride = 1920;
        let len = uncompressed_buffer_len(PixelFormat::NV12, y_stride, 1920, 1080);
        assert_eq!(len, 3_110_400, "NV12 1920x1080");
    }

    /// Test uncompressed_buffer_len for semi-planar NV12 with odd dimensions
    #[test]
    fn test_uncompressed_buffer_len_nv12_odd() {
        // 1921x1081 NV12 (odd width and height)
        // Y: 1921 * 1081 = 2,076,601
        // UV: 1921 * ceil(1081/2) = 1921 * 541 = 1,039,261
        // Total: 2,076,601 + 1,039,261 = 3,115,862
        let y_stride = 1921;
        let len = uncompressed_buffer_len(PixelFormat::NV12, y_stride, 1921, 1081);
        assert_eq!(len, 3_115_862, "NV12 1921x1081 (odd dimensions)");
    }

    /// Test ceil_div2 helper
    #[test]
    fn test_ceil_div2() {
        assert_eq!(ceil_div2(0), 0);
        assert_eq!(ceil_div2(1), 1);
        assert_eq!(ceil_div2(2), 1);
        assert_eq!(ceil_div2(3), 2);
        assert_eq!(ceil_div2(4), 2);
        assert_eq!(ceil_div2(1920), 960);
        assert_eq!(ceil_div2(1921), 961);
        assert_eq!(ceil_div2(1080), 540);
        assert_eq!(ceil_div2(1081), 541);
    }

    /// Test is_planar_420 helper
    #[test]
    fn test_is_planar_420() {
        assert!(is_planar_420(PixelFormat::YV12));
        assert!(is_planar_420(PixelFormat::I420));
        assert!(is_planar_420(PixelFormat::NV12));

        assert!(!is_planar_420(PixelFormat::BGRA));
        assert!(!is_planar_420(PixelFormat::RGBA));
        assert!(!is_planar_420(PixelFormat::UYVY));
        assert!(!is_planar_420(PixelFormat::UYVA));
    }

    /// Test VideoFrame builder with planar formats produces correct buffer sizes
    #[test]
    fn test_videoframe_builder_planar_even() {
        let frame = VideoFrame::builder()
            .resolution(1920, 1080)
            .pixel_format(PixelFormat::NV12)
            .build()
            .expect("Builder should succeed");

        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.pixel_format, PixelFormat::NV12);
        assert_eq!(frame.data.len(), 3_110_400, "NV12 1920x1080 buffer size");
    }

    /// Test VideoFrame builder with planar formats and odd dimensions
    #[test]
    fn test_videoframe_builder_planar_odd() {
        let frame = VideoFrame::builder()
            .resolution(1921, 1081)
            .pixel_format(PixelFormat::I420)
            .build()
            .expect("Builder should succeed");

        assert_eq!(frame.width, 1921);
        assert_eq!(frame.height, 1081);
        assert_eq!(frame.pixel_format, PixelFormat::I420);
        assert_eq!(
            frame.data.len(),
            3_115_321,
            "I420 1921x1081 buffer size with ceiling division"
        );
    }

    /// Test VideoFrame builder with packed format (regression test)
    #[test]
    fn test_videoframe_builder_packed() {
        let frame = VideoFrame::builder()
            .resolution(1920, 1080)
            .pixel_format(PixelFormat::BGRA)
            .build()
            .expect("Builder should succeed");

        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.pixel_format, PixelFormat::BGRA);
        assert_eq!(
            frame.data.len(),
            1920 * 1080 * 4,
            "BGRA buffer size unchanged"
        );
    }

    /// Test VideoFrame::from_raw with synthetic NV12 frame
    #[test]
    fn test_videoframe_from_raw_nv12() {
        // Create a synthetic NV12 frame
        let width = 1920;
        let height = 1080;
        let y_stride = 1920;
        let expected_size = 3_110_400; // Y + UV for NV12

        let mut data = vec![0u8; expected_size];
        // Mark the last byte to verify it's copied
        data[expected_size - 1] = 0xFF;

        let c_frame = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: PixelFormat::NV12.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: y_stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let frame = unsafe { VideoFrame::from_raw(&c_frame) }.expect("from_raw should succeed");

        assert_eq!(frame.width, width);
        assert_eq!(frame.height, height);
        assert_eq!(frame.pixel_format, PixelFormat::NV12);
        assert_eq!(
            frame.data.len(),
            expected_size,
            "Should copy full Y+UV buffer"
        );
        assert_eq!(
            frame.data[expected_size - 1],
            0xFF,
            "Last byte should be copied"
        );
    }

    /// Test VideoFrame::from_raw with synthetic I420 frame (odd dimensions)
    #[test]
    fn test_videoframe_from_raw_i420_odd() {
        let width = 1921;
        let height = 1081;
        let y_stride = 1921;
        let expected_size = 3_115_321; // Y + U + V with ceiling division

        let mut data = vec![0u8; expected_size];
        data[expected_size - 1] = 0xAA;

        let c_frame = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: PixelFormat::I420.into(),
            frame_rate_N: 30,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: y_stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let frame = unsafe { VideoFrame::from_raw(&c_frame) }.expect("from_raw should succeed");

        assert_eq!(
            frame.data.len(),
            expected_size,
            "I420 odd dimensions: full buffer copied"
        );
        assert_eq!(frame.data[expected_size - 1], 0xAA, "Last byte copied");
    }

    /// Regression test: VideoFrame::from_raw with packed format should be unchanged
    #[test]
    fn test_videoframe_from_raw_packed_regression() {
        let width = 1920;
        let height = 1080;
        let stride = 1920 * 4; // BGRA
        let expected_size = (stride * height) as usize;

        let mut data = vec![0u8; expected_size];

        let c_frame = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let frame = unsafe { VideoFrame::from_raw(&c_frame) }.expect("from_raw should succeed");
        assert_eq!(
            frame.data.len(),
            expected_size,
            "BGRA buffer size unchanged"
        );
    }

    /// Test that VideoFrameRef::new rejects unknown FourCC
    #[test]
    fn test_videoframeref_unknown_fourcc() {
        use crate::recv_guard::RecvVideoGuard;

        let width = 1920;
        let height = 1080;
        let stride = 1920 * 4;
        let expected_size = (stride * height) as usize;
        let mut data = vec![0u8; expected_size];

        // Use an unknown FourCC value (0xDEADBEEF)
        // On Windows FourCC is i32, on Linux it's u32
        #[allow(clippy::unnecessary_cast)]
        let c_frame = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            #[cfg(target_os = "windows")]
            FourCC: 0xDEADBEEFu32 as i32, // Unknown FourCC
            #[cfg(not(target_os = "windows"))]
            FourCC: 0xDEADBEEF, // Unknown FourCC
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        // Create a mock receiver instance (null is fine for this test since we don't free)
        let mock_instance = ptr::null_mut();
        let guard = unsafe { RecvVideoGuard::new(mock_instance, c_frame) };

        // VideoFrameRef::new should return an error for unknown FourCC
        let result = unsafe { VideoFrameRef::new(guard) };
        assert!(result.is_err(), "Should reject unknown FourCC");

        if let Err(Error::InvalidFrame(ref msg)) = result {
            assert!(
                msg.contains("0xDEADBEEF"),
                "Error message should include FourCC: {}",
                msg
            );
        } else {
            panic!("Expected InvalidFrame error");
        }

        // Manually free to prevent guard from calling NDI free on null instance
        std::mem::forget(result);
    }

    /// Test that VideoFrameRef::new accepts known FourCC and stores validated format
    #[test]
    fn test_videoframeref_known_fourcc() {
        use crate::recv_guard::RecvVideoGuard;

        let width = 1920;
        let height = 1080;
        let stride = 1920 * 4;
        let expected_size = (stride * height) as usize;
        let mut data = vec![0u8; expected_size];

        let c_frame = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let mock_instance = ptr::null_mut();
        let guard = unsafe { RecvVideoGuard::new(mock_instance, c_frame) };

        let frame_ref = unsafe { VideoFrameRef::new(guard) }.expect("Should accept BGRA FourCC");
        assert_eq!(
            frame_ref.pixel_format(),
            PixelFormat::BGRA,
            "Should store validated pixel format"
        );

        // Manually free to prevent guard from calling NDI free on null instance
        std::mem::forget(frame_ref);
    }

    /// Test that AudioFrameRef::new rejects unknown FourCC
    #[test]
    fn test_audioframeref_unknown_fourcc() {
        use crate::recv_guard::RecvAudioGuard;

        let num_samples = 1024;
        let num_channels = 2;
        let sample_count = (num_samples * num_channels) as usize;
        let mut data = vec![0.0f32; sample_count];

        // Use an unknown FourCC value (0xBADC0DE)
        let c_frame = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: num_channels,
            no_samples: num_samples,
            timecode: 0,
            FourCC: 0xBADC0DE, // Unknown audio FourCC
            p_data: data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: num_samples * 4,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let mock_instance = ptr::null_mut();
        let guard = unsafe { RecvAudioGuard::new(mock_instance, c_frame) };

        let result = unsafe { AudioFrameRef::new(guard) };
        assert!(result.is_err(), "Should reject unknown audio FourCC");

        if let Err(Error::InvalidFrame(ref msg)) = result {
            assert!(
                msg.contains("0x0BADC0DE"),
                "Error message should include FourCC: {}",
                msg
            );
        } else {
            panic!("Expected InvalidFrame error");
        }

        std::mem::forget(result);
    }

    /// Test that AudioFrameRef::new accepts known FourCC and stores validated format
    #[test]
    fn test_audioframeref_known_fourcc() {
        use crate::recv_guard::RecvAudioGuard;

        let num_samples = 1024;
        let num_channels = 2;
        let sample_count = (num_samples * num_channels) as usize;
        let mut data = vec![0.0f32; sample_count];

        let c_frame = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: num_channels,
            no_samples: num_samples,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: num_samples * 4,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let mock_instance = ptr::null_mut();
        let guard = unsafe { RecvAudioGuard::new(mock_instance, c_frame) };

        let frame_ref = unsafe { AudioFrameRef::new(guard) }.expect("Should accept FLTP FourCC");
        assert_eq!(
            frame_ref.format(),
            AudioFormat::FLTP,
            "Should store validated audio format"
        );

        std::mem::forget(frame_ref);
    }

    /// Test that VideoFrameRef correctly uses validated format for data size calculation
    #[test]
    fn test_videoframeref_data_uses_validated_format() {
        use crate::recv_guard::RecvVideoGuard;

        // Test with uncompressed format (BGRA)
        let width = 1920;
        let height = 1080;
        let stride = 1920 * 4;
        let expected_size = (stride * height) as usize;
        let mut data = vec![0xAB_u8; expected_size];

        let c_frame = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let mock_instance = ptr::null_mut();
        let guard = unsafe { RecvVideoGuard::new(mock_instance, c_frame) };
        let frame_ref = unsafe { VideoFrameRef::new(guard) }.expect("Should create frame ref");

        // Verify data() returns correct size based on validated format
        assert_eq!(
            frame_ref.data().len(),
            expected_size,
            "data() should use validated pixel format for size calculation"
        );

        // Verify line_stride_or_size() uses validated format
        assert_eq!(
            frame_ref.line_stride_or_size(),
            LineStrideOrSize::LineStrideBytes(stride),
            "line_stride_or_size() should use validated format"
        );

        std::mem::forget(frame_ref);
    }

    /// Test that MAX_VIDEO_BYTES constant is properly defined
    #[test]
    fn test_max_video_bytes_constant() {
        // Verify the constant is set to 100 MiB as specified
        assert_eq!(MAX_VIDEO_BYTES, 100 * 1024 * 1024);
    }

    /// Test that MAX_AUDIO_BYTES constant is properly defined
    #[test]
    fn test_max_audio_bytes_constant() {
        // Verify the constant is set to 64 MiB as specified
        assert_eq!(MAX_AUDIO_BYTES, 64 * 1024 * 1024);
    }

    /// Test that audio frames with overflow in checked_mul are rejected
    #[test]
    fn test_audio_overflow_checked_mul() {
        // Use no_samples = i32::MAX and no_channels = 2 to trigger overflow
        // When multiplying i32::MAX * 2, the result overflows usize::checked_mul
        let no_samples = i32::MAX;
        let no_channels = 2;
        let mut dummy_data = vec![0f32; 1024]; // Small buffer, won't be used due to guard

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels,
            no_samples,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: dummy_data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 0,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = AudioFrame::from_raw(raw);
        assert!(
            result.is_err(),
            "Should reject audio frame with sample count overflow or exceeding size limit"
        );

        if let Err(Error::InvalidFrame(msg)) = result {
            // Accept either overflow or size limit error - both are correct guards
            assert!(
                msg.contains("overflow") || msg.contains("exceeds maximum size"),
                "Error message should mention overflow or size limit, got: {msg}"
            );
        } else {
            panic!("Expected InvalidFrame error");
        }
    }

    /// Test that normal audio frames within bounds succeed
    #[test]
    fn test_audio_within_bounds_succeeds() {
        // Typical audio frame: 48kHz, 2 channels, 1024 samples
        let sample_rate = 48000;
        let no_channels = 2;
        let no_samples = 1024;
        let sample_count = (no_samples * no_channels) as usize;
        let mut data = vec![0.5f32; sample_count];

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate,
            no_channels,
            no_samples,
            timecode: 12345,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 0,
            },
            p_metadata: ptr::null(),
            timestamp: 67890,
        };

        let result = AudioFrame::from_raw(raw);
        assert!(
            result.is_ok(),
            "Should accept normal audio frame within bounds"
        );

        let frame = result.unwrap();
        assert_eq!(frame.data().len(), sample_count);
        assert_eq!(frame.num_samples, no_samples);
        assert_eq!(frame.num_channels, no_channels);
    }

    /// Test that uncompressed video uses MAX_VIDEO_BYTES constant for bounds check
    #[test]
    fn test_video_uncompressed_uses_constant_cap() {
        // Create an uncompressed frame that would exceed MAX_VIDEO_BYTES
        // 8K resolution: 7680 x 4320, BGRA = 4 bytes per pixel
        // Total: 7680 * 4320 * 4 = 132,710,400 bytes > 100 MiB
        let width = 7680;
        let height = 4320;
        let stride = width * 4;
        let expected_size = (stride * height) as usize;
        let mut data = vec![0u8; expected_size];

        let c_frame = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = unsafe { VideoFrame::from_raw(&c_frame) };
        assert!(
            result.is_err(),
            "Should reject uncompressed video frame exceeding MAX_VIDEO_BYTES"
        );

        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(
                msg.contains("exceeds maximum size"),
                "Error message should mention size limit, got: {msg}"
            );
        } else {
            panic!("Expected InvalidFrame error");
        }
    }
}
