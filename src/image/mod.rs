//! Still images from video frames: the `image-encoding` feature.
//!
//! An encode reads the frame a row at a time through one of five readers
//! ([`read`]), converts YUV to RGB in the color stage ([`color`]), averages
//! blocks when asked to reduce the image ([`downscale`]), and hands the rows
//! to PNG, JPEG or WebP. Every check of the options and the frame
//! happens in [`Plan::new`], before anything is written.

mod color;
mod downscale;
mod jpeg;
mod options;
mod png;
mod read;
mod webp;

#[cfg(test)]
mod tests;

pub use options::{
    AlphaMode, ColorPolicy, Downscale, HdrPolicy, ImageFormat, ImageOptions, PngCompression,
    SampleDepth, YuvRange,
};

use std::{io::Write, marker::PhantomData};

use self::{
    color::Sample,
    downscale::{reduced, Downscaler},
    read::{Reader, Sink},
};
use crate::{
    color_info, frames::VideoParts, ColorInfo, ColorPrimaries, Error, PixelFormat, Result,
    TransferFunction, VideoPlane, VideoPlanes, YuvMatrix,
};

/// The parts of a video frame an encode reads, borrowed from any frame type.
pub(crate) struct Source<'a> {
    width: usize,
    height: usize,
    format: PixelFormat,
    planes: VideoPlanes<'a>,
    metadata: Option<&'a str>,
}

impl<'a> Source<'a> {
    pub(crate) fn new<F: VideoParts + ?Sized>(frame: &'a F) -> Self {
        let layout = frame.layout();
        Self {
            width: layout.width as usize,
            height: layout.height as usize,
            format: layout.pixel_format,
            planes: frame.plane_views(),
            metadata: frame.metadata_text().map(|text| text.as_str()),
        }
    }
}

/// Reusable conversion storage for still-image encoding.
///
/// Keeps one PNG row, WebP's complete converted image, and downscale sums
/// at each sample depth. Capacity grows to the largest encode and remains
/// retained until [`release`](Self::release) or drop. Each concurrent worker
/// needs its own workspace. Codec state and encoded output are not retained;
/// lossy WebP still allocates its complete encoded result in the backend.
/// Eligible RGBA input is borrowed without populating conversion scratch.
#[derive(Debug, Default)]
pub struct ImageWorkspace {
    row: Vec<u8>,
    image: Vec<u8>,
    sums8: Vec<[u32; 4]>,
    sums16: Vec<[u64; 4]>,
}

impl ImageWorkspace {
    /// Create an empty workspace without allocating.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes retained in wrapper scratch, excluding source, codec and output.
    pub fn retained_bytes(&self) -> usize {
        self.row.capacity()
            + self.image.capacity()
            + self.sums8.capacity() * std::mem::size_of::<[u32; 4]>()
            + self.sums16.capacity() * std::mem::size_of::<[u64; 4]>()
    }

    /// Release all retained scratch. The workspace remains usable.
    pub fn release(&mut self) {
        *self = Self::new();
    }

    /// Encode a frame into a newly allocated result, reusing conversion scratch.
    ///
    /// Uses the same options, validation and errors as `frame.encode_image`.
    /// All frame types implementing [`crate::VideoSource`] are accepted.
    pub fn encode<F: crate::VideoSource + ?Sized>(
        &mut self,
        frame: &F,
        options: &ImageOptions,
    ) -> Result<Vec<u8>> {
        self.encode_source(Source::new(frame), options)
    }

