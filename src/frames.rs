//! Frame types for video, audio, and metadata.

use std::{
    ffi::{CStr, CString},
    fmt,
    marker::PhantomData,
    num::NonZeroUsize,
    os::raw::c_char,
    ptr, slice, str,
};

use crate::{
    capture::{AudioKind, FrameFree, FrameSyncAudioFree, Guard, VideoKind},
    recv_guard::{RecvAudioGuard, RecvMetadataGuard},
    sys::{self, bindings::*},
    Error, FourCc, FrameRate, Rejected, Result, SdkTimestamp, Timecode,
};

/// The uncompressed pixel formats NDI sends and receives.
///
/// Each converts to and from its [`FourCc`].
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
/// let format = PixelFormat::Bgra;
///
/// // For bandwidth-efficient streaming
/// let format = PixelFormat::Uyvy;
///
/// // When matching, always include a wildcard for forward compatibility
/// match format {
///     PixelFormat::Bgra | PixelFormat::Rgba => println!("Full quality RGB"),
///     PixelFormat::Uyvy => println!("Compressed YUV"),
///     _ => println!("Other format"),
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
// Each discriminant is the format's SDK FourCC value.
#[repr(u32)]
pub enum PixelFormat {
    /// YCbCr 4:2:2 format (16 bits per pixel) - bandwidth efficient.
    Uyvy = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVY,
    /// YCbCr 4:2:2 with alpha channel (24 bits per pixel).
    Uyva = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVA,
    /// 16-bit YCbCr 4:2:2 format.
    P216 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_P216,
    /// 16-bit YCbCr 4:2:2 with alpha.
    Pa16 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_PA16,
    /// Planar YCbCr 4:2:0 format (12 bits per pixel).
    Yv12 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_YV12,
    /// Planar YCbCr 4:2:0 format (12 bits per pixel).
    I420 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_I420,
    /// Semi-planar YCbCr 4:2:0 format (12 bits per pixel).
    Nv12 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NV12,
    /// Blue-Green-Red-Alpha format (32 bits per pixel) - full quality.
    Bgra = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA,
    /// Blue-Green-Red with padding (32 bits per pixel).
    Bgrx = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRX,
    /// Red-Green-Blue-Alpha format (32 bits per pixel) - full quality.
    Rgba = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBA,
    /// Red-Green-Blue with padding (32 bits per pixel).
    Rgbx = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBX,
    /// Full-resolution planar Y, U, V with 8-bit samples. Sending it needs
    /// [`RuntimeFeature::Planar444Video`](crate::RuntimeFeature::Planar444Video).
    Na48 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NA48,
    /// Full-resolution planar Y, U, V, alpha with 8-bit samples. Sending it
    /// needs [`RuntimeFeature::Planar444Video`](crate::RuntimeFeature::Planar444Video).
    Nb48 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NB48,
    /// Full-resolution planar Y, U, V with 16-bit samples. Sending it needs
    /// [`RuntimeFeature::Planar444Video`](crate::RuntimeFeature::Planar444Video).
    Na4g = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NA4G,
    /// Full-resolution planar Y, U, V, alpha with 16-bit samples. Sending it
    /// needs [`RuntimeFeature::Planar444Video`](crate::RuntimeFeature::Planar444Video).
    Nb4g = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NB4G,
}

/// Category of pixel format for buffer calculation and union field access.
///
/// This enum distinguishes between different memory layouts:
/// - **Packed**: All pixel components are interleaved in a single buffer
/// - **Planar420**: Y, U, V planes stored separately with 4:2:0 chroma subsampling
/// - **SemiPlanar420**: Y plane followed by interleaved UV plane (NV12)
/// - **PackedAlpha**: Packed 4:2:2 plane followed by an 8-bit alpha plane (UYVA)
/// - **SemiPlanar422**: 16-bit Y plane followed by interleaved CbCr plane (P216)
/// - **SemiPlanar422Alpha**: [`FormatCategory::SemiPlanar422`] followed by a
///   16-bit alpha plane (PA16)
/// - **Planar444**: Three full-resolution Y, U, V planes
/// - **Planar444Alpha**: Four full-resolution Y, U, V, alpha planes
///
/// This enum is marked `#[non_exhaustive]` because the NDI SDK's video FourCC
/// namespace is open and grows across releases; future formats may introduce
/// additional memory-layout categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FormatCategory {
    /// Packed formats: simple stride * height buffer.
    Packed,
    /// Planar 4:2:0 with separate U and V planes (YV12, I420).
    Planar420,
    /// Semi-planar 4:2:0 with interleaved UV plane (NV12).
    SemiPlanar420,
    /// Packed UYVY plane followed by an 8-bit alpha plane whose stride is half
    /// the line stride (UYVA).
    PackedAlpha,
    /// Semi-planar 4:2:2 with a 16-bit Y plane followed by an interleaved
    /// 16-bit CbCr plane, both full height at the line stride (P216).
    SemiPlanar422,
    /// [`FormatCategory::SemiPlanar422`] followed by a 16-bit alpha plane at
    /// the line stride (PA16).
    SemiPlanar422Alpha,
    /// Three full-resolution Y, U, V planes with equal byte strides.
    Planar444,
    /// Four full-resolution Y, U, V, alpha planes with equal byte strides.
    Planar444Alpha,
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
/// let info = PixelFormat::Bgra.info();
/// assert_eq!(info.bytes_per_pixel(), 4);
/// assert_eq!(info.category(), FormatCategory::Packed);
///
/// let info = PixelFormat::Nv12.info();
/// assert_eq!(info.bytes_per_pixel(), 1);
/// assert_eq!(info.category(), FormatCategory::SemiPlanar420);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelFormatInfo {
    /// Bytes per pixel for packed formats, or first-plane bytes per pixel for
    /// planar and semi-planar formats.
    bytes_per_pixel: u8,
    /// Format category for union field access and buffer calculation.
    category: FormatCategory,
}

impl PixelFormatInfo {
    /// Get bytes per pixel for packed formats, or first-plane bytes per pixel
    /// for planar and semi-planar formats (the Y plane, or the UYVY plane of
    /// UYVA).
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
    /// * `line_stride` - The first-plane line stride in bytes (for planar formats) or total line stride (for packed formats)
    /// * `height` - Frame height in pixels
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if `line_stride` or `height` is zero,
    /// if planar 4:2:0 stride/height requirements are not met, if arithmetic
    /// overflows, or if the result exceeds the crate's maximum video frame
    /// size.
    ///
    /// # Format-specific calculations
    ///
    /// - **Packed RGB/YUV** (BGRA/BGRX/RGBA/RGBX/UYVY): `line_stride * height`
    /// - **Packed 4:2:2 with alpha UYVA**: requires even stride, then
    ///   `UYVY + A` where:
    ///   - UYVY plane: `line_stride * height`
    ///   - Alpha plane: `(line_stride/2) * height`
    /// - **Semi-planar 4:2:2 P216**: `Y + CbCr` where each plane is
    ///   `line_stride * height`
    /// - **Semi-planar 4:2:2 with alpha PA16**: `Y + CbCr + A` where each
    ///   plane is `line_stride * height`
    /// - **Planar 4:2:0 YV12/I420**: requires even stride and height,
    ///   then `Y + U + V` where:
    ///   - Y plane: `line_stride * height`
    ///   - U plane: `(line_stride/2) * (height/2)`
    ///   - V plane: `(line_stride/2) * (height/2)`
    /// - **Semi-planar 4:2:0 NV12**: requires even height (any stride), then
    ///   `Y + UV` where:
    ///   - Y plane: `line_stride * height`
    ///   - UV plane: `line_stride * (height/2)`
    /// - **Planar 4:4:4** (NA48/NA4G, and NB48/NB4G with alpha): three or
    ///   four planes of `line_stride * height`
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::PixelFormat;
    ///
    /// // NV12 with 2048-byte rows: Y (2048*1080) + UV (2048*540).
    /// assert_eq!(PixelFormat::Nv12.info().buffer_len(2048, 1080)?, 3_317_760);
    /// # Ok::<(), grafton_ndi::Error>(())
    /// ```
    pub fn buffer_len(&self, line_stride: usize, height: u32) -> Result<usize> {
        if line_stride == 0 {
            return Err(Error::InvalidFrame(
                "Video line stride must be positive, got 0".into(),
            ));
        }
        if height == 0 {
            return Err(Error::InvalidFrame(
                "Video frame height must be positive, got 0".into(),
            ));
        }

        let len = calculate_buffer_len_for_info_checked(*self, line_stride, height as usize)?;
        validate_video_data_len(len)?;
        Ok(len)
    }
}

impl PixelFormat {
    /// Every format, in declaration order.
    pub(crate) const ALL: [Self; 15] = [
        Self::Uyvy,
        Self::Uyva,
        Self::P216,
        Self::Pa16,
        Self::Yv12,
        Self::I420,
        Self::Nv12,
        Self::Bgra,
        Self::Bgrx,
        Self::Rgba,
        Self::Rgbx,
        Self::Na48,
        Self::Nb48,
        Self::Na4g,
        Self::Nb4g,
    ];

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
    /// let info = PixelFormat::Bgra.info();
    /// assert_eq!(info.bytes_per_pixel(), 4);
    /// assert_eq!(info.category(), FormatCategory::Packed);
    ///
    /// // Get properties for YV12 (planar 4:2:0)
    /// let info = PixelFormat::Yv12.info();
    /// assert_eq!(info.bytes_per_pixel(), 1);
    /// assert_eq!(info.category(), FormatCategory::Planar420);
    /// ```
    #[must_use]
    pub const fn info(self) -> PixelFormatInfo {
        match self {
            Self::Bgra | Self::Bgrx | Self::Rgba | Self::Rgbx => PixelFormatInfo {
                bytes_per_pixel: 4,
                category: FormatCategory::Packed,
            },
            Self::Uyvy => PixelFormatInfo {
                bytes_per_pixel: 2,
                category: FormatCategory::Packed,
            },
            Self::Uyva => PixelFormatInfo {
                bytes_per_pixel: 2,
                category: FormatCategory::PackedAlpha,
            },
            Self::P216 => PixelFormatInfo {
                bytes_per_pixel: 2,
                category: FormatCategory::SemiPlanar422,
            },
            Self::Pa16 => PixelFormatInfo {
                bytes_per_pixel: 2,
                category: FormatCategory::SemiPlanar422Alpha,
            },
            Self::Yv12 | Self::I420 => PixelFormatInfo {
                bytes_per_pixel: 1,
                category: FormatCategory::Planar420,
            },
            Self::Nv12 => PixelFormatInfo {
                bytes_per_pixel: 1,
                category: FormatCategory::SemiPlanar420,
            },
            Self::Na48 => PixelFormatInfo {
                bytes_per_pixel: 1,
                category: FormatCategory::Planar444,
            },
            Self::Nb48 => PixelFormatInfo {
                bytes_per_pixel: 1,
                category: FormatCategory::Planar444Alpha,
            },
            Self::Na4g => PixelFormatInfo {
                bytes_per_pixel: 2,
                category: FormatCategory::Planar444,
            },
            Self::Nb4g => PixelFormatInfo {
                bytes_per_pixel: 2,
                category: FormatCategory::Planar444Alpha,
            },
        }
    }

    /// Calculate line stride in bytes for a given width using checked
    /// arithmetic.
    ///
    /// For packed formats, this returns the total bytes per row.
    /// For planar and semi-planar formats, this returns the first-plane
    /// stride, which is what the SDK expects in `line_stride_in_bytes`.
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::PixelFormat;
    ///
    /// // BGRA: 4 bytes per pixel
    /// assert_eq!(PixelFormat::Bgra.line_stride(1920)?, 7680);
    ///
    /// // UYVY: 2 bytes per pixel
    /// assert_eq!(PixelFormat::Uyvy.line_stride(1920)?, 3840);
    ///
    /// // NV12: Y-plane has 1 byte per pixel
    /// assert_eq!(PixelFormat::Nv12.line_stride(1920)?, 1920);
    ///
    /// // P216: Y-plane has 16 bits per pixel
    /// assert_eq!(PixelFormat::P216.line_stride(1920)?, 3840);
    /// # Ok::<(), grafton_ndi::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if the width is zero or odd, or if the
    /// stride does not fit the SDK's signed 32-bit stride field.
    pub fn line_stride(self, width: u32) -> Result<usize> {
        validate_video_width(width.into())?;
        let stride = min_video_line_stride_checked(self, width as usize)?;
        if i32::try_from(stride).is_err() {
            return Err(Error::InvalidFrame(format!(
                "Video line stride {stride} exceeds i32 range"
            )));
        }
        Ok(stride)
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
    /// assert_eq!(PixelFormat::Bgra.buffer_len(1920, 1080)?, 8_294_400);
    ///
    /// // NV12 1920x1080: Y (1920*1080) + UV (1920*540) = 3,110,400 bytes
    /// assert_eq!(PixelFormat::Nv12.buffer_len(1920, 1080)?, 3_110_400);
    /// # Ok::<(), grafton_ndi::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if dimensions are invalid for this
    /// format, if arithmetic overflows, or if the result exceeds the crate's
    /// maximum video frame size.
    pub fn buffer_len(self, width: u32, height: u32) -> Result<usize> {
        let layout = ValidatedVideoLayout::new_uncompressed(self, width, height, None)?;
        Ok(layout.data_len_bytes)
    }

    /// Whether sending this format needs the runtime to support it: the
    /// planar 4:4:4 formats need [`RuntimeFeature::Planar444Video`].
    ///
    /// [`RuntimeFeature::Planar444Video`]: crate::RuntimeFeature::Planar444Video
    pub(crate) fn require_sendable(self, ndi: &crate::Ndi) -> Result<()> {
        if matches!(
            self.info().category(),
            FormatCategory::Planar444 | FormatCategory::Planar444Alpha
        ) {
            ndi.require_feature(crate::RuntimeFeature::Planar444Video)?;
        }
        Ok(())
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ScanType {
    /// Progressive scan - full frames rendered sequentially.
    Progressive,
    /// Interlaced scan - alternating even/odd lines.
    Interlaced,
    /// Field 0 only (first field of interlaced content).
    Field0,
    /// Field 1 only (second field of interlaced content).
    Field1,
}

impl ScanType {
    /// Every scan type, in declaration order.
    const ALL: [Self; 4] = [
        Self::Progressive,
        Self::Interlaced,
        Self::Field0,
        Self::Field1,
    ];

    /// The SDK's frame format type for this scan type.
    pub(crate) const fn to_sdk(self) -> NDIlib_frame_format_type_e {
        match self {
            Self::Progressive => NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive,
            Self::Interlaced => NDIlib_frame_format_type_e_NDIlib_frame_format_type_interleaved,
            Self::Field0 => NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_0,
            Self::Field1 => NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_1,
        }
    }

    /// The scan type an SDK frame format type names, if it names one.
    pub(crate) fn from_sdk(value: NDIlib_frame_format_type_e) -> Option<Self> {
        Self::ALL.into_iter().find(|scan| scan.to_sdk() == value)
    }
}

/// The header fields of a video frame besides its layout, validated
/// together: they are the same for owned, borrowed and received frames.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VideoHeader {
    pub(crate) frame_rate: FrameRate,
    /// Finite and nonnegative; zero means square pixels.
    pub(crate) picture_aspect_ratio: f32,
    pub(crate) scan_type: ScanType,
    pub(crate) timecode: Timecode,
}

impl VideoHeader {
    /// Validate the header of a frame the SDK delivered.
    fn from_sdk(raw: &NDIlib_video_frame_v2_t) -> Result<Self> {
        let frame_rate = FrameRate::from_sdk(raw.frame_rate_N, raw.frame_rate_D)?;
        validate_picture_aspect_ratio(raw.picture_aspect_ratio)?;
        let scan_type = ScanType::from_sdk(raw.frame_format_type).ok_or_else(|| {
            Error::InvalidFrame(format!(
                "Unknown scan type: 0x{:08X}",
                raw.frame_format_type
            ))
        })?;
        Ok(Self {
            frame_rate,
            picture_aspect_ratio: raw.picture_aspect_ratio,
            scan_type,
            timecode: Timecode::from_ticks(raw.timecode),
        })
    }
}

/// The SDK descriptor of an uncompressed frame. It borrows `data` and
/// `metadata` (null for none), which must outlive every use of it.
pub(crate) fn sdk_video_frame(
    layout: &ValidatedVideoLayout,
    header: &VideoHeader,
    data: *const u8,
    metadata: *const c_char,
    timestamp: Option<SdkTimestamp>,
) -> NDIlib_video_frame_v2_t {
    let (frame_rate_n, frame_rate_d) = header.frame_rate.to_sdk();
    NDIlib_video_frame_v2_t {
        xres: layout.sdk_width(),
        yres: layout.sdk_height(),
        FourCC: FourCc::from(layout.pixel_format).raw(),
        frame_rate_N: frame_rate_n,
        frame_rate_D: frame_rate_d,
        picture_aspect_ratio: header.picture_aspect_ratio,
        frame_format_type: header.scan_type.to_sdk(),
        timecode: header.timecode.ticks(),
        p_data: data.cast_mut(),
        __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
            line_stride_in_bytes: layout.sdk_line_stride(),
        },
        p_metadata: metadata,
        timestamp: SdkTimestamp::to_sdk(timestamp),
    }
}

/// An owned video frame with validated dimensions, format, stride, and buffer size.
///
/// Owned frames contain tightly packed active pixels: copies of borrowed or
/// received frames omit their row padding. Use [`Self::builder`] to create
/// one for sending. Cloning copies the pixels and metadata.
#[derive(Clone)]
pub struct VideoFrame {
    layout: ValidatedVideoLayout,
    header: VideoHeader,
    data: Vec<u8>,
    metadata: Option<FrameMetadata>,
    timestamp: Option<SdkTimestamp>,
}

impl fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("pixel_format", &self.pixel_format())
            .field("frame_rate", &self.frame_rate())
            .field("picture_aspect_ratio", &self.picture_aspect_ratio())
            .field("scan_type", &self.scan_type())
            .field("timecode", &self.timecode())
            .field("data (bytes)", &self.data.len())
            .field("line_stride_bytes", &self.line_stride_bytes())
            .field("metadata", &self.metadata())
            .field("timestamp", &self.timestamp())
            .finish()
    }
}

impl VideoFrame {
    /// Create a builder for a frame to send.
    ///
    /// Its defaults are 1920x1080 BGRA at 60/1 frames per second, a 16:9
    /// picture aspect ratio, progressive scan, [`Timecode::SYNTHESIZE`], no
    /// metadata and zero-filled pixels.
    pub fn builder() -> VideoFrameBuilder {
        VideoFrameBuilder {
            width: 1920,
            height: 1080,
            pixel_format: PixelFormat::Bgra,
            frame_rate: const { FrameRate::per_second(60) },
            picture_aspect_ratio: 16.0 / 9.0,
            scan_type: ScanType::Progressive,
            timecode: Timecode::SYNTHESIZE,
            metadata: None,
            data: None,
        }
    }

    /// Backing allocations retained when a frame enters a bounded send queue.
    #[cfg(feature = "advanced")]
    pub(crate) fn retained_bytes(&self) -> usize {
        self.data.capacity().saturating_add(
            self.metadata
                .as_ref()
                .map_or(0, |metadata| metadata.inner.as_bytes_with_nul().len()),
        )
    }

    /// The SDK descriptor of this frame, borrowing its pixels and metadata.
    pub(crate) fn to_raw(&self) -> NDIlib_video_frame_v2_t {
        sdk_video_frame(
            &self.layout,
            &self.header,
            self.data.as_ptr(),
            self.metadata
                .as_ref()
                .map_or(ptr::null(), FrameMetadata::as_ptr),
            self.timestamp,
        )
    }

    /// Get the frame width in pixels.
    pub fn width(&self) -> u32 {
        self.layout.width
    }

    /// Get the frame height in pixels.
    pub fn height(&self) -> u32 {
        self.layout.height
    }

    /// Get the supported pixel format.
    pub fn pixel_format(&self) -> PixelFormat {
        self.layout.pixel_format
    }

    /// Get the frame rate.
    pub fn frame_rate(&self) -> FrameRate {
        self.header.frame_rate
    }

    /// Get the picture aspect ratio, width over height as displayed; `0.0`
    /// means square pixels.
    pub fn picture_aspect_ratio(&self) -> f32 {
        self.header.picture_aspect_ratio
    }

    /// Get the scan type.
    pub fn scan_type(&self) -> ScanType {
        self.header.scan_type
    }

    /// Get the timecode.
    ///
    /// A built frame starts with [`Timecode::SYNTHESIZE`]; a copy of a
    /// received frame keeps the timecode it arrived with.
    pub fn timecode(&self) -> Timecode {
        self.header.timecode
    }

