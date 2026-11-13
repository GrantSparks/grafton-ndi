//! What to encode and how: [`ImageOptions`] and the settings it holds.

use crate::YuvMatrix;

/// The image format a video frame is encoded to.
///
/// # Examples
///
/// ```
/// use grafton_ndi::ImageFormat;
///
/// // PNG: lossless, any bit depth, carries an HDR tag.
/// let png = ImageFormat::Png;
///
/// // JPEG at quality 85: lossy and the smallest.
/// let jpeg = ImageFormat::Jpeg(85);
///
/// // Lossless WebP: smaller and far faster than PNG on camera video.
/// let webp = ImageFormat::WebpLossless;
/// ```
///
/// This enum is marked `#[non_exhaustive]` so that encoders, lossy WebP
/// among them, can be added without a breaking change. Match arms should
/// include a wildcard `_` pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageFormat {
    /// PNG: lossless, 8 or 16 bits a channel, with or without alpha.
    Png,
    /// JPEG at a quality from 1, the lowest, to 100, the highest; 8 bits a
    /// channel and no alpha. Below 90 chroma is halved in both directions
    /// (4:2:0); from 90 it is kept whole (4:4:4).
    Jpeg(u8),
    /// Lossless WebP: 8 bits a channel, with or without alpha.
    WebpLossless,
}

impl ImageFormat {
    /// The format's name in messages.
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg(_) => "JPEG",
            Self::WebpLossless => "WebP",
        }
    }

    /// The format's media type, for a data URL.
    pub(super) const fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg(_) => "image/jpeg",
            Self::WebpLossless => "image/webp",
        }
    }
}

