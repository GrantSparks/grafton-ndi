//! JPEG: 8-bit YCbCr through `jpeg-encoder`, with pixels converted straight
//! into the codec's component rows.

use std::{
    cell::{Cell, RefCell},
    io::Write,
};

use super::{
    read::{Sink, BATCH},
    Channels, ImageShape, Pixels, SampleDepth,
};
use crate::{Error, Result};

/// JPEG's largest width or height.
pub(super) const MAX_DIMENSION: usize = u16::MAX as usize;

/// Only this module can construct or inspect the checked quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct JpegQuality(u8);
impl JpegQuality {
    pub(super) fn new(value: u8) -> Result<Self> {
        super::ImageFormat::Jpeg(value).validate_quality()?;
        Ok(Self(value))
    }
}

/// JPEG geometry has one representation, narrowed by checked conversions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct JpegDimensions {
    width: u16,
    height: u16,
}
impl JpegDimensions {
    pub(super) fn new(width: usize, height: usize) -> Result<Self> {
        let narrow = |length, dimension| {
            u16::try_from(length).map_err(|_| {
                Error::InvalidFrame(crate::FrameDefect::ImageDimension {
                    codec: crate::ImageCodec::Jpeg,
                    dimension,
                    length,
                    maximum: MAX_DIMENSION,
                })
            })
        };
        Ok(Self {
            width: narrow(width, crate::ImageDimension::Width)?,
            height: narrow(height, crate::ImageDimension::Height)?,
        })
    }
    pub(super) fn shape(self) -> ImageShape {
        ImageShape {
            width: self.width.into(),
            height: self.height.into(),
            channels: Channels::Rgb,
            depth: SampleDepth::Eight,
            row_bytes: usize::from(self.width) * 3,
        }
    }
}

/// Encode `pixels` as a JPEG of `quality` into `out`. The plan checked the
/// quality and the dimensions.
pub(super) fn encode(
    pixels: &mut Pixels<'_, u8>,
    quality: JpegQuality,
    dimensions: JpegDimensions,
    density: Option<super::PixelDensity>,
    cache: &mut Vec<u8>,
    out: &mut dyn Write,
) -> Result<()> {
    let JpegDimensions { width, height } = dimensions;
    // Plan validation precedes this function, including metadata/HDR checks.
    // VideoPlane::data exposes only initialized contiguous active pixels;
    // padded native rows must stay on the row path. Both color types ignore
    // the fourth byte, including RGBX/BGRX's unspecified alpha.
    if pixels.downscaler.is_none() {
        if let Some((plane, rgb)) = pixels.reader.rgb_rows() {
            if let Some(data) = plane.data() {
                let color = if rgb {
                    jpeg_encoder::ColorType::Rgba
                } else {
                    jpeg_encoder::ColorType::Bgra
                };
                return encoder(out, quality, density)
                    .encode(data, width, height, color)
                    .map_err(encoding_error);
            }
        }
    }
    encode_rows(pixels, quality, dimensions, density, cache, out)
}

fn encode_rows(
    pixels: &mut Pixels<'_, u8>,
    quality: JpegQuality,
    dimensions: JpegDimensions,
    density: Option<super::PixelDensity>,
    cache: &mut Vec<u8>,
    out: &mut dyn Write,
) -> Result<()> {
    let JpegDimensions { width, height } = dimensions;
    super::resize_scratch(cache, dimensions.shape().row_bytes)?;
    let image = Rows::new(pixels, width, height, cache);
    encoder(out, quality, density)
        .encode_image(image)
        .map_err(encoding_error)
}

fn encoder(
    out: &mut dyn Write,
    quality: JpegQuality,
    density: Option<super::PixelDensity>,
) -> jpeg_encoder::Encoder<&mut dyn Write> {
    let mut encoder = jpeg_encoder::Encoder::new(out, quality.0);
    if let Some(density) = density {
        encoder.set_density(jpeg_encoder::PixelDensity {
            density: (density.x, density.y),
            unit: jpeg_encoder::PixelDensityUnit::PixelAspectRatio,
        });
    }
    encoder
}

fn encoding_error(error: jpeg_encoder::EncodingError) -> Error {
    match error {
        jpeg_encoder::EncodingError::IoError(error) => Error::Io(error),
        error => Error::InvalidFrame(crate::FrameDefect::EncodingFailed {
            codec: crate::ImageCodec::Jpeg,
            diagnostic: error.to_string().into_boxed_str(),
        }),
    }
}

