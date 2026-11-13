//! The row readers: every uncompressed pixel format, read a row at a time
//! into RGBA at the image's depth.
//!
//! Five readers cover the formats: packed RGB, packed 4:2:2, semi-planar
//! 4:2:2, 4:2:0 and planar 4:4:4. Each reads active rows through
//! [`VideoPlane::row`], never padding, and produces one output row from at
//! most one row of each plane. Chroma is repeated across the pixels it
//! covers, as the NDI SDK does: in 4:2:2 pixels 2k and 2k + 1 take chroma
//! sample k, and in 4:2:0 rows 2j and 2j + 1 also take chroma row j.

use super::{
    color::{Codes, Kernel, Sample},
    options::YuvRange,
    Source,
};
use crate::{frames::VideoPlane, PixelFormat, YuvMatrix};

/// The most pixels a [`Sink`] receives at once. Converting a batch of
/// fixed size in small arrays keeps the conversion free of allocation and
/// lets the compiler vectorize it.
pub(super) const BATCH: usize = 16;

/// Where converted pixels go, a batch at a time, left to right along a row.
pub(super) trait Sink<P> {
    /// Take the next pixels of the row, at most [`BATCH`] of them.
    fn put(&mut self, pixels: &[[P; 4]]);
}

/// The pixels of a frame, converted a row at a time.
#[derive(Debug)]
pub(super) struct Reader<'a, P: Sample> {
    width: usize,
    height: usize,
    layout: Layout<'a, P>,
}

#[derive(Debug)]
enum Layout<'a, P: Sample> {
    /// BGRA, BGRX, RGBA and RGBX: four bytes a pixel.
    Rgb {
        plane: VideoPlane<'a>,
        red: usize,
        blue: usize,
        /// Whether the fourth byte is read as alpha.
        alpha: bool,
    },
    /// UYVY, and UYVA with its 8-bit alpha plane: Cb Y0 Cr Y1 for each
    /// pixel pair.
    Packed422 {
        plane: VideoPlane<'a>,
        alpha: Option<VideoPlane<'a>>,
        kernel: Kernel<P>,
    },
    /// P216, and PA16 with its 16-bit alpha plane: a 16-bit Y plane and an
    /// interleaved 16-bit CbCr plane, both full height.
    SemiPlanar422 {
        luma: VideoPlane<'a>,
        chroma: VideoPlane<'a>,
        alpha: Option<VideoPlane<'a>>,
        kernel: Kernel<P>,
    },
    /// NV12, I420 and YV12: an 8-bit Y plane and chroma at half width and
    /// half height.
    Planar420 {
        luma: VideoPlane<'a>,
        chroma: Chroma420<'a>,
        kernel: Kernel<P>,
    },
    /// NA48, NB48, NA4G and NB4G: full-resolution Y, Cb and Cr planes, and
    /// an alpha plane for NB48 and NB4G.
    Planar444 {
        luma: VideoPlane<'a>,
        cb: VideoPlane<'a>,
        cr: VideoPlane<'a>,
        alpha: Option<VideoPlane<'a>>,
        /// Whether samples are 16-bit rather than 8-bit.
        wide: bool,
        kernel: Kernel<P>,
    },
}

/// The chroma planes of a 4:2:0 frame.
#[derive(Debug, Clone, Copy)]
enum Chroma420<'a> {
    /// NV12: one plane of Cb Cr pairs.
    Interleaved(VideoPlane<'a>),
    /// I420 and YV12: a Cb plane and a Cr plane.
    Separate {
        cb: VideoPlane<'a>,
        cr: VideoPlane<'a>,
    },
}