    /// Encode into a writer, reusing conversion scratch. A cleared `Vec<u8>`
    /// also reuses output capacity. Lossy WebP buffers its complete result
    /// before writing; other codecs write incrementally. Validation happens
    /// before writing; a codec/writer failure may leave partial output.
    ///
    /// ```
    /// use grafton_ndi::{ImageWorkspace, ImageOptions, ImageFormat, PixelFormat, VideoFrame};
    /// let frame = VideoFrame::builder().resolution(2, 2)
    ///     .pixel_format(PixelFormat::Bgra).data(vec![255; 16]).build()?;
    /// let mut workspace = ImageWorkspace::new();
    /// let mut output = Vec::new();
    /// for _ in 0..3 {
    ///     output.clear();
    ///     workspace.encode_to(&frame, &ImageOptions::new(ImageFormat::WebpLossless), &mut output)?;
    /// }
    /// workspace.release();
    /// # Ok::<(), grafton_ndi::Error>(())
    /// ```
    pub fn encode_to<F: crate::VideoSource + ?Sized, W: Write>(
        &mut self,
        frame: &F,
        options: &ImageOptions,
        mut out: W,
    ) -> Result<()> {
        let source = Source::new(frame);
        let plan = Plan::new(&source, options)?;
        write_image(&source, plan, &mut out, self)
    }

    fn encode_source(&mut self, source: Source<'_>, options: &ImageOptions) -> Result<Vec<u8>> {
        let plan = Plan::new(&source, options)?;
        // Preserve the backend's output buffer: no second lossy WebP copy.
        if let Codec::WebpLossy {
            quality,
            effort,
            sharp_yuv,
        } = plan.codec
        {
            return plan.with_pixels::<u8, _>(&source, &mut self.sums8, |pixels| {
                webp::encode_lossy(pixels, quality, effort, sharp_yuv, &mut self.image)
            });
        }
        let mut image = Vec::new();
        write_image(&source, plan, &mut image, self)?;
        Ok(image)
    }
}

/// Encode `source` as `options` describes into `out`.
pub(crate) fn encode_to(
    source: Source<'_>,
    options: &ImageOptions,
    out: &mut dyn Write,
) -> Result<()> {
    let plan = Plan::new(&source, options)?;
    write_image(&source, plan, out, &mut ImageWorkspace::new())
}

fn write_image(
    source: &Source<'_>,
    plan: Plan,
    out: &mut dyn Write,
    workspace: &mut ImageWorkspace,
) -> Result<()> {
    match plan.codec {
        Codec::Png {
            depth: SampleDepth::Sixteen,
            compression,
            cicp,
        } => plan.with_pixels::<u16, _>(source, &mut workspace.sums16, |pixels| {
            png::encode(
                pixels,
                plan.channels(),
                compression,
                cicp,
                out,
                &mut workspace.row,
            )
        }),
        codec => plan.with_pixels::<u8, _>(source, &mut workspace.sums8, |pixels| match codec {
            Codec::Png {
                compression, cicp, ..
            } => png::encode(
                pixels,
                plan.channels(),
                compression,
                cicp,
                out,
                &mut workspace.row,
            ),
            Codec::Jpeg { quality } => jpeg::encode(pixels, quality, out),
            Codec::WebpLossless => {
                webp::encode_lossless(pixels, plan.channels(), out, &mut workspace.image)
            }
            Codec::WebpLossy {
                quality,
                effort,
                sharp_yuv,
            } => {
                let image =
                    webp::encode_lossy(pixels, quality, effort, sharp_yuv, &mut workspace.image)?;
                out.write_all(&image).map_err(Error::Io)
            }
        }),
    }
}

/// Encode `source` into a new buffer.
pub(crate) fn encode(source: Source<'_>, options: &ImageOptions) -> Result<Vec<u8>> {
    ImageWorkspace::new().encode_source(source, options)
}

/// Resize only after checked geometry; retain capacity between encodes.
fn resize_scratch<T: Default + Clone>(scratch: &mut Vec<T>, len: usize) -> Result<()> {
    scratch
        .try_reserve_exact(len.saturating_sub(scratch.len()))
        .map_err(|error| {
            Error::InvalidFrame(format!("image scratch allocation failed: {error}"))
        })?;
    scratch.resize(len, T::default());
    Ok(())
}

