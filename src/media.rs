//! Media values shared by frames, packets, receivers and recorded files.

use std::{
    hash::{Hash, Hasher},
    ops::{BitOr, BitOrAssign},
};

use crate::{Error, PixelFormat, Result};

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
    // Both in 1..=i32::MAX, the range of the SDK's signed fields.
    numerator: u32,
    denominator: u32,
}

impl PartialEq for FrameRate {
    fn eq(&self, other: &Self) -> bool {
        // Each component is at most i32::MAX, so either product fits in u64.
        u64::from(self.numerator) * u64::from(other.denominator)
            == u64::from(other.numerator) * u64::from(self.denominator)
    }
}

impl Hash for FrameRate {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let (mut a, mut b) = (self.numerator, self.denominator);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        (self.numerator / a, self.denominator / a).hash(state);
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
        Self::checked(numerator.into(), denominator.into()).map_err(Error::InvalidInput)
    }

    /// `frames` frames per second, for the builders' defaults. Call it in a
    /// `const` block, where a value outside `1..=i32::MAX` fails to compile.
    pub(crate) const fn per_second(frames: u32) -> Self {
        assert!(frames >= 1 && frames <= i32::MAX.unsigned_abs());
        Self {
            numerator: frames,
            denominator: 1,
        }
    }

    /// The frame rate of a frame the SDK delivered; an invalid one makes the
    /// frame invalid.
    pub(crate) fn from_sdk(numerator: i32, denominator: i32) -> Result<Self> {
        Self::checked(numerator.into(), denominator.into()).map_err(Error::InvalidFrame)
    }

    /// The one frame-rate check, for values from the API and from the SDK.
    /// The error is the reason, for the caller to report as its own variant.
    fn checked(numerator: i64, denominator: i64) -> std::result::Result<Self, String> {
        let part = |value: i64| {
            u32::try_from(value)
                .ok()
                .filter(|value| (1..=i32::MAX.unsigned_abs()).contains(value))
        };
        match (part(numerator), part(denominator)) {
            (Some(numerator), Some(denominator)) => Ok(Self {
                numerator,
                denominator,
            }),
            _ => Err(format!(
                "Invalid frame rate {numerator}/{denominator}: both parts must be in 1..={}",
                i32::MAX
            )),
        }
    }

    /// The number of frames in [`denominator`](Self::denominator) seconds.
    #[must_use]
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    /// The number of seconds that [`numerator`](Self::numerator) frames take.
    #[must_use]
    pub const fn denominator(self) -> u32 {
        self.denominator
    }

    /// The SDK's `frame_rate_N` and `frame_rate_D` fields.
    pub(crate) const fn to_sdk(self) -> (i32, i32) {
        // Lossless: construction keeps both parts at or below i32::MAX.
        (self.numerator as i32, self.denominator as i32)
    }
}

/// A four-character code: the SDK's identifier for a pixel format or codec.
///
/// It holds any value, including one this crate does not know, so a value
/// read from the SDK is never lost. [`PixelFormat`] converts to and from it.
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

impl TryFrom<FourCc> for PixelFormat {
    type Error = Error;

    /// The pixel format `fourcc` names.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when it names no uncompressed pixel format
    /// this crate supports.
    fn try_from(fourcc: FourCc) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|&format| FourCc::from(format) == fourcc)
            .ok_or_else(|| {
                Error::InvalidFrame(format!("Unknown pixel format FourCC: 0x{:08X}", fourcc.0))
            })
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
                Err(Error::InvalidFrame(message)) if message.contains(&format!("{numerator}/{denominator}"))
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
            Err(Error::InvalidFrame(message)) if message.contains(&format!("{:08X}", v210.raw()))
        ));
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
