//! The color stage: YCbCr codes to RGB at the image's depth, in fixed point.
//!
//! Readers hand the kernel 16-bit codes, an 8-bit sample shifted left by 8,
//! so one kernel serves every source depth. With n the source depth and
//! s = 2^(16 − n):
//!
//! * limited range: Y' = (Y − 4096) / 56064, P = (C − 32768) / 57344;
//! * full range: Y' = Y / ((2^n − 1)·s), P = (C − 2^15) / ((2^n − 1)·s);
//!
//! then R = Y' + 2(1 − K<sub>R</sub>)·P<sub>R</sub>, B = Y' + 2(1 − K<sub>B</sub>)·P<sub>B</sub> and
//! G = (Y' − K<sub>R</sub>·R − K<sub>B</sub>·B) / (1 − K<sub>R</sub> − K<sub>B</sub>), each clamped to 0 to 1
//! and rounded to the image's maximum code.

use std::ops::{Add, Mul, Neg, Shl, Shr, Sub};

use super::{options::YuvRange, read::BATCH};
use crate::YuvMatrix;

/// One channel of an encoded image: `u8` for 8-bit images and `u16` for
/// 16-bit ones.
pub(super) trait Sample: Copy + Default + Into<f64> + Send + Sync + 'static {
    /// The largest code.
    const MAX: Self;
    /// The bytes one sample takes.
    const BYTES: usize;
    /// The kernel's fixed-point type at this depth: products of 17-bit codes
    /// and coefficients of [`Self::FRACTION`] fractional bits fit in it.
    type Fixed: Fixed;
    /// The fractional bits of the kernel's coefficients. Rounding the three
    /// coefficients a channel sums moves it by under 0.05 of a code.
    const FRACTION: u32;
    /// [`Self::MAX`] as a kernel value.
    const MAX_CODE: Self::Fixed;
    /// The sum of up to 64 samples, each weighted by an alpha: the downscale
    /// accumulator.
    type Sum: Copy
        + Default
        + PartialEq
        + Into<u64>
        + std::ops::AddAssign
        + std::ops::Mul<Output = Self::Sum>;

    /// An 8-bit full-range value: an RGB channel or an alpha.
    fn from_u8(value: u8) -> Self;
    /// A 16-bit full-range value: an alpha.
    fn from_u16(value: u16) -> Self;
    /// A kernel result, rounded and clamped to `0..=MAX_CODE` already.
    fn from_code(code: Self::Fixed) -> Self;
    /// The value as a sum.
    fn widen(self) -> Self::Sum;
    /// A mean of sums, which is at most [`Self::MAX`].
    fn narrow(mean: u64) -> Self;
    /// Pack the first `CH` samples of each pixel into `out`, which holds
    /// exactly that many, in PNG's byte order: big-endian.
    fn pack<const CH: usize>(pixels: &[[Self; 4]], out: &mut [u8]);
}

impl Sample for u8 {
    const MAX: Self = u8::MAX;
    const BYTES: usize = 1;
    /// At 21 fractional bits a channel's largest sum, a 16-bit luma term
    /// and a chroma term, stays under 1.3 × 2^30.
    type Fixed = i32;
    const FRACTION: u32 = 21;
    const MAX_CODE: i32 = 255;
    type Sum = u32;

    #[inline(always)]
    fn from_u8(value: u8) -> Self {
        value
    }
    /// round(a × 255 / 65535) = round(a / 257), which `(a + 128) / 257`
    /// computes exactly because 257 is odd.
    #[inline(always)]
    fn from_u16(value: u16) -> Self {
        ((u32::from(value) + 128) / 257) as u8
    }
    #[inline(always)]
    fn from_code(code: i32) -> Self {
        code as u8
    }
    #[inline(always)]
    fn widen(self) -> u32 {
        u32::from(self)
    }
    #[inline(always)]
    fn narrow(mean: u64) -> Self {
        mean as u8
    }
    #[inline(always)]
    fn pack<const CH: usize>(pixels: &[[Self; 4]], out: &mut [u8]) {
        for (pixel, out) in pixels.iter().zip(out.as_chunks_mut::<CH>().0) {
            out.copy_from_slice(&pixel[..CH]);
        }
    }
}

impl Sample for u16 {
    const MAX: Self = u16::MAX;
    const BYTES: usize = 2;
    type Fixed = i64;
    const FRACTION: u32 = 32;
    const MAX_CODE: i64 = 65535;
    type Sum = u64;

