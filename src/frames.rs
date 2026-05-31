//! Frame types for video, audio, and metadata.

use num_enum::{IntoPrimitive, TryFromPrimitive};

#[cfg(feature = "image-encoding")]
use std::borrow::Cow;
use std::{
    ffi::{CStr, CString},
    fmt,
    num::NonZeroUsize,
    os::raw::c_char,
    ptr, slice, str,
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

/// Category of pixel format for buffer calculation and union field access.
///
/// This enum distinguishes between different memory layouts:
/// - **Packed**: All pixel components are interleaved in a single buffer
/// - **Planar420**: Y, U, V planes stored separately with 4:2:0 chroma subsampling
/// - **SemiPlanar420**: Y plane followed by interleaved UV plane (NV12)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatCategory {
    /// Packed formats: simple stride * height buffer.
    Packed,
    /// Planar 4:2:0 with separate U and V planes (YV12, I420).
    Planar420,
    /// Semi-planar 4:2:0 with interleaved UV plane (NV12).
    SemiPlanar420,
}

/// Compile-time pixel format properties.
///
/// This struct encapsulates all format-specific knowledge in one location,
/// providing a single source of truth for buffer size calculations, stride
/// computation, and format category detection.
///
/// # Examples
///
/// ```
/// use grafton_ndi::{PixelFormat, FormatCategory};
///
/// let info = PixelFormat::BGRA.info();
/// assert_eq!(info.bytes_per_pixel(), 4);
/// assert_eq!(info.category(), FormatCategory::Packed);
///
/// let info = PixelFormat::NV12.info();
/// assert_eq!(info.bytes_per_pixel(), 1);
/// assert_eq!(info.category(), FormatCategory::SemiPlanar420);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormatInfo {
    /// Bytes per pixel for packed formats, or Y-plane bytes per pixel for planar formats.
    bytes_per_pixel: u8,
    /// Format category for union field access and buffer calculation.
    category: FormatCategory,
}

impl PixelFormatInfo {
    /// Get bytes per pixel for packed formats, or Y-plane bytes per pixel for planar.
    #[must_use]
    pub const fn bytes_per_pixel(&self) -> u8 {
        self.bytes_per_pixel
    }

    /// Get the format category.
    #[must_use]
    pub const fn category(&self) -> FormatCategory {
        self.category
    }

    /// Returns true if this is a planar 4:2:0 format (YV12, I420, or NV12).
    #[must_use]
    pub const fn is_planar_420(&self) -> bool {
        matches!(
            self.category,
            FormatCategory::Planar420 | FormatCategory::SemiPlanar420
        )
    }

    /// Calculate total buffer size for given dimensions and stride using
    /// checked arithmetic.
    ///
    /// # Arguments
    ///
    /// * `y_stride` - The Y-plane line stride in bytes (for planar formats) or total line stride (for packed formats)
    /// * `height` - Frame height in pixels
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if `y_stride` or `height` is not
    /// positive, if planar 4:2:0 stride/height requirements are not met, if
    /// arithmetic overflows, or if the result exceeds the crate's maximum video
    /// frame size.
    ///
    /// # Format-specific calculations
    ///
    /// - **Packed RGB/YUV** (BGRA/BGRX/RGBA/RGBX/UYVY/UYVA/P216/PA16): `y_stride * height`
    /// - **Planar 4:2:0 YV12/I420**: requires even stride and height,
    ///   then `Y + U + V` where:
    ///   - Y plane: `y_stride * height`
    ///   - U plane: `(y_stride/2) * (height/2)`
    ///   - V plane: `(y_stride/2) * (height/2)`
    /// - **Semi-planar 4:2:0 NV12**: requires even height, then `Y + UV` where:
    ///   - Y plane: `y_stride * height`
    ///   - UV plane: `y_stride * (height/2)`
    pub fn try_buffer_len(&self, y_stride: i32, height: i32) -> Result<usize> {
        if y_stride <= 0 {
            return Err(Error::InvalidFrame(format!(
                "Video line stride must be positive, got {y_stride}"
            )));
        }
        if height <= 0 {
            return Err(Error::InvalidFrame(format!(
                "Video frame height must be positive, got {height}"
            )));
        }

        let y_stride = usize::try_from(y_stride)
            .map_err(|_| Error::InvalidFrame(format!("Invalid y_stride value: {y_stride}")))?;
        let height = usize::try_from(height)
            .map_err(|_| Error::InvalidFrame(format!("Invalid height value: {height}")))?;

        let len = calculate_buffer_len_for_info_checked(*self, y_stride, height)?;
        validate_video_data_len(len)?;
        Ok(len)
    }
}

impl PixelFormat {
    /// Get compile-time format properties.
    ///
    /// This provides a single source of truth for all format-specific knowledge,
    /// including bytes per pixel and format category.
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::{PixelFormat, FormatCategory};
    ///
    /// // Get properties for BGRA (32 bpp packed)
    /// let info = PixelFormat::BGRA.info();
    /// assert_eq!(info.bytes_per_pixel(), 4);
    /// assert_eq!(info.category(), FormatCategory::Packed);
    ///
    /// // Get properties for YV12 (planar 4:2:0)
    /// let info = PixelFormat::YV12.info();
    /// assert_eq!(info.bytes_per_pixel(), 1);
    /// assert_eq!(info.category(), FormatCategory::Planar420);
    /// ```
    #[must_use]
    pub const fn info(self) -> PixelFormatInfo {
        match self {
            Self::BGRA | Self::BGRX | Self::RGBA | Self::RGBX => PixelFormatInfo {
                bytes_per_pixel: 4,
                category: FormatCategory::Packed,
            },
            Self::UYVY => PixelFormatInfo {
                bytes_per_pixel: 2,
                category: FormatCategory::Packed,
            },
            Self::UYVA => PixelFormatInfo {
                bytes_per_pixel: 3,
                category: FormatCategory::Packed,
            },
            Self::P216 | Self::PA16 => PixelFormatInfo {
                bytes_per_pixel: 4,
                category: FormatCategory::Packed,
            },
            Self::YV12 | Self::I420 => PixelFormatInfo {
                bytes_per_pixel: 1,
                category: FormatCategory::Planar420,
            },
            Self::NV12 => PixelFormatInfo {
                bytes_per_pixel: 1,
                category: FormatCategory::SemiPlanar420,
            },
        }
    }

    /// Calculate line stride in bytes for a given width using checked
    /// arithmetic.
    ///
    /// For packed formats, this returns the total bytes per row.
    /// For planar formats, this returns the Y-plane stride.
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::PixelFormat;
    ///
    /// // BGRA: 4 bytes per pixel
    /// assert_eq!(PixelFormat::BGRA.try_line_stride(1920)?, 7680);
    ///
    /// // UYVY: 2 bytes per pixel
    /// assert_eq!(PixelFormat::UYVY.try_line_stride(1920)?, 3840);
    ///
    /// // NV12: Y-plane has 1 byte per pixel
    /// assert_eq!(PixelFormat::NV12.try_line_stride(1920)?, 1920);
    /// # Ok::<(), grafton_ndi::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if the width is invalid for this format
    /// or if the stride does not fit in `i32`.
    pub fn try_line_stride(self, width: i32) -> Result<i32> {
        validate_video_width_for_format(self, width)?;
        let width_usize = usize::try_from(width)
            .map_err(|_| Error::InvalidFrame(format!("Invalid width value: {width}")))?;
        let stride = min_video_line_stride_checked(self, width_usize)?;
        i32::try_from(stride).map_err(|_| {
            Error::InvalidFrame(format!("Video line stride {stride} exceeds i32 range"))
        })
    }

    /// Calculate the total buffer size needed for a frame with given dimensions
    /// using checked arithmetic.
    ///
    /// This computes the validated minimum stride from the width and delegates
    /// to the shared video layout validator.
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::PixelFormat;
    ///
    /// // BGRA 1920x1080: 1920 * 4 * 1080 = 8,294,400 bytes
    /// assert_eq!(PixelFormat::BGRA.try_buffer_size(1920, 1080)?, 8_294_400);
    ///
    /// // NV12 1920x1080: Y (1920*1080) + UV (1920*540) = 3,110,400 bytes
    /// assert_eq!(PixelFormat::NV12.try_buffer_size(1920, 1080)?, 3_110_400);
    /// # Ok::<(), grafton_ndi::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if dimensions are invalid for this
    /// format, if arithmetic overflows, or if the result exceeds the crate's
    /// maximum video frame size.
    pub fn try_buffer_size(self, width: i32, height: i32) -> Result<usize> {
        let layout = ValidatedVideoLayout::new_uncompressed(self, width, height, None)?;
        Ok(layout.data_len_bytes)
    }
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
    layout: ValidatedVideoLayout,
    frame_rate_n: i32,
    frame_rate_d: i32,
    picture_aspect_ratio: f32,
    scan_type: ScanType,
    timecode: i64,
    data: Vec<u8>,
    metadata: Option<FrameMetadata>,
    timestamp: i64,
}