/// The JPEG backend repeats the final row to pad its last block. Retain only
/// that row's converted components; every other row still converts directly
/// into the codec's buffers without an intermediate copy.
pub(super) struct Rows<'p, 'a> {
    pixels: RefCell<&'p mut Pixels<'a, u8>>,
    cache: RefCell<&'p mut [u8]>,
    last_filled: Cell<bool>,
    width: u16,
    height: u16,
}

impl<'p, 'a> Rows<'p, 'a> {
    pub(super) fn new(
        pixels: &'p mut Pixels<'a, u8>,
        width: u16,
        height: u16,
        cache: &'p mut [u8],
    ) -> Self {
        Self {
            pixels: RefCell::new(pixels),
            cache: RefCell::new(cache),
            last_filled: Cell::new(false),
            width,
            height,
        }
    }
}

impl jpeg_encoder::ImageBuffer for Rows<'_, '_> {
    fn get_jpeg_color_type(&self) -> jpeg_encoder::JpegColorType {
        jpeg_encoder::JpegColorType::Ycbcr
    }

    fn width(&self) -> u16 {
        self.width
    }

    fn height(&self) -> u16 {
        self.height
    }

    fn fill_buffers(&self, y: u16, buffers: &mut [Vec<u8>; 4]) {
        if y != self.height - 1 {
            self.pixels
                .borrow_mut()
                .row(usize::from(y), &mut Components(buffers));
            return;
        }
        let mut cache = self.cache.borrow_mut();
        if !self.last_filled.get() {
            let width = usize::from(self.width);
            let (luma, chroma) = cache.split_at_mut(width);
            let (cb, cr) = chroma.split_at_mut(width);
            self.pixels.borrow_mut().row(
                usize::from(y),
                &mut CachedComponents {
                    rows: [luma, cb, cr],
                    at: 0,
                },
            );
            self.last_filled.set(true);
        }
        for (buffer, row) in buffers
            .iter_mut()
            .zip(cache.chunks_exact(usize::from(self.width)))
        {
            buffer.extend_from_slice(row);
        }
    }
}

/// Appends pixels to the codec's Y, Cb and Cr rows, converted with the
/// codec's own JFIF conversion and dropping alpha.
struct Components<'b>(&'b mut [Vec<u8>; 4]);

impl Sink<u8> for Components<'_> {
    #[inline]
    fn put(&mut self, pixels: &[[u8; 4]]) {
        // A batch converts into arrays first, so the conversion vectorizes
        // and each component row grows once a batch.
        let mut components = [[0; BATCH]; 3];
        for (i, &[red, green, blue, _]) in pixels.iter().enumerate() {
            let (y, cb, cr) = jpeg_encoder::rgb_to_ycbcr(red, green, blue);
            components[0][i] = y;
            components[1][i] = cb;
            components[2][i] = cr;
        }
        for (buffer, values) in self.0.iter_mut().zip(&components) {
            buffer.extend_from_slice(&values[..pixels.len()]);
        }
    }
}

struct CachedComponents<'r> {
    rows: [&'r mut [u8]; 3],
    at: usize,
}