    /// Get the time the sender submitted this frame, when it was copied from
    /// a received frame that carries one. Frames built for sending have none.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        self.timestamp
    }

    /// Get the validated first-plane line stride in bytes.
    pub fn line_stride_bytes(&self) -> usize {
        self.layout.line_stride_bytes
    }

    /// Set the timecode without reallocating the pixels.
    pub fn set_timecode(&mut self, timecode: Timecode) {
        self.header.timecode = timecode;
    }

    /// Set the scan type without changing the frame's validated layout.
    pub fn set_scan_type(&mut self, scan_type: ScanType) {
        self.header.scan_type = scan_type;
    }

    /// Set the frame rate.
    pub fn set_frame_rate(&mut self, frame_rate: FrameRate) {
        self.header.frame_rate = frame_rate;
    }

    /// Set the picture aspect ratio, width over height as displayed; `0.0`
    /// means square pixels.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if `ratio` is negative or not finite.
    /// The frame is unchanged.
    pub fn set_picture_aspect_ratio(&mut self, ratio: f32) -> Result<()> {
        validate_picture_aspect_ratio(ratio)?;
        self.header.picture_aspect_ratio = ratio;
        Ok(())
    }

    pub(crate) fn validated_layout(&self) -> ValidatedVideoLayout {
        self.layout
    }

    pub(crate) fn header(&self) -> VideoHeader {
        self.header
    }

    /// Get the frame data: every plane, tightly packed, in SDK plane order.
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
    /// Returns `data`, unused, with [`Error::InvalidFrame`] if it is not
    /// exactly the validated layout size. The frame is unchanged.
    pub fn replace_data(&mut self, data: Vec<u8>) -> std::result::Result<(), Rejected<Vec<u8>>> {
        if let Err(error) = check_video_data_len(&self.layout, data.len()) {
            return Err(Rejected::new(error, data));
        }
        self.data = data;
        Ok(())
    }

    /// Get one plane in SDK memory order, or `None` for an invalid index.
    ///
    /// Plane order follows the pixel format, as for [`VideoPlane`]. The rows
    /// of an owned frame have no padding.
    pub fn plane(&self, index: usize) -> Option<VideoPlane<'_>> {
        // SAFETY: the frame's buffer covers its validated layout with every
        // byte initialized, and stays immutable while `self` is borrowed.
        unsafe { self.layout.view_plane(self.data.as_ptr(), index) }
    }

    /// Iterate planes in SDK memory order without allocating or copying.
    pub fn planes(&self) -> VideoPlanes<'_> {
        // SAFETY: as for `plane`.
        unsafe { self.layout.view_planes(self.data.as_ptr()) }
    }

    /// Get frame metadata as UTF-8 text, if present.
    pub fn metadata(&self) -> Option<&str> {
        self.metadata.as_ref().map(FrameMetadata::as_str)
    }

    pub(crate) fn metadata_cstr(&self) -> Option<&CStr> {
        self.metadata.as_ref().map(FrameMetadata::as_cstr)
    }

    pub(crate) fn validated_metadata(&self) -> ValidatedFrameMetadata {
        ValidatedFrameMetadata {
            len_with_nul: self
                .metadata_cstr()
                .and_then(|m| NonZeroUsize::new(m.to_bytes_with_nul().len())),
            text_len: self.metadata().map_or(0, str::len),
        }
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

    #[cfg(feature = "image-encoding")]
    fn image_source(&self) -> Result<ImagePixelSource<'_>> {
        ImagePixelSource::from_plane(self.layout, self.plane(0).expect("validated video plane"))
    }

    /// Encode the video frame as PNG bytes.
    ///
    /// This method encodes the frame to PNG format, automatically handling color format
    /// conversion from the NDI frame format (BGRA/RGBA/etc.) to PNG-compatible RGBA.
    ///
    /// # Supported Formats
    ///
    /// - `RGBA`: Direct active-row encoding without a pixel copy
    /// - `BGRA`: Swaps red and blue channels and preserves alpha
    /// - `RGBX`: Treats the fourth byte as padding and writes opaque alpha
    /// - `BGRX`: Swaps red/blue and writes opaque alpha
    /// - Other formats: Returns an error (unsupported for now)
    ///
    /// # Stride Handling
    ///
    /// This method consumes active pixels row-by-row according to the frame's
    /// validated line stride. Valid row padding is skipped. RGBA rows borrow
    /// directly; other supported formats reuse at most `width * 4` scratch bytes.
    /// Codec buffers and encoded output require additional memory.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The frame format is not RGBA/RGBX/BGRA/BGRX
    /// - PNG encoding fails
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Finder, FinderOptions, ReceiverOptions, Receiver, ReceiverColorFormat};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_millis(1000))?;
    /// # let sources = finder.current_sources()?;
    /// # let Some(source) = sources.first() else { return Ok(()); };
    /// # let options = ReceiverOptions::new(source.clone()).color_format(ReceiverColorFormat::RgbxRgba);
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// # let Some(video_frame) = receiver.video().capture(Duration::from_secs(5))? else { return Ok(()); };
    /// let png_bytes = video_frame.encode_png()?;
    /// std::fs::write("frame.png", &png_bytes)?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "image-encoding")]
    pub fn encode_png(&self) -> Result<Vec<u8>> {
        self.image_source()?.encode_png()
    }

    /// Encode the video frame as JPEG bytes with the specified quality.
    ///
    /// This method encodes the frame to JPEG format, automatically handling color format
    /// conversion from the NDI frame format into codec component rows without
    /// a full-frame RGB staging buffer. Codec/output storage is additional.
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
    /// - The quality is outside `1..=100`
    /// - The dimensions exceed JPEG's `u16` width/height range
    /// - JPEG encoding fails
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Finder, FinderOptions, ReceiverOptions, Receiver, ReceiverColorFormat};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_millis(1000))?;
    /// # let sources = finder.current_sources()?;
    /// # let Some(source) = sources.first() else { return Ok(()); };
    /// # let options = ReceiverOptions::new(source.clone()).color_format(ReceiverColorFormat::RgbxRgba);
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// # let Some(video_frame) = receiver.video().capture(Duration::from_secs(5))? else { return Ok(()); };
    /// let jpeg_bytes = video_frame.encode_jpeg(85)?;
    /// std::fs::write("frame.jpg", &jpeg_bytes)?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "image-encoding")]
    pub fn encode_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        self.image_source()?.encode_jpeg(quality)
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
    /// # use grafton_ndi::{Ndi, Finder, FinderOptions, ReceiverOptions, Receiver, ReceiverColorFormat, ImageFormat};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_millis(1000))?;
    /// # let sources = finder.current_sources()?;
    /// # let Some(source) = sources.first() else { return Ok(()); };
    /// # let options = ReceiverOptions::new(source.clone()).color_format(ReceiverColorFormat::RgbxRgba);
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// # let Some(video_frame) = receiver.video().capture(Duration::from_secs(5))? else { return Ok(()); };
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
        self.image_source()?.encode_data_url(format)
    }

    /// # Safety
    /// `c_frame` must describe one live allocation covering its SDK plane
    /// layout, with every active row initialized and immutable through this
    /// copy, and null or SDK-readable metadata.
    #[cfg(test)]
    pub(crate) unsafe fn from_raw(c_frame: &NDIlib_video_frame_v2_t) -> Result<VideoFrame> {
        let (layout, header) = validate_video_frame(c_frame)?;
        let metadata_layout = unsafe { validate_frame_metadata(c_frame.p_metadata)? };
        unsafe { Self::from_raw_validated(c_frame, layout, header, metadata_layout) }
    }

    /// Copy a validated SDK frame into tightly packed owned storage.
    ///
    /// # Safety
    ///
    /// `layout` and `header` must have been validated from `c_frame`, and
    /// `metadata_layout` from its metadata pointer. Its allocation must cover
    /// the layout, with every active row initialized and immutable throughout
    /// this call; row padding may be uninitialized.
    pub(crate) unsafe fn from_raw_validated(
        c_frame: &NDIlib_video_frame_v2_t,
        layout: ValidatedVideoLayout,
        header: VideoHeader,
        metadata_layout: ValidatedFrameMetadata,
    ) -> Result<VideoFrame> {
        let owned_layout = ValidatedVideoLayout::new_uncompressed(
            layout.pixel_format,
            layout.width,
            layout.height,
            None,
        )?;
        let mut data = Vec::with_capacity(layout.payload_len_bytes);
        // SAFETY: The caller retains the source allocation through this copy.
        // Only active rows are initialized by the SDK; never read stride padding.
        if layout.is_contiguous() {
            data.extend_from_slice(unsafe {
                slice::from_raw_parts(c_frame.p_data, layout.payload_len_bytes)
            });
        } else {
            for plane in unsafe { layout.view_planes(c_frame.p_data) } {
                for row in plane.rows() {
                    data.extend_from_slice(row);
                }
            }
        }

        let metadata =
            unsafe { FrameMetadata::copy_from_raw_validated(c_frame.p_metadata, metadata_layout) };

        Ok(VideoFrame {
            layout: owned_layout,
            header,
            data,
            metadata,
            timestamp: SdkTimestamp::from_sdk(c_frame.timestamp),
        })
    }
}

/// Builder for a [`VideoFrame`], reached through [`VideoFrame::builder`].
#[derive(Debug, Clone)]
pub struct VideoFrameBuilder {
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    frame_rate: FrameRate,
    picture_aspect_ratio: f32,
    scan_type: ScanType,
    timecode: Timecode,
    metadata: Option<String>,
    data: Option<Vec<u8>>,
}

impl VideoFrameBuilder {
    /// Set the video resolution. Width must be positive and even; height must
    /// be positive (and even for 4:2:0 formats).
    #[must_use]
    pub fn resolution(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Set the pixel format.
    #[must_use]
    pub fn pixel_format(mut self, pixel_format: PixelFormat) -> Self {
        self.pixel_format = pixel_format;
        self
    }

    /// Set the frame rate.
    #[must_use]
    pub fn frame_rate(mut self, frame_rate: FrameRate) -> Self {
        self.frame_rate = frame_rate;
        self
    }

    /// Set the picture aspect ratio, width over height as displayed; `0.0`
    /// means square pixels. It must be finite and nonnegative.
    #[must_use]
    pub fn picture_aspect_ratio(mut self, ratio: f32) -> Self {
        self.picture_aspect_ratio = ratio;
        self
    }

    /// Set the scan type (progressive, interlaced, etc.).
    #[must_use]
    pub fn scan_type(mut self, scan_type: ScanType) -> Self {
        self.scan_type = scan_type;
        self
    }

    /// Set the timecode.
    #[must_use]
    pub fn timecode(mut self, timecode: Timecode) -> Self {
        self.timecode = timecode;
        self
    }

    /// Set the metadata text.
    #[must_use]
    pub fn metadata<S: Into<String>>(mut self, metadata: S) -> Self {
        self.metadata = Some(metadata.into());
        self
    }

    /// Supply owned pixels without an intermediate zero-filled allocation.
    ///
    /// `build` requires exactly the validated layout size and retains this
    /// allocation unchanged. Use `data_mut` on the resulting frame for reuse.
    #[must_use]
    pub fn data(mut self, data: Vec<u8>) -> Self {
        self.data = Some(data);
        self
    }

    /// Build the video frame.
    ///
    /// Unset fields keep the defaults [`VideoFrame::builder`] lists. Building
    /// never loads or queries an NDI runtime: whether the runtime can send
    /// the pixel format is checked when the frame is sent.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if the resolution, pixel format,
    /// aspect ratio, metadata size or supplied data length do not form a
    /// valid frame, and [`Error::InvalidCString`] for metadata containing a
    /// NUL byte.
    pub fn build(self) -> Result<VideoFrame> {
        validate_picture_aspect_ratio(self.picture_aspect_ratio)?;
        let layout = ValidatedVideoLayout::new_uncompressed(
            self.pixel_format,
            self.width,
            self.height,
            None,
        )?;
        let metadata = self.metadata.map(FrameMetadata::new).transpose()?;
        let data = match self.data {
            Some(data) => {
                check_video_data_len(&layout, data.len())?;
                data
            }
            None => vec![0u8; layout.data_len_bytes],
        };

        Ok(VideoFrame {
            layout,
            header: VideoHeader {
                frame_rate: self.frame_rate,
                picture_aspect_ratio: self.picture_aspect_ratio,
                scan_type: self.scan_type,
                timecode: self.timecode,
            },
            data,
            metadata,
            timestamp: None,
        })
    }
}

/// Owned, tightly packed planar floating-point audio with a validated layout.
///
/// Channel samples are contiguous, with channels stored one after another.
/// Conversion from borrowed SDK audio omits inter-channel padding. Cloning
/// copies the samples and metadata.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    layout: ValidatedAudioLayout,
    timecode: Timecode,
    data: Vec<f32>,
    metadata: Option<FrameMetadata>,
    timestamp: Option<SdkTimestamp>,
}

impl AudioFrame {
    /// Create a builder for a frame to send.
    ///
    /// Its defaults are 48 kHz stereo FLTP with 1024 samples per channel,
    /// planar input, [`Timecode::SYNTHESIZE`], no metadata and silent
    /// samples.
    pub fn builder() -> AudioFrameBuilder {
        AudioFrameBuilder {
            sample_rate: 48_000,
            num_channels: 2,
            num_samples: 1024,
            timecode: Timecode::SYNTHESIZE,
            layout: AudioLayout::Planar,
            data: None,
            metadata: None,
        }
    }

    /// The SDK descriptor of this frame, borrowing its samples and metadata.
    pub(crate) fn to_raw(&self) -> NDIlib_audio_frame_v3_t {
        NDIlib_audio_frame_v3_t {
            sample_rate: self.layout.sdk_sample_rate(),
            no_channels: self.layout.sdk_num_channels(),
            no_samples: self.layout.sdk_num_samples(),
            timecode: self.timecode.ticks(),
            FourCC: self.format().to_sdk(),
            p_data: self.data.as_ptr().cast_mut().cast(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: self.layout.sdk_channel_stride(),
            },
            p_metadata: self
                .metadata
                .as_ref()
                .map_or(ptr::null(), FrameMetadata::as_ptr),
            timestamp: SdkTimestamp::to_sdk(self.timestamp),
        }
    }

    #[cfg(test)]
    /// # Safety
    /// `raw` must reference live SDK sample and metadata allocations with
    /// initialized active samples through this ownership copy.
    pub(crate) unsafe fn from_raw(raw: NDIlib_audio_frame_v3_t) -> Result<AudioFrame> {
        // Use the shared validation helper to validate and compute layout
        let layout = validate_audio_layout(&raw)?;
        let metadata_layout = unsafe { validate_frame_metadata(raw.p_metadata)? };

        unsafe { Self::from_raw_validated(raw, layout, metadata_layout) }
    }

    /// # Safety
    /// `layout` must have been validated from this exact `raw` descriptor.
    /// Its active samples must remain live, aligned, and initialized through
    /// copying; `metadata_layout` must describe its live metadata pointer.
    pub(crate) unsafe fn from_raw_validated(
        raw: NDIlib_audio_frame_v3_t,
        layout: ValidatedAudioLayout,
        metadata_layout: ValidatedFrameMetadata,
    ) -> Result<AudioFrame> {
        if layout.is_empty() {
            return Err(Error::InvalidFrame(
                "Cannot create owned AudioFrame from an empty audio layout".into(),
            ));
        }

        // Only channel samples are initialized audio. Never form a typed slice
        // over inter-channel padding supplied by the SDK.
        let packed_layout = layout.packed();
        let mut data = Vec::with_capacity(packed_layout.sample_count);
        // SAFETY: the SDK owns an aligned FLTP buffer with no_samples
        // initialized floats at each validated channel offset.
        for samples in unsafe { layout.channels(raw.p_data.cast::<f32>()) } {
            data.extend_from_slice(samples);
        }

        // Copy the string, don't take ownership - SDK will free the original.
        let metadata =
            unsafe { FrameMetadata::copy_from_raw_validated(raw.p_metadata, metadata_layout) };

        Ok(AudioFrame {
            layout: packed_layout,
            timecode: Timecode::from_ticks(raw.timecode),
            data,
            metadata,
            timestamp: SdkTimestamp::from_sdk(raw.timestamp),
        })
    }

    /// Get the sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.layout.sample_rate
    }

    /// Get the number of audio channels.
    pub fn num_channels(&self) -> u32 {
        self.layout.num_channels()
    }

    /// Get the number of samples per channel.
    pub fn num_samples(&self) -> u32 {
        self.layout.num_samples()
    }

    /// Get the timecode.
    ///
    /// A built frame starts with [`Timecode::SYNTHESIZE`]; a copy of a
    /// received frame keeps the timecode it arrived with.
    pub fn timecode(&self) -> Timecode {
        self.timecode
    }

    /// Get the time the sender submitted this frame, when it was copied from
    /// a received frame that carries one. Frames built for sending have none.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        self.timestamp
    }

    /// Set the timecode without reallocating the samples.
    pub fn set_timecode(&mut self, timecode: Timecode) {
        self.timecode = timecode;
    }

    /// Get the audio format.
    pub fn format(&self) -> AudioFormat {
        self.layout
            .format()
            .expect("owned AudioFrame always has a concrete audio format")
    }

    /// Get the distance between the starts of consecutive channels in bytes:
    /// one channel's samples, since an owned frame has no padding.
    pub fn channel_stride_bytes(&self) -> usize {
        self.layout.channel_stride_bytes()
    }

    /// Get tightly packed planar audio as 32-bit floats.
    /// Each channel's samples are contiguous; channels follow one another.
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
    /// Returns `data`, unused, with [`Error::InvalidFrame`] if it is not
    /// exactly the validated sample count. The frame is unchanged.
    pub fn replace_data(&mut self, data: Vec<f32>) -> std::result::Result<(), Rejected<Vec<f32>>> {
        if data.len() != self.layout.sample_count {
            let error = Error::InvalidFrame(format!(
                "Audio data length {}, expected {} samples for validated layout",
                data.len(),
                self.layout.sample_count
            ));
            return Err(Rejected::new(error, data));
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

    /// Get audio data for a specific channel.
    ///
    /// Data is always stored in planar format internally. If `AudioLayout::Interleaved`
    /// was specified at build time, the data was converted to planar during construction.
    pub fn channel_data(&self, channel: usize) -> Option<&[f32]> {
        let range = self.layout.channel_range(channel)?;
        Some(&self.data[range])
    }

    /// Get mutable samples for one channel without allocating.
    pub fn channel_data_mut(&mut self, channel: usize) -> Option<&mut [f32]> {
        let range = self.layout.channel_range(channel)?;
        Some(&mut self.data[range])
    }

    /// Iterate over the channels' samples, in channel order.
    pub fn channels(&self) -> impl ExactSizeIterator<Item = &[f32]> + '_ {
        // SAFETY: the frame's buffer holds every sample of its packed layout,
        // initialized, and stays immutable while `self` is borrowed.
        unsafe { self.layout.channels(self.data.as_ptr()) }
    }
}

/// Builder for an [`AudioFrame`], reached through [`AudioFrame::builder`].
#[derive(Debug, Clone)]
pub struct AudioFrameBuilder {
    sample_rate: u32,
    num_channels: u32,
    num_samples: u32,
    timecode: Timecode,
    layout: AudioLayout,
    data: Option<Vec<f32>>,
    metadata: Option<String>,
}

impl AudioFrameBuilder {
    /// Set the sample rate in Hz.
    #[must_use]
    pub fn sample_rate(mut self, sample_rate: u32) -> Self {
        self.sample_rate = sample_rate;
        self
    }

    /// Set the number of audio channels.
    #[must_use]
    pub fn num_channels(mut self, num_channels: u32) -> Self {
        self.num_channels = num_channels;
        self
    }

    /// Set the number of samples per channel.
    #[must_use]
    pub fn num_samples(mut self, num_samples: u32) -> Self {
        self.num_samples = num_samples;
        self
    }