/// Where a YUV row's alpha comes from.
#[derive(Clone, Copy)]
enum AlphaRow<'r> {
    /// None: every pixel's alpha is the maximum.
    InPixels,
    /// One 8-bit alpha for each pixel.
    Bytes(&'r [u8]),
    /// One 16-bit alpha for each pixel, in native byte order.
    Words(&'r [u8]),
}

impl<'a, P: Sample> Reader<'a, P> {
    /// A reader of `source` that converts YUV with `matrix` and `range`. It
    /// reads alpha only when `alpha` asks for it; otherwise every pixel's
    /// alpha is the maximum.
    pub(super) fn new(
        source: &Source<'a>,
        matrix: YuvMatrix,
        range: YuvRange,
        alpha: bool,
    ) -> Self {
        let mut planes = source.planes.clone();
        let mut plane = || {
            planes
                .next()
                .expect("a validated layout has every plane of its format")
        };
        let kernel = |bits| Kernel::new(matrix, range, bits);
        // Every format has a reader: a format added to `PixelFormat` stops
        // this match from compiling until it has one.
        let layout = match source.format {
            PixelFormat::Bgra | PixelFormat::Bgrx | PixelFormat::Rgba | PixelFormat::Rgbx => {
                let rgb_order = matches!(source.format, PixelFormat::Rgba | PixelFormat::Rgbx);
                Layout::Rgb {
                    plane: plane(),
                    red: if rgb_order { 0 } else { 2 },
                    blue: if rgb_order { 2 } else { 0 },
                    alpha: alpha && matches!(source.format, PixelFormat::Bgra | PixelFormat::Rgba),
                }
            }
            PixelFormat::Uyvy | PixelFormat::Uyva => Layout::Packed422 {
                plane: plane(),
                alpha: (source.format == PixelFormat::Uyva && alpha).then(&mut plane),
                kernel: kernel(8),
            },
            PixelFormat::P216 | PixelFormat::Pa16 => Layout::SemiPlanar422 {
                luma: plane(),
                chroma: plane(),
                alpha: (source.format == PixelFormat::Pa16 && alpha).then(&mut plane),
                kernel: kernel(16),
            },
            PixelFormat::Nv12 => Layout::Planar420 {
                luma: plane(),
                chroma: Chroma420::Interleaved(plane()),
                kernel: kernel(8),
            },
            PixelFormat::I420 => Layout::Planar420 {
                luma: plane(),
                chroma: Chroma420::Separate {
                    cb: plane(),
                    cr: plane(),
                },
                kernel: kernel(8),
            },
            PixelFormat::Yv12 => {
                let luma = plane();
                let cr = plane();
                let cb = plane();
                Layout::Planar420 {
                    luma,
                    chroma: Chroma420::Separate { cb, cr },
                    kernel: kernel(8),
                }
            }
            PixelFormat::Na48 | PixelFormat::Nb48 | PixelFormat::Na4g | PixelFormat::Nb4g => {
                let wide = matches!(source.format, PixelFormat::Na4g | PixelFormat::Nb4g);
                let has_alpha = matches!(source.format, PixelFormat::Nb48 | PixelFormat::Nb4g);
                Layout::Planar444 {
                    luma: plane(),
                    cb: plane(),
                    cr: plane(),
                    alpha: (has_alpha && alpha).then(&mut plane),
                    wide,
                    kernel: kernel(if wide { 16 } else { 8 }),
                }
            }
        };
        Self {
            width: source.width,
            height: source.height,
            layout,
        }
    }

    pub(super) fn width(&self) -> usize {
        self.width
    }

    pub(super) fn height(&self) -> usize {
        self.height
    }

    /// Whether the pixels carry an alpha read from the frame, rather than
    /// the maximum.
    pub(super) fn has_alpha(&self) -> bool {
        match &self.layout {
            Layout::Rgb { alpha, .. } => *alpha,
            Layout::Packed422 { alpha, .. }
            | Layout::SemiPlanar422 { alpha, .. }
            | Layout::Planar444 { alpha, .. } => alpha.is_some(),
            Layout::Planar420 { .. } => false,
        }
    }

    /// The frame's own rows when they are exactly the image's 8-bit RGBA, so
    /// that a codec can read them in place.
    pub(super) fn rgba_rows(&self) -> Option<VideoPlane<'a>> {
        match self.layout {
            Layout::Rgb {
                plane,
                red: 0,
                alpha: true,
                ..
            } if P::BYTES == 1 => Some(plane),
            _ => None,
        }
    }

    /// Convert row `y` into `sink`.
    pub(super) fn row(&self, y: usize, sink: &mut impl Sink<P>) {
        let width = self.width;
        match &self.layout {
            &Layout::Rgb {
                plane,
                red,
                blue,
                alpha,
            } => {
                let row = row(plane, y);
                // Two loops, so that neither tests for alpha at each pixel.
                if alpha {
                    rgb_row(row, sink, |pixel| {
                        [pixel[red], pixel[1], pixel[blue], pixel[3]]
                    });
                } else {
                    rgb_row(row, sink, |pixel| {
                        [pixel[red], pixel[1], pixel[blue], u8::MAX]
                    });
                }
            }
            Layout::Packed422 {
                plane,
                alpha,
                kernel,
            } => {
                let row = row(*plane, y);
                yuv_row(
                    width,
                    kernel,
                    alpha_bytes(*alpha, y),
                    sink,
                    |start, len, codes| {
                        let pairs = row[start * 2..(start + len) * 2].as_chunks::<4>().0;
                        for (pair, &[cb, y0, cr, y1]) in codes.pairs().zip(pairs) {
                            pair.set([code(y0), code(y1)], code(cb), code(cr));
                        }
                    },
                );
            }
            Layout::SemiPlanar422 {
                luma,
                chroma,
                alpha,
                kernel,
            } => {
                let (luma, chroma) = (row(*luma, y), row(*chroma, y));
                yuv_row(
                    width,
                    kernel,
                    alpha_words(*alpha, y),
                    sink,
                    |start, len, codes| {
                        let span = start * 2..(start + len) * 2;
                        let lumas = luma[span.clone()].as_chunks::<4>().0;
                        let chromas = chroma[span].as_chunks::<4>().0;
                        for ((pair, &[y0, y1, y2, y3]), &[b0, b1, r0, r1]) in
                            codes.pairs().zip(lumas).zip(chromas)
                        {
                            pair.set([wide(y0, y1), wide(y2, y3)], wide(b0, b1), wide(r0, r1));
                        }
                    },
                );
            }
            Layout::Planar420 {
                luma,
                chroma,
                kernel,
            } => {
                let luma = row(*luma, y);
                match *chroma {
                    Chroma420::Interleaved(plane) => {
                        let chroma = row(plane, y / 2);
                        yuv_row(
                            width,
                            kernel,
                            AlphaRow::InPixels,
                            sink,
                            |start, len, codes| {
                                let lumas = luma[start..start + len].as_chunks::<2>().0;
                                let chromas = chroma[start..start + len].as_chunks::<2>().0;
                                for ((pair, &[y0, y1]), &[cb, cr]) in
                                    codes.pairs().zip(lumas).zip(chromas)
                                {
                                    pair.set([code(y0), code(y1)], code(cb), code(cr));
                                }
                            },
                        );
                    }
                    Chroma420::Separate { cb, cr } => {
                        let (cbs, crs) = (row(cb, y / 2), row(cr, y / 2));
                        yuv_row(
                            width,
                            kernel,
                            AlphaRow::InPixels,
                            sink,
                            |start, len, codes| {
                                let lumas = luma[start..start + len].as_chunks::<2>().0;
                                let span = start / 2..(start + len) / 2;
                                let chromas = cbs[span.clone()].iter().zip(&crs[span]);
                                for ((pair, &[y0, y1]), (&cb, &cr)) in
                                    codes.pairs().zip(lumas).zip(chromas)
                                {
                                    pair.set([code(y0), code(y1)], code(cb), code(cr));
                                }
                            },
                        );
                    }
                }
            }
            Layout::Planar444 {
                luma,
                cb,
                cr,
                alpha,
                wide: false,
                kernel,
            } => {
                let planes = [row(*luma, y), row(*cb, y), row(*cr, y)];
                yuv_row(
                    width,
                    kernel,
                    alpha_bytes(*alpha, y),
                    sink,
                    |start, len, codes| {
                        for (codes, samples) in codes.channels().zip(planes) {
                            for (code_slot, &sample) in
                                codes.iter_mut().zip(&samples[start..start + len])
                            {
                                *code_slot = code(sample);
                            }
                        }
                    },
                );
            }
            Layout::Planar444 {
                luma,
                cb,
                cr,
                alpha,
                wide: true,
                kernel,
            } => {
                let planes = [row(*luma, y), row(*cb, y), row(*cr, y)];
                yuv_row(
                    width,
                    kernel,
                    alpha_words(*alpha, y),
                    sink,
                    |start, len, codes| {
                        for (codes, samples) in codes.channels().zip(planes) {
                            let samples = samples[start * 2..(start + len) * 2].as_chunks::<2>().0;
                            for (code_slot, &sample) in codes.iter_mut().zip(samples) {
                                *code_slot = u16::from_ne_bytes(sample);
                            }
                        }
                    },
                );
            }
        }
    }
}