fn packed_len(width: usize, height: usize, channels: usize) -> Result<usize> {
    width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(channels))
        .ok_or_else(|| Error::InvalidFrame("image scratch size overflow".into()))
}

/// Encode `source` as a base64 data URL, allocated at its exact final
/// length.
pub(crate) fn encode_data_url(source: Source<'_>, options: &ImageOptions) -> Result<String> {
    use base64::{engine::general_purpose::STANDARD, Engine};

    let image = encode(source, options)?;
    let media_type = options.format.media_type();
    let encoded_len =
        base64::encoded_len(image.len(), true).expect("the base64 of a Vec's bytes fits in usize");
    let mut url = String::with_capacity("data:;base64,".len() + media_type.len() + encoded_len);
    url.push_str("data:");
    url.push_str(media_type);
    url.push_str(";base64,");
    STANDARD.encode_string(&image, &mut url);
    Ok(url)
}

/// What an encode resolves its options and frame to. Building one makes
/// every check, so an encode that has a plan writes a whole image unless its
/// writer or codec fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Plan {
    codec: Codec,
    /// The conversion of a YUV frame.
    matrix: YuvMatrix,
    range: YuvRange,
    /// Whether the image has an alpha channel.
    alpha: bool,
    /// log2 of the reduction.
    shift: u32,
}

/// The codec and its settings, valid for the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Png {
        depth: SampleDepth,
        compression: PngCompression,
        /// The `cICP` chunk's primaries, transfer, matrix and range flag.
        cicp: Option<[u8; 4]>,
    },
    /// Quality in 1 to 100, dimensions within JPEG's limit.
    Jpeg { quality: u8 },
    /// Dimensions within WebP's limit.
    WebpLossless,
    /// Quality in 0 to 100, dimensions within lossy WebP's limit.
    WebpLossy {
        quality: u8,
        effort: u8,
        sharp_yuv: bool,
    },
}

impl Plan {
    /// Check `options` with `source`, in this order: the options alone, then
    /// the frame's color description, its HDR transfer, and the image's size.
    fn new(source: &Source<'_>, options: &ImageOptions) -> Result<Self> {
        let format = options.format;
        let quality_range = match format {
            ImageFormat::Jpeg(quality) => Some((quality, 1)),
            ImageFormat::WebpLossy(quality) => Some((quality, 0)),
            _ => None,
        };
        if let Some((quality, min)) = quality_range {
            if !(min..=100).contains(&quality) {
                return Err(Error::InvalidInput(format!(
                    "{} quality must be in {min}..=100, got {quality}",
                    format.name()
                )));
            }
        }
        if matches!(format, ImageFormat::WebpLossy(_)) && options.webp_effort > 6 {
            return Err(Error::InvalidInput(format!(
                "WebP effort must be in 0..=6, got {}",
                options.webp_effort
            )));
        }
        if options.depth == SampleDepth::Sixteen && format != ImageFormat::Png {
            return Err(Error::InvalidInput(format!(
                "{} images are 8 bits a channel; SampleDepth::Sixteen needs ImageFormat::Png",
                format.name()
            )));
        }

        // The caller who overrides the conversion takes responsibility for
        // the frame's color: unreadable metadata then counts as none.
        let color = match options.color {
            ColorPolicy::Sdk => color_info::parse(source.metadata)?,
            ColorPolicy::Override { .. } => color_info::parse(source.metadata).ok().flatten(),
        };
        if let Some(transfer) = color.and_then(|color| hdr_transfer(&color)) {
            match options.hdr {
                HdrPolicy::Refuse => {
                    return Err(Error::InvalidFrame(format!(
                        "The frame's ndi_color_info gives the {transfer} transfer function; \
                         HdrPolicy::CodeValues encodes its code values to a tagged PNG"
                    )))
                }
                HdrPolicy::CodeValues if format != ImageFormat::Png => {
                    return Err(Error::InvalidInput(format!(
                        "{} cannot say that its values are {transfer} code values; \
                         HdrPolicy::CodeValues needs ImageFormat::Png",
                        format.name()
                    )))
                }
                HdrPolicy::CodeValues => {}
            }
        }
        let (matrix, range) = match options.color {
            ColorPolicy::Sdk => (
                color
                    .and_then(|color| color.matrix())
                    .unwrap_or(YuvMatrix::Bt709),
                YuvRange::Limited,
            ),
            ColorPolicy::Override { matrix, range } => (matrix, range),
        };

        let shift = options.downscale.shift();
        let (width, height) = (reduced(source.width, shift), reduced(source.height, shift));
        let codec = match format {
            ImageFormat::Png => Codec::Png {
                depth: options.depth,
                compression: options.png_compression,
                cicp: color.and_then(|color| cicp(&color)),
            },
            ImageFormat::Jpeg(quality) => {
                check_size("JPEG", width, height, jpeg::MAX_DIMENSION)?;
                Codec::Jpeg { quality }
            }
            ImageFormat::WebpLossless => {
                check_size("WebP", width, height, webp::MAX_LOSSLESS_DIMENSION)?;
                Codec::WebpLossless
            }
            ImageFormat::WebpLossy(quality) => {
                check_size("WebP", width, height, webp::MAX_LOSSY_DIMENSION)?;
                Codec::WebpLossy {
                    quality,
                    effort: options.webp_effort,
                    sharp_yuv: options.webp_sharp_yuv,
                }
            }
        };
        Ok(Self {
            codec,
            matrix,
            range,
            alpha: options.alpha == AlphaMode::Keep && !matches!(codec, Codec::Jpeg { .. }),
            shift,
        })
    }

