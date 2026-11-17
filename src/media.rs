//! Media values shared by frames, packets, receivers and recorded files.

use std::{
    hash::{Hash, Hasher},
    num::NonZeroU32,
    ops::{BitOr, BitOrAssign},
};

use crate::{AudioFormat, Error, PixelFormat, Result};

// An omitted variant makes index() non-exhaustive; a duplicate cannot define
// its index twice. Keep decoding and iteration tied to the same checked list.
macro_rules! decoded_variants {
    ($name:ident: $($variant:ident),+ $(,)?) => {
        impl $name {
            /// Every variant, in declaration order.
            pub(crate) const ALL: [Self; [$(stringify!($variant)),+].len()] =
                [$(Self::$variant),+];

            const fn index(self) -> usize {
                enum Index { $($variant),+ }
                match self { $(Self::$variant => Index::$variant as usize),+ }
            }
        }

        const _: () = {
            let mut index = 0;
            while index < $name::ALL.len() {
                assert!($name::ALL[index].index() == index);
                index += 1;
            }
        };
    };
}
pub(crate) use decoded_variants;

/// A positive quantity that fits the SDK's signed integer fields.
/// Construction retains the range proof until the native conversion.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SdkQuantity(NonZeroU32);

// Keep the private range proof out of the public values' numeric Debug fields.
impl std::fmt::Debug for SdkQuantity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl SdkQuantity {
    pub(crate) const fn new(value: i64) -> Option<Self> {
        // Explicit comparisons keep the shared predicate usable in const
        // contexts on the minimum supported Rust version.
        if value < 1 || value > i32::MAX as i64 {
            return None;
        }
        match NonZeroU32::new(value as u32) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub(crate) const fn get(self) -> u32 {
        self.0.get()
    }

    pub(crate) const fn to_sdk(self) -> i32 {
        // Lossless: construction proves the value is in 1..=i32::MAX.
        self.get() as i32
    }

    pub(crate) const fn to_usize(self) -> usize {
        self.get() as usize
    }
}

const _: () = assert!(std::mem::size_of::<Option<SdkQuantity>>() == 4);

/// A frame rate in frames per second, as the fraction
/// `numerator / denominator`.
///
/// Equality and hashing compare the rate: `30/1` and `60/2` are equal.
/// The supplied numerator and denominator are preserved by the accessors and
/// when submitted to the SDK. Compare both components explicitly when their
/// exact representation matters.
///
/// # Examples
///
/// ```
/// use grafton_ndi::FrameRate;
///
/// // NTSC: 29.97 frames per second.
/// let ntsc = FrameRate::new(30_000, 1_001)?;
/// assert_eq!((ntsc.numerator(), ntsc.denominator()), (30_000, 1_001));
/// assert!(FrameRate::new(30, 0).is_err());
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
#[derive(Debug, Clone, Copy, Eq)]
pub struct FrameRate {
    numerator: SdkQuantity,
    denominator: SdkQuantity,
}

/// The shared default for owned, borrowed and compressed video construction.
pub(crate) const DEFAULT_VIDEO_FRAME_RATE: FrameRate = FrameRate::per_second(60);

impl PartialEq for FrameRate {
    fn eq(&self, other: &Self) -> bool {
        // Each component is at most i32::MAX, so either product fits in u64.
        u64::from(self.numerator.get()) * u64::from(other.denominator.get())
            == u64::from(other.numerator.get()) * u64::from(self.denominator.get())
    }
}

impl Hash for FrameRate {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let (mut a, mut b) = (self.numerator.get(), self.denominator.get());
        while b != 0 {
            (a, b) = (b, a % b);
        }
        (self.numerator.get() / a, self.denominator.get() / a).hash(state);
    }
}

impl FrameRate {
    /// A frame rate of `numerator / denominator` frames per second.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] unless both parts are in `1..=i32::MAX`, the
    /// range the SDK can carry.
    pub fn new(numerator: u32, denominator: u32) -> Result<Self> {
        Self::checked(numerator.into(), denominator.into())
            .map_err(|cause| Error::InvalidInput(crate::InputDefect::Layout(cause)))
    }