    /// Exact: 255 × 257 = 65535.
    #[inline(always)]
    fn from_u8(value: u8) -> Self {
        u16::from(value) * 257
    }
    #[inline(always)]
    fn from_u16(value: u16) -> Self {
        value
    }
    #[inline(always)]
    fn from_code(code: i64) -> Self {
        code as u16
    }
    #[inline(always)]
    fn widen(self) -> u64 {
        u64::from(self)
    }
    #[inline(always)]
    fn narrow(mean: u64) -> Self {
        mean as u16
    }
    #[inline(always)]
    fn pack<const CH: usize>(pixels: &[[Self; 4]], out: &mut [u8]) {
        let samples = pixels.iter().flat_map(|pixel| &pixel[..CH]);
        for (sample, out) in samples.zip(out.as_chunks_mut::<2>().0) {
            *out = sample.to_be_bytes();
        }
    }
}

/// Signed fixed-point arithmetic: `i32` or `i64`.
pub(super) trait Fixed:
    Copy
    + std::fmt::Debug
    + Ord
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Neg<Output = Self>
    + Shl<u32, Output = Self>
    + Shr<u32, Output = Self>
{
    const ZERO: Self;
    const ONE: Self;
    fn from_u16(value: u16) -> Self;
    fn round_from(value: f64) -> Self;
}

impl Fixed for i32 {
    const ZERO: Self = 0;
    const ONE: Self = 1;
    #[inline(always)]
    fn from_u16(value: u16) -> Self {
        i32::from(value)
    }
    fn round_from(value: f64) -> Self {
        value.round() as i32
    }
}

impl Fixed for i64 {
    const ZERO: Self = 0;
    const ONE: Self = 1;
    #[inline(always)]
    fn from_u16(value: u16) -> Self {
        i64::from(value)
    }
    fn round_from(value: f64) -> Self {
        value.round() as i64
    }
}

/// The 16-bit codes of up to [`BATCH`] pixels, one array a channel, with
/// chroma already repeated across the pixels it covers.
#[derive(Debug, Default)]
pub(super) struct Codes {
    luma: [u16; BATCH],
    cb: [u16; BATCH],
    cr: [u16; BATCH],
}

impl Codes {
    /// The codes of each pixel pair, in order: the 4:2:2 and 4:2:0 readers
    /// fill a pair at a time.
    pub(super) fn pairs(&mut self) -> impl Iterator<Item = Pair<'_>> {
        let luma = self.luma.as_chunks_mut::<2>().0.iter_mut();
        let cb = self.cb.as_chunks_mut::<2>().0.iter_mut();
        let cr = self.cr.as_chunks_mut::<2>().0.iter_mut();
        luma.zip(cb)
            .zip(cr)
            .map(|((luma, cb), cr)| Pair { luma, cb, cr })
    }

    /// The luma, Cb and Cr arrays, in that order: the 4:4:4 readers fill a
    /// channel at a time.
    pub(super) fn channels(&mut self) -> impl Iterator<Item = &mut [u16; BATCH]> {
        [&mut self.luma, &mut self.cb, &mut self.cr].into_iter()
    }
}

/// The codes of one pixel pair, which share their chroma.
pub(super) struct Pair<'c> {
    luma: &'c mut [u16; 2],
    cb: &'c mut [u16; 2],
    cr: &'c mut [u16; 2],
}

impl Pair<'_> {
    #[inline(always)]
    pub(super) fn set(self, luma: [u16; 2], cb: u16, cr: u16) {
        *self.luma = luma;
        *self.cb = [cb; 2];
        *self.cr = [cr; 2];
    }
}

/// A YCbCr to RGB conversion at the depth of `P`: one matrix, one range and
/// one source depth, in fixed point.
#[derive(Debug, Clone, Copy)]
pub(super) struct Kernel<P: Sample> {
    luma_offset: P::Fixed,
    luma_gain: P::Fixed,
    red_cr: P::Fixed,
    green_cb: P::Fixed,
    green_cr: P::Fixed,
    blue_cb: P::Fixed,
}

impl<P: Sample> Kernel<P> {
    /// The conversion for `matrix` and `range` from a source of `bits` bits a
    /// sample, 8 or 16.
    pub(super) fn new(matrix: YuvMatrix, range: YuvRange, bits: u32) -> Self {
        let (luma_offset, luma_span, chroma_span) = spans(range, bits);
        let (kr, kb) = coefficients(matrix);
        let kg = 1.0 - kr - kb;
        let max: f64 = P::MAX.into();
        let scale = max * (1u64 << P::FRACTION) as f64;
        let fixed = |value: f64| P::Fixed::round_from(value * scale);
        Self {
            luma_offset: P::Fixed::from_u16(luma_offset),
            luma_gain: fixed(1.0 / luma_span),
            red_cr: fixed(2.0 * (1.0 - kr) / chroma_span),
            green_cb: fixed(2.0 * kb * (1.0 - kb) / kg / chroma_span),
            green_cr: fixed(2.0 * kr * (1.0 - kr) / kg / chroma_span),
            blue_cb: fixed(2.0 * (1.0 - kb) / chroma_span),
        }
    }