    /// Set the timecode.
    #[must_use]
    pub fn timecode(mut self, timecode: Timecode) -> Self {
        self.timecode = timecode;
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
    ///     .num_channels(2)
    ///     .num_samples(100)
    ///     .layout(AudioLayout::Planar)
    ///     .build()
    ///     .unwrap();
    /// ```
    #[must_use]
    pub fn layout(mut self, layout: AudioLayout) -> Self {
        self.layout = layout;
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

    /// Set the metadata text.
    #[must_use]
    pub fn metadata<S: Into<String>>(mut self, metadata: S) -> Self {
        self.metadata = Some(metadata.into());
        self
    }

    /// Build the audio frame.
    ///
    /// Unset fields keep the defaults [`AudioFrame::builder`] lists. When
    /// `AudioLayout::Interleaved` is set, the input data is converted to
    /// planar format in Rust. The resulting frame always stores planar data
    /// with channels `num_samples * 4` bytes apart.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if the sample rate, channel or sample
    /// count, metadata size or supplied data length do not form a valid
    /// frame, and [`Error::InvalidCString`] for metadata containing a NUL
    /// byte.
    pub fn build(self) -> Result<AudioFrame> {
        let audio_layout = validate_outbound_audio_layout(
            self.sample_rate,
            self.num_channels,
            self.num_samples,
            AudioFormat::Fltp,
        )?;
        let sample_count = audio_layout.sample_count;

        let data = if let Some(input_data) = self.data {
            if input_data.len() != sample_count {
                return Err(Error::InvalidFrame(format!(
                    "Audio data length {}, expected {} ({}ch x {}samples)",
                    input_data.len(),
                    sample_count,
                    self.num_channels,
                    self.num_samples
                )));
            }

            match self.layout {
                AudioLayout::Planar => input_data,
                AudioLayout::Interleaved => {
                    let nc = audio_layout.no_channels;
                    let ns = audio_layout.no_samples;
                    // The validated layout and the length check above make
                    // `input_data` exactly `ns` frames of `nc` interleaved
                    // samples; channel `ch` is every `nc`th sample from `ch`.
                    let mut planar = vec![0.0f32; sample_count];
                    for (ch, out) in planar.chunks_exact_mut(ns).enumerate() {
                        for (dst, &sample) in
                            out.iter_mut().zip(input_data[ch..].iter().step_by(nc))
                        {
                            *dst = sample;
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
            timecode: self.timecode,
            data,
            metadata,
            timestamp: None,
        })
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
/// let format = AudioFormat::Fltp;
///
/// // When matching, always include a wildcard for forward compatibility
/// match format {
///     AudioFormat::Fltp => println!("32-bit float planar"),
///     _ => println!("Other format"),
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioFormat {
    /// 32-bit floating point planar audio (FLTP).
    Fltp,
}

impl AudioFormat {
    /// The SDK's FourCC for this format.
    pub(crate) const fn to_sdk(self) -> NDIlib_FourCC_audio_type_e {
        match self {
            Self::Fltp => NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
        }
    }
}

/// Audio data layout format
///
/// Determines how multi-channel audio samples are arranged in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// Interleaved data is converted to planar format in Rust at build time,
    /// so the resulting `AudioFrame` always stores planar data.
    Interleaved,
}

/// Maximum allowed size for supported video frame data (256 MiB).
pub(crate) const MAX_VIDEO_BYTES: usize = 256 * 1024 * 1024;

/// Maximum allowed size for audio frame data (64 MiB).
/// Comfortably above typical NDI audio frames while preventing unbounded allocations.
pub(crate) const MAX_AUDIO_BYTES: usize = 64 * 1024 * 1024;

/// Maximum allowed size for SDK metadata C strings (4 MiB), including the
/// trailing NUL terminator.
pub(crate) const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;

/// Owned UTF-8 metadata text and its timecode.
///
/// NDI expects XML when sending; callers are responsible for XML syntax. Text
/// cannot contain NUL bytes and is limited to 4 MiB including the SDK terminator.
///
/// # Examples
///
/// ```
/// use grafton_ndi::{MetadataFrame, Timecode};
///
/// let frame = MetadataFrame::new("<tally on_program=\"true\"/>")?
///     .with_timecode(Timecode::from_ticks(0));
/// assert_eq!(frame.data(), "<tally on_program=\"true\"/>");
/// assert_eq!(frame.timecode().ticks(), 0);
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MetadataFrame {
    data: FrameMetadata,
    timecode: Timecode,
}

impl MetadataFrame {
    /// Create a metadata frame from UTF-8 text, with
    /// [`Timecode::SYNTHESIZE`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] if `data` contains an interior NUL byte
    /// or [`Error::InvalidFrame`] if the SDK metadata length would exceed the
    /// crate's metadata size limit.
    pub fn new(data: impl Into<String>) -> Result<Self> {
        Ok(MetadataFrame {
            data: FrameMetadata::new(data)?,
            timecode: Timecode::SYNTHESIZE,
        })
    }

    /// Return this frame with `timecode`.
    #[must_use]
    pub fn with_timecode(mut self, timecode: Timecode) -> Self {
        self.timecode = timecode;
        self
    }

    /// Get the metadata text.
    pub fn data(&self) -> &str {
        self.data.as_str()
    }

    /// Consume the frame and return the owned metadata text.
    pub fn into_data(self) -> String {
        self.data.into_string()
    }

    /// Get the timecode.
    ///
    /// A new frame starts with [`Timecode::SYNTHESIZE`]; a received frame
    /// keeps the timecode it arrived with.
    pub fn timecode(&self) -> Timecode {
        self.timecode
    }

    /// Replace the metadata text.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] if `data` contains an interior NUL byte
    /// or [`Error::InvalidFrame`] if the SDK metadata length would exceed the
    /// crate's metadata size limit. The frame is unchanged.
    pub fn set_data(&mut self, data: impl Into<String>) -> Result<()> {
        self.data = FrameMetadata::new(data)?;
        Ok(())
    }

    /// Set the timecode.
    pub fn set_timecode(&mut self, timecode: Timecode) {
        self.timecode = timecode;
    }

    /// Borrow the retained C string for a synchronous SDK call. The caller
    /// must keep this frame alive until the SDK call returns.
    pub(crate) fn to_raw(&self) -> NDIlib_metadata_frame_t {
        NDIlib_metadata_frame_t {
            length: self.data.inner.as_bytes_with_nul().len() as i32,
            timecode: self.timecode.ticks(),
            p_data: self.data.as_ptr() as *mut c_char,
        }
    }

    /// Create from raw NDI metadata frame (copies the data)
    ///
    /// # Safety
    ///
    /// `raw` must be an SDK-populated metadata frame whose `p_data` pointer is
    /// valid for `raw.length` bytes when `length > 0`. For zero length with a
    /// non-null pointer, it must be readable through its first NUL terminator
    /// or the crate's metadata cap, as required by `validate_frame_metadata`.
    #[cfg(test)]
    pub(crate) unsafe fn from_raw(raw: &NDIlib_metadata_frame_t) -> Result<Self> {
        let layout = unsafe { validate_metadata_layout(raw)? };
        Ok(unsafe { Self::from_raw_validated(raw, layout) })
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
        let data = if layout.len_with_nul == 0 {
            FrameMetadata::new("").expect("empty metadata is valid")
        } else {
            unsafe {
                FrameMetadata::copy_from_raw_validated(
                    raw.p_data,
                    ValidatedFrameMetadata {
                        len_with_nul: std::num::NonZeroUsize::new(layout.len_with_nul),
                        text_len: layout.text_len,
                    },
                )
                .expect("non-null metadata")
            }
        };

        Self {
            data,
            timecode: Timecode::from_ticks(raw.timecode),
        }
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
/// # Safety
/// `raw.p_data`, when non-null, must reference readable storage for the
/// declared length or a NUL-terminated string within the metadata cap.
pub(crate) unsafe fn validate_metadata_layout(
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

        // The SDK permits zero length as a C-string sentinel. Apply the same
        // bounded scan and SDK pointer-validity contract as per-frame metadata.
        let metadata = unsafe { validate_frame_metadata(raw.p_data)? };
        return Ok(ValidatedMetadataLayout {
            len_with_nul: metadata.len_with_nul.expect("non-null metadata").get(),
            text_len: metadata.text_len,
        });
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

/// # Safety
/// `layout` must describe this exact live `raw.p_data` allocation, which must
/// remain immutable for the returned borrow.
unsafe fn metadata_payload_bytes(
    raw: &NDIlib_metadata_frame_t,
    layout: ValidatedMetadataLayout,
) -> &[u8] {
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
#[derive(Clone, PartialEq, Eq, Hash)]
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
        // SAFETY: construction accepts String and validated SDK UTF-8 only.
        unsafe { str::from_utf8_unchecked(self.inner.as_bytes()) }
    }

    pub(crate) fn into_string(self) -> String {
        self.inner
            .into_string()
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

        let mut bytes = Vec::with_capacity(metadata_layout.text_len + 1);
        if metadata_layout.text_len != 0 {
            bytes.extend_from_slice(unsafe {
                slice::from_raw_parts(p_metadata.cast::<u8>(), metadata_layout.text_len)
            });
        }
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

impl ValidatedFrameMetadata {
    /// Bounded application CStr validation uses the known slice length, never a raw scan.
    pub(crate) fn from_cstr(metadata: Option<&CStr>) -> Result<Self> {
        let Some(metadata) = metadata else {
            return Ok(Self {
                len_with_nul: None,
                text_len: 0,
            });
        };
        let len = metadata.to_bytes_with_nul().len();
        validate_metadata_len_with_nul(len)?;
        metadata
            .to_str()
            .map_err(|e| Error::InvalidUtf8(e.to_string()))?;
        Ok(Self {
            len_with_nul: NonZeroUsize::new(len),
            text_len: len - 1,
        })
    }
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

    // SAFETY: forwarded to the caller's obligation for a non-null pointer.
    let Some(payload) = (unsafe { bounded_sdk_text(p_metadata) }) else {
        return Err(Error::InvalidFrame(format!(
            "Frame metadata is missing a NUL terminator within {MAX_METADATA_BYTES} bytes"
        )));
    };
    str::from_utf8(payload).map_err(|err| Error::InvalidUtf8(err.to_string()))?;
    Ok(ValidatedFrameMetadata {
        len_with_nul: NonZeroUsize::new(payload.len() + 1),
        text_len: payload.len(),
    })
}

/// The bytes of an SDK string before its first NUL, or `None` when there is
/// no NUL within the first [`MAX_METADATA_BYTES`] bytes.
///
/// The scan reads one byte at a time and stops at the first NUL, so it never
/// reads past the string, and it reads at most `MAX_METADATA_BYTES` bytes.
///
/// # Safety
///
/// `text` must be non-null and readable byte-by-byte for `'a` until either
/// its first NUL or `MAX_METADATA_BYTES` bytes have been read.
pub(crate) unsafe fn bounded_sdk_text<'a>(text: *const c_char) -> Option<&'a [u8]> {
    let bytes = text.cast::<u8>();
    // SAFETY: each byte read is at or before the first NUL and within the
    // bound, which the caller guarantees readable.
    let len = (0..MAX_METADATA_BYTES).find(|&len| unsafe { bytes.add(len).read() } == 0)?;
    // SAFETY: the `len` bytes before the NUL were just read, and the caller
    // keeps them readable for `'a`.
    Some(unsafe { slice::from_raw_parts(bytes, len) })
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
///
/// This enum is marked `#[non_exhaustive]` so that additional encoders can be
/// added without a breaking change. Match arms should include a wildcard `_`
/// pattern.
#[cfg(feature = "image-encoding")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
            PixelFormat::Rgba => Ok(Self {
                channel_order: ImageChannelOrder::Rgba,
                alpha_policy: ImageAlphaPolicy::Preserve,
            }),
            PixelFormat::Bgra => Ok(Self {
                channel_order: ImageChannelOrder::Bgra,
                alpha_policy: ImageAlphaPolicy::Preserve,
            }),
            PixelFormat::Rgbx => Ok(Self {
                channel_order: ImageChannelOrder::Rgba,
                alpha_policy: ImageAlphaPolicy::Opaque,
            }),
            PixelFormat::Bgrx => Ok(Self {
                channel_order: ImageChannelOrder::Bgra,
                alpha_policy: ImageAlphaPolicy::Opaque,
            }),
            _ => Err(Error::InvalidFrame(format!(
                "Unsupported format for image encoding: {pixel_format:?}. Only RGBA/RGBX/BGRA/BGRX are supported."
            ))),
        }
    }

    fn can_borrow_png_rows(self) -> bool {
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
    plane: VideoPlane<'a>,
    width: usize,
    height: usize,
    pixel_format: ImagePixelFormat,
}

#[cfg(feature = "image-encoding")]
impl<'a> ImagePixelSource<'a> {
    fn encode_png(&self) -> Result<Vec<u8>> {
        use png::{BitDepth, ColorType, Encoder};

        use std::io::Write;

        let width = u32::try_from(self.width).map_err(|_| {
            Error::InvalidFrame(format!("PNG width {} exceeds u32 range", self.width))
        })?;
        let height = u32::try_from(self.height).map_err(|_| {
            Error::InvalidFrame(format!("PNG height {} exceeds u32 range", self.height))
        })?;
        let mut conversion = self.png_conversion_row()?;

        // Encode to PNG
        let mut png_data = Vec::new();
        let mut encoder = Encoder::new(&mut png_data, width, height);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);

        let png_error = |e| Error::InvalidFrame(format!("PNG encoding failed: {e}"));
        let mut writer = encoder.write_header().map_err(png_error)?;
        {
            let mut stream = writer.stream_writer().map_err(png_error)?;
            for row in self.plane.rows() {
                let rgba = if self.pixel_format.can_borrow_png_rows() {
                    row
                } else {
                    for (out, pixel) in conversion
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(row.as_chunks::<{ ImagePixelFormat::BYTES_PER_PIXEL }>().0)
                    {
                        *out = self.pixel_format.rgba_pixel(pixel);
                    }
                    &conversion
                };
                stream
                    .write_all(rgba)
                    .map_err(|e| Error::InvalidFrame(format!("PNG encoding failed: {e}")))?;
            }
            stream.finish().map_err(png_error)?;
        }
        writer.finish().map_err(png_error)?;

        Ok(png_data)
    }

    fn encode_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        use jpeg_encoder::Encoder as JpegEncoder;

        let (width, height) = self.jpeg_dimensions(quality)?;

        // Encode to JPEG
        let mut jpeg_data = Vec::new();
        let encoder = JpegEncoder::new(&mut jpeg_data, quality);
        encoder
            .encode_image(JpegRows {
                source: self,
                width,
                height,
            })
            .map_err(|e| Error::InvalidFrame(format!("JPEG encoding failed: {e}")))?;

        Ok(jpeg_data)
    }

    fn encode_data_url(&self, format: ImageFormat) -> Result<String> {
        use base64::{engine::general_purpose::STANDARD, Engine};

        let (mime_type, image_bytes) = match format {
            ImageFormat::Png => ("image/png", self.encode_png()?),
            ImageFormat::Jpeg(quality) => ("image/jpeg", self.encode_jpeg(quality)?),
        };

        let base64_data = STANDARD.encode(&image_bytes);
        Ok(format!("data:{mime_type};base64,{base64_data}"))
    }

    fn from_plane(layout: ValidatedVideoLayout, plane: VideoPlane<'a>) -> Result<Self> {
        let pixel_format = ImagePixelFormat::from_pixel_format(layout.pixel_format)?;
        Ok(Self {
            plane,
            width: layout.width as usize,
            height: layout.height as usize,
            pixel_format,
        })
    }

    fn png_conversion_row(&self) -> Result<Vec<u8>> {
        if self.pixel_format.can_borrow_png_rows() {
            return Ok(Vec::new());
        }
        let len = self.width.checked_mul(4).ok_or_else(|| {
            Error::InvalidFrame(format!("PNG row size overflow: {}x4", self.width))
        })?;
        Ok(vec![0; len])
    }

    fn jpeg_dimensions(&self, quality: u8) -> Result<(u16, u16)> {
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

        Ok((width, height))
    }
}

/// Reads validated active rows directly into the codec's component buffers.
/// No wrapper RGB staging allocation, including for padded captured frames.
#[cfg(feature = "image-encoding")]
struct JpegRows<'s, 'a> {
    source: &'s ImagePixelSource<'a>,
    width: u16,
    height: u16,
}

#[cfg(feature = "image-encoding")]
impl jpeg_encoder::ImageBuffer for JpegRows<'_, '_> {
    fn get_jpeg_color_type(&self) -> jpeg_encoder::JpegColorType {
        jpeg_encoder::JpegColorType::Ycbcr
    }

    fn width(&self) -> u16 {
        self.width
    }
    fn height(&self) -> u16 {
        self.height
    }

    fn fill_buffers(&self, row: u16, buffers: &mut [Vec<u8>; 4]) {
        let row = self
            .source
            .plane
            .row(usize::from(row))
            .expect("codec row is in bounds");
        // Like the codec's slice adapter, batch 16 pixels so LLVM can vectorize
        // conversion and append components without per-pixel Vec growth checks.
        for chunk in row.as_chunks::<64>().0 {
            let mut components = [[0; 16]; 3];
            for (i, pixel) in chunk.as_chunks::<4>().0.iter().enumerate() {
                let [r, g, b] = self.source.pixel_format.rgb_pixel(pixel);
                let (y, cb, cr) = jpeg_encoder::rgb_to_ycbcr(r, g, b);
                components[0][i] = y;
                components[1][i] = cb;
                components[2][i] = cr;
            }
            for (buffer, values) in buffers.iter_mut().zip(components) {
                buffer.extend_from_slice(&values);
            }
        }
        for pixel in row.as_chunks::<64>().1.as_chunks::<4>().0 {
            let [r, g, b] = self.source.pixel_format.rgb_pixel(pixel);
            let (y, cb, cr) = jpeg_encoder::rgb_to_ycbcr(r, g, b);
            buffers[0].push(y);
            buffers[1].push(cb);
            buffers[2].push(cr);
        }
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
    pub width: u32,
    /// The validated height in pixels.
    pub height: u32,
    /// The validated pixel format.
    pub pixel_format: PixelFormat,
    /// Complete storage extent in bytes; SDK padding need not be initialized.
    pub data_len_bytes: usize,
    /// Initialized active payload, excluding all row padding.
    payload_len_bytes: usize,
    plane_layouts: [VideoPlaneLayout; MAX_VIDEO_PLANES],
    plane_count: usize,
    /// The validated first-plane line stride in bytes.
    pub line_stride_bytes: usize,
}

// The width, height and line stride of a valid layout are each at most its
// storage extent, so every one of them fits the SDK's signed 32-bit fields.
const _: () = assert!(MAX_VIDEO_BYTES <= i32::MAX as usize);

impl ValidatedVideoLayout {
    /// Validate an uncompressed layout. A `line_stride` of `None` or zero
    /// selects the packed stride.
    pub(crate) fn new_uncompressed(
        pixel_format: PixelFormat,
        width: u32,
        height: u32,
        line_stride: Option<usize>,
    ) -> Result<Self> {
        validate_video_dimensions(width.into(), height.into())?;

        let min_stride = min_video_line_stride_checked(pixel_format, width as usize)?;
        let line_stride = match line_stride {
            None | Some(0) => min_stride,
            Some(stride) if stride < min_stride => {
                return Err(Error::InvalidFrame(format!(
                    "Video line_stride_in_bytes {stride} is smaller than minimum row size {min_stride} for {pixel_format:?} width {width}"
                )));
            }
            Some(stride) => stride,
        };

        let (plane_layouts, plane_count, data_len_bytes, payload_len_bytes) = video_plane_layouts(
            pixel_format.info(),
            line_stride,
            min_stride,
            height as usize,
        )?;
        validate_video_data_len(data_len_bytes)?;

        Ok(Self {
            width,
            height,
            pixel_format,
            data_len_bytes,
            payload_len_bytes,
            plane_layouts,
            plane_count,
            line_stride_bytes: line_stride,
        })
    }

    /// Validate the layout of an uncompressed SDK frame: `width`, `height`
    /// and `line_stride` are the SDK's signed fields, and a zero stride
    /// selects the packed stride.
    pub(crate) fn from_sdk(
        pixel_format: PixelFormat,
        width: i32,
        height: i32,
        line_stride: i32,
    ) -> Result<Self> {
        validate_video_dimensions(width.into(), height.into())?;
        let line_stride = usize::try_from(line_stride).map_err(|_| {
            Error::InvalidFrame(format!(
                "Uncompressed video frame has invalid line_stride_in_bytes: {line_stride}"
            ))
        })?;
        // Both dimensions were just checked positive.
        Self::new_uncompressed(
            pixel_format,
            width.unsigned_abs(),
            height.unsigned_abs(),
            Some(line_stride),
        )
    }

    /// The SDK's `xres`.
    pub(crate) fn sdk_width(&self) -> i32 {
        self.width as i32
    }

    /// The SDK's `yres`.
    pub(crate) fn sdk_height(&self) -> i32 {
        self.height as i32
    }

    /// The SDK's `line_stride_in_bytes`.
    pub(crate) fn sdk_line_stride(&self) -> i32 {
        self.line_stride_bytes as i32
    }

    fn planes(&self) -> &[VideoPlaneLayout] {
        &self.plane_layouts[..self.plane_count]
    }

    fn is_contiguous(&self) -> bool {
        // Validated planes are adjacent storage regions with active <= stride.
        // Their last active endpoint equals payload length exactly when there
        // are no intervening gaps. Trailing padding need not be included.
        let last = self.planes().last().expect("validated video has a plane");
        last.offset + (last.row_count - 1) * last.row_stride + last.active_row_bytes
            == self.payload_len_bytes
    }

    /// One plane of the frame stored at `data`, or `None` for an invalid
    /// index. Owned, borrowed and received frames share this view.
    ///
    /// # Safety
    ///
    /// `data` must point to storage this layout describes, with every active
    /// row initialized and immutable for `'a`.
    unsafe fn view_plane<'a>(&'a self, data: *const u8, index: usize) -> Option<VideoPlane<'a>> {
        let layout = *self.planes().get(index)?;
        // SAFETY: forwarded to the caller.
        Some(unsafe { VideoPlane::new(data, layout) })
    }

    /// Every plane of the frame stored at `data`, in SDK memory order.
    ///
    /// # Safety
    ///
    /// As for [`Self::view_plane`].
    unsafe fn view_planes<'a>(&'a self, data: *const u8) -> VideoPlanes<'a> {
        VideoPlanes {
            data,
            layouts: self.planes().iter(),
        }
    }
}

/// Validate an uncompressed video frame the SDK delivered: its pointer,
/// header, pixel format and layout.
///
/// Performs checked arithmetic for the buffer size and enforces
/// `MAX_VIDEO_BYTES`, returning [`Error::InvalidFrame`] for any violation.
pub(crate) fn validate_video_frame(
    raw: &NDIlib_video_frame_v2_t,
) -> Result<(ValidatedVideoLayout, VideoHeader)> {
    if raw.p_data.is_null() {
        return Err(Error::InvalidFrame(
            "Video frame has null data pointer".into(),
        ));
    }

    let header = VideoHeader::from_sdk(raw)?;
    let pixel_format = PixelFormat::try_from(FourCc::from_raw(raw.FourCC))?;
    // SAFETY: an uncompressed frame's union holds its line stride.
    let line_stride = unsafe { raw.__bindgen_anon_1.line_stride_in_bytes };
    let layout = ValidatedVideoLayout::from_sdk(pixel_format, raw.xres, raw.yres, line_stride)?;
    Ok((layout, header))
}

/// The one dimension check, for values from the API and from the SDK.
fn validate_video_dimensions(width: i64, height: i64) -> Result<()> {
    validate_video_width(width)?;
    if height <= 0 {
        return Err(Error::InvalidFrame(format!(
            "Video frame has invalid height: {height}"
        )));
    }

    Ok(())
}

fn validate_video_width(width: i64) -> Result<()> {
    if width <= 0 {
        return Err(Error::InvalidFrame(format!(
            "Video frame has invalid width: {width}"
        )));
    }

    // The SDK uses 4:2:2 internally, including for RGB input/output.
    // https://docs.ndi.video/all/developing-with-ndi/sdk/frame-types
    if width % 2 != 0 {
        return Err(Error::InvalidFrame(format!(
            "NDI video frames require even width, got {width}"
        )));
    }

    Ok(())
}

/// The one picture-aspect-ratio check: finite and nonnegative, where zero
/// means square pixels.
pub(crate) fn validate_picture_aspect_ratio(ratio: f32) -> Result<()> {
    if !ratio.is_finite() || ratio < 0.0 {
        return Err(Error::InvalidFrame(format!(
            "Video frame has invalid picture aspect ratio: {ratio}"
        )));
    }
    Ok(())
}

/// Require supplied pixels to fill `layout` exactly.
fn check_video_data_len(layout: &ValidatedVideoLayout, len: usize) -> Result<()> {
    if len != layout.data_len_bytes {
        return Err(Error::InvalidFrame(format!(
            "Video data length {len}, expected {} bytes for validated layout",
            layout.data_len_bytes
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
#[cfg(test)]
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
    video_plane_layouts(info, y_stride, y_stride, height).map(|(_, _, storage, _)| storage)
}

#[derive(Debug, Clone, Copy, Default)]
struct VideoPlaneLayout {
    offset: usize,
    row_stride: usize,
    active_row_bytes: usize,
    row_count: usize,
}

/// Derive SDK plane offsets and active row sizes together. See
/// https://docs.ndi.video/all/developing-with-ndi/sdk/frame-types .
/// Plane offsets include preceding storage padding; payload lengths do not.
const MAX_VIDEO_PLANES: usize = 4;

fn video_plane_layouts(
    info: PixelFormatInfo,
    stride: usize,
    active: usize,
    height: usize,
) -> Result<([VideoPlaneLayout; MAX_VIDEO_PLANES], usize, usize, usize)> {
    let mut dimensions = [(stride, active, height); MAX_VIDEO_PLANES];
    let count = match info.category() {
        FormatCategory::Packed => 1,
        FormatCategory::Planar420 => {
            if !height.is_multiple_of(2) || !stride.is_multiple_of(2) {
                return Err(Error::InvalidFrame(format!(
                    "Planar 4:2:0 (YV12/I420) video frames require even height and line stride, got height {height} and stride {stride}"
                )));
            }
            dimensions[1] = (stride / 2, active / 2, height / 2);
            dimensions[2] = dimensions[1];
            3
        }
        FormatCategory::SemiPlanar420 => {
            if !height.is_multiple_of(2) {
                return Err(Error::InvalidFrame(format!(
                    "Semi-planar 4:2:0 (NV12) video frames require even height, got {height}"
                )));
            }
            dimensions[1] = (stride, active, height / 2);
            2
        }
        FormatCategory::PackedAlpha => {
            if !stride.is_multiple_of(2) {
                return Err(Error::InvalidFrame(format!(
                    "UYVA video frames require even line stride, got {stride}"
                )));
            }
            dimensions[1] = (stride / 2, active / 2, height);
            2
        }
        FormatCategory::SemiPlanar422 => 2,
        FormatCategory::SemiPlanar422Alpha | FormatCategory::Planar444 => 3,
        FormatCategory::Planar444Alpha => 4,
    };
    let mut planes = [VideoPlaneLayout::default(); MAX_VIDEO_PLANES];
    let mut storage = 0usize;
    let mut payload = 0usize;
    for (plane, &(row_stride, active_row_bytes, row_count)) in
        planes.iter_mut().zip(&dimensions[..count])
    {
        *plane = VideoPlaneLayout {
            offset: storage,
            row_stride,
            active_row_bytes,
            row_count,
        };
        storage = row_stride
            .checked_mul(row_count)
            .and_then(|n| storage.checked_add(n))
            .ok_or_else(|| Error::InvalidFrame("Video buffer size overflow".into()))?;
        payload = active_row_bytes
            .checked_mul(row_count)
            .and_then(|n| payload.checked_add(n))
            .ok_or_else(|| Error::InvalidFrame("Video payload size overflow".into()))?;
    }
    Ok((planes, count, storage, payload))
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
    /// The validated sample rate: positive, or zero only for an empty
    /// no-source state; at most `i32::MAX`.
    pub sample_rate: u32,
    /// The validated channel count.
    pub no_channels: usize,
    /// The validated samples per channel.
    pub no_samples: usize,
    /// The validated channel stride in `f32` samples.
    pub channel_stride_samples: usize,
    /// The validated backing sample count. For strided planar audio this may be
    /// larger than `no_channels * no_samples` because it includes inter-channel
    /// padding up to the last channel's final sample.
    pub sample_count: usize,
}

// The channel count, sample count and channel stride in bytes of a valid
// layout are each at most its byte extent, and the sample rate is checked
// against i32::MAX, so every one of them fits the SDK's signed 32-bit fields.
// The channel count of an empty layout comes from such a field.
const _: () = assert!(MAX_AUDIO_BYTES <= i32::MAX as usize);

impl ValidatedAudioLayout {
    fn from_packed(
        format: Option<AudioFormat>,
        sample_rate: u32,
        packed: &PackedAudioDimensions,
    ) -> Self {
        Self {
            format,
            sample_rate,
            no_channels: packed.no_channels,
            no_samples: packed.no_samples,
            channel_stride_samples: packed.no_samples,
            sample_count: packed.sample_count,
        }
    }

    /// The same frame with channels stored back to back, without padding.
    fn packed(self) -> Self {
        Self {
            channel_stride_samples: self.no_samples,
            sample_count: self.no_channels * self.no_samples,
            ..self
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        self.sample_count == 0
    }

    pub(crate) fn format(self) -> Option<AudioFormat> {
        self.format
    }

    fn num_channels(self) -> u32 {
        self.no_channels as u32
    }

    fn num_samples(self) -> u32 {
        self.no_samples as u32
    }

    fn channel_stride_bytes(self) -> usize {
        self.channel_stride_samples * std::mem::size_of::<f32>()
    }

    /// The SDK's `sample_rate`.
    fn sdk_sample_rate(self) -> i32 {
        self.sample_rate as i32
    }

    /// The SDK's `no_channels`.
    fn sdk_num_channels(self) -> i32 {
        self.no_channels as i32
    }

    /// The SDK's `no_samples`.
    fn sdk_num_samples(self) -> i32 {
        self.no_samples as i32
    }

    /// The SDK's `channel_stride_in_bytes`.
    fn sdk_channel_stride(self) -> i32 {
        self.channel_stride_bytes() as i32
    }

    pub(crate) fn channel_range(self, channel: usize) -> Option<std::ops::Range<usize>> {
        if self.is_empty() || channel >= self.no_channels {
            return None;
        }

        let start = channel.checked_mul(self.channel_stride_samples)?;
        let end = start.checked_add(self.no_samples)?;

        (end <= self.sample_count).then_some(start..end)
    }

    /// One channel's samples of the frame stored at `data`, or `None` for an
    /// invalid channel. Owned and received frames share this view.
    ///
    /// # Safety
    ///
    /// `data` must point to `f32`-aligned storage this layout describes, with
    /// every channel's samples initialized and immutable for `'a`.
    unsafe fn channel<'a>(self, data: *const f32, channel: usize) -> Option<&'a [f32]> {
        let range = self.channel_range(channel)?;
        // SAFETY: only this channel's initialized sample region is borrowed.
        Some(unsafe { slice::from_raw_parts(data.add(range.start), self.no_samples) })
    }

    /// Every channel's samples of the frame stored at `data`, in order,
    /// without reading inter-channel padding. An empty frame has none.
    ///
    /// # Safety
    ///
    /// As for [`Self::channel`].
    unsafe fn channels<'a>(self, data: *const f32) -> impl ExactSizeIterator<Item = &'a [f32]> {
        let count = if self.is_empty() { 0 } else { self.no_channels };
        // SAFETY: forwarded to the caller, for channels that all exist.
        (0..count)
            .map(move |channel| unsafe { self.channel(data, channel) }.expect("validated channel"))
    }
}

fn validate_outbound_audio_layout(
    sample_rate: u32,
    no_channels: u32,
    no_samples: u32,
    format: AudioFormat,
) -> Result<ValidatedAudioLayout> {
    let sample_rate = validate_audio_sample_rate(sample_rate.into())?;
    let packed = validate_packed_audio_dimensions(no_channels, no_samples)?;

    Ok(ValidatedAudioLayout::from_packed(
        Some(format),
        sample_rate,
        &packed,
    ))
}

/// The one sample-rate check for every audio path, for values from the API
/// and from the SDK: `1..=i32::MAX` Hz.
pub(crate) fn validate_audio_sample_rate(sample_rate: i64) -> Result<u32> {
    u32::try_from(sample_rate)
        .ok()
        .filter(|rate| (1..=i32::MAX.unsigned_abs()).contains(rate))
        .ok_or_else(|| Error::InvalidFrame(format!("Invalid sample rate: {sample_rate}")))
}

/// Require a positive channel and sample count and widen both to `usize`.
fn validate_positive_audio_counts(no_channels: i64, no_samples: i64) -> Result<(usize, usize)> {
    let count = |value: i64, name: &str| {
        usize::try_from(value)
            .ok()
            .filter(|&count| count > 0)
            .ok_or_else(|| Error::InvalidFrame(format!("Invalid number of {name}: {value}")))
    };
    Ok((
        count(no_channels, "channels")?,
        count(no_samples, "samples")?,
    ))
}

/// The one audio extent calculation. Returns the backing sample count from the
/// first sample of the first channel to the last sample of the last channel
/// when channels start `stride_samples` apart, enforcing `MAX_AUDIO_BYTES`.
/// Packed audio is the case `stride_samples == no_samples`.
fn checked_audio_extent(
    no_channels: usize,
    no_samples: usize,
    stride_samples: usize,
) -> Result<usize> {
    let last_channel_offset = no_channels
        .checked_sub(1)
        .and_then(|last| last.checked_mul(stride_samples))
        .ok_or_else(|| {
            Error::InvalidFrame(format!(
                "Audio channel offset overflow: {no_channels} channels × {stride_samples} stride samples"
            ))
        })?;
    let sample_count = last_channel_offset.checked_add(no_samples).ok_or_else(|| {
        Error::InvalidFrame(format!(
            "Audio backing sample count overflow: channel offset {last_channel_offset} + {no_samples} samples"
        ))
    })?;
    let byte_size = sample_count
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| {
            Error::InvalidFrame(format!(
                "Audio byte size overflow: {sample_count} samples × {} bytes",
                std::mem::size_of::<f32>()
            ))
        })?;

    if byte_size > MAX_AUDIO_BYTES {
        return Err(Error::InvalidFrame(format!(
            "Audio frame exceeds maximum size: {byte_size} bytes > {MAX_AUDIO_BYTES} bytes"
        )));
    }

    Ok(sample_count)
}

/// Packed FLTP dimensions shared by owned frames and FrameSync requests.
/// Checking this layout allocates nothing on success and never allocates samples.
pub(crate) struct PackedAudioDimensions {
    no_channels: usize,
    no_samples: usize,
    sample_count: usize,
}

impl PackedAudioDimensions {
    /// The validated total `f32` sample count (`channels * samples`).
    pub(crate) fn sample_count(&self) -> usize {
        self.sample_count
    }
}

pub(crate) fn validate_packed_audio_dimensions(
    no_channels: u32,
    no_samples: u32,
) -> Result<PackedAudioDimensions> {
    let (no_channels, no_samples) =
        validate_positive_audio_counts(no_channels.into(), no_samples.into())?;
    if no_samples
        .checked_mul(std::mem::size_of::<f32>())
        .and_then(|bytes| i32::try_from(bytes).ok())
        .is_none()
    {
        return Err(Error::InvalidFrame(format!(
            "Audio channel stride exceeds i32 range: {no_samples} samples"
        )));
    }
    let sample_count = checked_audio_extent(no_channels, no_samples, no_samples)?;

    Ok(PackedAudioDimensions {
        no_channels,
        no_samples,
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

    if !raw.p_data.cast::<f32>().is_aligned() {
        return Err(Error::InvalidFrame(
            "Audio data pointer is not aligned for f32".into(),
        ));
    }

    let sample_rate = validate_audio_sample_rate(raw.sample_rate.into())?;
    let (no_channels, no_samples) =
        validate_positive_audio_counts(raw.no_channels.into(), raw.no_samples.into())?;

    let format = validate_audio_format(raw.FourCC)?;
    let channel_stride_in_bytes = unsafe { raw.__bindgen_anon_1.channel_stride_in_bytes };
    let channel_stride_bytes = usize::try_from(channel_stride_in_bytes)
        .ok()
        .filter(|&bytes| bytes > 0)
        .ok_or_else(|| {
            Error::InvalidFrame(format!(
                "Invalid channel_stride_in_bytes: {channel_stride_in_bytes}"
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
    let sample_count = checked_audio_extent(no_channels, no_samples, channel_stride_samples)?;

    Ok(ValidatedAudioLayout {
        format: Some(format),
        sample_rate,
        no_channels,
        no_samples,
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

    // Both were just checked nonnegative.
    Ok(ValidatedAudioLayout {
        format,
        sample_rate: raw.sample_rate.unsigned_abs(),
        no_channels: raw.no_channels.unsigned_abs() as usize,
        no_samples: 0,
        channel_stride_samples: 0,
        sample_count: 0,
    })
}

fn validate_audio_format(fourcc: NDIlib_FourCC_audio_type_e) -> Result<AudioFormat> {
    match fourcc {
        sys::bindings::NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP => {
            Ok(AudioFormat::Fltp)
        }
        _ => Err(Error::InvalidFrame(format!(
            "Unknown audio format FourCC: 0x{:08X}",
            fourcc
        ))),
    }
}

// ============================================================================
// Zero-copy borrowed receive frames
// ============================================================================

/// An allocation-free view of one video plane's initialized active bytes.
///
/// Plane order follows the pixel format: packed pixels; UYVY/A for UYVA;
/// Y/CbCr[/A] for P216/PA16; Y/V/U for YV12; Y/U/V for I420; Y/UV for NV12;
/// Y/U/V[/A] for the planar 4:4:4 formats. The view borrows its frame, owned
/// or captured, which keeps the allocation alive.
/// Immutable pixel views are `Send + Sync`, including views borrowed from
/// FrameSync. Scoped workers must finish before the frame is freed. This does
/// not transfer the SDK-owning frame reference or change its freeing thread.
///
/// ```no_run
/// # fn read(frame: &grafton_ndi::FrameSyncVideoRef<'_>) {
/// let plane = frame.plane(0).unwrap();
/// let rows = plane.rows();
/// std::thread::scope(|scope| {
///     let worker = scope.spawn(move || {
///         rows.flatten().map(|&byte| u64::from(byte)).sum::<u64>()
///     });
///     assert_eq!(worker.join().unwrap(),
///         plane.rows().flatten().map(|&byte| u64::from(byte)).sum::<u64>());
/// });
/// # }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct VideoPlane<'a> {
    data: *const u8,
    layout: VideoPlaneLayout,
    lifetime: PhantomData<&'a [u8]>,
}

// SAFETY: Every private construction site retains one allocation for 'a and
// validates all offsets, strides, and active lengths before constructing views.
// Active bytes are initialized and immutable for 'a; padding is never sliced.
// Reads require only u8 alignment (1) and expose the same &'a [u8] already
// returned by row/data. Copying or sharing a view grants no mutation or SDK
// access. Views have no destructor/free operation. Their borrow retains the
// captured guard until all workers finish, so FrameSync still frees its frame
// on the capture thread. Owned-slice construction likewise retains its borrow.
// SDK ownership/free boundary: Processing.NDI.Recv.h (capture_v3/free_video_v2)
// and Processing.NDI.FrameSync.h (capture_video/free_video).
unsafe impl Send for VideoPlane<'_> {}
// SAFETY: Same immutable access and lifetime proof as Send above.
unsafe impl Sync for VideoPlane<'_> {}

impl<'a> VideoPlane<'a> {
    /// Caller must retain an allocation covering the validated layout for 'a,
    /// with every active row readable, initialized, and immutable for that lifetime.
    unsafe fn new(data: *const u8, layout: VideoPlaneLayout) -> Self {
        Self {
            data,
            layout,
            lifetime: PhantomData,
        }
    }

    /// Distance between row starts in bytes, including padding.
    pub fn row_stride(&self) -> usize {
        self.layout.row_stride
    }

    /// Number of initialized bytes in each row.
    pub fn active_row_bytes(&self) -> usize {
        self.layout.active_row_bytes
    }

    /// Number of rows in this plane.
    pub fn row_count(&self) -> usize {
        self.layout.row_count
    }

    /// Get an active row, excluding padding, or `None` for an invalid index.
    pub fn row(&self, index: usize) -> Option<&'a [u8]> {
        if index >= self.layout.row_count {
            return None;
        }
        // SAFETY: Construction ties the allocation to 'a. Checked plane/row
        // arithmetic bounds this range within storage. The SDK initializes the
        // active bytes; padding is never included. The guard prevents freeing.
        Some(unsafe {
            slice::from_raw_parts(
                self.data
                    .add(self.layout.offset + index * self.layout.row_stride),
                self.layout.active_row_bytes,
            )
        })
    }

    /// Iterate active rows without allocating or copying pixels.
    pub fn rows(&self) -> VideoRows<'a> {
        VideoRows {
            plane: *self,
            indices: 0..self.row_count(),
        }
    }

    /// Borrow all active bytes when contiguous, excluding trailing padding.
    /// Returns `None` when padding separates rows.
    pub fn data(&self) -> Option<&'a [u8]> {
        if self.row_count() != 1 && self.row_stride() != self.active_row_bytes() {
            return None;
        }
        // SAFETY: All rows are adjacent initialized ranges in the retained
        // allocation; the final row's padding is excluded.
        Some(unsafe {
            slice::from_raw_parts(
                self.data.add(self.layout.offset),
                self.row_count() * self.active_row_bytes(),
            )
        })
    }
}

/// Active video rows, borrowing the frame that owns the allocation.
/// `Send + Sync` through its immutable plane; iteration never calls the SDK.
#[derive(Debug, Clone)]
pub struct VideoRows<'a> {
    plane: VideoPlane<'a>,
    indices: std::ops::Range<usize>,
}

impl<'a> Iterator for VideoRows<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<Self::Item> {
        self.plane.row(self.indices.next()?)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.indices.size_hint()
    }
}
impl DoubleEndedIterator for VideoRows<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.plane.row(self.indices.next_back()?)
    }
}
impl ExactSizeIterator for VideoRows<'_> {}
impl std::iter::FusedIterator for VideoRows<'_> {}

/// Video planes in SDK memory order, with no allocation or pixel copies.
/// `Send + Sync` for scoped reads; the owning frame's thread rules still apply.
#[derive(Debug, Clone)]
pub struct VideoPlanes<'a> {
    data: *const u8,
    layouts: slice::Iter<'a, VideoPlaneLayout>,
}
// SAFETY: Constructed only by ValidatedVideoLayout::view_planes for a validated
// live frame. The shared layout slice and data borrow last for 'a; each yielded plane
// satisfies VideoPlane's immutable, initialized, bounded u8 access proof above.
// Iterator state is exclusively mutated, clones are independent, and neither
// iteration nor dropping it accesses an SDK handle or frees the allocation.
unsafe impl Send for VideoPlanes<'_> {}
// SAFETY: Sharing the iterator exposes only immutable layouts and bytes.
unsafe impl Sync for VideoPlanes<'_> {}
impl<'a> Iterator for VideoPlanes<'a> {
    type Item = VideoPlane<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        let layout = *self.layouts.next()?;
        // SAFETY: Created only from a live frame with validated active rows.
        Some(unsafe { VideoPlane::new(self.data, layout) })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.layouts.size_hint()
    }
}
impl ExactSizeIterator for VideoPlanes<'_> {}
impl std::iter::FusedIterator for VideoPlanes<'_> {}

/// A zero-copy borrowed video frame, generic over its free strategy `S`.
///
/// This type wraps an RAII `Guard` that owns the NDI frame buffer lifetime,
/// exposing a safe, zero-copy view of the video data. The frame is automatically
/// freed when dropped, via whichever `NDIlib_*_free_video*` call the strategy `S`
/// encodes.
///
/// The two public spellings are type aliases over this one core, so every
/// accessor and the `Debug` impl are written exactly once:
/// - [`VideoFrameRef<'rx>`] — frames captured from a [`Receiver`](crate::Receiver)
///   (freed via `NDIlib_recv_free_video_v2`).
/// - [`FrameSyncVideoRef<'fs>`](crate::FrameSyncVideoRef) — frames captured from a
///   [`FrameSync`](crate::FrameSync) (freed via `NDIlib_framesync_free_video`).
///
/// **Key characteristics:**
/// - Zero allocations: References NDI SDK buffers directly
/// - Zero copies: No memcpy of pixel data
/// - RAII lifetime: Exactly one free per frame, enforced at compile time
/// - Receiver refs are `Send + Sync`; FrameSync refs stay on their capture thread
/// - Immutable plane/row views support scoped worker reads from either family
///
/// # Lifetime
///
/// The lifetime parameter `'a` ties this frame to the owner (a `Receiver` or a
/// `FrameSync`) that created it. The borrow checker ensures the owner cannot be
/// dropped while any frame references are alive, preventing use-after-free at
/// compile time with zero runtime cost. The underlying NDI buffer is freed when
/// the frame ref is dropped.
///
/// # Performance
///
/// For a 1920×1080 BGRA frame, this eliminates ~8.3 MB of memcpy compared to
/// the owned [`VideoFrame`]. At 60 fps, this saves ~475 MB/s of memory bandwidth.
#[allow(private_bounds)]
pub struct VideoRef<'a, S: FrameFree<RawFrame = NDIlib_video_frame_v2_t>> {
    guard: Guard<'a, S>,
    /// Cached validated layout information (pixel format, data length, stride/size).
    /// Computed once at construction time; `data()` uses this cached value.
    layout: ValidatedVideoLayout,
    header: VideoHeader,
    metadata: ValidatedFrameMetadata,
}

/// A zero-copy borrowed video frame from a [`Receiver`](crate::Receiver) capture.
///
/// This is the receiver spelling of the generic [`VideoRef`]; the
/// [`FrameSync`](crate::FrameSync) spelling is
/// [`FrameSyncVideoRef`](crate::FrameSyncVideoRef). Both share one accessor and
/// `Debug` implementation.
pub type VideoFrameRef<'rx> = VideoRef<'rx, VideoKind>;

#[allow(private_bounds)]
impl<'a, S: FrameFree<RawFrame = NDIlib_video_frame_v2_t>> VideoRef<'a, S> {
    /// Create a borrowed video frame from an RAII guard.
    ///
    /// Validates the frame including:
    /// - Valid pixel format (FourCC)
    /// - Non-null data pointer
    /// - Valid dimensions and stride
    /// - Valid frame rate, picture aspect ratio and scan type
    /// - Buffer size within `MAX_VIDEO_BYTES` limit
    ///
    /// The validated layout and header are cached so that accessors return
    /// them without re-computation or unchecked arithmetic.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI instance
    /// and contains a frame populated by the matching SDK capture call.
    pub(crate) unsafe fn new(guard: Guard<'a, S>) -> Result<Self> {
        let (layout, header) = validate_video_frame(guard.frame())?;
        let metadata = unsafe { validate_frame_metadata(guard.frame().p_metadata)? };

        Ok(Self {
            guard,
            layout,
            header,
            metadata,
        })
    }

    /// Get the frame width in pixels.
    pub fn width(&self) -> u32 {
        self.layout.width
    }

    /// Get the frame height in pixels.
    pub fn height(&self) -> u32 {
        self.layout.height
    }

    /// Get the pixel format.
    ///
    /// This is guaranteed to be a valid, supported format since it's validated during construction.
    pub fn pixel_format(&self) -> PixelFormat {
        self.layout.pixel_format
    }

    /// Get the frame rate.
    pub fn frame_rate(&self) -> FrameRate {
        self.header.frame_rate
    }

    /// Get the picture aspect ratio, width over height as displayed; `0.0`
    /// means square pixels.
    pub fn picture_aspect_ratio(&self) -> f32 {
        self.header.picture_aspect_ratio
    }

    /// Get the scan type (progressive, interlaced, etc.).
    pub fn scan_type(&self) -> ScanType {
        self.header.scan_type
    }

    /// Get the timecode.
    pub fn timecode(&self) -> Timecode {
        self.header.timecode
    }

    /// Get the time the sender submitted this frame, or `None` when the SDK
    /// does not know it.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        SdkTimestamp::from_sdk(self.guard.frame().timestamp)
    }

    /// Get the validated first-plane line stride in bytes.
    ///
    /// This returns the cached, validated value computed at construction time.
    pub fn line_stride_bytes(&self) -> usize {
        self.layout.line_stride_bytes
    }

    /// Get frame metadata as UTF-8 text, if present.
    pub fn metadata(&self) -> Option<&str> {
        unsafe { frame_metadata_str(self.guard.frame().p_metadata, self.metadata) }
    }

    /// Borrow all active pixels when contiguous, in SDK plane order.
    ///
    /// Returns `None` if padding separates active rows or planes. Use
    /// [`Self::planes`] / [`Self::plane`] for zero-copy padded access, or
    /// [`Self::try_to_owned`] for a tightly packed owned buffer. A single packed
    /// row can be borrowed even when its stride includes trailing padding.
    pub fn data(&self) -> Option<&[u8]> {
        if !self.layout.is_contiguous() {
            return None;
        }
        // SAFETY: The guard retains the SDK allocation for this borrow. Layout
        // validation bounds the range; contiguity proves every byte is active,
        // initialized payload, with no intervening or trailing padding.
        Some(unsafe {
            slice::from_raw_parts(self.guard.frame().p_data, self.layout.payload_len_bytes)
        })
    }

    /// Get one plane in SDK memory order, or `None` for an invalid index.
    pub fn plane(&self, index: usize) -> Option<VideoPlane<'_>> {
        // SAFETY: The returned lifetime borrows this guard, which retains the
        // allocation and its initialized, immutable active rows.
        unsafe { self.layout.view_plane(self.guard.frame().p_data, index) }
    }

    /// Iterate planes in SDK memory order without allocating or copying.
    pub fn planes(&self) -> VideoPlanes<'_> {
        // SAFETY: as for `plane`.
        unsafe { self.layout.view_planes(self.guard.frame().p_data) }
    }

    #[cfg(feature = "image-encoding")]
    fn image_source(&self) -> Result<ImagePixelSource<'_>> {
        ImagePixelSource::from_plane(self.layout, self.plane(0).expect("validated video plane"))
    }