    /// `frames` frames per second, for the builders' defaults. Call it in a
    /// `const` block, where a value outside `1..=i32::MAX` fails to compile.
    pub(crate) const fn per_second(frames: u32) -> Self {
        Self {
            numerator: match SdkQuantity::new(frames as i64) {
                Some(value) => value,
                #[expect(
                    clippy::panic,
                    reason = "invalid frame-rate constants must fail during const evaluation"
                )]
                None => panic!("frame rate must be in 1..=i32::MAX"),
            },
            denominator: match SdkQuantity::new(1) {
                Some(value) => value,
                #[expect(
                    clippy::unreachable,
                    reason = "one is in the checked SDK quantity range"
                )]
                None => unreachable!(),
            },
        }
    }

    /// The frame rate of a frame the SDK delivered; an invalid one makes the
    /// frame invalid.
    pub(crate) fn from_sdk(numerator: i32, denominator: i32) -> Result<Self> {
        Self::checked(numerator.into(), denominator.into()).map_err(Into::into)
    }

    /// The one frame-rate check, for values from the API and from the SDK.
    /// The error is the reason, for the caller to report as its own variant.
    fn checked(numerator: i64, denominator: i64) -> crate::frames::LayoutResult<Self> {
        match (SdkQuantity::new(numerator), SdkQuantity::new(denominator)) {
            (Some(numerator), Some(denominator)) => Ok(Self {
                numerator,
                denominator,
            }),
            _ => Err(crate::LayoutDefect::FrameRate {
                detail: crate::CompactDiagnostic::new(crate::FrameRateDefect {
                    numerator,
                    denominator,
                }),
            }),
        }
    }

    /// The number of frames in [`denominator`](Self::denominator) seconds.
    #[must_use]
    pub const fn numerator(self) -> u32 {
        self.numerator.get()
    }

    /// The number of seconds that [`numerator`](Self::numerator) frames take.
    #[must_use]
    pub const fn denominator(self) -> u32 {
        self.denominator.get()
    }

    /// The SDK's `frame_rate_N` and `frame_rate_D` fields.
    pub(crate) const fn to_sdk(self) -> (i32, i32) {
        (self.numerator.to_sdk(), self.denominator.to_sdk())
    }
}

/// The displayed picture's width divided by its height.
///
/// Zero asks the SDK to derive the ratio from the frame dimensions (square
/// pixels). Every other value is finite and positive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureAspectRatio(f32);

impl Eq for PictureAspectRatio {}

impl Hash for PictureAspectRatio {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl PictureAspectRatio {
    /// Derive the displayed aspect ratio from the pixel dimensions.
    pub const SQUARE_PIXELS: Self = Self(0.0);

    /// A finite, nonnegative ratio; zero means square pixels.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `ratio` is negative or not finite.
    pub fn new(ratio: f32) -> Result<Self> {
        Self::checked(ratio)
            .map_err(|reason| Error::InvalidInput(crate::InputDefect::Layout(reason)))
    }

    /// The ratio, with zero representing square pixels.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    pub(crate) fn from_sdk(ratio: f32) -> Result<Self> {
        Self::checked(ratio).map_err(Into::into)
    }

    pub(crate) const fn to_sdk(self) -> f32 {
        self.0
    }

    fn checked(ratio: f32) -> crate::frames::LayoutResult<Self> {
        if !ratio.is_finite() || ratio < 0.0 {
            return Err(crate::LayoutDefect::PictureAspectRatio(ratio));
        }
        // Equality treats both zeros alike; store one bit pattern for hashing.
        Ok(Self(if ratio == 0.0 { 0.0 } else { ratio }))
    }
}

/// A four-character code: the SDK's identifier for a pixel or audio format or codec.
///
/// It holds any value, including one this crate does not know, so a value
/// read from the SDK is never lost. [`PixelFormat`] and [`AudioFormat`] convert
/// to and from it.
///
/// # Examples
///
/// ```
/// use grafton_ndi::{FourCc, PixelFormat};
///
/// let fourcc = FourCc::from(PixelFormat::Uyvy);
/// assert_eq!(fourcc.raw().to_le_bytes(), *b"UYVY");
/// assert_eq!(PixelFormat::try_from(fourcc)?, PixelFormat::Uyvy);
/// assert!(PixelFormat::try_from(FourCc::from_raw(u32::from_le_bytes(*b"V210"))).is_err());
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCc(u32);

impl FourCc {
    /// The code with the SDK's numeric value `value`: its four characters in
    /// little-endian byte order.
    #[must_use]
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    /// The SDK's numeric value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl From<PixelFormat> for FourCc {
    fn from(format: PixelFormat) -> Self {
        // Each variant's discriminant is the SDK's FourCC value.
        Self(format as u32)
    }
}

