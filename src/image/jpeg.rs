//! JPEG: 8-bit YCbCr through `jpeg-encoder`, with pixels converted straight
//! into the codec's component rows.

use std::{cell::RefCell, io::Write};

use super::{
    read::{Sink, BATCH},
    Pixels,
};
use crate::{Error, Result};

/// JPEG's largest width or height.
pub(super) const MAX_DIMENSION: usize = u16::MAX as usize;

/// Encode `pixels` as a JPEG of `quality` into `out`. The plan checked the
/// quality and the dimensions.
pub(super) fn encode(pixels: &mut Pixels<'_, u8>, quality: u8, out: &mut dyn Write) -> Result<()> {
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
                return jpeg_encoder::Encoder::new(out, quality)
                    .encode(data, to_u16(pixels.width()), to_u16(pixels.height()), color)
                    .map_err(encoding_error);
            }
        }
    }
    encode_rows(pixels, quality, out)
}

fn encode_rows(pixels: &mut Pixels<'_, u8>, quality: u8, out: &mut dyn Write) -> Result<()> {
    let image = Rows {
        width: to_u16(pixels.width()),
        height: to_u16(pixels.height()),
        pixels: RefCell::new(pixels),
    };
    jpeg_encoder::Encoder::new(out, quality)
        .encode_image(image)
        .map_err(encoding_error)
}

fn encoding_error(error: jpeg_encoder::EncodingError) -> Error {
    match error {
        jpeg_encoder::EncodingError::IoError(error) => Error::Io(error),
        error => Error::InvalidFrame(format!("JPEG encoding failed: {error}")),
    }
}

fn to_u16(length: usize) -> u16 {
    u16::try_from(length).expect("the plan checked JPEG's dimension limit")
}

/// The image the codec reads, a row at a time. The codec asks for its rows
/// through a shared reference, and for the last row again to pad its final
/// blocks, so the pixels sit in a `RefCell` and every row is read afresh.
pub(super) struct Rows<'p, 'a> {
    pub(super) pixels: RefCell<&'p mut Pixels<'a, u8>>,
    pub(super) width: u16,
    pub(super) height: u16,
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
        self.pixels
            .borrow_mut()
            .row(usize::from(y), &mut Components(buffers));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        image::{Plan, Source},
        ImageFormat, ImageOptions, PixelFormat, VideoFrame,
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
                    encode_rows(&mut plan.pixels::<u8>(&source), quality, &mut rows).unwrap();
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