    /// Encode PNG directly from the borrowed SDK pixels, without first
    /// copying the frame into an owned `VideoFrame`. Uses the same supported
    /// formats and padding handling as [`VideoFrame::encode_png`].
    #[cfg(feature = "image-encoding")]
    pub fn encode_png(&self) -> Result<Vec<u8>> {
        self.image_source()?.encode_png()
    }

    /// Encode JPEG directly from borrowed SDK pixels. Quality must be in
    /// `1..=100`; see [`VideoFrame::encode_jpeg`] for format restrictions.
    #[cfg(feature = "image-encoding")]
    pub fn encode_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        self.image_source()?.encode_jpeg(quality)
    }

    /// Encode borrowed SDK pixels as a PNG or JPEG base64 data URL without an
    /// intermediate owned frame. See [`VideoFrame::encode_data_url`].
    #[cfg(feature = "image-encoding")]
    pub fn encode_data_url(&self, format: ImageFormat) -> Result<String> {
        self.image_source()?.encode_data_url(format)
    }

    /// Copy this borrowed frame into an owned `VideoFrame`.
    ///
    /// Copies only active rows into one tightly packed pixel allocation and
    /// copies metadata separately. The owned stride can be smaller than the
    /// SDK stride. Preserves format, timing, aspect ratio, and scan type.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if the packed copy is not a valid
    /// frame.
    pub fn try_to_owned(&self) -> Result<VideoFrame> {
        // SAFETY: the guard owns the exact validated frame through copying.
        unsafe {
            VideoFrame::from_raw_validated(
                self.guard.frame(),
                self.layout,
                self.header,
                self.metadata,
            )
        }
    }
}

#[allow(private_bounds)]
impl<'a, S: FrameFree<RawFrame = NDIlib_video_frame_v2_t>> fmt::Debug for VideoRef<'a, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(S::REF_DEBUG_NAME)
            .field("width", &self.width())
            .field("height", &self.height())
            .field("pixel_format", &self.pixel_format())
            .field("frame_rate", &self.frame_rate())
            .field("picture_aspect_ratio", &self.picture_aspect_ratio())
            .field("scan_type", &self.scan_type())
            .field("timecode", &self.timecode())
            .field("active data (bytes)", &self.layout.payload_len_bytes)
            .field("line_stride_bytes", &self.line_stride_bytes())
            .field("metadata", &self.metadata())
            .field("timestamp", &self.timestamp())
            .finish()
    }
}

/// A zero-copy borrowed audio frame, generic over its free strategy `S`.
///
/// This type wraps an RAII `Guard` that owns the NDI frame buffer lifetime,
/// exposing a safe, zero-copy view of the audio data. The frame is automatically
/// freed when dropped, via whichever `NDIlib_*_free_audio*` call the strategy `S`
/// encodes.
///
/// The shared accessors and the `Debug` impl are written exactly once here; the
/// two public spellings are aliases that layer their differing format/owned/empty
/// policy on top:
/// - [`AudioFrameRef<'rx>`] — frames captured from a [`Receiver`](crate::Receiver)
///   (always concrete: `format() -> AudioFormat`, `try_to_owned() -> AudioFrame`).
/// - [`FrameSyncAudioRef<'fs>`](crate::FrameSyncAudioRef) — frames captured from a
///   [`FrameSync`](crate::FrameSync), which additionally model a validated empty
///   query/no-source state (`is_empty`, `Option`-returning `format`/`try_to_owned`).
///
/// **Key characteristics:**
/// - Zero allocations: References NDI SDK buffers directly
/// - Zero copies: No memcpy of audio samples
/// - RAII lifetime: Exactly one free per frame, enforced at compile time
/// - Receiver refs are `Send + Sync`; FrameSync refs stay on their capture thread
#[allow(private_bounds)]
pub struct AudioRef<'a, S: FrameFree<RawFrame = NDIlib_audio_frame_v3_t>> {
    guard: Guard<'a, S>,
    /// Cached validated layout information (format, sample count).
    /// Computed once at construction time; `data()` uses this cached value.
    layout: ValidatedAudioLayout,
    metadata: ValidatedFrameMetadata,
}

/// A zero-copy borrowed audio frame from a [`Receiver`](crate::Receiver) capture.
///
/// This is the receiver spelling of the generic [`AudioRef`]; the
/// [`FrameSync`](crate::FrameSync) spelling is
/// [`FrameSyncAudioRef`](crate::FrameSyncAudioRef). The receiver path always
/// yields a concrete audio frame, so [`format`](AudioRef::format) returns an
/// [`AudioFormat`] and [`try_to_owned`](AudioRef::try_to_owned) an [`AudioFrame`]; the
/// FrameSync path additionally models a validated empty query/no-source state.
pub type AudioFrameRef<'rx> = AudioRef<'rx, AudioKind>;

#[allow(private_bounds)]
impl<'a, S: FrameFree<RawFrame = NDIlib_audio_frame_v3_t>> AudioRef<'a, S> {
    /// Store a guard alongside an already-validated audio layout, validating and
    /// caching the frame metadata. The per-family constructors compute `layout`
    /// (concrete vs. empty-query) and funnel through here.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI instance
    /// and that `layout` was validated from the same frame the guard owns.
    pub(crate) unsafe fn from_validated_layout(
        guard: Guard<'a, S>,
        layout: ValidatedAudioLayout,
    ) -> Result<Self> {
        let metadata = unsafe { validate_frame_metadata(guard.frame().p_metadata)? };

        Ok(Self {
            guard,
            layout,
            metadata,
        })
    }

    /// Get the sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.layout.sample_rate
    }

    /// Get the number of audio channels.
    pub fn num_channels(&self) -> u32 {
        self.layout.num_channels()
    }

    /// Get the number of samples per channel.
    pub fn num_samples(&self) -> u32 {
        self.layout.num_samples()
    }

    /// Get the timecode.
    pub fn timecode(&self) -> Timecode {
        Timecode::from_ticks(self.guard.frame().timecode)
    }

    /// Get the time the sender submitted this frame, or `None` when the SDK
    /// does not know it.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        SdkTimestamp::from_sdk(self.guard.frame().timestamp)
    }

    /// Get the distance between the starts of consecutive channels in bytes,
    /// including any padding between them.
    pub fn channel_stride_bytes(&self) -> usize {
        self.layout.channel_stride_bytes()
    }

    /// Get frame metadata as UTF-8 text, if present.
    pub fn metadata(&self) -> Option<&str> {
        unsafe { frame_metadata_str(self.guard.frame().p_metadata, self.metadata) }
    }

    /// Get a zero-copy slice when channels are tightly packed.
    ///
    /// Returns `None` for multi-channel buffers with padding between channels.
    /// Use [`channel_data`](Self::channel_data) or [`channels`](Self::channels)
    /// for those buffers. An empty query/no-source frame returns `Some(&[])`.
    pub fn data(&self) -> Option<&[f32]> {
        if self.layout.is_empty() {
            return Some(&[]);
        }
        if self.layout.no_channels > 1
            && self.layout.channel_stride_samples != self.layout.no_samples
        {
            return None;
        }
        // SAFETY: a packed FLTP buffer contains only initialized samples;
        // construction checked dimensions, byte bounds, and float alignment.
        Some(unsafe {
            slice::from_raw_parts(
                self.guard.frame().p_data.cast::<f32>(),
                self.layout.sample_count,
            )
        })
    }

    /// Get the zero-copy samples for a single channel, excluding padding.
    pub fn channel_data(&self, channel: usize) -> Option<&[f32]> {
        // SAFETY: the guard retains the validated, aligned SDK buffer, whose
        // channel samples are initialized, for this borrow.
        unsafe {
            self.layout
                .channel(self.guard.frame().p_data.cast(), channel)
        }
    }

    /// Iterate over channel slices without reading inter-channel padding.
    /// Empty query/no-source frames yield no channels.
    pub fn channels(&self) -> impl ExactSizeIterator<Item = &[f32]> + '_ {
        // SAFETY: as for `channel_data`.
        unsafe { self.layout.channels(self.guard.frame().p_data.cast()) }
    }
}