    /// Convert every pixel of `codes` into the RGB of `pixels`, leaving
    /// their alpha. The batch is always converted whole, a fixed number of
    /// lanes that the compiler can vectorize; pixels past the batch's end
    /// hold no meaning.
    #[inline(always)]
    pub(super) fn convert(&self, codes: &Codes, pixels: &mut [[P; 4]; BATCH]) {
        let half = P::Fixed::ONE << (P::FRACTION - 1);
        let middle = P::Fixed::from_u16(32768);
        let narrow = |value: P::Fixed| {
            P::from_code((value >> P::FRACTION).clamp(P::Fixed::ZERO, P::MAX_CODE))
        };
        for (i, pixel) in pixels.iter_mut().enumerate() {
            let luma =
                (P::Fixed::from_u16(codes.luma[i]) - self.luma_offset) * self.luma_gain + half;
            let cb = P::Fixed::from_u16(codes.cb[i]) - middle;
            let cr = P::Fixed::from_u16(codes.cr[i]) - middle;
            // Whole pixels, so that the stores are contiguous; alpha follows.
            *pixel = [
                narrow(luma + self.red_cr * cr),
                narrow(luma - self.green_cb * cb - self.green_cr * cr),
                narrow(luma + self.blue_cb * cb),
                P::MAX,
            ];
        }
    }
}

/// K<sub>R</sub> and K<sub>B</sub> of a matrix.
pub(super) const fn coefficients(matrix: YuvMatrix) -> (f64, f64) {
    match matrix {
        YuvMatrix::Bt601 => (0.299, 0.114),
        YuvMatrix::Bt709 => (0.2126, 0.0722),
        YuvMatrix::Bt2020 => (0.2627, 0.0593),
    }
}