impl PixelFormat {
    /// Decode an uncompressed format without allocating for an unknown FourCC.
    pub(crate) fn from_fourcc(fourcc: FourCc) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|&format| FourCc::from(format) == fourcc)
    }
}

impl TryFrom<FourCc> for PixelFormat {
    type Error = Error;

    /// The pixel format `fourcc` names.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when it names no uncompressed pixel format
    /// this crate supports.
    fn try_from(fourcc: FourCc) -> Result<Self> {
        Self::from_fourcc(fourcc).ok_or({
            Error::InvalidFrame(crate::FrameDefect::UnknownPixelFormat { fourcc: fourcc.0 })
        })
    }
}

impl From<AudioFormat> for FourCc {
    fn from(format: AudioFormat) -> Self {
        Self(format.to_sdk())
    }
}

impl TryFrom<FourCc> for AudioFormat {
    type Error = Error;

    /// The audio format `fourcc` names.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when it names no uncompressed audio format
    /// this crate supports.
    fn try_from(fourcc: FourCc) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|&format| FourCc::from(format) == fourcc)
            .ok_or(Error::InvalidFrame(
                crate::FrameDefect::UnknownAudioFormat {
                    fourcc: fourcc.raw(),
                },
            ))
    }
}

/// Which of a source's two video streams: the full-quality program stream or
/// the low-bandwidth preview stream.
///
/// A source can send both. A receiver takes its video from one of them, a
/// compressed frame belongs to one of them, and a recorded file holds a track
/// of each it recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamTier {
    /// The full-quality program stream.
    Program,
    /// The low-bandwidth preview stream.
    Preview,
}

/// A set of media types: any combination of [`VIDEO`](Self::VIDEO),
/// [`AUDIO`](Self::AUDIO) and [`METADATA`](Self::METADATA).
///
/// # Examples
///
/// ```
/// use grafton_ndi::MediaTypes;
///
/// let mut types = MediaTypes::VIDEO | MediaTypes::METADATA;
/// assert!(types.contains(MediaTypes::VIDEO));
/// assert!(!types.contains(MediaTypes::AUDIO));
/// types |= MediaTypes::AUDIO;
/// assert_eq!(types, MediaTypes::ALL);
/// assert!(MediaTypes::NONE.is_empty());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MediaTypes(u8);

impl MediaTypes {
    /// No media type.
    pub const NONE: Self = Self(0);
    /// Video.
    pub const VIDEO: Self = Self(1);
    /// Audio.
    pub const AUDIO: Self = Self(1 << 1);
    /// Metadata.
    pub const METADATA: Self = Self(1 << 2);
    /// Video, audio and metadata.
    pub const ALL: Self = Self(Self::VIDEO.0 | Self::AUDIO.0 | Self::METADATA.0);

