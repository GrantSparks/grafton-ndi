//! Still images from video frames: the `image-encoding` feature.
//!
//! An encode reads the frame a row at a time through one of five readers
//! ([`read`]), converts YUV to RGB in the color stage ([`color`]), averages
//! blocks when asked to reduce the image ([`downscale`]), and hands the rows
//! to PNG, JPEG or lossless WebP. Every check of the options and the frame
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

/// Encode `source` as `options` describes into `out`, writing as it encodes.
pub(crate) fn encode_to(
    source: Source<'_>,
    options: &ImageOptions,
    out: &mut dyn Write,
) -> Result<()> {
    let plan = Plan::new(&source, options)?;
    match plan.codec {
        Codec::Png {
            depth: SampleDepth::Eight,
            compression,
            cicp,
        } => png::encode(
            &mut plan.pixels::<u8>(&source),
            plan.channels(),
            compression,
            cicp,
            out,
        ),
        Codec::Png {
            depth: SampleDepth::Sixteen,
            compression,
            cicp,
        } => png::encode(
            &mut plan.pixels::<u16>(&source),
            plan.channels(),
            compression,
            cicp,
            out,
        ),
        Codec::Jpeg { quality } => jpeg::encode(&mut plan.pixels::<u8>(&source), quality, out),
        Codec::WebpLossless => webp::encode(&mut plan.pixels::<u8>(&source), plan.channels(), out),
    }
}

/// Encode `source` into a new buffer.
pub(crate) fn encode(source: Source<'_>, options: &ImageOptions) -> Result<Vec<u8>> {
    let mut image = Vec::new();
    encode_to(source, options, &mut image)?;
    Ok(image)
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
}

impl Plan {
    /// Check `options` with `source`, in this order: the options alone, then
    /// the frame's color description, its HDR transfer, and the image's size.
    fn new(source: &Source<'_>, options: &ImageOptions) -> Result<Self> {
        let format = options.format;
        if let ImageFormat::Jpeg(quality) = format {
            if !(1..=100).contains(&quality) {
                return Err(Error::InvalidInput(format!(
                    "JPEG quality must be in 1..=100, got {quality}"
                )));
            }
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
                check_size("WebP", width, height, webp::MAX_DIMENSION)?;
                Codec::WebpLossless
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

    /// The pixel pipeline for `source`.
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