/// How a video frame is encoded as a still image: its format, color
/// handling, bit depth, alpha, HDR policy, size and PNG compression.
///
/// Every video frame type encodes with it, whether owned, captured or
/// borrowed for sending, through `encode_image`, `encode_image_to` and
/// `encode_image_data_url`, for example [`VideoFrame::encode_image`]. The
/// shortcuts `encode_png`, `encode_jpeg` and `encode_data_url` are exactly
/// those methods with `ImageOptions::new(format)`.
///
/// ```
/// use grafton_ndi::{ColorPolicy, Downscale, ImageFormat, ImageOptions, PixelFormat, VideoFrame, YuvMatrix, YuvRange};
///
/// let frame = VideoFrame::builder()
///     .resolution(64, 36)
///     .pixel_format(PixelFormat::Uyvy)
///     .data([128, 16, 128, 16].repeat(64 * 36 / 2))
///     .build()?;
///
/// // A quarter-size JPEG thumbnail.
/// let thumbnail = ImageOptions::new(ImageFormat::Jpeg(85)).downscale(Downscale::Quarter);
/// let jpeg = frame.encode_image(&thumbnail)?;
///
/// // A source known to label its color wrongly.
/// let options = ImageOptions::new(ImageFormat::Png)
///     .color(ColorPolicy::Override { matrix: YuvMatrix::Bt601, range: YuvRange::Limited });
/// let png = frame.encode_image(&options)?;
/// # assert!(jpeg.starts_with(&[0xFF, 0xD8]) && png.starts_with(b"\x89PNG"));
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
///
/// # Formats
///
/// Every uncompressed [`PixelFormat`](crate::PixelFormat) encodes: the four
/// 8-bit RGB formats, and the YUV formats UYVY, UYVA, P216, PA16, NV12, I420,
/// YV12, NA48, NB48, NA4G and NB4G, which are converted to RGB. Rows are read
/// in place, skipping any row padding.
///
/// | Output | Channels | Bits a channel |
/// |---|---|---|
/// | PNG, [`AlphaMode::Keep`] | RGBA; a source without alpha gets alpha 255 | 8 or 16 |
/// | PNG, [`AlphaMode::Discard`] | RGB | 8 or 16 |
/// | JPEG | YCbCr; alpha is dropped | 8 |
/// | Lossless WebP | RGBA, or RGB with [`AlphaMode::Discard`] | 8 |
///
/// An untagged frame, or one tagged BT.601 or BT.709 SDR, in one of the four
/// RGB formats encodes, with the default options, to the same bytes as in
/// grafton-ndi 2.0.
///
/// # Color
///
/// YUV frames are converted with BT.709, limited range and repeated chroma,
/// as the NDI 6.4 SDK converts at every frame size, unless the frame's
/// `ndi_color_info` metadata ([`ColorInfo`](crate::ColorInfo)) names another
/// matrix: the encoder then follows the tag, which the SDK ignores.
/// [`ColorPolicy::Override`] replaces the matrix and range. The coefficients
/// are exact, so white is 255 where the SDK's own RGB output gives 253. See
/// [`ColorPolicy`].
///
/// # HDR
///
/// A frame tagged with the PQ or HLG transfer function is refused unless
/// [`HdrPolicy::CodeValues`] asks for its code values, which only PNG can
/// carry, tagged so that an HDR-aware viewer shows them correctly. Nothing is
/// tone-mapped. See [`HdrPolicy`].
///
/// # Performance
///
/// Encoding is CPU work. Measured on one thread for a photographic 1920×1080
/// RGBA frame: 554 ms as PNG at [`PngCompression::Balanced`], the default,
/// 24 ms at [`PngCompression::Fast`], 19 ms as lossless WebP and 21 ms as
/// JPEG at quality 85. Converting a YUV frame adds about 6 ms. A snapshot loop
/// of camera video should use `Fast` or WebP; see [`PngCompression`].
///
/// Async code runs the encoder on a blocking thread, with an owned frame:
///
/// ```no_run
/// # async fn snapshot(frame: grafton_ndi::VideoFrame) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
/// use grafton_ndi::{ImageFormat, ImageOptions};
///
/// let options = ImageOptions::new(ImageFormat::WebpLossless);
/// let webp = tokio::task::spawn_blocking(move || frame.encode_image(&options)).await??;
/// # Ok(webp)
/// # }
/// ```
///
/// # Memory
///
/// PNG and JPEG convert at most one output row at a time, so their
/// conversion scratch does not grow with the frame's height: none for a PNG
/// of an RGBA frame, whose rows the codec reads in place, or for a JPEG,
/// whose pixels convert straight into the codec's rows; one row otherwise.
/// Lossless WebP encodes from one buffer, so it holds the whole converted
/// image, unless the frame is RGBA with contiguous rows, which it reads in
/// place. Codec state and the encoded output take memory besides.
///
/// # Errors
///
/// Setters never fail; the encoding methods check the options with the
/// frame, before anything is written.
///
/// | Problem | Error |
/// |---|---|
/// | A JPEG quality outside 1 to 100 | [`Error::InvalidInput`] |
/// | JPEG or WebP with [`SampleDepth::Sixteen`] | [`Error::InvalidInput`] |
/// | JPEG or WebP with [`HdrPolicy::CodeValues`] for a PQ or HLG frame | [`Error::InvalidInput`] |
/// | A PQ or HLG frame with [`HdrPolicy::Refuse`] | [`Error::InvalidFrame`] |
/// | Unreadable `ndi_color_info` with [`ColorPolicy::Sdk`] | [`Error::InvalidFrame`] |
/// | An image wider or taller than JPEG's 65,535 or WebP's 16,384 pixels | [`Error::InvalidFrame`] |
/// | The writer given to `encode_image_to` fails | [`Error::Io`] |
/// | The codec fails for another reason | [`Error::InvalidFrame`] |
///
/// `ImageOptions` holds settings only, no buffers, so one value serves any
/// number of threads.
///
/// [`VideoFrame::encode_image`]: crate::VideoFrame::encode_image
/// [`Error::InvalidInput`]: crate::Error::InvalidInput
/// [`Error::InvalidFrame`]: crate::Error::InvalidFrame
/// [`Error::Io`]: crate::Error::Io
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageOptions {
    pub(super) format: ImageFormat,
    pub(super) color: ColorPolicy,
    pub(super) depth: SampleDepth,
    pub(super) alpha: AlphaMode,
    pub(super) hdr: HdrPolicy,
    pub(super) downscale: Downscale,
    pub(super) png_compression: PngCompression,
}