    /// The image's channels: RGBA or RGB.
    fn channels(&self) -> usize {
        if self.alpha {
            4
        } else {
            3
        }
    }

    /// Lend the retained sums to a single encode, restoring them even when
    /// the codec or writer returns an error. A panic may drop scratch but
    /// leaves the workspace valid for reuse.
    fn with_pixels<P: Sample, T>(
        &self,
        source: &Source<'_>,
        sums: &mut Vec<[P::Sum; 4]>,
        encode: impl FnOnce(&mut Pixels<'_, P>) -> Result<T>,
    ) -> Result<T> {
        let reader = Reader::new(source, self.matrix, self.range, self.alpha);
        let downscaler = if self.shift > 0 {
            resize_scratch(sums, reduced(reader.width(), self.shift))?;
            Some(Downscaler::with_sums(
                &reader,
                self.shift,
                std::mem::take(sums),
            ))
        } else {
            None
        };
        let mut pixels = Pixels { reader, downscaler };
        let result = encode(&mut pixels);
        if let Some(downscaler) = pixels.downscaler {
            *sums = downscaler.into_sums();
        }
        result
    }

    /// The pixel pipeline for tests of individual conversion stages.
    #[cfg(test)]
    fn pixels<'a, P: Sample>(&self, source: &Source<'a>) -> Pixels<'a, P> {
        let reader = Reader::new(source, self.matrix, self.range, self.alpha);
        let downscaler = (self.shift > 0).then(|| Downscaler::new(&reader, self.shift));
        Pixels { reader, downscaler }
    }
}

/// The PQ or HLG transfer of a color description, named for messages.
fn hdr_transfer(color: &ColorInfo) -> Option<&'static str> {
    match color.transfer()? {
        TransferFunction::Pq => Some("PQ"),
        TransferFunction::Hlg => Some("HLG"),
        _ => None,
    }
}

