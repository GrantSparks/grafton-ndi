//! WebP from one buffer of the whole image: lossless through `image-webp`,
//! lossy through the scalar Rust port of libwebp, `libwebpop`.

use std::{io::Write, ops::RangeInclusive};

use super::{BufferedImageShape, Channels, Pixels};
use crate::{Error, Result};

/// Lossless WebP's largest width or height.
pub(super) const MAX_LOSSLESS_DIMENSION: usize = 16_384;

/// VP8 spends 14 bits on a dimension, with zero reserved.
pub(super) const MAX_LOSSY_DIMENSION: usize = 16_383;

// A complete, four-channel WebP buffer fits usize even on a 32-bit target.
// Both codec dimension checks use these same constants before buffering.
const_assert!({
    assert!(MAX_LOSSY_DIMENSION <= MAX_LOSSLESS_DIMENSION);
    assert!(MAX_LOSSLESS_DIMENSION <= usize::MAX / MAX_LOSSLESS_DIMENSION / 4);
});

/// Quality is checked once before examining frame metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WebpQuality(u8);
impl WebpQuality {
    pub(super) const RANGE: RangeInclusive<u8> = 0..=100;

    pub(super) fn new(value: u8) -> Result<Self> {
        super::ImageFormat::WebpLossy(value).validate_quality()?;
        Ok(Self(value))
    }
}

/// Effort is checked once with the format's options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WebpEffort(u8);
impl WebpEffort {
    pub(super) const RANGE: RangeInclusive<u8> = 0..=6;

    pub(super) fn new(value: u8) -> Result<Self> {
        super::options::check_image_setting(
            crate::ImageCodecKind::Webp,
            crate::ImageSettingKind::Effort,
            value,
            Self::RANGE,
        )?;
        Ok(Self(value))
    }
}

/// Encode `pixels` as a lossless WebP of `channels` channels into `out`.
///
/// An RGBA frame with contiguous rows is read in place; anything else
/// converts into one buffer of the whole image.
pub(super) fn encode_lossless(
    pixels: &mut Pixels<'_, u8>,
    shape: BufferedImageShape,
    out: &mut dyn Write,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    let (width, height) = (shape.rows.width, shape.rows.height);
    let encoder = image_webp::WebPEncoder::new(out);
    let image = image_with(pixels, shape, scratch)?;
    let color = match shape.rows.channels {
        Channels::Rgba => image_webp::ColorType::Rgba8,
        Channels::Rgb => image_webp::ColorType::Rgb8,
    };
    encoder
        .encode(image, width, height, color)
        .map_err(|error| match error {
            image_webp::EncodingError::IoError(error) => Error::Io(error),
            error => Error::InvalidFrame(crate::FrameDefect::EncodingFailed {
                codec: crate::ImageCodecKind::Webp,
                diagnostic: error.to_string().into_boxed_str(),
            }),
        })
}

/// The plan's checked lossy WebP settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LossySettings {
    pub(super) shape: BufferedImageShape,
    pub(super) quality: WebpQuality,
    pub(super) effort: WebpEffort,
    pub(super) sharp_yuv: bool,
}

/// Lossy color with exact alpha and optional SharpYUV conversion. The backend
/// takes RGBA, including opaque alpha if the source has none or it was
/// discarded, and returns a complete image.
pub(super) fn encode_lossy(
    pixels: &mut Pixels<'_, u8>,
    settings: LossySettings,
    scratch: &mut Vec<u8>,
) -> Result<Vec<u8>> {
    use libwebpop::{OpConfig, OpImage, OpQuality};

    let LossySettings {
        shape,
        quality,
        effort,
        sharp_yuv,
    } = settings;
    let (width, height) = (shape.rows.width, shape.rows.height);
    let image = image_with(pixels, shape, scratch)?;
    let mut config = OpConfig::new(OpQuality::Lossy {
        quality: f32::from(quality.0),
        alpha_quality: 100,
        sharp_yuv,
    });
    config.method = effort.0;
    libwebpop::encode(
        OpImage::Rgba {
            pixels: image,
            width,
            height,
            stride: shape.rows.row_bytes,
        },
        &config,
    )
    .map_err(encoding_error)
}

fn encoding_error(error: libwebpop::WebPEncodingError) -> Error {
    // libwebpop 0.1 supports this type as an Error, but does not guarantee its
    // variants or expose an allocation classifier. Keep the documented
    // EncodingFailed exception until a supported classifier is available.
    Error::InvalidFrame(crate::FrameDefect::EncodingFailed {
        codec: crate::ImageCodecKind::Webp,
        diagnostic: error.to_string().into_boxed_str(),
    })
}

/// Borrow only initialized active pixels; otherwise reuse a converted image.
pub(super) fn image_with<'s, 'a: 's>(
    pixels: &mut Pixels<'a, u8>,
    shape: BufferedImageShape,
    scratch: &'s mut Vec<u8>,
) -> Result<&'s [u8]> {
    if let Some(data) = pixels.direct_rgba() {
        return Ok(data);
    }
    super::resize_scratch(scratch, shape.image_bytes)?;
    for (y, row) in scratch.chunks_exact_mut(shape.rows.row_bytes).enumerate() {
        pixels.pack_row(y, shape.rows.channels, row);
    }
    Ok(scratch)
}

#[cfg(test)]
pub(super) fn image<'a>(
    pixels: &mut Pixels<'a, u8>,
    shape: BufferedImageShape,
) -> std::borrow::Cow<'a, [u8]> {
    match pixels.direct_rgba() {
        Some(data) => std::borrow::Cow::Borrowed(data),
        None => {
            let mut scratch = Vec::new();
            image_with(pixels, shape, &mut scratch).unwrap();
            std::borrow::Cow::Owned(scratch)
        }
    }
}
