//! What to encode and how: [`ImageOptions`] and the settings it holds.

use crate::YuvMatrix;

/// The image format a video frame is encoded to.
///
/// # Examples
///
/// ```
/// use grafton_ndi::image_encoding::ImageFormat;
///
/// // PNG: lossless, any bit depth, carries an HDR tag.
/// let png = ImageFormat::Png;
///
/// // JPEG at quality 85: lossy, without alpha.
/// let jpeg = ImageFormat::Jpeg(85);
///
/// // Lossless WebP: 8-bit color with optional alpha.
/// let webp = ImageFormat::WebpLossless;
///
/// // Lossy WebP at quality 80, preserving alpha exactly.
/// let preview = ImageFormat::WebpLossy(80);
/// ```
///
/// This enum is marked `#[non_exhaustive]` so that encoders
/// can be added without a breaking change. Match arms should
/// include a wildcard `_` pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageFormat {
    /// PNG: lossless, 8 or 16 bits a channel, with or without alpha.
    Png,
    /// JPEG at a quality from 1, the lowest, to 100, the highest; 8 bits a
    /// channel and no alpha. Below 90 chroma is halved in both directions
    /// (4:2:0). From 90 it is kept whole (4:4:4). Frames that need a color
    /// tag require PNG. See [`HdrPolicy`].
    Jpeg(u8),
    /// Lossless WebP: 8 bits a channel, with or without alpha. Frames that
    /// need a color tag require PNG. See [`HdrPolicy`].
    WebpLossless,
    /// Lossy WebP at a quality from 0, the lowest, to 100, the highest;
    /// 8 bits a channel, with or without alpha. Alpha is preserved exactly.
    /// Color uses 4:2:0 chroma sampling and, by default, SharpYUV conversion;
    /// even quality 100 can change RGB values. Use [`Self::WebpLossless`]
    /// for exact pixels. Frames that need a color tag require PNG. See
    /// [`HdrPolicy`].
    WebpLossy(u8),
}

/// Fixed codec capabilities, stated exhaustively for every format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ImageFormatInfo {
    pub(super) codec: crate::ImageCodecKind,
    pub(super) media_type: &'static str,
    pub(super) quality: Option<std::ops::RangeInclusive<u8>>,
    pub(super) alpha: AlphaChannel,
    pub(super) sixteen_bit: bool,
    pub(super) carries_color_tag: bool,
    pub(super) aspect_tag: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AlphaChannel {
    None,
    Optional,
    AlwaysRgba,
}

impl ImageFormat {
    pub(super) const fn info(self) -> ImageFormatInfo {
        match self {
            Self::Png => ImageFormatInfo {
                codec: crate::ImageCodecKind::Png,
                media_type: "image/png",
                quality: None,
                alpha: AlphaChannel::Optional,
                sixteen_bit: true,
                carries_color_tag: true,
                aspect_tag: true,
            },
            Self::Jpeg(_) => ImageFormatInfo {
                codec: crate::ImageCodecKind::Jpeg,
                media_type: "image/jpeg",
                quality: Some(1..=100),
                alpha: AlphaChannel::None,
                sixteen_bit: false,
                carries_color_tag: false,
                aspect_tag: true,
            },
            Self::WebpLossless => ImageFormatInfo {
                codec: crate::ImageCodecKind::Webp,
                media_type: "image/webp",
                quality: None,
                alpha: AlphaChannel::Optional,
                sixteen_bit: false,
                carries_color_tag: false,
                aspect_tag: false,
            },
            Self::WebpLossy(_) => ImageFormatInfo {
                codec: crate::ImageCodecKind::Webp,
                media_type: "image/webp",
                quality: Some(0..=100),
                alpha: AlphaChannel::AlwaysRgba,
                sixteen_bit: false,
                carries_color_tag: false,
                aspect_tag: false,
            },
        }
    }

    pub(super) fn validate_quality(self) -> crate::Result<()> {
        let info = self.info();
        if let Self::Jpeg(quality) | Self::WebpLossy(quality) = self {
            if let Some(bounds) = info.quality {
                if !bounds.contains(&quality) {
                    return Err(crate::Error::InvalidInput(
                        crate::InputDefect::ImageQuality {
                            codec: info.codec,
                            quality,
                            minimum: *bounds.start(),
                            maximum: *bounds.end(),
                        },
                    ));
                }
            }
        }
        Ok(())
    }
}

