//! Reduction by 2, 4 or 8: blocks of converted pixels averaged inside the
//! row loop, so a thumbnail never holds a full-size image.

use super::{
    color::Sample,
    read::{Reader, Sink, BATCH},
};

/// Averages blocks of a [`Reader`]'s pixels into a reduced image, one output
/// row at a time. Rows can be asked for in any order and more than once, as
/// the JPEG codec asks for its last row.
#[derive(Debug)]
pub(super) struct Downscaler<P: Sample> {
    /// log2 of the factor.
    shift: u32,
    /// Whether color is weighted by alpha.
    weighted: bool,
    /// One accumulator for each output column: red, green and blue sums,
    /// then the sum of alpha.
    sums: Vec<[P::Sum; 4]>,
}

impl<P: Sample> Downscaler<P> {
    /// A reduction of `reader` by 2 to the power `shift`. With
    /// `reader.has_alpha()`, color is weighted by alpha.
    #[cfg(test)]
    pub(super) fn new(reader: &Reader<'_, P>, shift: u32) -> Self {
        Self {
            shift,
            weighted: reader.has_alpha(),
            sums: vec![Default::default(); reduced(reader.width(), shift)],
        }
    }

    pub(super) fn with_sums(reader: &Reader<'_, P>, shift: u32, sums: Vec<[P::Sum; 4]>) -> Self {
        Self {
            shift,
            weighted: reader.has_alpha(),
            sums,
        }
    }

    pub(super) fn into_sums(self) -> Vec<[P::Sum; 4]> {
        self.sums
    }

    /// log2 of the factor.
    #[cfg(test)]
    pub(super) fn shift(&self) -> u32 {
        self.shift
    }

    /// Average output row `y` of `reader`'s pixels into `sink`.
    pub(super) fn row(&mut self, reader: &Reader<'_, P>, y: usize, sink: &mut impl Sink<P>) {
        if self.weighted {
            self.row_with::<true>(reader, y, sink);
        } else {
            self.row_with::<false>(reader, y, sink);
        }
    }

    fn row_with<const WEIGHTED: bool>(
        &mut self,
        reader: &Reader<'_, P>,
        y: usize,
        sink: &mut impl Sink<P>,
    ) {
        let size = 1 << self.shift;
        let top = y << self.shift;
        let bottom = (top + size).min(reader.height());
        self.sums.fill(Default::default());
        for source_y in top..bottom {
            reader.row(
                source_y,
                &mut Accumulate::<P, WEIGHTED> {
                    sums: &mut self.sums,
                    shift: self.shift,
                    x: 0,
                },
            );
        }
        let rows = (bottom - top) as u64;
        let width = reader.width();
        let mut batch = [[P::MAX; 4]; BATCH];
        for (start, sums) in self.sums.chunks(BATCH).enumerate() {
            for (offset, (pixel, sum)) in batch.iter_mut().zip(sums).enumerate() {
                let left = (start * BATCH + offset) << self.shift;
                let columns = size.min(width - left);
                let count = rows * columns as u64;
                let full_shift = (rows == size as u64 && columns == size).then_some(self.shift * 2);
                *pixel = mean::<P, WEIGHTED>(*sum, count, full_shift);
            }
            sink.put(&batch[..sums.len()]);
        }
    }
}

/// The reduced length of `length` pixels: ceil(length / 2^shift).
pub(super) fn reduced(length: usize, shift: u32) -> usize {
    length.div_ceil(1 << shift)
}

/// The average of a block of `count` pixels. Sums round half up.
fn mean<P: Sample, const WEIGHTED: bool>(
    sum: [P::Sum; 4],
    count: u64,
    full_shift: Option<u32>,
) -> [P; 4] {
    let [red, green, blue, alpha] = sum.map(Into::<u64>::into);
    let plain = |total: u64| {
        P::narrow(match full_shift {
            Some(shift) => (total + count / 2) >> shift,
            None => (total + count / 2) / count,
        })
    };
    let color = |total: u64| {
        if WEIGHTED && alpha > 0 {
            P::narrow((total + alpha / 2) / alpha)
        } else {
            plain(total)
        }
    };
    [color(red), color(green), color(blue), plain(alpha)]
}

/// Adds one complete horizontal block at a time. The reader emits batches of
/// BATCH pixels, divisible by every reduction factor, so a block never straddles
/// two batches. Plain and weighted sums stay in registers until one store.
struct Accumulate<'s, P: Sample, const WEIGHTED: bool> {
    sums: &'s mut [[P::Sum; 4]],
    shift: u32,
    x: usize,
}

