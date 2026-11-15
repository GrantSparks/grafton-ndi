//! PNG: 8 or 16 bits a channel, RGBA or RGB, tagged with `cICP` when its
//! colors need it.

use std::io::Write;

use super::{color::Sample, options::PngCompression, Channels, ImageShape, Pixels};
use crate::{Error, Result};

/// Encode `pixels` as a PNG of `channels` channels into `out`, with a `cICP`
/// chunk when `cicp` gives one.
///
/// An RGBA frame whose rows are the image's rows is read in place; anything
/// else converts into one row of scratch, reused for every row.
pub(super) fn encode<P: Sample>(
    pixels: &mut Pixels<'_, P>,
    shape: ImageShape,
    compression: PngCompression,
    cicp: Option<[u8; 4]>,
    out: &mut dyn Write,
    row: &mut Vec<u8>,
) -> Result<()> {
    let mut encoder = ::png::Encoder::new(out, shape.width, shape.height);
    encoder.set_color(match shape.channels {
        Channels::Rgba => ::png::ColorType::Rgba,
        Channels::Rgb => ::png::ColorType::Rgb,
    });
    encoder.set_depth(if P::BYTES == 1 {
        ::png::BitDepth::Eight
    } else {
        ::png::BitDepth::Sixteen
    });
    encoder.set_compression(match compression {
        PngCompression::Fast => ::png::Compression::Fast,
        PngCompression::Balanced => ::png::Compression::Balanced,
        PngCompression::High => ::png::Compression::High,
    });
    let mut writer = encoder.write_header().map_err(png_error)?;
    if let Some(cicp) = cicp {
        // `png` reads `cICP` but does not write it, so the chunk is written
        // here: after the header and before the image data, as PNG requires.
        writer
            .write_chunk(::png::chunk::cICP, &cicp)
            .map_err(png_error)?;
    }
    {
        let mut stream = writer.stream_writer().map_err(png_error)?;
        if let Some(plane) = pixels.rgba_rows() {
            for row in plane.rows() {
                stream.write_all(row)?;
            }
        } else {
            super::resize_scratch(row, shape.row_bytes)?;
            for y in 0..shape.height as usize {
                pixels.pack_row(y, shape.channels, row);
                stream.write_all(row)?;
            }
        }
        stream.finish().map_err(png_error)?;
    }
    writer.finish().map_err(png_error)
}

/// The one row of scratch a converted PNG needs.
#[cfg(test)]
pub(super) fn row_buffer<P: Sample>(width: usize, channels: usize) -> Vec<u8> {
    vec![0; width * channels * P::BYTES]
}

fn png_error(error: ::png::EncodingError) -> Error {
    match error {
        ::png::EncodingError::IoError(error) => Error::Io(error),
        error => Error::InvalidFrame(format!("PNG encoding failed: {error}")),
    }
}