// One list supplies representative formats and an exhaustive coverage check,
// just as decoded_variants does for pixel formats.
#[cfg(test)]
macro_rules! image_format_variants {
    ($($variant:ident $(($argument:pat))? => $sample:expr),+ $(,)?) => {
        impl ImageFormat {
            pub(super) const ALL: [Self; [$(stringify!($variant)),+].len()] = [$($sample),+];

            const fn variant_index(self) -> usize {
                enum Index { $($variant),+ }
                match self { $(Self::$variant $(($argument))? => Index::$variant as usize),+ }
            }
        }
        const_assert!({
            let mut index = 0;
            while index < ImageFormat::ALL.len() {
                assert!(ImageFormat::ALL[index].variant_index() == index);
                index += 1;
            }
        });
    };
}
#[cfg(test)]
image_format_variants!(
    Png => ImageFormat::Png,
    Jpeg(_) => ImageFormat::Jpeg(85),
    WebpLossless => ImageFormat::WebpLossless,
    WebpLossy(_) => ImageFormat::WebpLossy(75),
);

/// How a video frame is encoded as a still image: its format, color
/// handling, bit depth, alpha, HDR policy, size and codec effort.
///
/// Every video frame type encodes with it, whether owned, captured or
/// borrowed for sending, through `encode_image`, `encode_image_to` and
/// `encode_image_data_url`, for example [`VideoFrame::encode_image`].
/// Construct these settings with `ImageOptions::new(format)`.
///
/// ```
/// use grafton_ndi::{PixelFormat, VideoFrame, YuvMatrix, image_encoding::{ColorPolicy, Downscale, ImageFormat, ImageOptions, YuvRange}};
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
/// PNG and JPEG preserve picture aspect in relative pixel-density tags (PNG
/// pHYs and JPEG JFIF) whenever the output needs non-square pixels. This includes
/// square-pixel sources whose partial downscale blocks change the stored
/// width/height ratio. Pixel dimensions and samples are unchanged. Outputs
/// needing square pixels retain their default density behavior; equivalent
/// implicit and explicit square-pixel sources produce identical bytes. Viewers
/// may ignore these tags. WebP carries no pixel-shape tag. Extreme ratios use
/// the nearest positive u16 density pair.
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
/// | Lossy WebP | Lossy color, exact alpha unless discarded | 8 |
///
/// # Color
///
/// By default, YUV frames use limited range and repeated chroma, with the
/// matrix in readable `ndi_color_info` metadata ([`ColorInfo`](crate::ColorInfo)),
/// or BT.709 when untagged. [`ColorPolicy::Override`] replaces the matrix and
/// range. See [`ColorPolicy`] for malformed metadata handling.
///
/// The SDK's own YUV-to-RGB conversion always uses BT.709 limited range,
/// whatever the tag says. RGB-received frames already carry that conversion:
/// encoding keeps their RGB pixels, so the tag's matrix can no longer be
/// applied. Readable tags still govern color admission and the PNG `cICP` chunk.
/// [`ReceiverColorFormat::UyvyBgra`](crate::ReceiverColorFormat::UyvyBgra) and
/// [`UyvyRgba`](crate::ReceiverColorFormat::UyvyRgba) keep opaque sources in
/// YUV, but deliver BGRA or RGBA for sources with alpha. When a tagged source
/// may carry alpha, use
/// [`ReceiverColorFormat::Fastest`](crate::ReceiverColorFormat::Fastest) for
/// UYVA or [`Best`](crate::ReceiverColorFormat::Best) for UYVA or PA16. `Best`
/// also retains high bit depth and HDR code values. Both deliver individual
/// fields even with [`allow_video_fields(false)`](crate::ReceiverOptions::allow_video_fields),
/// so interlaced snapshots may need field assembly.
///
/// # HDR
///
/// A frame tagged with the PQ or HLG transfer function is refused unless
/// [`HdrPolicy::CodeValues`] asks for its code values, which only PNG can
/// carry, tagged so that an HDR-aware viewer shows them correctly. Nothing is
/// tone-mapped. Receive YUV with
/// [`ReceiverColorFormat::Best`](crate::ReceiverColorFormat::Best) to retain
/// high bit depth and HDR code values: an RGB-received frame already holds
/// the SDK's BT.709 limited-range conversion, which encoding at 16 bits cannot
/// undo. The passed-through tag still governs color admission and the PNG
/// `cICP` chunk. Frames with BT.2020 primaries also require tagged PNG output.
/// See [`HdrPolicy`].
///
/// # Performance
///
/// Encoding is synchronous CPU work. Compare formats, PNG compression and
/// lossy WebP effort on representative content at acceptable visual quality.
/// Lower [`Self::webp_effort`] and disable [`Self::webp_sharp_yuv`] when
/// prioritizing latency independently of quality; measure the result and bound
/// concurrent encoding jobs. Neither setting provides a time or memory limit.
///
/// Async code runs the encoder on a blocking thread, with an owned frame:
///
/// ```no_run
/// # async fn snapshot(frame: grafton_ndi::VideoFrame) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
/// use grafton_ndi::{image_encoding::{ImageFormat, ImageOptions}};
///
/// let options = ImageOptions::new(ImageFormat::WebpLossless);
/// let webp = tokio::task::spawn_blocking(move || frame.encode_image(&options)).await??;
/// # Ok(webp)
/// # }
/// ```
///
/// # Memory
///
/// PNG and JPEG convert one output row at a time, so conversion scratch
/// does not grow with frame height. PNG borrows eligible RGBA rows or
/// retains one row; JPEG converts into the codec's rows and retains its
/// final component row to avoid converting padding requests again.
/// WebP encodes from one buffer, so it holds the whole converted
/// image, unless full-size contiguous RGBA can be borrowed with alpha kept.
/// Lossless WebP uses three channels with alpha discarded; lossy WebP
/// uses four with opaque alpha. Both WebP formats buffer the complete encoded
/// image before writing it. Codec state and encoded output take memory besides.
/// Each concurrent encode owns its own buffers, so worker and queue limits
/// matter alongside compressed image size.
///
/// # Errors
///
/// Setters never fail; the encoding methods check the options with the
/// frame, before anything is written.
///
/// | Problem | Error |
/// |---|---|
/// | A JPEG quality outside 1 to 100 | [`Error::InvalidInput`] |
/// | A lossy WebP quality outside 0 to 100 | [`Error::InvalidInput`] |
/// | A lossy WebP effort outside 0 to 6 | [`Error::InvalidInput`] |
/// | JPEG or WebP with [`SampleDepth::Sixteen`] | [`Error::InvalidInput`] |
/// | JPEG or WebP for a frame that needs a color tag (BT.2020 primaries, or PQ or HLG with [`HdrPolicy::CodeValues`]) | [`Error::InvalidInput`] |
/// | A PQ or HLG frame with [`HdrPolicy::Refuse`] | [`Error::InvalidFrame`] |
/// | Unreadable `ndi_color_info` with [`ColorPolicy::Sdk`] | [`Error::InvalidFrame`] |
/// | An image wider or taller than JPEG's 65,535, lossless WebP's 16,384 or lossy WebP's 16,383 pixels | [`Error::InvalidFrame`] |
/// | Writing or flushing the writer given to `encode_image_to` fails | [`Error::Io`] |
/// | Conversion scratch cannot be allocated | [`Error::AllocationFailed`] |
/// | The lossy WebP encoder cannot allocate its working storage | [`Error::InvalidFrame`] (`crate::FrameDefect::EncodingFailed`) |
/// | The codec fails for another reason | [`Error::InvalidFrame`] |
///
/// `ImageOptions` holds settings only, no buffers, so one value serves any
/// number of threads.
///
/// [`VideoFrame::encode_image`]: crate::VideoFrame::encode_image
/// [`Error::InvalidInput`]: crate::Error::InvalidInput
/// [`Error::AllocationFailed`]: crate::Error::AllocationFailed
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
    pub(super) webp_effort: u8,
    pub(super) webp_sharp_yuv: bool,
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

    /// Requested lossy WebP compression effort (default 4).
    #[must_use]
    pub fn configured_webp_effort(&self) -> u8 {
        self.webp_effort
    }

    /// Whether lossy WebP uses SharpYUV color conversion (default true).
    #[must_use]
    pub fn configured_webp_sharp_yuv(&self) -> bool {
        self.webp_sharp_yuv
    }

    /// Options for `format`, with every other setting at its default:
    /// [`ColorPolicy::Sdk`], [`SampleDepth::Eight`], [`AlphaMode::Keep`],
    /// [`HdrPolicy::Refuse`], [`Downscale::Off`],
    /// [`PngCompression::Balanced`], lossy WebP effort 4 and SharpYUV enabled.
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
            webp_effort: 4,
            webp_sharp_yuv: true,
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

    /// Choose lossy WebP's compression effort, independently of color quality:
    /// 0 is fastest, 6 spends the most effort, and the default is 4. Higher
    /// effort generally trades CPU time for smaller output; measure your content.
    /// Alpha remains exact at every effort.
    ///
    /// Values outside 0 to 6 return [`Error::InvalidInput`](crate::Error::InvalidInput)
    /// when encoding lossy WebP, before anything is written. This setting has
    /// no effect on PNG, JPEG or lossless WebP.
    #[must_use]
    pub fn webp_effort(mut self, effort: u8) -> Self {
        self.webp_effort = effort;
        self
    }

    /// Choose lossy WebP's RGB-to-YUV conversion. The default, true, uses
    /// SharpYUV for sharper chroma edges at an additional CPU cost. False
    /// uses the faster standard conversion; alpha remains exact.
    ///
    /// Combine `webp_effort(0)` and `webp_sharp_yuv(false)` when prioritizing
    /// encoding latency. Neither setting imposes a time or memory limit.
    /// This setting has no effect on PNG, JPEG or lossless WebP.
    #[must_use]
    pub fn webp_sharp_yuv(mut self, enabled: bool) -> Self {
        self.webp_sharp_yuv = enabled;
        self
    }
}