impl fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("pixel_format", &self.pixel_format())
            .field("frame_rate_n", &self.frame_rate_n)
            .field("frame_rate_d", &self.frame_rate_d)
            .field("picture_aspect_ratio", &self.picture_aspect_ratio)
            .field("scan_type", &self.scan_type)
            .field("timecode", &self.timecode)
            .field("data (bytes)", &self.data.len())
            .field("line_stride_or_size", &self.line_stride_or_size())
            .field("metadata", &self.metadata())
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
    pub(crate) fn to_raw(&self) -> NDIlib_video_frame_v2_t {
        NDIlib_video_frame_v2_t {
            xres: self.layout.width,
            yres: self.layout.height,
            FourCC: self.layout.pixel_format.into(),
            frame_rate_N: self.frame_rate_n,
            frame_rate_D: self.frame_rate_d,
            picture_aspect_ratio: self.picture_aspect_ratio,
            frame_format_type: self.scan_type.into(),
            timecode: self.timecode,
            p_data: self.data.as_ptr() as *mut u8,
            __bindgen_anon_1: self.layout.line_stride_or_size.into(),
            p_metadata: self
                .metadata
                .as_ref()
                .map_or(ptr::null(), FrameMetadata::as_ptr),
            timestamp: self.timestamp,
        }
    }

    /// Get the frame width in pixels.
    pub fn width(&self) -> i32 {
        self.layout.width
    }

    /// Get the frame height in pixels.
    pub fn height(&self) -> i32 {
        self.layout.height
    }

    /// Get the supported pixel format.
    pub fn pixel_format(&self) -> PixelFormat {
        self.layout.pixel_format
    }

    /// Get the frame rate numerator.
    pub fn frame_rate_n(&self) -> i32 {
        self.frame_rate_n
    }

    /// Get the frame rate denominator.
    pub fn frame_rate_d(&self) -> i32 {
        self.frame_rate_d
    }

    /// Get the picture aspect ratio.
    pub fn picture_aspect_ratio(&self) -> f32 {
        self.picture_aspect_ratio
    }

    /// Get the scan type.
    pub fn scan_type(&self) -> ScanType {
        self.scan_type
    }

    /// Get the timecode.
    ///
    /// A value of zero is passed through to the SDK as its default timestamp
    /// behavior.
    pub fn timecode(&self) -> i64 {
        self.timecode
    }

    /// Get the timestamp.
    ///
    /// A value of zero is passed through to the SDK as its default timestamp
    /// behavior.
    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }

    /// Get the validated line stride or data size union field.
    pub fn line_stride_or_size(&self) -> LineStrideOrSize {
        self.layout.line_stride_or_size
    }

    pub(crate) fn validated_layout(&self) -> ValidatedVideoLayout {
        self.layout
    }

    /// Get the frame data.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Get mutable access to the frame data without changing the validated
    /// layout.
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Replace the owned frame data while preserving the validated layout.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if `data` is not exactly the validated
    /// layout size.
    pub fn replace_data(&mut self, data: Vec<u8>) -> Result<()> {
        if data.len() != self.layout.data_len_bytes {
            return Err(Error::InvalidFrame(format!(
                "Video data length {}, expected {} bytes for validated layout",
                data.len(),
                self.layout.data_len_bytes
            )));
        }

        self.data = data;
        Ok(())
    }

    /// Get frame metadata as UTF-8 text, if present.
    pub fn metadata(&self) -> Option<&str> {
        self.metadata.as_ref().map(FrameMetadata::as_str)
    }

    pub(crate) fn metadata_cstr(&self) -> Option<&CStr> {
        self.metadata.as_ref().map(FrameMetadata::as_cstr)
    }

    /// Replace frame metadata.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] if `metadata` contains an interior NUL
    /// byte, or [`Error::InvalidFrame`] if the emitted C string would exceed
    /// the metadata size cap.
    pub fn set_metadata<S: Into<String>>(&mut self, metadata: Option<S>) -> Result<()> {
        self.metadata = metadata.map(FrameMetadata::new).transpose()?;
        Ok(())
    }

    /// Set the frame rate.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if the numerator or denominator is not
    /// positive.
    pub fn set_frame_rate(&mut self, numerator: i32, denominator: i32) -> Result<()> {
        validate_video_frame_metadata(numerator, denominator, self.picture_aspect_ratio)?;
        self.frame_rate_n = numerator;
        self.frame_rate_d = denominator;
        Ok(())
    }

    /// Set the picture aspect ratio.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if `ratio` is not finite and positive.
    pub fn set_picture_aspect_ratio(&mut self, ratio: f32) -> Result<()> {
        validate_video_frame_metadata(self.frame_rate_n, self.frame_rate_d, ratio)?;
        self.picture_aspect_ratio = ratio;
        Ok(())
    }

    /// Encode the video frame as PNG bytes.
    ///
    /// This method encodes the frame to PNG format, automatically handling color format
    /// conversion from the NDI frame format (BGRA/RGBA/etc.) to PNG-compatible RGBA.
    ///
    /// # Supported Formats
    ///
    /// - `RGBA`: Direct encoding when rows are tightly packed
    /// - `BGRA`: Swaps red and blue channels and preserves alpha
    /// - `RGBX`: Treats the fourth byte as padding and writes opaque alpha
    /// - `BGRX`: Swaps red/blue and writes opaque alpha
    /// - Other formats: Returns an error (unsupported for now)
    ///
    /// # Stride Handling
    ///
    /// This method consumes active pixels row-by-row according to the frame's
    /// validated line stride. Valid row padding is skipped.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The frame format is not RGBA/RGBX/BGRA/BGRX
    /// - The frame uses data-size layout instead of line-stride layout
    /// - The backing data length does not match the validated layout
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
    /// # let sources = finder.current_sources()?;
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

        let image = ImagePixelSource::new(self.layout, &self.data)?;
        let (rgba_data, width, height) = image.png_rgba_input()?;

        // Encode to PNG
        let mut png_data = Vec::new();
        let mut encoder = Encoder::new(&mut png_data, width, height);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);

        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(rgba_data.as_ref()))
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
    /// - `RGBA` / `RGBX`: Emits RGB
    /// - `BGRA` / `BGRX`: Swaps red/blue and emits RGB
    /// - Other formats: Returns an error (unsupported for now)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The frame format is not RGBA/RGBX/BGRA/BGRX
    /// - The frame uses data-size layout instead of line-stride layout
    /// - The backing data length does not match the validated layout
    /// - The quality is outside `1..=100`
    /// - The dimensions exceed JPEG's `u16` width/height range
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
    /// # let sources = finder.current_sources()?;
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

        let image = ImagePixelSource::new(self.layout, &self.data)?;
        let (rgb_data, width, height) = image.jpeg_rgb_input(quality)?;

        // Encode to JPEG
        let mut jpeg_data = Vec::new();
        let encoder = JpegEncoder::new(&mut jpeg_data, quality);
        encoder
            .encode(&rgb_data, width, height, JpegColorType::Rgb)
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
    /// # let sources = finder.current_sources()?;
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
        // Use the shared validation helper to validate and compute layout
        let layout = validate_video_layout(c_frame)?;
        let metadata_layout = unsafe { validate_frame_metadata(c_frame.p_metadata)? };

        Self::from_raw_validated(c_frame, layout, metadata_layout)
    }

    pub(crate) unsafe fn from_raw_validated(
        c_frame: &NDIlib_video_frame_v2_t,
        layout: ValidatedVideoLayout,
        metadata_layout: ValidatedFrameMetadata,
    ) -> Result<VideoFrame> {
        // Copy data for ownership
        let slice = slice::from_raw_parts(c_frame.p_data, layout.data_len_bytes);
        let data = slice.to_vec();

        let metadata =
            unsafe { FrameMetadata::copy_from_raw_validated(c_frame.p_metadata, metadata_layout) };

        #[allow(clippy::unnecessary_cast)] // Required for Windows where frame_format_type is i32
        let scan_type = ScanType::try_from(c_frame.frame_format_type as u32).map_err(|_| {
            Error::InvalidFrame(format!(
                "Unknown scan type: 0x{:08X}",
                c_frame.frame_format_type
            ))
        })?;

        Ok(VideoFrame {
            layout,
            frame_rate_n: c_frame.frame_rate_N,
            frame_rate_d: c_frame.frame_rate_D,
            picture_aspect_ratio: c_frame.picture_aspect_ratio,
            scan_type,
            timecode: c_frame.timecode,
            data,
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

        validate_video_frame_metadata(frame_rate_n, frame_rate_d, picture_aspect_ratio)?;
        let layout = ValidatedVideoLayout::new_uncompressed(pixel_format, width, height, None)?;
        let buffer_size = layout.data_len_bytes;
        let data = vec![0u8; buffer_size];

        let metadata = self.metadata.map(FrameMetadata::new).transpose()?;

        Ok(VideoFrame {
            layout,
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio,
            scan_type,
            timecode: self.timecode.unwrap_or(0),
            data,
            metadata,
            timestamp: self.timestamp.unwrap_or(0),
        })
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
    layout: ValidatedAudioLayout,
    timecode: i64,
    data: Vec<f32>,
    metadata: Option<FrameMetadata>,
    timestamp: i64,
}

impl AudioFrame {
    pub(crate) fn to_raw(&self) -> NDIlib_audio_frame_v3_t {
        NDIlib_audio_frame_v3_t {
            sample_rate: self.layout.sample_rate,
            no_channels: self.num_channels(),
            no_samples: self.num_samples(),
            timecode: self.timecode,
            FourCC: self.format().into(),
            p_data: self.data.as_ptr() as *mut f32 as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: self.layout.channel_stride_in_bytes,
            },
            p_metadata: self
                .metadata
                .as_ref()
                .map_or(ptr::null(), FrameMetadata::as_ptr),
            timestamp: self.timestamp,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_raw(raw: NDIlib_audio_frame_v3_t) -> Result<AudioFrame> {
        // Use the shared validation helper to validate and compute layout
        let layout = validate_audio_layout(&raw)?;
        let metadata_layout = unsafe { validate_frame_metadata(raw.p_metadata)? };

        Self::from_raw_validated(raw, layout, metadata_layout)
    }

    pub(crate) fn from_raw_validated(
        raw: NDIlib_audio_frame_v3_t,
        layout: ValidatedAudioLayout,
        metadata_layout: ValidatedFrameMetadata,
    ) -> Result<AudioFrame> {
        if layout.is_empty() {
            return Err(Error::InvalidFrame(
                "Cannot create owned AudioFrame from an empty audio layout".into(),
            ));
        }

        // Copy data for ownership
        let slice = unsafe { slice::from_raw_parts(raw.p_data as *const f32, layout.sample_count) };
        let data = slice.to_vec();

        // Copy the string, don't take ownership - SDK will free the original.
        let metadata =
            unsafe { FrameMetadata::copy_from_raw_validated(raw.p_metadata, metadata_layout) };

        Ok(AudioFrame {
            layout,
            timecode: raw.timecode,
            data,
            metadata,
            timestamp: raw.timestamp,
        })
    }

    /// Create a builder for configuring an audio frame
    pub fn builder() -> AudioFrameBuilder {
        AudioFrameBuilder::new()
    }

    /// Get the sample rate in Hz.
    pub fn sample_rate(&self) -> i32 {
        self.layout.sample_rate
    }

    /// Get the number of audio channels.
    pub fn num_channels(&self) -> i32 {
        self.layout.no_channels as i32
    }

    /// Get the number of samples per channel.
    pub fn num_samples(&self) -> i32 {
        self.layout.no_samples as i32
    }

    /// Get the timecode.
    ///
    /// A value of zero is passed through to the SDK as its default timestamp
    /// behavior.
    pub fn timecode(&self) -> i64 {
        self.timecode
    }

    /// Get the timestamp.
    ///
    /// A value of zero is passed through to the SDK as its default timestamp
    /// behavior.
    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }

    /// Get the audio format.
    pub fn format(&self) -> AudioFormat {
        self.layout
            .format()
            .expect("owned AudioFrame always has a concrete audio format")
    }

    /// Get the channel stride in bytes.
    pub fn channel_stride_in_bytes(&self) -> i32 {
        self.layout.channel_stride_in_bytes
    }

    /// Get audio data as 32-bit floats
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// Get mutable audio sample data without changing the validated layout.
    pub fn data_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }

    /// Replace the owned audio data while preserving the validated layout.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if `data` is not exactly the validated
    /// sample count.
    pub fn replace_data(&mut self, data: Vec<f32>) -> Result<()> {
        if data.len() != self.layout.sample_count {
            return Err(Error::InvalidFrame(format!(
                "Audio data length {}, expected {} samples for validated layout",
                data.len(),
                self.layout.sample_count
            )));
        }

        self.data = data;
        Ok(())
    }

    /// Get frame metadata as UTF-8 text, if present.
    pub fn metadata(&self) -> Option<&str> {
        self.metadata.as_ref().map(FrameMetadata::as_str)
    }

    /// Replace frame metadata.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] if `metadata` contains an interior NUL
    /// byte, or [`Error::InvalidFrame`] if the emitted C string would exceed
    /// the metadata size cap.
    pub fn set_metadata<S: Into<String>>(&mut self, metadata: Option<S>) -> Result<()> {
        self.metadata = metadata.map(FrameMetadata::new).transpose()?;
        Ok(())
    }

    /// Get audio data for a specific channel
    ///
    /// Data is always stored in planar format internally. If `AudioLayout::Interleaved`
    /// was specified at build time, the data was converted to planar during construction.
    pub fn channel_data(&self, channel: usize) -> Option<Vec<f32>> {
        let range = self.layout.channel_range(channel)?;
        Some(self.data[range].to_vec())
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
    /// The data length must equal `num_channels * num_samples`. The layout must match
    /// the configured `AudioLayout`:
    /// - **Planar**: `[C0S0, C0S1, ..., C1S0, C1S1, ...]`
    /// - **Interleaved**: `[C0S0, C1S0, C0S1, C1S1, ...]` (converted to planar at build time)
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
    /// When `AudioLayout::Interleaved` is set, the input data is converted to planar
    /// format using the NDI SDK utility function. The resulting frame always stores
    /// planar data with `channel_stride_in_bytes = num_samples * 4`.
    pub fn build(self) -> Result<AudioFrame> {
        let sample_rate = self.sample_rate.unwrap_or(48000);
        let num_channels = self.num_channels.unwrap_or(2);
        let num_samples = self.num_samples.unwrap_or(1024);
        let format = self.format.unwrap_or(AudioFormat::FLTP);
        let layout = self.layout.unwrap_or(AudioLayout::Planar);
        let timecode = self.timecode.unwrap_or(0);
        let audio_layout =
            validate_outbound_audio_layout(sample_rate, num_channels, num_samples, format)?;
        let sample_count = audio_layout.sample_count;

        let data = if let Some(input_data) = self.data {
            if input_data.len() != sample_count {
                return Err(Error::InvalidFrame(format!(
                    "Audio data length {}, expected {} ({}ch x {}samples)",
                    input_data.len(),
                    sample_count,
                    num_channels,
                    num_samples
                )));
            }

            match layout {
                AudioLayout::Planar => input_data,
                AudioLayout::Interleaved => {
                    let nc = audio_layout.no_channels;
                    let ns = audio_layout.no_samples;
                    let mut planar = vec![0.0f32; sample_count];
                    for ch in 0..nc {
                        for s in 0..ns {
                            let dst = ch
                                .checked_mul(ns)
                                .and_then(|idx| idx.checked_add(s))
                                .ok_or_else(|| {
                                    Error::InvalidFrame(
                                        "Audio planar conversion index overflow".into(),
                                    )
                                })?;
                            let src = s
                                .checked_mul(nc)
                                .and_then(|idx| idx.checked_add(ch))
                                .ok_or_else(|| {
                                    Error::InvalidFrame(
                                        "Audio interleaved conversion index overflow".into(),
                                    )
                                })?;
                            planar[dst] = input_data[src];
                        }
                    }
                    planar
                }
            }
        } else {
            vec![0.0f32; sample_count]
        };

        let metadata = self.metadata.map(FrameMetadata::new).transpose()?;

        Ok(AudioFrame {
            layout: audio_layout,
            timecode,
            data,
            metadata,
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
    /// Input memory layout for 2 channels, 3 samples:
    /// `[C0S0, C1S0, C0S1, C1S1, C0S2, C1S2]`
    ///
    /// Interleaved data is converted to planar format at build time using the NDI SDK
    /// utility function, so the resulting `AudioFrame` always stores planar data.
    Interleaved,
}

impl From<AudioFormat> for i32 {
    fn from(value: AudioFormat) -> Self {
        let u32_value: u32 = value.into();
        u32_value as i32
    }
}

/// Maximum allowed size for supported video frame data (100 MiB).
const MAX_VIDEO_BYTES: usize = 100 * 1024 * 1024;

/// Maximum allowed size for audio frame data (64 MiB).
/// Comfortably above typical NDI audio frames while preventing unbounded allocations.
const MAX_AUDIO_BYTES: usize = 64 * 1024 * 1024;

/// Maximum allowed size for SDK metadata C strings (4 MiB), including the
/// trailing NUL terminator.
pub(crate) const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct MetadataFrame {
    data: String, // Owned metadata (typically XML)
    timecode: i64,
}

impl MetadataFrame {
    /// Create an empty metadata frame with default SDK timecode behavior.
    pub fn new() -> Self {
        MetadataFrame {
            data: String::new(),
            timecode: 0,
        }
    }

    /// Create a metadata frame from UTF-8 text and a timecode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] if `data` contains an interior NUL byte
    /// or [`Error::InvalidFrame`] if the SDK metadata length would exceed the
    /// crate's metadata size limit.
    pub fn with_data(data: impl Into<String>, timecode: i64) -> Result<Self> {
        let data = data.into();
        validate_metadata_text(&data)?;
        Ok(MetadataFrame { data, timecode })
    }

    /// Get the metadata text.
    pub fn data(&self) -> &str {
        &self.data
    }

    /// Consume the frame and return the owned metadata text.
    pub fn into_data(self) -> String {
        self.data
    }

    /// Get the timecode.
    ///
    /// A value of zero is passed through to the SDK as its default timestamp
    /// behavior.
    pub fn timecode(&self) -> i64 {
        self.timecode
    }

    /// Replace the metadata text.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] if `data` contains an interior NUL byte
    /// or [`Error::InvalidFrame`] if the SDK metadata length would exceed the
    /// crate's metadata size limit.
    pub fn set_data(&mut self, data: impl Into<String>) -> Result<()> {
        let data = data.into();
        validate_metadata_text(&data)?;
        self.data = data;
        Ok(())
    }

    /// Set the timecode.
    pub fn set_timecode(&mut self, timecode: i64) {
        self.timecode = timecode;
    }

    /// Return this frame with an updated timecode.
    pub fn with_timecode(mut self, timecode: i64) -> Self {
        self.timecode = timecode;
        self
    }

    /// Convert to raw format for sending
    pub(crate) fn to_raw(&self) -> Result<(CString, NDIlib_metadata_frame_t)> {
        let c_data = CString::new(self.data.as_bytes()).map_err(Error::InvalidCString)?;
        let length = validate_metadata_len_with_nul(c_data.as_bytes_with_nul().len())?;
        let raw = NDIlib_metadata_frame_t {
            length,
            timecode: self.timecode,
            p_data: c_data.as_ptr() as *mut c_char,
        };
        Ok((c_data, raw))
    }

    /// Create from raw NDI metadata frame (copies the data)
    ///
    /// # Safety
    ///
    /// `raw` must be an SDK-populated metadata frame whose `p_data` pointer is
    /// valid for `raw.length` bytes when `length > 0`.
    #[cfg(test)]
    pub(crate) unsafe fn from_raw(raw: &NDIlib_metadata_frame_t) -> Result<Self> {
        let layout = validate_metadata_layout(raw)?;
        Ok(Self::from_raw_validated(raw, layout))
    }

    /// Create from a raw NDI metadata frame after its layout has been validated.
    ///
    /// # Safety
    ///
    /// `layout` must have been produced by `validate_metadata_layout(raw)` while
    /// the same `raw.p_data` allocation is still valid.
    pub(crate) unsafe fn from_raw_validated(
        raw: &NDIlib_metadata_frame_t,
        layout: ValidatedMetadataLayout,
    ) -> Self {
        let bytes = metadata_payload_bytes(raw, layout);
        let data = str::from_utf8_unchecked(bytes).to_owned();

        Self {
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

/// Validated standalone metadata frame layout information.
///
/// The SDK `length` field includes the trailing NUL terminator. A
/// `len_with_nul` of zero represents the accepted empty null frame
/// (`length == 0 && p_data == NULL`) and never requires reading from `p_data`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidatedMetadataLayout {
    /// Total SDK metadata length including trailing NUL, or zero for the valid
    /// empty null frame.
    pub len_with_nul: usize,
    /// Public UTF-8 text payload length excluding the trailing NUL.
    pub text_len: usize,
}

/// Validate standalone metadata frame layout from raw FFI fields.
///
/// This validates only memory layout and UTF-8. XML well-formedness is
/// intentionally not checked because NDI receivers are expected to tolerate
/// badly formed XML metadata.
pub(crate) fn validate_metadata_layout(
    raw: &NDIlib_metadata_frame_t,
) -> Result<ValidatedMetadataLayout> {
    if raw.length < 0 {
        return Err(Error::InvalidFrame(format!(
            "Metadata frame has negative length: {}",
            raw.length
        )));
    }

    if raw.length == 0 {
        if raw.p_data.is_null() {
            return Ok(ValidatedMetadataLayout {
                len_with_nul: 0,
                text_len: 0,
            });
        }

        return Err(Error::InvalidFrame(
            "Metadata frame uses lengthless non-null C-string data".into(),
        ));
    }

    if raw.p_data.is_null() {
        return Err(Error::InvalidFrame(
            "Metadata frame has non-zero length with null data pointer".into(),
        ));
    }

    let len_with_nul = usize::try_from(raw.length).map_err(|_| {
        Error::InvalidFrame(format!("Invalid metadata length value: {}", raw.length))
    })?;
    validate_metadata_len_with_nul(len_with_nul)?;

    let bytes = unsafe { slice::from_raw_parts(raw.p_data.cast::<u8>(), len_with_nul) };
    if bytes[len_with_nul - 1] != 0 {
        return Err(Error::InvalidFrame(
            "Metadata frame length does not include a trailing NUL terminator".into(),
        ));
    }

    let payload = &bytes[..len_with_nul - 1];
    if payload.contains(&0) {
        return Err(Error::InvalidFrame(
            "Metadata frame contains an interior NUL byte".into(),
        ));
    }

    str::from_utf8(payload).map_err(|err| Error::InvalidUtf8(err.to_string()))?;

    Ok(ValidatedMetadataLayout {
        len_with_nul,
        text_len: payload.len(),
    })
}

fn validate_metadata_text(data: &str) -> Result<()> {
    let len_with_nul = data.len().checked_add(1).ok_or_else(|| {
        Error::InvalidFrame("Metadata length overflow while adding terminator".into())
    })?;
    validate_metadata_len_with_nul(len_with_nul)?;
    CString::new(data.as_bytes()).map_err(Error::InvalidCString)?;
    Ok(())
}

fn validate_metadata_len_with_nul(len_with_nul: usize) -> Result<i32> {
    if len_with_nul == 0 {
        return Err(Error::InvalidFrame(
            "Metadata length must include a trailing NUL terminator".into(),
        ));
    }

    if len_with_nul > MAX_METADATA_BYTES {
        return Err(Error::InvalidFrame(format!(
            "Metadata exceeds maximum size: {} bytes > {} bytes",
            len_with_nul, MAX_METADATA_BYTES
        )));
    }

    metadata_len_to_i32(len_with_nul)
}

fn metadata_len_to_i32(len_with_nul: usize) -> Result<i32> {
    i32::try_from(len_with_nul).map_err(|_| {
        Error::InvalidFrame(format!(
            "Metadata length {len_with_nul} exceeds SDK i32 range"
        ))
    })
}

fn metadata_payload_bytes(raw: &NDIlib_metadata_frame_t, layout: ValidatedMetadataLayout) -> &[u8] {
    if layout.text_len == 0 {
        &[]
    } else {
        // SAFETY: `validate_metadata_layout` checked that `p_data` is non-null
        // for non-empty payloads and that `text_len` is bounded by `length`.
        unsafe { slice::from_raw_parts(raw.p_data.cast::<u8>(), layout.text_len) }
    }
}

/// Owned video/audio per-frame metadata.
///
/// The wrapped C string is guaranteed to be valid UTF-8, contain no interior
/// NUL bytes, and fit within the crate metadata size cap including its trailing
/// terminator. `None` on a frame means a null SDK `p_metadata`; `Some("")`
/// means an explicit non-null empty C string.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct FrameMetadata {
    inner: CString,
}

impl FrameMetadata {
    pub(crate) fn new<S: Into<String>>(metadata: S) -> Result<Self> {
        let metadata = metadata.into();
        let len_with_nul = metadata.len().checked_add(1).ok_or_else(|| {
            Error::InvalidFrame("Frame metadata length overflow while adding terminator".into())
        })?;
        validate_metadata_len_with_nul(len_with_nul)?;

        Ok(Self {
            inner: CString::new(metadata).map_err(Error::InvalidCString)?,
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        self.inner
            .to_str()
            .expect("FrameMetadata validates UTF-8 at construction")
    }

    pub(crate) fn as_cstr(&self) -> &CStr {
        self.inner.as_c_str()
    }

    pub(crate) fn as_ptr(&self) -> *const c_char {
        self.inner.as_ptr()
    }

    /// Copy per-frame metadata from a pointer that has already been validated
    /// with [`validate_frame_metadata`].
    ///
    /// # Safety
    ///
    /// `metadata_layout` must have been produced for this exact `p_metadata`
    /// pointer while the pointed-to SDK allocation is still valid.
    pub(crate) unsafe fn copy_from_raw_validated(
        p_metadata: *const c_char,
        metadata_layout: ValidatedFrameMetadata,
    ) -> Option<Self> {
        metadata_layout.len_with_nul?;
        debug_assert!(!p_metadata.is_null());

        let mut bytes = if metadata_layout.text_len == 0 {
            Vec::with_capacity(1)
        } else {
            unsafe {
                slice::from_raw_parts(p_metadata.cast::<u8>(), metadata_layout.text_len).to_vec()
            }
        };
        bytes.push(0);

        let inner = unsafe { CString::from_vec_with_nul_unchecked(bytes) };
        Some(Self { inner })
    }
}

impl fmt::Debug for FrameMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FrameMetadata")
            .field(&self.as_str())
            .finish()
    }
}

/// Cached layout for video/audio per-frame `p_metadata`.
///
/// The SDK exposes these pointers as optional lengthless C strings. This type
/// records the first terminator found by a bounded scan so later accessors can
/// expose UTF-8 text without rescanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidatedFrameMetadata {
    /// `None` means the SDK pointer was null. `Some` includes the first trailing
    /// NUL terminator found within `MAX_METADATA_BYTES`.
    pub(crate) len_with_nul: Option<NonZeroUsize>,
    /// UTF-8 text payload length excluding the trailing NUL terminator.
    pub(crate) text_len: usize,
}

/// Validate a video/audio per-frame `p_metadata` pointer with a bounded scan.
///
/// This continues to trust the NDI SDK that a non-null pointer is readable. The
/// validation does not make dangling or otherwise invalid pointers safe; it
/// only limits the scan to `MAX_METADATA_BYTES`, requires a terminator within
/// that cap, and validates UTF-8 before exposing safe text access.
///
/// # Safety
///
/// When non-null, `p_metadata` must be readable byte-by-byte until either the
/// first NUL terminator or `MAX_METADATA_BYTES` bytes have been read.
pub(crate) unsafe fn validate_frame_metadata(
    p_metadata: *const c_char,
) -> Result<ValidatedFrameMetadata> {
    if p_metadata.is_null() {
        return Ok(ValidatedFrameMetadata {
            len_with_nul: None,
            text_len: 0,
        });
    }

    let metadata = p_metadata.cast::<u8>();
    for text_len in 0..MAX_METADATA_BYTES {
        let byte = unsafe { metadata.add(text_len).read() };
        if byte == 0 {
            if text_len > 0 {
                let payload = unsafe { slice::from_raw_parts(metadata, text_len) };
                str::from_utf8(payload).map_err(|err| Error::InvalidUtf8(err.to_string()))?;
            }

            return Ok(ValidatedFrameMetadata {
                len_with_nul: NonZeroUsize::new(text_len + 1),
                text_len,
            });
        }
    }

    Err(Error::InvalidFrame(format!(
        "Frame metadata is missing a NUL terminator within {MAX_METADATA_BYTES} bytes"
    )))
}

/// Expose validated per-frame metadata as borrowed UTF-8 text.
///
/// # Safety
///
/// `metadata_layout` must have been produced for this exact `p_metadata`
/// pointer while the pointed-to SDK allocation is still valid.
pub(crate) unsafe fn frame_metadata_str<'a>(
    p_metadata: *const c_char,
    metadata_layout: ValidatedFrameMetadata,
) -> Option<&'a str> {
    metadata_layout.len_with_nul?;
    debug_assert!(!p_metadata.is_null());

    if metadata_layout.text_len == 0 {
        return Some("");
    }

    let bytes = unsafe { slice::from_raw_parts(p_metadata.cast::<u8>(), metadata_layout.text_len) };
    Some(unsafe { str::from_utf8_unchecked(bytes) })
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

#[cfg(feature = "image-encoding")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageChannelOrder {
    Rgba,
    Bgra,
}

#[cfg(feature = "image-encoding")]
impl ImageChannelOrder {
    fn red_index(self) -> usize {
        match self {
            Self::Rgba => 0,
            Self::Bgra => 2,
        }
    }

    fn blue_index(self) -> usize {
        match self {
            Self::Rgba => 2,
            Self::Bgra => 0,
        }
    }
}

#[cfg(feature = "image-encoding")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageAlphaPolicy {
    Preserve,
    Opaque,
}

#[cfg(feature = "image-encoding")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImagePixelFormat {
    channel_order: ImageChannelOrder,
    alpha_policy: ImageAlphaPolicy,
}

#[cfg(feature = "image-encoding")]
impl ImagePixelFormat {
    const BYTES_PER_PIXEL: usize = 4;

    fn from_pixel_format(pixel_format: PixelFormat) -> Result<Self> {
        match pixel_format {
            PixelFormat::RGBA => Ok(Self {
                channel_order: ImageChannelOrder::Rgba,
                alpha_policy: ImageAlphaPolicy::Preserve,
            }),
            PixelFormat::BGRA => Ok(Self {
                channel_order: ImageChannelOrder::Bgra,
                alpha_policy: ImageAlphaPolicy::Preserve,
            }),
            PixelFormat::RGBX => Ok(Self {
                channel_order: ImageChannelOrder::Rgba,
                alpha_policy: ImageAlphaPolicy::Opaque,
            }),
            PixelFormat::BGRX => Ok(Self {
                channel_order: ImageChannelOrder::Bgra,
                alpha_policy: ImageAlphaPolicy::Opaque,
            }),
            _ => Err(Error::InvalidFrame(format!(
                "Unsupported format for image encoding: {pixel_format:?}. Only RGBA/RGBX/BGRA/BGRX are supported."
            ))),
        }
    }

    fn can_borrow_tightly_packed_png(self) -> bool {
        self.channel_order == ImageChannelOrder::Rgba
            && self.alpha_policy == ImageAlphaPolicy::Preserve
    }

    fn rgba_pixel(self, pixel: &[u8]) -> [u8; 4] {
        [
            pixel[self.channel_order.red_index()],
            pixel[1],
            pixel[self.channel_order.blue_index()],
            match self.alpha_policy {
                ImageAlphaPolicy::Preserve => pixel[3],
                ImageAlphaPolicy::Opaque => 255,
            },
        ]
    }

    fn rgb_pixel(self, pixel: &[u8]) -> [u8; 3] {
        [
            pixel[self.channel_order.red_index()],
            pixel[1],
            pixel[self.channel_order.blue_index()],
        ]
    }
}

#[cfg(feature = "image-encoding")]
#[derive(Debug)]
struct ImagePixelSource<'a> {
    data: &'a [u8],
    width: usize,
    height: usize,
    line_stride: usize,
    active_row_bytes: usize,
    pixel_format: ImagePixelFormat,
}