impl ImageOptions {
    /// Replace the output format, preserving every other encoding setting.
    #[must_use]
    pub fn format(mut self, format: ImageFormat) -> Self {
        self.format = format;
        self
    }

    /// Requested encoded image format.
    #[must_use]
    pub fn configured_format(&self) -> ImageFormat {
        self.format
    }

    /// Requested color conversion policy.
    #[must_use]
    pub fn configured_color(&self) -> ColorPolicy {
        self.color
    }

    /// Requested bits per channel.
    #[must_use]
    pub fn configured_depth(&self) -> SampleDepth {
        self.depth
    }

    /// Requested alpha handling.
    #[must_use]
    pub fn configured_alpha(&self) -> AlphaMode {
        self.alpha
    }

    /// Requested HDR handling.
    #[must_use]
    pub fn configured_hdr(&self) -> HdrPolicy {
        self.hdr
    }

    /// Requested image downscale factor.
    #[must_use]
    pub fn configured_downscale(&self) -> Downscale {
        self.downscale
    }

    /// Requested PNG compression level.
    #[must_use]
    pub fn configured_png_compression(&self) -> PngCompression {
        self.png_compression
    }

    /// Options for `format`, with every other setting at its default:
    /// [`ColorPolicy::Sdk`], [`SampleDepth::Eight`], [`AlphaMode::Keep`],
    /// [`HdrPolicy::Refuse`], [`Downscale::Off`] and
    /// [`PngCompression::Balanced`].
    #[must_use]
    pub fn new(format: ImageFormat) -> Self {
        Self {
            format,
            color: ColorPolicy::default(),
            depth: SampleDepth::default(),
            alpha: AlphaMode::default(),
            hdr: HdrPolicy::default(),
            downscale: Downscale::default(),
            png_compression: PngCompression::default(),
        }
    }

    /// Choose how YUV frames are converted to RGB. The default is
    /// [`ColorPolicy::Sdk`].
    #[must_use]
    pub fn color(mut self, policy: ColorPolicy) -> Self {
        self.color = policy;
        self
    }

    /// Choose the bits a channel of the image: [`SampleDepth::Sixteen`]
    /// needs [`ImageFormat::Png`]. The default is [`SampleDepth::Eight`].
    #[must_use]
    pub fn depth(mut self, depth: SampleDepth) -> Self {
        self.depth = depth;
        self
    }

    /// Choose whether a PNG or WebP image has an alpha channel. JPEG never
    /// has one. The default is [`AlphaMode::Keep`].
    #[must_use]
    pub fn alpha(mut self, alpha: AlphaMode) -> Self {
        self.alpha = alpha;
        self
    }

    /// Choose what happens to a frame tagged PQ or HLG. The default is
    /// [`HdrPolicy::Refuse`].
    #[must_use]
    pub fn hdr(mut self, hdr: HdrPolicy) -> Self {
        self.hdr = hdr;
        self
    }

    /// Reduce the image by 2, 4 or 8 in each direction. The default is
    /// [`Downscale::Off`].
    #[must_use]
    pub fn downscale(mut self, by: Downscale) -> Self {
        self.downscale = by;
        self
    }

    /// Choose how hard PNG compresses. It has no effect on JPEG or WebP. The
    /// default is [`PngCompression::Balanced`].
    #[must_use]
    pub fn png_compression(mut self, level: PngCompression) -> Self {
        self.png_compression = level;
        self
    }
}