impl Sink<u8> for CachedComponents<'_> {
    fn put(&mut self, pixels: &[[u8; 4]]) {
        let mut components = [[0; BATCH]; 3];
        for (i, &[red, green, blue, _]) in pixels.iter().enumerate() {
            let (y, cb, cr) = jpeg_encoder::rgb_to_ycbcr(red, green, blue);
            components[0][i] = y;
            components[1][i] = cb;
            components[2][i] = cr;
        }
        for (row, values) in self.rows.iter_mut().zip(&components) {
            row[self.at..self.at + pixels.len()].copy_from_slice(&values[..pixels.len()]);
        }
        self.at += pixels.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        image_encoding::{ImageFormat, ImageOptions},
        image_encoding::{Plan, Source},
        PixelFormat, VideoFrame,
    };

    fn decode(bytes: &[u8], size: (u16, u16)) -> Vec<u8> {
        let mut decoder = jpeg_decoder::Decoder::new(bytes);
        let decoded = decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((info.width, info.height), size);
        assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
        decoded
    }

    #[test]
    fn eighth_size_jpeg_reads_each_source_row_once_including_its_partial_last_band() {
        for height in [1080, 1081, 2160] {
            let frame = VideoFrame::builder()
                .resolution(18, height)
                .pixel_format(PixelFormat::Rgba)
                .data(vec![127; 18 * height as usize * 4])
                .build()
                .unwrap();
            let options = ImageOptions::new(ImageFormat::Jpeg(85))
                .downscale(crate::image_encoding::Downscale::Eighth);
            let (image, rows) =
                crate::image_encoding::read::count_rows(|| frame.encode_image(&options).unwrap());
            assert_eq!(rows, height as usize, "source height {height}");
            assert_eq!(
                decode(&image, (3, height.div_ceil(8) as u16)).len(),
                3 * height.div_ceil(8) as usize * 3
            );
        }
    }

    #[test]
    fn packed_rgb_preserves_channels_discards_alpha_and_matches_rows() {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        eprintln!("JPEG AVX2={}", std::is_x86_feature_detected!("avx2"));
        // Widths straddle the backend's eight-pixel AVX2 input batches. NDI
        // uncompressed frame widths are even, so use their nearest neighbors.
        for width in [2u16, 6, 8, 10, 14, 16, 18, 30, 32, 34, 66] {
            for format in [
                PixelFormat::Rgba,
                PixelFormat::Rgbx,
                PixelFormat::Bgra,
                PixelFormat::Bgrx,
            ] {
                for quality in [50, 85, 89, 90, 91, 100] {
                    let mut data = Vec::new();
                    let rgb = matches!(format, PixelFormat::Rgba | PixelFormat::Rgbx);
                    for i in 0..usize::from(width) * 3 {
                        let (r, g, b) = (190 + (i % 30) as u8, 60, 20);
                        data.extend_from_slice(&if rgb {
                            [r, g, b, i as u8]
                        } else {
                            [b, g, r, i as u8]
                        });
                    }
                    let frame = VideoFrame::builder()
                        .resolution(width.into(), 3)
                        .pixel_format(format)
                        .data(data)
                        .build()
                        .unwrap();
                    let options = ImageOptions::new(ImageFormat::Jpeg(quality));
                    let direct = frame.encode_image(&options).unwrap();
                    let source = Source::new(&frame);
                    let plan = Plan::new(&source, &options).unwrap();
                    let mut rows = Vec::new();
                    encode_rows(
                        &mut plan.pixels::<u8>(),
                        JpegQuality::new(quality).unwrap(),
                        JpegDimensions::new(width.into(), 3).unwrap(),
                        None,
                        &mut Vec::new(),
                        &mut rows,
                    )
                    .unwrap();
                    let direct = decode(&direct, (width, 3));
                    let rows = decode(&rows, (width, 3));
                    // Runtime SIMD rounding is permitted: mean <= 1 code and
                    // maximum <= 16 codes against the component-row path.
                    let delta: Vec<_> = direct
                        .iter()
                        .zip(&rows)
                        .map(|(&a, &b)| a.abs_diff(b) as usize)
                        .collect();
                    assert!(delta.iter().sum::<usize>() <= delta.len());
                    assert!(delta.iter().all(|&d| d <= 16));
                    for pixel in direct.as_chunks::<3>().0 {
                        assert!(
                            pixel[0] > 160
                                && pixel[1].abs_diff(60) <= 15
                                && pixel[2].abs_diff(20) <= 15,
                            "{format:?} q={quality}: {pixel:?}"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[test]
fn checked_settings_enforce_jpeg_representation_boundaries() {
    assert_eq!(JpegDimensions::new(65_535, 65_535).unwrap().width, u16::MAX);
    for (width, height, dimension) in [
        (65_536, 1, crate::ImageDimension::Width),
        (1, 65_536, crate::ImageDimension::Height),
    ] {
        assert!(
            matches!(JpegDimensions::new(width, height), Err(Error::InvalidFrame(crate::FrameDefect::ImageDimension { codec: crate::ImageCodec::Jpeg, dimension: actual, length: 65_536, maximum: 65_535 })) if actual == dimension)
        );
    }
    for value in [1, 85, 100] {
        assert_eq!(JpegQuality::new(value).unwrap().0, value);
    }
    for value in [0, 101, 255] {
        assert!(JpegQuality::new(value).is_err());
    }
}
