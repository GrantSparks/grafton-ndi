//! PNG: 8 or 16 bits a channel, RGBA or RGB, tagged with `cICP` when its
//! colors need it.

use std::io::{self, Write};

use super::{color::Sample, options::PngCompression, Pixels};
use crate::{Error, Result};

/// Encode `pixels` as a PNG of `channels` channels into `out`, with a `cICP`
/// chunk when `cicp` gives one.
///
/// An RGBA frame whose rows are the image's rows is read in place; anything
/// else converts into one row of scratch, reused for every row.
pub(super) fn encode<P: Sample>(
    pixels: &mut Pixels<'_, P>,
    channels: usize,
    compression: PngCompression,
    cicp: Option<[u8; 4]>,
    out: &mut dyn Write,
    row: &mut Vec<u8>,
) -> Result<()> {
    let mut out = KeepError { out, error: None };
    let result = write(pixels, channels, compression, cicp, &mut out, row);
    match out.error {
        Some(error) if result.is_err() => Err(Error::Io(error)),
        _ => result,
    }
}

fn write<P: Sample>(
    pixels: &mut Pixels<'_, P>,
    channels: usize,
    compression: PngCompression,
    cicp: Option<[u8; 4]>,
    out: &mut KeepError<'_>,
    row: &mut Vec<u8>,
) -> Result<()> {
    let (width, height) = (pixels.width(), pixels.height());
    let mut encoder = ::png::Encoder::new(out, to_u32(width), to_u32(height));
    encoder.set_color(if channels == 4 {
        ::png::ColorType::Rgba
    } else {
        ::png::ColorType::Rgb
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
            super::resize_scratch(row, super::packed_len(width, 1, channels * P::BYTES)?)?;
            for y in 0..height {
                pixels.pack_row(y, channels, row);
                stream.write_all(row)?;
            }
        }
        stream.finish().map_err(png_error)?;
    }
    writer.finish().map_err(png_error)
}

/// A writer that keeps the first error of the writer it wraps. Once image
/// data has begun, `png` passes a failure of the writer on only as text, an
/// `io::Error` of kind `Other`; the encode reports the kept error instead,
/// the writer's own.
struct KeepError<'w> {
    out: &'w mut dyn Write,
    error: Option<io::Error>,
}

impl KeepError<'_> {
    /// Keep `error`, unless one is kept already, and return a copy for the
    /// codec. An interruption is no failure: `write_all` retries it.
    fn keep(&mut self, error: io::Error) -> io::Error {
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

/// The one row of scratch a converted PNG needs.
#[cfg(test)]
pub(super) fn row_buffer<P: Sample>(width: usize, channels: usize) -> Vec<u8> {
    vec![0; width * channels * P::BYTES]
}

/// A PNG dimension, which the frame's own `u32` dimension bounds.
fn to_u32(length: usize) -> u32 {
    u32::try_from(length).expect("an image is no larger than its frame, whose size is a u32")
}

fn png_error(error: ::png::EncodingError) -> Error {
    match error {
        ::png::EncodingError::IoError(error) => Error::Io(error),
        error => Error::InvalidFrame(format!("PNG encoding failed: {error}")),
    }
}