/// The `cICP` chunk of a PNG whose colors an untagged viewer would show
/// wrongly: BT.2020 primaries, or a PQ or HLG transfer. BT.601 and BT.709
/// SDR stay untagged, since viewers assume sRGB for them.
///
/// Code points are ITU-T H.273's. Primaries: BT.709 1, BT.601 6, BT.2020 9,
/// and 2, unspecified, for an HDR frame that does not give them. Transfer:
/// PQ 16, HLG 18, and 1 for every SDR transfer, which H.273 defines as one
/// curve. The matrix is 0, the only one PNG allows for RGB, and the range
/// flag 1, because the RGB written is full range.
fn cicp(color: &ColorInfo) -> Option<[u8; 4]> {
    let transfer = match color.transfer() {
        Some(TransferFunction::Pq) => 16,
        Some(TransferFunction::Hlg) => 18,
        _ => 1,
    };
    let primaries = match color.primaries() {
        Some(ColorPrimaries::Bt709) => 1,
        Some(ColorPrimaries::Bt601) => 6,
        Some(ColorPrimaries::Bt2020) => 9,
        None => 2,
    };
    (primaries == 9 || color.is_hdr()).then_some([primaries, transfer, 0, 1])
}

/// Refuse an image larger than its codec can write.
fn check_size(codec: &str, width: usize, height: usize, max: usize) -> Result<()> {
    for (dimension, length) in [("width", width), ("height", height)] {
        if length > max {
            return Err(Error::InvalidFrame(format!(
                "{codec} {dimension} {length} exceeds maximum supported value {max}"
            )));
        }
    }
    Ok(())
}

/// The converted pixels of one encode, a row at a time: the reader alone,
/// or through the downscaler.
struct Pixels<'a, P: Sample> {
    reader: Reader<'a, P>,
    downscaler: Option<Downscaler<P>>,
}

impl<'a, P: Sample> Pixels<'a, P> {
    /// log2 of the reduction.
    fn shift(&self) -> u32 {
        self.downscaler.as_ref().map_or(0, Downscaler::shift)
    }

    fn width(&self) -> usize {
        reduced(self.reader.width(), self.shift())
    }

    fn height(&self) -> usize {
        reduced(self.reader.height(), self.shift())
    }

    /// The frame's own rows when they are exactly the image's: an 8-bit RGBA
    /// image of an RGBA frame at full size.
    fn rgba_rows(&self) -> Option<VideoPlane<'a>> {
        match self.downscaler {
            Some(_) => None,
            None => self.reader.rgba_rows(),
        }
    }

    /// Convert row `y` into `sink`.
    fn row(&mut self, y: usize, sink: &mut impl Sink<P>) {
        match &mut self.downscaler {
            Some(downscaler) => downscaler.row(&self.reader, y, sink),
            None => self.reader.row(y, sink),
        }
    }

    /// Convert row `y` into `row`, packed as `channels` samples a pixel in
    /// PNG's byte order: big-endian at 16 bits.
    fn pack_row(&mut self, y: usize, channels: usize, row: &mut [u8]) {
        match channels {
            4 => self.row(y, &mut Packed::<P, 4>::new(row)),
            _ => self.row(y, &mut Packed::<P, 3>::new(row)),
        }
    }
}

/// Packs pixels into a byte row: `CH` samples of `P` a pixel, big-endian.
struct Packed<'r, P, const CH: usize> {
    row: &'r mut [u8],
    /// Where the next pixel goes.
    at: usize,
    sample: PhantomData<P>,
}

impl<'r, P: Sample, const CH: usize> Packed<'r, P, CH> {
    fn new(row: &'r mut [u8]) -> Self {
        Self {
            row,
            at: 0,
            sample: PhantomData,
        }
    }
}

impl<P: Sample, const CH: usize> Sink<P> for Packed<'_, P, CH> {
    #[inline]
    fn put(&mut self, pixels: &[[P; 4]]) {
        let len = pixels.len() * CH * P::BYTES;
        P::pack::<CH>(pixels, &mut self.row[self.at..self.at + len]);
        self.at += len;
    }
}
