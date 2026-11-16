//! Still images from video frames: the `image-encoding` feature.
//!
//! [`ImageOptions`] selects PNG, JPEG, lossless or lossy WebP, plus color,
//! alpha, bit depth and reduction settings. Every [`VideoSource`](crate::VideoSource)
//! exposes `encode_image`, `encode_image_to` and `encode_image_data_url`.
//! [`ImageWorkspace`] reuses conversion storage across repeated encodes.
//!
//! Encoding validates the frame and options before writing output. YUV input
//! follows the SDK color conventions by default; explicit options select other
//! color handling and HDR code values.

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

use std::{
    io::{self, Write},
    marker::PhantomData,
};

use self::{
    color::Sample,
    downscale::{reduced, Downscaler},
    read::{Reader, Sink},
};
use crate::frames::VideoPartsExt;
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
    aspect: crate::PictureAspectRatio,
    metadata: color_info::Metadata<'a>,
}

impl<'a> Source<'a> {
    pub(crate) fn new<F: VideoParts + ?Sized>(frame: &'a F) -> Self {
        let layout = frame.layout();
        Self {
            width: layout.width() as usize,
            height: layout.height() as usize,
            format: layout.pixel_format(),
            planes: frame.plane_views(),
            aspect: frame.header().picture_aspect_ratio(),
            metadata: frame.color_metadata(),
        }
    }
}

/// Reusable conversion storage for still-image encoding.
///
/// Keeps one PNG row or JPEG final row, WebP's complete converted image, and downscale sums
/// at each sample depth. Capacity grows to the largest encode and remains
/// retained until [`release`](Self::release) or drop. Each concurrent worker
/// needs its own workspace. Codec state and encoded output are not retained;
/// lossy WebP still allocates its complete encoded result in the backend.
/// Eligible RGBA input is borrowed without populating conversion scratch.
/// Remembers the last successful output size for matching format settings,
/// retaining no output memory; [`release`](Self::release) resets that hint.
#[derive(Debug, Default)]
pub struct ImageWorkspace {
    row: Vec<u8>,
    image: Vec<u8>,
    sums8: Vec<[u32; 4]>,
    sums16: Vec<[u64; 4]>,
    output_hint: Option<OutputHint>,
}

#[derive(Debug, Clone, Copy)]
struct OutputHint {
    format: ImageFormat,
    length: usize,
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
    /// also reuses output capacity. Both WebP formats buffer their complete
    /// encoded result before writing; PNG and JPEG write incrementally.
    /// Success means every byte was written and the writer was flushed.
    /// Validation happens before writing; a codec/writer failure may leave
    /// partial output.
    ///
    /// ```
    /// use grafton_ndi::{PixelFormat, VideoFrame, image_encoding::{ImageWorkspace, ImageOptions, ImageFormat}};
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
        write_image(plan, &mut out, self)
    }

    /// Encode a frame as a base64 data URL, reusing conversion scratch.
    ///
    /// Validation, format and color handling match [`Self::encode`].
    pub fn encode_data_url<F: crate::VideoSource + ?Sized>(
        &mut self,
        frame: &F,
        options: &ImageOptions,
    ) -> Result<String> {
        let image = self.encode(frame, options)?;
        Ok(data_url(&image, options.format))
    }

    fn encode_source(&mut self, source: Source<'_>, options: &ImageOptions) -> Result<Vec<u8>> {
        let plan = Plan::new(&source, options)?;
        // Preserve the backend's output buffer: no second lossy WebP copy.
        if let Codec::WebpLossy {
            quality,
            effort,
            sharp_yuv,
            shape,
        } = plan.codec
        {
            return plan.with_pixels::<u8, _>(&mut self.sums8, |pixels| {
                webp::encode_lossy(pixels, shape, quality, effort, sharp_yuv, &mut self.image)
            });
        }
        let shape = plan.shape();
        // A sudden reduction must not reserve a previous large image's output.
        // This is only a hint; larger encoded results can grow normally.
        let cap = shape
            .row_bytes
            .saturating_mul(shape.height as usize)
            .saturating_mul(2)
            .saturating_add(1024);
        let hint = self
            .output_hint
            .filter(|hint| hint.format == options.format)
            .map_or(0, |hint| hint.length.min(cap));
        let mut image = OutputBuffer {
            bytes: Vec::new(),
            hint,
        };
        write_image(plan, &mut image, self)?;
        self.output_hint = Some(OutputHint {
            format: options.format,
            length: image.bytes.len(),
        });
        Ok(image.bytes)
    }
}