/// How YUV frames are converted to RGB.
///
/// RGB frames are encoded as they are; the policy still decides whether
/// their `ndi_color_info` must be readable.
///
/// This enum is marked `#[non_exhaustive]` so that policies can be added
/// without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorPolicy {
    /// Convert as the NDI SDK converts, with BT.709 and limited range,
    /// unless the frame's `ndi_color_info` metadata names another matrix,
    /// which is then used.
    ///
    /// The NDI 6.4 SDK uses BT.709 limited range for every frame size, in
    /// both directions, whatever the frame's metadata says, although its
    /// manual describes BT.601 for SD and BT.2020 for UHD. A frame tagged
    /// with another matrix, BT.2020 for example, therefore encodes correctly
    /// here but shows BT.709 coefficients in an RGB receiver of the same
    /// source. An untagged BT.601 source, such as older SD hardware, needs
    /// [`ColorPolicy::Override`].
    ///
    /// A frame whose `ndi_color_info` cannot be read is refused with
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame), so the encoder
    /// never guesses at a frame that says something it cannot read.
    #[default]
    Sdk,
    /// Convert with this matrix and range, whatever the frame says.
    ///
    /// The caller takes responsibility for the frame's color: unreadable
    /// `ndi_color_info` is ignored, and the frame then counts as SDR with
    /// unknown primaries. Readable metadata still decides the HDR check and
    /// a PNG's color tag.
    Override {
        /// The matrix that relates the frame's YCbCr to RGB.
        matrix: YuvMatrix,
        /// The range of the frame's code values.
        range: YuvRange,
    },
}

/// The range of a YUV frame's code values.
///
/// This enum is marked `#[non_exhaustive]` so that ranges can be added
/// without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum YuvRange {
    /// Limited, or video, range: at 8 bits, luma 16 to 235 and chroma 16 to
    /// 240; at 16 bits, those values times 256. NDI video uses it.
    Limited,
    /// Full range: every code value.
    Full,
}

/// The bits a channel of an encoded image.
///
/// This enum is marked `#[non_exhaustive]` so that depths can be added
/// without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum SampleDepth {
    /// 8 bits a channel, rounded from any source. Every format writes it.
    #[default]
    Eight,
    /// 16 bits a channel, for PNG only. P216, PA16, NA4G and NB4G frames
    /// keep their precision, and 8-bit sources are scaled exactly (times
    /// 257). Receivers set to [`ReceiverColorFormat::Best`] or
    /// [`ReceiverColorFormat::Best444`] get such frames from a
    /// high-bit-depth source.
    ///
    /// [`ReceiverColorFormat::Best`]: crate::ReceiverColorFormat::Best
    /// [`ReceiverColorFormat::Best444`]: crate::ReceiverColorFormat::Best444
    Sixteen,
}

/// Whether a PNG or WebP image has an alpha channel.
///
/// This enum is marked `#[non_exhaustive]` so that modes can be added without
/// a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum AlphaMode {
    /// Write RGBA. A source with alpha keeps it, converted to the image's
    /// depth; one without gets alpha 255, or 65535 at 16 bits.
    #[default]
    Keep,
    /// Write RGB, dropping any alpha. Smaller when the alpha is opaque
    /// anyway.
    Discard,
}

/// What happens to a frame whose `ndi_color_info` gives the PQ or HLG
/// transfer function.
///
/// | The frame's transfer | [`HdrPolicy::Refuse`] | [`HdrPolicy::CodeValues`] |
/// |---|---|---|
/// | None, `bt_601`, `bt_709` or `bt_2020` | Encoded | Encoded |
/// | `bt_2100_pq` | [`Error::InvalidFrame`] | PNG of the PQ code values, tagged |
/// | `bt_2100_hlg` | [`Error::InvalidFrame`] | PNG of the HLG code values, tagged |
///
/// The check applies to every pixel format, RGB as well as YUV: an RGB
/// receiver of a PQ source gets the PQ code values, with the tag still
/// attached. Nothing tone-maps. A program that wants an SDR rendering of HDR
/// video makes it itself, from a 16-bit PNG or from the frame, and
/// [`color_info`](crate::VideoFrame::color_info) lets it check a frame before
/// encoding it.
///
/// This enum is marked `#[non_exhaustive]` so that policies can be added
/// without a breaking change.
///
/// [`Error::InvalidFrame`]: crate::Error::InvalidFrame
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum HdrPolicy {
    /// Refuse a PQ or HLG frame with
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame), rather than write
    /// an image whose colors every SDR viewer shows washed out.
    #[default]
    Refuse,
    /// Write a PQ or HLG frame's non-linear code values to a PNG, tagged with
    /// a `cICP` chunk that tells an HDR-aware viewer their primaries and
    /// transfer function. Use [`SampleDepth::Sixteen`] to keep a
    /// high-bit-depth frame's precision.
    ///
    /// JPEG and WebP refuse such a frame with
    /// [`Error::InvalidInput`](crate::Error::InvalidInput): neither can say
    /// what its numbers mean, and untagged PQ looks wrong in every viewer.
    CodeValues,
}