impl<P: Sample, const WEIGHTED: bool> Sink<P> for Accumulate<'_, P, WEIGHTED> {
    fn put(&mut self, pixels: &[[P; 4]]) {
        let size = 1 << self.shift;
        let start = self.x >> self.shift;
        self.x += pixels.len();
        for (sum, block) in self.sums[start..].iter_mut().zip(pixels.chunks(size)) {
            let mut plain = [P::Sum::default(); 3];
            let mut weighted = [P::Sum::default(); 3];
            let mut alpha = P::Sum::default();
            for &[red, green, blue, opacity] in block {
                let opacity = opacity.widen();
                for ((plain, weighted), value) in
                    plain.iter_mut().zip(&mut weighted).zip([red, green, blue])
                {
                    let value = value.widen();
                    *plain += value;
                    if WEIGHTED {
                        *weighted += opacity * value;
                    }
                }
                alpha += opacity;
            }
            if WEIGHTED && alpha != P::Sum::default() {
                // A first nontransparent block replaces prior transparent
                // blocks' plain color. Later blocks add weighted color only.
                if sum[3] == P::Sum::default() {
                    sum[..3].fill(P::Sum::default());
                }
                for (total, value) in sum.iter_mut().zip(weighted) {
                    *total += value;
                }
            } else if !WEIGHTED || sum[3] == P::Sum::default() {
                for (total, value) in sum.iter_mut().zip(plain) {
                    *total += value;
                }
            }
            sum[3] += alpha;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{frames::VideoParts, image_encoding::Source, PixelFormat, VideoFrame, YuvMatrix};

    use super::super::options::YuvRange;

    struct Collect<P>(Vec<[P; 4]>);
    impl<P: Sample> Sink<P> for Collect<P> {
        fn put(&mut self, pixels: &[[P; 4]]) {
            self.0.extend_from_slice(pixels);
        }
    }

    fn rgba(width: u32, height: u32, pixels: &[[u8; 4]]) -> VideoFrame {
        VideoFrame::builder()
            .resolution(width, height)
            .pixel_format(PixelFormat::Rgba)
            .data(pixels.concat())
            .build()
            .unwrap()
    }

    fn reduce<P: Sample>(frame: &VideoFrame, shift: u32, alpha: bool) -> Vec<Vec<[P; 4]>> {
        let source = Source::new(frame);
        let reader = Reader::<P>::new(&source, YuvMatrix::Bt709, YuvRange::Limited, alpha);
        let mut downscaler = Downscaler::new(&reader, shift);
        let rows = reduced(VideoParts::layout(frame).height() as usize, shift);
        (0..rows)
            .map(|y| {
                let mut row = Collect(Vec::new());
                downscaler.row(&reader, y, &mut row);
                assert_eq!(row.0.len(), reduced(reader.width(), shift));
                row.0
            })
            .collect()
    }

    #[test]
    fn edge_blocks_average_only_their_pixels_and_round_half_up() {
        // 6×3 at Half: 3×2 blocks; the bottom row of blocks has one row.
        let mut pixels = Vec::new();
        for y in 0..3u8 {
            for x in 0..6u8 {
                pixels.push([x * 10 + y, 1 + x, y * 3 + x % 2, 255]);
            }
        }
        let frame = rgba(6, 3, &pixels);
        let reduced = reduce::<u8>(&frame, 1, false);
        let block = |bx: usize, by: usize| {
            let mut sum = [0u32; 4];
            let mut count = 0;
            for y in by * 2..(by * 2 + 2).min(3) {
                for x in bx * 2..bx * 2 + 2 {
                    for (total, value) in sum.iter_mut().zip(pixels[y * 6 + x]) {
                        *total += u32::from(value);
                    }
                    count += 1;
                }
            }
            sum.map(|total| ((total + count / 2) / count) as u8)
        };
        assert_eq!(reduced.len(), 2);
        for (by, row) in reduced.iter().enumerate() {
            assert_eq!(row.len(), 3);
            for (bx, pixel) in row.iter().enumerate() {
                assert_eq!(*pixel, block(bx, by), "block {bx},{by}");
            }
        }
        // 1 and 2 average to 1.5, which rounds up.
        let frame = rgba(2, 1, &[[1, 1, 1, 255], [2, 2, 2, 255]]);
        assert_eq!(reduce::<u8>(&frame, 1, false), [[[2, 2, 2, 255]]]);
        // A 6×1 frame at Eighth: one block of the 6 pixels there are.
        let frame = rgba(
            6,
            1,
            &[
                [8, 0, 0, 255],
                [9, 0, 0, 255],
                [9, 0, 0, 255],
                [9, 0, 0, 255],
                [9, 0, 0, 255],
                [9, 0, 0, 255],
            ],
        );
        assert_eq!(reduce::<u8>(&frame, 3, false), [[[9, 0, 0, 255]]]);
    }

    #[test]
    fn alpha_weights_color_and_a_transparent_block_takes_the_plain_mean() {
        let frame = rgba(
            4,
            2,
            &[
                [255, 0, 0, 255],
                [0, 0, 0, 0],
                [10, 20, 30, 0],
                [30, 40, 50, 0],
                [0, 0, 0, 0],
                [0, 0, 0, 0],
                [50, 60, 70, 0],
                [70, 80, 90, 0],
            ],
        );
        // The first block's color is its one visible pixel's; the second,
        // transparent throughout, is the plain mean.
        assert_eq!(
            reduce::<u8>(&frame, 1, true),
            [[[255, 0, 0, 64], [40, 50, 60, 0]]]
        );
        // Without alpha, a plain mean of color.
        assert_eq!(
            reduce::<u8>(&frame, 1, false),
            [[[64, 0, 0, 255], [40, 50, 60, 255]]]
        );
        // Transparent pixels before the first visible one are discarded.
        let frame = rgba(2, 1, &[[0, 0, 0, 0], [200, 100, 50, 128]]);
        assert_eq!(reduce::<u8>(&frame, 1, true), [[[200, 100, 50, 64]]]);
        // Weighting at 16 bits uses wide sums: 4 pixels of 65535 × 65535.
        let frame = rgba(2, 2, &[[255, 255, 255, 255]; 4]);
        assert_eq!(reduce::<u16>(&frame, 1, true), [[[65535; 4]]]);
    }

    #[test]
    fn weighted_mean_matches_a_direct_computation() {
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        };
        let (width, height) = (18usize, 11usize);
        let pixels: Vec<[u8; 4]> = (0..width * height)
            .map(|i| {
                let alpha = if i % 5 == 0 { 0 } else { next() };
                [next(), next(), next(), alpha]
            })
            .collect();
        let frame = rgba(width as u32, height as u32, &pixels);
        for shift in 1..=3 {
            let size = 1 << shift;
            let reduced = reduce::<u8>(&frame, shift, true);
            for (by, row) in reduced.iter().enumerate() {
                for (bx, pixel) in row.iter().enumerate() {
                    let block: Vec<[u8; 4]> = (by * size..((by + 1) * size).min(height))
                        .flat_map(|y| {
                            (bx * size..((bx + 1) * size).min(width)).map(move |x| (x, y))
                        })
                        .map(|(x, y)| pixels[y * width + x])
                        .collect();
                    let count = block.len() as u64;
                    let alpha: u64 = block.iter().map(|p| u64::from(p[3])).sum();
                    let expected: [u8; 4] = std::array::from_fn(|c| {
                        if c == 3 {
                            return ((alpha + count / 2) / count) as u8;
                        }
                        if alpha == 0 {
                            let sum: u64 = block.iter().map(|p| u64::from(p[c])).sum();
                            return ((sum + count / 2) / count) as u8;
                        }
                        let sum: u64 = block
                            .iter()
                            .map(|p| u64::from(p[c]) * u64::from(p[3]))
                            .sum();
                        ((sum + alpha / 2) / alpha) as u8
                    });
                    assert_eq!(*pixel, expected, "shift {shift} block {bx},{by}");
                }
            }
        }
    }

    #[test]
    fn blocked_accumulation_matches_direct_means_at_both_depths_and_every_edge() {
        fn check<P: Sample + std::fmt::Debug + PartialEq>(
            frame: &VideoFrame,
            pixels: &[[u8; 4]],
            width: usize,
            height: usize,
            shift: u32,
            weighted: bool,
        ) {
            let size = 1 << shift;
            for (by, row) in reduce::<P>(frame, shift, weighted).iter().enumerate() {
                for (bx, actual) in row.iter().enumerate() {
                    let mut plain = [0u64; 3];
                    let mut color = [0u64; 3];
                    let (mut alpha, mut count) = (0, 0);
                    for y in by * size..((by + 1) * size).min(height) {
                        for x in bx * size..((bx + 1) * size).min(width) {
                            let pixel = pixels[y * width + x].map(P::from_u8);
                            let opacity: u64 =
                                if weighted { pixel[3] } else { P::MAX }.widen().into();
                            for c in 0..3 {
                                let value: u64 = pixel[c].widen().into();
                                plain[c] += value;
                                color[c] += value * opacity;
                            }
                            alpha += opacity;
                            count += 1;
                        }
                    }
                    let average = |sum, divisor| P::narrow((sum + divisor / 2) / divisor);
                    let expected = std::array::from_fn(|c| {
                        if c == 3 {
                            average(alpha, count)
                        } else if weighted && alpha > 0 {
                            average(color[c], alpha)
                        } else {
                            average(plain[c], count)
                        }
                    });
                    assert_eq!(
                        *actual, expected,
                        "{width}x{height}, shift={shift}, weighted={weighted}, block={bx},{by}"
                    );
                }
            }
        }

        for width in [2, 6, 14, 16, 18, 30, 34] {
            for height in [1, 3, 9, 17] {
                for alpha_case in 0..3 {
                    let pixels: Vec<_> = (0..height)
                        .flat_map(|y| (0..width).map(move |x| (x, y)))
                        .map(|(x, y)| {
                            [
                                ((x * 7 + y * 3) % 256) as u8,
                                ((x * 3 + y * 11) % 256) as u8,
                                ((x * 5 + y * 7) % 256) as u8,
                                match alpha_case {
                                    0 => 0,
                                    1 => 255,
                                    _ => ((x * 17 + y * 29) % 256) as u8,
                                },
                            ]
                        })
                        .collect();
                    let frame = rgba(width as u32, height as u32, &pixels);
                    for shift in 1..=3 {
                        for weighted in [false, true] {
                            check::<u8>(&frame, &pixels, width, height, shift, weighted);
                            check::<u16>(&frame, &pixels, width, height, shift, weighted);
                        }
                    }
                }
            }
        }
    }
}