/// Reserve at the first write, rather than before running the codec. Lossless
/// WebP builds its complete frame before writing; delaying the output allocation
/// avoids adding it to that backend's transient compression working set.
struct OutputBuffer {
    bytes: Vec<u8>,
    hint: usize,
}
impl Write for OutputBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !bytes.is_empty() && self.hint != 0 {
            self.bytes.reserve_exact(std::mem::take(&mut self.hint));
        }
        self.bytes.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Encode `source` as `options` describes into `out`.
pub(crate) fn encode_to(
    source: Source<'_>,
    options: &ImageOptions,
    out: &mut dyn Write,
) -> Result<()> {
    let plan = Plan::new(&source, options)?;
    write_image(plan, out, &mut ImageWorkspace::new())
}

fn write_image(plan: Plan<'_>, out: &mut dyn Write, workspace: &mut ImageWorkspace) -> Result<()> {
    let mut out = KeepError { out, error: None };
    let result = match plan.codec {
        Codec::Png {
            shape,
            compression,
            cicp,
            density,
        } => match shape.depth {
            SampleDepth::Eight => plan.with_pixels::<u8, _>(&mut workspace.sums8, |pixels| {
                png::encode(
                    pixels,
                    shape,
                    compression,
                    cicp,
                    density,
                    &mut out,
                    &mut workspace.row,
                )
            }),
            SampleDepth::Sixteen => plan.with_pixels::<u16, _>(&mut workspace.sums16, |pixels| {
                png::encode(
                    pixels,
                    shape,
                    compression,
                    cicp,
                    density,
                    &mut out,
                    &mut workspace.row,
                )
            }),
        },
        Codec::Jpeg {
            quality,
            dimensions,
            density,
        } => plan.with_pixels::<u8, _>(&mut workspace.sums8, |pixels| {
            jpeg::encode(
                pixels,
                quality,
                dimensions,
                density,
                &mut workspace.row,
                &mut out,
            )
        }),
        Codec::WebpLossless { shape } => plan
            .with_pixels::<u8, _>(&mut workspace.sums8, |pixels| {
                webp::encode_lossless(pixels, shape, &mut out, &mut workspace.image)
            }),
        Codec::WebpLossy {
            quality,
            effort,
            sharp_yuv,
            shape,
        } => plan.with_pixels::<u8, _>(&mut workspace.sums8, |pixels| {
            let image = webp::encode_lossy(
                pixels,
                shape,
                quality,
                effort,
                sharp_yuv,
                &mut workspace.image,
            )?;
            out.write_all(&image).map_err(Error::Io)
        }),
    };
    let result = result.and_then(|()| out.flush().map_err(Error::Io));
    match out.error {
        Some(error) => Err(Error::Io(error)),
        None => result,
    }
}

/// Preserve the first writer failure even when a codec replaces it with text.
struct KeepError<'w> {
    out: &'w mut dyn Write,
    error: Option<io::Error>,
}

impl KeepError<'_> {
    fn keep(&mut self, error: io::Error) -> io::Error {
        // `write_all` retries interruptions; they do not end the encode.
        if error.kind() == io::ErrorKind::Interrupted {
            return error;
        }
        let copy = io::Error::new(error.kind(), error.to_string());
        self.error.get_or_insert(error);
        copy
    }
}

impl Write for KeepError<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out.write(buf).map_err(|error| self.keep(error))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush().map_err(|error| self.keep(error))
    }
}

/// Encode `source` into a new buffer.
pub(crate) fn encode(source: Source<'_>, options: &ImageOptions) -> Result<Vec<u8>> {
    ImageWorkspace::new().encode_source(source, options)
}