/// How much an image is reduced in each direction.
///
/// Reduction averages blocks of converted pixels inside the row loop, so a
/// thumbnail of a full-size frame never holds a full-size image. The image
/// is ceil(width / f) by ceil(height / f), where f is 2, 4 or 8; a block at
/// the right or bottom edge averages only the pixels it has. Averages are
/// taken on code values, as previews usually are, and round half up. With
/// alpha kept, color is weighted by alpha, so transparent pixels do not
/// darken edges; a block that is transparent throughout takes the plain mean.
///
/// It serves frames that arrive full size: from a FrameSync, from receivers
/// with [`ReceiverOptions::high_quality_preset`], and frames being sent. A
/// [`ReceiverOptions::snapshot_preset`] receiver already gets the source's
/// smaller preview stream.
///
/// This enum is marked `#[non_exhaustive]` so that factors can be added
/// without a breaking change.
///
/// [`ReceiverOptions::high_quality_preset`]: crate::ReceiverOptions::high_quality_preset
/// [`ReceiverOptions::snapshot_preset`]: crate::ReceiverOptions::snapshot_preset
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Downscale {
    /// Full size.
    #[default]
    Off,
    /// Half the width and height: 2×2 blocks.
    Half,
    /// A quarter of the width and height: 4×4 blocks.
    Quarter,
    /// An eighth of the width and height: 8×8 blocks.
    Eighth,
}

impl Downscale {
    /// log2 of the factor.
    pub(super) const fn shift(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Half => 1,
            Self::Quarter => 2,
            Self::Eighth => 3,
        }
    }
}

/// How hard PNG compresses.
///
/// PNG's cost is compression; converting the pixels takes a few milliseconds.
/// Measured on one thread for a 1920×1080 RGBA frame:
///
/// | Content | `Balanced` | `Fast` | `High` | Lossless WebP |
/// |---|---|---|---|---|
/// | Photographic | 3.38 MB, 554 ms | 3.76 MB, 24 ms | 3.35 MB, 1,198 ms | 2.80 MB, 19 ms |
/// | Graphics | 150 KB, 40 ms | 492 KB, 8 ms | 135 KB, 104 ms | 329 KB, 8 ms |
///
/// `Fast` is about 23 times faster than `Balanced` for 11% more bytes on
/// camera video, but 3.3 times the bytes on graphics; lossless WebP is
/// smaller still on camera video and as fast. A snapshot loop of camera video
/// should use one of them. `Balanced` stays the default because it suits
/// graphics and keeps the bytes grafton-ndi 2.0 wrote. Every level is
/// lossless.
///
/// This enum is marked `#[non_exhaustive]` so that levels can be added
/// without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum PngCompression {
    /// The `png` crate's fast DEFLATE: about 23 times faster than `Balanced`
    /// on camera video.
    Fast,
    /// The `png` crate's default level.
    #[default]
    Balanced,
    /// Maximum DEFLATE compression: about 1% smaller than `Balanced` on camera
    /// video and 10% smaller on graphics, at about twice its time.
    High,
}