impl<'rx> AudioRef<'rx, AudioKind> {
    /// Create a borrowed audio frame from a receiver RAII guard.
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
        unsafe { Self::from_validated_layout(guard, layout) }
    }

    /// Get the audio format (FourCC code).
    ///
    /// This is guaranteed to be a valid, supported format since it's validated during construction.
    pub fn format(&self) -> AudioFormat {
        self.layout
            .format()
            .expect("validate_audio_layout requires a concrete audio format")
    }

    /// Copy this borrowed frame into an owned `AudioFrame`.
    ///
    /// Copies channel samples into tightly packed planar storage and copies
    /// metadata, allowing the frame to outlive the SDK buffer. Padding is omitted.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if the packed copy is not a valid
    /// frame.
    pub fn try_to_owned(&self) -> Result<AudioFrame> {
        // SAFETY: the receive guard owns the exact validated frame through copying.
        unsafe { AudioFrame::from_raw_validated(*self.guard.frame(), self.layout, self.metadata) }
    }
}

impl<'fs> AudioRef<'fs, FrameSyncAudioFree> {
    /// Create a borrowed audio frame from a FrameSync RAII guard.
    ///
    /// When `query_input` is set, the SDK may report a validated empty
    /// query/no-source state (no samples); otherwise a concrete frame is
    /// required. A query that unexpectedly returns samples is rejected.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid FrameSync
    /// instance and contains a frame populated by `NDIlib_framesync_capture_audio_v2`.
    pub(crate) unsafe fn new(
        guard: Guard<'fs, FrameSyncAudioFree>,
        query_input: bool,
    ) -> Result<Self> {
        let layout = if query_input {
            if guard.frame().no_samples != 0 {
                return Err(Error::InvalidFrame(format!(
                    "FrameSync audio query returned {} samples",
                    guard.frame().no_samples
                )));
            }
            validate_audio_layout_allow_empty(guard.frame())?
        } else {
            validate_audio_layout(guard.frame())?
        };
        unsafe { Self::from_validated_layout(guard, layout) }
    }

    /// Returns true for a valid query/no-source state with no sample buffer.
    pub fn is_empty(&self) -> bool {
        self.layout.is_empty()
    }

    /// Get the audio format (FourCC code), or `None` for an empty query/no-source state.
    pub fn format(&self) -> Option<AudioFormat> {
        self.layout.format()
    }

    /// Copy this borrowed frame into an owned `AudioFrame`.
    ///
    /// Copies channel samples into tightly packed planar storage and copies
    /// metadata, allowing the frame to outlive the SDK buffer. Padding is omitted.
    ///
    /// Returns `Ok(None)` for a valid query/no-source state with no sample
    /// buffer to own.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if the packed copy is not a valid
    /// frame.
    pub fn try_to_owned(&self) -> Result<Option<AudioFrame>> {
        if self.layout.is_empty() {
            Ok(None)
        } else {
            // SAFETY: the FrameSync guard owns the exact validated frame through copying.
            unsafe {
                AudioFrame::from_raw_validated(*self.guard.frame(), self.layout, self.metadata)
            }
            .map(Some)
        }
    }
}