#[cfg(test)]
std::thread_local! {
    static FAIL_SCRATCH_RESERVATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Resize only after checked geometry; retain capacity between encodes.
fn resize_scratch<T: Default + Clone>(scratch: &mut Vec<T>, len: usize) -> Result<()> {
    let unavailable = || {
        crate::error::allocation_failed(
            crate::AllocationKind::ImageScratch,
            len.checked_mul(size_of::<T>()),
        )
    };
    #[cfg(test)]
    if len > scratch.capacity() && FAIL_SCRATCH_RESERVATION.with(|fail| fail.replace(false)) {
        return Err(unavailable());
    }
    scratch
        .try_reserve_exact(len.saturating_sub(scratch.len()))
        .map_err(|_| unavailable())?;
    scratch.resize(len, T::default());
    Ok(())
}

fn packed_len(width: usize, height: usize, channels: usize) -> Result<usize> {
    width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(channels))
        .ok_or(Error::InvalidFrame(
            crate::FrameDefect::ImageScratchOverflow,
        ))
}

/// Encode `source` as a base64 data URL, allocated at its exact final
/// length.
pub(crate) fn encode_data_url(source: Source<'_>, options: &ImageOptions) -> Result<String> {
    let image = encode(source, options)?;
    Ok(data_url(&image, options.format))
}

fn data_url(image: &[u8], format: ImageFormat) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let media_type = format.media_type();
    let encoded_len =
        base64::encoded_len(image.len(), true).expect("the base64 of a Vec's bytes fits in usize");
    let mut url = String::with_capacity("data:;base64,".len() + media_type.len() + encoded_len);
    url.push_str("data:");
    url.push_str(media_type);
    url.push_str(";base64,");
    STANDARD.encode_string(image, &mut url);
    url
}

/// What an encode resolves its options and frame to. Building one makes
/// every check, so an encode that has a plan writes a whole image unless its
/// writer or codec fails.
struct Plan<'a> {
    source: &'a Source<'a>,
    codec: Codec,
    matrix: YuvMatrix,
    range: YuvRange,
    alpha: bool,
    shift: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channels {
    Rgb,
    Rgba,
}

impl Channels {
    const fn count(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }

    const fn from_alpha(alpha: bool) -> Self {
        if alpha {
            Self::Rgba
        } else {
            Self::Rgb
        }
    }
}

/// Checked row geometry. Row codecs do not require a whole-image product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageShape {
    width: u32,
    height: u32,
    channels: Channels,
    depth: SampleDepth,
    row_bytes: usize,
}

impl ImageShape {
    fn new(width: usize, height: usize, channels: Channels, depth: SampleDepth) -> Result<Self> {
        let row_bytes = packed_len(width, 1, channels.count() * depth.bytes())?;
        let dimension = |value| {
            u32::try_from(value)
                .map_err(|_| Error::InvalidFrame(crate::FrameDefect::ImageDimensionOverflow))
        };
        Ok(Self {
            width: dimension(width)?,
            height: dimension(height)?,
            channels,
            depth,
            row_bytes,
        })
    }

    fn buffered(self) -> Result<BufferedImageShape> {
        let image_bytes =
            self.row_bytes
                .checked_mul(self.height as usize)
                .ok_or(Error::InvalidFrame(
                    crate::FrameDefect::ImageScratchOverflow,
                ))?;
        Ok(BufferedImageShape {
            rows: self,
            image_bytes,
        })
    }
}

/// Whole-image codecs additionally carry their checked allocation size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BufferedImageShape {
    rows: ImageShape,
    image_bytes: usize,
}