    /// Whether every type in `other` is in this set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether the set has no type in it.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for MediaTypes {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOrAssign for MediaTypes {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile the private const constructor itself, so rejection cannot be
    /// satisfied accidentally by a visibility or non-const-call error.
    #[test]
    #[cfg(not(miri))]
    fn constant_frame_rates_reject_zero() {
        fn declaration<'a>(source: &'a str, marker: &str) -> &'a str {
            let source = &source[source.find(marker).unwrap()..];
            let mut depth = 0;
            for (index, byte) in source.bytes().enumerate() {
                match byte {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return &source[..=index];
                        }
                    }
                    _ => {}
                }
            }
            panic!("unterminated declaration: {marker}")
        }
        let source = include_str!("media.rs");
        let quantity = declaration(source, "impl SdkQuantity {");
        let rate = declaration(source, "pub struct FrameRate {");
        let per_second = declaration(source, "pub(crate) const fn per_second(");
        let fixture = include_str!("../tests/ui/internal/fail/frame_rate_zero_const.rs");
        let definitions = format!(
            "#![allow(dead_code)]\nuse std::num::NonZeroU32;\n\
             struct SdkQuantity(NonZeroU32);\n{quantity}\n{rate}\n\
             impl FrameRate {{ {per_second} }}\n"
        );
        let directory =
            std::env::temp_dir().join(format!("grafton-const-frame-rate-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        for (name, fixture, succeeds) in [
            (
                "positive",
                fixture.replace("per_second(0)", "per_second(1)"),
                true,
            ),
            ("zero", fixture.to_owned(), false),
        ] {
            let path = directory.join(format!("{name}.rs"));
            std::fs::write(&path, format!("{definitions}\n{fixture}")).unwrap();
            let output = std::process::Command::new("rustc")
                .args(["--edition=2021", "--emit=metadata", "--out-dir"])
                .arg(&directory)
                .arg(path)
                .output()
                .unwrap();
            let error = String::from_utf8(output.stderr).unwrap();
            assert_eq!(output.status.success(), succeeds, "{name}: {error}");
            if !succeeds {
                assert!(error.contains("E0080"), "{error}");
            }
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn sdk_quantities_preserve_exactly_the_signed_positive_range() {
        const ONE: SdkQuantity = SdkQuantity::new(1).unwrap();
        const MAX: SdkQuantity = SdkQuantity::new(i32::MAX as i64).unwrap();
        const DEFAULT: FrameRate = FrameRate::per_second(60);
        assert_eq!((ONE.get(), ONE.to_sdk(), ONE.to_usize()), (1, 1, 1));
        assert_eq!(MAX.to_sdk(), i32::MAX);
        assert_eq!(DEFAULT.to_sdk(), (60, 1));
        assert_eq!(
            format!("{DEFAULT:?}"),
            "FrameRate { numerator: 60, denominator: 1 }"
        );
        for value in [
            i64::MIN,
            -1,
            0,
            i32::MAX as i64 + 1,
            u32::MAX as i64,
            i64::MAX,
        ] {
            assert_eq!(SdkQuantity::new(value), None, "{value}");
        }
        for value in [1, 48_000, i32::MAX as i64] {
            let quantity = SdkQuantity::new(value).unwrap();
            assert_eq!(i64::from(quantity.get()), value);
            assert_eq!(i64::from(quantity.to_sdk()), value);
            assert_eq!(quantity.to_usize() as u64, value as u64);
        }
        for value in [0, i32::MAX as u32 + 1, u32::MAX] {
            assert!(std::panic::catch_unwind(|| FrameRate::per_second(value)).is_err());
        }
    }

    #[test]
    fn picture_aspect_ratio_accepts_only_finite_nonnegative_values() {
        assert_eq!(PictureAspectRatio::SQUARE_PIXELS.get(), 0.0);
        for value in [0.0, -0.0, 1.0, 16.0 / 9.0, f32::MAX] {
            let ratio = PictureAspectRatio::new(value).unwrap();
            assert_eq!(
                ratio.get().to_bits(),
                if value == 0.0 {
                    0.0f32.to_bits()
                } else {
                    value.to_bits()
                }
            );
            assert_eq!(PictureAspectRatio::from_sdk(ratio.to_sdk()).unwrap(), ratio);
        }
        for value in [-1.0, f32::NEG_INFINITY, f32::INFINITY, f32::NAN] {
            assert!(matches!(
                PictureAspectRatio::new(value),
                Err(Error::InvalidInput(_))
            ));
            assert!(matches!(
                PictureAspectRatio::from_sdk(value),
                Err(Error::InvalidFrame(_))
            ));
        }
    }

    #[test]
    fn picture_aspect_ratio_equality_and_hashing_share_canonical_zero() {
        let zero = PictureAspectRatio::new(0.0).unwrap();
        let negative_zero = PictureAspectRatio::new(-0.0).unwrap();
        assert_eq!(negative_zero.get().to_bits(), 0.0f32.to_bits());
        let values = std::collections::HashSet::from([zero, negative_zero]);
        assert_eq!(values.len(), 1);
        for value in [0.0, 1.0, 16.0 / 9.0, f32::MAX] {
            assert_eq!(
                PictureAspectRatio::new(value).unwrap(),
                PictureAspectRatio::new(value).unwrap()
            );
        }
        assert_ne!(zero, PictureAspectRatio::new(1.0).unwrap());
    }

    #[test]
    fn every_audio_format_round_trips_through_its_fourcc() {
        for format in AudioFormat::ALL {
            let fourcc = FourCc::from(format);
            assert_eq!(AudioFormat::try_from(fourcc).unwrap(), format);
        }
        assert_eq!(
            FourCc::from(AudioFormat::Fltp).raw().to_le_bytes(),
            *b"FLTp"
        );
        let unknown = FourCc::from_raw(u32::MAX);
        assert!(
            matches!(AudioFormat::try_from(unknown), Err(Error::InvalidFrame(crate::FrameDefect::UnknownAudioFormat { fourcc })) if fourcc == unknown.raw())
        );
    }

    #[test]
    fn frame_rate_accepts_exactly_the_sdk_range_from_both_sides() {
        let max = i32::MAX.unsigned_abs();
        for (numerator, denominator, valid) in [
            (1, 1, true),
            (30_000, 1_001, true),
            (max, max, true),
            (0, 1, false),
            (1, 0, false),
            (max + 1, 1, false),
            (1, u32::MAX, false),
        ] {
            let rate = FrameRate::new(numerator, denominator);
            assert_eq!(rate.is_ok(), valid, "{numerator}/{denominator}");
            if let Ok(rate) = rate {
                assert_eq!(
                    (rate.numerator(), rate.denominator()),
                    (numerator, denominator)
                );
                let (n, d) = rate.to_sdk();
                assert_eq!(FrameRate::from_sdk(n, d).unwrap(), rate);
            }
        }
        for (numerator, denominator) in [(-1, 1), (1, -1), (0, 0), (i32::MIN, i32::MIN)] {
            assert!(matches!(
                FrameRate::from_sdk(numerator, denominator),
                Err(Error::InvalidFrame(message)) if matches!(&message, crate::FrameDefect::Layout(crate::LayoutDefect::FrameRate {detail}) if detail.numerator==i64::from(numerator) && detail.denominator==i64::from(denominator))
            ));
        }
    }

    #[test]
    fn equivalent_frame_rates_compare_and_hash_equally_without_losing_components() {
        use std::collections::{HashMap, HashSet};
        let rate = |n, d| FrameRate::new(n, d).unwrap();
        let max = i32::MAX as u32;
        for (left, right) in [
            (rate(30, 1), rate(60, 2)),
            (rate(30_000, 1001), rate(60_000, 2002)),
            (rate(1, 1), rate(max, max)),
        ] {
            assert_eq!(left, right);
            assert_eq!(HashSet::from([left, right]).len(), 1);
            assert_eq!(HashMap::from([(left, "rate")]).get(&right), Some(&"rate"));
        }
        assert_ne!(rate(30, 1), rate(30_000, 1001));
        assert_ne!(rate(max, max - 1), rate(max - 1, max - 2));
        assert_eq!(
            (rate(60, 2).numerator(), rate(60, 2).denominator()),
            (60, 2)
        );
        assert_eq!(rate(60, 2).to_sdk(), (60, 2));
        assert_eq!(FrameRate::from_sdk(60, 2).unwrap().to_sdk(), (60, 2));
    }

    #[test]
    fn every_pixel_format_round_trips_through_its_fourcc() {
        for format in PixelFormat::ALL {
            let fourcc = FourCc::from(format);
            assert_eq!(PixelFormat::try_from(fourcc).unwrap(), format);
            assert!(fourcc
                .raw()
                .to_le_bytes()
                .iter()
                .all(u8::is_ascii_alphanumeric));
        }
        assert_eq!(
            FourCc::from(PixelFormat::Nb4g).raw().to_le_bytes(),
            *b"NB4G"
        );
        let v210 = FourCc::from_raw(u32::from_le_bytes(*b"V210"));
        assert!(matches!(
            PixelFormat::try_from(v210),
            Err(Error::InvalidFrame(message)) if matches!(message, crate::FrameDefect::UnknownPixelFormat {fourcc} if fourcc==v210.raw())
        ));
    }

    #[test]
    fn every_scan_type_round_trips_through_its_sdk_code() {
        for scan in crate::ScanType::ALL {
            assert_eq!(crate::ScanType::from_sdk(scan.to_sdk()), Some(scan));
        }
        assert_eq!(crate::ScanType::from_sdk(u32::MAX), None);
    }

    #[test]
    fn media_types_combine_as_a_set() {
        let singles = [MediaTypes::VIDEO, MediaTypes::AUDIO, MediaTypes::METADATA];
        for (index, single) in singles.into_iter().enumerate() {
            assert!(!single.is_empty());
            assert!(MediaTypes::ALL.contains(single));
            assert!(!MediaTypes::NONE.contains(single));
            for (other_index, other) in singles.into_iter().enumerate() {
                assert_eq!(single.contains(other), index == other_index);
            }
        }
        let mut all = MediaTypes::NONE;
        for single in singles {
            all |= single;
        }
        assert_eq!(all, MediaTypes::ALL);
        assert_eq!(
            MediaTypes::VIDEO | MediaTypes::AUDIO | MediaTypes::METADATA,
            MediaTypes::ALL
        );
        assert!(MediaTypes::NONE.is_empty());
        assert!(MediaTypes::ALL.contains(MediaTypes::NONE));
        assert!(!(MediaTypes::VIDEO | MediaTypes::AUDIO).contains(MediaTypes::ALL));
    }
}