/// How YUV frames are converted to RGB.
///
/// RGB frames are encoded as they are; the policy still decides whether
/// their `ndi_color_info` must be readable. RGB-received frames already carry
/// the SDK's BT.709 limited-range YUV conversion, whatever that tag says.
///
/// This enum is marked `#[non_exhaustive]` so that policies can be added
/// without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorPolicy {
    /// Convert with BT.709 and limited range, unless the frame's
    /// `ndi_color_info` metadata names another matrix, which is then used.
    /// An untagged source with different color encoding needs
    /// [`ColorPolicy::Override`].
    ///
    /// RGB-received frames already hold the SDK's BT.709 limited-range
    /// conversion, which ignores the tag; this policy cannot apply its matrix
    /// afterward. Tags still govern color admission and the PNG `cICP` chunk.
    /// See [the color guidance](ImageOptions#color) for receiving YUV when the
    /// tag's matrix matters, including sources with alpha.
    ///
    /// A frame whose `ndi_color_info` cannot be read, including received
    /// metadata withheld as non-UTF-8 or unterminated text, is refused with
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame), so the encoder
    /// never guesses at a frame that says something it cannot read.
    #[default]
    Sdk,
    /// Convert with this matrix and range, whatever the frame says.
    ///
    /// The caller takes responsibility for the frame's color: unreadable
    /// `ndi_color_info` and withheld received metadata are ignored, and the
    /// frame then counts as SDR with
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
    /// keep their precision. Eight-bit RGB and alpha samples scale exactly
    /// by 257; YUV sources are converted at 16-bit precision.
    /// Receivers set to [`ReceiverColorFormat::Best`] or
    /// [`ReceiverColorFormat::Best444`] get such frames from a
    /// high-bit-depth source.
    ///
    /// [`ReceiverColorFormat::Best`]: crate::ReceiverColorFormat::Best
    /// [`ReceiverColorFormat::Best444`]: crate::ReceiverColorFormat::Best444
    Sixteen,
}