/// Positive relative pixel densities. PNG and JFIF both store density, so
/// physical pixel width/height is y/x. A common u16 bound keeps both outputs
/// consistent; ratios outside its representable range use the nearest endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PixelDensity {
    x: u16,
    y: u16,
}
impl PixelDensity {
    fn for_source(source: &Source<'_>, width: usize, height: usize) -> Option<Self> {
        let aspect = f64::from(source.aspect.get());
        // Zero explicitly requests square pixels. Explicit square-pixel ratios
        // also stay untagged, including rounding from the SDK's f32 field.
        let source_pixel_aspect = aspect * source.height as f64 / source.width as f64;
        if aspect == 0.0 || (source_pixel_aspect - 1.0).abs() <= f64::from(f32::EPSILON) {
            return None;
        }
        // Use the actual reduced shape: ceil division can change the stored
        // width/height ratio when the final reduction block is partial.
        let pixel_aspect = aspect * height as f64 / width as f64;
        let maximum = f64::from(u16::MAX);
        let (x, y) = if pixel_aspect >= 1.0 {
            (
                (maximum / pixel_aspect).round().clamp(1.0, maximum) as u16,
                u16::MAX,
            )
        } else {
            (
                u16::MAX,
                (maximum * pixel_aspect).round().clamp(1.0, maximum) as u16,
            )
        };
        let (mut a, mut b) = (x, y);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        Some(Self { x: x / a, y: y / a })
    }
}

/// The codec and its settings, valid for the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Png {
        shape: ImageShape,
        compression: PngCompression,
        /// The `cICP` chunk's primaries, transfer, matrix and range flag.
        cicp: Option<[u8; 4]>,
        density: Option<PixelDensity>,
    },
    Jpeg {
        quality: jpeg::JpegQuality,
        dimensions: jpeg::JpegDimensions,
        density: Option<PixelDensity>,
    },
    /// Dimensions within WebP's limit.
    WebpLossless { shape: BufferedImageShape },
    /// Quality in 0 to 100, dimensions within lossy WebP's limit.
    WebpLossy {
        shape: BufferedImageShape,
        quality: u8,
        effort: u8,
        sharp_yuv: bool,
    },
}

