//! Frame types for video, audio, and metadata.

use std::{
    ffi::{CStr, CString},
    fmt,
    marker::PhantomData,
    num::NonZeroU32,
    os::raw::c_char,
    ptr, slice, str,
};

use crate::{
    capture::{
        AudioKind, FrameFree, FrameSyncAudioFree, FrameSyncVideoFree, Guard, RecvAudioGuard,
        RecvMetadataGuard, VideoKind,
    },
    framesync::{AudioInputFormat, FrameSyncAudioRef, FrameSyncVideoRef},
    pixels::Pixels,
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

    /// The complete readable allocation size for sending this layout.
    ///
    /// `line_stride_bytes` is `None` for tightly packed rows, or the byte
    /// distance between rows. The returned size includes every plane and
    /// the extra readable bytes required by the format and scan type. It is
    /// suitable for allocating external buffers used with
    /// [`BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail`](crate::BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail).
    /// Padding and tail bytes must be initialized before direct sending;
    /// the visible picture still occupies [`Self::buffer_len`] bytes for a
    /// packed layout. Owned frames initialize their own tail as needed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] for invalid dimensions, stride,
    /// excessive layout size, or arithmetic overflow.
    pub fn send_buffer_len(
        self,
        width: u32,
        height: u32,
        line_stride_bytes: Option<usize>,
        scan_type: ScanType,
    ) -> Result<usize> {
        let layout =
            ValidatedVideoLayout::new_uncompressed(self, width, height, line_stride_bytes)?;
        layout
            .data_len_bytes
            .checked_add(crate::pixels::send_room(&layout, scan_type))
            .ok_or_else(|| Error::InvalidFrame("Video send allocation size overflow".into()))
    }

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

    /// The number of readable bytes the NDI SDK needs after the last pixel
    /// of a frame of this format that it sends: 64 for [`Self::Uyvy`] and
    /// [`Self::Uyva`], and zero for every other format. On 64-bit ARM Linux
    /// it is zero for every format.
    ///
    /// The x86 builds of the SDK read UYVY and UYVA frames a fixed number of
    /// bytes at a time, and for many frames the last read ends after the
    /// frame, by up to 28 bytes. Whether it does depends on the frame's
    /// width, height and scan type and on where its buffer lies in memory: a
    /// progressive 1920x1080 frame is read exactly, the same frame interlaced
    /// is not, and neither is a 720x480 frame or any frame whose width is not
    /// a multiple of 16. The SDK documents no such requirement, and it
    /// compresses what it reads there into the picture's last blocks. The
    /// 64-bit ARM Linux build reads every frame exactly. The figures are
    /// measured with the Linux runtimes 6.1.1, 6.3.2 and 6.4.0.2; the SDK
    /// validation notes in the repository's `docs` directory have the method
    /// and the numbers. Every build but the 64-bit ARM Linux one takes the
    /// x86 figure, the 32-bit x86 Linux build included.
    ///
    /// The crate keeps this room for the SDK, and fills it with zeros, so a
    /// program needs the number only to avoid a copy or a reallocation:
    ///
    /// - A [`VideoFrame`] has the complete room for its layout and scan
    ///   type. Construction, copying, pixel replacement and scan-type
    ///   changes grow its allocation when necessary, so sending it needs
    ///   no wrapper pixel copy or reallocation.
    /// - [`Sender::send_video`](crate::Sender::send_video) sends a
    ///   [`BorrowedVideoFrame`](crate::BorrowedVideoFrame) through a packed,
    ///   zero-tailed copy by default when this room is nonzero. Direct sending
    ///   requires both sufficient room and explicit opt-in through
    ///   [`BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail`](crate::BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail).
    ///   [`BorrowedVideoFrame::sends_in_place`](crate::BorrowedVideoFrame::sends_in_place)
    ///   tells which.
    /// - A buffer that cannot be made longer, but is known to be followed by
    ///   initialized memory in the same allocation, can say so with
    ///   [`BorrowedVideoFrameBuilder::readable_after`](crate::BorrowedVideoFrameBuilder::readable_after).
    ///
    /// One kind of frame needs two rows more, twice the line stride: an
    /// interlaced or single-field UYVY or UYVA frame whose height is not a
    /// multiple of four, which the SDK reads as far as the row after next.
    /// Owned frames keep these extra rows too. A borrowed frame is sent
    /// from a copy unless its buffer has that room and direct sending was
    /// explicitly enabled.
    ///
    /// # Examples
    ///
    /// A buffer the sender does not copy:
    ///
    /// ```
    /// use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
    ///
    /// let format = PixelFormat::Uyvy;
    /// let buffer = vec![0u8; 720 * 480 * 2 + format.send_tail_room()];
    /// let frame = BorrowedVideoFrame::builder(&buffer, 720, 480, format)
    ///     .allow_send_from_padding_and_tail(true)
    ///     .build()?;
    /// assert!(frame.sends_in_place());
    ///
    /// assert_eq!(PixelFormat::Bgra.send_tail_room(), 0);
    /// # Ok::<(), grafton_ndi::Error>(())
    /// ```
    #[must_use]
    pub const fn send_tail_room(self) -> usize {
        // Measured exact: the SDK for 64-bit ARM Linux.
        if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            return 0;
        }
        match self {
            Self::Uyvy | Self::Uyva => 64,
            Self::P216
            | Self::Pa16
            | Self::Yv12
            | Self::I420
            | Self::Nv12
            | Self::Bgra
            | Self::Bgrx
            | Self::Rgba
            | Self::Rgbx
            | Self::Na48
            | Self::Nb48
            | Self::Na4g
            | Self::Nb4g => 0,
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
    /// assert_eq!(PixelFormat::Bgra.line_stride_bytes(1920)?, 7680);
    ///
    /// // UYVY: 2 bytes per pixel
    /// assert_eq!(PixelFormat::Uyvy.line_stride_bytes(1920)?, 3840);
    ///
    /// // NV12: Y-plane has 1 byte per pixel
    /// assert_eq!(PixelFormat::Nv12.line_stride_bytes(1920)?, 1920);
    ///
    /// // P216: Y-plane has 16 bits per pixel
    /// assert_eq!(PixelFormat::P216.line_stride_bytes(1920)?, 3840);
    /// # Ok::<(), grafton_ndi::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if the width is zero or odd, or if the
    /// stride does not fit the SDK's signed 32-bit stride field.
    pub fn line_stride_bytes(self, width: u32) -> Result<usize> {
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
            ndi.table::<sys::Planar444Video>()?;
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
    pub(crate) const ALL: [Self; 4] = [
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
    /// Let the SDK derive display aspect from the frame dimensions.
    pub(crate) const SQUARE_PIXELS: f32 = 0.0;

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
    authorized: SendableVideoLayout,
    header: &VideoHeader,
    data: *const u8,
    metadata: *const c_char,
    timestamp: Option<SdkTimestamp>,
) -> NDIlib_video_frame_v2_t {
    let layout = authorized.layout();
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

/// The parts of a video frame that its shared accessors and owned copies
/// read.
///
/// [`VideoFrame`], a received frame's [`ValidatedFrame`] and its
/// [`VideoFrameRef`], and [`BorrowedVideoFrame`](crate::BorrowedVideoFrame) lend
/// their validated layout, header, pixels and metadata through it, and
/// [`video_accessors!`] generates their public accessors and `Debug` from
/// it, so that each is written and documented once. Each implementation
/// takes every part from one value that was validated as a whole. The trait
/// is crate-private, so these are its only implementations.
///
/// # Safety
///
/// The plane views read through [`pixels`](Self::pixels): it must point to
/// storage that [`layout`](Self::layout) describes, with every active row
/// initialized and unchanged while `self` is borrowed. Row padding need not
/// be initialized. Metadata text must remain a bounded NUL-terminated UTF-8
/// string for the borrow. Any overridden descriptor builder must describe only
/// live storage; an overridden `can_send_direct` must additionally guarantee
/// initialization of the SDK's entire read, including padding and tail room.
pub(crate) unsafe trait VideoParts {
    /// The validated layout of the frame's pixels.
    fn layout(&self) -> &ValidatedVideoLayout;

    /// The validated header.
    fn header(&self) -> &VideoHeader;

    /// The start of the frame's pixel storage.
    fn pixels(&self) -> *const u8;

    /// The validated metadata text, or `None` when the frame has none.
    fn metadata_text(&self) -> Option<MetadataText<'_>>;

    /// The cached receive status, or the presence of validated sending text.
    fn metadata_state(&self) -> FrameMetadataStatus {
        if self.metadata_text().is_some() {
            FrameMetadataStatus::Text
        } else {
            FrameMetadataStatus::Absent
        }
    }

    /// The time the sender submitted the frame, when it is known.
    fn sdk_timestamp(&self) -> Option<SdkTimestamp>;

    /// Whether the SDK's complete synchronous read is known to be initialized.
    /// Capture guarantees only active pixels, never padding or trailing room.
    fn can_send_direct(&self) -> bool {
        self.layout().payload_len_bytes == self.layout().data_len_bytes
            && crate::pixels::send_room(self.layout(), self.header().scan_type) == 0
    }

    /// Borrow this validated frame's SDK descriptor without copying pixels.
    fn video_descriptor(&self, authorized: SendableVideoLayout) -> NDIlib_video_frame_v2_t {
        sdk_video_frame(
            authorized,
            self.header(),
            self.pixels(),
            self.metadata_text()
                .map_or(ptr::null(), MetadataText::as_ptr),
            self.sdk_timestamp(),
        )
    }

    /// One plane, or `None` for an invalid index: the view behind every
    /// generated `plane`.
    #[inline]
    fn plane_view(&self, index: usize) -> Option<VideoPlane<'_>> {
        // SAFETY: the trait's contract: `pixels` points to storage `layout`
        // describes, with every active row initialized and unchanged while
        // `self`, which the view borrows, is borrowed.
        unsafe { self.layout().view_plane(self.pixels(), index) }
    }

    /// Every plane in SDK memory order: the view behind every generated
    /// `planes`.
    #[inline]
    fn plane_views(&self) -> VideoPlanes<'_> {
        // SAFETY: as for `plane_view`.
        unsafe { self.layout().view_planes(self.pixels()) }
    }

    /// Every active byte in SDK plane order, or `None` when padding
    /// separates rows or planes: the view behind a received frame's `data`.
    #[inline]
    fn active_data(&self) -> Option<&[u8]> {
        // SAFETY: as for `plane_view`.
        unsafe { self.layout().view_data(self.pixels()) }
    }

    /// Copy the frame into a new owned frame: one pixel allocation, and one
    /// metadata allocation when there is metadata.
    fn owned_copy(&self) -> VideoFrame {
        let packed = self.layout().packed();
        let metadata = self
            .metadata_text()
            .map(|text| FrameMetadata::copy_from(text, None))
            .unwrap_or_default();
        let room = crate::pixels::send_room(&packed, self.header().scan_type);
        packed_video(
            self,
            packed,
            Pixels::with_room(packed.data_len_bytes, room),
            metadata,
        )
    }

    /// Copy the frame into `destination`, reusing its pixel and metadata
    /// allocations.
    ///
    /// `destination` becomes exactly the frame [`Self::owned_copy`] returns,
    /// whatever it held before. Its pixel buffer is reused when its capacity
    /// holds the packed frame and its scan-dependent SDK
    /// [room](PixelFormat::send_tail_room), and otherwise replaced without
    /// copying its old contents; its metadata string is reused as
    /// [`FrameMetadata::copy_from`] describes.
    ///
    /// Nothing can fail part-way: any allocation happens while `destination`
    /// is still a whole frame, and the only moment its buffer and layout
    /// disagree spans copies into reserved capacity, which neither allocate
    /// nor panic.
    fn copy_into_owned(&self, destination: &mut VideoFrame) {
        let packed = self.layout().packed();
        let len = packed.data_len_bytes;
        let room = crate::pixels::send_room(&packed, self.header().scan_type);
        let replacement =
            (!destination.data.holds(len, room)).then(|| Pixels::with_room(len, room));
        let mut metadata = std::mem::take(&mut destination.metadata);
        match self.metadata_text() {
            Some(text) => metadata.copy_text(text),
            None => metadata.inner.clear(),
        }
        let data = replacement.unwrap_or_else(|| std::mem::take(&mut destination.data));
        *destination = packed_video(self, packed, data, metadata);
    }

    /// Fill reusable storage with packed active rows and a zeroed SDK tail.
    fn copy_active_pixels(&self, data: &mut Pixels, packed: &ValidatedVideoLayout, room: usize) {
        data.fill(packed.data_len_bytes, room, |bytes| {
            for plane in self.plane_views() {
                match plane.data() {
                    Some(active) => bytes.extend_from_slice(active),
                    None => plane.rows().for_each(|row| bytes.extend_from_slice(row)),
                }
            }
        });
    }
}

/// A validated uncompressed video input for synchronous sending and owned copies.
///
/// Implemented by [`VideoFrame`], [`crate::BorrowedVideoFrame`],
/// [`VideoFrameRef`] and [`FrameSyncVideoRef`]. This trait is sealed: applications
/// cannot claim that unchecked storage is a valid frame. It exposes no SDK
/// pointers or allocation ownership. Operations borrow their input and preserve
/// its lifetime and thread restrictions; converting a captured frame is unnecessary.
#[allow(private_bounds)]
pub trait VideoSource: VideoParts {
    /// Frame width in pixels.
    fn width(&self) -> u32 {
        self.layout().width
    }

    /// Frame height in pixels.
    fn height(&self) -> u32 {
        self.layout().height
    }

    /// Validated uncompressed pixel format.
    fn pixel_format(&self) -> PixelFormat {
        self.layout().pixel_format
    }

    /// Frame rate, including its rational representation.
    fn frame_rate(&self) -> FrameRate {
        self.header().frame_rate
    }

    /// Display width divided by height; zero means square pixels.
    fn picture_aspect_ratio(&self) -> f32 {
        self.header().picture_aspect_ratio
    }

    /// Progressive, interlaced, or field scanning.
    fn scan_type(&self) -> ScanType {
        self.header().scan_type
    }

    /// Frame timecode, or the sender's synthesis sentinel.
    fn timecode(&self) -> Timecode {
        self.header().timecode
    }

    /// SDK submission time, when supplied by a captured frame.
    fn timestamp(&self) -> Option<SdkTimestamp> {
        self.sdk_timestamp()
    }

    /// Distance in bytes between first-plane row starts.
    fn line_stride_bytes(&self) -> usize {
        self.layout().line_stride_bytes
    }

    /// UTF-8 frame metadata; an explicit empty string differs from no metadata.
    /// Unreadable received text is withheld; inspect [`Self::metadata_status`].
    fn metadata(&self) -> Option<&str> {
        self.metadata_text().map(MetadataText::as_str)
    }

    /// Whether per-frame metadata is absent, readable, or withheld as unreadable.
    fn metadata_status(&self) -> FrameMetadataStatus {
        self.metadata_state()
    }

    /// Read the frame's `ndi_color_info` description without copying metadata.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] for malformed, truncated or conflicting color
    /// metadata, with the same semantics as [`VideoFrame::color_info`].
    fn color_info(&self) -> Result<Option<crate::ColorInfo>> {
        crate::color_info::parse(self.metadata())
    }

    /// One plane's initialized active rows, excluding row padding.
    fn plane(&self, index: usize) -> Option<VideoPlane<'_>> {
        self.plane_view(index)
    }

    /// All initialized active planes in SDK order, without allocating.
    fn planes(&self) -> VideoPlanes<'_> {
        self.plane_views()
    }

    /// Contiguous active pixels, or `None` when row/plane padding separates them.
    /// Padding and SDK trailing read room are never exposed through this slice.
    fn data(&self) -> Option<&[u8]> {
        self.active_data()
    }

    /// Copy active pixels and metadata into a new, tightly packed owned frame.
    /// Allocates pixel storage and, when present, metadata storage.
    fn to_owned(&self) -> VideoFrame {
        self.owned_copy()
    }

    /// Copy active pixels and metadata into an owned destination, preserving
    /// the header and timestamp. Row padding is omitted. Pixel and metadata
    /// allocations are reused when their capacities/layouts permit; this has
    /// the same allocation behavior as a captured frame's `copy_into`. A copy
    /// without metadata retains the destination's text allocation.
    fn copy_into(&self, destination: &mut VideoFrame) {
        self.copy_into_owned(destination);
    }

    /// Whether synchronous sending can read this frame directly, without a
    /// wrapper pixel copy. The query allocates nothing and makes no SDK call.
    ///
    /// Captured padding is never assumed initialized. Padded captures and
    /// formats requiring trailing SDK room therefore use reusable staging;
    /// owned frames and eligible packed captures send directly. An external
    /// [`crate::BorrowedVideoFrame`] retains its explicit padding/tail policy.
    fn sends_in_place(&self) -> bool {
        self.can_send_direct()
    }
}

/// The owned copy of `source`: `packed`, its packed layout, with `data`,
/// which holds `packed` and its scan-dependent [room](PixelFormat::send_tail_room),
/// filled with every plane's active rows. Every owned video copy is made
/// here. Filling storage that holds the copy neither allocates nor panics.
fn packed_video<P: VideoParts + ?Sized>(
    source: &P,
    packed: ValidatedVideoLayout,
    mut data: Pixels,
    metadata: FrameMetadata,
) -> VideoFrame {
    let room = crate::pixels::send_room(&packed, source.header().scan_type);
    source.copy_active_pixels(&mut data, &packed, room);

    VideoFrame {
        layout: packed,
        header: *source.header(),
        data,
        metadata,
        timestamp: source.sdk_timestamp(),
    }
}