/// The luma offset, luma span and chroma span of 16-bit codes from a source
/// of `bits` bits a sample.
pub(super) fn spans(range: YuvRange, bits: u32) -> (u16, f64, f64) {
    match range {
        YuvRange::Limited => (16 << 8, f64::from(219 << 8), f64::from(224 << 8)),
        YuvRange::Full => {
            let span = f64::from(((1u32 << bits) - 1) << (16 - bits));
            (0, span, span)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The formulas in `f64`: the reference the kernel must stay within one
    /// code of.
    fn reference(
        matrix: YuvMatrix,
        range: YuvRange,
        bits: u32,
        max: f64,
        [y, cb, cr]: [u16; 3],
    ) -> [f64; 3] {
        let (luma_offset, luma_span, chroma_span) = spans(range, bits);
        let (kr, kb) = coefficients(matrix);
        let luma = (f64::from(y) - f64::from(luma_offset)) / luma_span;
        let pb = (f64::from(cb) - 32768.0) / chroma_span;
        let pr = (f64::from(cr) - 32768.0) / chroma_span;
        let r = luma + 2.0 * (1.0 - kr) * pr;
        let b = luma + 2.0 * (1.0 - kb) * pb;
        let g = (luma - kr * r - kb * b) / (1.0 - kr - kb);
        [r, g, b].map(|value| (value.clamp(0.0, 1.0) * max).round())
    }

    /// The boundary codes of the spec at 8 bits, as 16-bit codes.
    const BOUNDARY: [u16; 16] = [
        0, 1, 15, 16, 17, 127, 128, 129, 234, 235, 236, 239, 240, 241, 254, 255,
    ];

    /// The RGB of each of `codes`, converted a batch at a time.
    fn convert<P: Sample>(kernel: &Kernel<P>, codes: &[[u16; 3]]) -> Vec<[P; 3]> {
        let mut rgb = Vec::new();
        for chunk in codes.chunks(BATCH) {
            let mut batch = Codes::default();
            for (i, &[luma, cb, cr]) in chunk.iter().enumerate() {
                (batch.luma[i], batch.cb[i], batch.cr[i]) = (luma, cb, cr);
            }
            let mut pixels = [[P::MAX; 4]; BATCH];
            kernel.convert(&batch, &mut pixels);
            rgb.extend(pixels[..chunk.len()].iter().map(|&[r, g, b, _]| [r, g, b]));
        }
        rgb
    }

    /// Every matrix and range against the reference, at most one code
    /// apart.
    fn check<P: Sample + Into<u32>>(codes: impl Iterator<Item = [u16; 3]> + Clone, bits: u32) {
        let max: f64 = P::MAX.into();
        for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
            for range in [YuvRange::Limited, YuvRange::Full] {
                let kernel = Kernel::<P>::new(matrix, range, bits);
                let codes: Vec<[u16; 3]> = codes.clone().collect();
                for (code, rgb) in codes.iter().copied().zip(convert(&kernel, &codes)) {
                    let expected = reference(matrix, range, bits, max, code);
                    for (got, want) in rgb.into_iter().zip(expected) {
                        let got: u32 = got.into();
                        assert!(
                            (f64::from(got) - want).abs() <= 1.0,
                            "{matrix:?} {range:?} {bits}-bit {code:?}: {got} against {want}"
                        );
                    }
                }
            }
        }
    }

    fn boundary_codes(shift: u32) -> impl Iterator<Item = [u16; 3]> + Clone {
        BOUNDARY.into_iter().flat_map(move |y| {
            BOUNDARY.into_iter().flat_map(move |cb| {
                BOUNDARY
                    .into_iter()
                    .map(move |cr| [y << shift, cb << shift, cr << shift])
            })
        })
    }

    fn random_codes(seed: u64, mask: u16) -> impl Iterator<Item = [u16; 3]> + Clone {
        let mut state = seed;
        (0..20_000).map(move |_| {
            [0; 3].map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u16 & mask
            })
        })
    }

    #[test]
    fn kernel_matches_the_formulas_at_both_depths_from_8_bit_sources() {
        check::<u8>(boundary_codes(8), 8);
        check::<u16>(boundary_codes(8), 8);
        check::<u8>(random_codes(7, 0xFF00), 8);
        check::<u16>(random_codes(7, 0xFF00), 8);
    }

    #[test]
    fn kernel_matches_the_formulas_at_both_depths_from_16_bit_sources() {
        let full = BOUNDARY
            .into_iter()
            .flat_map(|value| [value << 8, (value << 8) | 0xFF, value * 257]);
        let codes: Vec<u16> = full
            .chain([0, 1, 0x7FFF, 0x8000, 0x8001, 0xFFFE, 0xFFFF])
            .collect();
        let codes = &codes;
        let triples: Vec<[u16; 3]> = codes
            .iter()
            .flat_map(|&y| {
                codes
                    .iter()
                    .flat_map(move |&cb| codes.iter().map(move |&cr| [y, cb, cr]))
            })
            .collect();
        check::<u8>(triples.iter().copied(), 16);
        check::<u16>(triples.iter().copied(), 16);
        check::<u8>(random_codes(11, 0xFFFF), 16);
        check::<u16>(random_codes(11, 0xFFFF), 16);
    }

    #[test]
    fn limited_range_extremes_are_exact() {
        for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
            let eight = Kernel::<u8>::new(matrix, YuvRange::Limited, 8);
            let neutral = 128 << 8;
            let rgb8 = |luma| convert(&eight, &[[luma, neutral, neutral]])[0];
            assert_eq!(rgb8(16 << 8), [0; 3]);
            assert_eq!(rgb8(235 << 8), [255; 3]);
            assert_eq!(rgb8(4 << 8), [0; 3], "sub-black clips");
            assert_eq!(rgb8(250 << 8), [255; 3], "super-white clips");
            let sixteen = Kernel::<u16>::new(matrix, YuvRange::Limited, 16);
            let rgb16 = |luma| convert(&sixteen, &[[luma, neutral, neutral]])[0];
            assert_eq!(rgb16(235 << 8), [65535; 3]);
            // 10-bit white, 940, in the top bits of a 16-bit sample.
            assert_eq!(rgb16(940 << 6), [65535; 3]);
            let full = Kernel::<u8>::new(matrix, YuvRange::Full, 8);
            assert_eq!(convert(&full, &[[255 << 8, neutral, neutral]])[0], [255; 3]);
            assert_eq!(convert(&full, &[[0, neutral, neutral]])[0], [0; 3]);
        }
    }

    #[test]
    fn alpha_and_rgb_conversions_between_depths_are_exact() {
        for value in 0..=255u8 {
            assert_eq!(u16::from_u8(value), u16::from(value) * 257);
            assert_eq!(u8::from_u16(u16::from(value) * 257), value);
        }
        for value in 0..=u16::MAX {
            let expected = (f64::from(value) * 255.0 / 65535.0).round() as u8;
            assert_eq!(u8::from_u16(value), expected, "{value}");
        }
    }
}