#[cfg(feature = "image-encoding")]
impl<'a> ImagePixelSource<'a> {
    fn new(layout: ValidatedVideoLayout, data: &'a [u8]) -> Result<Self> {
        let pixel_format = ImagePixelFormat::from_pixel_format(layout.pixel_format)?;

        if data.len() != layout.data_len_bytes {
            return Err(Error::InvalidFrame(format!(
                "Video data length {}, expected {} bytes for validated layout",
                data.len(),
                layout.data_len_bytes
            )));
        }

        let line_stride = match layout.line_stride_or_size {
            LineStrideOrSize::LineStrideBytes(stride) => {
                if stride <= 0 {
                    return Err(Error::InvalidFrame(format!(
                        "Image line stride must be positive, got {stride}"
                    )));
                }

                usize::try_from(stride).map_err(|_| {
                    Error::InvalidFrame(format!("Invalid image line stride value: {stride}"))
                })?
            }
            LineStrideOrSize::DataSizeBytes(size) => {
                return Err(Error::InvalidFrame(format!(
                    "Cannot encode image from data-size frame ({size} bytes). Image encoding requires line_stride_in_bytes."
                )));
            }
        };

        let width = usize::try_from(layout.width)
            .map_err(|_| Error::InvalidFrame(format!("Invalid image width: {}", layout.width)))?;
        let height = usize::try_from(layout.height)
            .map_err(|_| Error::InvalidFrame(format!("Invalid image height: {}", layout.height)))?;

        if width == 0 || height == 0 {
            return Err(Error::InvalidFrame(format!(
                "Image dimensions must be positive, got {}x{}",
                layout.width, layout.height
            )));
        }

        let active_row_bytes = width
            .checked_mul(ImagePixelFormat::BYTES_PER_PIXEL)
            .ok_or_else(|| {
                Error::InvalidFrame(format!(
                    "Image row size overflow for width {} and {} bytes per pixel",
                    width,
                    ImagePixelFormat::BYTES_PER_PIXEL
                ))
            })?;

        if line_stride < active_row_bytes {
            return Err(Error::InvalidFrame(format!(
                "Image line stride {line_stride} is smaller than active row size {active_row_bytes}"
            )));
        }

        let expected_data_len = line_stride.checked_mul(height).ok_or_else(|| {
            Error::InvalidFrame(format!(
                "Image data length overflow: {line_stride} stride x {height} height"
            ))
        })?;

        if expected_data_len != layout.data_len_bytes {
            return Err(Error::InvalidFrame(format!(
                "Image layout data length {} does not match line stride x height ({expected_data_len})",
                layout.data_len_bytes
            )));
        }

        Ok(Self {
            data,
            width,
            height,
            line_stride,
            active_row_bytes,
            pixel_format,
        })
    }

    fn png_rgba_input(&self) -> Result<(Cow<'a, [u8]>, u32, u32)> {
        let width = u32::try_from(self.width).map_err(|_| {
            Error::InvalidFrame(format!("PNG width {} exceeds u32 range", self.width))
        })?;
        let height = u32::try_from(self.height).map_err(|_| {
            Error::InvalidFrame(format!("PNG height {} exceeds u32 range", self.height))
        })?;

        if self.pixel_format.can_borrow_tightly_packed_png()
            && self.line_stride == self.active_row_bytes
        {
            return Ok((Cow::Borrowed(self.data), width, height));
        }

        let mut rgba = Vec::with_capacity(self.output_len(4)?);

        if self.pixel_format.can_borrow_tightly_packed_png() {
            for row in self.active_rows() {
                rgba.extend_from_slice(row);
            }
        } else {
            for row in self.active_rows() {
                for pixel in row.chunks_exact(ImagePixelFormat::BYTES_PER_PIXEL) {
                    rgba.extend_from_slice(&self.pixel_format.rgba_pixel(pixel));
                }
            }
        }

        Ok((Cow::Owned(rgba), width, height))
    }

    fn jpeg_rgb_input(&self, quality: u8) -> Result<(Vec<u8>, u16, u16)> {
        if !(1..=100).contains(&quality) {
            return Err(Error::InvalidFrame(format!(
                "JPEG quality must be in 1..=100, got {quality}"
            )));
        }

        let width = u16::try_from(self.width).map_err(|_| {
            Error::InvalidFrame(format!(
                "JPEG width {} exceeds maximum supported value {}",
                self.width,
                u16::MAX
            ))
        })?;
        let height = u16::try_from(self.height).map_err(|_| {
            Error::InvalidFrame(format!(
                "JPEG height {} exceeds maximum supported value {}",
                self.height,
                u16::MAX
            ))
        })?;

        let mut rgb = Vec::with_capacity(self.output_len(3)?);
        for row in self.active_rows() {
            for pixel in row.chunks_exact(ImagePixelFormat::BYTES_PER_PIXEL) {
                rgb.extend_from_slice(&self.pixel_format.rgb_pixel(pixel));
            }
        }

        Ok((rgb, width, height))
    }