/// Row `y` of a plane, which the validated layout guarantees exists.
fn row(plane: VideoPlane<'_>, y: usize) -> &[u8] {
    plane
        .row(y)
        .expect("readers ask only for rows inside the validated frame")
}

/// An 8-bit sample as a 16-bit code. Shifting keeps limited-range codes
/// exact: 16 becomes 4096, as the SDK describes for high-bit-depth video.
#[inline(always)]
fn code(sample: u8) -> u16 {
    u16::from(sample) << 8
}

/// A 16-bit sample from its bytes in native order, at any address.
#[inline(always)]
fn wide(first: u8, second: u8) -> u16 {
    u16::from_ne_bytes([first, second])
}

fn alpha_bytes(plane: Option<VideoPlane<'_>>, y: usize) -> AlphaRow<'_> {
    plane.map_or(AlphaRow::InPixels, |plane| AlphaRow::Bytes(row(plane, y)))
}

fn alpha_words(plane: Option<VideoPlane<'_>>, y: usize) -> AlphaRow<'_> {
    plane.map_or(AlphaRow::InPixels, |plane| AlphaRow::Words(row(plane, y)))
}

/// Convert an RGB row with `pixel`, which reorders one pixel's bytes into
/// RGBA, a batch at a time.
#[inline(always)]
fn rgb_row<P: Sample>(row: &[u8], sink: &mut impl Sink<P>, pixel: impl Fn(&[u8; 4]) -> [u8; 4]) {
    let mut batch = [[P::MAX; 4]; BATCH];
    for chunk in row.chunks(BATCH * 4) {
        let pixels = chunk.as_chunks::<4>().0;
        for (out, pixel_bytes) in batch.iter_mut().zip(pixels) {
            *out = pixel(pixel_bytes).map(P::from_u8);
        }
        sink.put(&batch[..pixels.len()]);
    }
}

/// Convert a YUV row of `width` pixels a batch at a time: `fill` writes the
/// codes of the `len` pixels from `start`, the kernel converts them, and
/// each pixel takes its alpha from `alpha`, or the maximum.
#[inline(always)]
fn yuv_row<P: Sample>(
    width: usize,
    kernel: &Kernel<P>,
    alpha: AlphaRow<'_>,
    sink: &mut impl Sink<P>,
    mut fill: impl FnMut(usize, usize, &mut Codes),
) {
    let mut codes = Codes::default();
    let mut batch = [[P::MAX; 4]; BATCH];
    for start in (0..width).step_by(BATCH) {
        let len = BATCH.min(width - start);
        fill(start, len, &mut codes);
        kernel.convert(&codes, &mut batch);
        let pixels = &mut batch[..len];
        match alpha {
            AlphaRow::InPixels => {}
            AlphaRow::Bytes(row) => {
                for (pixel, &value) in pixels.iter_mut().zip(&row[start..start + len]) {
                    pixel[3] = P::from_u8(value);
                }
            }
            AlphaRow::Words(row) => {
                let values = row[start * 2..(start + len) * 2].as_chunks::<2>().0;
                for (pixel, value) in pixels.iter_mut().zip(values) {
                    pixel[3] = P::from_u16(u16::from_ne_bytes(*value));
                }
            }
        }
        sink.put(pixels);
    }
}
