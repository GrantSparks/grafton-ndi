//! WebP from one buffer of the whole image: lossless through `image-webp`,
//! lossy through the scalar Rust port of libwebp, `libwebpop`.

use std::io::Write;

use super::Pixels;
use crate::{Error, Result};

/// Lossless WebP's largest width or height.
pub(super) const MAX_LOSSLESS_DIMENSION: usize = 16_384;

/// VP8 spends 14 bits on a dimension, with zero reserved.
pub(super) const MAX_LOSSY_DIMENSION: usize = 16_383;

/// Encode `pixels` as a lossless WebP of `channels` channels into `out`.
///
/// An RGBA frame with contiguous rows is read in place; anything else
/// converts into one buffer of the whole image.
pub(super) fn encode_lossless(
    pixels: &mut Pixels<'_, u8>,
    channels: usize,
    out: &mut dyn Write,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    let (width, height) = (to_u32(pixels.width()), to_u32(pixels.height()));
    let encoder = image_webp::WebPEncoder::new(out);
    let image = image_with(pixels, channels, scratch)?;
    let color = if channels == 4 {
        image_webp::ColorType::Rgba8
    } else {
        image_webp::ColorType::Rgb8
    };
    encoder
        .encode(image, width, height, color)
        .map_err(|error| match error {
            image_webp::EncodingError::IoError(error) => Error::Io(error),
            error => Error::InvalidFrame(format!("WebP encoding failed: {error}")),
        })
}

/// Lossy color with exact alpha and optional SharpYUV conversion. `Plan` validated
/// quality, effort and dimensions. The backend takes RGBA, including opaque alpha
/// when discarded, and returns a complete image.
pub(super) fn encode_lossy(
    pixels: &mut Pixels<'_, u8>,
    quality: u8,
    effort: u8,
    sharp_yuv: bool,
    scratch: &mut Vec<u8>,
) -> Result<Vec<u8>> {
    use libwebpop::{OpConfig, OpImage, OpQuality};

    let (width, height) = (to_u32(pixels.width()), to_u32(pixels.height()));
    let image = image_with(pixels, 4, scratch)?;
    let mut config = OpConfig::new(OpQuality::Lossy {
        quality: f32::from(quality),
        alpha_quality: 100,
        sharp_yuv,
    });
    config.method = effort;
    libwebpop::encode(
        OpImage::Rgba {
            pixels: image,
            width,
            height,
            stride: width as usize * 4,
        },
        &config,
    )
    .map_err(|error| Error::InvalidFrame(format!("WebP encoding failed: {error}")))
}

/// Borrow only initialized active pixels; otherwise reuse a converted image.
pub(super) fn image_with<'s, 'a: 's>(
    pixels: &mut Pixels<'a, u8>,
    channels: usize,
    scratch: &'s mut Vec<u8>,
) -> Result<&'s [u8]> {
    if let Some(data) = pixels.rgba_rows().and_then(|plane| plane.data()) {
        return Ok(data);
    }
    let len = super::packed_len(pixels.width(), pixels.height(), channels)?;
    super::resize_scratch(scratch, len)?;
    let row_len = pixels.width() * channels;
    for (y, row) in scratch.chunks_exact_mut(row_len).enumerate() {
        pixels.pack_row(y, channels, row);
    }
    Ok(scratch)
}

#[cfg(test)]
pub(super) fn image<'a>(
    pixels: &mut Pixels<'a, u8>,
    channels: usize,
) -> std::borrow::Cow<'a, [u8]> {
    match pixels.rgba_rows().and_then(|plane| plane.data()) {
        Some(data) => std::borrow::Cow::Borrowed(data),
        None => {
            let mut scratch = Vec::new();
            image_with(pixels, channels, &mut scratch).unwrap();
            std::borrow::Cow::Owned(scratch)
        }
    }
}

/// A WebP dimension, which the plan checked against the codec's limit.
fn to_u32(length: usize) -> u32 {
    u32::try_from(length).expect("the plan checked WebP's dimension limit")
}