impl<'a> Plan<'a> {
    /// Check `options` with `source`, in this order: the options alone, then
    /// the frame's color description, its HDR transfer, and the image's size.
    fn new(source: &'a Source<'a>, options: &ImageOptions) -> Result<Self> {
        let format = options.format;
        format.validate_quality()?;
        let depth = options.depth;
        if matches!(format, ImageFormat::WebpLossy(_)) && options.webp_effort > 6 {
            return Err(Error::InvalidInput(crate::InputDefect::WebpEffort {
                effort: options.webp_effort,
            }));
        }
        if depth == SampleDepth::Sixteen && format != ImageFormat::Png {
            return Err(Error::InvalidInput(
                crate::InputDefect::SixteenBitNeedsPng {
                    format: format.name(),
                },
            ));
        }

        // The caller who overrides the conversion takes responsibility for
        // the frame's color: unreadable metadata then counts as none.
        let color = match options.color {
            ColorPolicy::Sdk => source.metadata.read()?,
            ColorPolicy::Override { .. } => source.metadata.read().ok().flatten(),
        };
        if let Some(transfer) = color.and_then(|color| hdr_transfer(&color)) {
            match options.hdr {
                HdrPolicy::Refuse => {
                    return Err(Error::InvalidFrame(crate::FrameDefect::HdrRefused {
                        transfer,
                    }))
                }
                HdrPolicy::CodeValues if format != ImageFormat::Png => {
                    return Err(Error::InvalidInput(crate::InputDefect::HdrNeedsPng {
                        format: format.name(),
                        transfer,
                    }))
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
        let alpha = options.alpha.keeps_alpha() && !matches!(format, ImageFormat::Jpeg(_));
        let channels = match format {
            ImageFormat::WebpLossy(_) => Channels::Rgba,
            ImageFormat::Png | ImageFormat::Jpeg(_) | ImageFormat::WebpLossless => {
                Channels::from_alpha(alpha)
            }
        };
        let depth = match format {
            ImageFormat::Png => depth,
            ImageFormat::Jpeg(_) | ImageFormat::WebpLossless | ImageFormat::WebpLossy(_) => {
                SampleDepth::Eight
            }
        };
        // Preserve codec dimension diagnostics before checking conversion storage.
        match format {
            ImageFormat::Png => {}
            ImageFormat::Jpeg(_) => {
                check_size(crate::ImageCodec::Jpeg, width, height, jpeg::MAX_DIMENSION)?
            }
            ImageFormat::WebpLossless => check_size(
                crate::ImageCodec::Webp,
                width,
                height,
                webp::MAX_LOSSLESS_DIMENSION,
            )?,
            ImageFormat::WebpLossy(_) => check_size(
                crate::ImageCodec::Webp,
                width,
                height,
                webp::MAX_LOSSY_DIMENSION,
            )?,
        }
        let shape = ImageShape::new(width, height, channels, depth)?;
        let density = PixelDensity::for_source(source, width, height);
        let codec = match format {
            ImageFormat::Png => Codec::Png {
                shape,
                compression: options.png_compression,
                cicp: color.and_then(|color| cicp(&color)),
                density,
            },
            ImageFormat::Jpeg(quality) => Codec::Jpeg {
                quality: jpeg::JpegQuality::new(quality)?,
                dimensions: jpeg::JpegDimensions::new(width, height)?,
                density,
            },
            ImageFormat::WebpLossless => Codec::WebpLossless {
                shape: shape.buffered()?,
            },
            ImageFormat::WebpLossy(quality) => Codec::WebpLossy {
                shape: shape.buffered()?,
                quality,
                effort: options.webp_effort,
                sharp_yuv: options.webp_sharp_yuv,
            },
        };
        if shift > 0 {
            packed_len(
                width,
                1,
                if depth == SampleDepth::Sixteen {
                    32
                } else {
                    16
                },
            )?;
        }
        Ok(Self {
            source,
            codec,
            matrix,
            range,
            alpha,
            shift,
        })
    }

    fn shape(&self) -> ImageShape {
        match self.codec {
            Codec::Png { shape, .. } => shape,
            Codec::Jpeg { dimensions, .. } => dimensions.shape(),
            Codec::WebpLossless { shape } | Codec::WebpLossy { shape, .. } => shape.rows,
        }
    }

    #[cfg(test)]
    fn channels(&self) -> Channels {
        self.shape().channels
    }

    /// Lend the retained sums to a single encode, restoring them even when
    /// the codec or writer returns an error. A panic may drop scratch but
    /// leaves the workspace valid for reuse.
    fn with_pixels<P: Sample, T>(
        &self,
        sums: &mut Vec<[P::Sum; 4]>,
        encode: impl FnOnce(&mut Pixels<'_, P>) -> Result<T>,
    ) -> Result<T> {
        let reader = Reader::new(self.source, self.matrix, self.range, self.alpha);
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
    fn pixels<P: Sample>(&self) -> Pixels<'a, P> {
        let reader = Reader::new(self.source, self.matrix, self.range, self.alpha);
        let downscaler = (self.shift > 0).then(|| Downscaler::new(&reader, self.shift));
        Pixels { reader, downscaler }
    }
}

/// The PQ or HLG transfer of a color description, named for messages.
fn hdr_transfer(color: &ColorInfo) -> Option<crate::TransferFunction> {
    color.transfer().filter(|transfer| transfer.is_hdr())
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
    let transfer = color.transfer().map_or(1, TransferFunction::h273_code);
    let primaries = color.primaries().map_or(2, ColorPrimaries::h273_code);
    (primaries == 9 || color.is_hdr()).then_some([primaries, transfer, 0, 1])
}

/// Refuse an image larger than its codec can write.
fn check_size(codec: crate::ImageCodec, width: usize, height: usize, max: usize) -> Result<()> {
    for (dimension, length) in [
        (crate::ImageDimension::Width, width),
        (crate::ImageDimension::Height, height),
    ] {
        if length > max {
            return Err(Error::InvalidFrame(crate::FrameDefect::ImageDimension {
                codec,
                dimension,
                length,
                maximum: max,
            }));
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
    #[cfg(test)]
    fn shift(&self) -> u32 {
        self.downscaler.as_ref().map_or(0, Downscaler::shift)
    }

    #[cfg(test)]
    fn width(&self) -> usize {
        reduced(self.reader.width(), self.shift())
    }

    #[cfg(test)]
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
    fn pack_row(&mut self, y: usize, components: Channels, row: &mut [u8]) {
        match components {
            Channels::Rgba => self.row(y, &mut Packed::<P, 4>::new(row)),
            Channels::Rgb => self.row(y, &mut Packed::<P, 3>::new(row)),
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