    fn output_len(&self, channels: usize) -> Result<usize> {
        self.width
            .checked_mul(self.height)
            .and_then(|pixels| pixels.checked_mul(channels))
            .ok_or_else(|| {
                Error::InvalidFrame(format!(
                    "Image output buffer size overflow: {}x{}x{}",
                    self.width, self.height, channels
                ))
            })
    }

    fn active_rows(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        (0..self.height).map(move |row| {
            let start = row * self.line_stride;
            let end = start + self.active_row_bytes;
            &self.data[start..end]
        })
    }
}

// ============================================================================
// Frame layout validation helpers
// ============================================================================

/// Validated video frame layout information.
///
/// This struct holds pre-validated layout information for a video frame,
/// including the computed buffer length and stride/size information.
/// Creating this struct performs all necessary bounds checking, so
/// consumers can safely use the cached values without re-validation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ValidatedVideoLayout {
    /// The validated width in pixels.
    pub width: i32,
    /// The validated height in pixels.
    pub height: i32,
    /// The validated pixel format.
    pub pixel_format: PixelFormat,
    /// The validated data buffer length in bytes.
    pub data_len_bytes: usize,
    /// The validated SDK union field. Supported safe video layouts always use
    /// line stride; the data-size variant is reserved for the unsafe escape
    /// hatch.
    pub line_stride_or_size: LineStrideOrSize,
}

impl ValidatedVideoLayout {
    pub(crate) fn new_uncompressed(
        pixel_format: PixelFormat,
        width: i32,
        height: i32,
        line_stride: Option<i32>,
    ) -> Result<Self> {
        validate_video_dimensions_for_format(pixel_format, width, height)?;

        let width_usize = usize::try_from(width)
            .map_err(|_| Error::InvalidFrame(format!("Invalid width value: {width}")))?;
        let height_usize = usize::try_from(height)
            .map_err(|_| Error::InvalidFrame(format!("Invalid height value: {height}")))?;

        let min_stride = min_video_line_stride_checked(pixel_format, width_usize)?;
        let line_stride_usize = match line_stride {
            Some(stride) => {
                if stride <= 0 {
                    return Err(Error::InvalidFrame(format!(
                        "Uncompressed video frame has invalid line_stride_in_bytes: {stride}"
                    )));
                }

                let stride_usize = usize::try_from(stride).map_err(|_| {
                    Error::InvalidFrame(format!("Invalid line_stride_in_bytes value: {stride}"))
                })?;

                if stride_usize < min_stride {
                    return Err(Error::InvalidFrame(format!(
                        "Video line_stride_in_bytes {stride} is smaller than minimum row size {min_stride} for {pixel_format:?} width {width}"
                    )));
                }

                stride_usize
            }
            None => min_stride,
        };

        if pixel_format.info().is_planar_420() && !line_stride_usize.is_multiple_of(2) {
            return Err(Error::InvalidFrame(format!(
                "Planar 4:2:0 video frame has odd line_stride_in_bytes: {line_stride_usize}"
            )));
        }

        let data_len_bytes =
            calculate_buffer_len_checked(pixel_format, line_stride_usize, height_usize)?;
        validate_video_data_len(data_len_bytes)?;

        let line_stride_i32 = i32::try_from(line_stride_usize).map_err(|_| {
            Error::InvalidFrame(format!(
                "Video line stride {line_stride_usize} exceeds i32 range"
            ))
        })?;

        Ok(Self {
            width,
            height,
            pixel_format,
            data_len_bytes,
            line_stride_or_size: LineStrideOrSize::LineStrideBytes(line_stride_i32),
        })
    }
}

/// Validate video frame layout from raw FFI fields.
///
/// This function performs all necessary validation including:
/// - Null pointer check for `p_data`
/// - Valid pixel format (FourCC) conversion
/// - Checked arithmetic for buffer size calculation
/// - `MAX_VIDEO_BYTES` cap enforcement
///
/// # Arguments
///
/// * `raw` - Reference to the raw NDI video frame
///
/// # Returns
///
/// `Ok(ValidatedVideoLayout)` if the frame is valid, or `Err(Error::InvalidFrame(...))` otherwise.
pub(crate) fn validate_video_layout(raw: &NDIlib_video_frame_v2_t) -> Result<ValidatedVideoLayout> {
    if raw.p_data.is_null() {
        return Err(Error::InvalidFrame(
            "Video frame has null data pointer".into(),
        ));
    }

    validate_video_frame_metadata(raw.frame_rate_N, raw.frame_rate_D, raw.picture_aspect_ratio)?;

    #[allow(clippy::unnecessary_cast)]
    ScanType::try_from(raw.frame_format_type as u32).map_err(|_| {
        Error::InvalidFrame(format!(
            "Unknown scan type: 0x{:08X}",
            raw.frame_format_type
        ))
    })?;

    #[allow(clippy::unnecessary_cast)] // Required for Windows where FourCC is i32
    let pixel_format = PixelFormat::try_from(raw.FourCC as u32).map_err(|_| {
        Error::InvalidFrame(format!("Unknown pixel format FourCC: 0x{:08X}", raw.FourCC))
    })?;

    let line_stride = unsafe { raw.__bindgen_anon_1.line_stride_in_bytes };
    ValidatedVideoLayout::new_uncompressed(pixel_format, raw.xres, raw.yres, Some(line_stride))
}

fn validate_video_dimensions_for_format(
    pixel_format: PixelFormat,
    width: i32,
    height: i32,
) -> Result<()> {
    validate_video_width_for_format(pixel_format, width)?;
    if height <= 0 {
        return Err(Error::InvalidFrame(format!(
            "Video frame has invalid height: {height}"
        )));
    }

    if pixel_format.info().is_planar_420() && (width % 2 != 0 || height % 2 != 0) {
        return Err(Error::InvalidFrame(format!(
            "Planar 4:2:0 video frames require even dimensions, got {}x{}",
            width, height
        )));
    }

    Ok(())
}

fn validate_video_width_for_format(pixel_format: PixelFormat, width: i32) -> Result<()> {
    if width <= 0 {
        return Err(Error::InvalidFrame(format!(
            "Video frame has invalid width: {width}"
        )));
    }

    if pixel_format.info().is_planar_420() && width % 2 != 0 {
        return Err(Error::InvalidFrame(format!(
            "Planar 4:2:0 video frames require even width, got {width}"
        )));
    }

    Ok(())
}

pub(crate) fn validate_video_frame_metadata(
    frame_rate_n: i32,
    frame_rate_d: i32,
    picture_aspect_ratio: f32,
) -> Result<()> {
    if frame_rate_n <= 0 {
        return Err(Error::InvalidFrame(format!(
            "Video frame has invalid frame rate numerator: {frame_rate_n}"
        )));
    }
    if frame_rate_d <= 0 {
        return Err(Error::InvalidFrame(format!(
            "Video frame has invalid frame rate denominator: {frame_rate_d}"
        )));
    }
    if !picture_aspect_ratio.is_finite() || picture_aspect_ratio <= 0.0 {
        return Err(Error::InvalidFrame(format!(
            "Video frame has invalid picture aspect ratio: {picture_aspect_ratio}"
        )));
    }

    Ok(())
}

fn validate_video_data_len(data_len_bytes: usize) -> Result<()> {
    if data_len_bytes == 0 {
        return Err(Error::InvalidFrame(
            "Video frame has zero calculated size".into(),
        ));
    }

    if data_len_bytes > MAX_VIDEO_BYTES {
        return Err(Error::InvalidFrame(format!(
            "Video frame exceeds maximum size: {} bytes > {} bytes",
            data_len_bytes, MAX_VIDEO_BYTES
        )));
    }

    Ok(())
}

fn min_video_line_stride_checked(pixel_format: PixelFormat, width: usize) -> Result<usize> {
    width
        .checked_mul(pixel_format.info().bytes_per_pixel() as usize)
        .ok_or_else(|| {
            Error::InvalidFrame(format!(
                "Video line stride overflow for {:?} width {}",
                pixel_format, width
            ))
        })
}

/// Calculate buffer length with checked arithmetic.
fn calculate_buffer_len_checked(
    pixel_format: PixelFormat,
    y_stride: usize,
    height: usize,
) -> Result<usize> {
    calculate_buffer_len_for_info_checked(pixel_format.info(), y_stride, height)
}

fn calculate_buffer_len_for_info_checked(
    info: PixelFormatInfo,
    y_stride: usize,
    height: usize,
) -> Result<usize> {
    // Y plane size = y_stride * height
    let y_size = y_stride.checked_mul(height).ok_or_else(|| {
        Error::InvalidFrame(format!(
            "Video buffer size overflow: {} stride × {} height",
            y_stride, height
        ))
    })?;

    match info.category() {
        FormatCategory::Packed => Ok(y_size),
        FormatCategory::Planar420 => {
            if !height.is_multiple_of(2) || !y_stride.is_multiple_of(2) {
                return Err(Error::InvalidFrame(
                    "Planar 4:2:0 video frames require even height and stride".into(),
                ));
            }
            // Planar 4:2:0: Y + U + V
            // U and V planes each have half width and half height
            let chroma_height = height / 2;
            let u_stride = y_stride / 2;
            let v_stride = y_stride / 2;

            let u_size = u_stride
                .checked_mul(chroma_height)
                .ok_or_else(|| Error::InvalidFrame("Video U-plane size overflow".into()))?;
            let v_size = v_stride
                .checked_mul(chroma_height)
                .ok_or_else(|| Error::InvalidFrame("Video V-plane size overflow".into()))?;

            let total = y_size
                .checked_add(u_size)
                .and_then(|s| s.checked_add(v_size))
                .ok_or_else(|| Error::InvalidFrame("Video total buffer size overflow".into()))?;

            Ok(total)
        }
        FormatCategory::SemiPlanar420 => {
            if !height.is_multiple_of(2) {
                return Err(Error::InvalidFrame(
                    "Semi-planar 4:2:0 video frames require even height".into(),
                ));
            }
            // Semi-planar 4:2:0: Y + interleaved UV
            // UV plane has full width and half height
            let chroma_height = height / 2;
            let uv_size = y_stride
                .checked_mul(chroma_height)
                .ok_or_else(|| Error::InvalidFrame("Video UV-plane size overflow".into()))?;

            let total = y_size
                .checked_add(uv_size)
                .ok_or_else(|| Error::InvalidFrame("Video total buffer size overflow".into()))?;

            Ok(total)
        }
    }
}

/// Validated audio frame layout information.
///
/// This struct holds pre-validated layout information for an audio frame,
/// including the channel stride and computed backing sample count. Creating
/// this struct performs all necessary bounds checking, so consumers can safely
/// use the cached values without re-validation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ValidatedAudioLayout {
    /// The validated audio format, or `None` for a query/no-source empty state.
    pub format: Option<AudioFormat>,
    /// The validated sample rate.
    pub sample_rate: i32,
    /// The validated channel count.
    pub no_channels: usize,
    /// The validated samples per channel.
    pub no_samples: usize,
    /// The validated channel stride in bytes.
    pub channel_stride_in_bytes: i32,
    /// The validated channel stride in `f32` samples.
    pub channel_stride_samples: usize,
    /// The validated backing sample count. For strided planar audio this may be
    /// larger than `no_channels * no_samples` because it includes inter-channel
    /// padding up to the last channel's final sample.
    pub sample_count: usize,
}

impl ValidatedAudioLayout {
    pub(crate) fn is_empty(self) -> bool {
        self.sample_count == 0
    }

    pub(crate) fn format(self) -> Option<AudioFormat> {
        self.format
    }

    pub(crate) fn channel_range(self, channel: usize) -> Option<std::ops::Range<usize>> {
        if self.is_empty() || channel >= self.no_channels {
            return None;
        }

        let start = channel.checked_mul(self.channel_stride_samples)?;
        let end = start.checked_add(self.no_samples)?;

        (end <= self.sample_count).then_some(start..end)
    }
}

fn validate_outbound_audio_layout(
    sample_rate: i32,
    no_channels: i32,
    no_samples: i32,
    format: AudioFormat,
) -> Result<ValidatedAudioLayout> {
    if sample_rate <= 0 {
        return Err(Error::InvalidFrame(format!(
            "Invalid sample rate: {sample_rate}"
        )));
    }
    if no_channels <= 0 {
        return Err(Error::InvalidFrame(format!(
            "Invalid number of channels: {no_channels}"
        )));
    }
    if no_samples <= 0 {
        return Err(Error::InvalidFrame(format!(
            "Invalid number of samples: {no_samples}"
        )));
    }

    validate_audio_format(format.into())?;

    let no_channels = usize::try_from(no_channels)
        .map_err(|_| Error::InvalidFrame(format!("Invalid no_channels value: {no_channels}")))?;
    let no_samples = usize::try_from(no_samples)
        .map_err(|_| Error::InvalidFrame(format!("Invalid no_samples value: {no_samples}")))?;

    let channel_stride_bytes = no_samples
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| {
            Error::InvalidFrame(format!(
                "Audio channel stride overflow: {} samples × {} bytes",
                no_samples,
                std::mem::size_of::<f32>()
            ))
        })?;
    let channel_stride_in_bytes = i32::try_from(channel_stride_bytes).map_err(|_| {
        Error::InvalidFrame(format!(
            "Audio channel stride {channel_stride_bytes} exceeds i32 range"
        ))
    })?;

    let sample_count = no_channels.checked_mul(no_samples).ok_or_else(|| {
        Error::InvalidFrame(format!(
            "Audio sample count overflow: {no_channels} channels × {no_samples} samples"
        ))
    })?;
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

    Ok(ValidatedAudioLayout {
        format: Some(format),
        sample_rate,
        no_channels,
        no_samples,
        channel_stride_in_bytes,
        channel_stride_samples: no_samples,
        sample_count,
    })
}

/// Validate audio frame layout from raw FFI fields.
///
/// This function performs all necessary validation including:
/// - Null pointer check for `p_data`
/// - Valid audio format (FourCC) conversion
/// - Positive sample rate, channel count, and sample count
/// - Checked arithmetic for sample count multiplication
/// - `MAX_AUDIO_BYTES` cap enforcement
///
/// # Arguments
///
/// * `raw` - Reference to the raw NDI audio frame
///
/// # Returns
///
/// `Ok(ValidatedAudioLayout)` if the frame is valid, or `Err(Error::InvalidFrame(...))` otherwise.
pub(crate) fn validate_audio_layout(raw: &NDIlib_audio_frame_v3_t) -> Result<ValidatedAudioLayout> {
    validate_audio_layout_inner(raw, false)
}

/// Validate a FrameSync audio frame that is allowed to be a documented
/// query/no-source zero-length state.
pub(crate) fn validate_audio_layout_allow_empty(
    raw: &NDIlib_audio_frame_v3_t,
) -> Result<ValidatedAudioLayout> {
    validate_audio_layout_inner(raw, true)
}