impl SampleDepth {
    pub(super) const fn bytes(self) -> usize {
        match self {
            Self::Eight => 1,
            Self::Sixteen => 2,
        }
    }
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
    /// Colors are treated as straight (not premultiplied by alpha), and PNG
    /// and WebP store straight alpha. The [SDK frame-type reference] documents
    /// this for BGRA and RGBA. It does not specify the convention for UYVA,
    /// PA16, NB48 or NB4G; this encoder assumes straight color for them too.
    ///
    /// [SDK frame-type reference]: https://docs.ndi.video/all/developing-with-ndi/sdk/frame-types
    #[default]
    Keep,
    /// Write RGB, dropping any alpha.
    Discard,
}

impl AlphaMode {
    pub(super) const fn keeps_alpha(self) -> bool {
        match self {
            Self::Keep => true,
            Self::Discard => false,
        }
    }
}

/// What happens to a frame whose `ndi_color_info` gives the PQ or HLG
/// transfer function. Frames with BT.2020 primaries or either HDR transfer
/// need a color tag, which the encoder can write only in PNG.
///
/// | The frame's primaries | The frame's transfer | [`HdrPolicy::Refuse`] | [`HdrPolicy::CodeValues`] |
/// |---|---|---|---|
/// | Unspecified, BT.601 or BT.709 | Unspecified or SDR | Every format, untagged | Every format, untagged |
/// | BT.2020 | Unspecified or SDR | PNG, tagged | PNG, tagged |
/// | Any | PQ or HLG | [`Error::InvalidFrame`] | PNG of the code values, tagged |
///
/// When PNG is required, JPEG and WebP return [`Error::InvalidInput`]
/// with [`InputDefect::ColorTagNeedsPng`](crate::InputDefect::ColorTagNeedsPng).
///
/// The check applies to every pixel format, RGB as well as YUV. The encoder
/// does not tone-map. A program that wants an SDR rendering of HDR video
/// makes it itself, from a 16-bit PNG or from the frame, and
/// [`color_info`](crate::VideoFrame::color_info) lets it check a frame before
/// encoding it.
///
/// This enum is marked `#[non_exhaustive]` so that policies can be added
/// without a breaking change.
///
/// [`Error::InvalidFrame`]: crate::Error::InvalidFrame
/// [`Error::InvalidInput`]: crate::Error::InvalidInput
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum HdrPolicy {
    /// Refuse a PQ or HLG frame with
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame).
    #[default]
    Refuse,
    /// Write a PQ or HLG frame's non-linear code values to a PNG, tagged with
    /// a `cICP` chunk that tells an HDR-aware viewer their primaries and
    /// transfer function. Use [`SampleDepth::Sixteen`] to keep a
    /// high-bit-depth frame's precision.
    ///
    /// RGB-received frames already contain the SDK's BT.709 limited-range
    /// conversion, whatever `ndi_color_info` says; 16-bit output cannot recover
    /// their original HDR code values. Receive YUV with
    /// [`ReceiverColorFormat::Best`](crate::ReceiverColorFormat::Best) to keep
    /// them. The tag still decides HDR refusal and the PNG `cICP` chunk.
    ///
    /// JPEG and WebP refuse such a frame with [`Error::InvalidInput`].
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
/// Color channels are treated as straight (not premultiplied by alpha), as
/// the [NDI SDK](https://docs.ndi.video/all/developing-with-ndi/sdk/frame-types)
/// documents for BGRA and RGBA. This is an encoder assumption for UYVA, PA16,
/// NB48 and NB4G; see [`AlphaMode::Keep`].
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
/// Every level is lossless. The level selects the `png` crate's compression
/// setting; compare encoded size and CPU cost on representative content.
/// `Balanced` is the default. No setting imposes a latency or memory limit.
///
/// This enum is marked `#[non_exhaustive]` so that levels can be added
/// without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum PngCompression {
    /// The `png` crate's `Fast` compression setting.
    Fast,
    /// The `png` crate's `Balanced` compression setting.
    #[default]
    Balanced,
    /// The `png` crate's `High` compression setting.
    High,
}
