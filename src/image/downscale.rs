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
    pub(super) fn new(reader: &Reader<'_, P>, shift: u32) -> Self {
        Self {
            shift,
            weighted: reader.has_alpha(),
            sums: vec![Default::default(); reduced(reader.width(), shift)],
        }
    }

    /// log2 of the factor.
    pub(super) fn shift(&self) -> u32 {
        self.shift
    }

    /// Average output row `y` of `reader`'s pixels into `sink`.
    pub(super) fn row(&mut self, reader: &Reader<'_, P>, y: usize, sink: &mut impl Sink<P>) {
        let size = 1 << self.shift;
        let top = y << self.shift;
        let bottom = (top + size).min(reader.height());
        self.sums.fill(Default::default());
        for source_y in top..bottom {
            reader.row(
                source_y,
                &mut Accumulate {
                    sums: &mut self.sums,
                    shift: self.shift,
                    weighted: self.weighted,
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
                let count = rows * (size.min(width - left)) as u64;
                *pixel = mean::<P>(*sum, count, self.weighted);
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
fn mean<P: Sample>(sum: [P::Sum; 4], count: u64, weighted: bool) -> [P; 4] {
    let [red, green, blue, alpha] = sum.map(Into::<u64>::into);
    let average = |total: u64, count: u64| P::narrow((total + count / 2) / count);
    // A block weighted by alpha whose alpha is zero throughout holds plain
    // sums, as `Accumulate` explains.
    let weight = if weighted && alpha > 0 { alpha } else { count };
    [
        average(red, weight),
        average(green, weight),
        average(blue, weight),
        average(alpha, count),
    ]
}

/// Adds a row's pixels into the accumulator of each one's output column.
struct Accumulate<'s, P: Sample> {
    sums: &'s mut [[P::Sum; 4]],
    shift: u32,
    weighted: bool,
    /// The next pixel's column.
    x: usize,
}

impl<P: Sample> Sink<P> for Accumulate<'_, P> {
    fn put(&mut self, pixels: &[[P; 4]]) {
        for &[red, green, blue, alpha] in pixels {
            let sum = &mut self.sums[self.x >> self.shift];
            self.x += 1;
            let color = [red.widen(), green.widen(), blue.widen()];
            let alpha = alpha.widen();
            if !self.weighted {
                for (total, value) in sum.iter_mut().zip(color) {
                    *total += value;
                }
                sum[3] += alpha;
                continue;
            }
            // Color weighted by alpha, Σ(a·c) / Σa, so that transparent
            // pixels do not darken edges. While every pixel so far is fully
            // transparent, the alpha sum is zero and the color sums hold
            // plain sums instead, for a block that stays transparent: its
            // plain mean. The first pixel with alpha replaces them, since a
            // transparent pixel's weighted contribution is zero.
            let transparent_so_far = sum[3] == P::Sum::default();
            if alpha == P::Sum::default() {
                if transparent_so_far {
                    for (total, value) in sum.iter_mut().zip(color) {
                        *total += value;
                    }
                }
                continue;
            }
            for (total, value) in sum.iter_mut().zip(color) {
                if transparent_so_far {
                    *total = P::Sum::default();
                }
                *total += alpha * value;
            }
            sum[3] += alpha;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{frames::VideoParts, image::Source, PixelFormat, VideoFrame, YuvMatrix};

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
        let rows = reduced(VideoParts::layout(frame).height as usize, shift);
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
}
