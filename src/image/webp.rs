//! Lossless WebP through `image-webp`, which encodes from one buffer of the
//! whole image.

use std::io::Write;

use super::Pixels;
use crate::{Error, Result};

/// Lossless WebP's largest width or height.
pub(super) const MAX_DIMENSION: usize = 16_384;

/// Encode `pixels` as a lossless WebP of `channels` channels into `out`.
///
/// An RGBA frame with contiguous rows is read in place; anything else
/// converts into one buffer of the whole image.
pub(super) fn encode(
    pixels: &mut Pixels<'_, u8>,
    channels: usize,
    out: &mut dyn Write,
) -> Result<()> {
    let (width, height) = (to_u32(pixels.width()), to_u32(pixels.height()));
    let encoder = image_webp::WebPEncoder::new(out);
    let result = match pixels.rgba_rows().and_then(|plane| plane.data()) {
        Some(data) => encoder.encode(data, width, height, image_webp::ColorType::Rgba8),
        None => {
            let image = image_buffer(pixels, channels);
            let color = if channels == 4 {
                image_webp::ColorType::Rgba8
            } else {
                image_webp::ColorType::Rgb8
            };
            encoder.encode(&image, width, height, color)
        }
    };
    result.map_err(|error| match error {
        image_webp::EncodingError::IoError(error) => Error::Io(error),
        error => Error::InvalidFrame(format!("WebP encoding failed: {error}")),
    })
}

/// The whole converted image, `channels` bytes a pixel: exactly width ×
/// height × channels bytes.
pub(super) fn image_buffer(pixels: &mut Pixels<'_, u8>, channels: usize) -> Vec<u8> {
    let row_len = pixels.width() * channels;
    let mut image = vec![0; row_len * pixels.height()];
    for (y, row) in image.chunks_exact_mut(row_len).enumerate() {
        pixels.pack_row(y, channels, row);
    }
    image
}

/// A WebP dimension, which the plan checked against [`MAX_DIMENSION`].
fn to_u32(length: usize) -> u32 {
    u32::try_from(length).expect("the plan checked WebP's dimension limit")
}
