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
    let image = Rows {
        width: to_u16(pixels.width()),
        height: to_u16(pixels.height()),
        pixels: RefCell::new(pixels),
    };
    jpeg_encoder::Encoder::new(out, quality)
        .encode_image(image)
        .map_err(|error| match error {
            jpeg_encoder::EncodingError::IoError(error) => Error::Io(error),
            error => Error::InvalidFrame(format!("JPEG encoding failed: {error}")),
        })
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