/// Generate a video frame type's shared public accessors and its `Debug`
/// from its [`VideoParts`], so that each accessor is written, and
/// documented, once. Every generated method delegates to the frame's
/// validated parts.
///
/// A frame type passes its `impl` header; its `Debug` name and the layout
/// field that sizes its pixel data; for the accessors whose meaning differs
/// between types, the type-specific paragraph of their docs; and in `images`
/// the code-block options and hidden lines that make a `frame` of its type
/// for the examples of `color_info` and the image encoders, with the
/// type-specific paragraph of the encoders' docs. A frame that can hold
/// received pixels, a capture or an owned frame that may be a copy of one,
/// also passes `timestamp`: it gets a timestamp. A frame built only for
/// sending has none.
macro_rules! video_accessors {
    (
        $(#[$attr:meta])*
        impl[$($generics:tt)*] $frame:ty;
        debug($name:expr, $data:literal = $data_len:ident);
        timecode { $(#[$timecode:meta])* }
        line_stride_bytes { $(#[$line_stride:meta])* }
        plane { $(#[$plane:meta])* }
        timestamp { $(#[$timestamp:meta])* }
        images($fence:literal, $example:literal) { $(#[$images:meta])* }
    ) => {
        $crate::frames::video_accessors! {
            @shared
            $(#[$attr])*
            impl[$($generics)*] $frame;
            debug($name, $data = $data_len, timestamp);
            timecode { $(#[$timecode])* }
            line_stride_bytes { $(#[$line_stride])* }
            plane { $(#[$plane])* }
            images($fence, $example) { $(#[$images])* }
            items {
                /// Get the time the sender submitted this frame.
                ///
                $(#[$timestamp])*
                #[inline]
                pub fn timestamp(&self) -> Option<$crate::SdkTimestamp> {
                    $crate::frames::VideoParts::sdk_timestamp(self)
                }
            }
        }
    };
    (
        $(#[$attr:meta])*
        impl[$($generics:tt)*] $frame:ty;
        debug($name:expr, $data:literal = $data_len:ident);
        timecode { $(#[$timecode:meta])* }
        line_stride_bytes { $(#[$line_stride:meta])* }
        plane { $(#[$plane:meta])* }
        images($fence:literal, $example:literal) { $(#[$images:meta])* }
    ) => {
        $crate::frames::video_accessors! {
            @shared
            $(#[$attr])*
            impl[$($generics)*] $frame;
            debug($name, $data = $data_len);
            timecode { $(#[$timecode])* }
            line_stride_bytes { $(#[$line_stride])* }
            plane { $(#[$plane])* }
            images($fence, $example) { $(#[$images])* }
            items {}
        }
    };
    // The accessors and `Debug` of every video frame, with a frame type's
    // own `items` and the accessors among them that `Debug` prints last.
    (
        @shared
        $(#[$attr:meta])*
        impl[$($generics:tt)*] $frame:ty;
        debug($name:expr, $data:literal = $data_len:ident $(, $extra:ident)*);
        timecode { $(#[$timecode:meta])* }
        line_stride_bytes { $(#[$line_stride:meta])* }
        plane { $(#[$plane:meta])* }
        images($fence:literal, $example:literal) { $(#[$images:meta])* }
        items { $($items:tt)* }
    ) => {
        $(#[$attr])*
        impl<$($generics)*> $frame {
            /// Get the frame width in pixels.
            #[inline]
            pub fn width(&self) -> u32 {
                $crate::frames::VideoParts::layout(self).width()
            }

            /// Get the frame height in pixels.
            #[inline]
            pub fn height(&self) -> u32 {
                $crate::frames::VideoParts::layout(self).height()
            }

            /// Get the pixel format.
            ///
            /// It is a supported uncompressed format, validated when the
            /// frame was constructed.
            #[inline]
            pub fn pixel_format(&self) -> $crate::PixelFormat {
                $crate::frames::VideoParts::layout(self).pixel_format()
            }

            /// Get the frame rate.
            #[inline]
            pub fn frame_rate(&self) -> $crate::FrameRate {
                $crate::frames::VideoParts::header(self).frame_rate
            }

            /// Get the picture aspect ratio, width over height as displayed; `0.0`
            /// means square pixels.
            #[inline]
            pub fn picture_aspect_ratio(&self) -> f32 {
                $crate::frames::VideoParts::header(self).picture_aspect_ratio
            }

            /// Get the scan type: progressive, interlaced, or a single field.
            #[inline]
            pub fn scan_type(&self) -> $crate::ScanType {
                $crate::frames::VideoParts::header(self).scan_type
            }

            /// Get the timecode.
            ///
            $(#[$timecode])*
            #[inline]
            pub fn timecode(&self) -> $crate::Timecode {
                $crate::frames::VideoParts::header(self).timecode
            }

            /// Get the validated first-plane line stride in bytes: the
            /// distance between the starts of consecutive rows of the first
            /// plane.
            ///
            $(#[$line_stride])*
            #[inline]
            pub fn line_stride_bytes(&self) -> usize {
                $crate::frames::VideoParts::layout(self).line_stride_bytes()
            }

            /// Get frame metadata as UTF-8 text, if present.
            ///
            /// Empty metadata, `Some("")`, is distinct from none, `None`.
            /// Unreadable received text returns `None`; use
            /// [`Self::metadata_status`] to distinguish it from absent text.
            #[inline]
            pub fn metadata(&self) -> Option<&str> {
                $crate::frames::VideoParts::metadata_text(self)
                    .map($crate::frames::MetadataText::as_str)
            }

            /// The metadata's presence or the reason unreadable received text
            /// was withheld. Querying the cached status allocates nothing.
            #[inline]
            pub fn metadata_status(&self) -> $crate::FrameMetadataStatus {
                $crate::frames::VideoParts::metadata_state(self)
            }


            /// Read the color description in the frame's `ndi_color_info`
            /// metadata.
            ///
            /// Returns `Ok(None)` when the frame has no metadata or no such
            /// element, which means BT.709 SDR with unknown primaries. See
            /// [`ColorInfo`](crate::ColorInfo) for what the element holds and
            /// how the SDK treats it. Image encoding reads the same
            /// description, so a program can check a frame, for an HDR
            /// transfer for example, before it encodes it.
            ///
            /// # Errors
            ///
            /// [`Error::InvalidFrame`](crate::Error::InvalidFrame), naming the
            /// attribute and its value, when the element is malformed or
            /// truncated, gives a value outside the SDK's list, or appears more
            /// than once with different values.
            ///
            /// # Example
            ///
            #[doc = concat!("```", $fence)]
            #[doc = $example]
            /// match frame.color_info()? {
            ///     Some(color) if color.is_hdr() => println!("HDR: {:?}", color.transfer()),
            ///     Some(color) => println!("SDR, matrix {:?}", color.matrix()),
            ///     None => println!("BT.709 SDR"),
            /// }
            /// # Ok::<(), Box<dyn std::error::Error>>(())
            /// ```
            pub fn color_info(&self) -> $crate::Result<Option<$crate::ColorInfo>> {
                $crate::color_info::parse(self.metadata())
            }

            /// Get one plane in SDK memory order, or `None` for an invalid index.
            ///
            /// Plane order follows the pixel format, as for
            /// [`VideoPlane`](crate::VideoPlane), whose rows exclude any row
            /// padding.
            ///
            $(#[$plane])*
            #[inline]
            pub fn plane(&self, index: usize) -> Option<$crate::VideoPlane<'_>> {
                $crate::frames::VideoParts::plane_view(self, index)
            }

            /// Iterate planes in SDK memory order without allocating or copying.
            #[inline]
            pub fn planes(&self) -> $crate::VideoPlanes<'_> {
                $crate::frames::VideoParts::plane_views(self)
            }

            /// Encode the frame as an image, as `options` describe.
            ///
            /// Every uncompressed pixel format encodes, as PNG, JPEG or
            /// lossless or lossy WebP. YUV is converted to RGB as the NDI SDK converts
            /// it, with BT.709, limited range and repeated chroma, unless the
            /// frame's `ndi_color_info` metadata names another matrix; a frame
            /// tagged PQ or HLG is refused unless the options ask for its code
            /// values. See
            /// [`ImageOptions`](crate::ImageOptions) for formats, color, HDR,
            /// bit depth, alpha, reduction, cost and memory.
            ///
            $(#[$images])*
            ///
            /// Encoding is CPU work, tens to hundreds of milliseconds for a
            /// 1080p frame: async code runs it on a blocking thread with an
            /// owned frame.
            ///
            /// # Errors
            ///
            /// [`Error::InvalidInput`](crate::Error::InvalidInput) for options
            /// that conflict: a JPEG quality outside 1 to 100, a WebP quality
            /// outside 0 to 100, 16 bits or HDR
            /// code values for JPEG or WebP.
            /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) for a frame
            /// the options cannot encode: a PQ or HLG frame under
            /// [`HdrPolicy::Refuse`](crate::HdrPolicy::Refuse), unreadable
            /// `ndi_color_info` under [`ColorPolicy::Sdk`](crate::ColorPolicy::Sdk),
            /// or an image larger than its codec allows; and when the codec
            /// fails.
            ///
            /// # Example
            ///
            #[doc = concat!("```", $fence)]
            #[doc = $example]
            /// use grafton_ndi::{Downscale, ImageFormat, ImageOptions, PngCompression};
            ///
            /// let options = ImageOptions::new(ImageFormat::Png)
            ///     .png_compression(PngCompression::Fast)
            ///     .downscale(Downscale::Half);
            /// let png = frame.encode_image(&options)?;
            /// # Ok::<(), Box<dyn std::error::Error>>(())
            /// ```
            #[cfg(feature = "image-encoding")]
            pub fn encode_image(&self, options: &$crate::ImageOptions) -> $crate::Result<Vec<u8>> {
                $crate::image::encode($crate::image::Source::new(self), options)
            }

            /// Encode the frame as an image, as `options` describe, writing it
            /// into `out`.
            ///
            /// It encodes as [`encode_image`](Self::encode_image) does, into
            /// any writer, so a program that takes snapshots on a timer can
            /// reuse one output buffer. A file or socket should be wrapped in
            /// a [`BufWriter`](std::io::BufWriter): the codecs write in small
            /// pieces. Lossy WebP buffers its complete encoded image before
            /// writing it; other codecs write as they encode.
            ///
            /// # Errors
            ///
            /// As for [`encode_image`](Self::encode_image), and
            /// [`Error::Io`](crate::Error::Io) when `out` fails. Every check
            /// of the options and the frame happens before anything is
            /// written; after a later error, `out` may hold part of an image,
            /// which the caller discards.
            ///
            /// # Example
            ///
            #[doc = concat!("```", $fence)]
            #[doc = $example]
            /// use grafton_ndi::{ImageFormat, ImageOptions};
            ///
            /// let options = ImageOptions::new(ImageFormat::Jpeg(85));
            /// let mut jpeg = Vec::new();
            /// for _ in 0..3 {
            ///     jpeg.clear();
            ///     frame.encode_image_to(&options, &mut jpeg)?;
            ///     // Publish `jpeg`.
            /// }
            /// # Ok::<(), Box<dyn std::error::Error>>(())
            /// ```
            #[cfg(feature = "image-encoding")]
            pub fn encode_image_to<W: ::std::io::Write>(
                &self,
                options: &$crate::ImageOptions,
                mut out: W,
            ) -> $crate::Result<()> {
                $crate::image::encode_to($crate::image::Source::new(self), options, &mut out)
            }

            /// Encode the frame as an image, as `options` describe, in a
            /// base64 data URL for HTML or JSON: `data:image/png;base64,`,
            /// `data:image/jpeg;base64,` or `data:image/webp;base64,` and the
            /// image.
            ///
            /// The string is allocated once, at its final length.
            ///
            /// # Errors
            ///
            /// As for [`encode_image`](Self::encode_image).
            ///
            /// # Example
            ///
            #[doc = concat!("```", $fence)]
            #[doc = $example]
            /// use grafton_ndi::{Downscale, ImageFormat, ImageOptions};
            ///
            /// let options = ImageOptions::new(ImageFormat::Jpeg(80)).downscale(Downscale::Quarter);
            /// let url = frame.encode_image_data_url(&options)?;
            /// println!("<img src=\"{url}\">");
            /// # Ok::<(), Box<dyn std::error::Error>>(())
            /// ```
            #[cfg(feature = "image-encoding")]
            pub fn encode_image_data_url(&self, options: &$crate::ImageOptions) -> $crate::Result<String> {
                $crate::image::encode_data_url($crate::image::Source::new(self), options)
            }

            /// Encode the frame as an 8-bit RGBA PNG at the default
            /// compression level.
            ///
            /// Exactly [`encode_image`](Self::encode_image) with
            /// `ImageOptions::new(ImageFormat::Png)`, and its checks. A snapshot
            /// loop of camera video is far faster with
            /// [`PngCompression::Fast`](crate::PngCompression::Fast) or
            /// [`ImageFormat::WebpLossless`](crate::ImageFormat::WebpLossless).
            ///
            /// # Errors
            ///
            /// As for [`encode_image`](Self::encode_image).
            ///
            /// # Example
            ///
            #[doc = concat!("```", $fence)]
            #[doc = $example]
            /// let png = frame.encode_png()?;
            /// println!("{} bytes of PNG", png.len());
            /// # Ok::<(), Box<dyn std::error::Error>>(())
            /// ```
            #[cfg(feature = "image-encoding")]
            pub fn encode_png(&self) -> $crate::Result<Vec<u8>> {
                self.encode_image(&$crate::ImageOptions::new($crate::ImageFormat::Png))
            }

            /// Encode the frame as a JPEG of `quality`, from 1, the lowest, to
            /// 100, the highest; 80 to 95 are typical.
            ///
            /// Exactly [`encode_image`](Self::encode_image) with
            /// `ImageOptions::new(ImageFormat::Jpeg(quality))`, and its checks.
            ///
            /// # Errors
            ///
            /// As for [`encode_image`](Self::encode_image): a quality outside
            /// 1 to 100 is [`Error::InvalidInput`](crate::Error::InvalidInput).
            ///
            /// # Example
            ///
            #[doc = concat!("```", $fence)]
            #[doc = $example]
            /// let jpeg = frame.encode_jpeg(85)?;
            /// println!("{} bytes of JPEG", jpeg.len());
            /// # Ok::<(), Box<dyn std::error::Error>>(())
            /// ```
            #[cfg(feature = "image-encoding")]
            pub fn encode_jpeg(&self, quality: u8) -> $crate::Result<Vec<u8>> {
                self.encode_image(&$crate::ImageOptions::new($crate::ImageFormat::Jpeg(quality)))
            }

            /// Encode the frame in `format` as a base64 data URL.
            ///
            /// Exactly [`encode_image_data_url`](Self::encode_image_data_url)
            /// with `ImageOptions::new(format)`, and its checks.
            ///
            /// # Errors
            ///
            /// As for [`encode_image`](Self::encode_image).
            ///
            /// # Example
            ///
            #[doc = concat!("```", $fence)]
            #[doc = $example]
            /// use grafton_ndi::ImageFormat;
            ///
            /// let url = frame.encode_data_url(ImageFormat::Jpeg(90))?;
            /// println!("<img src=\"{url}\">");
            /// # Ok::<(), Box<dyn std::error::Error>>(())
            /// ```
            #[cfg(feature = "image-encoding")]
            pub fn encode_data_url(&self, format: $crate::ImageFormat) -> $crate::Result<String> {
                self.encode_image_data_url(&$crate::ImageOptions::new(format))
            }

            $($items)*
        }

        $(#[$attr])*
        impl<$($generics)*> ::std::fmt::Debug for $frame {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.debug_struct($name)
                    .field("width", &self.width())
                    .field("height", &self.height())
                    .field("pixel_format", &self.pixel_format())
                    .field("frame_rate", &self.frame_rate())
                    .field("picture_aspect_ratio", &self.picture_aspect_ratio())
                    .field("scan_type", &self.scan_type())
                    .field("timecode", &self.timecode())
                    .field($data, &$crate::frames::VideoParts::layout(self).$data_len())
                    .field("line_stride_bytes", &self.line_stride_bytes())
                    .field("metadata", &self.metadata())
                    $(.field(stringify!($extra), &self.$extra()))*
                    .finish()
            }
        }
    };
}
pub(crate) use video_accessors;

/// An owned video frame with validated dimensions, format, stride, and buffer size.
///
/// Owned frames contain tightly packed active pixels: copies of borrowed or
/// received frames omit their row padding. Use [`Self::builder`] to create
/// one for sending. Cloning copies the pixels and metadata.
///
/// The allocation that holds the pixels also holds the
/// [room](PixelFormat::send_tail_room) the SDK needs after a frame of this
/// layout and scan type, so an owned frame is sent in place. Construction,
/// copies, pixel replacement and scan-type changes preserve this invariant;
/// sending requires no pixel copy or allocation growth.
#[derive(Clone)]
pub struct VideoFrame {
    layout: ValidatedVideoLayout,
    header: VideoHeader,
    /// The pixels, followed by the room required by the layout and scan type.
    data: Pixels,
    metadata: FrameMetadata,
    timestamp: Option<SdkTimestamp>,
}

// SAFETY: Pixels owns and initializes the validated layout and scan-dependent
// SDK tail. FrameMetadata owns bounded terminated text. Immutable borrows keep
// both allocations unchanged; can_send_direct includes all SDK-readable room.
unsafe impl VideoParts for VideoFrame {
    #[inline]
    fn layout(&self) -> &ValidatedVideoLayout {
        &self.layout
    }
    #[inline]
    fn header(&self) -> &VideoHeader {
        &self.header
    }
    #[inline]
    fn pixels(&self) -> *const u8 {
        self.data.as_ptr()
    }
    #[inline]
    fn metadata_text(&self) -> Option<MetadataText<'_>> {
        self.metadata.optional_text()
    }
    #[inline]
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
        self.timestamp
    }

    fn can_send_direct(&self) -> bool {
        // Every construction and mutation preserves the complete initialized
        // SDK read extent, including the current scan type's extra rows.
        true
    }
}

impl VideoSource for VideoFrame {}

video_accessors! {
    impl[] VideoFrame;
    debug("VideoFrame", "data (bytes)" = data_len_bytes);
    timecode {
        /// A built frame starts with [`Timecode::SYNTHESIZE`]; a copy of a
        /// received frame keeps the timecode it arrived with.
    }
    line_stride_bytes {
        /// An owned frame's rows are tightly packed, so this is the packed
        /// stride.
    }
    plane {
        /// The rows of an owned frame have no padding.
    }
    timestamp {
        /// A frame has one only when it was copied from a received frame
        /// that carries one; frames built for sending have none.
    }
    images("", "\
 # use grafton_ndi::{PixelFormat, VideoFrame};
 # let frame = VideoFrame::builder().resolution(64, 36).pixel_format(PixelFormat::Uyvy).build()?;") {}
}

impl VideoFrame {
    /// Copy this frame and its metadata into independent owned storage.
    #[must_use]
    pub fn to_owned(&self) -> Self {
        self.clone()
    }

    /// Create a builder for a frame to send.
    ///
    /// Its defaults are 1920x1080 BGRA at 60/1 frames per second, square
    /// pixels (picture aspect ratio `0.0`), progressive scan, [`Timecode::SYNTHESIZE`], no
    /// metadata and zero-filled pixels.
    pub fn builder() -> VideoFrameBuilder {
        VideoFrameBuilder {
            width: 1920,
            height: 1080,
            pixel_format: PixelFormat::Bgra,
            frame_rate: crate::media::DEFAULT_VIDEO_FRAME_RATE,
            picture_aspect_ratio: VideoHeader::SQUARE_PIXELS,
            scan_type: ScanType::Progressive,
            timecode: Timecode::SYNTHESIZE,
            metadata: None,
            data: None,
        }
    }

    /// Backing allocations retained when a frame enters a bounded send queue.
    #[cfg(feature = "advanced")]
    pub(crate) fn retained_bytes(&self) -> usize {
        self.data
            .capacity()
            .saturating_add(self.metadata.retained_bytes())
    }

    /// The SDK descriptor of this frame, borrowing its pixels and metadata.
    pub(crate) fn to_raw(&self, authorized: SendableVideoLayout) -> NDIlib_video_frame_v2_t {
        self.video_descriptor(authorized)
    }

    #[cfg(test)]
    pub(crate) fn as_test_raw(&self) -> NDIlib_video_frame_v2_t {
        self.to_raw(SendableVideoLayout::for_test(*self.layout()))
    }

    /// Set the timecode without reallocating the pixels.
    pub fn set_timecode(&mut self, timecode: Timecode) {
        self.header.timecode = timecode;
    }

    /// Set the scan type without changing the frame's validated layout.
    ///
    /// Grows the pixel allocation if this scan type needs more SDK tail
    /// room. Existing capacity is reused and never shrunk, so switching
    /// among scan types allocates at most once after construction.
    pub fn set_scan_type(&mut self, scan_type: ScanType) {
        self.data
            .reserve_room(crate::pixels::send_room(&self.layout, scan_type));
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

    /// Get the frame data: every plane, tightly packed, in SDK plane order.
    pub fn data(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// Recover the original pixel vector, retaining its allocation and capacity.
    ///
    /// The length covers pixels only; initialized SDK tail bytes remain in
    /// spare capacity and will be restored if the vector is built into a frame.
    pub fn into_data(self) -> Vec<u8> {
        self.data.into_vec()
    }

    /// Replace pixels and recover the previous allocation for reuse.
    ///
    /// The incoming vector follows the allocation rules of [`Self::replace_data`].
    ///
    /// # Errors
    ///
    /// Returns `data` unchanged with [`Error::InvalidFrame`] when its length
    /// differs from the validated layout. The frame is unchanged.
    pub fn swap_data(&mut self, data: Vec<u8>) -> std::result::Result<Vec<u8>, Rejected<Vec<u8>>> {
        if let Err(error) = check_video_data_len(&self.layout, data.len()) {
            return Err(Rejected::new(error, data));
        }
        let replacement = Pixels::adopt(
            data,
            crate::pixels::send_room(&self.layout, self.header.scan_type),
        );
        Ok(std::mem::replace(&mut self.data, replacement).into_vec())
    }

    /// Get mutable access to the frame data without changing the validated
    /// layout.
    pub fn data_mut(&mut self) -> &mut [u8] {
        self.data.as_mut_slice()
    }

    /// The zero bytes that follow the pixels in the frame's allocation for
    /// the SDK to read, including any scan-dependent extra rows.
    #[cfg(test)]
    pub(crate) fn tail_room(&self) -> usize {
        self.data.tail_room()
    }

    /// Replace the owned frame data while preserving the validated layout.
    ///
    /// The frame keeps `data`'s allocation when its spare capacity holds the
    /// layout and scan type's [room](PixelFormat::send_tail_room), which is
    /// nothing for most formats, and otherwise grows it once, which may move
    /// the pixels.
    ///
    /// # Errors
    ///
    /// Returns `data`, unused, with [`Error::InvalidFrame`] if it is not
    /// exactly the validated layout size. The frame is unchanged.
    pub fn replace_data(&mut self, data: Vec<u8>) -> std::result::Result<(), Rejected<Vec<u8>>> {
        self.swap_data(data).map(drop)
    }

    #[cfg(test)]
    pub(crate) fn metadata_cstr(&self) -> Option<&CStr> {
        self.metadata.optional_text().map(|metadata| {
            CStr::from_bytes_with_nul(metadata.with_nul.as_bytes())
                .expect("owned metadata ends in its only NUL byte")
        })
    }

    /// Replace frame metadata. Use [`Self::clear_metadata`] to remove it.
    ///
    /// Invalid replacement text leaves the existing metadata unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] if `metadata` contains an interior NUL
    /// byte, or [`Error::InvalidFrame`] if the emitted C string would exceed
    /// the metadata size cap.
    pub fn set_metadata(&mut self, metadata: impl Into<String>) -> Result<()> {
        self.metadata = FrameMetadata::new(metadata)?;
        Ok(())
    }

    /// Remove frame metadata, retaining its allocation for later copies.
    pub fn clear_metadata(&mut self) {
        self.metadata.inner.clear();
    }

    /// # Safety
    /// `c_frame` must describe one live allocation covering its SDK plane
    /// layout, with every active row initialized and immutable through this
    /// copy, and null or SDK-readable metadata.
    #[cfg(test)]
    pub(crate) unsafe fn from_raw(c_frame: &NDIlib_video_frame_v2_t) -> Result<VideoFrame> {
        // SAFETY: forwarded to the caller; the borrow holds the descriptor.
        let frame = unsafe { ValidatedVideo::video(c_frame)? };
        Ok(frame.owned_copy())
    }
}

#[cfg(all(test, feature = "image-encoding"))]
impl VideoFrame {
    /// A frame over `data` laid out with `line_stride`, which may pad its
    /// rows, as received frames' strides do: owned frames are otherwise
    /// packed.
    pub(crate) fn with_line_stride(
        pixel_format: PixelFormat,
        width: u32,
        height: u32,
        line_stride: usize,
        data: Vec<u8>,
        metadata: Option<&str>,
    ) -> Self {
        let layout =
            ValidatedVideoLayout::new_uncompressed(pixel_format, width, height, Some(line_stride))
                .unwrap();
        assert_eq!(data.len(), layout.data_len_bytes);
        Self {
            layout,
            header: VideoHeader {
                frame_rate: FrameRate::new(60, 1).unwrap(),
                picture_aspect_ratio: VideoHeader::SQUARE_PIXELS,
                scan_type: ScanType::Progressive,
                timecode: Timecode::from_ticks(0),
            },
            data: Pixels::adopt(
                data,
                crate::pixels::send_room(&layout, ScanType::Progressive),
            ),
            metadata: metadata
                .map(|text| FrameMetadata::new(text).unwrap())
                .unwrap_or_default(),
            timestamp: None,
        }
    }

    /// The frame as a captured reference to its own pixels, as a FrameSync
    /// capture would lend it; dropping it frees nothing.
    pub(crate) fn as_test_capture(&self) -> crate::FrameSyncVideoRef<'_> {
        // SAFETY: the descriptor points into `self`, which the reference
        // borrows, and the no-op free releases nothing it does not own.
        unsafe {
            VideoRef::new(Guard::<crate::capture::FrameSyncVideoFree>::with_test_free(
                ptr::null_mut(),
                self.as_test_raw(),
                |_, _| {},
            ))
        }
        .unwrap()
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
    /// An owned string retains its allocation when there is capacity for
    /// the trailing NUL; invalid metadata leaves a rejected builder unchanged.
    #[must_use]
    pub fn metadata<S: Into<String>>(mut self, metadata: S) -> Self {
        self.metadata = Some(metadata.into());
        self
    }

    /// Supply owned pixels without an intermediate zero-filled allocation.
    ///
    /// `build` requires exactly the validated layout size. The frame keeps
    /// this allocation when its spare capacity holds the layout and scan
    /// type's [room](PixelFormat::send_tail_room), which is nothing for most
    /// formats, and otherwise grows it once, which may move the pixels. Use
    /// `data_mut` on the resulting frame for reuse.
    #[must_use]
    pub fn data(mut self, data: Vec<u8>) -> Self {
        self.data = Some(data);
        self
    }

    /// The caller-supplied pixels, before any validation or transformation.
    pub fn supplied_data(&self) -> Option<&[u8]> {
        self.data.as_deref()
    }

    /// Take supplied data while retaining the builder's other settings.
    ///
    /// This permits repairing a refused buffer and putting it back with
    /// [`Self::data`]. Until then, building uses the zero-filled default.
    pub fn take_data(&mut self) -> Option<Vec<u8>> {
        self.data.take()
    }

    /// Recover the caller-supplied vector without building a frame.
    ///
    /// Its allocation, capacity and bytes are unchanged, including after a
    /// failed [`Self::build`]. Returns `None` for zero-filled default input.
    pub fn into_data(self) -> Option<Vec<u8>> {
        self.data
    }

    /// Use zero-filled default data instead of a supplied buffer.
    ///
    /// Recover the buffer first with [`Self::take_data`] when it is needed again.
    #[must_use]
    pub fn clear_data(mut self) -> Self {
        self.data = None;
        self
    }

    /// Remove metadata, for example to repair a rejected builder.
    #[must_use]
    pub fn clear_metadata(mut self) -> Self {
        self.metadata = None;
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
    /// Returns the unchanged builder with [`Error::InvalidFrame`] if the
    /// resolution, pixel format, aspect ratio, metadata size or supplied data
    /// length do not form a
    /// valid frame, and [`Error::InvalidCString`] for metadata containing a
    /// NUL byte.
    pub fn build(mut self) -> std::result::Result<VideoFrame, Rejected<Self>> {
        let mut validate = || -> Result<_> {
            validate_picture_aspect_ratio(self.picture_aspect_ratio)?;
            let layout = ValidatedVideoLayout::new_uncompressed(
                self.pixel_format,
                self.width,
                self.height,
                None,
            )?;
            if let Some(data) = &self.data {
                check_video_data_len(&layout, data.len())?;
            }
            // The last fallible check transfers metadata only on success,
            // preserving the complete builder on every rejected input.
            let metadata = FrameMetadata::take_from(&mut self.metadata)?.unwrap_or_default();
            Ok((layout, metadata))
        };
        let (layout, metadata) = match validate() {
            Ok(parts) => parts,
            Err(error) => return Err(Rejected::new(error, self)),
        };
        let room = crate::pixels::send_room(&layout, self.scan_type);
        let data = match self.data {
            Some(data) => Pixels::adopt(data, room),
            None => Pixels::zeroed(layout.data_len_bytes, room),
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

/// The parts of an audio frame that its shared accessors and owned copies
/// read.
///
/// [`AudioFrame`], and a received frame's [`ValidatedFrame`] and its
/// [`AudioFrameRef`], lend their validated layout, timing, samples and metadata
/// through it, and [`audio_accessors!`] generates their public accessors and
/// `Debug` from it, so that each is written and documented once. Each
/// implementation takes every part from one value that was validated as a
/// whole. The trait is crate-private, so these are its only
/// implementations.
///
/// # Safety
///
/// The channel views read through [`samples`](Self::samples): it must point
/// to `f32`-aligned storage that [`layout`](Self::layout) describes, with
/// every channel's samples initialized and unchanged while `self` is
/// borrowed. Padding between channels need not be initialized. Metadata text
/// must remain a bounded NUL-terminated UTF-8 string for the borrow. Overridden
/// descriptor builders must describe no more readable initialized samples or
/// metadata than the implementation keeps alive.
pub(crate) unsafe trait AudioParts {
    /// The validated layout of the frame's samples.
    fn layout(&self) -> &ValidatedAudioLayout;

    /// The frame's timecode.
    fn audio_timecode(&self) -> Timecode;

    /// The time the sender submitted the frame, when it is known.
    fn sdk_timestamp(&self) -> Option<SdkTimestamp>;

    /// The start of the frame's sample storage.
    fn samples(&self) -> *const f32;

    /// The validated metadata text, or `None` when the frame has none.
    fn metadata_text(&self) -> Option<MetadataText<'_>>;

    /// The cached receive status, or the presence of validated sending text.
    fn metadata_state(&self) -> FrameMetadataStatus {
        if self.metadata_text().is_some() {
            FrameMetadataStatus::Text
        } else {
            FrameMetadataStatus::Absent
        }
    }

    /// Borrow the original validated channel stride, samples and metadata.
    fn audio_descriptor(&self) -> NDIlib_audio_frame_v3_t {
        let layout = self.layout();
        NDIlib_audio_frame_v3_t {
            sample_rate: layout.sdk_sample_rate(),
            no_channels: layout.sdk_num_channels(),
            no_samples: layout.sdk_num_samples(),
            timecode: self.audio_timecode().ticks(),
            FourCC: AudioFormat::Fltp.to_sdk(),
            p_data: self.samples().cast_mut().cast(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: layout.sdk_channel_stride(),
            },
            p_metadata: self
                .metadata_text()
                .map_or(ptr::null(), MetadataText::as_ptr),
            timestamp: SdkTimestamp::to_sdk(self.sdk_timestamp()),
        }
    }

    /// One channel's samples, or `None` for an invalid channel: the view
    /// behind every generated `channel_data`.
    #[inline]
    fn channel_view(&self, channel: usize) -> Option<&[f32]> {
        // SAFETY: the trait's contract: `samples` points to aligned storage
        // `layout` describes, with every channel's samples initialized and
        // unchanged while `self`, which the slice borrows, is borrowed.
        unsafe { self.layout().channel(self.samples(), channel) }
    }

    /// Every channel's samples, in order: the view behind every generated
    /// `channels`.
    #[inline]
    fn channel_views(&self) -> AudioChannels<'_> {
        // SAFETY: as for `channel_view`.
        unsafe { self.layout().channels(self.samples()) }
    }

    /// Every sample, channels back to back, or `None` when padding separates
    /// channels: the view behind a received frame's `data`.
    #[inline]
    fn contiguous_samples(&self) -> Option<&[f32]> {
        // SAFETY: as for `channel_view`.
        unsafe { self.layout().view_samples(self.samples()) }
    }

    /// Copy the frame into a new owned frame: one sample allocation, and one
    /// metadata allocation when there is metadata.
    fn owned_copy(&self) -> AudioFrame {
        let packed = self.layout().packed();
        let metadata = self
            .metadata_text()
            .map(|text| FrameMetadata::copy_from(text, None))
            .unwrap_or_default();
        packed_audio(
            self,
            packed,
            Vec::with_capacity(packed.sample_count),
            metadata,
        )
    }

    /// Copy the frame into `destination`, reusing its sample and metadata
    /// allocations.
    ///
    /// `destination` becomes exactly the frame [`Self::owned_copy`] returns,
    /// whatever it held before. Its sample buffer is reused when its capacity
    /// holds the packed samples, and otherwise replaced without copying its
    /// old contents; its metadata string is reused as
    /// [`FrameMetadata::copy_from`] describes. As for video, any allocation
    /// happens while `destination` is still a whole frame.
    fn copy_into_owned(&self, destination: &mut AudioFrame) {
        let packed = self.layout().packed();
        let len = packed.sample_count();
        let replacement = (destination.data.capacity() < len).then(|| Vec::with_capacity(len));
        let mut metadata = std::mem::take(&mut destination.metadata);
        match self.metadata_text() {
            Some(text) => metadata.copy_text(text),
            None => metadata.inner.clear(),
        }
        let data = replacement.unwrap_or_else(|| std::mem::take(&mut destination.data));
        *destination = packed_audio(self, packed, data, metadata);
    }
}

/// A validated planar audio input for synchronous sending and conversion.
///
/// Implemented by [`AudioFrame`], [`BorrowedAudioFrame`], [`AudioFrameRef`] and
/// [`FrameSyncAudioRef`]. Generic and `dyn AudioSource` consumers can inspect
/// validated layouts and channel samples without allocating.
/// This sealed trait prevents unchecked sample pointers or channel layouts.
/// Native operations borrow the original samples and channel stride directly,
/// with no intermediate owned planar copy or wrapper allocation. The input's
/// lifetime and thread restrictions still apply.
#[allow(private_bounds)]
pub trait AudioSource: AudioParts {
    /// Sample rate in Hz.
    fn sample_rate(&self) -> u32 {
        self.layout().sample_rate
    }

    /// Number of channels.
    fn num_channels(&self) -> u32 {
        self.layout().num_channels()
    }

    /// Number of active samples per channel.
    fn num_samples(&self) -> u32 {
        self.layout().num_samples()
    }

    /// Validated planar floating-point audio format.
    fn format(&self) -> AudioFormat {
        self.layout().format
    }

    /// Distance in bytes between consecutive channel starts.
    fn channel_stride_bytes(&self) -> usize {
        self.layout().channel_stride_bytes()
    }

    /// Frame timecode, or the sender's synthesis sentinel.
    fn timecode(&self) -> Timecode {
        self.audio_timecode()
    }

    /// SDK submission time, when supplied by a captured frame.
    fn timestamp(&self) -> Option<SdkTimestamp> {
        self.sdk_timestamp()
    }

    /// UTF-8 metadata; an explicit empty string differs from no metadata.
    /// Unreadable received text is withheld; inspect [`Self::metadata_status`].
    fn metadata(&self) -> Option<&str> {
        self.metadata_text().map(MetadataText::as_str)
    }

    /// Whether per-frame metadata is absent, readable, or withheld as unreadable.
    fn metadata_status(&self) -> FrameMetadataStatus {
        self.metadata_state()
    }

    /// One channel's initialized samples, excluding any inter-channel padding.
    fn channel_data(&self, channel: usize) -> Option<&[f32]> {
        self.channel_view(channel)
    }

    /// Iterate initialized channel samples without allocating or reading padding.
    fn channels(&self) -> AudioChannels<'_> {
        self.channel_views()
    }

    /// Contiguous samples, or `None` when padding separates channels.
    fn data(&self) -> Option<&[f32]> {
        self.contiguous_samples()
    }

    /// Copy samples and metadata into a new owned, tightly packed planar frame.
    /// Allocates sample storage and, when present, metadata storage.
    fn to_owned(&self) -> AudioFrame {
        self.owned_copy()
    }

    /// Copy into an owned destination, reusing sample and metadata allocations
    /// when their capacities permit. Padding is omitted. Absent source metadata
    /// clears the destination text while retaining its allocation.
    fn copy_into(&self, destination: &mut AudioFrame) {
        self.copy_into_owned(destination);
    }
}

/// Validated planar floating-point audio borrowed from application storage.
///
/// Samples and optional NUL-terminated metadata are borrowed for the frame's
/// lifetime. Synchronous sending and [`crate::audio_conversion::AudioConverter`]
/// use the original channel stride without copying samples or allocating.
/// Inspect channels with [`Self::channel_data`] or [`Self::channels`]; those
/// slices exclude inter-channel padding.
#[derive(Clone, Copy)]
pub struct BorrowedAudioFrame<'buf> {
    layout: ValidatedAudioLayout,
    timecode: Timecode,
    data: &'buf [f32],
    metadata: Option<MetadataText<'buf>>,
}

// SAFETY: Construction checks the f32 slice covers the validated layout, with
// initialized samples. Its borrow keeps the slice and validated terminated
// metadata alive and unchanged; the default descriptor uses only those parts.
unsafe impl AudioParts for BorrowedAudioFrame<'_> {
    fn layout(&self) -> &ValidatedAudioLayout {
        &self.layout
    }
    fn audio_timecode(&self) -> Timecode {
        self.timecode
    }
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
        None
    }
    fn samples(&self) -> *const f32 {
        self.data.as_ptr()
    }
    fn metadata_text(&self) -> Option<MetadataText<'_>> {
        self.metadata
    }
}

impl AudioSource for BorrowedAudioFrame<'_> {}

impl<'buf> BorrowedAudioFrame<'buf> {
    /// Configure a borrowed planar frame with explicit sample rate and shape.
    ///
    /// Defaults to tightly packed channels, [`Timecode::SYNTHESIZE`] and no
    /// metadata. A larger input slice is permitted; only the checked frame
    /// extent is retained. All validation occurs in the builder's `build`.
    ///
    /// ```
    /// use grafton_ndi::{AudioSource, BorrowedAudioFrame};
    /// let samples = [1.0, 2.0, 99.0, 3.0, 4.0];
    /// let frame = BorrowedAudioFrame::builder(&samples, 48_000, 2, 2)
    ///     .channel_stride_bytes(12)
    ///     .build()?;
    /// let source: &dyn AudioSource = &frame;
    /// assert_eq!(source.channels().collect::<Vec<_>>(), [&[1.0, 2.0][..], &[3.0, 4.0][..]]);
    /// assert!(source.data().is_none());
    /// # Ok::<(), grafton_ndi::Error>(())
    /// ```
    pub fn builder(
        data: &'buf [f32],
        sample_rate: u32,
        num_channels: u32,
        num_samples: u32,
    ) -> BorrowedAudioFrameBuilder<'buf> {
        BorrowedAudioFrameBuilder {
            data,
            sample_rate,
            num_channels,
            num_samples,
            channel_stride_bytes: None,
            timecode: Timecode::SYNTHESIZE,
            metadata: None,
        }
    }

    /// Contiguous active samples, or `None` when padding separates channels.
    pub fn data(&self) -> Option<&[f32]> {
        self.contiguous_samples()
    }

    /// Copy initialized samples and metadata into a new tightly packed frame.
    /// The returned frame retains no borrow; padding is omitted.
    pub fn to_owned(&self) -> AudioFrame {
        self.owned_copy()
    }

    /// Copy into an existing packed frame, reusing its sample and metadata
    /// allocations when their capacity and lengths permit. A copy without
    /// metadata retains the destination's text allocation.
    pub fn copy_into(&self, destination: &mut AudioFrame) {
        self.copy_into_owned(destination);
    }

    /// Return this frame with the supplied timecode; no samples are copied.
    #[must_use]
    pub fn with_timecode(mut self, timecode: Timecode) -> Self {
        self.timecode = timecode;
        self
    }
}

/// Builder for [`BorrowedAudioFrame`]; no sample or metadata copies are made.
#[derive(Clone, Copy, Debug)]
pub struct BorrowedAudioFrameBuilder<'buf> {
    data: &'buf [f32],
    sample_rate: u32,
    num_channels: u32,
    num_samples: u32,
    channel_stride_bytes: Option<usize>,
    timecode: Timecode,
    metadata: Option<&'buf CStr>,
}

impl<'buf> BorrowedAudioFrameBuilder<'buf> {
    /// Set the distance in bytes between consecutive channel starts.
    /// Must be a positive multiple of `size_of::<f32>()`, at least one channel's
    /// samples, and fit the SDK's signed 32-bit stride field.
    #[must_use]
    pub fn channel_stride_bytes(mut self, bytes: usize) -> Self {
        self.channel_stride_bytes = Some(bytes);
        self
    }

    /// Reset the stride override to tightly packed channels.
    #[must_use]
    pub fn clear_channel_stride_bytes(mut self) -> Self {
        self.channel_stride_bytes = None;
        self
    }

    /// Set the frame timecode; defaults to [`Timecode::SYNTHESIZE`].
    #[must_use]
    pub fn timecode(mut self, timecode: Timecode) -> Self {
        self.timecode = timecode;
        self
    }

    /// Borrow NUL-terminated UTF-8 metadata, validated when built.
    /// An explicit empty C string differs from absent metadata.
    #[must_use]
    pub fn metadata(mut self, metadata: &'buf CStr) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Remove the optional metadata override.
    #[must_use]
    pub fn clear_metadata(mut self) -> Self {
        self.metadata = None;
        self
    }

    /// Validate the rate, positive channel/sample counts, stride, sample extent
    /// and optional UTF-8 metadata without allocating sample storage.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] for an invalid layout, insufficient storage,
    /// exceeding the 64 MiB audio limit or the 4 MiB metadata cap;
    /// [`Error::InvalidUtf8`] for metadata that is not UTF-8.
    pub fn build(self) -> Result<BorrowedAudioFrame<'buf>> {
        let packed = validate_outbound_audio_layout(
            self.sample_rate,
            self.num_channels,
            self.num_samples,
            AudioFormat::Fltp,
        )?;
        let stride = self
            .channel_stride_bytes
            .unwrap_or(packed.channel_stride_bytes());
        let stride = i32::try_from(stride)
            .map_err(|_| Error::InvalidFrame("Audio channel stride exceeds i32 range".into()))?;
        // Layout validation checks fields only; it never dereferences samples.
        // The slice supplies alignment, and the checked extent below establishes
        // that every active channel is initialized and borrowed for 'buf.
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: packed.sdk_sample_rate(),
            no_channels: packed.sdk_num_channels(),
            no_samples: packed.sdk_num_samples(),
            timecode: self.timecode.ticks(),
            FourCC: AudioFormat::Fltp.to_sdk(),
            p_data: self.data.as_ptr().cast_mut().cast(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: stride,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };
        let layout = validate_audio_layout(&raw)?;
        let data = self.data.get(..layout.sample_count).ok_or_else(|| {
            Error::InvalidFrame(format!(
                "Audio buffer has {} samples, requires at least {}",
                self.data.len(),
                layout.sample_count,
            ))
        })?;
        let metadata = self.metadata.map(MetadataText::from_cstr).transpose()?;
        Ok(BorrowedAudioFrame {
            layout,
            timecode: self.timecode,
            data,
            metadata,
        })
    }
}

/// The owned copy of `source`: `packed`, its packed layout, with `data`,
/// whose capacity holds `packed`, filled with the channels back to back.
/// Every owned audio copy is made here. Filling reserved capacity neither
/// allocates nor panics.
fn packed_audio<P: AudioParts + ?Sized>(
    source: &P,
    packed: ValidatedAudioLayout,
    mut data: Vec<f32>,
    metadata: FrameMetadata,
) -> AudioFrame {
    data.clear();
    // The views read channel samples only, never padding between channels.
    for samples in source.channel_views() {
        data.extend_from_slice(samples);
    }

    AudioFrame {
        layout: packed,
        timecode: source.audio_timecode(),
        data,
        metadata,
        timestamp: source.sdk_timestamp(),
    }
}

/// Generate an audio frame type's shared public accessors and its `Debug`
/// from its [`AudioParts`], so that each accessor is written, and
/// documented, once. Every generated method delegates to the frame's
/// validated parts.
///
/// A frame type passes its `impl` header, its `Debug` name and, for the
/// accessors whose meaning differs between types, the type-specific
/// paragraph of their docs.
macro_rules! audio_accessors {
    (
        $(#[$attr:meta])*
        impl[$($generics:tt)*] $frame:ty;
        debug($name:expr);
        timecode { $(#[$timecode:meta])* }
        timestamp { $(#[$timestamp:meta])* }
        channel_stride_bytes { $(#[$channel_stride:meta])* }
        channel_data { $(#[$channel_data:meta])* }
    ) => {
        $(#[$attr])*
        impl<$($generics)*> $frame {
            /// Get the sample rate in Hz.
            #[inline]
            pub fn sample_rate(&self) -> u32 {
                AudioParts::layout(self).sample_rate()
            }

            /// Get the number of audio channels.
            #[inline]
            pub fn num_channels(&self) -> u32 {
                AudioParts::layout(self).num_channels()
            }

            /// Get the number of samples per channel.
            #[inline]
            pub fn num_samples(&self) -> u32 {
                AudioParts::layout(self).num_samples()
            }

            /// Get the timecode.
            ///
            $(#[$timecode])*
            #[inline]
            pub fn timecode(&self) -> Timecode {
                AudioParts::audio_timecode(self)
            }

            /// Get the time the sender submitted this frame.
            ///
            $(#[$timestamp])*
            #[inline]
            pub fn timestamp(&self) -> Option<SdkTimestamp> {
                AudioParts::sdk_timestamp(self)
            }

            /// Get the audio format.
            ///
            /// It is a supported format, validated when the frame was
            /// constructed.
            #[inline]
            pub fn format(&self) -> AudioFormat {
                AudioParts::layout(self).format()
            }

            /// Get the distance between the starts of consecutive channels in
            /// bytes.
            ///
            $(#[$channel_stride])*
            #[inline]
            pub fn channel_stride_bytes(&self) -> usize {
                AudioParts::layout(self).channel_stride_bytes()
            }

            /// Get frame metadata as UTF-8 text, if present.
            ///
            /// Empty metadata, `Some("")`, is distinct from none, `None`.
            /// Unreadable received text returns `None`; use
            /// [`Self::metadata_status`] to distinguish it from absent text.
            #[inline]
            pub fn metadata(&self) -> Option<&str> {
                AudioParts::metadata_text(self).map(MetadataText::as_str)
            }

            /// The metadata's presence or the reason unreadable received text
            /// was withheld. Querying the cached status allocates nothing.
            #[inline]
            pub fn metadata_status(&self) -> crate::FrameMetadataStatus {
                AudioParts::metadata_state(self)
            }


            /// Get one channel's samples, or `None` for an invalid channel.
            ///
            /// The slice borrows the frame's samples without copying and
            /// excludes any padding between channels.
            ///
            $(#[$channel_data])*
            #[inline]
            pub fn channel_data(&self, channel: usize) -> Option<&[f32]> {
                AudioParts::channel_view(self, channel)
            }

            /// Iterate over the channels' samples, in channel order, without
            /// copying or reading padding between channels.
            #[inline]
            pub fn channels(&self) -> AudioChannels<'_> {
                AudioParts::channel_views(self)
            }
        }

        $(#[$attr])*
        impl<$($generics)*> ::std::fmt::Debug for $frame {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.debug_struct($name)
                    .field("sample_rate", &self.sample_rate())
                    .field("num_channels", &self.num_channels())
                    .field("num_samples", &self.num_samples())
                    .field("timecode", &self.timecode())
                    .field("format", &self.format())
                    .field("samples per channel", &AudioParts::layout(self).no_samples())
                    .field("channel_stride_bytes", &self.channel_stride_bytes())
                    .field("metadata", &self.metadata())
                    .field("timestamp", &self.timestamp())
                    .finish()
            }
        }
    };
}

audio_accessors! {
    impl['buf] BorrowedAudioFrame<'buf>;
    debug("BorrowedAudioFrame");
    timecode {}
    timestamp {
        /// Application input has no SDK submission timestamp.
    }
    channel_stride_bytes {
        /// Includes any configured inter-channel padding.
    }
    channel_data {
        /// Borrows the caller's active samples without reading padding.
    }
}

/// Owned, tightly packed planar floating-point audio with a validated layout.
///
/// Channel samples are contiguous, with channels stored one after another.
/// Conversion from borrowed SDK audio omits inter-channel padding. Cloning
/// copies the samples and metadata.
#[derive(Clone)]
pub struct AudioFrame {
    layout: ValidatedAudioLayout,
    timecode: Timecode,
    data: Vec<f32>,
    metadata: FrameMetadata,
    timestamp: Option<SdkTimestamp>,
}

// SAFETY: The owned f32 vector covers and initializes the packed layout;
// FrameMetadata owns bounded terminated text. Both stay unchanged for the
// borrow, and the default descriptor addresses only those live allocations.
unsafe impl AudioParts for AudioFrame {
    #[inline]
    fn layout(&self) -> &ValidatedAudioLayout {
        &self.layout
    }
    #[inline]
    fn audio_timecode(&self) -> Timecode {
        self.timecode
    }
    #[inline]
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
        self.timestamp
    }
    #[inline]
    fn samples(&self) -> *const f32 {
        self.data.as_ptr()
    }
    #[inline]
    fn metadata_text(&self) -> Option<MetadataText<'_>> {
        self.metadata.optional_text()
    }
}

impl AudioSource for AudioFrame {}

audio_accessors! {
    impl[] AudioFrame;
    debug("AudioFrame");
    timecode {
        /// A built frame starts with [`Timecode::SYNTHESIZE`]; a copy of a
        /// received frame keeps the timecode it arrived with.
    }
    timestamp {
        /// A frame has one only when it was copied from a received frame
        /// that carries one; frames built for sending have none.
    }
    channel_stride_bytes {
        /// It is one channel's samples, since an owned frame has no padding.
    }
    channel_data {
        /// Data is always stored in planar format internally. If
        /// `AudioLayout::Interleaved` was specified at build time, the data
        /// was converted to planar during construction.
    }
}

impl AudioFrame {
    /// Copy this frame and its metadata into independent owned storage.
    #[must_use]
    pub fn to_owned(&self) -> Self {
        self.clone()
    }

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
        self.audio_descriptor()
    }

    #[cfg(test)]
    /// # Safety
    /// `raw` must reference live SDK sample and metadata allocations with
    /// initialized active samples through this ownership copy.
    pub(crate) unsafe fn from_raw(raw: NDIlib_audio_frame_v3_t) -> Result<AudioFrame> {
        // SAFETY: forwarded to the caller; the borrow holds the descriptor.
        let frame = unsafe { ValidatedAudio::audio(&raw)? };
        Ok(frame.owned_copy())
    }

    /// Set the timecode without reallocating the samples.
    pub fn set_timecode(&mut self, timecode: Timecode) {
        self.timecode = timecode;
    }

    /// Get tightly packed planar audio as 32-bit floats.
    /// Each channel's samples are contiguous; channels follow one another.
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// Recover tightly packed planar samples, retaining their allocation and capacity.
    pub fn into_data(self) -> Vec<f32> {
        self.data
    }

    /// Replace samples and return the previous vector for allocation reuse.
    ///
    /// # Errors
    ///
    /// Returns `data` unchanged with [`Error::InvalidFrame`] unless it has
    /// exactly the validated sample count. The frame is unchanged.
    pub fn swap_data(
        &mut self,
        data: Vec<f32>,
    ) -> std::result::Result<Vec<f32>, Rejected<Vec<f32>>> {
        if let Err(error) = check_audio_data_len(&self.layout, data.len()) {
            return Err(Rejected::new(error, data));
        }
        Ok(std::mem::replace(&mut self.data, data))
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
        self.swap_data(data).map(drop)
    }

    /// Replace frame metadata. Use [`Self::clear_metadata`] to remove it.
    ///
    /// Invalid replacement text leaves the existing metadata unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] if `metadata` contains an interior NUL
    /// byte, or [`Error::InvalidFrame`] if the emitted C string would exceed
    /// the metadata size cap.
    pub fn set_metadata(&mut self, metadata: impl Into<String>) -> Result<()> {
        self.metadata = FrameMetadata::new(metadata)?;
        Ok(())
    }

    /// Remove frame metadata, retaining its allocation for later copies.
    pub fn clear_metadata(&mut self) {
        self.metadata.inner.clear();
    }

    /// Get mutable samples for one channel without allocating.
    pub fn channel_data_mut(&mut self, channel: usize) -> Option<&mut [f32]> {
        let range = self.layout.channel_range(channel)?;
        Some(&mut self.data[range])
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
    /// An owned string retains its allocation when there is capacity for
    /// the trailing NUL; invalid metadata leaves a rejected builder unchanged.
    #[must_use]
    pub fn metadata<S: Into<String>>(mut self, metadata: S) -> Self {
        self.metadata = Some(metadata.into());
        self
    }

    /// The caller-supplied samples in their configured input layout.
    pub fn supplied_data(&self) -> Option<&[f32]> {
        self.data.as_deref()
    }

    /// Take supplied data while retaining the builder's other settings.
    ///
    /// This permits repairing a refused buffer and putting it back with
    /// [`Self::data`]. Until then, building uses the zero-filled default.
    pub fn take_data(&mut self) -> Option<Vec<f32>> {
        self.data.take()
    }

    /// Recover the original supplied samples, retaining allocation and capacity.
    ///
    /// A rejected builder retains the input layout and untouched samples,
    /// including interleaved input. Returns `None` for default silent input.
    pub fn into_data(self) -> Option<Vec<f32>> {
        self.data
    }

    /// Use zero-filled default data instead of a supplied buffer.
    ///
    /// Recover the buffer first with [`Self::take_data`] when it is needed again.
    #[must_use]
    pub fn clear_data(mut self) -> Self {
        self.data = None;
        self
    }

    /// Remove metadata, for example to repair a rejected builder.
    #[must_use]
    pub fn clear_metadata(mut self) -> Self {
        self.metadata = None;
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
    /// Returns the unchanged builder with [`Error::InvalidFrame`] if the sample
    /// rate, channel or sample count, metadata size or supplied data length
    /// do not form a valid
    /// frame, and [`Error::InvalidCString`] for metadata containing a NUL
    /// byte.
    pub fn build(mut self) -> std::result::Result<AudioFrame, Rejected<Self>> {
        let mut validate = || -> Result<_> {
            let layout = validate_outbound_audio_layout(
                self.sample_rate,
                self.num_channels,
                self.num_samples,
                AudioFormat::Fltp,
            )?;
            if let Some(data) = &self.data {
                check_audio_data_len(&layout, data.len())?;
            }
            // Transfer metadata only after every rejection check, before
            // interleaving conversion or changing sample storage.
            let metadata = FrameMetadata::take_from(&mut self.metadata)?.unwrap_or_default();
            Ok((layout, metadata))
        };
        let (audio_layout, metadata) = match validate() {
            Ok(parts) => parts,
            Err(error) => return Err(Rejected::new(error, self)),
        };
        let sample_count = audio_layout.sample_count;

        let data = if let Some(input_data) = self.data {
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
    /// An owned string retains its allocation when there is capacity for
    /// the trailing NUL, and otherwise grows once.
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

    /// Get the UTF-8 payload bytes, excluding the trailing NUL.
    pub fn as_bytes(&self) -> &[u8] {
        self.data().as_bytes()
    }

    /// Copy this frame into an independently owned metadata frame.
    #[must_use]
    pub fn to_owned(&self) -> Self {
        self.clone()
    }

    /// Copy text and timecode into an existing frame. Reuses its text
    /// allocation when its capacity holds the text and trailing NUL.
    pub fn copy_into(&self, destination: &mut Self) {
        MetadataSource::copy_into(self, destination);
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

    /// Borrow the retained C string through a synchronous SDK call.
    pub(crate) fn to_raw(&self) -> NDIlib_metadata_frame_t {
        NDIlib_metadata_frame_t {
            length: self.data.inner.len() as i32,
            timecode: self.timecode.ticks(),
            p_data: self.data.as_ptr().cast_mut(),
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
        // SAFETY: forwarded to the caller.
        let text = unsafe { validate_metadata_layout(raw)? };
        Ok(Self::copied(text, Timecode::from_ticks(raw.timecode)))
    }

    /// Copy validated metadata text into an owned frame. `None`, the empty
    /// null frame, carries no text; it is empty metadata.
    pub(crate) fn copied(text: Option<MetadataText<'_>>, timecode: Timecode) -> Self {
        let data = FrameMetadata::copy_from(text.unwrap_or(MetadataText { with_nul: "\0" }), None);
        Self { data, timecode }
    }
}

/// The private, validated text boundary used by [`MetadataSource`].
///
/// # Safety
///
/// Returned text must be bounded NUL-terminated UTF-8, live and unchanged for
/// the borrow. Any overridden descriptor builder must describe only that live
/// initialized text, including its terminator, within the metadata size cap.
pub(crate) unsafe trait MetadataParts {
    fn metadata_text_for_send(&self) -> Option<MetadataText<'_>>;
    fn metadata_timecode(&self) -> Timecode;

    /// Canonicalize empty metadata to a non-null empty C string. The SDK
    /// reads/copies the text during the call, and the source remains borrowed
    /// until it returns. Validation has already bounded the length to i32.
    fn metadata_raw(&self) -> NDIlib_metadata_frame_t {
        let text = self.metadata_text_for_send();
        NDIlib_metadata_frame_t {
            length: text.map_or(1, MetadataText::len_with_nul) as i32,
            timecode: self.metadata_timecode().ticks(),
            p_data: text.map_or(c"".as_ptr(), MetadataText::as_ptr).cast_mut(),
        }
    }
}

/// A validated metadata input for sending, connection metadata and recording.
///
/// Implemented by [`MetadataFrame`] and [`MetadataFrameRef`]. Both can be sent
/// directly, including through `&dyn MetadataSource`, without a wrapper string
/// allocation or copy in synchronous operations. The SDK copies the text during
/// the call, so a captured frame can be released as soon as the operation
/// returns, even when registering metadata for future connections. Async
/// recording copies the text immediately into its deferred command. The
/// source's timecode is preserved in every operation.
///
/// This trait is sealed: applications cannot supply unchecked native pointers.
#[allow(private_bounds)]
pub trait MetadataSource: MetadataParts {
    /// The UTF-8 text without its trailing NUL. Empty metadata returns `""`.
    fn data(&self) -> &str {
        self.metadata_text_for_send()
            .map_or("", MetadataText::as_str)
    }

    /// The payload bytes, excluding the trailing NUL.
    fn as_bytes(&self) -> &[u8] {
        self.data().as_bytes()
    }

    /// The source timecode, including [`Timecode::SYNTHESIZE`] when requested.
    fn timecode(&self) -> Timecode {
        self.metadata_timecode()
    }

    /// Copy the text and timecode into an independently owned frame.
    fn to_owned(&self) -> MetadataFrame {
        MetadataFrame::copied(self.metadata_text_for_send(), self.metadata_timecode())
    }

    /// Copy into an existing frame, reusing its text allocation when its
    /// capacity holds the text and trailing NUL, even when the length changes.
    fn copy_into(&self, destination: &mut MetadataFrame) {
        let text = self
            .metadata_text_for_send()
            .unwrap_or(MetadataText { with_nul: "\0" });
        destination.data.copy_text(text);
        destination.timecode = self.metadata_timecode();
    }
}

// SAFETY: FrameMetadata owns bounded UTF-8 and its sole terminal NUL. The
// overridden descriptor uses exactly that live initialized string, whose
// allocation and contents stay unchanged for the borrow.
unsafe impl MetadataParts for MetadataFrame {
    fn metadata_text_for_send(&self) -> Option<MetadataText<'_>> {
        Some(self.data.text())
    }

    fn metadata_timecode(&self) -> Timecode {
        self.timecode
    }

    fn metadata_raw(&self) -> NDIlib_metadata_frame_t {
        self.to_raw()
    }
}

impl MetadataSource for MetadataFrame {}

/// Validated metadata text, borrowed together with its terminator: the
/// pointer and length of a C string that is known to be valid.
///
/// `with_nul` is UTF-8 whose only NUL byte is its last, and it is at most
/// [`MAX_METADATA_BYTES`] long. Each construction establishes this: the
/// validation of application, SDK and owned text, and the re-borrow of SDK
/// text that [`ValidatedFrame`] measured. Absent text (a null pointer, or a
/// standalone frame's empty null frame) is `None` of
/// `Option<MetadataText>`; an explicit, non-null empty C string is `"\0"`.
///
/// It is a shared string borrow, so a frame that holds one keeps its thread
/// and unwind auto traits.
#[derive(Clone, Copy)]
pub(crate) struct MetadataText<'a> {
    with_nul: &'a str,
}

impl<'a> MetadataText<'a> {
    /// Validate application metadata: its length and UTF-8. A `CStr` ends in
    /// its only NUL, so its known length bounds every check, and nothing
    /// scans raw memory.
    pub(crate) fn from_cstr(metadata: &'a CStr) -> Result<Self> {
        let with_nul = metadata.to_bytes_with_nul();
        validate_metadata_len_with_nul(with_nul.len())?;
        let with_nul = str::from_utf8(with_nul).map_err(|e| Error::InvalidUtf8(e.to_string()))?;
        Ok(Self { with_nul })
    }

    /// The text, without its terminator.
    pub(crate) fn as_str(self) -> &'a str {
        // The last byte is the one-byte NUL character, so this is a
        // character boundary.
        &self.with_nul[..self.with_nul.len() - 1]
    }

    /// The text as a C string for the SDK: the terminator follows it.
    pub(crate) fn as_ptr(self) -> *const c_char {
        self.with_nul.as_ptr().cast()
    }

    /// The text's length, terminator included.
    fn len_with_nul(self) -> usize {
        self.with_nul.len()
    }
}

impl fmt::Debug for MetadataText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("MetadataText").field(&self.as_str()).finish()
    }
}

/// Validate standalone metadata frame layout from raw FFI fields.
///
/// The SDK `length` field includes the trailing NUL terminator. The accepted
/// empty null frame (`length == 0 && p_data == NULL`) is `None` and never
/// requires reading from `p_data`.
///
/// This validates only memory layout and UTF-8. XML well-formedness is
/// intentionally not checked because NDI receivers are expected to tolerate
/// badly formed XML metadata.
///
/// # Safety
///
/// `raw.p_data`, when non-null, must reference readable storage for the
/// declared length or a NUL-terminated string within the metadata cap, live
/// and unchanged for as long as `raw` is borrowed.
pub(crate) unsafe fn validate_metadata_layout(
    raw: &NDIlib_metadata_frame_t,
) -> Result<Option<MetadataText<'_>>> {
    let len_with_nul = validate_metadata_prefix(raw)?;

    if len_with_nul == 0 {
        if raw.p_data.is_null() {
            return Ok(None);
        }

        // The SDK permits zero length as a C-string sentinel. Apply the same
        // bounded scan and SDK pointer-validity contract as per-frame metadata;
        // a non-null pointer validates to `Some`.
        // SAFETY: the caller keeps a non-null `p_data` of zero length
        // readable up to its first NUL or the metadata cap, and unchanged,
        // while `raw` is borrowed.
        return unsafe { validate_frame_metadata(raw.p_data) };
    }

    // SAFETY: `p_data` is non-null, and the caller keeps the declared
    // `len_with_nul` bytes at it readable and unchanged while `raw` is
    // borrowed; `u8` needs no alignment, and the length is within the
    // metadata cap.
    let bytes = unsafe { slice::from_raw_parts(raw.p_data.cast::<u8>(), len_with_nul) };
    let Some((&0, payload)) = bytes.split_last() else {
        return Err(Error::InvalidFrame(
            "Metadata frame length does not include a trailing NUL terminator".into(),
        ));
    };
    if payload.contains(&0) {
        return Err(Error::InvalidFrame(
            "Metadata frame contains an interior NUL byte".into(),
        ));
    }

    // The bytes end in their only NUL and are within the cap.
    let with_nul = str::from_utf8(bytes).map_err(|err| Error::InvalidUtf8(err.to_string()))?;
    Ok(Some(MetadataText { with_nul }))
}

/// Validate metadata returned specifically by `NDIlib_send_capture`.
///
/// `Processing.NDI.structs.h` independently guarantees NUL-terminated UTF-8
/// `p_data`, and the SDK's Send_VirtualPTZ sample reads captured sender metadata
/// as a C string. Positive reported counts are checked for invalid values and
/// excessive size, but are not treated as exact byte extents: native captures
/// can report a count that does not include the string's terminator.
///
/// Unlike [`validate_metadata_layout`], this function requires the SDK's full
/// C-string guarantee even for positive lengths. The declared-length validator
/// remains strict for descriptors promising only that many readable bytes.
///
/// # Safety
///
/// A non-null `p_data` must be readable byte-by-byte through its first NUL
/// terminator or the metadata cap, live and unchanged while `raw` is borrowed.
/// In particular, `length` readable bytes alone do not satisfy this contract.
pub(crate) unsafe fn validate_sender_metadata(
    raw: &NDIlib_metadata_frame_t,
) -> Result<Option<MetadataText<'_>>> {
    validate_metadata_prefix(raw)?;
    // SAFETY: the caller supplies the stronger SDK C-string contract regardless
    // of the reported count. The shared scan stops at the first NUL or the cap.
    unsafe { validate_frame_metadata(raw.p_data) }
}

/// Check descriptor fields before either bounded-byte or native C-string
/// validation reads a pointer. Zero retains the SDK's lengthless sentinel.
fn validate_metadata_prefix(raw: &NDIlib_metadata_frame_t) -> Result<usize> {
    if raw.length < 0 {
        return Err(Error::InvalidFrame(format!(
            "Metadata frame has negative length: {}",
            raw.length
        )));
    }

    if raw.length == 0 {
        return Ok(0);
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
    Ok(len_with_nul)
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

/// The state of optional per-frame metadata attached to video or audio.
///
/// Unreadable metadata does not discard otherwise valid media. Its text is
/// withheld from [`VideoSource::metadata`] and [`AudioSource::metadata`], from
/// forwarding descriptors, and from owned copies. Owned frames contain only
/// valid text, so a copy of unreadable metadata has [`Self::Absent`] status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FrameMetadataStatus {
    /// No per-frame metadata pointer was supplied.
    Absent,
    /// Valid UTF-8 text, including an explicitly empty string.
    Text,
    /// A terminated string contained bytes that are not UTF-8.
    NotUtf8,
    /// No NUL terminator appeared within the 4 MiB metadata cap.
    Unterminated,
}

/// Cached validation of one optional per-frame metadata pointer.
/// Private fields prevent a caller from forging a validated text length.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameMetadataState {
    status: FrameMetadataStatus,
    len_with_nul: usize,
}

impl FrameMetadataState {
    /// Scan once without allocation, retaining media when its side text is bad.
    ///
    /// # Safety
    ///
    /// A non-null pointer must be readable through its first NUL or the
    /// metadata cap, whichever comes first.
    pub(crate) unsafe fn scan(pointer: *const c_char) -> Self {
        if pointer.is_null() {
            return Self::from_text(None);
        }
        // SAFETY: the caller guarantees the bounded readable extent.
        let Some(bytes) = (unsafe { bounded_sdk_text(pointer, MAX_METADATA_BYTES) }) else {
            return Self {
                status: FrameMetadataStatus::Unterminated,
                len_with_nul: 0,
            };
        };
        Self {
            status: if str::from_utf8(bytes).is_ok() {
                FrameMetadataStatus::Text
            } else {
                FrameMetadataStatus::NotUtf8
            },
            len_with_nul: bytes.len() + 1,
        }
    }

    /// Cache text that was already validated, including standalone metadata.
    pub(crate) fn from_text(text: Option<MetadataText<'_>>) -> Self {
        Self {
            status: if text.is_some() {
                FrameMetadataStatus::Text
            } else {
                FrameMetadataStatus::Absent
            },
            len_with_nul: text.map_or(0, MetadataText::len_with_nul),
        }
    }

    pub(crate) const fn status(self) -> FrameMetadataStatus {
        self.status
    }

    /// Reborrow validated text without rescanning it.
    ///
    /// # Safety
    ///
    /// `pointer` must name the same bytes used to construct this state, live
    /// and unchanged for `'a`. This includes the measured terminal NUL.
    pub(crate) unsafe fn text<'a>(self, pointer: *const c_char) -> Option<MetadataText<'a>> {
        if self.status != FrameMetadataStatus::Text {
            return None;
        }
        // SAFETY: construction validated UTF-8 and the terminator; the caller
        // keeps those same bytes live and unchanged for the returned borrow.
        let with_nul = unsafe {
            str::from_utf8_unchecked(slice::from_raw_parts(pointer.cast(), self.len_with_nul))
        };
        Some(MetadataText { with_nul })
    }
}

/// Owned video/audio per-frame metadata.
///
/// Present text includes its only NUL as its last byte and fits within the
/// metadata size cap. An empty string buffer means absent metadata while
/// retaining its capacity; explicit empty text holds its one NUL byte. Keeping its capacity, unlike a `CString`, lets copies
/// reuse storage when per-frame text changes length. UTF-8 is guaranteed by
/// `String`; the private constructors preserve termination. `None` on a frame
/// means a null SDK `p_metadata`; `Some("")` is a non-null empty C string.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub(crate) struct FrameMetadata {
    inner: String,
}

impl FrameMetadata {
    pub(crate) fn new<S: Into<String>>(metadata: S) -> Result<Self> {
        Ok(Self::take_from(&mut Some(metadata.into()))?
            .expect("a supplied metadata string produces a value"))
    }

    /// Validate and take an optional owned string, retaining its capacity.
    /// Failure leaves the original option and text unchanged, so builders
    /// can return their complete input without copying successful metadata.
    fn take_from(metadata: &mut Option<String>) -> Result<Option<Self>> {
        let Some(text) = metadata.as_mut() else {
            return Ok(None);
        };
        let len_with_nul = text.len().checked_add(1).ok_or_else(|| {
            Error::InvalidFrame("Frame metadata length overflow while adding terminator".into())
        })?;
        validate_metadata_len_with_nul(len_with_nul)?;
        if text.as_bytes().contains(&0) {
            // Construct the existing owned diagnostic only on the error path;
            // the builder keeps its original string and capacity.
            return Err(Error::InvalidCString(
                CString::new(text.as_str()).expect_err("metadata contains an interior NUL"),
            ));
        }
        // String already guarantees UTF-8; adding its only NUL needs no
        // CString round trip, allocation shrink or unchecked UTF-8 conversion.
        text.reserve_exact(1);
        text.push('\0');
        Ok(metadata.take().map(|inner| Self { inner }))
    }

    /// Present text with its terminator, distinguishing absence from empty text.
    fn optional_text(&self) -> Option<MetadataText<'_>> {
        (!self.inner.is_empty()).then_some(MetadataText {
            with_nul: &self.inner,
        })
    }

    /// The text of standalone metadata, which always holds its terminal NUL.
    pub(crate) fn text(&self) -> MetadataText<'_> {
        self.optional_text()
            .expect("standalone metadata holds terminated text")
    }

    pub(crate) fn as_str(&self) -> &str {
        self.optional_text().map_or("", MetadataText::as_str)
    }

    pub(crate) fn into_string(mut self) -> String {
        self.inner.pop();
        self.inner
    }

    pub(crate) fn as_ptr(&self) -> *const c_char {
        if self.inner.is_empty() {
            ptr::null()
        } else {
            self.inner.as_ptr().cast()
        }
    }

    /// All bytes retained by this metadata, including spare string capacity.
    #[cfg(feature = "advanced")]
    pub(crate) fn retained_bytes(&self) -> usize {
        self.inner.capacity()
    }

    /// Copy validated text into owned metadata, reusing the allocation of
    /// `storage` when there is one.
    ///
    /// Without `storage` this allocates once. With it, it allocates nothing
    /// when the capacity holds the new text and its terminator.
    pub(crate) fn copy_from(text: MetadataText<'_>, storage: Option<Self>) -> Self {
        match storage {
            Some(mut storage) => {
                storage.copy_text(text);
                storage
            }
            None => Self {
                inner: text.with_nul.to_owned(),
            },
        }
    }

    /// Replace validated text, reserving before mutation so any allocation
    /// happens while the original string is still complete and terminated.
    fn copy_text(&mut self, text: MetadataText<'_>) {
        self.inner
            .reserve_exact(text.len_with_nul().saturating_sub(self.inner.len()));
        self.inner.clear();
        self.inner.push_str(text.with_nul);
    }
}

impl fmt::Debug for FrameMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FrameMetadata")
            .field(&self.as_str())
            .finish()
    }
}

/// Validate a video/audio per-frame `p_metadata` pointer with a bounded scan.
///
/// The SDK exposes these pointers as optional lengthless C strings. A null
/// pointer is `None`; otherwise the result records the first terminator found
/// by the scan.
///
/// This continues to trust the NDI SDK that a non-null pointer is readable. The
/// validation does not make dangling or otherwise invalid pointers safe; it
/// only limits the scan to `MAX_METADATA_BYTES`, requires a terminator within
/// that cap, and validates UTF-8 before exposing safe text access.
///
/// # Safety
///
/// When non-null, `p_metadata` must be readable byte-by-byte until either the
/// first NUL terminator or `MAX_METADATA_BYTES` bytes have been read, and
/// the bytes it reads must stay live and unchanged for `'a`.
pub(crate) unsafe fn validate_frame_metadata<'a>(
    p_metadata: *const c_char,
) -> Result<Option<MetadataText<'a>>> {
    if p_metadata.is_null() {
        return Ok(None);
    }

    // SAFETY: forwarded to the caller's obligation for a non-null pointer.
    let Some(payload) = (unsafe { bounded_sdk_text(p_metadata, MAX_METADATA_BYTES) }) else {
        return Err(Error::InvalidFrame(format!(
            "Frame metadata is missing a NUL terminator within {MAX_METADATA_BYTES} bytes"
        )));
    };
    // SAFETY: the scan just read these bytes and the NUL after them, all
    // within the cap, from the non-null `p_metadata`, which the caller keeps
    // live and unchanged for `'a`; `u8` needs no alignment.
    let bytes = unsafe { slice::from_raw_parts(p_metadata.cast::<u8>(), payload.len() + 1) };
    // The bytes end in their only NUL, found below the cap.
    let with_nul = str::from_utf8(bytes).map_err(|err| Error::InvalidUtf8(err.to_string()))?;
    Ok(Some(MetadataText { with_nul }))
}

/// The bytes of an SDK string before its first NUL, or `None` when there is
/// no NUL within the first `limit` bytes.
///
/// The scan reads one byte at a time and stops at the first NUL, so it never
/// reads past the string, and it reads at most `limit` bytes.
///
/// # Safety
///
/// `text` must be non-null and readable byte-by-byte for `'a` until either
/// its first NUL or `limit` bytes have been read.
pub(crate) unsafe fn bounded_sdk_text<'a>(text: *const c_char, limit: usize) -> Option<&'a [u8]> {
    let bytes = text.cast::<u8>();
    // SAFETY: each byte read is at or before the first NUL and within the
    // bound, which the caller guarantees readable.
    let len = (0..limit).find(|&len| unsafe { bytes.add(len).read() } == 0)?;
    // SAFETY: the `len` bytes before the NUL were just read, and the caller
    // keeps them readable for `'a`.
    Some(unsafe { slice::from_raw_parts(bytes, len) })
}

/// The most one SDK string other than frame metadata may take, terminator
/// included: a Discovery Server string, or a source's name, address or
/// advertised metadata.
pub(crate) const MAX_SDK_STRING_BYTES: usize = 4 * 1024 * 1024;

/// Copy an SDK string of at most [`MAX_SDK_STRING_BYTES`], terminator
/// included, replacing bytes that are not UTF-8.
///
/// # Errors
///
/// [`Error::UnexpectedSdkResult`] naming `operation`, the SDK function that
/// returned the string, when there is no terminator within the limit.
///
/// # Safety
///
/// `text` must be non-null and readable byte-by-byte until either its first
/// NUL or [`MAX_SDK_STRING_BYTES`] bytes have been read.
pub(crate) unsafe fn copy_sdk_string(text: *const c_char, operation: &str) -> Result<String> {
    // SAFETY: forwarded to the caller's obligation.
    let bytes = unsafe { bounded_sdk_text(text, MAX_SDK_STRING_BYTES) }.ok_or_else(|| {
        Error::UnexpectedSdkResult(format!(
            "{operation} returned a string longer than the 4 MiB one string may be"
        ))
    })?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
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
    width: u32,
    /// The validated height in pixels.
    height: u32,
    /// The validated pixel format.
    pixel_format: PixelFormat,
    /// Complete storage extent in bytes; SDK padding need not be initialized.
    data_len_bytes: usize,
    /// Initialized active payload, excluding all row padding.
    payload_len_bytes: usize,
    plane_layouts: [VideoPlaneLayout; MAX_VIDEO_PLANES],
    plane_count: usize,
    /// The validated first-plane line stride in bytes.
    line_stride_bytes: usize,
}

/// A validated layout authorized for the selected runtime. Its fields are
/// private: a proof for one format cannot be used to write another FourCC.
#[derive(Clone, Copy)]
pub(crate) struct SendableVideoLayout(ValidatedVideoLayout);

impl SendableVideoLayout {
    pub(crate) fn new(layout: ValidatedVideoLayout, ndi: &crate::Ndi) -> Result<Self> {
        layout.pixel_format().require_sendable(ndi)?;
        Ok(Self(layout))
    }

    pub(crate) const fn layout(&self) -> &ValidatedVideoLayout {
        &self.0
    }

    /// Packing preserves format and dimensions, and therefore authorization.
    pub(crate) fn packed(self) -> Self {
        Self(self.0.packed())
    }

    #[cfg(test)]
    pub(crate) fn for_test(layout: ValidatedVideoLayout) -> Self {
        Self(layout)
    }
}

// The width, height and line stride of a valid layout are each at most its
// storage extent, so every one of them fits the SDK's signed 32-bit fields.
const _: () = assert!(MAX_VIDEO_BYTES <= i32::MAX as usize);

impl ValidatedVideoLayout {
    /// Initialized active payload, excluding padding.
    pub(crate) const fn payload_len_bytes(&self) -> usize {
        self.payload_len_bytes
    }

    /// The validated `width` value.
    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    /// The validated `height` value.
    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    /// The validated `pixel_format` value.
    pub(crate) const fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    /// The validated `data_len_bytes` value.
    pub(crate) const fn data_len_bytes(&self) -> usize {
        self.data_len_bytes
    }

    /// The validated `line_stride_bytes` value.
    pub(crate) const fn line_stride_bytes(&self) -> usize {
        self.line_stride_bytes
    }

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

    /// This layout with every plane's rows packed, without padding: the
    /// layout of an owned copy of the frame.
    ///
    /// It keeps each validated plane's active rows and closes the gaps: a
    /// plane's stride becomes its active row and its offset the payload of
    /// the planes before it, so the storage is exactly the active payload.
    /// Nothing is computed that validation did not already bound: every
    /// partial sum is at most the payload, which validation summed without
    /// overflow and which is at most the storage it checked against
    /// [`MAX_VIDEO_BYTES`]. It is the layout [`video_plane_layouts`] gives
    /// the packed stride, `width` times the bytes per pixel, which a test
    /// checks for every format.
    pub(crate) fn packed(&self) -> Self {
        let mut plane_layouts = self.plane_layouts;
        let mut offset = 0;
        for plane in &mut plane_layouts[..self.plane_count] {
            plane.offset = offset;
            plane.row_stride = plane.active_row_bytes;
            offset += plane.active_row_bytes * plane.row_count;
        }
        debug_assert_eq!(offset, self.payload_len_bytes);
        Self {
            data_len_bytes: self.payload_len_bytes,
            plane_layouts,
            line_stride_bytes: self.plane_layouts[0].active_row_bytes,
            ..*self
        }
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

    /// The storage after the frame's last active byte: the padding of the
    /// last row of the last plane, where the storage ends.
    pub(crate) fn trailing_padding(&self) -> usize {
        let last = self.plane_layouts[self.plane_count - 1];
        last.row_stride - last.active_row_bytes
    }

    fn planes(&self) -> &[VideoPlaneLayout] {
        &self.plane_layouts[..self.plane_count]
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

    /// Every active byte of the frame stored at `data`, in SDK plane order,
    /// or `None` when padding separates rows or planes. A single row may
    /// have padding after it, which is not part of the bytes.
    ///
    /// The bytes are the one row of the span [`VideoPlaneLayout::contiguous`]
    /// finds, read as [`VideoPlane::data`] reads a plane's, so a frame and
    /// each of its planes share one contiguity rule and one slice.
    ///
    /// # Safety
    ///
    /// As for [`Self::view_plane`].
    unsafe fn view_data<'a>(&self, data: *const u8) -> Option<&'a [u8]> {
        let span = VideoPlaneLayout::contiguous(self.planes())?;
        // SAFETY: the span is one row of active bytes, from the first plane's
        // offset to the end of the last plane's last active row, within the
        // storage this layout describes. The caller keeps every active row
        // initialized and immutable for `'a`.
        unsafe { VideoPlane::new(data, span) }.data()
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
    // SAFETY: both members of the descriptor's union are `int`s at the same
    // offset, so reading either one reads the four bytes the SDK or the
    // caller wrote as a valid `i32`. `Processing.NDI.structs.h` makes the
    // member the line stride for a FourCC that is not a compressed type, and
    // `PixelFormat::try_from` just accepted an uncompressed format.
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

impl VideoPlaneLayout {
    /// The active rows of `planes`, consecutive planes of one validated
    /// frame, as one row, or `None` when padding separates their rows or the
    /// planes: the one contiguity rule, for a frame and for each of its
    /// planes. Padding after the last row is not part of the span.
    fn contiguous(planes: &[Self]) -> Option<Self> {
        let start = planes.first()?.offset;
        let mut end = start;
        for plane in planes {
            let rows_adjacent = plane.row_count == 1 || plane.row_stride == plane.active_row_bytes;
            if plane.offset != end || !rows_adjacent {
                return None;
            }
            // At most the plane's storage, which validation bounded.
            end = plane.offset + plane.row_count * plane.active_row_bytes;
        }
        let len = end - start;
        Some(Self {
            offset: start,
            row_stride: len,
            active_row_bytes: len,
            row_count: 1,
        })
    }
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
/// This struct holds pre-validated layout information for a frame of audio
/// samples, including the channel stride and computed backing sample count.
/// Every layout describes at least one sample in at least one channel; audio
/// the SDK returns without samples is classified by
/// [`validate_returned_audio`] instead. Creating this struct performs all
/// necessary bounds checking, so consumers can safely use the cached values
/// without re-validation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ValidatedAudioLayout {
    /// The validated audio format.
    format: AudioFormat,
    /// The validated sample rate: positive and at most `i32::MAX`.
    sample_rate: u32,
    /// The validated channel count: positive.
    no_channels: usize,
    /// The validated samples per channel: positive.
    no_samples: usize,
    /// The validated channel stride in `f32` samples.
    channel_stride_samples: usize,
    /// The validated backing sample count. For strided planar audio this may be
    /// larger than `no_channels * no_samples` because it includes inter-channel
    /// padding up to the last channel's final sample.
    sample_count: usize,
}

/// Initialized audio channel slices in channel order, without allocation.
///
/// Inter-channel padding is never included. The iterator borrows the frame and
/// keeps its sample storage alive; its immutable views support scoped thread
/// transfer even when the owning capture must be freed on its original thread.
#[derive(Debug, Clone)]
pub struct AudioChannels<'a> {
    data: *const f32,
    layout: ValidatedAudioLayout,
    indices: std::ops::Range<usize>,
    lifetime: PhantomData<&'a [f32]>,
}

// SAFETY: Only validated, immutable channel samples can construct this iterator.
// Its borrow keeps their aligned allocation alive for 'a. Each yielded slice
// covers active initialized samples only. No operation accesses a native handle,
// mutates samples or frees storage; frame destruction still follows owner rules.
unsafe impl Send for AudioChannels<'_> {}
// SAFETY: Sharing exposes only immutable samples and independent iterator state.
unsafe impl Sync for AudioChannels<'_> {}

impl<'a> Iterator for AudioChannels<'a> {
    type Item = &'a [f32];

    fn next(&mut self) -> Option<Self::Item> {
        let channel = self.indices.next()?;
        // SAFETY: Construction retains validated immutable samples for 'a.
        unsafe { self.layout.channel(self.data, channel) }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.indices.size_hint()
    }
}

impl DoubleEndedIterator for AudioChannels<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let channel = self.indices.next_back()?;
        // SAFETY: As for Iterator::next.
        unsafe { self.layout.channel(self.data, channel) }
    }
}

impl ExactSizeIterator for AudioChannels<'_> {}
impl std::iter::FusedIterator for AudioChannels<'_> {}

// The channel count, sample count and channel stride in bytes of a valid
// layout are each at most its byte extent, and the sample rate is checked
// against i32::MAX, so every one of them fits the SDK's signed 32-bit fields.
const _: () = assert!(MAX_AUDIO_BYTES <= i32::MAX as usize);

impl ValidatedAudioLayout {
    /// The validated `format` value.
    pub(crate) const fn format(&self) -> AudioFormat {
        self.format
    }

    /// The validated `sample_rate` value.
    pub(crate) const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The validated `no_channels` value.
    pub(crate) const fn no_channels(&self) -> usize {
        self.no_channels
    }

    /// The validated `no_samples` value.
    pub(crate) const fn no_samples(&self) -> usize {
        self.no_samples
    }

    /// The validated `channel_stride_samples` value.
    pub(crate) const fn channel_stride_samples(&self) -> usize {
        self.channel_stride_samples
    }

    /// The validated `sample_count` value.
    pub(crate) const fn sample_count(&self) -> usize {
        self.sample_count
    }

    fn from_packed(format: AudioFormat, sample_rate: u32, packed: &PackedAudioDimensions) -> Self {
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

    fn num_channels(self) -> u32 {
        self.no_channels as u32
    }

    fn num_samples(self) -> u32 {
        self.no_samples as u32
    }

    fn channel_stride_bytes(self) -> usize {
        self.channel_stride_samples() * std::mem::size_of::<f32>()
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
        if channel >= self.no_channels {
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
    /// without reading inter-channel padding.
    ///
    /// # Safety
    ///
    /// As for [`Self::channel`].
    unsafe fn channels<'a>(self, data: *const f32) -> AudioChannels<'a> {
        AudioChannels {
            data,
            layout: self,
            indices: 0..self.no_channels,
            lifetime: PhantomData,
        }
    }

    /// Every sample of the frame stored at `data`, channels back to back, or
    /// `None` when padding separates channels: the one contiguity rule for
    /// audio, beside the channel views. An owned frame's channels are always
    /// back to back.
    ///
    /// # Safety
    ///
    /// As for [`Self::channel`].
    unsafe fn view_samples<'a>(self, data: *const f32) -> Option<&'a [f32]> {
        if self.no_channels > 1 && self.channel_stride_samples != self.no_samples {
            return None;
        }
        // SAFETY: with one channel, or channels back to back, the
        // `sample_count` samples from `data` are every channel's samples and
        // nothing else, which the caller keeps aligned, initialized and
        // immutable for `'a`.
        Some(unsafe { slice::from_raw_parts(data, self.sample_count) })
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
        format,
        sample_rate,
        &packed,
    ))
}

/// Require supplied samples to fill the packed `layout` exactly.
fn check_audio_data_len(layout: &ValidatedAudioLayout, len: usize) -> Result<()> {
    if len != layout.sample_count {
        return Err(Error::InvalidFrame(format!(
            "Audio data length {len}, expected {} samples ({} channels x {} samples) for validated layout",
            layout.sample_count, layout.no_channels, layout.no_samples
        )));
    }
    Ok(())
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
/// A descriptor without samples is invalid here; only
/// [`validate_returned_audio`] accepts one.
pub(crate) fn validate_audio_layout(raw: &NDIlib_audio_frame_v3_t) -> Result<ValidatedAudioLayout> {
    if raw.no_samples == 0 {
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
    // SAFETY: both members of the descriptor's union are `int`s at the same
    // offset, so reading either one reads the four bytes the SDK or the
    // caller wrote as a valid `i32`. `Processing.NDI.structs.h` makes the
    // member the channel stride for planar audio that is not compressed, and
    // `validate_audio_format` just accepted FLTP, the planar float format.
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
        format,
        sample_rate,
        no_channels,
        no_samples,
        channel_stride_samples,
        sample_count,
    })
}

/// Audio the SDK returned from a call that may legitimately deliver no
/// samples: a FrameSync input-format query, or an AVSync result with nothing
/// to deliver.
///
/// A frame of samples is its layout when only the shape was classified, and
/// a [`ValidatedFrame`] when [`ValidatedAudio::returned_audio`] validated its
/// metadata too.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ReturnedAudio<T = ValidatedAudioLayout> {
    /// A frame of samples, validated as [`validate_audio_layout`] validates.
    Samples(T),
    /// No samples. Carries the input format when the SDK reported one, and
    /// `None` when no source format is known yet.
    Empty(Option<AudioInputFormat>),
}

/// Classify audio the SDK returned where "no samples" is a valid answer.
///
/// A descriptor with a nonzero sample count must pass
/// [`validate_audio_layout`]. One without samples must have a null data
/// pointer and a zero channel stride, and either a zero sample rate and
/// channel count (no source yet, where the FourCC may also be left zero) or a
/// positive sample rate and channel count with a supported FourCC.
///
/// # Errors
///
/// [`Error::InvalidFrame`] for any other shape. The descriptor's metadata is
/// not examined.
pub(crate) fn validate_returned_audio(raw: &NDIlib_audio_frame_v3_t) -> Result<ReturnedAudio> {
    if raw.no_samples != 0 {
        return validate_audio_layout(raw).map(ReturnedAudio::Samples);
    }

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

    // SAFETY: both members of the descriptor's union are `int`s at the same
    // offset, so reading either one reads the four bytes the SDK wrote as a
    // valid `i32`, whichever member `Processing.NDI.structs.h` makes it for
    // this FourCC. Audio without samples has neither a channel stride nor a
    // buffer size, so the value must be zero whatever its meaning.
    let channel_stride_in_bytes = unsafe { raw.__bindgen_anon_1.channel_stride_in_bytes };
    if channel_stride_in_bytes != 0 {
        return Err(Error::InvalidFrame(format!(
            "Zero-length audio frame has non-zero channel_stride_in_bytes: {}",
            channel_stride_in_bytes
        )));
    }

    // Both were just checked nonnegative, so zero is the only value that
    // does not convert.
    let sample_rate = NonZeroU32::new(raw.sample_rate.unsigned_abs());
    let num_channels = NonZeroU32::new(raw.no_channels.unsigned_abs());
    let format = match (sample_rate, num_channels) {
        // No source format is known yet. The SDK may leave the FourCC unset.
        (None, None) => {
            if raw.FourCC != 0 {
                validate_audio_format(raw.FourCC)?;
            }
            None
        }
        (Some(sample_rate), Some(num_channels)) => {
            validate_audio_format(raw.FourCC)?;
            Some(AudioInputFormat::new(sample_rate, num_channels))
        }
        _ => {
            return Err(Error::InvalidFrame(format!(
                "Invalid zero-length audio state: sample_rate={}, no_channels={}",
                raw.sample_rate, raw.no_channels
            )))
        }
    };

    Ok(ReturnedAudio::Empty(format))
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
// SDK frames validated as a whole
// ============================================================================

/// An SDK frame descriptor that a [`ValidatedFrame`] holds: where it keeps
/// its metadata text.
pub(crate) trait SdkDescriptor {
    /// The descriptor's metadata text pointer, null for none.
    fn metadata_pointer(&self) -> *const c_char;
}

impl SdkDescriptor for NDIlib_video_frame_v2_t {
    fn metadata_pointer(&self) -> *const c_char {
        self.p_metadata
    }
}

impl SdkDescriptor for NDIlib_audio_frame_v3_t {
    fn metadata_pointer(&self) -> *const c_char {
        self.p_metadata
    }
}

impl SdkDescriptor for NDIlib_metadata_frame_t {
    fn metadata_pointer(&self) -> *const c_char {
        self.p_data
    }
}

/// A holder of one SDK frame descriptor: a capture [`Guard`], which owns the
/// frame and frees it when dropped, or a borrow of a descriptor.
pub(crate) trait HoldsDescriptor {
    /// The descriptor it holds.
    type Descriptor: SdkDescriptor;

    /// The descriptor.
    fn descriptor(&self) -> &Self::Descriptor;

    /// Normalize an audio timestamp for this descriptor's capture family.
    fn audio_timestamp(&self, ticks: i64) -> Option<SdkTimestamp> {
        SdkTimestamp::from_sdk(ticks)
    }
}

impl<S: FrameFree> HoldsDescriptor for Guard<'_, S>
where
    S::RawFrame: SdkDescriptor,
{
    type Descriptor = S::RawFrame;

    fn descriptor(&self) -> &S::RawFrame {
        self.frame()
    }

    fn audio_timestamp(&self, ticks: i64) -> Option<SdkTimestamp> {
        if ticks == S::AUDIO_TIMESTAMP_UNDEFINED {
            None
        } else {
            SdkTimestamp::from_sdk(ticks)
        }
    }
}

impl<D: SdkDescriptor> HoldsDescriptor for &D {
    type Descriptor = D;

    fn descriptor(&self) -> &D {
        self
    }
}

/// An SDK frame validated as a whole: the holder `F` of its descriptor, what
/// was validated from that descriptor, `V`, and its cached metadata state.
///
/// Only the validation functions construct one, each from the one descriptor
/// its holder holds: [`ValidatedVideo::video`], [`ValidatedAudio::audio`],
/// [`ValidatedAudio::returned_audio`] and
/// [`ValidatedMetadata::metadata_frame`]. A layout or text validated from
/// another descriptor therefore cannot be paired with it, and every view and
/// owned copy of a received frame takes its parts from here.
///
/// The text is kept as a length and borrowed again from the descriptor's
/// pointer, so that this type adds no pointer of its own: a frame reference
/// that holds one keeps the thread and unwind auto traits of its guard. `V`
/// is a parameter rather than a projection from `F` so that a reference
/// stays covariant in the lifetime of its guard.
pub(crate) struct ValidatedFrame<F, V> {
    frame: F,
    validated: V,
    /// Text length when valid, or the reason optional metadata was withheld.
    metadata_state: FrameMetadataState,
}

/// A video frame validated with its layout and header.
pub(crate) type ValidatedVideo<F> = ValidatedFrame<F, (ValidatedVideoLayout, VideoHeader)>;

/// A frame of audio samples validated with its layout.
pub(crate) type ValidatedAudio<F> = ValidatedFrame<F, ValidatedAudioLayout>;

/// A standalone metadata frame, which is its text alone.
pub(crate) type ValidatedMetadata<F> = ValidatedFrame<F, ()>;

impl<F: HoldsDescriptor, V> ValidatedFrame<F, V> {
    /// Pair `frame` with `validated`, which the caller validated from its
    /// descriptor, and the per-frame metadata text the descriptor points to.
    ///
    /// # Safety
    ///
    /// The descriptor's metadata pointer must be null, or readable up to its
    /// first NUL or [`MAX_METADATA_BYTES`], with those bytes live and
    /// unchanged until `frame` is dropped, and `frame` must hold that one
    /// descriptor for as long as it lives.
    unsafe fn with_frame_metadata(frame: F, validated: V) -> Result<Self> {
        // SAFETY: forwarded to the caller.
        let metadata_state =
            unsafe { FrameMetadataState::scan(frame.descriptor().metadata_pointer()) };
        Ok(Self {
            metadata_state,
            frame,
            validated,
        })
    }

    /// The descriptor, exactly as the SDK delivered it.
    pub(crate) fn descriptor(&self) -> &F::Descriptor {
        self.frame.descriptor()
    }

    /// The validated metadata text, or `None` when the frame has none.
    pub(crate) fn metadata(&self) -> Option<MetadataText<'_>> {
        // SAFETY: construction scanned this descriptor's pointer; the holder
        // keeps the same bytes allocated and unchanged for this borrow.
        unsafe {
            self.metadata_state
                .text(self.descriptor().metadata_pointer())
        }
    }
}

impl<F: HoldsDescriptor<Descriptor = NDIlib_video_frame_v2_t>> ValidatedVideo<F> {
    /// Validate the uncompressed video frame `frame` holds: its pointer,
    /// header, pixel format and layout, as [`validate_video_frame`] does, and
    /// its metadata text.
    ///
    /// # Safety
    ///
    /// `frame` must hold one descriptor for as long as it lives, and until
    /// it is dropped, its pixel allocation must cover the layout the
    /// descriptor describes, with every active row initialized, and its
    /// metadata pointer must be null or readable up to its first NUL or
    /// [`MAX_METADATA_BYTES`], all live and unchanged. Row padding need not
    /// be initialized.
    pub(crate) unsafe fn video(frame: F) -> Result<Self> {
        let validated = validate_video_frame(frame.descriptor())?;
        // SAFETY: forwarded to the caller.
        unsafe { Self::with_frame_metadata(frame, validated) }
    }
}

impl<F: HoldsDescriptor<Descriptor = NDIlib_audio_frame_v3_t>> ValidatedAudio<F> {
    /// Validate the frame of audio samples `frame` holds, as
    /// [`validate_audio_layout`] does, and its metadata text.
    ///
    /// # Safety
    ///
    /// `frame` must hold one descriptor for as long as it lives, and until
    /// it is dropped, every channel's samples at the offsets the descriptor
    /// describes must be initialized, and its metadata pointer must be null
    /// or readable up to its first NUL or [`MAX_METADATA_BYTES`], all live
    /// and unchanged. Padding between channels need not be initialized.
    pub(crate) unsafe fn audio(frame: F) -> Result<Self> {
        let validated = validate_audio_layout(frame.descriptor())?;
        // SAFETY: forwarded to the caller.
        unsafe { Self::with_frame_metadata(frame, validated) }
    }

    /// Validate audio the SDK returned where no samples is a valid answer:
    /// its shape, as [`validate_returned_audio`] classifies it, and the
    /// metadata text of a frame of samples.
    ///
    /// # Safety
    ///
    /// As for [`Self::audio`], for a descriptor that holds samples.
    #[cfg(all(feature = "advanced", any(test, ndi_has_advanced_av_sync)))]
    pub(crate) unsafe fn returned_audio(frame: F) -> Result<ReturnedAudio<Self>> {
        Ok(match validate_returned_audio(frame.descriptor())? {
            ReturnedAudio::Empty(format) => ReturnedAudio::Empty(format),
            // SAFETY: forwarded to the caller for a frame of samples.
            ReturnedAudio::Samples(validated) => {
                ReturnedAudio::Samples(unsafe { Self::with_frame_metadata(frame, validated)? })
            }
        })
    }
}

impl<F: HoldsDescriptor<Descriptor = NDIlib_metadata_frame_t>> ValidatedMetadata<F> {
    /// Validate the standalone metadata frame `frame` holds, as
    /// [`validate_metadata_layout`] does.
    ///
    /// # Safety
    ///
    /// `frame` must hold one descriptor for as long as it lives, and until
    /// it is dropped, its data pointer must be null or reference readable
    /// storage for the declared length, or a NUL-terminated string within
    /// the metadata cap when the length is zero, live and unchanged.
    pub(crate) unsafe fn metadata_frame(frame: F) -> Result<Self> {
        // SAFETY: forwarded to the caller.
        let text = unsafe { validate_metadata_layout(frame.descriptor())? };
        Ok(Self {
            metadata_state: FrameMetadataState::from_text(text),
            frame,
            validated: (),
        })
    }
}

// SAFETY: video validates this descriptor's layout and metadata; its unsafe
// caller guarantees initialized active pixels and live unchanged storage for
// F's lifetime. Views borrow this holder. Default descriptors and direct-send
// checks never extend the initialized active span into padding or tail room.
unsafe impl<F: HoldsDescriptor<Descriptor = NDIlib_video_frame_v2_t>> VideoParts
    for ValidatedVideo<F>
{
    #[inline]
    fn layout(&self) -> &ValidatedVideoLayout {
        &self.validated.0
    }
    #[inline]
    fn header(&self) -> &VideoHeader {
        &self.validated.1
    }
    #[inline]
    fn pixels(&self) -> *const u8 {
        self.descriptor().p_data
    }
    #[inline]
    fn metadata_text(&self) -> Option<MetadataText<'_>> {
        self.metadata()
    }
    fn metadata_state(&self) -> FrameMetadataStatus {
        self.metadata_state.status()
    }

    #[inline]
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
        SdkTimestamp::from_sdk(self.descriptor().timestamp)
    }
}

// SAFETY: audio/returned_audio validate this descriptor's layout and text;
// their unsafe caller guarantees aligned initialized active samples and live
// unchanged storage for F's lifetime. Views and default descriptors borrow
// this holder and address only that validated sample extent and metadata.
unsafe impl<F: HoldsDescriptor<Descriptor = NDIlib_audio_frame_v3_t>> AudioParts
    for ValidatedAudio<F>
{
    #[inline]
    fn layout(&self) -> &ValidatedAudioLayout {
        &self.validated
    }
    #[inline]
    fn audio_timecode(&self) -> Timecode {
        Timecode::from_ticks(self.descriptor().timecode)
    }
    #[inline]
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
        self.frame.audio_timestamp(self.descriptor().timestamp)
    }
    #[inline]
    fn samples(&self) -> *const f32 {
        self.descriptor().p_data.cast()
    }
    #[inline]
    fn metadata_text(&self) -> Option<MetadataText<'_>> {
        self.metadata()
    }
    fn metadata_state(&self) -> FrameMetadataStatus {
        self.metadata_state.status()
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
    pub fn row_stride_bytes(&self) -> usize {
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
        let span = VideoPlaneLayout::contiguous(slice::from_ref(&self.layout))?;
        // SAFETY: Construction ties the allocation to 'a. The span is this
        // plane's rows, adjacent and active, so every byte is initialized and
        // within its checked storage; the final row's padding is excluded.
        Some(unsafe { slice::from_raw_parts(self.data.add(span.offset), span.active_row_bytes) })
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

mod borrowed {
    use super::*;

    #[allow(private_bounds)]
    pub struct VideoRef<'a, S: FrameFree<RawFrame = NDIlib_video_frame_v2_t>> {
        /// The captured frame and the layout, header and metadata text validated
        /// from it once, when it was captured; accessors read the cached values.
        pub(super) frame: ValidatedVideo<Guard<'a, S>>,
    }

    #[allow(private_bounds)]
    pub struct AudioRef<'a, S: FrameFree<RawFrame = NDIlib_audio_frame_v3_t>> {
        /// The captured frame and the layout and metadata text validated from it
        /// once, when it was captured; accessors read the cached values.
        pub(super) frame: ValidatedAudio<Guard<'a, S>>,
    }
}

pub(crate) use borrowed::{AudioRef, VideoRef};

/// A zero-copy video frame captured from a [`Receiver`](crate::Receiver).
///
/// Borrows its receiver and frees the SDK buffer when dropped. It can be moved
/// or shared with scoped worker threads (`Send + Sync`). Converting with
/// `to_owned` copies the active pixels into an owned frame. No pixels are
/// copied by capture itself. For padded buffers, `data()` returns `None`; use
/// `planes()` and each plane's rows to read initialized active pixels.
/// See [`crate::FrameSyncVideoRef`] for clock-driven capture.
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
    /// - Metadata text
    ///
    /// The validated layout, header and text are cached so that accessors
    /// return them without re-computation or unchecked arithmetic.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI instance
    /// and contains a frame populated by the matching SDK capture call.
    pub(crate) unsafe fn new(guard: Guard<'a, S>) -> Result<Self> {
        // SAFETY: the guard holds the one descriptor the capture populated
        // until it frees the frame on drop, and the SDK keeps the pixels and
        // metadata it points to allocated and unchanged until then.
        let frame = unsafe { ValidatedVideo::video(guard)? };
        Ok(Self { frame })
    }

    /// The SDK descriptor of this frame, exactly as its capture delivered it,
    /// for an SDK call that reads a captured frame in place. The guard keeps
    /// every buffer it points to allocated and unchanged for as long as
    /// `self` is borrowed.
    #[cfg(feature = "advanced")]
    pub(crate) fn raw(&self) -> &NDIlib_video_frame_v2_t {
        self.frame.descriptor()
    }
}

// Specialize methods on the public aliases so rustdoc shows their full API
// even though the generic frame core has no public path.
macro_rules! borrowed_video_methods {
    ($name:ident) => {
        impl<'a> $name<'a> {
            /// Borrow all active pixels when contiguous, in SDK plane order.
            ///
            /// Returns `None` if padding separates active rows or planes. Use
            /// [`Self::planes`] / [`Self::plane`] for zero-copy padded access, or
            /// [`Self::to_owned`] for a tightly packed owned buffer. A single packed
            /// row can be borrowed even when its stride includes trailing padding.
            pub fn data(&self) -> Option<&[u8]> {
                self.frame.active_data()
            }

            /// Copy this borrowed frame into an owned `VideoFrame`.
            ///
            /// Copies only active rows into one tightly packed pixel allocation and
            /// copies metadata separately. The owned stride can be smaller than the
            /// SDK stride. Preserves format, timing, aspect ratio, and scan type.
            /// [`Self::copy_into`] makes the same copy into an existing frame,
            /// reusing its allocations.
            ///
            /// The copy cannot fail: capture validated everything it relies on, and
            /// the packed layout of a valid frame is valid.
            pub fn to_owned(&self) -> VideoFrame {
                self.frame.owned_copy()
            }

            /// Copy this borrowed frame into `destination`, reusing its allocations.
            ///
            /// `destination` ends up exactly as [`Self::to_owned`] would have
            /// returned it, whatever it held before: tightly packed active rows,
            /// format, dimensions, frame rate, aspect ratio, scan type, timecode,
            /// timestamp and metadata.
            ///
            /// Its pixel buffer is reused when its capacity holds the packed frame
            /// and its scan-dependent [room](PixelFormat::send_tail_room), and otherwise
            /// replaced by one allocation of exactly that size. Its
            /// metadata string is reused when its capacity holds the new text and
            /// its terminator; otherwise it grows once. With sufficient pixel and
            /// metadata capacity, subsequent copies allocate nothing, including
            /// when sources alternate between absent and present metadata. A
            /// copy without metadata retains the destination's text allocation.
            ///
            /// The copy makes no SDK call. It runs wherever this reference may be
            /// used: a receiver frame can be copied on a scoped worker thread, while a
            /// FrameSync frame stays on its capture thread. The destination is an
            /// ordinary owned frame either way.
            pub fn copy_into(&self, destination: &mut VideoFrame) {
                self.frame.copy_into_owned(destination);
            }
        }
    };
}
borrowed_video_methods!(VideoFrameRef);
borrowed_video_methods!(FrameSyncVideoRef);

impl VideoSource for VideoFrameRef<'_> {}
impl VideoSource for FrameSyncVideoRef<'_> {}

// SAFETY: The validated frame holds a Guard that keeps the SDK allocation
// alive until matching release. Every part delegates to that one validated
// descriptor, whose initialized active pixels/text remain unchanged for the
// borrow. Default descriptors/direct-send checks preserve its storage bounds.
unsafe impl<S: FrameFree<RawFrame = NDIlib_video_frame_v2_t>> VideoParts for VideoRef<'_, S> {
    #[inline]
    fn layout(&self) -> &ValidatedVideoLayout {
        self.frame.layout()
    }
    #[inline]
    fn header(&self) -> &VideoHeader {
        self.frame.header()
    }
    #[inline]
    fn pixels(&self) -> *const u8 {
        self.frame.pixels()
    }
    #[inline]
    fn metadata_text(&self) -> Option<MetadataText<'_>> {
        self.frame.metadata()
    }
    fn metadata_state(&self) -> FrameMetadataStatus {
        self.frame.metadata_state.status()
    }

    #[inline]
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
        self.frame.sdk_timestamp()
    }
}

macro_rules! borrowed_video_accessors {
    ($name:ident, $strategy:ty) => {
        video_accessors! {
            impl['a] $name<'a>;
            debug(<$strategy as FrameFree>::REF_DEBUG_NAME, "active data (bytes)" = payload_len_bytes);
            timecode {}
            line_stride_bytes {
                /// It is the SDK's stride, which can include row padding, cached and
                /// validated when the frame was captured.
            }
            plane {}
            timestamp {
                /// It is `None` when the SDK does not know it.
            }
            images("no_run", "\
 # use grafton_ndi::{Finder, FinderOptions, Ndi, Receiver, ReceiverOptions};
 # use std::time::Duration;
 # let ndi = Ndi::new()?;
 # let finder = Finder::new(&ndi, &FinderOptions::default())?;
 # finder.wait_for_sources(Duration::from_secs(1))?;
 # let Some(source) = finder.current_sources()?.into_iter().next() else { return Ok(()) };
 # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
 # let Some(frame) = receiver.video().capture_ref(Duration::from_secs(5))? else { return Ok(()) };") {
                /// It encodes directly from the borrowed SDK pixels, without first
                /// copying the frame into an owned [`VideoFrame`].
            }
        }
    };
}
borrowed_video_accessors!(VideoFrameRef, VideoKind);
borrowed_video_accessors!(FrameSyncVideoRef, FrameSyncVideoFree);

/// A zero-copy audio frame captured from a [`Receiver`](crate::Receiver).
///
/// Borrows its receiver and frees the SDK buffer when dropped. It can be moved
/// or shared with scoped worker threads (`Send + Sync`). Converting with
/// `to_owned` copies the active samples into an owned frame. No samples are
/// copied by capture itself. For padded channels, `data()` returns `None`; use
/// `channel_data()` or `channels()` to read initialized samples. Every captured
/// audio frame has at least one channel and one sample per channel.
/// See [`crate::FrameSyncAudioRef`] for clock-driven capture.
pub type AudioFrameRef<'rx> = AudioRef<'rx, AudioKind>;

#[allow(private_bounds)]
impl<'a, S: FrameFree<RawFrame = NDIlib_audio_frame_v3_t>> AudioRef<'a, S> {
    /// Validate the frame an RAII guard owns, for either family's `new`.
    ///
    /// Validates the frame layout including:
    /// - Valid audio format (FourCC)
    /// - Non-null, `f32`-aligned data pointer
    /// - Positive sample rate, channel count, and sample count
    /// - Channel stride and buffer size within the `MAX_AUDIO_BYTES` limit
    ///
    /// and the frame metadata. Both are cached so that accessors return
    /// slices without re-computation or unchecked arithmetic. A descriptor
    /// without samples is rejected.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI instance
    /// and contains a frame populated by the matching SDK capture call.
    unsafe fn from_guard(guard: Guard<'a, S>) -> Result<Self> {
        // SAFETY: the guard holds the one descriptor the capture populated
        // until it frees the frame on drop, and the SDK keeps the samples and
        // metadata it points to allocated and unchanged until then.
        let frame = unsafe { ValidatedAudio::audio(guard)? };
        Ok(Self { frame })
    }
}

// Specialize methods on the public aliases so rustdoc shows their full API
// even though the generic frame core has no public path.
macro_rules! borrowed_audio_methods {
    ($name:ident) => {
        impl<'a> $name<'a> {
            /// Get a zero-copy slice when channels are tightly packed.
            ///
            /// Returns `None` for multi-channel buffers with padding between channels.
            /// Use [`channel_data`](Self::channel_data) or [`channels`](Self::channels)
            /// for those buffers.
            pub fn data(&self) -> Option<&[f32]> {
                self.frame.contiguous_samples()
            }

            /// Copy this borrowed frame into an owned `AudioFrame`.
            ///
            /// Copies channel samples into tightly packed planar storage and copies
            /// metadata, allowing the frame to outlive the SDK buffer. Padding is omitted.
            /// Allocates one sample buffer, and one metadata string when present.
            /// [`Self::copy_into`] makes the same copy into an existing frame,
            /// reusing its allocations.
            ///
            /// The copy cannot fail: capture validated everything it relies on.
            pub fn to_owned(&self) -> AudioFrame {
                self.frame.owned_copy()
            }

            /// Copy this borrowed frame into `destination`, reusing its allocations.
            ///
            /// `destination` ends up exactly as [`Self::to_owned`] would have
            /// returned it, whatever it held before: the channels' samples packed
            /// back to back without inter-channel padding, format, sample rate,
            /// channel and sample counts, timecode, timestamp and metadata.
            ///
            /// Its sample buffer is reused when its capacity holds the packed
            /// samples, and otherwise replaced by one allocation of exactly that
            /// size. Its metadata string is reused when its capacity holds the new
            /// text and its terminator; otherwise it grows once. With sufficient
            /// sample and metadata capacity, subsequent copies allocate nothing,
            /// including when sources alternate between absent and present
            /// metadata. A copy without text retains its allocation.
            ///
            /// The copy makes no SDK call. It runs wherever this reference may be
            /// used: a receiver frame can be copied on a scoped worker thread, while a
            /// FrameSync frame stays on its capture thread. The destination is an
            /// ordinary owned frame either way.
            pub fn copy_into(&self, destination: &mut AudioFrame) {
                self.frame.copy_into_owned(destination);
            }
        }
    };
}
borrowed_audio_methods!(AudioFrameRef);
borrowed_audio_methods!(FrameSyncAudioRef);

impl AudioSource for AudioFrameRef<'_> {}
impl AudioSource for FrameSyncAudioRef<'_> {}

// SAFETY: The validated frame holds a Guard that keeps the SDK allocation
// alive until matching release. All parts delegate to that one validated
// descriptor, with aligned initialized active samples and bounded terminated
// text unchanged for the borrow; the default descriptor preserves the bounds.
unsafe impl<S: FrameFree<RawFrame = NDIlib_audio_frame_v3_t>> AudioParts for AudioRef<'_, S> {
    #[inline]
    fn layout(&self) -> &ValidatedAudioLayout {
        self.frame.layout()
    }
    #[inline]
    fn audio_timecode(&self) -> Timecode {
        self.frame.audio_timecode()
    }
    #[inline]
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
        self.frame.sdk_timestamp()
    }
    #[inline]
    fn samples(&self) -> *const f32 {
        self.frame.samples()
    }
    #[inline]
    fn metadata_text(&self) -> Option<MetadataText<'_>> {
        self.frame.metadata()
    }
    fn metadata_state(&self) -> FrameMetadataStatus {
        self.frame.metadata_state.status()
    }
}

macro_rules! borrowed_audio_accessors {
    ($name:ident, $strategy:ty) => {
        audio_accessors! {
            impl['a] $name<'a>;
            debug(<$strategy as FrameFree>::REF_DEBUG_NAME);
            timecode {
                /// FrameSync output without a source timecode carries
                /// [`Timecode::SYNTHESIZE`].
            }
            timestamp {
                /// It is `None` when the SDK does not know it. FrameSync audio
                /// also treats its zero timestamp marker as unknown; that
                /// marker alone does not identify synthesized silence.
            }
            channel_stride_bytes {
                /// It includes any padding between channels.
            }
            channel_data {}
        }
    };
}
borrowed_audio_accessors!(AudioFrameRef, AudioKind);
borrowed_audio_accessors!(FrameSyncAudioRef, FrameSyncAudioFree);

impl<'rx> AudioRef<'rx, AudioKind> {
    /// Create a borrowed audio frame from a receiver RAII guard, validated
    /// as [`from_guard`](Self::from_guard) describes.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI receiver
    /// and contains a frame populated by `NDIlib_recv_capture_v3`.
    pub(crate) unsafe fn new(guard: RecvAudioGuard<'rx>) -> Result<Self> {
        // SAFETY: forwarded to the caller.
        unsafe { Self::from_guard(guard) }
    }
}

impl<'fs> AudioRef<'fs, FrameSyncAudioFree> {
    /// Create a borrowed audio frame from a FrameSync RAII guard, validated
    /// as [`from_guard`](Self::from_guard) describes.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid FrameSync
    /// instance and contains a frame populated by
    /// `NDIlib_framesync_capture_audio_v2` with a capture request.
    pub(crate) unsafe fn new(guard: Guard<'fs, FrameSyncAudioFree>) -> Result<Self> {
        // SAFETY: forwarded to the caller.
        unsafe { Self::from_guard(guard) }
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
    /// The captured frame and its text, validated once when it was captured;
    /// `data()` and `as_bytes()` read the cached length. The empty null frame
    /// has no text.
    frame: ValidatedMetadata<RecvMetadataGuard<'rx>>,
}

impl<'rx> MetadataFrameRef<'rx> {
    /// Create a borrowed metadata frame from an RAII guard.
    ///
    /// # Safety
    ///
    /// The caller must ensure the guard was created from a valid NDI receiver
    /// and contains a frame populated by `NDIlib_recv_capture_v3`.
    pub(crate) unsafe fn new(guard: RecvMetadataGuard<'rx>) -> Result<Self> {
        // SAFETY: the receive guard holds the one descriptor the capture
        // populated until it frees the frame on drop, and the SDK keeps the
        // text it points to allocated and unchanged until then.
        let frame = unsafe { ValidatedMetadata::metadata_frame(guard)? };
        Ok(Self { frame })
    }

    /// Get the timecode.
    pub fn timecode(&self) -> Timecode {
        Timecode::from_ticks(self.frame.descriptor().timecode)
    }

    /// Get a zero-copy view of the metadata text.
    ///
    /// This returns a reference directly into the NDI SDK's buffer.
    /// No allocation or string copying is performed.
    pub fn data(&self) -> &str {
        self.frame.metadata().map_or("", MetadataText::as_str)
    }

    /// Get a zero-copy view of the metadata UTF-8 payload bytes, excluding the
    /// SDK trailing NUL terminator.
    pub fn as_bytes(&self) -> &[u8] {
        self.data().as_bytes()
    }

    /// Convert this borrowed frame to an owned `MetadataFrame`.
    ///
    /// This performs a string copy, allowing the frame to outlive
    /// the NDI buffer and be sent across threads.
    pub fn to_owned(&self) -> MetadataFrame {
        MetadataFrame::copied(self.frame.metadata(), self.timecode())
    }

    /// Copy text and timecode into an existing owned frame. Reuses its text
    /// allocation when its capacity holds the text and trailing NUL.
    pub fn copy_into(&self, destination: &mut MetadataFrame) {
        MetadataSource::copy_into(self, destination);
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

// SAFETY: The validated metadata frame and its Guard retain the SDK's bounded
// initialized terminated UTF-8 until matching release. The default descriptor
// borrows only that text, which remains unchanged for the reference's borrow.
unsafe impl MetadataParts for MetadataFrameRef<'_> {
    fn metadata_text_for_send(&self) -> Option<MetadataText<'_>> {
        self.frame.metadata()
    }

    fn metadata_timecode(&self) -> Timecode {
        self.timecode()
    }
}

impl MetadataSource for MetadataFrameRef<'_> {}

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

    /// The packed layout of every valid layout, padded or not, is the one
    /// validation gives the same format and dimensions without padding, so
    /// an owned copy, which packs, cannot meet a layout validation refuses.
    #[test]
    fn every_valid_layout_packs_into_the_validated_packed_layout() {
        for format in PixelFormat::ALL {
            let bytes = format.info().bytes_per_pixel() as usize;
            let mut packed_layouts = 0;
            for (width, height) in [
                (2u32, 1u32),
                (2, 2),
                (642, 4),
                (1920, 1080),
                (3840, 2160),
                (16_384, 8_192),
            ] {
                for padding in [0, 2, 64, 4096] {
                    let stride = width as usize * bytes + padding;
                    let Ok(layout) =
                        ValidatedVideoLayout::new_uncompressed(format, width, height, Some(stride))
                    else {
                        continue;
                    };
                    let expected =
                        ValidatedVideoLayout::new_uncompressed(format, width, height, None)
                            .unwrap();
                    let packed = layout.packed();
                    assert_eq!(format!("{packed:?}"), format!("{expected:?}"));
                    assert_eq!(packed.data_len_bytes, layout.payload_len_bytes);
                    packed_layouts += 1;
                }
            }
            assert!(packed_layouts > 0, "{format:?}");
        }
    }

    /// A frame's `data` and each plane's follow one contiguity rule: active
    /// rows are one span when no padding separates the rows of a plane or
    /// the planes, and a single row may have padding after it. Audio is one
    /// span with one channel or with channels back to back.
    #[test]
    fn frames_planes_and_audio_share_one_contiguity_rule() {
        let rows_adjacent = |plane: &VideoPlaneLayout| {
            plane.row_count == 1 || plane.row_stride == plane.active_row_bytes
        };
        let storage = vec![7u8; 4096];
        for format in PixelFormat::ALL {
            let bytes = format.info().bytes_per_pixel() as usize;
            for (height, padding) in [(1u32, 0usize), (1, 8), (2, 0), (2, 8), (4, 2), (4, 64)] {
                let Ok(layout) = ValidatedVideoLayout::new_uncompressed(
                    format,
                    4,
                    height,
                    Some(4 * bytes + padding),
                ) else {
                    continue;
                };
                let planes = layout.planes();
                let (last, before) = planes.split_last().unwrap();
                let contiguous = before
                    .iter()
                    .all(|plane| plane.row_stride == plane.active_row_bytes)
                    && rows_adjacent(last);
                // SAFETY: the storage is initialized and covers every layout
                // here, and nothing writes it while the views live.
                let data = unsafe { layout.view_data(storage.as_ptr()) };
                assert_eq!(
                    data.map(<[u8]>::len),
                    contiguous.then_some(layout.payload_len_bytes),
                    "{format:?} height {height} padding {padding}"
                );
                for (index, plane) in planes.iter().enumerate() {
                    // SAFETY: as above.
                    let view = unsafe { layout.view_plane(storage.as_ptr(), index) }.unwrap();
                    assert_eq!(
                        view.data().map(<[u8]>::len),
                        rows_adjacent(plane).then_some(plane.row_count * plane.active_row_bytes)
                    );
                }
            }
        }

        let samples = [0.5f32; 24];
        for (channels, stride, contiguous) in [(1, 8, true), (3, 5, true), (3, 8, false)] {
            let raw = NDIlib_audio_frame_v3_t {
                sample_rate: 48_000,
                no_channels: channels,
                no_samples: 5,
                FourCC: AudioFormat::Fltp.to_sdk(),
                p_data: samples.as_ptr().cast_mut().cast(),
                __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                    channel_stride_in_bytes: stride * 4,
                },
                timecode: 0,
                timestamp: 0,
                p_metadata: ptr::null(),
            };
            let layout = validate_audio_layout(&raw).unwrap();
            // SAFETY: the samples are initialized, aligned and cover the
            // layout, and nothing writes them while the view lives.
            let view = unsafe { layout.view_samples(samples.as_ptr()) };
            assert_eq!(
                view.map(<[f32]>::len),
                contiguous.then_some(layout.sample_count)
            );
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
            let captured = frame.to_owned();
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
                let recopy = owned.clone();
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
            let request = crate::FrameSyncAudioRequest::capture_at(48_000, channels, samples);
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
                        assert_eq!(plane.row_stride_bytes(), stride * active / dimensions[0].0);
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
                                    .add(offsets[i] + row_index * plane.row_stride_bytes())
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
                assert_eq!(VideoParts::layout(&frame).data_len_bytes, offset);
                assert_eq!(VideoParts::layout(&frame).payload_len_bytes, expected.len());
                let owned = frame.to_owned();
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
                    .line_stride_bytes(stride)
                    .metadata(&metadata)
                    .build()
                    .unwrap();
                let normalized = send.to_owned();
                assert_eq!(normalized.data(), expected);
                assert_eq!(normalized.timecode(), Timecode::from_ticks(12345));
                // A borrowed frame carries no timestamp.
                assert_eq!(normalized.timestamp(), None);
                assert_eq!(normalized.line_stride_bytes(), copied.line_stride_bytes());
                assert_eq!(normalized.metadata(), Some("<video-padding/>"));
                // Copies into existing frames pack the same rows from the
                // FrameSync view, over uninitialized padding, and from padded
                // borrowed frames with and without metadata.
                let bare = crate::BorrowedVideoFrame::builder(&initialized, 4, 4, format)
                    .line_stride_bytes(stride)
                    .build()
                    .unwrap();
                let sources: [&dyn CopySource<VideoFrame>; 3] = [&second, &send, &bare];
                for source in sources {
                    assert_eq!(source.owned().data(), expected);
                    assert_copies_reuse(source, video_destinations("<video-padding/>".len()));
                }
            }
        }
    }

    #[test]
    fn borrowed_video_planes_borrow_only_the_active_rows_of_a_padded_buffer() {
        fn rows_of(planes: VideoPlanes<'_>) -> Vec<Vec<Vec<u8>>> {
            planes
                .map(|plane| plane.rows().map(<[u8]>::to_vec).collect())
                .collect()
        }
        const PADDING: u8 = 0xEE;
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
                // Each row's active bytes followed by its padding, which holds
                // a marker; the expected rows hold the active bytes alone.
                let mut buffer = Vec::new();
                let mut expected = Vec::new();
                let mut row_offsets = Vec::new();
                for (i, &(active, rows)) in dimensions.iter().enumerate() {
                    let plane_stride = stride * active / dimensions[0].0;
                    let mut plane_rows = Vec::new();
                    let mut plane_offsets = Vec::new();
                    for row in 0..rows {
                        let values: Vec<u8> = (0..active)
                            .map(|byte| (i * 71 + row * 17 + byte) as u8)
                            .collect();
                        plane_offsets.push(buffer.len());
                        buffer.extend_from_slice(&values);
                        buffer.resize(buffer.len() + plane_stride - active, PADDING);
                        plane_rows.push(values);
                    }
                    expected.push(plane_rows);
                    row_offsets.push(plane_offsets);
                }
                let frame = crate::BorrowedVideoFrame::builder(&buffer, 4, 4, format)
                    .line_stride_bytes(stride)
                    .build()
                    .unwrap();
                // `data` is the whole padded storage; the views see past it.
                assert_eq!(frame.data(), buffer);
                let ((), allocations) = crate::test_alloc::allocation_count(|| {
                    assert_eq!(frame.planes().len(), dimensions.len());
                    assert!(frame.plane(dimensions.len()).is_none());
                    for (i, plane) in frame.planes().enumerate() {
                        let (active, rows) = dimensions[i];
                        assert_eq!(plane.row_stride_bytes(), stride * active / dimensions[0].0);
                        assert_eq!(plane.active_row_bytes(), active);
                        assert_eq!(plane.row_count(), rows);
                        assert_eq!(plane.data().is_some(), padding == 0);
                        assert!(plane.row(rows).is_none());
                        let indexed = frame.plane(i).unwrap();
                        for (row_index, row) in plane.rows().enumerate() {
                            assert_eq!(row, expected[i][row_index]);
                            assert_eq!(indexed.row(row_index), Some(row));
                            // Borrowed in place, at the row's start in the buffer.
                            assert_eq!(row.as_ptr(), buffer[row_offsets[i][row_index]..].as_ptr());
                        }
                    }
                });
                assert_eq!(allocations, 0);
                assert_eq!(rows_of(frame.planes()), expected);
                // The owned copy holds the same rows, packed without padding,
                // and a frame borrowing that copy views them unchanged.
                let owned = frame.to_owned();
                assert_eq!(rows_of(owned.planes()), expected);
                assert_eq!(rows_of(owned.planes()), expected);
                assert!(owned.planes().all(|plane| plane.data().is_some()));
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
            assert!(VideoPlaneLayout::contiguous(tight.planes()).is_some());
            let active = format.line_stride_bytes(2).unwrap();
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
            assert!(
                !frame.sends_in_place(),
                "padded {format:?} capture must stage"
            );
            assert!(frame.to_owned().sends_in_place());
            for plane in frame.planes() {
                assert_eq!(plane.data().unwrap(), vec![42; plane.active_row_bytes()]);
            }
            assert_eq!(frame.to_owned().data(), vec![42; tight.payload_len_bytes]);
        }
        for format in [PixelFormat::Yv12, PixelFormat::I420, PixelFormat::Nv12] {
            let tight = ValidatedVideoLayout::new_uncompressed(format, 4, 2, Some(0)).unwrap();
            assert!(VideoPlaneLayout::contiguous(tight.planes()).is_some());
            let padded = ValidatedVideoLayout::new_uncompressed(format, 4, 2, Some(8)).unwrap();
            assert!(VideoPlaneLayout::contiguous(padded.planes()).is_none()); // Chroma is one row, but plane gaps remain.
        }
    }

    /// Encoders read active bytes only: every format's padding is left
    /// uninitialized, which Miri and Memcheck would report if read.
    #[cfg(feature = "image-encoding")]
    #[test]
    fn video_padding_encoders_never_read_uninitialized_gaps() {
        use crate::{
            capture::{FrameSyncVideoFree, Guard},
            AlphaMode, Downscale, ImageFormat, ImageOptions, SampleDepth,
        };
        use std::mem::MaybeUninit;
        let outputs = [
            ImageOptions::new(ImageFormat::Png),
            ImageOptions::new(ImageFormat::Png).depth(SampleDepth::Sixteen),
            ImageOptions::new(ImageFormat::Png).downscale(Downscale::Half),
            ImageOptions::new(ImageFormat::Jpeg(85)),
            ImageOptions::new(ImageFormat::WebpLossless).alpha(AlphaMode::Discard),
            ImageOptions::new(ImageFormat::WebpLossy(75)),
            ImageOptions::new(ImageFormat::WebpLossy(75)).alpha(AlphaMode::Discard),
        ];
        for format in PixelFormat::ALL {
            let (width, height) = (4, 2);
            let packed =
                ValidatedVideoLayout::new_uncompressed(format, width, height, None).unwrap();
            let stride = packed.line_stride_bytes + 8;
            let padded =
                ValidatedVideoLayout::new_uncompressed(format, width, height, Some(stride))
                    .unwrap();
            let mut storage = vec![MaybeUninit::<u8>::uninit(); padded.data_len_bytes];
            let mut active = Vec::new();
            let mut value = 7u8;
            for plane in padded.planes() {
                for row in 0..plane.row_count {
                    for byte in 0..plane.active_row_bytes {
                        value = value.wrapping_mul(31).wrapping_add(17);
                        storage[plane.offset + row * plane.row_stride + byte].write(value);
                        active.push(value);
                    }
                }
            }
            let raw = NDIlib_video_frame_v2_t {
                xres: width as i32,
                yres: height as i32,
                FourCC: FourCc::from(format).raw(),
                frame_rate_N: 30,
                frame_rate_D: 1,
                frame_format_type: ScanType::Progressive.to_sdk(),
                p_data: storage.as_mut_ptr().cast(),
                __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                    line_stride_in_bytes: stride as i32,
                },
                ..Default::default()
            };
            // SAFETY: the descriptor points into `storage`, which outlives
            // the reference, and the no-op free releases nothing.
            let borrowed = unsafe {
                VideoRef::new(Guard::<FrameSyncVideoFree>::with_test_free(
                    ptr::null_mut(),
                    raw,
                    |_, _| {},
                ))
            }
            .unwrap();
            let owned = VideoFrame::builder()
                .resolution(width, height)
                .pixel_format(format)
                .data(active.clone())
                .build()
                .unwrap();
            for options in &outputs {
                assert_eq!(
                    borrowed.encode_image(options).unwrap(),
                    owned.encode_image(options).unwrap(),
                    "{format:?} {options:?}"
                );
            }
            assert_eq!(borrowed.to_owned().data(), active);
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
        let owned = frame.to_owned();
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
        assert_eq!(built.picture_aspect_ratio(), 0.0);
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
        let raw = frame.as_test_raw();
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
                .line_stride_bytes(stride)
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

    /// Test PixelFormat::line_stride_bytes for all formats
    #[test]
    fn test_pixel_format_line_stride() {
        // Packed RGB: 4 bytes per pixel
        assert_eq!(PixelFormat::Bgra.line_stride_bytes(1920).unwrap(), 7680);
        assert_eq!(PixelFormat::Bgrx.line_stride_bytes(1920).unwrap(), 7680);
        assert_eq!(PixelFormat::Rgba.line_stride_bytes(1920).unwrap(), 7680);
        assert_eq!(PixelFormat::Rgbx.line_stride_bytes(1920).unwrap(), 7680);

        // UYVY: 2 bytes per pixel
        assert_eq!(PixelFormat::Uyvy.line_stride_bytes(1920).unwrap(), 3840);

        // UYVA: UYVY plane stride = 2 bytes per pixel
        assert_eq!(PixelFormat::Uyva.line_stride_bytes(1920).unwrap(), 3840);

        // P216/PA16: Y-plane stride = 2 bytes per pixel
        assert_eq!(PixelFormat::P216.line_stride_bytes(1920).unwrap(), 3840);
        assert_eq!(PixelFormat::Pa16.line_stride_bytes(1920).unwrap(), 3840);

        // Planar 4:2:0: Y-plane stride = 1 byte per pixel
        assert_eq!(PixelFormat::Yv12.line_stride_bytes(1920).unwrap(), 1920);
        assert_eq!(PixelFormat::I420.line_stride_bytes(1920).unwrap(), 1920);
        assert_eq!(PixelFormat::Nv12.line_stride_bytes(1920).unwrap(), 1920);
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
                    format.line_stride_bytes(width).is_err(),
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
            assert!(format.line_stride_bytes(6).is_ok());
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
            .build()
            .map_err(Rejected::into_error);

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
        use crate::capture::RecvAudioGuard;

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
        use crate::capture::RecvAudioGuard;

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

    /// The receiver and FrameSync video refs agree: given byte-identical raw
    /// frames, every shared accessor, `scan_type` included, returns identical
    /// results, as both spellings share one [`VideoFrameRef`] core.
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
            // A non-default scan type, which a ref that falls back to
            // `Progressive` would report differently.
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

        assert!(layout.is_none());
    }

    #[test]
    fn test_validate_metadata_layout_accepts_one_byte_empty_payload() {
        let mut data = vec![0u8];
        let raw = metadata_raw_from_bytes(&mut data, 1);
        let layout =
            unsafe { validate_metadata_layout(&raw) }.expect("explicit empty payload is valid");

        assert_eq!(layout.map(MetadataText::as_str), Some(""));
        assert_eq!(
            layout.map(MetadataText::as_ptr),
            Some(raw.p_data.cast_const())
        );
    }

    #[test]
    fn test_validate_metadata_layout_accepts_utf8_with_bounded_length() {
        let mut data = "<ndi tally=\"preview\"/>".as_bytes().to_vec();
        data.push(0);
        let length = data.len() as i32;
        let raw = metadata_raw_from_bytes(&mut data, length);
        let layout = unsafe { validate_metadata_layout(&raw) }.expect("valid UTF-8 metadata");

        // The SDK length includes the terminator; the text excludes it.
        assert_eq!(
            layout.map(MetadataText::as_str),
            Some("<ndi tally=\"preview\"/>")
        );
        assert_eq!(layout.map(MetadataText::len_with_nul), Some(data.len()));
    }

    #[test]
    fn test_validate_metadata_layout_ignores_bytes_after_length() {
        let mut data = b"ok\0\0\xFF".to_vec();
        let raw = metadata_raw_from_bytes(&mut data, 3);
        let layout =
            unsafe { validate_metadata_layout(&raw) }.expect("extra bytes after length ignored");

        assert_eq!(layout.map(MetadataText::as_str), Some("ok"));
        let owned = MetadataFrame::copied(layout, Timecode::from_ticks(raw.timecode));
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
        assert_eq!(layout.map(MetadataText::as_str), Some("<ndi/>"));
        let owned = MetadataFrame::copied(layout, Timecode::from_ticks(raw.timecode));
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
    fn sender_metadata_uses_terminated_text_with_different_reported_counts() {
        for text in ["<ascii/>", "<payload>é🎥</payload>", ""] {
            let mut bytes = text.as_bytes().to_vec();
            bytes.push(0);
            for count in [0, text.len(), text.len() + 1, text.chars().count() + 1] {
                let raw = metadata_raw_from_bytes(&mut bytes, count as i32);
                // SAFETY: bytes retains the full NUL-terminated string,
                // independent of the descriptor's reported count.
                let actual = unsafe { validate_sender_metadata(&raw) }.unwrap().unwrap();
                assert_eq!(actual.as_str(), text);
                assert_eq!(actual.as_ptr(), raw.p_data.cast_const());
                assert_eq!(actual.len_with_nul(), bytes.len());
                assert_eq!(
                    raw.length, count as i32,
                    "the SDK free descriptor is unchanged"
                );
            }
        }
    }

    #[test]
    fn sender_metadata_rejects_invalid_fields_utf8_and_unbounded_text() {
        // SAFETY: null pointers are checked without reading, and zero means
        // the documented empty-null metadata representation.
        assert!(unsafe { validate_sender_metadata(&null_metadata_raw(0)) }
            .unwrap()
            .is_none());
        for count in [-1, 1, i32::MAX] {
            assert!(matches!(
                unsafe { validate_sender_metadata(&null_metadata_raw(count)) },
                Err(Error::InvalidFrame(_)),
            ));
        }
        let mut empty = [0];
        let excessive = metadata_raw_from_bytes(&mut empty, (MAX_METADATA_BYTES + 1) as i32);
        // SAFETY: the pointed-to C string is complete, even though its
        // descriptor's excessive count is rejected before scanning it.
        assert!(matches!(
            unsafe { validate_sender_metadata(&excessive) },
            Err(Error::InvalidFrame(_))
        ));

        let mut invalid = [0xff, 0];
        let raw = metadata_raw_from_bytes(&mut invalid, 1);
        // SAFETY: full NUL-terminated storage is retained; UTF-8 is checked.
        assert!(matches!(
            unsafe { validate_sender_metadata(&raw) },
            Err(Error::InvalidUtf8(_))
        ));

        let mut boundary = vec![b'x'; MAX_METADATA_BYTES];
        boundary[MAX_METADATA_BYTES - 1] = 0;
        let raw = metadata_raw_from_bytes(&mut boundary, 1);
        // SAFETY: storage covers the entire string through its terminator.
        assert_eq!(
            unsafe { validate_sender_metadata(&raw) }
                .unwrap()
                .unwrap()
                .len_with_nul(),
            MAX_METADATA_BYTES
        );
        boundary[MAX_METADATA_BYTES - 1] = b'x';
        let raw = metadata_raw_from_bytes(&mut boundary, 1);
        // SAFETY: the entire capped scan is readable even without a terminator.
        assert!(matches!(
            unsafe { validate_sender_metadata(&raw) },
            Err(Error::InvalidFrame(_))
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
                let raw = frame.metadata_raw();
                assert_eq!(raw.p_data, expected);
                assert_eq!(raw.length, 1101);
                assert_eq!(frame.data().len(), 1100);
            }
        });
        assert_eq!(allocations, 0);

        let (copy, allocations) = crate::test_alloc::allocation_count(|| {
            FrameMetadata::copy_from(frame.data.text(), None)
        });
        assert_eq!(allocations, 1);
        assert_eq!(copy.as_str(), frame.data());
        let (text, allocations) = crate::test_alloc::allocation_count(|| copy.into_string());
        assert_eq!(allocations, 0);
        assert_eq!(text, frame.data());
    }

    #[test]
    fn metadata_construction_retains_owned_storage() {
        fn text() -> String {
            let mut text = String::with_capacity(1024);
            text.push_str("<metadata label='é'/>");
            text
        }
        let supplied = text();
        let (pointer, capacity) = (supplied.as_ptr(), supplied.capacity());
        let (metadata, allocations) =
            crate::test_alloc::allocation_count(|| MetadataFrame::new(supplied).unwrap());
        assert_eq!(allocations, 0);
        assert_eq!(metadata.data().as_ptr(), pointer);
        assert_eq!(metadata.into_data().capacity(), capacity);

        let supplied = text();
        let (pointer, capacity) = (supplied.as_ptr(), supplied.capacity());
        let builder = VideoFrame::builder()
            .resolution(2, 2)
            .data(vec![7; 16])
            .metadata(supplied);
        let (video, allocations) = crate::test_alloc::allocation_count(|| builder.build().unwrap());
        assert_eq!(allocations, 0);
        assert_eq!(video.metadata().unwrap().as_ptr(), pointer);
        assert_eq!(video.metadata.inner.capacity(), capacity);

        let supplied = text();
        let (pointer, capacity) = (supplied.as_ptr(), supplied.capacity());
        let builder = AudioFrame::builder()
            .num_channels(2)
            .num_samples(2)
            .data(vec![0.25; 4])
            .metadata(supplied);
        let (audio, allocations) = crate::test_alloc::allocation_count(|| builder.build().unwrap());
        assert_eq!(allocations, 0);
        assert_eq!(audio.metadata().unwrap().as_ptr(), pointer);
        assert_eq!(audio.metadata.inner.capacity(), capacity);
    }

    #[test]
    fn rejected_metadata_keeps_builder_text_and_sample_storage() {
        for text in ["bad\0metadata".to_owned(), "x".repeat(MAX_METADATA_BYTES)] {
            let video_text = text.clone();
            let pointer = video_text.as_ptr();
            let mut pixels = Vec::with_capacity(64);
            pixels.resize(16, 7);
            let pixels_pointer = pixels.as_ptr();
            let builder = VideoFrame::builder()
                .resolution(2, 2)
                .data(pixels)
                .metadata(video_text)
                .build()
                .unwrap_err()
                .into_inner();
            assert_eq!(builder.metadata.as_deref(), Some(text.as_str()));
            assert_eq!(builder.metadata.as_ref().unwrap().as_ptr(), pointer);
            assert_eq!(builder.supplied_data().unwrap().as_ptr(), pixels_pointer);

            let audio_text = text.clone();
            let pointer = audio_text.as_ptr();
            let samples = vec![1.0, 3.0, 2.0, 4.0];
            let samples_pointer = samples.as_ptr();
            let builder = AudioFrame::builder()
                .num_channels(2)
                .num_samples(2)
                .layout(AudioLayout::Interleaved)
                .data(samples)
                .metadata(audio_text)
                .build()
                .unwrap_err()
                .into_inner();
            assert_eq!(builder.metadata.as_deref(), Some(text.as_str()));
            assert_eq!(builder.metadata.as_ref().unwrap().as_ptr(), pointer);
            assert_eq!(builder.supplied_data().unwrap().as_ptr(), samples_pointer);
            assert_eq!(builder.supplied_data().unwrap(), [1.0, 3.0, 2.0, 4.0]);
        }
    }

    #[test]
    fn metadata_copies_reuse_storage_across_lengths() {
        let sources = ["same", "é", "", "longer text", "same"].map(|text| {
            MetadataFrame::new(text)
                .unwrap()
                .with_timecode(Timecode::from_ticks(42))
        });
        let mut destination = MetadataFrame::new("x".repeat(64)).unwrap();
        let pointer = destination.data().as_ptr();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for source in &sources {
                let source: &dyn MetadataSource = source;
                source.copy_into(&mut destination);
                assert_eq!(destination.data(), source.data());
                assert_eq!(destination.timecode(), source.timecode());
                assert_eq!(destination.data().as_ptr(), pointer);
                let raw = destination.metadata_raw();
                assert_eq!(raw.length as usize, source.data().len() + 1);
                // SAFETY: the descriptor borrows the live, validated destination.
                assert_eq!(
                    unsafe { CStr::from_ptr(raw.p_data) }.to_bytes(),
                    source.as_bytes()
                );
            }
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn media_copies_reuse_metadata_storage_across_lengths() {
        let pixels = [7; 16];
        let samples = [0.25; 4];
        let mut video = VideoFrame::builder()
            .resolution(2, 2)
            .metadata("x".repeat(64))
            .build()
            .unwrap();
        let mut audio = AudioFrame::builder()
            .num_channels(2)
            .num_samples(2)
            .metadata("x".repeat(64))
            .build()
            .unwrap();
        let video_pointer = video.data().as_ptr();
        let audio_pointer = audio.data().as_ptr();
        let video_text = video.metadata().unwrap().as_ptr();
        let audio_text = audio.metadata().unwrap().as_ptr();
        #[cfg(feature = "advanced")]
        let retained = video.retained_bytes();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for text in [c"same", c"é", c"", c"longer text", c"same"] {
                let source = crate::BorrowedVideoFrame::builder(&pixels, 2, 2, PixelFormat::Bgra)
                    .metadata(text)
                    .timecode(Timecode::from_ticks(42))
                    .build()
                    .unwrap();
                VideoSource::copy_into(&source, &mut video);
                let source = BorrowedAudioFrame::builder(&samples, 48_000, 2, 2)
                    .metadata(text)
                    .timecode(Timecode::from_ticks(42))
                    .build()
                    .unwrap();
                AudioSource::copy_into(&source, &mut audio);
                let text = text.to_str().unwrap();
                assert_eq!(video.metadata(), Some(text));
                assert_eq!(audio.metadata(), Some(text));
                assert_eq!(video.metadata().unwrap().as_ptr(), video_text);
                assert_eq!(audio.metadata().unwrap().as_ptr(), audio_text);
                assert_eq!(video.data().as_ptr(), video_pointer);
                assert_eq!(audio.data().as_ptr(), audio_pointer);
                assert_eq!(video.data(), pixels);
                assert_eq!(audio.data(), samples);
                assert_eq!(video.timecode().ticks(), 42);
                assert_eq!(audio.timecode().ticks(), 42);
                #[cfg(feature = "advanced")]
                assert_eq!(video.retained_bytes(), retained);
            }
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn metadata_storage_grows_once_then_keeps_capacity() {
        let larger = MetadataFrame::new("é".repeat(64)).unwrap();
        let smaller = MetadataFrame::new("").unwrap();
        let mut destination = MetadataFrame::new("x").unwrap();
        let ((), allocations) =
            crate::test_alloc::allocation_count(|| larger.copy_into(&mut destination));
        assert_eq!(allocations, 1);
        let pointer = destination.data().as_ptr();
        let capacity = destination.data.inner.capacity();
        let (text, allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..10 {
                smaller.copy_into(&mut destination);
                larger.copy_into(&mut destination);
                assert_eq!(destination.data().as_ptr(), pointer);
                assert_eq!(destination.data.inner.capacity(), capacity);
            }
            destination.into_data()
        });
        assert_eq!(allocations, 0);
        assert_eq!(text, larger.data());
        assert_eq!(text.as_ptr(), pointer);
        assert_eq!(text.capacity(), capacity);
    }

    #[test]
    fn test_metadata_frame_copy_holds_only_payload() {
        let mut data = b"copy-me\0\0\xFF".to_vec();
        let raw = metadata_raw_from_bytes(&mut data, 8);
        let layout = unsafe { validate_metadata_layout(&raw) }.unwrap();
        let frame = MetadataFrame::copied(layout, Timecode::from_ticks(raw.timecode));

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
        let raw = frame.metadata_raw();

        assert_eq!(raw.length, 4);
        assert_eq!(raw.timecode, 101);
        assert_eq!(raw.p_data, frame.data.as_ptr() as *mut c_char);
        assert_eq!(frame.data.inner.as_bytes(), b"abc\0");
    }

    #[test]
    fn test_empty_metadata_frame_to_raw_sends_explicit_nul() {
        let frame = MetadataFrame::new("").unwrap();
        let raw = frame.metadata_raw();

        assert_eq!(raw.length, 1);
        assert_eq!(raw.timecode, NDIlib_send_timecode_synthesize);
        assert_eq!(frame.data.inner.as_bytes(), b"\0");
    }

    #[test]
    fn captured_metadata_source_reborrows_text_without_allocation() {
        use crate::capture::RecvMetadataGuard;

        let mut bytes = b"<relay>\xc3\xa9</relay>\0".to_vec();
        let length = bytes.len() as i32;
        let raw = metadata_raw_from_bytes(&mut bytes, length);
        let guard = unsafe { RecvMetadataGuard::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let captured = unsafe { MetadataFrameRef::new(guard) }.unwrap();
        let source: &dyn MetadataSource = &captured;
        let (descriptor, allocations) = crate::test_alloc::allocation_count(|| {
            assert_eq!(source.data(), "<relay>é</relay>");
            assert_eq!(source.as_bytes(), "<relay>é</relay>".as_bytes());
            assert_eq!(source.timecode().ticks(), raw.timecode);
            source.metadata_raw()
        });
        assert_eq!(allocations, 0);
        assert_eq!(descriptor.p_data, raw.p_data);
        assert_eq!(descriptor.length, length);
        assert_eq!(descriptor.timecode, raw.timecode);

        let mut destination = MetadataFrame::new("x".repeat(bytes.len() - 1)).unwrap();
        let pointer = destination.data().as_ptr();
        let ((), allocations) =
            crate::test_alloc::allocation_count(|| source.copy_into(&mut destination));
        assert_eq!(allocations, 0);
        assert_eq!(destination.data().as_ptr(), pointer);
        assert_eq!(destination.data(), source.data());
        assert_eq!(destination.timecode(), source.timecode());
        assert_eq!(MetadataSource::to_owned(source), destination);
    }

    #[test]
    fn captured_empty_metadata_source_sends_a_terminated_empty_payload() {
        use crate::capture::RecvMetadataGuard;

        let raw = null_metadata_raw(0);
        let guard = unsafe { RecvMetadataGuard::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let captured = unsafe { MetadataFrameRef::new(guard) }.unwrap();
        let source: &dyn MetadataSource = &captured;
        let outgoing = source.metadata_raw();
        assert_eq!(outgoing.length, 1);
        assert_eq!(outgoing.timecode, raw.timecode);
        assert!(!outgoing.p_data.is_null());
        assert_eq!(unsafe { CStr::from_ptr(outgoing.p_data) }, c"");
        assert_eq!(source.to_owned().data(), "");
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
        // The text is borrowed in place, with the length the SDK declared.
        assert_eq!(frame_ref.data().as_ptr(), data.as_ptr());
        assert_eq!(
            frame_ref.frame.metadata().map(MetadataText::len_with_nul),
            Some(10)
        );

        assert_eq!(frame_ref.timecode(), Timecode::from_ticks(12345));
        let owned = frame_ref.to_owned();
        assert_eq!(owned.data(), "zero-copy");
        assert_eq!(owned.timecode(), Timecode::from_ticks(12345));
    }

    #[test]
    fn test_empty_null_frame_and_empty_sentinel_both_read_as_empty() {
        use crate::capture::RecvMetadataGuard;

        // The empty null frame is absent text: nothing is read from `p_data`.
        let raw = null_metadata_raw(0);
        let guard = unsafe { RecvMetadataGuard::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let frame_ref = unsafe { MetadataFrameRef::new(guard) }.expect("empty null frame");
        assert!(frame_ref.frame.metadata().is_none());
        assert_eq!(frame_ref.data(), "");
        assert_eq!(frame_ref.as_bytes(), b"");
        let owned = frame_ref.to_owned();
        assert_eq!(owned.data(), "");
        assert_eq!(owned.timecode(), Timecode::from_ticks(12345));
        assert_eq!(owned.data.inner.as_bytes(), b"\0");

        // A zero length with a non-null empty C string is present, empty text.
        let mut data = b"\0ignored".to_vec();
        let raw = metadata_raw_from_bytes(&mut data, 0);
        let guard = unsafe { RecvMetadataGuard::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let frame_ref = unsafe { MetadataFrameRef::new(guard) }.expect("empty C-string sentinel");
        assert_eq!(
            frame_ref.frame.metadata().map(MetadataText::len_with_nul),
            Some(1)
        );
        assert_eq!(frame_ref.data(), "");
        assert_eq!(frame_ref.as_bytes(), b"");
        assert_eq!(frame_ref.to_owned().data(), "");
    }

    #[test]
    fn test_bounded_sdk_text_stops_at_its_limit() {
        let data = b"abc\0\xFF";
        let text = data.as_ptr().cast::<c_char>();

        // SAFETY: each scan stays within the live array: it ends at the NUL
        // at index 3, or at a smaller limit.
        unsafe {
            assert_eq!(
                bounded_sdk_text(text, MAX_METADATA_BYTES),
                Some(&b"abc"[..])
            );
            // A terminator at the last index within the limit is found.
            assert_eq!(bounded_sdk_text(text, 4), Some(&b"abc"[..]));
            // A terminator at the limit itself is not.
            assert_eq!(bounded_sdk_text(text, 3), None);
            assert_eq!(bounded_sdk_text(text, 0), None);
            assert_eq!(bounded_sdk_text(c"".as_ptr(), 1), Some(&b""[..]));
        }
    }

    #[test]
    fn test_validate_frame_metadata_accepts_null_without_allocation_state() {
        let layout = unsafe { validate_frame_metadata(ptr::null()) }.expect("null is valid");

        assert!(layout.is_none());
    }

    #[test]
    fn test_validate_frame_metadata_accepts_explicit_empty() {
        let mut data = b"\0".to_vec();
        let layout = unsafe { validate_frame_metadata(data.as_mut_ptr().cast::<c_char>()) }
            .expect("explicit empty metadata is valid");

        let text = layout.expect("explicit empty text");
        assert_eq!(text.as_str(), "");
        assert_eq!(text.as_ptr(), data.as_ptr().cast::<c_char>());
    }

    #[test]
    fn test_validate_frame_metadata_accepts_utf8_and_ignores_after_first_nul() {
        let mut data = b"hello metadata\0\0\xFF".to_vec();
        let layout = unsafe { validate_frame_metadata(data.as_mut_ptr().cast::<c_char>()) }
            .expect("valid UTF-8 metadata");

        let text = layout.expect("text");
        assert_eq!(text.as_str(), "hello metadata");
        assert_eq!(text.len_with_nul(), "hello metadata".len() + 1);
        assert_eq!(text.as_ptr(), data.as_ptr().cast::<c_char>());
    }

    #[test]
    fn test_validate_frame_metadata_accepts_max_boundary() {
        let mut data = vec![b'x'; MAX_METADATA_BYTES];
        data[MAX_METADATA_BYTES - 1] = 0;

        let layout = unsafe { validate_frame_metadata(data.as_mut_ptr().cast::<c_char>()) }
            .expect("metadata exactly at cap including terminator is valid");

        // Exactly `MAX_METADATA_BYTES` including the terminator.
        assert_eq!(
            layout.map(MetadataText::len_with_nul),
            Some(MAX_METADATA_BYTES)
        );
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

        let owned = frame_ref.to_owned();
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
        let owned = frame_ref.to_owned();
        assert_eq!(owned.metadata(), Some("audio meta"));
        assert_eq!(owned.data().len(), sample_count);
        assert_eq!(owned.timecode(), Timecode::from_ticks(123));
        assert_eq!(owned.timestamp(), Some(SdkTimestamp::from_ticks(456)));
    }

    #[test]
    fn test_owned_video_from_raw_omits_malformed_frame_metadata() {
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

        let frame = unsafe { VideoFrame::from_raw(&raw) }.unwrap();
        assert_eq!(frame.metadata(), None);
        assert_eq!(frame.data(), data);
        assert!(frame.as_test_raw().p_metadata.is_null());

        metadata[0] = 0xFF;
        metadata[1] = 0;
        let frame = unsafe { VideoFrame::from_raw(&raw) }.unwrap();
        assert_eq!(frame.metadata(), None);
        assert_eq!(frame.data(), data);
        assert!(frame.as_test_raw().p_metadata.is_null());
    }

    #[test]
    fn test_owned_frame_metadata_builders_setters_and_raw_conversion() {
        let mut video = VideoFrame::builder().metadata("").build().unwrap();
        assert_eq!(video.metadata(), Some(""));
        let raw = video.as_test_raw();
        assert!(!raw.p_metadata.is_null());
        assert_eq!(
            unsafe { slice::from_raw_parts(raw.p_metadata.cast::<u8>(), 1) },
            b"\0"
        );

        video.set_metadata("video meta").unwrap();
        assert_eq!(video.metadata(), Some("video meta"));
        let raw = video.as_test_raw();
        assert_eq!(
            unsafe { slice::from_raw_parts(raw.p_metadata.cast::<u8>(), 11) },
            b"video meta\0"
        );

        video.clear_metadata();
        assert!(video.metadata().is_none());
        assert!(video.as_test_raw().p_metadata.is_null());

        let mut audio = AudioFrame::builder().metadata("").build().unwrap();
        assert_eq!(audio.metadata(), Some(""));
        assert!(!audio.to_raw().p_metadata.is_null());
        audio.set_metadata("audio meta").unwrap();
        assert_eq!(audio.metadata(), Some("audio meta"));

        assert!(matches!(
            VideoFrame::builder()
                .metadata("bad\0metadata")
                .build()
                .map_err(Rejected::into_error),
            Err(Error::InvalidCString(_))
        ));
        assert!(matches!(
            AudioFrame::builder()
                .metadata("bad\0metadata")
                .build()
                .map_err(Rejected::into_error),
            Err(Error::InvalidCString(_))
        ));
        video.set_metadata("preserved video").unwrap();
        assert!(matches!(
            video.set_metadata("bad\0metadata"),
            Err(Error::InvalidCString(_))
        ));
        assert!(matches!(
            audio.set_metadata("bad\0metadata"),
            Err(Error::InvalidCString(_))
        ));

        assert_eq!(video.metadata(), Some("preserved video"));
        assert_eq!(audio.metadata(), Some("audio meta"));
        let oversized = "x".repeat(MAX_METADATA_BYTES);
        assert!(matches!(
            VideoFrame::builder()
                .metadata(oversized.clone())
                .build()
                .map_err(Rejected::into_error),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder()
                .metadata(oversized.clone())
                .build()
                .map_err(Rejected::into_error),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            video.set_metadata(oversized.clone()),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            audio.set_metadata(oversized),
            Err(Error::InvalidFrame(_))
        ));
        assert_eq!(video.metadata(), Some("preserved video"));
        assert_eq!(audio.metadata(), Some("audio meta"));
        // Clearing an already empty frame is also infallible.
        for _ in 0..2 {
            video.clear_metadata();
            audio.clear_metadata();
            assert_eq!(video.metadata(), None);
            assert_eq!(audio.metadata(), None);
            assert!(video.as_test_raw().p_metadata.is_null());
            assert!(audio.to_raw().p_metadata.is_null());
        }
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
    fn audio_frame_debug_lists_the_borrowed_frame_fields_not_samples() {
        let frame = AudioFrame::builder()
            .num_channels(2)
            .num_samples(3)
            .timecode(Timecode::from_ticks(5))
            .metadata("<audio/>")
            .build()
            .unwrap();
        assert_eq!(
            format!("{frame:?}"),
            "AudioFrame { sample_rate: 48000, num_channels: 2, num_samples: 3, \
             timecode: Timecode(5), format: Fltp, samples per channel: 3, \
             channel_stride_bytes: 12, metadata: Some(\"<audio/>\"), timestamp: None }"
        );
    }

    #[test]
    fn test_audio_builder_rejects_invalid_dimensions() {
        assert!(matches!(
            AudioFrame::builder()
                .sample_rate(0)
                .build()
                .map_err(Rejected::into_error),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder()
                .sample_rate(1 << 31)
                .build()
                .map_err(Rejected::into_error),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder()
                .num_channels(0)
                .build()
                .map_err(Rejected::into_error),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder()
                .num_samples(0)
                .build()
                .map_err(Rejected::into_error),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            AudioFrame::builder()
                .num_samples(u32::MAX)
                .build()
                .map_err(Rejected::into_error),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_audio_builder_rejects_oversized_layout() {
        let samples = (MAX_AUDIO_BYTES / std::mem::size_of::<f32>()) as u32 + 1;
        let result = AudioFrame::builder()
            .num_channels(1)
            .num_samples(samples)
            .build()
            .map_err(Rejected::into_error);

        assert!(matches!(result, Err(Error::InvalidFrame(_))));
    }

    #[test]
    fn test_default_outbound_timecodes_are_synthesized() {
        let video = VideoFrame::builder().resolution(16, 16).build().unwrap();
        assert_eq!(video.timecode(), Timecode::SYNTHESIZE);
        assert_eq!(
            video.as_test_raw().timecode,
            NDIlib_send_timecode_synthesize
        );
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
                .as_test_raw()
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
                .metadata_raw()
                .timecode,
            0
        );
    }

    #[test]
    fn test_video_builder_rejects_invalid_send_metadata() {
        assert!(matches!(FrameRate::new(0, 1), Err(Error::InvalidInput(_))));
        assert!(matches!(FrameRate::new(30, 0), Err(Error::InvalidInput(_))));
        assert!(matches!(
            VideoFrame::builder()
                .picture_aspect_ratio(f32::NAN)
                .build()
                .map_err(Rejected::into_error),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            VideoFrame::builder()
                .picture_aspect_ratio(-1.0)
                .build()
                .map_err(Rejected::into_error),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn video_aspect_defaults_follow_dimensions_and_override_order_is_independent() {
        for (width, height) in [(640, 480), (1920, 1080), (1080, 1920), (64, 64)] {
            let frame = VideoFrame::builder()
                .resolution(width, height)
                .build()
                .unwrap();
            assert_eq!(frame.picture_aspect_ratio(), 0.0);
            assert_eq!(frame.as_test_raw().picture_aspect_ratio, 0.0);
            for builder in [
                VideoFrame::builder()
                    .picture_aspect_ratio(1.25)
                    .resolution(width, height),
                VideoFrame::builder()
                    .resolution(width, height)
                    .picture_aspect_ratio(1.25),
            ] {
                let frame = builder.build().unwrap();
                assert_eq!(frame.picture_aspect_ratio(), 1.25);
                assert_eq!(frame.as_test_raw().picture_aspect_ratio, 1.25);
            }
        }
        for ratio in [0.0, 1.25] {
            let frame = VideoFrame::builder()
                .resolution(16, 16)
                .picture_aspect_ratio(ratio)
                .build()
                .unwrap();
            let raw = frame.as_test_raw();
            // SAFETY: `raw` borrows this live owned frame's initialized pixels.
            let captured = unsafe { ValidatedVideo::video(&raw).unwrap() };
            let mut copy = captured.owned_copy();
            assert_eq!(copy.picture_aspect_ratio(), ratio);
            copy.set_picture_aspect_ratio(2.0).unwrap();
            captured.copy_into_owned(&mut copy);
            assert_eq!(copy.as_test_raw().picture_aspect_ratio, ratio);
        }
        for ratio in [-1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(matches!(
                VideoFrame::builder()
                    .resolution(16, 16)
                    .picture_aspect_ratio(ratio)
                    .build()
                    .map_err(Rejected::into_error),
                Err(Error::InvalidFrame(_))
            ));
        }
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
        assert_eq!(layout.format, AudioFormat::Fltp);
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
        let frame = unsafe { AudioRef::<FrameSyncAudioFree>::new(guard) }.unwrap();
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
        let mut owned = frame.to_owned();
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

    /// The SDK's no-source answer has no samples and no format, whether it
    /// leaves the FourCC unset or sets it.
    #[test]
    fn returned_audio_accepts_the_no_source_state() {
        let unset = NDIlib_audio_frame_v3_t::default();
        let set = NDIlib_audio_frame_v3_t {
            FourCC: AudioFormat::Fltp.to_sdk(),
            ..unset
        };
        for raw in [unset, set] {
            assert!(matches!(
                validate_returned_audio(&raw),
                Ok(ReturnedAudio::Empty(None))
            ));
        }
    }

    /// A positive sample rate and channel count without samples is the
    /// source's format.
    #[test]
    fn returned_audio_reports_a_format_without_samples() {
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            FourCC: AudioFormat::Fltp.to_sdk(),
            ..Default::default()
        };
        match validate_returned_audio(&raw) {
            Ok(ReturnedAudio::Empty(Some(format))) => {
                assert_eq!((format.sample_rate(), format.num_channels()), (48000, 2));
            }
            other => panic!("expected a format without samples, got {other:?}"),
        }
    }

    /// Samples are validated exactly as every other frame of audio is, and
    /// neither state without samples is a frame of audio.
    #[test]
    fn returned_audio_validates_samples_strictly() {
        let mut data = [0.0f32; 8];
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            no_samples: 4,
            FourCC: AudioFormat::Fltp.to_sdk(),
            p_data: data.as_mut_ptr().cast(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 16,
            },
            ..Default::default()
        };
        match validate_returned_audio(&raw) {
            Ok(ReturnedAudio::Samples(layout)) => {
                assert_eq!(layout.format, AudioFormat::Fltp);
                assert_eq!((layout.no_channels, layout.no_samples), (2, 4));
                assert_eq!(layout.sample_count, 8);
            }
            other => panic!("expected samples, got {other:?}"),
        }
        // A negative count is not "no samples"; it fails strict validation.
        let negative = NDIlib_audio_frame_v3_t {
            no_samples: -1,
            ..raw
        };
        assert!(matches!(
            validate_returned_audio(&negative),
            Err(Error::InvalidFrame(_))
        ));

        let format_only = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            FourCC: AudioFormat::Fltp.to_sdk(),
            ..Default::default()
        };
        for raw in [NDIlib_audio_frame_v3_t::default(), format_only] {
            match validate_audio_layout(&raw) {
                Err(Error::InvalidFrame(message)) => {
                    assert_eq!(message, "Invalid number of samples: 0");
                }
                other => panic!("expected InvalidFrame, got {other:?}"),
            }
        }
    }

    /// Every other shape without samples is rejected.
    #[test]
    fn returned_audio_rejects_partial_and_malformed_empty_states() {
        let format = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            FourCC: AudioFormat::Fltp.to_sdk(),
            ..Default::default()
        };
        let mut sample = 0.0f32;
        let rejected = [
            // Only one of the sample rate and channel count is known.
            NDIlib_audio_frame_v3_t {
                no_channels: 0,
                ..format
            },
            NDIlib_audio_frame_v3_t {
                sample_rate: 0,
                ..format
            },
            // Negative values.
            NDIlib_audio_frame_v3_t {
                sample_rate: -48000,
                ..format
            },
            NDIlib_audio_frame_v3_t {
                no_channels: -2,
                ..format
            },
            // A buffer or a channel stride without samples.
            NDIlib_audio_frame_v3_t {
                p_data: (&mut sample as *mut f32).cast(),
                ..format
            },
            NDIlib_audio_frame_v3_t {
                __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                    channel_stride_in_bytes: 4,
                },
                ..format
            },
            // A format needs a supported FourCC; no source allows only an
            // unset or supported one.
            NDIlib_audio_frame_v3_t {
                FourCC: 0,
                ..format
            },
            NDIlib_audio_frame_v3_t {
                FourCC: 0x1234_5678,
                ..format
            },
            NDIlib_audio_frame_v3_t {
                FourCC: 0x1234_5678,
                ..Default::default()
            },
        ];
        for (index, raw) in rejected.iter().enumerate() {
            let result = validate_returned_audio(raw);
            assert!(
                matches!(result, Err(Error::InvalidFrame(_))),
                "shape {index}: {result:?}"
            );
        }
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
            VideoParts::layout(&frame_ref).data_len_bytes,
            expected_size,
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
            AudioParts::layout(&frame_ref).sample_count,
            sample_count,
            "Cached sample_count should match expected"
        );
    }

    /// A borrowed frame's two owned copies: a new frame, and a copy into an
    /// existing one.
    trait CopySource<F> {
        fn owned(&self) -> F;
        fn copy(&self, destination: &mut F);
    }

    impl<S: FrameFree<RawFrame = NDIlib_video_frame_v2_t>> CopySource<VideoFrame> for VideoRef<'_, S> {
        fn owned(&self) -> VideoFrame {
            self.frame.owned_copy()
        }
        fn copy(&self, destination: &mut VideoFrame) {
            self.frame.copy_into_owned(destination);
        }
    }

    impl CopySource<VideoFrame> for crate::BorrowedVideoFrame<'_> {
        fn owned(&self) -> VideoFrame {
            self.to_owned()
        }
        fn copy(&self, destination: &mut VideoFrame) {
            self.copy_into(destination);
        }
    }

    impl<S: FrameFree<RawFrame = NDIlib_audio_frame_v3_t>> CopySource<AudioFrame> for AudioRef<'_, S> {
        fn owned(&self) -> AudioFrame {
            self.frame.owned_copy()
        }
        fn copy(&self, destination: &mut AudioFrame) {
            self.frame.copy_into_owned(destination);
        }
    }

    /// However an owned frame gets its pixels, their allocation ends with
    /// the zeroed room the SDK reads after a frame of its format, and a
    /// format that needs none keeps the vector it was given.
    #[test]
    fn owned_frames_hold_the_room_the_sdk_reads_after_them() {
        for format in PixelFormat::ALL {
            // The x86 SDK reads past these two formats; the SDK for 64-bit
            // ARM Linux reads every frame exactly.
            let read_past = matches!(format, PixelFormat::Uyvy | PixelFormat::Uyva)
                && !cfg!(all(target_os = "linux", target_arch = "aarch64"));
            assert_eq!(
                format.send_tail_room(),
                if read_past { 64 } else { 0 },
                "{format:?}"
            );
            for height in [4, 6] {
                for scan in ScanType::ALL {
                    let builder = || {
                        VideoFrame::builder()
                            .resolution(18, height)
                            .pixel_format(format)
                            .scan_type(scan)
                    };
                    let room = format.send_buffer_len(18, height, None, scan).unwrap()
                        - format.buffer_len(18, height).unwrap();
                    let holds_room = |frame: &VideoFrame, label: &str| {
                        assert_eq!(frame.tail_room(), room, "{format:?} {label}");
                        assert_eq!(frame.data.room(), vec![0; room], "{format:?} {label}");
                        assert!(VideoSource::sends_in_place(frame), "{format:?} {label}");
                    };

                    let (built, allocations) =
                        crate::test_alloc::allocation_count(|| builder().build().unwrap());
                    assert_eq!(allocations, 1, "{format:?}");
                    let len = built.data().len();
                    assert_eq!(built.data.capacity(), len + room, "{format:?}");
                    holds_room(&built, "built");

                    // A vector that ends with the frame is kept only when the format
                    // needs no room.
                    let exact = vec![7; len];
                    let address = exact.as_ptr();
                    let supplied = builder().data(exact).build().unwrap();
                    assert_eq!(supplied.data(), vec![7; len], "{format:?}");
                    if room == 0 {
                        assert_eq!(
                            (supplied.data().as_ptr(), supplied.data.capacity()),
                            (address, len),
                            "{format:?}"
                        );
                    }
                    holds_room(&supplied, "supplied");

                    // A vector with the room is always kept, whatever its spare
                    // capacity held.
                    let mut roomy = vec![9; len + room];
                    roomy.truncate(len);
                    let address = roomy.as_ptr();
                    let (kept, allocations) = crate::test_alloc::allocation_count(|| {
                        builder().data(roomy).build().unwrap()
                    });
                    assert_eq!(allocations, 0, "{format:?}");
                    assert_eq!(kept.data().as_ptr(), address, "{format:?}");
                    holds_room(&kept, "kept");

                    let mut replaced = built;
                    replaced.replace_data(vec![5; len]).unwrap();
                    assert_eq!(replaced.data(), vec![5; len], "{format:?}");
                    holds_room(&replaced, "replaced");
                    let rejected = replaced.replace_data(vec![5; len + 1]).unwrap_err();
                    assert_eq!(rejected.into_inner().len(), len + 1);
                    holds_room(&replaced, "after a rejected replacement");

                    let (clone, allocations) =
                        crate::test_alloc::allocation_count(|| replaced.clone());
                    assert_eq!(allocations, 1, "{format:?}");
                    assert_eq!(clone.data(), replaced.data(), "{format:?}");
                    holds_room(&clone, "cloned");

                    let borrowed =
                        crate::BorrowedVideoFrame::builder(replaced.data(), 18, height, format)
                            .scan_type(scan)
                            .build()
                            .unwrap();
                    assert_eq!(borrowed.sends_in_place(), room == 0, "{format:?}");
                    let (copy, allocations) =
                        crate::test_alloc::allocation_count(|| borrowed.to_owned());
                    assert_eq!(allocations, 1, "{format:?}");
                    assert_eq!(copy.data.capacity(), len + room, "{format:?}");
                    holds_room(&copy, "copied");
                    let mut destination = VideoFrame::builder().resolution(2, 2).build().unwrap();
                    borrowed.copy_into(&mut destination);
                    holds_room(&destination, "copied into");
                    let pointer = destination.data().as_ptr();
                    let ((), allocations) = crate::test_alloc::allocation_count(|| {
                        borrowed.copy_into(&mut destination);
                    });
                    assert_eq!(allocations, 0);
                    assert_eq!(destination.data().as_ptr(), pointer);
                    holds_room(&destination, "copied again");
                }
            }
        }
    }

    /// Changing the scan type establishes its complete read extent before
    /// the frame can be borrowed, without moving pixels again on later changes.
    #[test]
    fn scan_type_changes_preserve_owned_send_room() {
        for format in PixelFormat::ALL {
            let room = format.send_tail_room();
            // Six rows: not a multiple of four.
            let mut frame = VideoFrame::builder()
                .resolution(18, 6)
                .pixel_format(format)
                .build()
                .unwrap();
            frame.data_mut().fill(7);
            let len = frame.data().len();
            let rows = if room == 0 {
                0
            } else {
                2 * frame.line_stride_bytes()
            };
            for scan in ScanType::ALL {
                frame.set_scan_type(scan);
                let needed = crate::pixels::send_room(&frame.layout, scan);
                let more = scan != ScanType::Progressive;
                assert_eq!(
                    needed,
                    room + if more { rows } else { 0 },
                    "{format:?} {scan:?}"
                );
                let raw = frame.as_test_raw();
                assert_eq!(raw.p_data.cast_const(), frame.data().as_ptr());
                assert_eq!(frame.tail_room(), needed, "{format:?} {scan:?}");
                assert_eq!(frame.data.room(), vec![0; needed], "{format:?} {scan:?}");
                assert_eq!(frame.data(), vec![7; len], "{format:?} {scan:?}");
                assert!(VideoSource::sends_in_place(&frame));
                // Repeating this scan type and every already-covered type
                // preserves the allocation and its initialized tail.
                let ((), allocations) = crate::test_alloc::allocation_count(|| {
                    frame.set_scan_type(scan);
                    frame.set_scan_type(ScanType::Progressive);
                    frame.set_scan_type(scan);
                    assert_eq!(frame.as_test_raw().p_data, raw.p_data);
                });
                assert_eq!(allocations, 0, "{format:?} {scan:?}");
            }
            // Even the first change needs no growth when the caller supplied
            // enough capacity. Stale bytes in that capacity must be zeroed.
            let mut pixels = frame.into_data();
            let address = pixels.as_ptr();
            pixels.resize(pixels.capacity(), 255);
            pixels.truncate(len);
            let mut frame = VideoFrame::builder()
                .resolution(18, 6)
                .pixel_format(format)
                .data(pixels)
                .build()
                .unwrap();
            let ((), allocations) = crate::test_alloc::allocation_count(|| {
                frame.set_scan_type(ScanType::Interlaced);
            });
            assert_eq!(allocations, 0);
            assert_eq!(frame.data().as_ptr(), address);
            assert!(frame.data.room().iter().all(|&byte| byte == 0));
        }
    }

    /// An owned frame as a copy destination: the storage a copy can reuse,
    /// and every field a copy sets.
    trait CopyDestination {
        /// The sample buffer's address, capacity and length, in elements.
        fn buffer(&self) -> (*const (), usize, usize);
        /// The elements the frame keeps after its samples for the SDK to
        /// read.
        fn room(&self) -> usize;
        fn text(&self) -> Option<&str>;
        fn text_capacity(&self) -> usize;
        /// Require the same layout, header, samples, metadata and timestamp,
        /// and the frame's own buffer to fill its layout exactly.
        fn assert_same(&self, expected: &Self);
    }

    impl CopyDestination for VideoFrame {
        fn buffer(&self) -> (*const (), usize, usize) {
            (
                self.data.as_ptr().cast(),
                self.data.capacity(),
                self.data.len(),
            )
        }
        fn room(&self) -> usize {
            self.tail_room()
        }
        fn text(&self) -> Option<&str> {
            self.metadata()
        }
        fn text_capacity(&self) -> usize {
            self.metadata.inner.capacity()
        }
        fn assert_same(&self, expected: &Self) {
            assert_eq!(self.data.len(), self.layout.data_len_bytes);
            let room = crate::pixels::send_room(&self.layout, self.header.scan_type);
            assert_eq!(self.data.room(), vec![0; room]);
            assert_eq!(
                format!("{:?}", self.layout),
                format!("{:?}", expected.layout)
            );
            assert_eq!(
                format!("{:?}", self.header),
                format!("{:?}", expected.header)
            );
            assert_eq!(self.data(), expected.data());
            assert_eq!(self.metadata(), expected.metadata());
            assert_eq!(self.timestamp(), expected.timestamp());
        }
    }

    impl CopyDestination for AudioFrame {
        fn buffer(&self) -> (*const (), usize, usize) {
            (
                self.data.as_ptr().cast(),
                self.data.capacity(),
                self.data.len(),
            )
        }
        fn room(&self) -> usize {
            0
        }
        fn text(&self) -> Option<&str> {
            self.metadata()
        }
        fn text_capacity(&self) -> usize {
            self.metadata.inner.capacity()
        }
        fn assert_same(&self, expected: &Self) {
            assert_eq!(self.data.len(), self.layout.sample_count);
            assert_eq!(
                format!("{:?}", self.layout),
                format!("{:?}", expected.layout)
            );
            assert_eq!(self.timecode(), expected.timecode());
            assert_eq!(self.data(), expected.data());
            assert_eq!(self.metadata(), expected.metadata());
            assert_eq!(self.timestamp(), expected.timestamp());
        }
    }

    /// Require `copy_into` to make each destination exactly the frame
    /// `to_owned` returns, whatever it held, allocating only what the
    /// destination lacks: a buffer too small for the packed copy and the
    /// room after it, and metadata storage too small for the new text.
    /// Copying again into the result, a copy loop's steady state, must allocate nothing
    /// and keep both allocations.
    fn assert_copies_reuse<F: CopyDestination>(source: &dyn CopySource<F>, destinations: Vec<F>) {
        let expected = source.owned();
        let needed = expected.buffer().2 + expected.room();
        for mut destination in destinations {
            let (pixels, capacity, _) = destination.buffer();
            let fits = capacity >= needed;
            let resized_text = expected
                .text()
                .is_some_and(|text| destination.text_capacity() < text.len() + 1);
            let ((), allocations) =
                crate::test_alloc::allocation_count(|| source.copy(&mut destination));
            assert_eq!(allocations, usize::from(!fits) + usize::from(resized_text));
            assert_eq!(destination.buffer().0 == pixels, fits);
            destination.assert_same(&expected);

            let (pixels, capacity, _) = destination.buffer();
            let text = destination.text().map(str::as_ptr);
            let ((), allocations) =
                crate::test_alloc::allocation_count(|| source.copy(&mut destination));
            assert_eq!(allocations, 0);
            assert_eq!(destination.buffer().0, pixels);
            assert_eq!(destination.buffer().1, capacity);
            assert_eq!(destination.text().map(str::as_ptr), text);
            destination.assert_same(&expected);
        }
    }

    /// Frames a copy of a 4x4 video frame must overwrite completely, with
    /// metadata of `metadata_len` bytes: larger, of another format and with
    /// longer metadata; too small, without metadata; planar, large enough,
    /// with a timestamp and metadata of the same length; and one whose
    /// buffer holds a 4x4 UYVY frame exactly, without the room after it.
    fn video_destinations(metadata_len: usize) -> Vec<VideoFrame> {
        let built = |width, height, format, metadata: Option<String>| {
            let mut builder = VideoFrame::builder()
                .resolution(width, height)
                .pixel_format(format)
                .timecode(Timecode::from_ticks(1));
            if let Some(metadata) = metadata {
                builder = builder.metadata(metadata);
            }
            builder.build().unwrap()
        };
        let mut planar = built(16, 8, PixelFormat::I420, Some("m".repeat(metadata_len)));
        planar.timestamp = Some(SdkTimestamp::from_ticks(1));
        vec![
            built(
                64,
                32,
                PixelFormat::Uyvy,
                Some("<longer metadata/>".repeat(8)),
            ),
            built(2, 2, PixelFormat::Bgra, None),
            planar,
            built(2, 4, PixelFormat::P216, None),
        ]
    }

    /// Frames a copy of fifteen samples must overwrite completely, with
    /// metadata of `metadata_len` bytes: larger, with longer metadata; too
    /// small, without metadata; and of another shape that holds exactly as
    /// many samples, with a timestamp and metadata of the same length.
    fn audio_destinations(metadata_len: usize) -> Vec<AudioFrame> {
        let built = |channels, samples, metadata: Option<String>| {
            let mut builder = AudioFrame::builder()
                .sample_rate(44_100)
                .num_channels(channels)
                .num_samples(samples)
                .timecode(Timecode::from_ticks(1));
            if let Some(metadata) = metadata {
                builder = builder.metadata(metadata);
            }
            builder.build().unwrap()
        };
        let mut reshaped = built(5, 3, Some("m".repeat(metadata_len)));
        reshaped.timestamp = Some(SdkTimestamp::from_ticks(1));
        vec![
            built(8, 1024, Some("<longer metadata/>".repeat(8))),
            built(1, 1, None),
            reshaped,
        ]
    }

    #[test]
    fn audio_copy_into_skips_channel_padding_and_reuses_storage() {
        use std::mem::MaybeUninit;
        // Three channels of five samples, eight samples apart. Only samples
        // are initialized; the slots between channels are not audio data.
        let (channels, samples, stride) = (3, 5, 8);
        let mut storage = [MaybeUninit::<f32>::uninit(); 21];
        let mut expected = Vec::new();
        for channel in 0..channels {
            for sample in 0..samples {
                let value = (10 * channel + sample) as f32 + 0.5;
                storage[channel * stride + sample].write(value);
                expected.push(value);
            }
        }
        let metadata = CString::new("<audio-copy/>").unwrap();
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48_000,
            no_channels: channels as i32,
            no_samples: samples as i32,
            FourCC: AudioFormat::Fltp.to_sdk(),
            p_data: storage.as_mut_ptr().cast(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: (stride * 4) as i32,
            },
            timecode: 123,
            timestamp: 456,
            p_metadata: metadata.as_ptr(),
        };
        let bare = NDIlib_audio_frame_v3_t {
            p_metadata: ptr::null(),
            ..raw
        };
        // Null guards with no-op frees skip SDK calls; the samples and the
        // metadata outlive every reference.
        let (framesync, received, bare) = unsafe {
            (
                AudioRef::<FrameSyncAudioFree>::new(Guard::with_test_free(
                    ptr::null_mut(),
                    raw,
                    |_, _| {},
                )),
                AudioFrameRef::new(RecvAudioGuard::with_test_free(
                    ptr::null_mut(),
                    raw,
                    |_, _| {},
                )),
                AudioFrameRef::new(RecvAudioGuard::with_test_free(
                    ptr::null_mut(),
                    bare,
                    |_, _| {},
                )),
            )
        };
        let (framesync, received, bare) = (framesync.unwrap(), received.unwrap(), bare.unwrap());
        assert!(framesync.data().is_none());
        let sources: [(&dyn CopySource<AudioFrame>, _); 3] = [
            (&framesync, Some("<audio-copy/>")),
            (&received, Some("<audio-copy/>")),
            (&bare, None),
        ];
        for (source, metadata) in sources {
            let owned = source.owned();
            assert_eq!(owned.data(), expected);
            assert_eq!(owned.channel_stride_bytes(), samples * 4);
            assert_eq!(owned.metadata(), metadata);
            assert_eq!(
                (owned.sample_rate(), owned.timecode(), owned.timestamp()),
                (
                    48_000,
                    Timecode::from_ticks(123),
                    Some(SdkTimestamp::from_ticks(456))
                )
            );
            assert_copies_reuse(source, audio_destinations("<audio-copy/>".len()));
        }
    }

    /// One pass of the copy-and-send loop `BorrowedVideoFrame::copy_into`
    /// documents, returning the address of the pixels it sent.
    fn copy_and_send(
        sender: &mut crate::Sender,
        spare: &mut Option<VideoFrame>,
        pixels: &[u8],
    ) -> *const u8 {
        let frame = crate::BorrowedVideoFrame::builder(pixels, 64, 32, PixelFormat::Bgra)
            .line_stride_bytes(512)
            .metadata(c"<copy-loop/>")
            .build()
            .unwrap();
        let owned = match spare.take() {
            Some(mut reused) => {
                frame.copy_into(&mut reused);
                reused
            }
            None => frame.to_owned(),
        };
        let sent = owned.data().as_ptr();
        *spare = sender.send_video_async(owned).unwrap();
        sent
    }

    #[test]
    fn copy_and_send_loop_allocates_nothing_once_two_frames_exist() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let options =
            crate::SenderOptions::new(crate::test_support::unique_name("frame copy loop"))
                .clock_video(false);
        let mut sender = crate::Sender::new(&ndi, &options).unwrap();
        // 64 pixels per row, padded to a 512-byte stride.
        let mut buffer = vec![0u8; 512 * 32];
        let mut spare = None;
        // The first two passes make the two frames that then alternate.
        let first = copy_and_send(&mut sender, &mut spare, &buffer);
        assert!(spare.is_none());
        buffer.fill(1);
        let second = copy_and_send(&mut sender, &mut spare, &buffer);
        assert_eq!(spare.as_ref().unwrap().data().as_ptr(), first);
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for shade in 2..10 {
                buffer.fill(shade);
                copy_and_send(&mut sender, &mut spare, &buffer);
            }
        });
        assert_eq!(allocations, 0);
        let last = sender.finish_video_async().unwrap();
        let previous = spare.unwrap();
        assert_eq!(
            (last.data().as_ptr(), previous.data().as_ptr()),
            (second, first)
        );
        for (frame, shade) in [(last, 9), (previous, 8)] {
            assert_eq!(frame.data().len(), 64 * 32 * 4);
            assert!(frame.data().iter().all(|&byte| byte == shade));
            assert_eq!(frame.metadata(), Some("<copy-loop/>"));
        }
    }
}

#[cfg(test)]
#[path = "frames/metadata_tests.rs"]
mod metadata_tests;