#[allow(private_bounds)]
impl<'a, S: FrameFree<RawFrame = NDIlib_audio_frame_v3_t>> fmt::Debug for AudioRef<'a, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(S::REF_DEBUG_NAME)
            .field("sample_rate", &self.sample_rate())
            .field("num_channels", &self.num_channels())
            .field("num_samples", &self.num_samples())
            .field("timecode", &self.timecode())
            // Render the format from the cached layout (`Option<AudioFormat>`) so
            // this single impl serves both the receiver and FrameSync spellings,
            // whose public `format()` return types differ.
            .field("format", &self.layout.format())
            .field("samples per channel", &self.layout.no_samples)
            .field("channel_stride_bytes", &self.channel_stride_bytes())
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
/// - `Send + Sync`: Captured metadata can be processed on a scoped worker thread
///
/// # Examples
///
/// ```no_run
/// # use grafton_ndi::{Ndi, ReceiverOptions, Receiver, Source, SourceAddress};
/// # use std::time::Duration;
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = Ndi::new()?;
/// # let source = Source::new("Test");
/// # let options = ReceiverOptions::new(source);
/// # let receiver = Receiver::new(&ndi, &options)?;
/// // Zero-copy capture
/// if let Some(frame) = receiver.metadata().capture_ref(Duration::from_millis(1000))? {
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
        // SAFETY: the receive guard owns the SDK metadata buffer through validation.
        let layout = unsafe { validate_metadata_layout(guard.frame())? };

        Ok(Self { guard, layout })
    }

    /// Get the timecode.
    pub fn timecode(&self) -> Timecode {
        Timecode::from_ticks(self.guard.frame().timecode)
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
        // SAFETY: the receive guard owns the exact validated buffer for this borrow.
        unsafe { metadata_payload_bytes(self.guard.frame(), self.layout) }
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

    /// The layout part of [`validate_video_frame`].
    fn validate_video_layout(raw: &NDIlib_video_frame_v2_t) -> Result<ValidatedVideoLayout> {
        validate_video_frame(raw).map(|(layout, _)| layout)
    }

    #[test]
    fn planar_444_layout_matches_independent_plane_math_and_bounds() {
        for (format, fourcc, bytes, count) in [
            (PixelFormat::Na48, b"NA48", 1usize, 3usize),
            (PixelFormat::Nb48, b"NB48", 1, 4),
            (PixelFormat::Na4g, b"NA4G", 2, 3),
            (PixelFormat::Nb4g, b"NB4G", 2, 4),
        ] {
            assert_eq!(FourCc::from(format).raw().to_le_bytes(), *fourcc);
            for width in [2u32, 4, 1920, 3840, 16_384] {
                for height in [1u32, 3, 1080, 2160, 16_384] {
                    for padding in [0, 2, 64] {
                        let active = width as usize * bytes;
                        let stride = active + padding;
                        let expected = stride as u128 * height as u128 * count as u128;
                        let layout = ValidatedVideoLayout::new_uncompressed(
                            format,
                            width,
                            height,
                            Some(stride),
                        );
                        assert_eq!(layout.is_ok(), expected <= MAX_VIDEO_BYTES as u128);
                        if let Ok(layout) = layout {
                            assert_eq!(layout.plane_count, count);
                            assert_eq!(layout.data_len_bytes as u128, expected);
                            assert_eq!(layout.payload_len_bytes, active * height as usize * count);
                            for (i, plane) in layout.planes().iter().enumerate() {
                                assert_eq!(plane.offset, i * stride * height as usize);
                                assert_eq!(plane.row_stride, stride);
                                assert_eq!(plane.active_row_bytes, active);
                                assert_eq!(plane.row_count, height as usize);
                            }
                        }
                    }
                }
            }
            assert!(ValidatedVideoLayout::new_uncompressed(format, 3, 3, None).is_err());
            assert!(ValidatedVideoLayout::new_uncompressed(format, 4, 3, Some(1)).is_err());
            assert!(calculate_buffer_len_checked(format, usize::MAX, 2).is_err());
            // Building needs no runtime; sending checks the runtime.
            let frame = VideoFrame::builder()
                .resolution(4, 3)
                .pixel_format(format)
                .build()
                .unwrap();
            assert_eq!(frame.planes().len(), count);
            assert!(frame.planes().all(|plane| plane.row_count() == 3));
        }
    }

    #[test]
    fn planar_444_rows_copy_without_padding_through_every_owned_path() {
        use crate::capture::FrameSyncVideoFree;
        use std::mem::MaybeUninit;
        for (format, bytes, count) in [
            (PixelFormat::Na48, 1usize, 3usize),
            (PixelFormat::Nb48, 1, 4),
            (PixelFormat::Na4g, 2, 3),
            (PixelFormat::Nb4g, 2, 4),
        ] {
            let active = 4 * bytes;
            let stride = active + 4;
            let mut storage = vec![MaybeUninit::<u8>::uninit(); stride * 3 * count];
            let mut expected = Vec::new();
            for plane in 0..count {
                for row in 0..3 {
                    for column in 0..active {
                        let value = (plane * 40 + row * 10 + column) as u8;
                        storage[plane * stride * 3 + row * stride + column].write(value);
                        expected.push(value);
                    }
                }
            }
            let raw = NDIlib_video_frame_v2_t {
                xres: 4,
                yres: 3,
                FourCC: FourCc::from(format).raw(),
                frame_rate_N: 60,
                frame_rate_D: 1,
                frame_format_type: ScanType::Progressive.to_sdk(),
                p_data: storage.as_mut_ptr().cast(),
                __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                    line_stride_in_bytes: stride as i32,
                },
                ..Default::default()
            };
            // Only initialized active bytes may be read; padding stays uninitialized.
            let frame = unsafe {
                VideoRef::new(Guard::<FrameSyncVideoFree>::with_test_free(
                    ptr::null_mut(),
                    raw,
                    |_, _| {},
                ))
            }
            .unwrap();
            assert_eq!(frame.planes().len(), count);
            assert_eq!(
                frame
                    .planes()
                    .flat_map(|p| p.rows())
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(frame.plane(count).is_none());
            let captured = frame.try_to_owned().unwrap();
            let copied = unsafe { VideoFrame::from_raw(&raw) }.unwrap();
            let built = VideoFrame::builder()
                .resolution(4, 3)
                .pixel_format(format)
                .data(expected.clone())
                .build()
                .unwrap();
            for owned in [captured, copied, built] {
                assert_eq!(owned.data(), expected);
                // The owned frame's planes are the same views, without padding.
                assert_eq!(
                    owned
                        .planes()
                        .flat_map(|p| p.rows())
                        .flatten()
                        .copied()
                        .collect::<Vec<_>>(),
                    expected
                );
                let recopy = crate::BorrowedVideoFrame::from(&owned)
                    .try_to_owned()
                    .unwrap();
                assert_eq!(recopy.data(), expected);
            }
        }
    }

    #[test]
    fn planar_444_formats_are_sendable_only_where_the_runtime_supports_them() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let supported = ndi
            .require_feature(crate::capabilities::RuntimeFeature::Planar444Video)
            .is_ok();
        for format in PixelFormat::ALL {
            let planar_444 = matches!(
                format,
                PixelFormat::Na48 | PixelFormat::Nb48 | PixelFormat::Na4g | PixelFormat::Nb4g
            );
            let result = format.require_sendable(&ndi);
            if planar_444 && !supported {
                assert!(
                    matches!(
                        result,
                        Err(Error::UnsupportedCapability {
                            feature: crate::capabilities::RuntimeFeature::Planar444Video,
                            ..
                        })
                    ),
                    "{format:?}: {result:?}"
                );
            } else {
                assert!(result.is_ok(), "{format:?}: {result:?}");
            }
        }
    }

    /// Interleaved input is transposed into exact per-channel planes.
    #[test]
    fn interleaved_audio_builds_exact_planar_output() {
        // Sample counts that are not multiples of the channel count or each other.
        for (channels, samples) in [
            (1, 1),
            (1, 7),
            (2, 1),
            (2, 5),
            (2, 13),
            (3, 7),
            (3, 1),
            (5, 11),
        ] {
            // Value encodes (channel, sample) so any misplacement is visible.
            let value = |ch: usize, s: usize| (ch * 1000 + s) as f32;
            let mut interleaved = Vec::with_capacity(channels * samples);
            for s in 0..samples {
                for ch in 0..channels {
                    interleaved.push(value(ch, s));
                }
            }
            let frame = AudioFrame::builder()
                .sample_rate(48_000)
                .num_channels(channels as u32)
                .num_samples(samples as u32)
                .layout(AudioLayout::Interleaved)
                .data(interleaved)
                .build()
                .unwrap();
            let expected: Vec<f32> = (0..channels)
                .flat_map(|ch| (0..samples).map(move |s| value(ch, s)))
                .collect();
            assert_eq!(frame.data(), expected, "{channels}ch x {samples}");
            // The channel iterator and per-channel access agree with the data.
            assert_eq!(frame.channels().len(), channels);
            for (ch, samples) in frame.channels().enumerate() {
                assert_eq!(Some(samples), frame.channel_data(ch));
                assert_eq!(samples, &expected[ch * samples.len()..][..samples.len()]);
            }
        }
        // A wrong input length is still rejected before any transposition.
        assert!(AudioFrame::builder()
            .num_channels(3)
            .num_samples(4)
            .layout(AudioLayout::Interleaved)
            .data(vec![0.0; 11])
            .build()
            .is_err());
    }

    /// Every audio path reports a bad sample rate with the same message.
    #[test]
    fn audio_sample_rate_check_is_shared_with_one_message() {
        let message = |result: Result<()>| match result {
            Err(Error::InvalidFrame(msg)) => msg,
            other => panic!("expected InvalidFrame, got {other:?}"),
        };
        for rate in [0u32, 1 << 31, u32::MAX] {
            let expected = format!("Invalid sample rate: {rate}");
            assert_eq!(
                message(validate_audio_sample_rate(rate.into()).map(|_| ())),
                expected
            );
            assert_eq!(
                message(validate_outbound_audio_layout(rate, 2, 8, AudioFormat::Fltp).map(|_| ())),
                expected
            );
            assert_eq!(
                message(
                    crate::framesync::FrameSyncAudioRequest::capture_at(rate, 2, 8).map(|_| ())
                ),
                expected
            );
            assert_eq!(
                message(
                    crate::audio_conversion::InterleavedAudioF32::new(rate, 2, &[0.0; 2])
                        .map(|_| ())
                ),
                expected
            );
        }
        // Rates the SDK reports go through the same check.
        for rate in [0, -48_000, i32::MIN] {
            let mut sample = 0.0f32;
            let raw = NDIlib_audio_frame_v3_t {
                sample_rate: rate,
                no_channels: 1,
                no_samples: 1,
                FourCC: AudioFormat::Fltp.to_sdk(),
                p_data: (&mut sample as *mut f32).cast(),
                __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                    channel_stride_in_bytes: 4,
                },
                ..Default::default()
            };
            assert_eq!(
                message(validate_audio_layout(&raw).map(|_| ())),
                format!("Invalid sample rate: {rate}")
            );
        }
        assert_eq!(validate_audio_sample_rate(1).unwrap(), 1);
        assert_eq!(
            validate_audio_sample_rate(i32::MAX.into()).unwrap(),
            i32::MAX.unsigned_abs()
        );
    }

    /// The extent helper gives packed and strided audio the same sample count
    /// and size cap.
    #[test]
    fn audio_extent_is_shared_by_packed_and_strided_layouts() {
        let packed = validate_packed_audio_dimensions(3, 5).unwrap();
        assert_eq!(packed.sample_count(), 15);
        assert_eq!(checked_audio_extent(3, 5, 5).unwrap(), 15);
        // Padding between channels counts, trailing padding does not.
        assert_eq!(checked_audio_extent(3, 5, 8).unwrap(), 2 * 8 + 5);
        let limit = MAX_AUDIO_BYTES / 4;
        assert_eq!(checked_audio_extent(1, limit, limit).unwrap(), limit);
        assert!(checked_audio_extent(1, limit + 1, limit + 1).is_err());
        assert!(checked_audio_extent(2, limit, limit).is_err());
        assert!(checked_audio_extent(2, 1, limit).is_err());
        assert!(checked_audio_extent(2, 1, usize::MAX).is_err());
    }

    #[test]
    fn packed_audio_dimensions_match_wide_oracle_and_owned_validation() {
        fn check(channels: u32, samples: u32) {
            let bytes = i128::from(channels) * i128::from(samples) * 4;
            let stride = i128::from(samples) * 4;
            let valid = channels > 0
                && samples > 0
                && stride <= i128::from(i32::MAX)
                && bytes <= MAX_AUDIO_BYTES as i128;
            let packed = validate_packed_audio_dimensions(channels, samples);
            let owned =
                validate_outbound_audio_layout(48_000, channels, samples, AudioFormat::Fltp);
            let request = crate::FrameSyncAudioRequest::capture(channels, samples);
            assert_eq!(
                packed.is_ok(),
                valid,
                "channels={channels}, samples={samples}"
            );
            assert_eq!(owned.is_ok(), valid);
            assert_eq!(request.is_ok(), valid);
            if let (Ok(packed), Ok(owned)) = (packed, owned) {
                assert_eq!(packed.sample_count as i128 * 4, bytes);
                assert_eq!(owned.channel_stride_bytes() as i128, stride);
                assert_eq!(packed.no_channels, owned.no_channels);
                assert_eq!(packed.no_samples, owned.no_samples);
                assert_eq!(packed.sample_count, owned.sample_count);
            }
        }
        let values = [
            0,
            1,
            2,
            3,
            16,
            1024,
            8_388_608,
            8_388_609,
            16_777_216,
            16_777_217,
            i32::MAX.unsigned_abs() / 4,
            i32::MAX.unsigned_abs() / 4 + 1,
            1_073_741_825,
            i32::MAX.unsigned_abs(),
            1 << 31,
            u32::MAX,
        ];
        for channels in values {
            for samples in values {
                check(channels, samples);
            }
        }
        // Deterministic property coverage around the payload boundary, plus the
        // full unsigned input range. No sample buffers are allocated.
        let mut seed = 102u64;
        for _ in 0..4096 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let channels = (seed >> 32) as u32;
            check(channels, seed as u32);
            let channels = ((seed >> 32) % 4096 + 1) as u32;
            let boundary = (MAX_AUDIO_BYTES / 4) as u32 / channels;
            for samples in [boundary - 1, boundary, boundary + 1] {
                check(channels, samples);
            }
        }
        // The signed stride boundary is rejected independently of the cap.
        let at_stride_limit = validate_packed_audio_dimensions(1, i32::MAX.unsigned_abs() / 4);
        assert!(at_stride_limit
            .err()
            .unwrap()
            .to_string()
            .contains("maximum size"));
        let over_stride_limit =
            validate_packed_audio_dimensions(1, i32::MAX.unsigned_abs() / 4 + 1);
        assert!(over_stride_limit
            .err()
            .unwrap()
            .to_string()
            .contains("stride"));
    }

    /// Compare the old full-span copy with normalized active-row copies using
    /// initialized storage. Run with `cargo test --release --lib
    /// video_padding_throughput -- --ignored --nocapture`.
    #[test]
    #[ignore = "Manual optimized video throughput comparison"]
    fn video_padding_throughput() {
        use crate::capture::{FrameSyncVideoFree, Guard};
        use std::{hint::black_box, time::Instant};
        for (width, height) in [(1920, 1080), (3840, 2160)] {
            for padding in [0, 64] {
                let stride = width * 4 + padding;
                let mut storage = vec![17u8; (stride * height) as usize];
                let raw = NDIlib_video_frame_v2_t {
                    xres: width,
                    yres: height,
                    FourCC: FourCc::from(PixelFormat::Bgra).raw(),
                    frame_rate_N: 60,
                    frame_rate_D: 1,
                    frame_format_type: ScanType::Progressive.to_sdk(),
                    p_data: storage.as_mut_ptr(),
                    __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                        line_stride_in_bytes: stride,
                    },
                    ..Default::default()
                };
                let frame = unsafe {
                    VideoRef::new(Guard::<FrameSyncVideoFree>::with_test_free(
                        ptr::null_mut(),
                        raw,
                        |_, _| {},
                    ))
                }
                .unwrap();
                let iterations = 100;
                // Warm allocation and page caches for both implementations.
                black_box(storage.clone());
                black_box(frame.try_to_owned().unwrap());
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(black_box(&storage).clone());
                }
                let baseline = start.elapsed();
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(black_box(&frame).try_to_owned().unwrap());
                }
                let packed = start.elapsed();
                let start = Instant::now();
                for _ in 0..iterations * 100 {
                    for plane in black_box(&frame).planes() {
                        for row in plane.rows() {
                            black_box(row);
                        }
                    }
                }
                let traversal = start.elapsed();
                eprintln!("{width}x{height} padding={padding}: old storage copy {:.3} ms/frame, active copy {:.3} ms/frame, row traversal {:.3} us/frame, copied {} active bytes",
                    baseline.as_secs_f64() * 1000.0 / f64::from(iterations),
                    packed.as_secs_f64() * 1000.0 / f64::from(iterations),
                    traversal.as_secs_f64() * 1_000_000.0 / f64::from(iterations * 100),
                    frame.layout.payload_len_bytes);
            }
        }
    }

    #[test]
    fn video_padding_all_formats_expose_and_copy_only_initialized_rows() {
        use crate::capture::{FrameSyncVideoFree, Guard};
        use std::mem::MaybeUninit;
        // Independently specified active plane dimensions for 4x4 images.
        let cases: &[(PixelFormat, &[(usize, usize)])] = &[
            (PixelFormat::Bgra, &[(16, 4)]),
            (PixelFormat::Bgrx, &[(16, 4)]),
            (PixelFormat::Rgba, &[(16, 4)]),
            (PixelFormat::Rgbx, &[(16, 4)]),
            (PixelFormat::Uyvy, &[(8, 4)]),
            (PixelFormat::Uyva, &[(8, 4), (4, 4)]),
            (PixelFormat::P216, &[(8, 4), (8, 4)]),
            (PixelFormat::Pa16, &[(8, 4), (8, 4), (8, 4)]),
            (PixelFormat::Yv12, &[(4, 4), (2, 2), (2, 2)]),
            (PixelFormat::I420, &[(4, 4), (2, 2), (2, 2)]),
            (PixelFormat::Nv12, &[(4, 4), (4, 2)]),
        ];
        for &(format, dimensions) in cases {
            for padding in [0, 4, 8] {
                let stride = dimensions[0].0 + padding;
                let mut storage = vec![MaybeUninit::<u8>::uninit(); 512];
                let mut expected = Vec::new();
                let mut offsets = Vec::new();
                let mut offset = 0;
                for (i, &(active, rows)) in dimensions.iter().enumerate() {
                    let plane_stride = stride * active / dimensions[0].0;
                    offsets.push(offset);
                    for row in 0..rows {
                        for byte in 0..active {
                            let value = (i * 71 + row * 17 + byte) as u8;
                            storage[offset + row * plane_stride + byte].write(value);
                            expected.push(value);
                        }
                    }
                    offset += plane_stride * rows;
                }
                let metadata = CString::new("<video-padding/>").unwrap();
                let raw = NDIlib_video_frame_v2_t {
                    xres: 4,
                    yres: 4,
                    FourCC: FourCc::from(format).raw(),
                    frame_rate_N: 30000,
                    frame_rate_D: 1001,
                    picture_aspect_ratio: 1.25,
                    frame_format_type: ScanType::Interlaced.to_sdk(),
                    timecode: 12345,
                    timestamp: 67890,
                    p_data: storage.as_mut_ptr().cast(),
                    p_metadata: metadata.as_ptr(),
                    __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                        line_stride_in_bytes: stride as i32,
                    },
                };
                // Null FrameSync guards skip SDK calls; storage and metadata
                // live until both independent guards and their views are gone.
                let make = || unsafe {
                    VideoRef::new(Guard::<FrameSyncVideoFree>::with_test_free(
                        ptr::null_mut(),
                        raw,
                        |_, _| {},
                    ))
                    .unwrap()
                };
                let (frame, allocations) = crate::test_alloc::allocation_count(make);
                assert_eq!(allocations, 0);
                let second = make();
                let ((), allocations) = crate::test_alloc::allocation_count(|| {
                    assert_eq!(frame.planes().len(), dimensions.len());
                    assert!(frame.plane(dimensions.len()).is_none());
                    assert_eq!(frame.data().is_some(), padding == 0);
                    if let Some(data) = frame.data() {
                        assert_eq!(data, expected);
                    }
                    let mut position = 0;
                    for (i, plane) in frame.planes().enumerate() {
                        let (active, rows) = dimensions[i];
                        assert_eq!(plane.row_count(), rows);
                        assert_eq!(plane.active_row_bytes(), active);
                        assert_eq!(plane.row_stride(), stride * active / dimensions[0].0);
                        assert_eq!(plane.data().is_some(), padding == 0);
                        assert!(plane.row(rows).is_none());
                        let mut iterator = plane.rows();
                        assert_eq!(iterator.len(), rows);
                        assert_eq!(iterator.next_back(), plane.row(rows - 1));
                        assert_eq!(iterator.len(), rows - 1);
                        for (row_index, row) in plane.rows().enumerate() {
                            assert_eq!(row.as_ptr(), unsafe {
                                storage
                                    .as_ptr()
                                    .cast::<u8>()
                                    .add(offsets[i] + row_index * plane.row_stride())
                            });
                            assert_eq!(row, &expected[position..position + active]);
                            position += active;
                        }
                    }
                    assert_eq!(position, expected.len());
                });
                assert_eq!(allocations, 0);
                // Preconstructed views and independent iterator clones can be
                // moved/shared while this thread retains the FrameSync guard.
                let planes = frame.planes();
                let moved_planes = planes.clone();
                let expected_sum: u64 = expected.iter().map(|&b| u64::from(b)).sum();
                std::thread::scope(|scope| {
                    let shared = &planes;
                    let reader = scope.spawn(move || {
                        shared
                            .clone()
                            .flat_map(|p| p.rows())
                            .flatten()
                            .map(|&b| u64::from(b))
                            .sum::<u64>()
                    });
                    let moved = scope.spawn(move || {
                        moved_planes
                            .flat_map(|p| p.rows().rev())
                            .flatten()
                            .map(|&b| u64::from(b))
                            .sum::<u64>()
                    });
                    assert_eq!(reader.join().unwrap(), expected_sum);
                    assert_eq!(moved.join().unwrap(), expected_sum);
                });
                for plane in frame.planes() {
                    let rows = plane.rows();
                    let clone = rows.clone();
                    let serial = rows.clone().flatten().copied().collect::<Vec<_>>();
                    std::thread::scope(|scope| {
                        let shared = &plane;
                        let reader = scope
                            .spawn(move || shared.rows().flatten().copied().collect::<Vec<_>>());
                        let moved =
                            scope.spawn(move || rows.flatten().copied().collect::<Vec<_>>());
                        let reversed = scope.spawn(move || {
                            clone.rev().rev().flatten().copied().collect::<Vec<_>>()
                        });
                        assert_eq!(reader.join().unwrap(), serial);
                        assert_eq!(moved.join().unwrap(), serial);
                        assert_eq!(reversed.join().unwrap(), serial);
                    });
                }
                assert_eq!(frame.layout.data_len_bytes, offset);
                assert_eq!(frame.layout.payload_len_bytes, expected.len());
                let owned = frame.try_to_owned().unwrap();
                let raw_owned = unsafe { VideoFrame::from_raw(&raw) }.unwrap();
                assert_eq!(owned.data(), expected);
                assert_eq!(raw_owned.data(), expected);
                assert_eq!(owned.pixel_format(), format);
                assert_eq!(owned.line_stride_bytes(), dimensions[0].0);
                assert_eq!(owned.metadata(), Some("<video-padding/>"));
                assert_eq!(owned.frame_rate(), FrameRate::new(30000, 1001).unwrap());
                assert_eq!(owned.picture_aspect_ratio(), 1.25);
                assert_eq!(owned.scan_type(), ScanType::Interlaced);
                assert_eq!(
                    (owned.timecode(), owned.timestamp()),
                    (
                        Timecode::from_ticks(12345),
                        Some(SdkTimestamp::from_ticks(67890))
                    )
                );
                drop(frame);
                assert_eq!(
                    second.plane(0).unwrap().row(0).unwrap(),
                    &expected[..dimensions[0].0]
                );
                // No metadata: exactly one allocation for active owned pixels.
                let raw = NDIlib_video_frame_v2_t {
                    p_metadata: ptr::null(),
                    ..raw
                };
                let (copied, allocations) = crate::test_alloc::allocation_count(|| {
                    unsafe { VideoFrame::from_raw(&raw) }.unwrap()
                });
                assert_eq!(allocations, 1);
                assert_eq!(copied.data(), expected);
                assert!(VideoFrame::builder()
                    .resolution(4, 4)
                    .pixel_format(format)
                    .data(vec![0; expected.len() - 1])
                    .build()
                    .is_err());
                assert!(crate::BorrowedVideoFrame::builder(
                    &vec![0; expected.len() - 1],
                    4,
                    4,
                    format,
                )
                .build()
                .is_err());
                // Copied-send conversion must use the same normalization.
                // This send-side fixture initializes padding too, because its
                // constructor takes an already initialized application slice.
                let mut initialized = vec![99; offset];
                let mut position = 0;
                for (i, &(active, rows)) in dimensions.iter().enumerate() {
                    let plane_stride = stride * active / dimensions[0].0;
                    for row in 0..rows {
                        let start = offsets[i] + row * plane_stride;
                        initialized[start..start + active]
                            .copy_from_slice(&expected[position..position + active]);
                        position += active;
                    }
                }
                let send = crate::BorrowedVideoFrame::builder(&initialized, 4, 4, format)
                    .frame_rate(FrameRate::new(30000, 1001).unwrap())
                    .picture_aspect_ratio(1.25)
                    .scan_type(ScanType::Interlaced)
                    .timecode(Timecode::from_ticks(12345))
                    .line_stride(stride)
                    .metadata(&metadata)
                    .build()
                    .unwrap();
                let normalized = send.try_to_owned().unwrap();
                assert_eq!(normalized.data(), expected);
                assert_eq!(normalized.timecode(), Timecode::from_ticks(12345));
                // A borrowed frame carries no timestamp.
                assert_eq!(normalized.timestamp(), None);
                assert_eq!(normalized.line_stride_bytes(), copied.line_stride_bytes());
                assert_eq!(normalized.metadata(), Some("<video-padding/>"));
            }
        }
    }

    #[test]
    fn video_padding_single_rows_and_zero_stride() {
        use crate::capture::{FrameSyncVideoFree, Guard};
        use std::mem::MaybeUninit;
        for format in [
            PixelFormat::Bgra,
            PixelFormat::Bgrx,
            PixelFormat::Rgba,
            PixelFormat::Rgbx,
            PixelFormat::Uyvy,
            PixelFormat::Uyva,
            PixelFormat::P216,
            PixelFormat::Pa16,
        ] {
            let tight = ValidatedVideoLayout::new_uncompressed(format, 2, 1, Some(0)).unwrap();
            assert!(tight.is_contiguous());
            let active = format.line_stride(2).unwrap();
            let padded =
                ValidatedVideoLayout::new_uncompressed(format, 2, 1, Some(active + 4)).unwrap();
            let mut storage = [MaybeUninit::<u8>::uninit(); 64];
            for plane in padded.planes() {
                for byte in &mut storage[plane.offset..plane.offset + plane.active_row_bytes] {
                    byte.write(42);
                }
            }
            let raw = NDIlib_video_frame_v2_t {
                xres: 2,
                yres: 1,
                FourCC: FourCc::from(format).raw(),
                frame_rate_N: 30,
                frame_rate_D: 1,
                p_data: storage.as_mut_ptr().cast(),
                frame_format_type: ScanType::Progressive.to_sdk(),
                __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                    line_stride_in_bytes: (active + 4) as i32,
                },
                ..Default::default()
            };
            let frame = unsafe {
                VideoRef::new(Guard::<FrameSyncVideoFree>::with_test_free(
                    ptr::null_mut(),
                    raw,
                    |_, _| {},
                ))
            }
            .unwrap();
            assert_eq!(frame.data().is_some(), padded.plane_count == 1);
            for plane in frame.planes() {
                assert_eq!(plane.data().unwrap(), vec![42; plane.active_row_bytes()]);
            }
            assert_eq!(
                frame.try_to_owned().unwrap().data(),
                vec![42; tight.payload_len_bytes]
            );
        }
        for format in [PixelFormat::Yv12, PixelFormat::I420, PixelFormat::Nv12] {
            let tight = ValidatedVideoLayout::new_uncompressed(format, 4, 2, Some(0)).unwrap();
            assert!(tight.is_contiguous());
            let padded = ValidatedVideoLayout::new_uncompressed(format, 4, 2, Some(8)).unwrap();
            assert!(!padded.is_contiguous()); // Chroma is one row, but plane gaps remain.
        }
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn video_padding_encoders_never_read_uninitialized_gaps() {
        use crate::capture::{FrameSyncVideoFree, Guard};
        use std::mem::MaybeUninit;
        for format in [
            PixelFormat::Bgra,
            PixelFormat::Bgrx,
            PixelFormat::Rgba,
            PixelFormat::Rgbx,
        ] {
            let mut storage = [MaybeUninit::<u8>::uninit(); 24];
            let pixels = [1, 2, 3, 255, 5, 6, 7, 255, 9, 10, 11, 255, 13, 14, 15, 255];
            for row in 0..2 {
                for byte in 0..8 {
                    storage[row * 12 + byte].write(pixels[row * 8 + byte]);
                }
            }
            let raw = NDIlib_video_frame_v2_t {
                xres: 2,
                yres: 2,
                FourCC: FourCc::from(format).raw(),
                frame_rate_N: 30,
                frame_rate_D: 1,
                frame_format_type: ScanType::Progressive.to_sdk(),
                p_data: storage.as_mut_ptr().cast(),
                __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                    line_stride_in_bytes: 12,
                },
                ..Default::default()
            };
            let borrowed = unsafe {
                VideoRef::new(Guard::<FrameSyncVideoFree>::with_test_free(
                    ptr::null_mut(),
                    raw,
                    |_, _| {},
                ))
            }
            .unwrap();
            let packed = image_test_frame(format, 2, 2, None, pixels.to_vec());
            let png = borrowed.encode_png().unwrap();
            assert_eq!(
                decode_png_rgba(&png),
                decode_png_rgba(&packed.encode_png().unwrap())
            );
            assert_eq!(
                borrowed.encode_jpeg(85).unwrap(),
                packed.encode_jpeg(85).unwrap()
            );
            assert_eq!(
                borrowed.encode_data_url(ImageFormat::Png).unwrap(),
                packed.encode_data_url(ImageFormat::Png).unwrap()
            );
            assert_eq!(borrowed.try_to_owned().unwrap().data(), pixels);
        }
    }

    #[test]
    fn video_padding_reported_642_pixel_reproduction() {
        use crate::capture::{FrameSyncVideoFree, Guard};
        use std::mem::MaybeUninit;
        let mut storage = vec![MaybeUninit::<u8>::uninit(); 2592 * 32];
        for row in 0..32 {
            for byte in &mut storage[row * 2592..row * 2592 + 2568] {
                byte.write(17);
            }
        }
        let raw = NDIlib_video_frame_v2_t {
            xres: 642,
            yres: 32,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 30,
            frame_rate_D: 1,
            frame_format_type: ScanType::Progressive.to_sdk(),
            p_data: storage.as_mut_ptr().cast(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 2592,
            },
            ..Default::default()
        };
        let frame = unsafe {
            VideoRef::new(Guard::<FrameSyncVideoFree>::with_test_free(
                ptr::null_mut(),
                raw,
                |_, _| {},
            ))
        }
        .unwrap();
        assert!(frame.data().is_none());
        let owned = frame.try_to_owned().unwrap();
        assert_eq!(owned.data().len(), 82_176);
        assert_eq!(owned.line_stride_bytes(), 2568);
        assert!(owned.data().iter().all(|&byte| byte == 17));
    }

    #[test]
    fn video_builder_defaults_and_reuse_updates_preserve_pixels() {
        let built = VideoFrame::builder().build().unwrap();
        assert_eq!((built.width(), built.height()), (1920, 1080));
        assert_eq!(built.pixel_format(), PixelFormat::Bgra);
        assert_eq!(built.scan_type(), ScanType::Progressive);
        assert_eq!(built.frame_rate(), FrameRate::new(60, 1).unwrap());
        assert_eq!(built.picture_aspect_ratio(), 16.0 / 9.0);
        assert_eq!(built.timecode(), Timecode::SYNTHESIZE);
        assert_eq!(built.timestamp(), None);
        assert!(built.data().iter().all(|&byte| byte == 0));
        let mut frame = VideoFrame::builder().resolution(2, 2).build().unwrap();
        let pixels = frame.data().as_ptr();
        frame.set_timecode(Timecode::from_ticks(123));
        frame.set_scan_type(ScanType::Field1);
        frame.set_frame_rate(FrameRate::new(30000, 1001).unwrap());
        frame.set_picture_aspect_ratio(0.0).unwrap();
        assert!(frame.set_picture_aspect_ratio(f32::NAN).is_err());
        let raw = frame.to_raw();
        assert_eq!(raw.timecode, 123);
        assert_eq!(raw.timestamp, NDIlib_recv_timestamp_undefined);
        assert_eq!((raw.frame_rate_N, raw.frame_rate_D), (30000, 1001));
        assert_eq!(raw.picture_aspect_ratio, 0.0);
        assert_eq!(frame.scan_type(), ScanType::Field1);
        assert_eq!(frame.data().as_ptr(), pixels);
    }

    #[test]
    fn owned_frames_clone_into_independent_allocations() {
        let mut video = VideoFrame::builder()
            .resolution(2, 2)
            .data(vec![7; 16])
            .metadata("<video/>")
            .build()
            .unwrap();
        let copy = video.clone();
        video.data_mut().fill(9);
        assert_ne!(copy.data().as_ptr(), video.data().as_ptr());
        assert_eq!(copy.data(), &[7; 16]);
        assert_eq!(copy.metadata(), Some("<video/>"));
        assert_ne!(
            copy.metadata_cstr().unwrap().as_ptr(),
            video.metadata_cstr().unwrap().as_ptr()
        );

        let mut audio = AudioFrame::builder()
            .num_channels(2)
            .num_samples(4)
            .metadata("<audio/>")
            .build()
            .unwrap();
        let copy = audio.clone();
        audio.data_mut().fill(1.0);
        assert_ne!(copy.data().as_ptr(), audio.data().as_ptr());
        assert!(copy.data().iter().all(|&sample| sample == 0.0));
        assert_eq!(copy.metadata(), Some("<audio/>"));

        let metadata = MetadataFrame::new("<text/>").unwrap();
        assert_eq!(metadata.clone(), metadata);
    }

    #[test]
    fn video_builder_retains_supplied_allocation_and_rejects_wrong_lengths() {
        let data = vec![37; 16];
        let pixels = data.as_ptr();
        let frame = VideoFrame::builder()
            .resolution(2, 2)
            .data(data)
            .build()
            .unwrap();
        assert_eq!(frame.data().as_ptr(), pixels);
        assert_eq!(frame.data(), &[37; 16]);
        for len in [0, 15, 17] {
            assert!(VideoFrame::builder()
                .resolution(2, 2)
                .data(vec![0; len])
                .build()
                .is_err());
        }
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn borrowed_video_encoding_uses_padded_pixels_without_owned_conversion() {
        use crate::capture::{FrameSyncVideoFree, Guard};
        let frame = image_test_frame(
            PixelFormat::Bgra,
            2,
            2,
            Some(12),
            vec![
                0, 0, 255, 255, 0, 255, 0, 128, 99, 99, 99, 99, 255, 0, 0, 64, 255, 255, 255, 0,
                99, 99, 99, 99,
            ],
        );
        // The null-instance FrameSync strategy never frees these test-owned
        // pixels. The owned source stays alive for the borrowed view's use.
        let guard = unsafe {
            Guard::<FrameSyncVideoFree>::with_test_free(ptr::null_mut(), frame.to_raw(), |_, _| {})
        };
        let borrowed = unsafe { VideoRef::new(guard) }.unwrap();
        assert!(borrowed.data().is_none());
        assert_eq!(
            borrowed.plane(0).unwrap().row(0).unwrap().as_ptr(),
            frame.data().as_ptr()
        );
        let png = borrowed.encode_png().unwrap();
        assert_eq!(
            decode_png_rgba(&png),
            (
                2,
                2,
                vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 255, 255, 255, 0,]
            )
        );
        assert_eq!(png, frame.encode_png().unwrap());
        assert_eq!(
            borrowed.encode_jpeg(85).unwrap(),
            frame.encode_jpeg(85).unwrap()
        );
        assert_eq!(
            borrowed.encode_data_url(ImageFormat::Png).unwrap(),
            frame.encode_data_url(ImageFormat::Png).unwrap()
        );
        assert_eq!(
            borrowed.encode_data_url(ImageFormat::Jpeg(85)).unwrap(),
            frame.encode_data_url(ImageFormat::Jpeg(85)).unwrap()
        );
        assert!(borrowed.encode_jpeg(0).is_err());

        let unsupported = VideoFrame::builder()
            .resolution(2, 2)
            .pixel_format(PixelFormat::Uyvy)
            .build()
            .unwrap();
        let guard = unsafe {
            Guard::<FrameSyncVideoFree>::with_test_free(
                ptr::null_mut(),
                unsupported.to_raw(),
                |_, _| {},
            )
        };
        let borrowed = unsafe { VideoRef::new(guard) }.unwrap();
        assert!(borrowed.encode_png().is_err());
        assert!(borrowed.encode_jpeg(85).is_err());
    }

    #[cfg(feature = "image-encoding")]
    fn image_test_frame(
        pixel_format: PixelFormat,
        width: u32,
        height: u32,
        line_stride: Option<usize>,
        data: Vec<u8>,
    ) -> VideoFrame {
        let layout =
            ValidatedVideoLayout::new_uncompressed(pixel_format, width, height, line_stride)
                .unwrap();
        assert_eq!(data.len(), layout.data_len_bytes);

        VideoFrame {
            layout,
            header: VideoHeader {
                frame_rate: FrameRate::new(60, 1).unwrap(),
                picture_aspect_ratio: 16.0 / 9.0,
                scan_type: ScanType::Progressive,
                timecode: Timecode::from_ticks(0),
            },
            data,
            metadata: None,
            timestamp: None,
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
                PixelFormat::Rgba,
                vec![10, 20, 30, 40, 50, 60, 70, 80],
                vec![10, 20, 30, 40, 50, 60, 70, 80],
            ),
            (
                PixelFormat::Bgra,
                vec![30, 20, 10, 40, 70, 60, 50, 80],
                vec![10, 20, 30, 40, 50, 60, 70, 80],
            ),
            (
                PixelFormat::Rgbx,
                vec![10, 20, 30, 0, 50, 60, 70, 99],
                vec![10, 20, 30, 255, 50, 60, 70, 255],
            ),
            (
                PixelFormat::Bgrx,
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
        let frame = image_test_frame(PixelFormat::Rgbx, 2, 2, Some(12), data);

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
            (PixelFormat::Rgba, vec![10, 20, 30, 40]),
            (PixelFormat::Bgra, vec![30, 20, 10, 40]),
            (PixelFormat::Rgbx, vec![10, 20, 30, 0]),
            (PixelFormat::Bgrx, vec![30, 20, 10, 0]),
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
        let frame = image_test_frame(PixelFormat::Bgrx, 2, 2, Some(12), data);

        let jpeg = frame.encode_jpeg(85).unwrap();
        assert_jpeg_markers(&jpeg);
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn test_encode_jpeg_rejects_invalid_quality() {
        let frame = image_test_frame(PixelFormat::Rgba, 2, 2, None, vec![0; 16]);

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
        let wide = image_test_frame(PixelFormat::Rgba, 65_536, 1, None, vec![0; 65_536 * 4]);
        let err = wide.encode_jpeg(85).unwrap_err();
        let err_msg = err.to_string();
        assert!(
            matches!(&err, Error::InvalidFrame(message) if message.contains("JPEG width")),
            "unexpected error: {err_msg}"
        );

        let tall = image_test_frame(PixelFormat::Rgba, 2, 65_536, None, vec![0; 65_536 * 8]);
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
        let frame = image_test_frame(PixelFormat::Uyvy, 2, 2, None, vec![0; 8]);

        let err = frame.encode_jpeg(85).unwrap_err();
        let err_msg = err.to_string();
        assert!(
            matches!(&err, Error::InvalidFrame(message) if message.contains("Unsupported format")),
            "unexpected error: {err_msg}"
        );
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn png_conversion_scratch_is_bounded_by_one_row() {
        for format in [
            PixelFormat::Rgba,
            PixelFormat::Bgra,
            PixelFormat::Rgbx,
            PixelFormat::Bgrx,
        ] {
            for height in [1, 19, 257] {
                let frame =
                    image_test_frame(format, 18, height, Some(84), vec![42; 84 * height as usize]);
                let source = frame.image_source().unwrap();
                let (scratch, allocations) =
                    crate::test_alloc::allocation_count(|| source.png_conversion_row().unwrap());
                let expected = if format == PixelFormat::Rgba {
                    0
                } else {
                    18 * 4
                };
                assert_eq!(scratch.len(), expected);
                assert_eq!(scratch.capacity(), expected);
                assert_eq!(allocations, usize::from(expected != 0));
                // The JPEG adapter borrows only the source; all component buffers
                // belong to the codec. Pre-reserving them proves conversion itself
                // allocates nothing, independently of image height.
                let mut buffers = std::array::from_fn(|_| Vec::with_capacity(18));
                let adapter = JpegRows {
                    source: &source,
                    width: 18,
                    height: height as u16,
                };
                let (_, allocations) = crate::test_alloc::allocation_count(|| {
                    for row in 0..height as u16 {
                        for buffer in &mut buffers {
                            buffer.clear();
                        }
                        jpeg_encoder::ImageBuffer::fill_buffers(&adapter, row, &mut buffers);
                    }
                });
                assert_eq!(allocations, 0);
            }
        }
    }

    /// Manual resource comparison with the pre-2.0 row-staging implementation.
    /// `cargo test --release --lib --features image-encoding image_encoding_resources -- --ignored --nocapture`
    #[cfg(feature = "image-encoding")]
    #[test]
    #[ignore = "Manual optimized image time/peak-allocation comparison"]
    fn image_encoding_resources() {
        use std::{hint::black_box, time::Instant};
        fn staged(source: &ImagePixelSource<'_>, format: ImageFormat) -> Vec<u8> {
            match format {
                ImageFormat::Png => {
                    let mut pixels = Vec::with_capacity(source.width * source.height * 4);
                    for row in source.plane.rows() {
                        for pixel in row.as_chunks::<4>().0 {
                            pixels.extend_from_slice(&source.pixel_format.rgba_pixel(pixel));
                        }
                    }
                    let mut output = Vec::new();
                    let mut encoder =
                        png::Encoder::new(&mut output, source.width as u32, source.height as u32);
                    encoder.set_color(png::ColorType::Rgba);
                    encoder.set_depth(png::BitDepth::Eight);
                    encoder
                        .write_header()
                        .unwrap()
                        .write_image_data(&pixels)
                        .unwrap();
                    output
                }
                ImageFormat::Jpeg(quality) => {
                    let mut pixels = Vec::with_capacity(source.width * source.height * 3);
                    for row in source.plane.rows() {
                        for pixel in row.as_chunks::<4>().0 {
                            pixels.extend_from_slice(&source.pixel_format.rgb_pixel(pixel));
                        }
                    }
                    let mut output = Vec::new();
                    jpeg_encoder::Encoder::new(&mut output, quality)
                        .encode(
                            &pixels,
                            source.width as u16,
                            source.height as u16,
                            jpeg_encoder::ColorType::Rgb,
                        )
                        .unwrap();
                    output
                }
            }
        }
        eprintln!(
            "width,height,padding,entropy,format,path,median_ms,peak_encode_bytes,output_bytes"
        );
        for (width, height) in [(1920, 1080), (3840, 2160), (7680, 4320)] {
            for padding in [0, 64] {
                for entropy in [false, true] {
                    let stride = width * 4 + padding;
                    let mut data = vec![42; stride * height];
                    if entropy {
                        let mut seed = 118u64;
                        for row in data.chunks_exact_mut(stride) {
                            for pixel in row[..width * 4].as_chunks_mut::<4>().0 {
                                for byte in &mut pixel[..3] {
                                    seed ^= seed << 13;
                                    seed ^= seed >> 7;
                                    seed ^= seed << 17;
                                    *byte = seed as u8;
                                }
                                pixel[3] = 255;
                            }
                        }
                    }
                    let frame = image_test_frame(
                        PixelFormat::Bgra,
                        width as u32,
                        height as u32,
                        Some(stride),
                        data,
                    );
                    let source = frame.image_source().unwrap();
                    for format in [
                        ImageFormat::Png,
                        ImageFormat::Jpeg(1),
                        ImageFormat::Jpeg(85),
                        ImageFormat::Jpeg(100),
                    ] {
                        for old in [true, false] {
                            let mut times = [0.0f64; 3];
                            let mut peak = 0;
                            let mut output_bytes = 0;
                            for time in &mut times {
                                let start = Instant::now();
                                let (output, bytes) = crate::test_alloc::peak_bytes(|| {
                                    if old {
                                        staged(black_box(&source), format)
                                    } else {
                                        match format {
                                            ImageFormat::Png => source.encode_png().unwrap(),
                                            ImageFormat::Jpeg(q) => source.encode_jpeg(q).unwrap(),
                                        }
                                    }
                                });
                                *time = start.elapsed().as_secs_f64() * 1000.0;
                                peak = peak.max(bytes);
                                output_bytes = output.len();
                                black_box(output);
                            }
                            times.sort_by(f64::total_cmp);
                            eprintln!("{width},{height},{padding},{entropy},{format:?},{},{:.3},{peak},{output_bytes}", if old { "staged" } else { "rows" }, times[1]);
                        }
                    }
                }
            }
        }
    }

    #[cfg(feature = "image-encoding")]
    #[test]
    fn streamed_images_match_independently_packed_pixels() {
        for format in [
            PixelFormat::Rgba,
            PixelFormat::Bgra,
            PixelFormat::Rgbx,
            PixelFormat::Bgrx,
        ] {
            for (width, height) in [(2, 1), (2, 2), (18, 19), (64, 33)] {
                for padding in [0, 12] {
                    let stride = width * 4 + padding;
                    let mut data = vec![0xCD; stride * height];
                    let mut rgba = Vec::new();
                    let mut rgb = Vec::new();
                    for y in 0..height {
                        for x in 0..width {
                            let r = (x * 17 + y * 7) as u8;
                            let g = (x * 31 + y * 13) as u8;
                            let b = (x * 43 + y * 19) as u8;
                            let a = (x * 59 + y * 23) as u8;
                            let pixel = if matches!(format, PixelFormat::Bgra | PixelFormat::Bgrx) {
                                [b, g, r, a]
                            } else {
                                [r, g, b, a]
                            };
                            data[y * stride + x * 4..y * stride + x * 4 + 4]
                                .copy_from_slice(&pixel);
                            rgba.extend_from_slice(&[
                                r,
                                g,
                                b,
                                if matches!(format, PixelFormat::Rgbx | PixelFormat::Bgrx) {
                                    255
                                } else {
                                    a
                                },
                            ]);
                            rgb.extend_from_slice(&[r, g, b]);
                        }
                    }
                    let frame =
                        image_test_frame(format, width as u32, height as u32, Some(stride), data);
                    assert_eq!(
                        decode_png_rgba(&frame.encode_png().unwrap()),
                        (width as u32, height as u32, rgba)
                    );
                    for quality in [1, 85, 100] {
                        let mut baseline = Vec::new();
                        jpeg_encoder::Encoder::new(&mut baseline, quality)
                            .encode(
                                &rgb,
                                width as u16,
                                height as u16,
                                jpeg_encoder::ColorType::Rgb,
                            )
                            .unwrap();
                        assert_eq!(
                            frame.encode_jpeg(quality).unwrap(),
                            baseline,
                            "{format:?} {width}x{height} padding={padding} quality={quality}"
                        );
                    }
                    // X and row padding have no effect on any encoded pixels.
                    let mut changed = image_test_frame(
                        format,
                        width as u32,
                        height as u32,
                        Some(stride),
                        frame.data().to_vec(),
                    );
                    for y in 0..height {
                        changed.data_mut()[y * stride + width * 4..(y + 1) * stride].fill(0x12);
                        if matches!(format, PixelFormat::Rgbx | PixelFormat::Bgrx) {
                            for x in 0..width {
                                changed.data_mut()[y * stride + x * 4 + 3] ^= 0xFF;
                            }
                        }
                    }
                    assert_eq!(frame.encode_png().unwrap(), changed.encode_png().unwrap());
                    assert_eq!(
                        frame.encode_jpeg(85).unwrap(),
                        changed.encode_jpeg(85).unwrap()
                    );
                }
            }
        }
    }

    /// Test PixelFormatInfo for packed RGB formats (32 bpp)
    #[test]
    fn test_pixel_format_info_packed_rgb() {
        let formats = [
            PixelFormat::Bgra,
            PixelFormat::Bgrx,
            PixelFormat::Rgba,
            PixelFormat::Rgbx,
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
            let len = info.buffer_len(7680, 1080).unwrap();
            assert_eq!(len, 7680 * 1080, "Format {:?} even dimensions", fmt);

            // Odd dimensions: 1921x1081
            let len = info.buffer_len(7684, 1081).unwrap();
            assert_eq!(len, 7684 * 1081, "Format {:?} odd dimensions", fmt);
        }
    }

    /// Test PixelFormatInfo for packed YUV formats
    #[test]
    fn test_pixel_format_info_packed_yuv() {
        // UYVY: 16 bpp = 2 bytes per pixel
        let info = PixelFormat::Uyvy.info();
        assert_eq!(info.bytes_per_pixel(), 2);
        assert_eq!(info.category(), FormatCategory::Packed);
        let len = info.buffer_len(3840, 1080).unwrap();
        assert_eq!(len, 3840 * 1080);

        // UYVA: UYVY plane (2 bytes per pixel) + alpha plane at half stride
        let info = PixelFormat::Uyva.info();
        assert_eq!(info.bytes_per_pixel(), 2);
        assert_eq!(info.category(), FormatCategory::PackedAlpha);
        assert!(!info.is_planar_420());
        let len = info.buffer_len(3840, 1080).unwrap();
        assert_eq!(len, 3840 * 1080 + 1920 * 1080);
        // Padded stride: alpha plane follows at stride / 2
        let len = info.buffer_len(4096, 1080).unwrap();
        assert_eq!(len, 4096 * 1080 + 2048 * 1080);
        assert!(info.buffer_len(3841, 1080).is_err());

        // P216: 16-bit Y plane + 16-bit CbCr plane, both at the line stride
        let info = PixelFormat::P216.info();
        assert_eq!(info.bytes_per_pixel(), 2);
        assert_eq!(info.category(), FormatCategory::SemiPlanar422);
        assert!(!info.is_planar_420());
        let len = info.buffer_len(3840, 1080).unwrap();
        assert_eq!(len, 3840 * 1080 * 2);

        // PA16: P216 + 16-bit alpha plane at the line stride
        let info = PixelFormat::Pa16.info();
        assert_eq!(info.bytes_per_pixel(), 2);
        assert_eq!(info.category(), FormatCategory::SemiPlanar422Alpha);
        assert!(!info.is_planar_420());
        let len = info.buffer_len(3840, 1080).unwrap();
        assert_eq!(len, 3840 * 1080 * 3);
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

        let info = PixelFormat::Yv12.info();
        assert_eq!(info.bytes_per_pixel(), 1);
        assert_eq!(info.category(), FormatCategory::Planar420);
        assert!(info.is_planar_420());
        let len = info.buffer_len(y_stride, 1080).unwrap();
        assert_eq!(len, 3_110_400, "YV12 1920x1080");

        let info = PixelFormat::I420.info();
        assert_eq!(info.category(), FormatCategory::Planar420);
        let len = info.buffer_len(y_stride, 1080).unwrap();
        assert_eq!(len, 3_110_400, "I420 1920x1080");
    }

    /// Test PixelFormatInfo rejects planar YV12/I420 with odd layout inputs
    #[test]
    fn test_pixel_format_info_planar_420_odd() {
        let y_stride = 1921;

        assert!(PixelFormat::Yv12.info().buffer_len(y_stride, 1081).is_err());
        assert!(PixelFormat::I420.info().buffer_len(y_stride, 1081).is_err());
    }

    /// Test PixelFormatInfo for semi-planar NV12 with even dimensions
    #[test]
    fn test_pixel_format_info_nv12_even() {
        // 1920x1080 NV12
        // Y: 1920 * 1080 = 2,073,600
        // UV: 1920 * (1080/2) = 1920 * 540 = 1,036,800
        // Total: 2,073,600 + 1,036,800 = 3,110,400
        let y_stride = 1920;

        let info = PixelFormat::Nv12.info();
        assert_eq!(info.bytes_per_pixel(), 1);
        assert_eq!(info.category(), FormatCategory::SemiPlanar420);
        assert!(info.is_planar_420());
        let len = info.buffer_len(y_stride, 1080).unwrap();
        assert_eq!(len, 3_110_400, "NV12 1920x1080");
    }

    /// Test PixelFormatInfo rejects semi-planar NV12 with odd layout inputs
    #[test]
    fn test_pixel_format_info_nv12_odd() {
        let y_stride = 1921;
        assert!(PixelFormat::Nv12.info().buffer_len(y_stride, 1081).is_err());
    }

    /// 4:2:0 parity is decided only by the shared plane layout, so every path
    /// answers alike: NV12 accepts an odd stride, YV12/I420 reject odd stride
    /// and odd height, and all three reject odd height.
    #[test]
    fn test_420_parity_is_consistent_across_paths() {
        // (format, height, stride, accepted)
        let cases = [
            (PixelFormat::Nv12, 1080, 1921, true),
            (PixelFormat::Nv12, 1080, 1920, true),
            (PixelFormat::Nv12, 1081, 1920, false),
            (PixelFormat::Yv12, 1080, 1921, false),
            (PixelFormat::I420, 1080, 1921, false),
            (PixelFormat::Yv12, 1081, 1920, false),
            (PixelFormat::I420, 1081, 1920, false),
            (PixelFormat::Yv12, 1080, 1920, true),
            (PixelFormat::I420, 1080, 1920, true),
        ];
        for (format, height, stride, accepted) in cases {
            let label = format!("{format:?} {stride}x{height}");
            let info_len = format.info().buffer_len(stride, height);
            let layout = ValidatedVideoLayout::new_uncompressed(format, 1920, height, Some(stride));
            assert_eq!(info_len.is_ok(), accepted, "buffer_len {label}");
            assert_eq!(layout.is_ok(), accepted, "new_uncompressed {label}");

            let data = vec![0u8; stride * height as usize * 2];
            let sender = crate::BorrowedVideoFrame::builder(&data, 1920, height, format)
                .line_stride(stride)
                .build();
            assert_eq!(sender.is_ok(), accepted, "BorrowedVideoFrame {label}");

            if let (Ok(len), Ok(layout)) = (info_len, layout) {
                assert_eq!(layout.data_len_bytes, len, "{label}");
            }
            if !accepted && height % 2 == 0 {
                let msg = format.info().buffer_len(stride, height).unwrap_err();
                let msg = msg.to_string();
                assert!(
                    msg.contains(&stride.to_string()) && msg.contains("4:2:0"),
                    "{label}: {msg}"
                );
            }
        }
        // NV12 odd stride: the UV plane is height/2 rows of the full stride.
        assert_eq!(
            PixelFormat::Nv12.info().buffer_len(1921, 1080).unwrap(),
            1921 * 1080 + 1921 * 540
        );
    }

    /// Test PixelFormat::line_stride for all formats
    #[test]
    fn test_pixel_format_line_stride() {
        // Packed RGB: 4 bytes per pixel
        assert_eq!(PixelFormat::Bgra.line_stride(1920).unwrap(), 7680);
        assert_eq!(PixelFormat::Bgrx.line_stride(1920).unwrap(), 7680);
        assert_eq!(PixelFormat::Rgba.line_stride(1920).unwrap(), 7680);
        assert_eq!(PixelFormat::Rgbx.line_stride(1920).unwrap(), 7680);

        // UYVY: 2 bytes per pixel
        assert_eq!(PixelFormat::Uyvy.line_stride(1920).unwrap(), 3840);

        // UYVA: UYVY plane stride = 2 bytes per pixel
        assert_eq!(PixelFormat::Uyva.line_stride(1920).unwrap(), 3840);

        // P216/PA16: Y-plane stride = 2 bytes per pixel
        assert_eq!(PixelFormat::P216.line_stride(1920).unwrap(), 3840);
        assert_eq!(PixelFormat::Pa16.line_stride(1920).unwrap(), 3840);

        // Planar 4:2:0: Y-plane stride = 1 byte per pixel
        assert_eq!(PixelFormat::Yv12.line_stride(1920).unwrap(), 1920);
        assert_eq!(PixelFormat::I420.line_stride(1920).unwrap(), 1920);
        assert_eq!(PixelFormat::Nv12.line_stride(1920).unwrap(), 1920);
    }

    /// Test PixelFormat::buffer_size for all formats
    #[test]
    fn test_pixel_format_buffer_size() {
        // Packed RGB: width * 4 * height
        assert_eq!(PixelFormat::Bgra.buffer_len(1920, 1080).unwrap(), 8_294_400);
        assert_eq!(PixelFormat::Rgba.buffer_len(1920, 1080).unwrap(), 8_294_400);

        // Planar 4:2:0: Y + U + V = 1.5 * width * height
        assert_eq!(PixelFormat::Yv12.buffer_len(1920, 1080).unwrap(), 3_110_400);
        assert_eq!(PixelFormat::I420.buffer_len(1920, 1080).unwrap(), 3_110_400);

        // Semi-planar 4:2:0: Y + UV = 1.5 * width * height
        assert_eq!(PixelFormat::Nv12.buffer_len(1920, 1080).unwrap(), 3_110_400);

        // UYVA: UYVY + alpha = 3 * width * height
        assert_eq!(PixelFormat::Uyva.buffer_len(1920, 1080).unwrap(), 6_220_800);

        // Semi-planar 4:2:2 16-bit: Y + CbCr = 4 * width * height
        assert_eq!(PixelFormat::P216.buffer_len(1920, 1080).unwrap(), 8_294_400);

        // Semi-planar 4:2:2 16-bit with alpha: Y + CbCr + A = 6 * width * height
        assert_eq!(
            PixelFormat::Pa16.buffer_len(1920, 1080).unwrap(),
            12_441_600
        );
    }

    /// Test PixelFormatInfo::is_planar_420 helper
    #[test]
    fn test_pixel_format_info_is_planar_420() {
        assert!(PixelFormat::Yv12.info().is_planar_420());
        assert!(PixelFormat::I420.info().is_planar_420());
        assert!(PixelFormat::Nv12.info().is_planar_420());

        assert!(!PixelFormat::Bgra.info().is_planar_420());
        assert!(!PixelFormat::Rgba.info().is_planar_420());
        assert!(!PixelFormat::Uyvy.info().is_planar_420());
        assert!(!PixelFormat::Uyva.info().is_planar_420());
    }

    /// Test VideoFrame builder with planar formats produces correct buffer sizes
    #[test]
    fn test_videoframe_builder_planar_even() {
        let frame = VideoFrame::builder()
            .resolution(1920, 1080)
            .pixel_format(PixelFormat::Nv12)
            .build()
            .expect("Builder should succeed");

        assert_eq!(frame.width(), 1920);
        assert_eq!(frame.height(), 1080);
        assert_eq!(frame.pixel_format(), PixelFormat::Nv12);
        assert_eq!(frame.data().len(), 3_110_400, "NV12 1920x1080 buffer size");
    }

    #[test]
    fn test_video_dimensions_across_all_formats() {
        let formats = [
            PixelFormat::Bgra,
            PixelFormat::Bgrx,
            PixelFormat::Rgba,
            PixelFormat::Rgbx,
            PixelFormat::Uyvy,
            PixelFormat::Uyva,
            PixelFormat::P216,
            PixelFormat::Pa16,
            PixelFormat::Yv12,
            PixelFormat::I420,
            PixelFormat::Nv12,
        ];
        let mut data = vec![0u8; 4096];
        for format in formats {
            for (width, height) in [(0, 2), (-2, 2), (1, 2), (3, 2), (2, 0), (2, -1)] {
                // The API's unsigned dimensions and the SDK's signed ones get
                // the same checks.
                if let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) {
                    assert!(
                        format.buffer_len(width, height).is_err(),
                        "{format:?} {width}x{height}"
                    );
                    assert!(VideoFrame::builder()
                        .resolution(width, height)
                        .pixel_format(format)
                        .build()
                        .is_err());
                    assert!(
                        crate::BorrowedVideoFrame::builder(&data, width, height, format)
                            .build()
                            .is_err()
                    );
                }
                let raw = NDIlib_video_frame_v2_t {
                    xres: width,
                    yres: height,
                    FourCC: FourCc::from(format).raw(),
                    frame_rate_N: 30,
                    frame_rate_D: 1,
                    picture_aspect_ratio: 0.0,
                    frame_format_type: ScanType::Progressive.to_sdk(),
                    timecode: 0,
                    p_data: data.as_mut_ptr(),
                    __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                        line_stride_in_bytes: 128,
                    },
                    p_metadata: ptr::null(),
                    timestamp: 0,
                };
                assert!(matches!(
                    validate_video_frame(&raw),
                    Err(Error::InvalidFrame(_))
                ));
                // Reject before forming a slice or copying received bytes.
                assert!(unsafe { VideoFrame::from_raw(&raw) }.is_err());
            }
            for width in [0, 1, 3] {
                assert!(
                    format.line_stride(width).is_err(),
                    "{format:?} width={width}"
                );
            }
            for height in [1, 2, 3] {
                let valid = !format.info().is_planar_420() || height % 2 == 0;
                assert_eq!(format.buffer_len(2, height).is_ok(), valid);
                assert_eq!(
                    VideoFrame::builder()
                        .resolution(2, height)
                        .pixel_format(format)
                        .build()
                        .is_ok(),
                    valid
                );
                assert_eq!(
                    crate::BorrowedVideoFrame::builder(&data, 2, height, format)
                        .build()
                        .is_ok(),
                    valid
                );
            }
            // Even width does not imply a larger alignment requirement.
            assert!(format.line_stride(6).is_ok());
            assert!(ValidatedVideoLayout::new_uncompressed(format, 6, 2, Some(128)).is_ok());
        }
        // RGB row padding need not be divisible by two or by a pixel size.
        assert!(ValidatedVideoLayout::new_uncompressed(PixelFormat::Bgra, 2, 3, Some(9)).is_ok());
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
            .pixel_format(PixelFormat::Bgra)
            .build()
            .expect("Builder should succeed");

        assert_eq!(frame.width(), 1920);
        assert_eq!(frame.height(), 1080);
        assert_eq!(frame.pixel_format(), PixelFormat::Bgra);
        assert_eq!(
            frame.data().len(),
            1920 * 1080 * 4,
            "BGRA buffer size unchanged"
        );
    }

    /// Test VideoFrame builder hands the SDK the first-plane stride for 4:2:2
    /// formats with extra planes
    #[test]
    fn test_videoframe_builder_semi_planar_422_and_alpha() {
        for (format, expected_len) in [
            (PixelFormat::Uyva, 256 * 2 * 64 + 256 * 64),
            (PixelFormat::P216, 256 * 2 * 64 * 2),
            (PixelFormat::Pa16, 256 * 2 * 64 * 3),
        ] {
            let frame = VideoFrame::builder()
                .resolution(256, 64)
                .pixel_format(format)
                .build()
                .expect("Builder should succeed");

            assert_eq!(frame.line_stride_bytes(), 256 * 2, "{format:?} line stride");
            assert_eq!(frame.data().len(), expected_len, "{format:?} buffer size");
        }
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
            FourCC: FourCc::from(PixelFormat::Nv12).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: y_stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let frame = unsafe { VideoFrame::from_raw(&c_frame) }.expect("from_raw should succeed");

        assert_eq!((frame.width(), frame.height()), (1920, 1080));
        assert_eq!(frame.pixel_format(), PixelFormat::Nv12);
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
            FourCC: FourCc::from(PixelFormat::I420).raw(),
            frame_rate_N: 30,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
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
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
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
        use crate::capture::{Guard, VideoKind};

        let width = 1920;
        let height = 1080;
        let stride = 1920 * 4;
        let expected_size = (stride * height) as usize;
        let mut data = vec![0u8; expected_size];

        let c_frame = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: 0xDEADBEEF, // Unknown FourCC
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let mock_instance = ptr::null_mut();
        let guard =
            unsafe { Guard::<VideoKind>::with_test_free(mock_instance, c_frame, |_, _| {}) };

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
    }

    /// Test that VideoFrameRef::new accepts known FourCC and stores validated format
    #[test]
    fn test_videoframeref_known_fourcc() {
        use crate::capture::{Guard, VideoKind};

        let width = 1920;
        let height = 1080;
        let stride = 1920 * 4;
        let expected_size = (stride * height) as usize;
        let mut data = vec![0u8; expected_size];

        let c_frame = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let mock_instance = ptr::null_mut();
        let guard =
            unsafe { Guard::<VideoKind>::with_test_free(mock_instance, c_frame, |_, _| {}) };

        let frame_ref = unsafe { VideoFrameRef::new(guard) }.expect("Should accept BGRA FourCC");
        assert_eq!(
            frame_ref.pixel_format(),
            PixelFormat::Bgra,
            "Should store validated pixel format"
        );
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
        let guard = unsafe { RecvAudioGuard::with_test_free(mock_instance, c_frame, |_, _| {}) };

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
        let guard = unsafe { RecvAudioGuard::with_test_free(mock_instance, c_frame, |_, _| {}) };

        let frame_ref = unsafe { AudioFrameRef::new(guard) }.expect("Should accept FLTP FourCC");
        assert_eq!(
            frame_ref.format(),
            AudioFormat::Fltp,
            "Should store validated audio format"
        );
    }

    /// Test that VideoFrameRef correctly uses validated format for data size calculation
    #[test]
    fn test_videoframeref_data_uses_validated_format() {
        use crate::capture::{Guard, VideoKind};

        // Test with uncompressed format (BGRA)
        let width = 1920;
        let height = 1080;
        let stride = 1920 * 4;
        let expected_size = (stride * height) as usize;
        let mut data = vec![0xAB_u8; expected_size];

        let c_frame = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let mock_instance = ptr::null_mut();
        let guard =
            unsafe { Guard::<VideoKind>::with_test_free(mock_instance, c_frame, |_, _| {}) };
        let frame_ref = unsafe { VideoFrameRef::new(guard) }.expect("Should create frame ref");

        // Verify data() returns correct size based on validated format
        assert_eq!(
            frame_ref.data().unwrap().len(),
            expected_size,
            "data() should use validated pixel format for size calculation"
        );

        // Verify line_stride_bytes() uses validated format
        assert_eq!(
            frame_ref.line_stride_bytes(),
            stride as usize,
            "line_stride_bytes() should use validated format"
        );
    }

    /// Regression lock against the receiver and FrameSync video refs re-diverging:
    /// given byte-identical raw frames, every shared accessor (including the
    /// previously-divergent `scan_type`) must return identical results now that
    /// both spellings share one [`VideoRef`] core.
    #[test]
    fn test_receiver_and_framesync_video_refs_agree() {
        use crate::capture::{FrameSyncVideoFree, Guard, VideoKind};

        let width = 16;
        let height = 8;
        let stride = width * 4;
        let len = (stride * height) as usize;

        // Two independent buffers with identical content, so each ref owns a
        // distinct (non-aliasing) frame while still comparing equal.
        let mut recv_data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let mut fs_data = recv_data.clone();
        let mut recv_meta = b"shared frame\0".to_vec();
        let mut fs_meta = recv_meta.clone();

        let raw = |data: &mut [u8], meta: &mut [u8]| NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 30,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            // A non-default scan type: the old FrameSync copy used
            // `unwrap_or(Progressive)`, so this also guards that divergence.
            frame_format_type: ScanType::Interlaced.to_sdk(),
            timecode: 123,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: meta.as_mut_ptr().cast(),
            timestamp: 456,
        };

        let recv_raw = raw(&mut recv_data, &mut recv_meta);
        let fs_raw = raw(&mut fs_data, &mut fs_meta);

        let recv_guard =
            unsafe { Guard::<VideoKind>::with_test_free(ptr::null_mut(), recv_raw, |_, _| {}) };
        let recv = unsafe { VideoFrameRef::new(recv_guard) }.expect("receiver video ref");
        let fs_guard = unsafe {
            Guard::<FrameSyncVideoFree>::with_test_free(ptr::null_mut(), fs_raw, |_, _| {})
        };
        let fs =
            unsafe { VideoRef::<FrameSyncVideoFree>::new(fs_guard) }.expect("framesync video ref");

        assert_eq!(recv.width(), fs.width());
        assert_eq!(recv.height(), fs.height());
        assert_eq!(recv.pixel_format(), fs.pixel_format());
        assert_eq!(recv.frame_rate(), fs.frame_rate());
        assert_eq!(recv.picture_aspect_ratio(), fs.picture_aspect_ratio());
        assert_eq!(recv.scan_type(), fs.scan_type());
        assert_eq!(recv.scan_type(), ScanType::Interlaced);
        assert_eq!(recv.timecode(), fs.timecode());
        assert_eq!(recv.timestamp(), fs.timestamp());
        assert_eq!(recv.line_stride_bytes(), fs.line_stride_bytes());
        assert_eq!(recv.metadata(), fs.metadata());
        assert_eq!(recv.data(), fs.data());
    }

    /// An out-of-range `frame_format_type` must be rejected by `validate_video_frame`
    /// before either video ref is constructed, so `scan_type()` can return the
    /// validated header without a fallible conversion.
    #[test]
    fn test_video_ref_rejects_out_of_range_scan_type() {
        use crate::capture::{FrameSyncVideoFree, Guard, VideoKind};

        let width = 16;
        let height = 8;
        let stride = width * 4;
        let len = (stride * height) as usize;
        let mut recv_data = vec![0u8; len];
        let mut fs_data = vec![0u8; len];

        let raw = |data: &mut [u8]| NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 30,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: 0x0BAD_F00D, // not a valid NDIlib_frame_format_type_e
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        // Frame validation rejects it directly...
        assert!(matches!(
            validate_video_frame(&raw(&mut recv_data)),
            Err(Error::InvalidFrame(_))
        ));

        // ...and so both ref constructors fail before any `scan_type()` call.
        let recv_guard = unsafe {
            Guard::<VideoKind>::with_test_free(ptr::null_mut(), raw(&mut recv_data), |_, _| {})
        };
        assert!(matches!(
            unsafe { VideoFrameRef::new(recv_guard) },
            Err(Error::InvalidFrame(_))
        ));

        let fs_guard = unsafe {
            Guard::<FrameSyncVideoFree>::with_test_free(
                ptr::null_mut(),
                raw(&mut fs_data),
                |_, _| {},
            )
        };
        assert!(matches!(
            unsafe { VideoRef::<FrameSyncVideoFree>::new(fs_guard) },
            Err(Error::InvalidFrame(_))
        ));
    }

    /// Test that MAX_VIDEO_BYTES constant is properly defined
    #[test]
    fn test_max_video_bytes_constant() {
        // Verify the crate resource cap is set to 256 MiB
        assert_eq!(MAX_VIDEO_BYTES, 256 * 1024 * 1024);
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
        let layout = unsafe { validate_metadata_layout(&raw) }.expect("empty null frame is valid");

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
        let layout =
            unsafe { validate_metadata_layout(&raw) }.expect("explicit empty payload is valid");

        assert_eq!(layout.len_with_nul, 1);
        assert_eq!(layout.text_len, 0);
    }

    #[test]
    fn test_validate_metadata_layout_accepts_utf8_with_bounded_length() {
        let mut data = "<ndi tally=\"preview\"/>".as_bytes().to_vec();
        data.push(0);
        let length = data.len() as i32;
        let raw = metadata_raw_from_bytes(&mut data, length);
        let layout = unsafe { validate_metadata_layout(&raw) }.expect("valid UTF-8 metadata");

        assert_eq!(layout.len_with_nul, data.len());
        assert_eq!(layout.text_len, data.len() - 1);
    }

    #[test]
    fn test_validate_metadata_layout_ignores_bytes_after_length() {
        let mut data = b"ok\0\0\xFF".to_vec();
        let raw = metadata_raw_from_bytes(&mut data, 3);
        let layout =
            unsafe { validate_metadata_layout(&raw) }.expect("extra bytes after length ignored");

        assert_eq!(layout.text_len, 2);
        let owned = unsafe { MetadataFrame::from_raw_validated(&raw, layout) };
        assert_eq!(owned.data(), "ok");
    }

    #[test]
    fn test_validate_metadata_layout_rejects_negative_length() {
        let raw = null_metadata_raw(-1);

        assert!(matches!(
            unsafe { validate_metadata_layout(&raw) },
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_metadata_layout_accepts_lengthless_non_null_data() {
        let mut data = b"<ndi/>\0ignored".to_vec();
        let raw = metadata_raw_from_bytes(&mut data, 0);
        let layout = unsafe { validate_metadata_layout(&raw) }.expect("SDK C-string sentinel");
        assert_eq!(layout.text_len, 6);
        assert_eq!(layout.len_with_nul, 7);
        let owned = unsafe { MetadataFrame::from_raw_validated(&raw, layout) };
        assert_eq!(owned.data(), "<ndi/>");
    }

    #[test]
    fn test_lengthless_metadata_rejects_invalid_utf8_and_missing_nul() {
        let mut invalid = vec![0xff, 0];
        assert!(matches!(
            unsafe { validate_metadata_layout(&metadata_raw_from_bytes(&mut invalid, 0)) },
            Err(Error::InvalidUtf8(_))
        ));
        let mut unterminated = vec![b'x'; MAX_METADATA_BYTES];
        assert!(unsafe {
            validate_metadata_layout(&metadata_raw_from_bytes(&mut unterminated, 0))
        }
        .is_err());
    }

    #[test]
    fn test_validate_metadata_layout_rejects_nonzero_length_null_data() {
        let raw = null_metadata_raw(1);

        assert!(matches!(
            unsafe { validate_metadata_layout(&raw) },
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_metadata_layout_rejects_missing_trailing_nul() {
        let mut data = b"abc".to_vec();
        let length = data.len() as i32;
        let raw = metadata_raw_from_bytes(&mut data, length);

        assert!(matches!(
            unsafe { validate_metadata_layout(&raw) },
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_metadata_layout_rejects_interior_nul() {
        let mut data = b"a\0b\0".to_vec();
        let length = data.len() as i32;
        let raw = metadata_raw_from_bytes(&mut data, length);

        assert!(matches!(
            unsafe { validate_metadata_layout(&raw) },
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_metadata_layout_rejects_oversized_length_before_reading() {
        let mut data = vec![0u8];
        let raw = metadata_raw_from_bytes(&mut data, (MAX_METADATA_BYTES + 1) as i32);

        assert!(matches!(
            unsafe { validate_metadata_layout(&raw) },
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_validate_metadata_layout_rejects_invalid_utf8() {
        let mut data = vec![0xFF, 0];
        let length = data.len() as i32;
        let raw = metadata_raw_from_bytes(&mut data, length);

        assert!(matches!(
            unsafe { validate_metadata_layout(&raw) },
            Err(Error::InvalidUtf8(_))
        ));
    }

    #[test]
    fn test_metadata_frame_constructor_and_accessors_preserve_text() {
        let frame = MetadataFrame::new("<ndi_product/>").unwrap();
        assert_eq!(frame.timecode(), Timecode::SYNTHESIZE);
        let frame = frame.with_timecode(Timecode::from_ticks(9876));

        assert_eq!(frame.data(), "<ndi_product/>");
        assert_eq!(frame.timecode(), Timecode::from_ticks(9876));
        assert_eq!(frame.clone().into_data(), "<ndi_product/>");
    }

    #[test]
    fn test_metadata_frame_rejects_interior_nul_input() {
        assert!(matches!(
            MetadataFrame::new("bad\0metadata"),
            Err(Error::InvalidCString(_))
        ));

        let mut frame = MetadataFrame::new("").unwrap();
        assert!(matches!(
            frame.set_data("bad\0metadata"),
            Err(Error::InvalidCString(_))
        ));
    }

    #[test]
    fn test_metadata_frame_rejects_oversized_input() {
        let oversized = "x".repeat(MAX_METADATA_BYTES);

        assert!(matches!(
            MetadataFrame::new(oversized),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_metadata_frame_setters_preserve_invariants() {
        let mut frame = MetadataFrame::new("").unwrap();

        frame.set_data("updated").unwrap();
        frame.set_timecode(Timecode::from_ticks(42));

        assert_eq!(frame.data(), "updated");
        assert_eq!(frame.timecode(), Timecode::from_ticks(42));
        assert!(frame.set_data("bad\0text").is_err());
        assert_eq!(frame.data(), "updated");
    }

    #[test]
    fn validated_metadata_reuses_storage_and_copies_once() {
        let text = "é".repeat(550);
        let frame = MetadataFrame::new(text).unwrap();
        let expected = frame.data.as_ptr() as *mut c_char;
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..100 {
                let raw = frame.to_raw();
                assert_eq!(raw.p_data, expected);
                assert_eq!(raw.length, 1101);
                assert_eq!(frame.data().len(), 1100);
            }
        });
        assert_eq!(allocations, 0);

        let bytes = frame.data.inner.as_bytes_with_nul();
        let layout = ValidatedFrameMetadata {
            len_with_nul: NonZeroUsize::new(bytes.len()),
            text_len: bytes.len() - 1,
        };
        let (copy, allocations) = crate::test_alloc::allocation_count(|| unsafe {
            FrameMetadata::copy_from_raw_validated(bytes.as_ptr().cast(), layout).unwrap()
        });
        assert_eq!(allocations, 1);
        assert_eq!(copy.as_str(), frame.data());
        let (text, allocations) = crate::test_alloc::allocation_count(|| copy.into_string());
        assert_eq!(allocations, 0);
        assert_eq!(text, frame.data());
    }

    #[test]
    fn test_metadata_frame_from_raw_validated_copies_only_payload() {
        let mut data = b"copy-me\0\0\xFF".to_vec();
        let raw = metadata_raw_from_bytes(&mut data, 8);
        let layout = unsafe { validate_metadata_layout(&raw) }.unwrap();
        let frame = unsafe { MetadataFrame::from_raw_validated(&raw, layout) };

        assert_eq!(frame.data(), "copy-me");
        assert_eq!(frame.timecode(), Timecode::from_ticks(12345));
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
        let frame = MetadataFrame::new("abc")
            .unwrap()
            .with_timecode(Timecode::from_ticks(101));
        let raw = frame.to_raw();

        assert_eq!(raw.length, 4);
        assert_eq!(raw.timecode, 101);
        assert_eq!(raw.p_data, frame.data.as_ptr() as *mut c_char);
        assert_eq!(frame.data.inner.as_bytes_with_nul(), b"abc\0");
    }

    #[test]
    fn test_empty_metadata_frame_to_raw_sends_explicit_nul() {
        let frame = MetadataFrame::new("").unwrap();
        let raw = frame.to_raw();

        assert_eq!(raw.length, 1);
        assert_eq!(raw.timecode, NDIlib_send_timecode_synthesize);
        assert_eq!(frame.data.inner.as_bytes_with_nul(), b"\0");
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
        let guard = unsafe { RecvMetadataGuard::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let frame_ref = unsafe { MetadataFrameRef::new(guard) }.expect("valid metadata ref");

        assert_eq!(frame_ref.data(), "zero-copy");
        assert_eq!(frame_ref.as_bytes(), b"zero-copy");
        assert_eq!(frame_ref.layout.text_len, 9);

        assert_eq!(frame_ref.timecode(), Timecode::from_ticks(12345));
        let owned = frame_ref.to_owned();
        assert_eq!(owned.data(), "zero-copy");
        assert_eq!(owned.timecode(), Timecode::from_ticks(12345));
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
        use crate::capture::{Guard, VideoKind};

        let width = 16;
        let height = 8;
        let stride = width * 4;
        let expected_len = (stride * height) as usize;
        let data = vec![0u8; expected_len];
        let mut metadata = b"cached\0\xFF".to_vec();

        let raw = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 123,
            p_data: data.as_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: metadata.as_mut_ptr().cast::<c_char>(),
            timestamp: 456,
        };

        let guard = unsafe { Guard::<VideoKind>::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let frame_ref = unsafe { VideoFrameRef::new(guard) }.expect("valid video ref");

        assert_eq!(frame_ref.metadata(), Some("cached"));
        metadata[6] = b'!';
        assert_eq!(frame_ref.metadata(), Some("cached"));
        assert!(format!("{frame_ref:?}").contains("metadata: Some(\"cached\")"));

        let owned = frame_ref.try_to_owned().expect("owned conversion");
        assert_eq!(owned.metadata(), Some("cached"));
        assert_eq!(owned.data().len(), expected_len);
        assert_eq!(owned.timecode(), Timecode::from_ticks(123));
        assert_eq!(owned.timestamp(), Some(SdkTimestamp::from_ticks(456)));
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

        let guard = unsafe { RecvAudioGuard::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let frame_ref = unsafe { AudioFrameRef::new(guard) }.expect("valid audio ref");

        assert_eq!(frame_ref.metadata(), Some("audio meta"));
        assert_eq!(frame_ref.timecode(), Timecode::from_ticks(123));
        assert_eq!(frame_ref.timestamp(), Some(SdkTimestamp::from_ticks(456)));
        let owned = frame_ref.try_to_owned().expect("owned conversion");
        assert_eq!(owned.metadata(), Some("audio meta"));
        assert_eq!(owned.data().len(), sample_count);
        assert_eq!(owned.timecode(), Timecode::from_ticks(123));
        assert_eq!(owned.timestamp(), Some(SdkTimestamp::from_ticks(456)));
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
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
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

        let result = unsafe { AudioFrame::from_raw(raw) };
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

        let result = unsafe { AudioFrame::from_raw(raw) };
        assert!(
            result.is_ok(),
            "Should accept normal audio frame within bounds"
        );

        let frame = result.unwrap();
        assert_eq!(frame.data().len(), sample_count);
        assert_eq!((frame.num_samples(), frame.num_channels()), (1024, 2));
        assert_eq!(frame.sample_rate(), 48_000);
        assert_eq!(frame.timestamp(), Some(SdkTimestamp::from_ticks(67890)));
    }

    #[test]
    fn test_audio_builder_rejects_invalid_dimensions() {
        assert!(matches!(
            AudioFrame::builder().sample_rate(0).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder().sample_rate(1 << 31).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder().num_channels(0).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder().num_samples(0).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder().num_samples(u32::MAX).build(),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_audio_builder_rejects_oversized_layout() {
        let samples = (MAX_AUDIO_BYTES / std::mem::size_of::<f32>()) as u32 + 1;
        let result = AudioFrame::builder()
            .num_channels(1)
            .num_samples(samples)
            .build();

        assert!(matches!(result, Err(Error::InvalidFrame(_))));
    }

    #[test]
    fn test_default_outbound_timecodes_are_synthesized() {
        let video = VideoFrame::builder().resolution(16, 16).build().unwrap();
        assert_eq!(video.timecode(), Timecode::SYNTHESIZE);
        assert_eq!(video.to_raw().timecode, NDIlib_send_timecode_synthesize);
        let audio = AudioFrame::builder().num_samples(1).build().unwrap();
        assert_eq!(audio.timecode(), Timecode::SYNTHESIZE);
        assert_eq!(audio.to_raw().timecode, NDIlib_send_timecode_synthesize);
        assert_eq!(
            MetadataFrame::new("<x/>").unwrap().timecode(),
            Timecode::SYNTHESIZE
        );
        let zero = Timecode::from_ticks(0);
        assert_eq!(
            VideoFrame::builder()
                .resolution(16, 16)
                .timecode(zero)
                .build()
                .unwrap()
                .to_raw()
                .timecode,
            0
        );
        assert_eq!(
            AudioFrame::builder()
                .num_samples(1)
                .timecode(zero)
                .build()
                .unwrap()
                .to_raw()
                .timecode,
            0
        );
        assert_eq!(
            MetadataFrame::new("<x/>")
                .unwrap()
                .with_timecode(zero)
                .to_raw()
                .timecode,
            0
        );
    }

    #[test]
    fn test_video_builder_rejects_invalid_send_metadata() {
        assert!(matches!(FrameRate::new(0, 1), Err(Error::InvalidInput(_))));
        assert!(matches!(FrameRate::new(30, 0), Err(Error::InvalidInput(_))));
        assert!(matches!(
            VideoFrame::builder().picture_aspect_ratio(f32::NAN).build(),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            VideoFrame::builder().picture_aspect_ratio(-1.0).build(),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_zero_aspect_ratio_uses_sdk_square_pixel_sentinel() {
        let frame = VideoFrame::builder()
            .resolution(16, 16)
            .picture_aspect_ratio(0.0)
            .build()
            .unwrap();
        assert_eq!(frame.picture_aspect_ratio(), 0.0);
        assert_eq!(frame.to_raw().picture_aspect_ratio, 0.0);
    }

    #[test]
    fn test_owned_frame_data_replacement_preserves_layout_size() {
        let mut video = VideoFrame::builder()
            .resolution(16, 16)
            .pixel_format(PixelFormat::Bgra)
            .build()
            .unwrap();
        let pixels = video.data().as_ptr();
        let rejected = video
            .replace_data(vec![0; video.data().len() - 1])
            .unwrap_err();
        assert!(matches!(rejected.error(), Error::InvalidFrame(_)));
        assert_eq!(rejected.into_inner().len(), 16 * 16 * 4 - 1);
        assert_eq!(video.data().as_ptr(), pixels);
        video.replace_data(vec![5; 16 * 16 * 4]).unwrap();
        assert_eq!(video.data(), vec![5; 16 * 16 * 4]);

        let mut audio = AudioFrame::builder()
            .num_channels(2)
            .num_samples(16)
            .build()
            .unwrap();
        let rejected = audio
            .replace_data(vec![0.0; audio.data().len() + 1])
            .unwrap_err();
        assert!(matches!(rejected.error(), Error::InvalidFrame(_)));
        assert_eq!(rejected.into_inner().len(), 33);
        audio.replace_data(vec![0.5; 32]).unwrap();
        assert_eq!(audio.channel_data(1), Some(&[0.5; 16][..]));
    }

    #[test]
    fn test_video_cap_allows_8k_bgra_without_allocating() {
        let layout =
            ValidatedVideoLayout::new_uncompressed(PixelFormat::Bgra, 7680, 4320, None).unwrap();
        assert_eq!(layout.data_len_bytes, 132_710_400);
        assert!(matches!(
            ValidatedVideoLayout::new_uncompressed(PixelFormat::Bgra, 16_384, 8_640, None),
            Err(Error::InvalidFrame(_))
        ));
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
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
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
        assert_eq!(layout.pixel_format, PixelFormat::Bgra);
        assert_eq!(layout.data_len_bytes, expected_size);
        assert_eq!(layout.sdk_line_stride(), stride);
        assert_eq!(layout.line_stride_bytes, stride as usize);
    }

    /// Test validate_video_layout accepts the first-plane strides the SDK
    /// delivers for UYVA, P216 and PA16
    #[test]
    fn test_validate_video_layout_semi_planar_422_and_alpha() {
        let width = 256;
        let height = 64;
        let stride = width * 2;
        let plane = (stride * height) as usize;

        for (format, expected_size) in [
            (PixelFormat::Uyva, plane + plane / 2),
            (PixelFormat::P216, plane * 2),
            (PixelFormat::Pa16, plane * 3),
        ] {
            let mut data = vec![0u8; expected_size];

            let raw = NDIlib_video_frame_v2_t {
                xres: width,
                yres: height,
                FourCC: FourCc::from(format).raw(),
                frame_rate_N: 60,
                frame_rate_D: 1,
                picture_aspect_ratio: 16.0 / 9.0,
                frame_format_type: ScanType::Progressive.to_sdk(),
                timecode: 0,
                p_data: data.as_mut_ptr(),
                __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                    line_stride_in_bytes: stride,
                },
                p_metadata: ptr::null(),
                timestamp: 0,
            };

            let layout = validate_video_layout(&raw)
                .unwrap_or_else(|e| panic!("{format:?} stride {stride} rejected: {e}"));
            assert_eq!(layout.pixel_format, format);
            assert_eq!(layout.data_len_bytes, expected_size, "{format:?}");
            assert_eq!(layout.sdk_line_stride(), stride);
            assert_eq!(layout.line_stride_bytes, stride as usize);
        }
    }

    /// Test validate_video_layout rejects an odd UYVA stride, since the alpha
    /// plane stride is half the line stride
    #[test]
    fn test_validate_video_layout_rejects_odd_uyva_stride() {
        let width = 256;
        let height = 64;
        let stride = width * 2 + 1;
        let mut data = vec![0u8; (stride * height * 2) as usize];

        let raw = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: FourCc::from(PixelFormat::Uyva).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        assert!(matches!(
            validate_video_layout(&raw),
            Err(Error::InvalidFrame(_))
        ));
    }

    /// Test validate_video_layout rejects null data pointer
    #[test]
    fn test_validate_video_layout_null_pointer() {
        let raw = NDIlib_video_frame_v2_t {
            xres: 1920,
            yres: 1080,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
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
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: -1, // Invalid stride
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let result = validate_video_layout(&raw);
        assert!(result.is_err(), "Should reject negative line_stride");

        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(
                msg.contains("invalid line_stride_in_bytes"),
                "Error should mention invalid stride, got: {msg}"
            );
        } else {
            panic!("Expected InvalidFrame error");
        }
    }

    #[test]
    fn test_validate_video_layout_accepts_default_zero_stride() {
        let mut data = vec![0u8; 16 * 16 * 4];
        let raw = NDIlib_video_frame_v2_t {
            xres: 16,
            yres: 16,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 1.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 0,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };
        let layout = validate_video_layout(&raw).unwrap();
        assert_eq!(layout.data_len_bytes, data.len());
        assert_eq!(layout.sdk_line_stride(), 64);
        assert_eq!(layout.line_stride_bytes, 64);
        let owned = unsafe { VideoFrame::from_raw(&raw) }.unwrap();
        assert_eq!(owned.data().len(), data.len());
    }

    /// Test validate_video_layout rejects strides smaller than one row.
    #[test]
    fn test_validate_video_layout_rejects_short_stride() {
        let mut data = vec![0u8; 1024];

        let raw = NDIlib_video_frame_v2_t {
            xres: 1920,
            yres: 1080,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
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
            FourCC: FourCc::from(PixelFormat::I420).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 4.0 / 3.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
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
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
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
        // Validation rejects before reading data, so no large allocation is needed.
        let width = 16_384;
        let height = 8_640;
        let stride = width * 4;
        let mut data = [0u8; 1];

        let raw = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
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
        assert_eq!(layout.format, Some(AudioFormat::Fltp));
        assert_eq!(layout.sample_count, sample_count);
    }

    #[test]
    fn test_audio_channel_views_and_owned_conversion_skip_padding() {
        use crate::capture::FrameSyncAudioFree;
        use std::mem::MaybeUninit;
        // Only samples are initialized; gap slots are not audio data.
        let mut storage = [MaybeUninit::<f32>::uninit(); 10];
        for (index, value) in [(0, 1.0), (1, 2.0), (2, 3.0), (6, 4.0), (7, 5.0), (8, 6.0)] {
            storage[index].write(value);
        }
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            no_samples: 3,
            FourCC: AudioFormat::Fltp.to_sdk(),
            p_data: storage.as_mut_ptr().cast(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 24,
            },
            timecode: 123,
            timestamp: 456,
            p_metadata: ptr::null(),
        };
        let guard =
            unsafe { Guard::<FrameSyncAudioFree>::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let frame = unsafe { AudioRef::<FrameSyncAudioFree>::new(guard, false) }.unwrap();
        assert!(frame.data().is_none());
        assert_eq!(frame.channel_data(0).unwrap(), &[1.0, 2.0, 3.0]);
        assert_eq!(frame.channel_data(1).unwrap(), &[4.0, 5.0, 6.0]);
        assert_eq!(frame.channel_data(1).unwrap().as_ptr(), unsafe {
            storage.as_ptr().add(6).cast::<f32>()
        });
        assert!(frame.channel_data(2).is_none());
        assert_eq!(frame.channels().len(), 2);
        assert_eq!(
            frame.channels().flatten().copied().collect::<Vec<_>>(),
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
        );
        let mut owned = frame.try_to_owned().unwrap().unwrap();
        assert_eq!(owned.channel_stride_bytes(), 12);
        assert_eq!(owned.data(), &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(
            owned.channels().flatten().copied().collect::<Vec<_>>(),
            owned.data()
        );
        assert_eq!(owned.timecode(), Timecode::from_ticks(123));
        assert_eq!(owned.timestamp(), Some(SdkTimestamp::from_ticks(456)));
        assert_eq!(owned.channel_data(1).unwrap().as_ptr(), unsafe {
            owned.data().as_ptr().add(3)
        });
        owned.channel_data_mut(1).unwrap()[0] = 99.0;
        assert_eq!(owned.data()[3], 99.0);
        assert!(owned.channel_data_mut(2).is_none());
        assert!(owned.replace_data(vec![0.0; 10]).is_err());
    }

    #[test]
    fn test_audio_rejects_unaligned_float_pointer_before_access() {
        let mut storage = [0.0f32; 4];
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 1,
            no_samples: 1,
            FourCC: AudioFormat::Fltp.to_sdk(),
            p_data: unsafe { storage.as_mut_ptr().cast::<u8>().add(1) },
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 4,
            },
            timecode: 0,
            timestamp: 0,
            p_metadata: ptr::null(),
        };
        assert!(matches!(
            validate_audio_layout(&raw),
            Err(Error::InvalidFrame(_))
        ));
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
        assert_eq!(layout.format(), Some(AudioFormat::Fltp));
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

    /// Test calculate_buffer_len_checked matches PixelFormatInfo::buffer_len for valid inputs
    #[test]
    fn test_calculate_buffer_len_checked_matches_public_helper() {
        let test_cases = [
            (PixelFormat::Bgra, 7680usize, 1080usize),
            (PixelFormat::Uyvy, 3840usize, 1080usize),
            (PixelFormat::Nv12, 1920usize, 1080usize),
            (PixelFormat::Yv12, 1920usize, 1080usize),
            (PixelFormat::I420, 1920usize, 1080usize),
        ];

        for (format, stride, height) in test_cases {
            let expected = format.info().buffer_len(stride, height as u32).unwrap();
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
        use crate::capture::{Guard, VideoKind};

        // Create a mock video frame for testing
        let width = 1920;
        let height = 1080;
        let stride = width * 4;
        let expected_size = (stride * height) as usize;
        let data = vec![0u8; expected_size];

        let raw = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 12345,
            p_data: data.as_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 67890,
        };

        let guard = unsafe { Guard::<VideoKind>::with_test_free(ptr::null_mut(), raw, |_, _| {}) };

        // Create the VideoFrameRef
        let frame_ref = unsafe { VideoFrameRef::new(guard) };
        assert!(frame_ref.is_ok(), "Should create valid VideoFrameRef");

        let frame_ref = frame_ref.unwrap();

        // Verify the cached length is used
        assert_eq!(
            frame_ref.data().unwrap().len(),
            expected_size,
            "data() should return slice with cached length"
        );
        assert_eq!(
            frame_ref.layout.data_len_bytes, expected_size,
            "Cached data_len_bytes should match expected"
        );
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

        let guard = unsafe { RecvAudioGuard::with_test_free(ptr::null_mut(), raw, |_, _| {}) };

        // Create the AudioFrameRef
        let frame_ref = unsafe { AudioFrameRef::new(guard) };
        assert!(frame_ref.is_ok(), "Should create valid AudioFrameRef");

        let frame_ref = frame_ref.unwrap();

        // Verify the cached sample count is used
        assert_eq!(
            frame_ref.data().unwrap().len(),
            sample_count,
            "data() should return slice with cached sample count"
        );
        assert_eq!(
            frame_ref.layout.sample_count, sample_count,
            "Cached sample_count should match expected"
        );
    }
}