fn validate_audio_layout_inner(
    raw: &NDIlib_audio_frame_v3_t,
    allow_empty: bool,
) -> Result<ValidatedAudioLayout> {
    if raw.no_samples == 0 {
        if allow_empty {
            return validate_empty_audio_layout(raw);
        }

        return Err(Error::InvalidFrame("Invalid number of samples: 0".into()));
    }

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

    let format = validate_audio_format(raw.FourCC)?;
    let channel_stride_in_bytes = unsafe { raw.__bindgen_anon_1.channel_stride_in_bytes };
    if channel_stride_in_bytes <= 0 {
        return Err(Error::InvalidFrame(format!(
            "Invalid channel_stride_in_bytes: {}",
            channel_stride_in_bytes
        )));
    }

    // Use checked math to prevent overflow when computing sample count
    let no_samples = usize::try_from(raw.no_samples).map_err(|_| {
        Error::InvalidFrame(format!("Invalid no_samples value: {}", raw.no_samples))
    })?;

    let no_channels = usize::try_from(raw.no_channels).map_err(|_| {
        Error::InvalidFrame(format!("Invalid no_channels value: {}", raw.no_channels))
    })?;

    let channel_stride_bytes = usize::try_from(channel_stride_in_bytes).map_err(|_| {
        Error::InvalidFrame(format!(
            "Invalid channel_stride_in_bytes value: {}",
            channel_stride_in_bytes
        ))
    })?;

    if channel_stride_bytes % std::mem::size_of::<f32>() != 0 {
        return Err(Error::InvalidFrame(format!(
            "channel_stride_in_bytes must be a multiple of {}, got {}",
            std::mem::size_of::<f32>(),
            channel_stride_in_bytes
        )));
    }

    let minimum_channel_stride = no_samples
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| {
            Error::InvalidFrame(format!(
                "Audio channel stride overflow: {} samples × {} bytes",
                no_samples,
                std::mem::size_of::<f32>()
            ))
        })?;

    if channel_stride_bytes < minimum_channel_stride {
        return Err(Error::InvalidFrame(format!(
            "channel_stride_in_bytes {} is smaller than one channel of audio samples {}",
            channel_stride_in_bytes, minimum_channel_stride
        )));
    }

    let channel_stride_samples = channel_stride_bytes / std::mem::size_of::<f32>();
    let last_channel_offset = no_channels
        .checked_sub(1)
        .and_then(|last| last.checked_mul(channel_stride_samples))
        .ok_or_else(|| {
            Error::InvalidFrame(format!(
                "Audio channel offset overflow: {} channels × {} stride samples",
                no_channels, channel_stride_samples
            ))
        })?;

    let sample_count = last_channel_offset.checked_add(no_samples).ok_or_else(|| {
        Error::InvalidFrame(format!(
            "Audio backing sample count overflow: channel offset {} + {} samples",
            last_channel_offset, no_samples
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

    Ok(ValidatedAudioLayout {
        format: Some(format),
        sample_rate: raw.sample_rate,
        no_channels,
        no_samples,
        channel_stride_in_bytes,
        channel_stride_samples,
        sample_count,
    })
}

fn validate_empty_audio_layout(raw: &NDIlib_audio_frame_v3_t) -> Result<ValidatedAudioLayout> {
    if !raw.p_data.is_null() {
        return Err(Error::InvalidFrame(
            "Zero-length audio frame has non-null data pointer".into(),
        ));
    }

    if raw.sample_rate < 0 {
        return Err(Error::InvalidFrame(format!(
            "Invalid sample rate: {}",
            raw.sample_rate
        )));
    }

    if raw.no_channels < 0 {
        return Err(Error::InvalidFrame(format!(
            "Invalid number of channels: {}",
            raw.no_channels
        )));
    }

    let channel_stride_in_bytes = unsafe { raw.__bindgen_anon_1.channel_stride_in_bytes };
    if channel_stride_in_bytes != 0 {
        return Err(Error::InvalidFrame(format!(
            "Zero-length audio frame has non-zero channel_stride_in_bytes: {}",
            channel_stride_in_bytes
        )));
    }

    let no_source = raw.sample_rate == 0 && raw.no_channels == 0;
    let query_format = raw.sample_rate > 0 && raw.no_channels > 0;
    if !no_source && !query_format {
        return Err(Error::InvalidFrame(format!(
            "Invalid zero-length audio query state: sample_rate={}, no_channels={}",
            raw.sample_rate, raw.no_channels
        )));
    }

    let format = if no_source && raw.FourCC == 0 {
        None
    } else {
        Some(validate_audio_format(raw.FourCC)?)
    };

    let no_channels = usize::try_from(raw.no_channels).map_err(|_| {
        Error::InvalidFrame(format!("Invalid no_channels value: {}", raw.no_channels))
    })?;

    Ok(ValidatedAudioLayout {
        format,
        sample_rate: raw.sample_rate,
        no_channels,
        no_samples: 0,
        channel_stride_in_bytes: 0,
        channel_stride_samples: 0,
        sample_count: 0,
    })
}

fn validate_audio_format(fourcc: NDIlib_FourCC_audio_type_e) -> Result<AudioFormat> {
    match fourcc {
        NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP => Ok(AudioFormat::FLTP),
        _ => Err(Error::InvalidFrame(format!(
            "Unknown audio format FourCC: 0x{:08X}",
            fourcc
        ))),
    }
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
    /// Cached validated layout information (pixel format, data length, stride/size).
    /// Computed once at construction time; `data()` uses this cached value.
    layout: ValidatedVideoLayout,
    metadata: ValidatedFrameMetadata,
}

impl<'rx> VideoFrameRef<'rx> {
    /// Create a borrowed video frame from an RAII guard.
    ///
    /// Validates the frame layout including:
    /// - Valid pixel format (FourCC)
    /// - Non-null data pointer
    /// - Valid dimensions and stride
    /// - Buffer size within `MAX_VIDEO_BYTES` limit
    ///
    /// The validated layout is cached so that `data()` can return slices
    /// without re-computation or unchecked arithmetic.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI receiver
    /// and contains a frame populated by `NDIlib_recv_capture_v3`.
    pub(crate) unsafe fn new(guard: RecvVideoGuard<'rx>) -> Result<Self> {
        let layout = validate_video_layout(guard.frame())?;
        let metadata = unsafe { validate_frame_metadata(guard.frame().p_metadata)? };

        Ok(Self {
            guard,
            layout,
            metadata,
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
        self.layout.pixel_format
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
    /// This is guaranteed to be valid since it is checked during construction.
    pub fn scan_type(&self) -> ScanType {
        #[allow(clippy::unnecessary_cast)]
        ScanType::try_from(self.guard.frame().frame_format_type as u32)
            .expect("VideoFrameRef validates scan type during construction")
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
    ///
    /// This returns the cached, validated value computed at construction time.
    pub fn line_stride_or_size(&self) -> LineStrideOrSize {
        self.layout.line_stride_or_size
    }

    /// Get frame metadata as UTF-8 text, if present.
    pub fn metadata(&self) -> Option<&str> {
        unsafe { frame_metadata_str(self.guard.frame().p_metadata, self.metadata) }
    }

    /// Get a zero-copy view of the frame data.
    ///
    /// This returns a slice directly into the NDI SDK's buffer.
    /// No allocation or memcpy is performed.
    ///
    /// For planar 4:2:0 formats (YV12/I420/NV12), this returns the full
    /// buffer including Y and UV planes.
    ///
    /// # Safety Guarantee
    ///
    /// The slice length is computed once at construction time using checked
    /// arithmetic and validated against `MAX_VIDEO_BYTES`. This eliminates
    /// the possibility of integer overflow or unbounded slice creation.
    pub fn data(&self) -> &[u8] {
        // SAFETY: The data pointer was validated as non-null during construction
        // (validate_video_layout returns Err if p_data is null).
        // The data length was computed with checked arithmetic and validated
        // against MAX_VIDEO_BYTES, so it's safe to create this slice.
        unsafe { slice::from_raw_parts(self.guard.frame().p_data, self.layout.data_len_bytes) }
    }

    /// Convert this borrowed frame to an owned `VideoFrame`.
    ///
    /// This performs a single memcpy of the frame data and metadata,
    /// allowing the frame to outlive the NDI buffer and be sent across threads.
    pub fn to_owned(&self) -> Result<VideoFrame> {
        unsafe { VideoFrame::from_raw_validated(self.guard.frame(), self.layout, self.metadata) }
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
    /// Cached validated layout information (format, sample count).
    /// Computed once at construction time; `data()` uses this cached value.
    layout: ValidatedAudioLayout,
    metadata: ValidatedFrameMetadata,
}

impl<'rx> AudioFrameRef<'rx> {
    /// Create a borrowed audio frame from an RAII guard.
    ///
    /// Validates the frame layout including:
    /// - Valid audio format (FourCC)
    /// - Non-null data pointer
    /// - Valid sample rate, channel count, and sample count
    /// - Buffer size within `MAX_AUDIO_BYTES` limit
    ///
    /// The validated layout is cached so that `data()` can return slices
    /// without re-computation or unchecked arithmetic.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI receiver
    /// and contains a frame populated by `NDIlib_recv_capture_v3`.
    pub(crate) unsafe fn new(guard: RecvAudioGuard<'rx>) -> Result<Self> {
        let layout = validate_audio_layout(guard.frame())?;
        let metadata = unsafe { validate_frame_metadata(guard.frame().p_metadata)? };

        Ok(Self {
            guard,
            layout,
            metadata,
        })
    }

    /// Get the sample rate in Hz.
    pub fn sample_rate(&self) -> i32 {
        self.layout.sample_rate
    }

    /// Get the number of audio channels.
    pub fn num_channels(&self) -> i32 {
        self.layout.no_channels as i32
    }

    /// Get the number of samples per channel.
    pub fn num_samples(&self) -> i32 {
        self.layout.no_samples as i32
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
        self.layout
            .format()
            .expect("validate_audio_layout requires a concrete audio format")
    }

    /// Get the channel stride in bytes.
    pub fn channel_stride_in_bytes(&self) -> i32 {
        self.layout.channel_stride_in_bytes
    }

    /// Get frame metadata as UTF-8 text, if present.
    pub fn metadata(&self) -> Option<&str> {
        unsafe { frame_metadata_str(self.guard.frame().p_metadata, self.metadata) }
    }

    /// Get a zero-copy view of the audio data as 32-bit floats.
    ///
    /// This returns a slice directly into the NDI SDK's buffer.
    /// No allocation or memcpy is performed.
    ///
    /// # Safety Guarantee
    ///
    /// The slice length is computed once at construction time using checked
    /// arithmetic and validated against `MAX_AUDIO_BYTES`. This eliminates
    /// the possibility of integer overflow or unbounded slice creation.
    pub fn data(&self) -> &[f32] {
        // SAFETY: The data pointer was validated as non-null during construction
        // (validate_audio_layout returns Err if p_data is null).
        // The sample count was computed with checked arithmetic and validated
        // against MAX_AUDIO_BYTES, so it's safe to create this slice.
        unsafe {
            slice::from_raw_parts(
                self.guard.frame().p_data as *const f32,
                self.layout.sample_count,
            )
        }
    }

    /// Get the zero-copy samples for a single channel.
    ///
    /// This respects `channel_stride_in_bytes`, so it works for tightly packed
    /// and strided planar FLTP audio.
    pub fn channel_data(&self, channel: usize) -> Option<&[f32]> {
        let range = self.layout.channel_range(channel)?;
        Some(&self.data()[range])
    }

    /// Convert this borrowed frame to an owned `AudioFrame`.
    ///
    /// This performs a single memcpy of the audio data and metadata,
    /// allowing the frame to outlive the NDI buffer and be sent across threads.
    pub fn to_owned(&self) -> Result<AudioFrame> {
        AudioFrame::from_raw_validated(*self.guard.frame(), self.layout, self.metadata)
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
///     println!("Metadata: {}", frame.data());
///
///     // Frame is freed here when `frame` goes out of scope
/// }
/// # Ok(())
/// # }
/// ```
pub struct MetadataFrameRef<'rx> {
    guard: RecvMetadataGuard<'rx>,
    /// Cached validated layout information. Computed once at construction time;
    /// `data()` and `as_bytes()` use this cached value.
    layout: ValidatedMetadataLayout,
}

impl<'rx> MetadataFrameRef<'rx> {
    /// Create a borrowed metadata frame from an RAII guard.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI receiver
    /// and contains a frame populated by `NDIlib_recv_capture_v3`.
    pub(crate) unsafe fn new(guard: RecvMetadataGuard<'rx>) -> Result<Self> {
        let layout = validate_metadata_layout(guard.frame())?;

        Ok(Self { guard, layout })
    }

    /// Get the timecode.
    pub fn timecode(&self) -> i64 {
        self.guard.frame().timecode
    }

    /// Get a zero-copy view of the metadata text.
    ///
    /// This returns a reference directly into the NDI SDK's buffer.
    /// No allocation or string copying is performed.
    pub fn data(&self) -> &str {
        let bytes = self.as_bytes();
        // SAFETY: `validate_metadata_layout` checked UTF-8 before this
        // borrowed frame was constructed.
        unsafe { str::from_utf8_unchecked(bytes) }
    }

    /// Get a zero-copy view of the metadata UTF-8 payload bytes, excluding the
    /// SDK trailing NUL terminator.
    pub fn as_bytes(&self) -> &[u8] {
        metadata_payload_bytes(self.guard.frame(), self.layout)
    }

    /// Convert this borrowed frame to an owned `MetadataFrame`.
    ///
    /// This performs a string copy, allowing the frame to outlive
    /// the NDI buffer and be sent across threads.
    pub fn to_owned(&self) -> MetadataFrame {
        unsafe { MetadataFrame::from_raw_validated(self.guard.frame(), self.layout) }
    }
}

impl<'rx> fmt::Debug for MetadataFrameRef<'rx> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetadataFrameRef")
            .field("data", &self.data())
            .field("data (bytes)", &self.as_bytes().len())
            .field("timecode", &self.timecode())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "image-encoding")]
    fn image_test_frame(
        pixel_format: PixelFormat,
        width: i32,
        height: i32,
        line_stride: Option<i32>,
        data: Vec<u8>,
    ) -> VideoFrame {
        let layout =
            ValidatedVideoLayout::new_uncompressed(pixel_format, width, height, line_stride)
                .unwrap();
        assert_eq!(data.len(), layout.data_len_bytes);

        VideoFrame {
            layout,
            frame_rate_n: 60,
            frame_rate_d: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            scan_type: ScanType::Progressive,
            timecode: 0,
            data,
            metadata: None,
            timestamp: 0,
        }
    }

    #[cfg(feature = "image-encoding")]
    fn decode_png_rgba(png_bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
        let mut reader = decoder.read_info().unwrap();
        let output_size = reader.output_buffer_size().unwrap();
        let mut output = vec![0; output_size];
        let info = reader.next_frame(&mut output).unwrap();
        output.truncate(info.buffer_size());

        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);

        (info.width, info.height, output)
    }

    #[cfg(feature = "image-encoding")]
    fn assert_jpeg_markers(jpeg_bytes: &[u8]) {
        assert!(jpeg_bytes.len() >= 4);
        assert_eq!(&jpeg_bytes[..2], &[0xFF, 0xD8]);
        assert_eq!(&jpeg_bytes[jpeg_bytes.len() - 2..], &[0xFF, 0xD9]);
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn test_encode_png_decodes_exact_supported_pixels() {
        let cases = [
            (
                PixelFormat::RGBA,
                vec![10, 20, 30, 40, 50, 60, 70, 80],
                vec![10, 20, 30, 40, 50, 60, 70, 80],
            ),
            (
                PixelFormat::BGRA,
                vec![30, 20, 10, 40, 70, 60, 50, 80],
                vec![10, 20, 30, 40, 50, 60, 70, 80],
            ),
            (
                PixelFormat::RGBX,
                vec![10, 20, 30, 0, 50, 60, 70, 99],
                vec![10, 20, 30, 255, 50, 60, 70, 255],
            ),
            (
                PixelFormat::BGRX,
                vec![30, 20, 10, 0, 70, 60, 50, 99],
                vec![10, 20, 30, 255, 50, 60, 70, 255],
            ),
        ];

        for (pixel_format, data, expected) in cases {
            let frame = image_test_frame(pixel_format, 2, 1, None, data);
            let png = frame.encode_png().unwrap();
            let (width, height, decoded) = decode_png_rgba(&png);

            assert_eq!((width, height), (2, 1), "{pixel_format:?}");
            assert_eq!(decoded, expected, "{pixel_format:?}");
        }
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn test_encode_png_skips_padded_rows() {
        let data = vec![
            1, 2, 3, 0, 4, 5, 6, 0, 200, 201, 202, 203, 7, 8, 9, 0, 10, 11, 12, 0, 204, 205, 206,
            207,
        ];
        let frame = image_test_frame(PixelFormat::RGBX, 2, 2, Some(12), data);

        let png = frame.encode_png().unwrap();
        let (width, height, decoded) = decode_png_rgba(&png);

        assert_eq!((width, height), (2, 2));
        assert_eq!(
            decoded,
            vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255,]
        );
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn test_encode_jpeg_accepts_supported_image_formats() {
        let cases = [
            (PixelFormat::RGBA, vec![10, 20, 30, 40]),
            (PixelFormat::BGRA, vec![30, 20, 10, 40]),
            (PixelFormat::RGBX, vec![10, 20, 30, 0]),
            (PixelFormat::BGRX, vec![30, 20, 10, 0]),
        ];

        for (pixel_format, pixel) in cases {
            let data = pixel.repeat(16);
            let frame = image_test_frame(pixel_format, 4, 4, None, data);
            let jpeg = frame.encode_jpeg(90).unwrap();
            assert_jpeg_markers(&jpeg);
        }
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn test_encode_jpeg_skips_padded_rows() {
        let data = vec![
            3, 2, 1, 0, 6, 5, 4, 0, 250, 251, 252, 253, 9, 8, 7, 0, 12, 11, 10, 0, 254, 255, 0, 1,
        ];
        let frame = image_test_frame(PixelFormat::BGRX, 2, 2, Some(12), data);

        let jpeg = frame.encode_jpeg(85).unwrap();
        assert_jpeg_markers(&jpeg);
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn test_encode_jpeg_rejects_invalid_quality() {
        let frame = image_test_frame(PixelFormat::RGBA, 2, 2, None, vec![0; 16]);

        for quality in [0, 101, 255] {
            let err = frame.encode_jpeg(quality).unwrap_err();
            let err_msg = err.to_string();
            assert!(
                matches!(&err, Error::InvalidFrame(message) if message.contains("quality")),
                "unexpected error for quality {quality}: {err_msg}"
            );
        }
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn test_encode_jpeg_rejects_oversized_dimensions_before_casting() {
        let wide = image_test_frame(PixelFormat::RGBA, 65_536, 1, None, vec![0; 65_536 * 4]);
        let err = wide.encode_jpeg(85).unwrap_err();
        let err_msg = err.to_string();
        assert!(
            matches!(&err, Error::InvalidFrame(message) if message.contains("JPEG width")),
            "unexpected error: {err_msg}"
        );

        let tall = image_test_frame(PixelFormat::RGBA, 1, 65_536, None, vec![0; 65_536 * 4]);
        let err = tall.encode_jpeg(85).unwrap_err();
        let err_msg = err.to_string();
        assert!(
            matches!(&err, Error::InvalidFrame(message) if message.contains("JPEG height")),
            "unexpected error: {err_msg}"
        );
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn test_encode_jpeg_rejects_unsupported_format() {
        let frame = image_test_frame(PixelFormat::UYVY, 2, 2, None, vec![0; 8]);

        let err = frame.encode_jpeg(85).unwrap_err();
        let err_msg = err.to_string();
        assert!(
            matches!(&err, Error::InvalidFrame(message) if message.contains("Unsupported format")),
            "unexpected error: {err_msg}"
        );
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn test_png_input_borrows_only_tightly_packed_rgba() {
        let rgba = image_test_frame(PixelFormat::RGBA, 2, 1, None, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let source = ImagePixelSource::new(rgba.layout, rgba.data()).unwrap();
        let (pixels, _, _) = source.png_rgba_input().unwrap();
        assert!(matches!(pixels, Cow::Borrowed(_)));

        let padded_rgba = image_test_frame(
            PixelFormat::RGBA,
            2,
            1,
            Some(12),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 200, 201, 202, 203],
        );
        let source = ImagePixelSource::new(padded_rgba.layout, padded_rgba.data()).unwrap();
        let (pixels, _, _) = source.png_rgba_input().unwrap();
        assert!(matches!(pixels, Cow::Owned(_)));

        let rgbx = image_test_frame(PixelFormat::RGBX, 2, 1, None, vec![1, 2, 3, 0, 5, 6, 7, 0]);
        let source = ImagePixelSource::new(rgbx.layout, rgbx.data()).unwrap();
        let (pixels, _, _) = source.png_rgba_input().unwrap();
        assert!(matches!(pixels, Cow::Owned(_)));
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn test_image_pixel_source_rejects_data_size_layout_and_bad_data_len() {
        let data_size_layout = ValidatedVideoLayout {
            width: 2,
            height: 1,
            pixel_format: PixelFormat::RGBA,
            data_len_bytes: 8,
            line_stride_or_size: LineStrideOrSize::DataSizeBytes(8),
        };
        let err = ImagePixelSource::new(data_size_layout, &[0; 8]).unwrap_err();
        let err_msg = err.to_string();
        assert!(
            matches!(&err, Error::InvalidFrame(message) if message.contains("data-size frame")),
            "unexpected error: {err_msg}"
        );

        let layout = ValidatedVideoLayout::new_uncompressed(PixelFormat::RGBA, 2, 1, None).unwrap();
        let err = ImagePixelSource::new(layout, &[0; 7]).unwrap_err();
        let err_msg = err.to_string();
        assert!(
            matches!(&err, Error::InvalidFrame(message) if message.contains("Video data length")),
            "unexpected error: {err_msg}"
        );
    }

    /// Test PixelFormatInfo for packed RGB formats (32 bpp)
    #[test]
    fn test_pixel_format_info_packed_rgb() {
        let formats = [
            PixelFormat::BGRA,
            PixelFormat::BGRX,
            PixelFormat::RGBA,
            PixelFormat::RGBX,
        ];

        for fmt in formats {
            let info = fmt.info();
            assert_eq!(
                info.bytes_per_pixel(),
                4,
                "Format {:?} bytes per pixel",
                fmt
            );
            assert_eq!(
                info.category(),
                FormatCategory::Packed,
                "Format {:?} category",
                fmt
            );
            assert!(
                !info.is_planar_420(),
                "Format {:?} should not be planar",
                fmt
            );

            // 1920x1080, stride = 1920 * 4 = 7680
            let len = info.try_buffer_len(7680, 1080).unwrap();
            assert_eq!(len, 7680 * 1080, "Format {:?} even dimensions", fmt);

            // Odd dimensions: 1921x1081
            let len = info.try_buffer_len(7684, 1081).unwrap();
            assert_eq!(len, 7684 * 1081, "Format {:?} odd dimensions", fmt);
        }
    }

    /// Test PixelFormatInfo for packed YUV formats
    #[test]
    fn test_pixel_format_info_packed_yuv() {
        // UYVY: 16 bpp = 2 bytes per pixel
        let info = PixelFormat::UYVY.info();
        assert_eq!(info.bytes_per_pixel(), 2);
        assert_eq!(info.category(), FormatCategory::Packed);
        let len = info.try_buffer_len(3840, 1080).unwrap();
        assert_eq!(len, 3840 * 1080);

        // UYVA: 24 bpp = 3 bytes per pixel
        let info = PixelFormat::UYVA.info();
        assert_eq!(info.bytes_per_pixel(), 3);
        assert_eq!(info.category(), FormatCategory::Packed);
        let len = info.try_buffer_len(5760, 1080).unwrap();
        assert_eq!(len, 5760 * 1080);

        // P216/PA16: 32 bpp = 4 bytes per pixel
        let info = PixelFormat::P216.info();
        assert_eq!(info.bytes_per_pixel(), 4);
        assert_eq!(info.category(), FormatCategory::Packed);
        let len = info.try_buffer_len(7680, 1080).unwrap();
        assert_eq!(len, 7680 * 1080);

        let info = PixelFormat::PA16.info();
        assert_eq!(info.bytes_per_pixel(), 4);
        let len = info.try_buffer_len(7680, 1080).unwrap();
        assert_eq!(len, 7680 * 1080);
    }

    /// Test PixelFormatInfo for planar YV12/I420 with even dimensions
    #[test]
    fn test_pixel_format_info_planar_420_even() {
        // 1920x1080 YV12/I420
        // Y: 1920 * 1080 = 2,073,600
        // U: (1920/2) * (1080/2) = 960 * 540 = 518,400
        // V: (1920/2) * (1080/2) = 960 * 540 = 518,400
        // Total: 2,073,600 + 518,400 + 518,400 = 3,110,400
        let y_stride = 1920;

        let info = PixelFormat::YV12.info();
        assert_eq!(info.bytes_per_pixel(), 1);
        assert_eq!(info.category(), FormatCategory::Planar420);
        assert!(info.is_planar_420());
        let len = info.try_buffer_len(y_stride, 1080).unwrap();
        assert_eq!(len, 3_110_400, "YV12 1920x1080");

        let info = PixelFormat::I420.info();
        assert_eq!(info.category(), FormatCategory::Planar420);
        let len = info.try_buffer_len(y_stride, 1080).unwrap();
        assert_eq!(len, 3_110_400, "I420 1920x1080");
    }

    /// Test PixelFormatInfo rejects planar YV12/I420 with odd layout inputs
    #[test]
    fn test_pixel_format_info_planar_420_odd() {
        let y_stride = 1921;

        assert!(PixelFormat::YV12
            .info()
            .try_buffer_len(y_stride, 1081)
            .is_err());
        assert!(PixelFormat::I420
            .info()
            .try_buffer_len(y_stride, 1081)
            .is_err());
    }

    /// Test PixelFormatInfo for semi-planar NV12 with even dimensions
    #[test]
    fn test_pixel_format_info_nv12_even() {
        // 1920x1080 NV12
        // Y: 1920 * 1080 = 2,073,600
        // UV: 1920 * (1080/2) = 1920 * 540 = 1,036,800
        // Total: 2,073,600 + 1,036,800 = 3,110,400
        let y_stride = 1920;

        let info = PixelFormat::NV12.info();
        assert_eq!(info.bytes_per_pixel(), 1);
        assert_eq!(info.category(), FormatCategory::SemiPlanar420);
        assert!(info.is_planar_420());
        let len = info.try_buffer_len(y_stride, 1080).unwrap();
        assert_eq!(len, 3_110_400, "NV12 1920x1080");
    }

    /// Test PixelFormatInfo rejects semi-planar NV12 with odd layout inputs
    #[test]
    fn test_pixel_format_info_nv12_odd() {
        let y_stride = 1921;
        assert!(PixelFormat::NV12
            .info()
            .try_buffer_len(y_stride, 1081)
            .is_err());
    }

    /// Test PixelFormat::line_stride for all formats
    #[test]
    fn test_pixel_format_line_stride() {
        // Packed RGB: 4 bytes per pixel
        assert_eq!(PixelFormat::BGRA.try_line_stride(1920).unwrap(), 7680);
        assert_eq!(PixelFormat::BGRX.try_line_stride(1920).unwrap(), 7680);
        assert_eq!(PixelFormat::RGBA.try_line_stride(1920).unwrap(), 7680);
        assert_eq!(PixelFormat::RGBX.try_line_stride(1920).unwrap(), 7680);

        // UYVY: 2 bytes per pixel
        assert_eq!(PixelFormat::UYVY.try_line_stride(1920).unwrap(), 3840);

        // UYVA: 3 bytes per pixel
        assert_eq!(PixelFormat::UYVA.try_line_stride(1920).unwrap(), 5760);

        // P216/PA16: 4 bytes per pixel
        assert_eq!(PixelFormat::P216.try_line_stride(1920).unwrap(), 7680);
        assert_eq!(PixelFormat::PA16.try_line_stride(1920).unwrap(), 7680);

        // Planar 4:2:0: Y-plane stride = 1 byte per pixel
        assert_eq!(PixelFormat::YV12.try_line_stride(1920).unwrap(), 1920);
        assert_eq!(PixelFormat::I420.try_line_stride(1920).unwrap(), 1920);
        assert_eq!(PixelFormat::NV12.try_line_stride(1920).unwrap(), 1920);
    }

    /// Test PixelFormat::buffer_size for all formats
    #[test]
    fn test_pixel_format_buffer_size() {
        // Packed RGB: width * 4 * height
        assert_eq!(
            PixelFormat::BGRA.try_buffer_size(1920, 1080).unwrap(),
            8_294_400
        );
        assert_eq!(
            PixelFormat::RGBA.try_buffer_size(1920, 1080).unwrap(),
            8_294_400
        );

        // Planar 4:2:0: Y + U + V = 1.5 * width * height
        assert_eq!(
            PixelFormat::YV12.try_buffer_size(1920, 1080).unwrap(),
            3_110_400
        );
        assert_eq!(
            PixelFormat::I420.try_buffer_size(1920, 1080).unwrap(),
            3_110_400
        );

        // Semi-planar 4:2:0: Y + UV = 1.5 * width * height
        assert_eq!(
            PixelFormat::NV12.try_buffer_size(1920, 1080).unwrap(),
            3_110_400
        );
    }

    /// Test PixelFormatInfo::is_planar_420 helper
    #[test]
    fn test_pixel_format_info_is_planar_420() {
        assert!(PixelFormat::YV12.info().is_planar_420());
        assert!(PixelFormat::I420.info().is_planar_420());
        assert!(PixelFormat::NV12.info().is_planar_420());

        assert!(!PixelFormat::BGRA.info().is_planar_420());
        assert!(!PixelFormat::RGBA.info().is_planar_420());
        assert!(!PixelFormat::UYVY.info().is_planar_420());
        assert!(!PixelFormat::UYVA.info().is_planar_420());
    }

    /// Test VideoFrame builder with planar formats produces correct buffer sizes
    #[test]
    fn test_videoframe_builder_planar_even() {
        let frame = VideoFrame::builder()
            .resolution(1920, 1080)
            .pixel_format(PixelFormat::NV12)
            .build()
            .expect("Builder should succeed");

        assert_eq!(frame.width(), 1920);
        assert_eq!(frame.height(), 1080);
        assert_eq!(frame.pixel_format(), PixelFormat::NV12);
        assert_eq!(frame.data().len(), 3_110_400, "NV12 1920x1080 buffer size");
    }

    /// Test VideoFrame builder rejects planar formats with odd dimensions
    #[test]
    fn test_videoframe_builder_planar_odd() {
        let result = VideoFrame::builder()
            .resolution(1921, 1081)
            .pixel_format(PixelFormat::I420)
            .build();

        assert!(
            matches!(result, Err(Error::InvalidFrame(_))),
            "Planar 4:2:0 odd dimensions should be rejected"
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

        assert_eq!(frame.width(), 1920);
        assert_eq!(frame.height(), 1080);
        assert_eq!(frame.pixel_format(), PixelFormat::BGRA);
        assert_eq!(
            frame.data().len(),
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

        assert_eq!(frame.width(), width);
        assert_eq!(frame.height(), height);
        assert_eq!(frame.pixel_format(), PixelFormat::NV12);
        assert_eq!(
            frame.data().len(),
            expected_size,
            "Should copy full Y+UV buffer"
        );
        assert_eq!(
            frame.data()[expected_size - 1],
            0xFF,
            "Last byte should be copied"
        );
    }

    /// Test VideoFrame::from_raw rejects synthetic I420 frame with odd dimensions
    #[test]
    fn test_videoframe_from_raw_i420_odd() {
        let width = 1921;
        let height = 1081;
        let y_stride = 1921;
        let expected_size = 3_116_403; // Y + U + V with ceiling division

        let mut data = vec![0u8; expected_size];

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

        let result = unsafe { VideoFrame::from_raw(&c_frame) };
        assert!(
            matches!(result, Err(Error::InvalidFrame(_))),
            "Planar 4:2:0 odd dimensions should be rejected"
        );
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
            frame.data().len(),
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

    /// Test that MAX_METADATA_BYTES constant is properly defined.
    #[test]
    fn test_max_metadata_bytes_constant() {
        assert_eq!(MAX_METADATA_BYTES, 4 * 1024 * 1024);
    }

    fn metadata_raw_from_bytes(data: &mut [u8], length: i32) -> NDIlib_metadata_frame_t {
        NDIlib_metadata_frame_t {
            length,
            timecode: 12345,
            p_data: data.as_mut_ptr().cast::<c_char>(),
        }
    }

    fn null_metadata_raw(length: i32) -> NDIlib_metadata_frame_t {
        NDIlib_metadata_frame_t {
            length,
            timecode: 12345,
            p_data: ptr::null_mut(),
        }
    }

    #[test]
    fn test_validate_metadata_layout_accepts_empty_null_frame() {
        let raw = null_metadata_raw(0);
        let layout = validate_metadata_layout(&raw).expect("empty null frame is valid");

        assert_eq!(
            layout,
            ValidatedMetadataLayout {
                len_with_nul: 0,
                text_len: 0,
            }
        );
    }

    #[test]
    fn test_validate_metadata_layout_accepts_one_byte_empty_payload() {
        let mut data = vec![0u8];
        let raw = metadata_raw_from_bytes(&mut data, 1);
        let layout = validate_metadata_layout(&raw).expect("explicit empty payload is valid");

        assert_eq!(layout.len_with_nul, 1);
        assert_eq!(layout.text_len, 0);
    }

    #[test]
    fn test_validate_metadata_layout_accepts_utf8_with_bounded_length() {
        let mut data = "<ndi tally=\"preview\"/>".as_bytes().to_vec();
        data.push(0);
        let length = data.len() as i32;
        let raw = metadata_raw_from_bytes(&mut data, length);
        let layout = validate_metadata_layout(&raw).expect("valid UTF-8 metadata");

        assert_eq!(layout.len_with_nul, data.len());
        assert_eq!(layout.text_len, data.len() - 1);
    }

    #[test]
    fn test_validate_metadata_layout_ignores_bytes_after_length() {
        let mut data = b"ok\0\0\xFF".to_vec();
        let raw = metadata_raw_from_bytes(&mut data, 3);
        let layout = validate_metadata_layout(&raw).expect("extra bytes after length ignored");

        assert_eq!(layout.text_len, 2);
        let owned = unsafe { MetadataFrame::from_raw_validated(&raw, layout) };
        assert_eq!(owned.data(), "ok");
    }

    #[test]
    fn test_validate_metadata_layout_rejects_negative_length() {
        let raw = null_metadata_raw(-1);

        assert!(matches!(
            validate_metadata_layout(&raw),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_metadata_layout_rejects_lengthless_non_null_data() {
        let mut data = vec![0u8];
        let raw = metadata_raw_from_bytes(&mut data, 0);

        assert!(matches!(
            validate_metadata_layout(&raw),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_metadata_layout_rejects_nonzero_length_null_data() {
        let raw = null_metadata_raw(1);

        assert!(matches!(
            validate_metadata_layout(&raw),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_metadata_layout_rejects_missing_trailing_nul() {
        let mut data = b"abc".to_vec();
        let length = data.len() as i32;
        let raw = metadata_raw_from_bytes(&mut data, length);

        assert!(matches!(
            validate_metadata_layout(&raw),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_metadata_layout_rejects_interior_nul() {
        let mut data = b"a\0b\0".to_vec();
        let length = data.len() as i32;
        let raw = metadata_raw_from_bytes(&mut data, length);

        assert!(matches!(
            validate_metadata_layout(&raw),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_metadata_layout_rejects_oversized_length_before_reading() {
        let mut data = vec![0u8];
        let raw = metadata_raw_from_bytes(&mut data, (MAX_METADATA_BYTES + 1) as i32);

        assert!(matches!(
            validate_metadata_layout(&raw),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_metadata_layout_rejects_invalid_utf8() {
        let mut data = vec![0xFF, 0];
        let length = data.len() as i32;
        let raw = metadata_raw_from_bytes(&mut data, length);

        assert!(matches!(
            validate_metadata_layout(&raw),
            Err(Error::InvalidUtf8(_))
        ));
    }

    #[test]
    fn test_metadata_frame_constructor_and_accessors_preserve_text() {
        let frame = MetadataFrame::with_data("<ndi_product/>", 9876).unwrap();

        assert_eq!(frame.data(), "<ndi_product/>");
        assert_eq!(frame.timecode(), 9876);
        assert_eq!(frame.clone().into_data(), "<ndi_product/>");
    }

    #[test]
    fn test_metadata_frame_rejects_interior_nul_input() {
        assert!(matches!(
            MetadataFrame::with_data("bad\0metadata", 0),
            Err(Error::InvalidCString(_))
        ));

        let mut frame = MetadataFrame::new();
        assert!(matches!(
            frame.set_data("bad\0metadata"),
            Err(Error::InvalidCString(_))
        ));
    }

    #[test]
    fn test_metadata_frame_rejects_oversized_input() {
        let oversized = "x".repeat(MAX_METADATA_BYTES);

        assert!(matches!(
            MetadataFrame::with_data(oversized, 0),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_metadata_frame_setters_preserve_invariants() {
        let mut frame = MetadataFrame::new();

        frame.set_data("updated").unwrap();
        frame.set_timecode(42);

        assert_eq!(frame.data(), "updated");
        assert_eq!(frame.timecode(), 42);
        assert_eq!(MetadataFrame::new().with_timecode(7).timecode(), 7);
    }

    #[test]
    fn test_metadata_frame_from_raw_validated_copies_only_payload() {
        let mut data = b"copy-me\0\0\xFF".to_vec();
        let raw = metadata_raw_from_bytes(&mut data, 8);
        let layout = validate_metadata_layout(&raw).unwrap();
        let frame = unsafe { MetadataFrame::from_raw_validated(&raw, layout) };

        assert_eq!(frame.data(), "copy-me");
        assert_eq!(frame.timecode(), 12345);
    }

    #[test]
    fn test_metadata_frame_from_raw_reports_invalid_utf8() {
        let mut data = vec![0xFF, 0];
        let raw = metadata_raw_from_bytes(&mut data, 2);

        assert!(matches!(
            unsafe { MetadataFrame::from_raw(&raw) },
            Err(Error::InvalidUtf8(_))
        ));
    }

    #[test]
    fn test_metadata_frame_to_raw_includes_trailing_nul() {
        let frame = MetadataFrame::with_data("abc", 101).unwrap();
        let (c_data, raw) = frame.to_raw().unwrap();

        assert_eq!(raw.length, 4);
        assert_eq!(raw.timecode, 101);
        assert_eq!(c_data.as_bytes_with_nul(), b"abc\0");
    }

    #[test]
    fn test_empty_metadata_frame_to_raw_sends_explicit_nul() {
        let frame = MetadataFrame::new();
        let (c_data, raw) = frame.to_raw().unwrap();

        assert_eq!(raw.length, 1);
        assert_eq!(c_data.as_bytes_with_nul(), b"\0");
    }

    #[test]
    fn test_metadata_raw_length_conversion_is_checked() {
        assert!(metadata_len_to_i32(i32::MAX as usize).is_ok());
        assert!(matches!(
            metadata_len_to_i32(i32::MAX as usize + 1),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_metadata_frame_ref_uses_cached_layout() {
        use crate::capture::RecvMetadataGuard;

        let mut data = b"zero-copy\0\0\xFF".to_vec();
        let raw = metadata_raw_from_bytes(&mut data, 10);
        let guard = unsafe { RecvMetadataGuard::new(ptr::null_mut(), raw) };
        let frame_ref = unsafe { MetadataFrameRef::new(guard) }.expect("valid metadata ref");

        assert_eq!(frame_ref.data(), "zero-copy");
        assert_eq!(frame_ref.as_bytes(), b"zero-copy");
        assert_eq!(frame_ref.layout.text_len, 9);

        let owned = frame_ref.to_owned();
        assert_eq!(owned.data(), "zero-copy");
        assert_eq!(owned.timecode(), 12345);

        std::mem::forget(frame_ref);
    }

    #[test]
    fn test_validate_frame_metadata_accepts_null_without_allocation_state() {
        let layout = unsafe { validate_frame_metadata(ptr::null()) }.expect("null is valid");

        assert_eq!(
            layout,
            ValidatedFrameMetadata {
                len_with_nul: None,
                text_len: 0,
            }
        );
    }

    #[test]
    fn test_validate_frame_metadata_accepts_explicit_empty() {
        let mut data = b"\0".to_vec();
        let layout = unsafe { validate_frame_metadata(data.as_mut_ptr().cast::<c_char>()) }
            .expect("explicit empty metadata is valid");

        assert_eq!(layout.len_with_nul.unwrap().get(), 1);
        assert_eq!(layout.text_len, 0);
        assert_eq!(
            unsafe { frame_metadata_str(data.as_ptr().cast::<c_char>(), layout) },
            Some("")
        );
    }

    #[test]
    fn test_validate_frame_metadata_accepts_utf8_and_ignores_after_first_nul() {
        let mut data = b"hello metadata\0\0\xFF".to_vec();
        let layout = unsafe { validate_frame_metadata(data.as_mut_ptr().cast::<c_char>()) }
            .expect("valid UTF-8 metadata");

        assert_eq!(layout.text_len, "hello metadata".len());
        assert_eq!(
            unsafe { frame_metadata_str(data.as_ptr().cast::<c_char>(), layout) },
            Some("hello metadata")
        );
    }

    #[test]
    fn test_validate_frame_metadata_accepts_max_boundary() {
        let mut data = vec![b'x'; MAX_METADATA_BYTES];
        data[MAX_METADATA_BYTES - 1] = 0;

        let layout = unsafe { validate_frame_metadata(data.as_mut_ptr().cast::<c_char>()) }
            .expect("metadata exactly at cap including terminator is valid");

        assert_eq!(layout.len_with_nul.unwrap().get(), MAX_METADATA_BYTES);
        assert_eq!(layout.text_len, MAX_METADATA_BYTES - 1);
    }

    #[test]
    fn test_validate_frame_metadata_rejects_missing_nul_within_cap() {
        let mut data = vec![b'x'; MAX_METADATA_BYTES];

        assert!(matches!(
            unsafe { validate_frame_metadata(data.as_mut_ptr().cast::<c_char>()) },
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_frame_metadata_rejects_invalid_utf8_before_nul() {
        let mut data = vec![0xFF, 0];

        assert!(matches!(
            unsafe { validate_frame_metadata(data.as_mut_ptr().cast::<c_char>()) },
            Err(Error::InvalidUtf8(_))
        ));
    }

    #[test]
    fn test_video_frame_ref_metadata_is_cached_and_owned_conversion_appends_nul() {
        use crate::capture::RecvVideoGuard;

        let width = 16;
        let height = 8;
        let stride = width * 4;
        let expected_len = (stride * height) as usize;
        let data = vec![0u8; expected_len];
        let mut metadata = b"cached\0\xFF".to_vec();

        let raw = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 123,
            p_data: data.as_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: metadata.as_mut_ptr().cast::<c_char>(),
            timestamp: 456,
        };

        let guard = unsafe { RecvVideoGuard::new(ptr::null_mut(), raw) };
        let frame_ref = unsafe { VideoFrameRef::new(guard) }.expect("valid video ref");

        assert_eq!(frame_ref.metadata(), Some("cached"));
        metadata[6] = b'!';
        assert_eq!(frame_ref.metadata(), Some("cached"));
        assert!(format!("{frame_ref:?}").contains("metadata: Some(\"cached\")"));

        let owned = frame_ref.to_owned().expect("owned conversion");
        assert_eq!(owned.metadata(), Some("cached"));
        assert_eq!(owned.data().len(), expected_len);

        std::mem::forget(frame_ref);
    }

    #[test]
    fn test_audio_frame_ref_metadata_is_text_and_owned_conversion_preserves_it() {
        use crate::capture::RecvAudioGuard;

        let no_samples = 4;
        let no_channels = 2;
        let sample_count = (no_samples * no_channels) as usize;
        let data = vec![0.25f32; sample_count];
        let mut metadata = b"audio meta\0".to_vec();

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels,
            no_samples,
            timecode: 123,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: no_samples * 4,
            },
            p_metadata: metadata.as_mut_ptr().cast::<c_char>(),
            timestamp: 456,
        };

        let guard = unsafe { RecvAudioGuard::new(ptr::null_mut(), raw) };
        let frame_ref = unsafe { AudioFrameRef::new(guard) }.expect("valid audio ref");

        assert_eq!(frame_ref.metadata(), Some("audio meta"));
        let owned = frame_ref.to_owned().expect("owned conversion");
        assert_eq!(owned.metadata(), Some("audio meta"));
        assert_eq!(owned.data().len(), sample_count);

        std::mem::forget(frame_ref);
    }

    #[test]
    fn test_owned_video_from_raw_rejects_malformed_frame_metadata() {
        let width = 16;
        let height = 8;
        let stride = width * 4;
        let expected_len = (stride * height) as usize;
        let mut data = vec![0u8; expected_len];
        let mut metadata = vec![b'x'; MAX_METADATA_BYTES];

        let raw = NDIlib_video_frame_v2_t {
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
            p_metadata: metadata.as_mut_ptr().cast::<c_char>(),
            timestamp: 0,
        };

        assert!(matches!(
            unsafe { VideoFrame::from_raw(&raw) },
            Err(Error::InvalidFrame(_))
        ));

        metadata[0] = 0xFF;
        metadata[1] = 0;
        assert!(matches!(
            unsafe { VideoFrame::from_raw(&raw) },
            Err(Error::InvalidUtf8(_))
        ));
    }

    #[test]
    fn test_owned_frame_metadata_builders_setters_and_raw_conversion() {
        let mut video = VideoFrame::builder().metadata("").build().unwrap();
        assert_eq!(video.metadata(), Some(""));
        let raw = video.to_raw();
        assert!(!raw.p_metadata.is_null());
        assert_eq!(
            unsafe { slice::from_raw_parts(raw.p_metadata.cast::<u8>(), 1) },
            b"\0"
        );

        video.set_metadata(Some("video meta")).unwrap();
        assert_eq!(video.metadata(), Some("video meta"));
        let raw = video.to_raw();
        assert_eq!(
            unsafe { slice::from_raw_parts(raw.p_metadata.cast::<u8>(), 11) },
            b"video meta\0"
        );

        video.set_metadata(Option::<String>::None).unwrap();
        assert!(video.metadata().is_none());
        assert!(video.to_raw().p_metadata.is_null());

        let mut audio = AudioFrame::builder().metadata("").build().unwrap();
        assert_eq!(audio.metadata(), Some(""));
        assert!(!audio.to_raw().p_metadata.is_null());
        audio.set_metadata(Some("audio meta")).unwrap();
        assert_eq!(audio.metadata(), Some("audio meta"));

        assert!(matches!(
            VideoFrame::builder().metadata("bad\0metadata").build(),
            Err(Error::InvalidCString(_))
        ));
        assert!(matches!(
            AudioFrame::builder().metadata("bad\0metadata").build(),
            Err(Error::InvalidCString(_))
        ));
        assert!(matches!(
            video.set_metadata(Some("bad\0metadata")),
            Err(Error::InvalidCString(_))
        ));
        assert!(matches!(
            audio.set_metadata(Some("bad\0metadata")),
            Err(Error::InvalidCString(_))
        ));

        let oversized = "x".repeat(MAX_METADATA_BYTES);
        assert!(matches!(
            VideoFrame::builder().metadata(oversized.clone()).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder().metadata(oversized.clone()).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            video.set_metadata(Some(oversized.clone())),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            audio.set_metadata(Some(oversized)),
            Err(Error::InvalidFrame(_))
        ));
    }

    /// Test that audio frames with overflow in checked_mul are rejected
    #[test]
    fn test_audio_overflow_checked_mul() {
        // Use an extreme channel count to exceed the maximum backing buffer size.
        let no_samples = 1024;
        let no_channels = i32::MAX;
        let mut dummy_data = vec![0f32; 1024]; // Small buffer, won't be used due to guard

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels,
            no_samples,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: dummy_data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: no_samples * 4,
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
                channel_stride_in_bytes: no_samples * 4,
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
        assert_eq!(frame.num_samples(), no_samples);
        assert_eq!(frame.num_channels(), no_channels);
    }

    #[test]
    fn test_audio_builder_rejects_invalid_dimensions() {
        assert!(matches!(
            AudioFrame::builder().sample_rate(0).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder().channels(0).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder().samples(0).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder().samples(-1).build(),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_audio_builder_rejects_oversized_layout() {
        let samples = (MAX_AUDIO_BYTES / std::mem::size_of::<f32>()) as i32 + 1;
        let result = AudioFrame::builder().channels(1).samples(samples).build();

        assert!(matches!(result, Err(Error::InvalidFrame(_))));
    }

    #[test]
    fn test_video_builder_rejects_invalid_send_metadata() {
        assert!(matches!(
            VideoFrame::builder().frame_rate(0, 1).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            VideoFrame::builder().frame_rate(30, 0).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            VideoFrame::builder().aspect_ratio(f32::NAN).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            VideoFrame::builder().aspect_ratio(0.0).build(),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_owned_frame_data_replacement_preserves_layout_size() {
        let mut video = VideoFrame::builder()
            .resolution(16, 16)
            .pixel_format(PixelFormat::BGRA)
            .build()
            .unwrap();
        assert!(video.replace_data(vec![0; video.data().len() - 1]).is_err());

        let mut audio = AudioFrame::builder()
            .channels(2)
            .samples(16)
            .build()
            .unwrap();
        assert!(audio
            .replace_data(vec![0.0; audio.data().len() + 1])
            .is_err());
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

    // =========================================================================
    // Tests for frame layout validation helpers
    // =========================================================================

    /// Test validate_video_layout with valid uncompressed frame
    #[test]
    fn test_validate_video_layout_valid_uncompressed() {
        let width = 1920;
        let height = 1080;
        let stride = width * 4; // BGRA
        let expected_size = (stride * height) as usize;
        let mut data = vec![0u8; expected_size];

        let raw = NDIlib_video_frame_v2_t {
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

        let result = validate_video_layout(&raw);
        assert!(result.is_ok(), "Should validate valid uncompressed frame");

        let layout = result.unwrap();
        assert_eq!(layout.pixel_format, PixelFormat::BGRA);
        assert_eq!(layout.data_len_bytes, expected_size);
        assert_eq!(
            layout.line_stride_or_size,
            LineStrideOrSize::LineStrideBytes(stride)
        );
    }

    /// Test validate_video_layout rejects null data pointer
    #[test]
    fn test_validate_video_layout_null_pointer() {
        let raw = NDIlib_video_frame_v2_t {
            xres: 1920,
            yres: 1080,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: ptr::null_mut(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 7680,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_video_layout(&raw);
        assert!(result.is_err(), "Should reject null data pointer");

        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(
                msg.contains("null data pointer"),
                "Error should mention null pointer, got: {msg}"
            );
        } else {
            panic!("Expected InvalidFrame error");
        }
    }

    /// Test validate_video_layout rejects invalid line_stride
    #[test]
    fn test_validate_video_layout_invalid_stride() {
        let mut data = vec![0u8; 1024];

        let raw = NDIlib_video_frame_v2_t {
            xres: 1920,
            yres: 1080,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 0, // Invalid stride
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_video_layout(&raw);
        assert!(result.is_err(), "Should reject zero line_stride");

        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(
                msg.contains("invalid line_stride_in_bytes"),
                "Error should mention invalid stride, got: {msg}"
            );
        } else {
            panic!("Expected InvalidFrame error");
        }
    }

    /// Test validate_video_layout rejects strides smaller than one row.
    #[test]
    fn test_validate_video_layout_rejects_short_stride() {
        let mut data = vec![0u8; 1024];

        let raw = NDIlib_video_frame_v2_t {
            xres: 1920,
            yres: 1080,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 1919 * 4,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_video_layout(&raw);
        assert!(matches!(result, Err(Error::InvalidFrame(_))));
    }

    /// Test validate_video_layout rejects odd dimensions for planar 4:2:0.
    #[test]
    fn test_validate_video_layout_rejects_planar_odd_dimensions() {
        let mut data = vec![0u8; 4096];

        let raw = NDIlib_video_frame_v2_t {
            xres: 641,
            yres: 480,
            FourCC: PixelFormat::I420.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 4.0 / 3.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 642,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_video_layout(&raw);
        assert!(matches!(result, Err(Error::InvalidFrame(_))));
    }

    /// Test validate_video_layout rejects negative dimensions
    #[test]
    fn test_validate_video_layout_negative_dimensions() {
        let mut data = vec![0u8; 1024];

        let raw = NDIlib_video_frame_v2_t {
            xres: 1920,
            yres: -1, // Negative height
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 7680,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_video_layout(&raw);
        assert!(result.is_err(), "Should reject negative height");

        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(
                msg.contains("invalid height"),
                "Error should mention invalid height, got: {msg}"
            );
        } else {
            panic!("Expected InvalidFrame error");
        }
    }

    /// Test validate_video_layout rejects oversized frames
    #[test]
    fn test_validate_video_layout_exceeds_max() {
        // 8K resolution: 7680 x 4320, BGRA = 4 bytes per pixel
        // Total: 7680 * 4320 * 4 = 132,710,400 bytes > 100 MiB (MAX_VIDEO_BYTES)
        let width = 7680;
        let height = 4320;
        let stride = width * 4;
        let expected_size = (stride * height) as usize;
        let mut data = vec![0u8; expected_size];

        let raw = NDIlib_video_frame_v2_t {
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

        let result = validate_video_layout(&raw);
        assert!(result.is_err(), "Should reject oversized frame");

        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(
                msg.contains("exceeds maximum size"),
                "Error should mention size limit, got: {msg}"
            );
        } else {
            panic!("Expected InvalidFrame error");
        }
    }

    /// Test validate_audio_layout with valid frame
    #[test]
    fn test_validate_audio_layout_valid() {
        let no_samples = 1024;
        let no_channels = 2;
        let sample_count = (no_samples * no_channels) as usize;
        let mut data = vec![0.0f32; sample_count];

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels,
            no_samples,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: no_samples * 4,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_audio_layout(&raw);
        assert!(result.is_ok(), "Should validate valid audio frame");

        let layout = result.unwrap();
        assert_eq!(layout.format, Some(AudioFormat::FLTP));
        assert_eq!(layout.sample_count, sample_count);
    }

    /// Test validate_audio_layout supports strided planar FLTP audio.
    #[test]
    fn test_validate_audio_layout_strided_planar() {
        let no_samples = 4;
        let no_channels = 2;
        let stride_samples = 6;
        let backing_samples = (stride_samples + no_samples) as usize;
        let mut data = vec![0.0f32; backing_samples];

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels,
            no_samples,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: stride_samples * 4,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let layout = validate_audio_layout(&raw).expect("strided FLTP should validate");
        assert_eq!(layout.sample_count, backing_samples);
        assert_eq!(layout.channel_stride_samples, stride_samples as usize);
        assert_eq!(layout.channel_range(1), Some(6..10));
    }

    /// Test validate_audio_layout_allow_empty accepts the documented no-source query state.
    #[test]
    fn test_validate_audio_layout_empty_query_no_source() {
        let raw = NDIlib_audio_frame_v3_t::default();

        let layout =
            validate_audio_layout_allow_empty(&raw).expect("all-zero query state should validate");
        assert!(layout.is_empty());
        assert_eq!(layout.format(), None);
        assert_eq!(layout.sample_count, 0);
    }

    /// Test validate_audio_layout_allow_empty accepts source-format query without samples.
    #[test]
    fn test_validate_audio_layout_empty_query_with_source_format() {
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            no_samples: 0,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: ptr::null_mut(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 0,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let layout = validate_audio_layout_allow_empty(&raw)
            .expect("query source format should validate without samples");
        assert!(layout.is_empty());
        assert_eq!(layout.format(), Some(AudioFormat::FLTP));
        assert_eq!(layout.sample_rate, 48000);
        assert_eq!(layout.no_channels, 2);
    }

    /// Test empty audio validation rejects partial query/no-source states.
    #[test]
    fn test_validate_audio_layout_rejects_partial_empty_query() {
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 0,
            no_samples: 0,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: ptr::null_mut(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 0,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_audio_layout_allow_empty(&raw);
        assert!(matches!(result, Err(Error::InvalidFrame(_))));
    }

    /// Test validate_audio_layout rejects invalid channel stride.
    #[test]
    fn test_validate_audio_layout_rejects_invalid_channel_stride() {
        let mut data = vec![0.0f32; 2048];

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            no_samples: 1024,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 1024 * 4 - 4,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_audio_layout(&raw);
        assert!(matches!(result, Err(Error::InvalidFrame(_))));
    }

    /// Test validate_audio_layout rejects null data pointer
    #[test]
    fn test_validate_audio_layout_null_pointer() {
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            no_samples: 1024,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: ptr::null_mut(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 1024 * 4,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_audio_layout(&raw);
        assert!(result.is_err(), "Should reject null data pointer");

        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(
                msg.contains("null data pointer"),
                "Error should mention null pointer, got: {msg}"
            );
        } else {
            panic!("Expected InvalidFrame error");
        }
    }

    /// Test validate_audio_layout rejects negative sample count
    #[test]
    fn test_validate_audio_layout_negative_samples() {
        let mut data = vec![0.0f32; 1024];

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            no_samples: -1, // Negative
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 1024 * 4,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_audio_layout(&raw);
        assert!(result.is_err(), "Should reject negative sample count");

        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(
                msg.contains("Invalid number of samples"),
                "Error should mention invalid samples, got: {msg}"
            );
        } else {
            panic!("Expected InvalidFrame error");
        }
    }

    /// Test validate_audio_layout rejects overflow scenario
    #[test]
    fn test_validate_audio_layout_overflow() {
        let mut data = vec![0.0f32; 1024];

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: i32::MAX,
            no_samples: 1024,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 1024 * 4,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_audio_layout(&raw);
        assert!(
            result.is_err(),
            "Should reject audio frame with overflow potential"
        );

        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(
                msg.contains("overflow") || msg.contains("exceeds maximum size"),
                "Error should mention overflow or size limit, got: {msg}"
            );
        } else {
            panic!("Expected InvalidFrame error");
        }
    }

    /// Test calculate_buffer_len_checked matches PixelFormatInfo::try_buffer_len for valid inputs
    #[test]
    fn test_calculate_buffer_len_checked_matches_public_helper() {
        let test_cases = [
            (PixelFormat::BGRA, 7680usize, 1080usize),
            (PixelFormat::UYVY, 3840usize, 1080usize),
            (PixelFormat::NV12, 1920usize, 1080usize),
            (PixelFormat::YV12, 1920usize, 1080usize),
            (PixelFormat::I420, 1920usize, 1080usize),
        ];

        for (format, stride, height) in test_cases {
            let expected = format
                .info()
                .try_buffer_len(stride as i32, height as i32)
                .unwrap();
            let result = calculate_buffer_len_checked(format, stride, height);

            assert!(
                result.is_ok(),
                "Should succeed for valid inputs: {:?}",
                format
            );
            assert_eq!(
                result.unwrap(),
                expected,
                "Checked calculation should match unchecked for {:?}",
                format
            );
        }
    }

    /// Test that VideoFrameRef::data() uses cached length
    #[test]
    fn test_video_frame_ref_uses_cached_length() {
        use crate::capture::RecvVideoGuard;

        // Create a mock video frame for testing
        let width = 1920;
        let height = 1080;
        let stride = width * 4;
        let expected_size = (stride * height) as usize;
        let data = vec![0u8; expected_size];

        let raw = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 12345,
            p_data: data.as_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 67890,
        };

        // Create a guard with a null receiver instance (we won't use the free function)
        // This is safe because we'll forget the guard before it drops
        let guard = unsafe { RecvVideoGuard::new(ptr::null_mut(), raw) };

        // Create the VideoFrameRef
        let frame_ref = unsafe { VideoFrameRef::new(guard) };
        assert!(frame_ref.is_ok(), "Should create valid VideoFrameRef");

        let frame_ref = frame_ref.unwrap();

        // Verify the cached length is used
        assert_eq!(
            frame_ref.data().len(),
            expected_size,
            "data() should return slice with cached length"
        );
        assert_eq!(
            frame_ref.layout.data_len_bytes, expected_size,
            "Cached data_len_bytes should match expected"
        );

        // Forget the guard to prevent calling the free function with null instance
        std::mem::forget(frame_ref);
    }

    /// Test that AudioFrameRef::data() uses cached sample count
    #[test]
    fn test_audio_frame_ref_uses_cached_sample_count() {
        use crate::capture::RecvAudioGuard;

        let no_samples = 1024;
        let no_channels = 2;
        let sample_count = (no_samples * no_channels) as usize;
        let data = vec![0.5f32; sample_count];

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels,
            no_samples,
            timecode: 12345,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: no_samples * 4,
            },
            p_metadata: ptr::null(),
            timestamp: 67890,
        };

        // Create a guard with a null receiver instance (we won't use the free function)
        let guard = unsafe { RecvAudioGuard::new(ptr::null_mut(), raw) };

        // Create the AudioFrameRef
        let frame_ref = unsafe { AudioFrameRef::new(guard) };
        assert!(frame_ref.is_ok(), "Should create valid AudioFrameRef");

        let frame_ref = frame_ref.unwrap();

        // Verify the cached sample count is used
        assert_eq!(
            frame_ref.data().len(),
            sample_count,
            "data() should return slice with cached sample count"
        );
        assert_eq!(
            frame_ref.layout.sample_count, sample_count,
            "Cached sample_count should match expected"
        );

        // Forget the guard to prevent calling the free function with null instance
        std::mem::forget(frame_ref);
    }
}
