//! Compressed frames and the packet envelope that frames H.264, HEVC and AAC
//! payloads on the wire.

use std::{ffi::c_char, num::NonZeroU32};

use super::{descriptors, invalid_frame};

#[path = "packets/payload.rs"]
mod payload;
use crate::media::SdkQuantity;
use crate::{
    frames::{FrameMetadataRef, OwnedFrameMetadata, VideoHeader},
    sys::bindings::*,
    Error, FourCc, FrameRate, PictureAspectRatio, Rejected, Result, ScanType, SdkTimestamp,
    StreamTier, Timecode,
};
pub(crate) use payload::{PacketBytes, PacketSlice};

pub use crate::media::MAX_PACKET_BYTES;

const_assert!(MAX_PACKET_BYTES <= i32::MAX as usize);

/// A nonempty packet extent, retained until its SDK descriptor is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub(crate) struct PacketLen(NonZeroU32);

impl PacketLen {
    pub(crate) fn new(bytes: usize) -> Option<Self> {
        if !(1..=MAX_PACKET_BYTES).contains(&bytes) {
            return None;
        }
        NonZeroU32::new(u32::try_from(bytes).ok()?).map(Self)
    }

    pub(crate) fn checked_add(self, other: Self) -> Option<Self> {
        self.to_usize()
            .checked_add(other.to_usize())
            .and_then(Self::new)
    }

    pub(crate) fn from_sdk(bytes: i32) -> Option<Self> {
        usize::try_from(bytes).ok().and_then(Self::new)
    }

    pub(crate) fn to_usize(self) -> usize {
        self.0.get() as usize
    }

    pub(crate) fn to_sdk(self) -> i32 {
        // Construction and the const assertion prove this conversion lossless.
        self.0.get().cast_signed()
    }

    fn to_u32(self) -> u32 {
        self.0.get()
    }
}

/// Size in bytes of the version-0 envelope header.
pub(crate) const ENVELOPE_HEADER_BYTES: usize = 44;

/// Set in every byte of the FourCC of a protected stream.
pub(crate) const PROTECTED: u32 = 0x8080_8080;

/// The envelope flag that marks a keyframe.
const KEYFRAME: u32 = 1;

/// Size in bytes of the header that begins a SpeedHQ frame.
const SPEEDHQ_HEADER_BYTES: usize = 4;

/// One more than the largest quality the first byte of a SpeedHQ frame states.
const SPEEDHQ_QUALITY_LIMIT: u8 = 100;

/// The largest width and height of an H.264 or HEVC picture on the preview
/// stream.
const MAX_PREVIEW_DIMENSION: u32 = 640;

// Byte offsets of the envelope header fields.
const VERSION_OFFSET: usize = 0;
const CODEC_OFFSET: usize = 4;
const PTS_OFFSET: usize = 8;
const DTS_OFFSET: usize = 16;
const RESERVED_OFFSET: usize = 24;
const FLAGS_OFFSET: usize = 32;
const DATA_SIZE_OFFSET: usize = 36;
const EXTRA_DATA_SIZE_OFFSET: usize = 40;

const fn fourcc(bytes: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*bytes)
}

/// `N` bytes of `bytes` starting at `offset`. The caller has checked the length.
fn field<const N: usize>(bytes: &[u8], offset: usize) -> [u8; N] {
    let mut value = [0; N];
    value.copy_from_slice(&bytes[offset..offset + N]);
    value
}

/// A compressed video codec.
///
/// The codec, the [`StreamTier`] and stream protection together select the
/// FourCC a frame is sent with.
///
/// # Delivery
///
/// The SDK takes any compressed video frame and silently discards the ones it
/// does not accept, so this module refuses to build or send those:
///
/// * A SpeedHQ frame begins with its quality, a byte below 100.
/// * An H.264 or HEVC picture on the preview stream is at most 640 pixels
///   wide and 640 high.
///
/// Which frames reach a receiver also depends on the order they are sent in;
/// see [`CompressedSender`](super::CompressedSender).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VideoCodec {
    /// SpeedHQ 4:2:0.
    SpeedHq420,
    /// SpeedHQ 4:2:2.
    SpeedHq422,
    /// SpeedHQ 4:2:2 with alpha.
    SpeedHq422Alpha,
    /// H.264 in a packet envelope.
    H264,
    /// HEVC in a packet envelope.
    Hevc,
    /// H.264 with alpha in a packet envelope.
    H264Alpha,
    /// HEVC with alpha in a packet envelope.
    HevcAlpha,
}

crate::media::decoded_variants!(VideoCodec:
    SpeedHq420, SpeedHq422, SpeedHq422Alpha, H264, Hevc, H264Alpha, HevcAlpha,
);

impl VideoCodec {
    /// The FourCC a frame of this codec is sent with.
    pub(crate) const fn outer(self, tier: StreamTier, protected: bool) -> u32 {
        let bytes = match (self, tier) {
            (Self::SpeedHq420, StreamTier::Program) => b"SHQ0",
            (Self::SpeedHq420, StreamTier::Preview) => b"shq0",
            (Self::SpeedHq422, StreamTier::Program) => b"SHQ2",
            (Self::SpeedHq422, StreamTier::Preview) => b"shq2",
            (Self::SpeedHq422Alpha, StreamTier::Program) => b"SHQ7",
            (Self::SpeedHq422Alpha, StreamTier::Preview) => b"shq7",
            (Self::H264, StreamTier::Program) => b"H264",
            (Self::H264, StreamTier::Preview) => b"h264",
            (Self::Hevc, StreamTier::Program) => b"HEVC",
            (Self::Hevc, StreamTier::Preview) => b"hevc",
            (Self::H264Alpha, StreamTier::Program) => b"A264",
            (Self::H264Alpha, StreamTier::Preview) => b"a264",
            (Self::HevcAlpha, StreamTier::Program) => b"AEVC",
            (Self::HevcAlpha, StreamTier::Preview) => b"aevc",
        };
        fourcc(bytes) | if protected { PROTECTED } else { 0 }
    }

    /// The codec, tier and protection a frame FourCC names, if it is compressed
    /// video this module knows.
    pub(crate) fn from_outer(value: u32) -> Option<(Self, StreamTier, bool)> {
        let protected = value & PROTECTED == PROTECTED;
        Self::ALL.into_iter().find_map(|codec| {
            [StreamTier::Program, StreamTier::Preview]
                .into_iter()
                .find(|tier| value == codec.outer(*tier, protected))
                .map(|tier| (codec, tier, protected))
        })
    }

    /// The envelope codec that frames this codec's payload. SpeedHQ frames
    /// have no envelope.
    pub fn envelope_codec(self) -> Option<EnvelopeCodec> {
        match self {
            Self::H264 | Self::H264Alpha => Some(EnvelopeCodec::H264),
            Self::Hevc | Self::HevcAlpha => Some(EnvelopeCodec::Hevc),
            Self::SpeedHq420 | Self::SpeedHq422 | Self::SpeedHq422Alpha => None,
        }
    }
}

/// A codec whose payload travels inside a packet envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EnvelopeCodec {
    /// H.264 video.
    H264,
    /// HEVC video.
    Hevc,
    /// AAC audio.
    Aac,
}

crate::media::decoded_variants!(EnvelopeCodec: H264, Hevc, Aac);

impl EnvelopeCodec {
    const fn raw_fourcc(self) -> u32 {
        match self {
            Self::H264 => fourcc(b"H264"),
            Self::Hevc => fourcc(b"HEVC"),
            Self::Aac => 0xff,
        }
    }
}

impl From<EnvelopeCodec> for FourCc {
    /// The FourCC carried by the codec's envelope header.
    fn from(codec: EnvelopeCodec) -> Self {
        Self::from_raw(codec.raw_fourcc())
    }
}

impl TryFrom<FourCc> for EnvelopeCodec {
    type Error = Error;

    /// Decode a packet-envelope codec from its FourCC.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when the FourCC names no supported envelope codec.
    fn try_from(fourcc: FourCc) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|&codec| FourCc::from(codec) == fourcc)
            .ok_or_else(|| {
                invalid_frame(crate::FrameDefect::UnknownEnvelopeCodec {
                    fourcc: fourcc.raw(),
                })
            })
    }
}

/// The extents established when an envelope is admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EnvelopeExtents {
    size: usize,
    data_len: usize,
    extra_len: usize,
}

impl EnvelopeExtents {
    fn checked_end(self) -> Option<usize> {
        self.size
            .checked_add(self.data_len)?
            .checked_add(self.extra_len)
    }

    fn check_extent(self, total: usize) -> Result<usize> {
        let end = self
            .checked_end()
            .ok_or_else(|| invalid_frame(crate::FrameDefect::PacketLengthOverflow))?;
        if end > total {
            return Err(invalid_frame(crate::FrameDefect::PacketTruncated));
        }
        Ok(end)
    }

    fn extra_offset(self) -> usize {
        self.size + self.data_len
    }
}

/// The fields that decide whether an envelope is admissible.
struct EnvelopeHeader {
    extents: EnvelopeExtents,
    codec: FourCc,
    flags: u32,
}

impl EnvelopeHeader {
    /// Read the header at the start of `bytes`.
    fn read(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < ENVELOPE_HEADER_BYTES {
            return Err(invalid_frame(crate::FrameDefect::PacketHeaderTruncated));
        }
        let size = usize::try_from(i32::from_ne_bytes(field(bytes, VERSION_OFFSET)))
            .ok()
            .filter(|size| *size >= ENVELOPE_HEADER_BYTES)
            .ok_or_else(|| invalid_frame(crate::FrameDefect::PacketHeaderSize))?;
        Ok(Self {
            extents: EnvelopeExtents {
                size,
                data_len: u32::from_ne_bytes(field(bytes, DATA_SIZE_OFFSET)) as usize,
                extra_len: u32::from_ne_bytes(field(bytes, EXTRA_DATA_SIZE_OFFSET)) as usize,
            },
            codec: FourCc::from_raw(u32::from_ne_bytes(field(bytes, CODEC_OFFSET))),
            flags: u32::from_ne_bytes(field(bytes, FLAGS_OFFSET)),
        })
    }

    fn is_keyframe(&self) -> bool {
        self.flags & KEYFRAME != 0
    }

    /// Check the codec, payload and declared lengths once for every container.
    /// Sending additionally requires the current header version, known flags
    /// and lengths that account for every byte. Receiving retains extensions
    /// and trailing bytes without loss.
    ///
    /// The reserved field takes no part. It belongs to the SDK, which
    /// overwrites it in every packet it sends, so a packet that has been sent
    /// or received carries whatever the SDK left there.
    fn validate(
        &self,
        codec: EnvelopeCodec,
        total: usize,
        purpose: Use,
    ) -> Result<EnvelopeExtents> {
        if purpose == Use::Send
            && (self.extents.size != ENVELOPE_HEADER_BYTES || self.flags & !KEYFRAME != 0)
        {
            return Err(invalid_frame(crate::FrameDefect::PacketHeaderUnsupported));
        }
        if self.codec != FourCc::from(codec) {
            return Err(invalid_frame(crate::FrameDefect::PacketCodecMismatch));
        }
        if self.extents.data_len == 0 {
            return Err(invalid_frame(crate::FrameDefect::EmptyCodecPayload));
        }
        let declared = self.extents.check_extent(total)?;
        if purpose == Use::Send && declared < total {
            return Err(invalid_frame(crate::FrameDefect::PacketLengthsMismatch));
        }
        Ok(self.extents)
    }

    /// Check the codec configuration of a video packet that is about to be
    /// sent: a decoder cannot start on a keyframe without its parameter sets.
    pub(crate) fn validate_video_configuration(&self) -> Result<()> {
        if self.is_keyframe() && self.extents.extra_len == 0 {
            return Err(invalid_frame(
                crate::FrameDefect::KeyframeConfigurationMissing,
            ));
        }
        Ok(())
    }

    /// Check what the SDK needs of an AAC packet to deliver it as audio with
    /// `parameters`. `config` is the first two bytes of the packet's codec
    /// configuration, when it has that many.
    ///
    /// The SDK reads the sample rate and the channel layout from those bytes,
    /// where an AudioSpecificConfig keeps them, and discards a packet whose
    /// frame says otherwise.
    pub(crate) fn validate_aac(
        &self,
        config: Option<[u8; 2]>,
        parameters: AudioPacketFormat,
    ) -> Result<()> {
        if !self.is_keyframe() {
            return Err(invalid_frame(crate::FrameDefect::AacKeyframeMissing));
        }
        let Some(config) = config else {
            return Err(invalid_frame(crate::FrameDefect::AacConfigurationMissing));
        };
        // Five bits of object type, four of sampling-frequency index, four of
        // channel configuration.
        let bits = u16::from_be_bytes(config);
        let frequency_index = usize::from((bits >> 7) & 0xf);
        let channel_configuration = usize::from((bits >> 3) & 0xf);
        if AAC_SAMPLE_RATES.get(frequency_index) != Some(&parameters.sample_rate()) {
            return Err(invalid_frame(crate::FrameDefect::AacRateMismatch));
        }
        if channel_configuration
            .checked_sub(1)
            .and_then(|index| AAC_CHANNELS.get(index))
            != Some(&parameters.num_channels())
        {
            return Err(invalid_frame(crate::FrameDefect::AacChannelsMismatch));
        }
        Ok(())
    }
}

/// A bounds-checked view of a packet envelope: its header, codec payload and
/// codec configuration data. [`encode`](Self::encode) builds one.
///
/// Parsing keeps what it does not understand. Flags, the reserved field and
/// header bytes added by a later envelope version are all readable, so a
/// received packet can be inspected or stored without loss.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PacketEnvelope<'a> {
    bytes: &'a [u8],
    extents: EnvelopeExtents,
}

impl std::fmt::Debug for PacketEnvelope<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PacketEnvelope")
            .field("codec", &self.codec())
            .field("fourcc", &self.fourcc())
            .field("presentation_time", &self.presentation_time())
            .field("decode_time", &self.decode_time())
            .field("keyframe", &self.is_keyframe())
            .field("flags", &self.flags())
            .field("known_version", &self.is_known_version())
            .field("header_extension_bytes", &self.header_extension().len())
            .field("payload_bytes", &self.data().len())
            .field("configuration_bytes", &self.extra_data().len())
            .field("trailing_bytes", &self.trailing_bytes().len())
            .finish()
    }
}

impl<'a> PacketEnvelope<'a> {
    /// View bytes with the extents their containing packet validated.
    fn from_validated(bytes: &'a [u8], extents: EnvelopeExtents) -> Self {
        Self { bytes, extents }
    }

    /// Parse `bytes` as one packet envelope.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when the slice is shorter than a header, longer
    /// than [`MAX_PACKET_BYTES`], or shorter than the lengths its header
    /// declares.
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        PacketLen::new(bytes.len()).ok_or_else(|| {
            invalid_frame(if bytes.is_empty() {
                crate::FrameDefect::PacketHeaderTruncated
            } else {
                crate::FrameDefect::PacketLength
            })
        })?;
        let extents = EnvelopeHeader::read(bytes)?.extents;
        extents.check_extent(bytes.len())?;
        Ok(Self::from_validated(bytes, extents))
    }

    /// Build a version-0 packet envelope around one codec payload.
    ///
    /// `presentation_time` and `decode_time` are in the stream's own units.
    /// `keyframe` marks a video keyframe; AAC has no other kind of packet, so
    /// an AAC envelope carries the flag whatever `keyframe` says. `extra_data`
    /// is the codec configuration (parameter sets for H.264 and HEVC, the
    /// AudioSpecificConfig for AAC); video keyframes and every AAC packet need
    /// it to be sendable.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when `data` is empty or the packet would exceed
    /// [`MAX_PACKET_BYTES`].
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::advanced::{EnvelopeCodec, PacketEnvelope};
    ///
    /// let packet = PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, true, &[0x65, 0x88], &[0x67, 0x42])?;
    /// let view = PacketEnvelope::parse(&packet)?;
    /// assert!(view.is_keyframe());
    /// assert_eq!(view.data(), [0x65, 0x88]);
    /// assert_eq!(view.extra_data(), [0x67, 0x42]);
    /// # Ok::<(), grafton_ndi::Error>(())
    /// ```
    pub fn encode(
        codec: EnvelopeCodec,
        presentation_time: i64,
        decode_time: i64,
        keyframe: bool,
        data: &[u8],
        extra_data: &[u8],
    ) -> Result<Vec<u8>> {
        let size = ENVELOPE_HEADER_BYTES
            .checked_add(data.len())
            .and_then(|n| n.checked_add(extra_data.len()))
            .ok_or_else(|| invalid_frame(crate::FrameDefect::PacketSizeOverflow))?;
        let size = PacketLen::new(size)
            .ok_or_else(|| invalid_frame(crate::FrameDefect::PacketPayloadSize))?;
        let data_len = PacketLen::new(data.len())
            .ok_or_else(|| invalid_frame(crate::FrameDefect::PacketPayloadSize))?;
        let extra_len = if extra_data.is_empty() {
            None
        } else {
            Some(
                PacketLen::new(extra_data.len())
                    .ok_or_else(|| invalid_frame(crate::FrameDefect::PacketPayloadSize))?,
            )
        };
        let mut bytes = vec![0; size.to_usize()];
        let mut put = |offset: usize, value: &[u8]| {
            bytes[offset..offset + value.len()].copy_from_slice(value);
        };
        put(
            VERSION_OFFSET,
            &(ENVELOPE_HEADER_BYTES as i32).to_ne_bytes(),
        );
        put(CODEC_OFFSET, &FourCc::from(codec).raw().to_ne_bytes());
        put(PTS_OFFSET, &presentation_time.to_ne_bytes());
        put(DTS_OFFSET, &decode_time.to_ne_bytes());
        // The SDK delivers no AAC packet without the flag.
        let keyframe = keyframe || codec == EnvelopeCodec::Aac;
        put(
            FLAGS_OFFSET,
            &(if keyframe { KEYFRAME } else { 0 }).to_ne_bytes(),
        );
        put(DATA_SIZE_OFFSET, &data_len.to_u32().to_ne_bytes());
        put(
            EXTRA_DATA_SIZE_OFFSET,
            &extra_len.map_or(0, PacketLen::to_u32).to_ne_bytes(),
        );
        put(ENVELOPE_HEADER_BYTES, data);
        put(ENVELOPE_HEADER_BYTES + data.len(), extra_data);
        Ok(bytes)
    }

    /// The codec, when the header names one this module knows.
    pub fn codec(self) -> Option<EnvelopeCodec> {
        EnvelopeCodec::try_from(self.fourcc()).ok()
    }

    /// The codec's FourCC exactly as the header carries it.
    pub fn fourcc(self) -> FourCc {
        FourCc::from_raw(u32::from_ne_bytes(field(self.bytes, CODEC_OFFSET)))
    }

    /// Presentation timestamp, in the stream's own units.
    pub fn presentation_time(self) -> i64 {
        i64::from_ne_bytes(field(self.bytes, PTS_OFFSET))
    }

    /// Decode timestamp, in the stream's own units.
    pub fn decode_time(self) -> i64 {
        i64::from_ne_bytes(field(self.bytes, DTS_OFFSET))
    }

    /// Whether the keyframe flag is set.
    pub fn is_keyframe(self) -> bool {
        self.flags() & KEYFRAME != 0
    }

    /// The header's flags field, every bit as the packet carries it,
    /// including bits this module does not interpret.
    /// [`is_keyframe`](Self::is_keyframe) reads the one it does.
    pub fn flags(self) -> u32 {
        u32::from_ne_bytes(field(self.bytes, FLAGS_OFFSET))
    }

    /// The header's reserved field.
    ///
    /// The field belongs to the SDK, which overwrites it in every packet it
    /// sends. Its value says nothing about whether a packet can be sent.
    pub fn reserved(self) -> u64 {
        u64::from_ne_bytes(field(self.bytes, RESERVED_OFFSET))
    }

    /// Header bytes beyond the version-0 fields. Empty for version 0.
    pub fn header_extension(self) -> &'a [u8] {
        &self.bytes[ENVELOPE_HEADER_BYTES..self.extents.size]
    }

    /// The codec payload.
    pub fn data(self) -> &'a [u8] {
        let start = self.extents.size;
        &self.bytes[start..start + self.extents.data_len]
    }

    /// Codec configuration data (parameter sets, or the AAC configuration),
    /// which follows the payload. Empty when the packet carries none.
    pub fn extra_data(self) -> &'a [u8] {
        let start = self.extents.extra_offset();
        &self.bytes[start..start + self.extents.extra_len]
    }

    /// Bytes after the lengths the header declares.
    pub fn trailing_bytes(self) -> &'a [u8] {
        &self.bytes[self.extents.extra_offset() + self.extents.extra_len..]
    }

    /// Whether the header is exactly version 0, the version this module writes.
    pub fn is_known_version(self) -> bool {
        self.extents.size == ENVELOPE_HEADER_BYTES
    }
}

/// The codec, geometry and timing of compressed video, independent of any
/// one frame's bytes.
///
/// # Examples
///
/// ```
/// use grafton_ndi::{
///     advanced::{VideoCodec, VideoPacketFormat},
///     FrameRate, StreamTier,
/// };
///
/// let format = VideoPacketFormat::new(VideoCodec::Hevc, 1920, 1080)?
///     .with_frame_rate(FrameRate::new(60_000, 1001)?)
///     .with_tier(StreamTier::Program);
/// assert_eq!(format.frame_rate(), FrameRate::new(60_000, 1001)?);
/// assert!(VideoPacketFormat::new(VideoCodec::Hevc, 0, 1080).is_err());
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VideoPacketFormat {
    codec: VideoCodec,
    tier: StreamTier,
    protected: bool,
    width: SdkQuantity,
    height: SdkQuantity,
    frame_rate: FrameRate,
    picture_aspect_ratio: PictureAspectRatio,
    scan_type: ScanType,
}

impl VideoPacketFormat {
    /// A progressive program-tier format at 60 frames per second with square
    /// pixels. Adjust it with the `with_*` methods.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] unless both dimensions are in `1..=i32::MAX`,
    /// the range the SDK can carry.
    pub fn new(codec: VideoCodec, width: u32, height: u32) -> Result<Self> {
        Self::checked(codec, width.into(), height.into())
            .map_err(|cause| Error::InvalidInput(crate::InputDefect::Layout(cause)))
    }

    /// The one dimension check, for values from the API and from the SDK.
    /// The error is the reason, for the caller to report as its own variant.
    fn checked(codec: VideoCodec, width: i64, height: i64) -> crate::frames::LayoutResult<Self> {
        match (SdkQuantity::new(width), SdkQuantity::new(height)) {
            (Some(width), Some(height)) => Ok(Self {
                codec,
                tier: StreamTier::Program,
                protected: false,
                width,
                height,
                frame_rate: crate::media::DEFAULT_VIDEO_FRAME_RATE,
                picture_aspect_ratio: PictureAspectRatio::SQUARE_PIXELS,
                scan_type: ScanType::Progressive,
            }),
            _ => Err(crate::LayoutDefect::CompressedVideoDimensions {
                detail: crate::CompactDiagnostic::new(crate::VideoDimensionsDefect {
                    width,
                    height,
                }),
            }),
        }
    }

    /// The format of a compressed frame the SDK delivered; a value out of
    /// range makes the frame invalid.
    pub(crate) fn from_sdk(raw: &NDIlib_video_frame_v2_t) -> Result<Self> {
        let (codec, tier, protected) = VideoCodec::from_outer(raw.FourCC)
            .ok_or_else(|| invalid_frame(crate::FrameDefect::UnknownCompressedVideo))?;
        let format = Self::checked(codec, raw.xres.into(), raw.yres.into()).map_err(Error::from)?;
        let header = VideoHeader::from_sdk(raw)?;
        Ok(Self {
            tier,
            protected,
            frame_rate: header.frame_rate(),
            picture_aspect_ratio: header.picture_aspect_ratio(),
            scan_type: header.scan_type(),
            ..format
        })
    }

    /// The same format at `frame_rate`.
    #[must_use]
    pub fn with_frame_rate(self, frame_rate: FrameRate) -> Self {
        Self { frame_rate, ..self }
    }

    /// The same format with the validated displayed picture aspect ratio.
    /// [`PictureAspectRatio::SQUARE_PIXELS`] asks the SDK to derive it from
    /// the pixel dimensions.
    #[must_use]
    pub fn with_picture_aspect_ratio(self, ratio: PictureAspectRatio) -> Self {
        Self {
            picture_aspect_ratio: ratio,
            ..self
        }
    }

    /// The same format on the program or preview stream.
    #[must_use]
    pub fn with_tier(self, tier: StreamTier) -> Self {
        Self { tier, ..self }
    }

    /// The same format with scan type `scan_type`.
    #[must_use]
    pub fn with_scan_type(self, scan_type: ScanType) -> Self {
        Self { scan_type, ..self }
    }

    /// The same format, marked as a protected stream or not.
    #[must_use]
    pub fn with_protected(self, protected: bool) -> Self {
        Self { protected, ..self }
    }

    /// The codec.
    pub fn codec(self) -> VideoCodec {
        self.codec
    }

    /// Width in pixels.
    pub fn width(self) -> u32 {
        self.width.get()
    }

    /// Height in pixels.
    pub fn height(self) -> u32 {
        self.height.get()
    }

    /// The frame rate.
    pub fn frame_rate(self) -> FrameRate {
        self.frame_rate
    }

    /// The picture aspect ratio; `0.0` means square pixels.
    pub fn picture_aspect_ratio(self) -> PictureAspectRatio {
        self.picture_aspect_ratio
    }

    /// The stream this format belongs to.
    pub fn tier(self) -> StreamTier {
        self.tier
    }

    /// The scan type.
    pub fn scan_type(self) -> ScanType {
        self.scan_type
    }

    /// Whether the stream is protected.
    pub fn is_protected(self) -> bool {
        self.protected
    }

    /// The number of bytes at the start of a frame of this format that hold
    /// its header.
    pub(crate) fn header_bytes(self) -> usize {
        match self.codec.envelope_codec() {
            Some(_) => ENVELOPE_HEADER_BYTES,
            None => SPEEDHQ_HEADER_BYTES,
        }
    }

    /// Check that a frame of this format, `total` bytes long and beginning
    /// with `header`, is one this module can send and the SDK delivers; see
    /// [`VideoCodec`]. `header` holds [`header_bytes`](Self::header_bytes)
    /// bytes or more.
    pub(crate) fn validate_for_send(
        self,
        header: &[u8],
        total: usize,
    ) -> Result<Option<EnvelopeExtents>> {
        let Some(codec) = self.codec.envelope_codec() else {
            if header
                .first()
                .is_none_or(|quality| *quality >= SPEEDHQ_QUALITY_LIMIT)
            {
                return Err(invalid_frame(crate::FrameDefect::SpeedHqQuality));
            }
            return Ok(None);
        };
        let header = EnvelopeHeader::read(header)?;
        let extents = header.validate(codec, total, Use::Send)?;
        header.validate_video_configuration()?;
        if self.tier == StreamTier::Preview
            && (self.width.get() > MAX_PREVIEW_DIMENSION
                || self.height.get() > MAX_PREVIEW_DIMENSION)
        {
            return Err(invalid_frame(crate::FrameDefect::PreviewVideoDimensions));
        }
        Ok(Some(extents))
    }

    /// A native frame descriptor with this format and no data, no timecode
    /// and no metadata.
    pub(crate) fn raw(self) -> NDIlib_video_frame_v2_t {
        let (numerator, denominator) = self.frame_rate.to_sdk();
        NDIlib_video_frame_v2_t {
            xres: self.width.to_sdk(),
            yres: self.height.to_sdk(),
            FourCC: self.codec.outer(self.tier, self.protected),
            frame_rate_N: numerator,
            frame_rate_D: denominator,
            picture_aspect_ratio: self.picture_aspect_ratio.to_sdk(),
            frame_format_type: self.scan_type.to_sdk(),
            ..descriptors::video_frame()
        }
    }

    /// A native descriptor of a frame with this format and `extras`, without
    /// data.
    pub(crate) fn descriptor(self, extras: &FrameExtras) -> NDIlib_video_frame_v2_t {
        let (timecode, timestamp, p_metadata) = extras.to_sdk();
        NDIlib_video_frame_v2_t {
            timecode,
            timestamp,
            p_metadata,
            ..self.raw()
        }
    }
}

/// The timecode, timestamp and metadata every compressed and scatter frame
/// carries, with the single checked way to set the metadata.
///
/// The default is what a frame built for sending carries: a timecode for the
/// SDK to synthesize, no timestamp and no metadata.
#[derive(Clone, Debug, Default)]
pub(crate) struct FrameExtras {
    timecode: Timecode,
    timestamp: Option<SdkTimestamp>,
    metadata: OwnedFrameMetadata,
}

impl FrameExtras {
    /// The values the SDK delivered with a received frame.
    ///
    /// # Safety
    ///
    /// `metadata` is null or readable through its first NUL or the metadata cap.
    #[cfg(test)]
    unsafe fn received(timecode: i64, timestamp: i64, metadata: *const c_char) -> Result<Self> {
        let descriptor = NDIlib_video_frame_v2_t {
            p_metadata: metadata,
            ..Default::default()
        };
        // SAFETY: only metadata is inspected; the caller retains its bounded
        // readable text while this synthetic descriptor is borrowed.
        let frame =
            unsafe { crate::frames::ValidatedFrame::validate_descriptor(&descriptor, |_| Ok(()))? };
        let mut extras = Self::default();
        extras.copy_received(
            Timecode::from_ticks(timecode),
            SdkTimestamp::from_sdk(timestamp),
            frame.frame_metadata(),
        );
        Ok(extras)
    }

    /// Reuse storage while copying the borrowed frame's one metadata value.
    fn copy_received(
        &mut self,
        timecode: Timecode,
        timestamp: Option<SdkTimestamp>,
        metadata: FrameMetadataRef<'_>,
    ) {
        self.metadata.copy_from(metadata);
        self.timecode = timecode;
        self.timestamp = timestamp;
    }

    /// The timecode.
    pub(crate) fn timecode(&self) -> Timecode {
        self.timecode
    }

    /// The timestamp of a received frame.
    pub(crate) fn timestamp(&self) -> Option<SdkTimestamp> {
        self.timestamp
    }

    /// The metadata text, if any.
    pub(crate) fn metadata(&self) -> Option<&str> {
        self.metadata
            .view()
            .text()
            .map(crate::frames::MetadataText::as_str)
    }

    pub(crate) fn metadata_status(&self) -> crate::FrameMetadataStatus {
        self.metadata.view().status()
    }

    pub(crate) fn set_timecode(&mut self, timecode: Timecode) {
        self.timecode = timecode;
    }

    pub(crate) fn set_metadata(
        &mut self,
        metadata: impl Into<String>,
    ) -> std::result::Result<(), Rejected<String>> {
        self.metadata.set(metadata)
    }

    pub(crate) fn clear_metadata(&mut self) {
        // Explicit packet clearing releases retained metadata storage as before;
        // copying an absent observation instead preserves it for later frames.
        self.metadata = OwnedFrameMetadata::default();
    }

    /// Bytes the metadata keeps allocated, including terminator and spare capacity.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.metadata.retained_bytes()
    }

    /// The timecode, timestamp and metadata fields of a native descriptor.
    /// The metadata pointer is null without metadata, and valid for as long
    /// as these extras are.
    fn to_sdk(&self) -> (i64, i64, *const c_char) {
        (
            self.timecode.ticks(),
            SdkTimestamp::to_sdk(self.timestamp),
            self.metadata.view().as_ptr(),
        )
    }
}

/// Accessors and editing operations shared by owned compressed and scatter frames.
macro_rules! frame_extras_accessors {
    () => {
        /// The timecode: [`crate::Timecode::SYNTHESIZE`] on a frame built for
        /// sending without an explicit timecode.
        pub fn timecode(&self) -> $crate::Timecode {
            self.extras.timecode()
        }

        /// Replace the frame's timecode.
        pub fn set_timecode(&mut self, timecode: $crate::Timecode) {
            self.extras.set_timecode(timecode);
        }

        /// The per-frame metadata, if any. Unreadable received text is omitted.
        pub fn metadata(&self) -> Option<&str> {
            self.extras.metadata()
        }

        /// Cached metadata presence or the reason unreadable text was withheld.
        pub fn metadata_status(&self) -> $crate::FrameMetadataStatus {
            self.extras.metadata_status()
        }

        /// Replace frame metadata. Use [`Self::clear_metadata`] to remove it.
        ///
        /// Invalid replacement text leaves the existing metadata unchanged.
        ///
        /// # Errors
        ///
        /// Returns the unchanged text in [`crate::Rejected`], with
        /// [`crate::Error::InvalidCString`] for an interior NUL, or
        /// [`crate::Error::InvalidFrame`] when the emitted C string would
        /// exceed the metadata size cap. `NulError` retains only the prefix
        /// through the first NUL; [`crate::Rejected::into_inner`] returns all text.
        pub fn set_metadata(
            &mut self,
            metadata: impl Into<String>,
        ) -> std::result::Result<(), $crate::Rejected<String>> {
            self.extras.set_metadata(metadata)
        }

        /// Remove the frame metadata.
        pub fn clear_metadata(&mut self) {
            self.extras.clear_metadata();
        }
    };
}
pub(crate) use frame_extras_accessors;

/// One field list for each compressed media kind, shared by borrowed, owned
/// and scatter packets. Storage is always summarized by extent, never contents.
macro_rules! packet_debug {
    ($type:ty, $name:literal, $this:ident, $kind:ident, $timestamp:expr;
        $($field:literal = $value:expr),+ $(,)?) => {
        impl std::fmt::Debug for $type {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let $this = self;
                let mut summary = f.debug_struct($name);
                packet_debug!(@$kind summary, $this);
                $(summary.field($field, &$value);)+
                summary
                    .field("timecode", &$this.timecode())
                    .field("timestamp", &$timestamp)
                    .field("metadata_status", &$this.metadata_status())
                    .finish()
            }
        }
    };
    (@video $summary:ident, $this:ident) => {
        $summary.field("format", &$this.format());
    };
    (@audio $summary:ident, $this:ident) => {
        $summary
            .field("codec", &$this.format().codec())
            .field("protected", &$this.format().is_protected())
            .field("sample_rate", &$this.format().sample_rate())
            .field("num_channels", &$this.format().num_channels())
            .field("num_samples", &$this.format().num_samples());
    };
}
pub(crate) use packet_debug;

macro_rules! envelope_accessor {
    ($($extents:tt)+) => {
        /// The validated envelope for H.264, HEVC or AAC. Other codecs have no envelope.
        pub fn envelope(&self) -> Option<PacketEnvelope<'_>> {
            self.$($extents)+
                .map(|extents| PacketEnvelope::from_validated(self.data(), extents))
        }
    };
}

/// One owned compressed video frame: a [`VideoPacketFormat`] and the codec
/// bytes, with optional metadata.
///
/// Construction checks the memory layout the SDK relies on (sizes and, for
/// H.264 and HEVC, the packet envelope). Whether the codec bitstream itself is
/// decodable is the application's responsibility.
///
/// Sending lends the bytes to the SDK, which writes to them: it stamps the
/// envelope's reserved field. A send therefore takes the frame exclusively or
/// by value, and a frame stays sendable however often it has been sent.
#[derive(Clone)]
pub struct CompressedVideoFrame {
    pub(crate) format: VideoPacketFormat,
    pub(crate) payload: PacketBytes,
    pub(crate) extras: FrameExtras,
}

packet_debug!(CompressedVideoFrame, "CompressedVideoFrame", frame, video, frame.timestamp();
    "bytes" = frame.data().len());

impl CompressedVideoFrame {
    frame_extras_accessors!();
    envelope_accessor!(payload.extents());

    /// Take ownership of one frame's bytes: a packet envelope for H.264 and
    /// HEVC (see [`PacketEnvelope::encode`]), a SpeedHQ frame otherwise. The
    /// timecode is synthesized by the SDK unless
    /// [`set_timecode`](Self::set_timecode) sets one.
    ///
    /// # Errors
    ///
    /// Returns the bytes with [`Error::InvalidFrame`] when they are empty, too
    /// large, not an envelope of the format's codec, an envelope this module
    /// cannot send (a later header version, unknown flags, trailing bytes, or
    /// a keyframe without codec configuration), or a frame the SDK does not
    /// deliver (see [`VideoCodec`]).
    pub fn new(
        format: VideoPacketFormat,
        bytes: Vec<u8>,
    ) -> std::result::Result<Self, Rejected<Vec<u8>>> {
        let payload = PacketBytes::new(bytes).map_err(|bytes| {
            Rejected::new(
                invalid_frame(crate::FrameDefect::CompressedVideoSize),
                bytes,
            )
        })?;
        let extents = match validate_video_content(format, payload.view(), Use::Send) {
            Ok(extents) => extents,
            Err(error) => return Err(Rejected::new(error, payload.into_vec())),
        };
        Ok(Self {
            format,
            payload: payload.with_envelope(extents),
            extras: FrameExtras::default(),
        })
    }

    /// The codec bytes.
    pub fn data(&self) -> &[u8] {
        self.payload.as_slice()
    }

    /// The format.
    pub fn format(&self) -> VideoPacketFormat {
        self.format
    }

    /// When the sender submitted a received frame, as the SDK recorded it;
    /// `None` on a frame built for sending.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        self.extras.timestamp()
    }

    /// Take the codec bytes, retaining the original allocation and capacity.
    pub fn into_data(self) -> Vec<u8> {
        self.payload.into_vec()
    }

    /// Bytes this frame keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.payload.capacity() + self.extras.retained_bytes()
    }

    /// A native descriptor that lends this frame's bytes and metadata to the
    /// SDK. The SDK may write to the bytes for as long as it holds the
    /// descriptor, so the caller keeps the frame exclusively for that long.
    pub(crate) fn raw(&mut self) -> NDIlib_video_frame_v2_t {
        let mut raw = self.format.descriptor(&self.extras);
        let (data, size) = self.payload.lend();
        raw.__bindgen_anon_1.data_size_in_bytes = size;
        raw.p_data = data;
        raw
    }
}

/// What a packet is being checked for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Use {
    /// Keeping a received packet: later envelope versions are accepted.
    Receive,
    /// Sending: only what this module fully understands is accepted.
    Send,
}

#[cfg(test)]
fn validate_video_packet(
    format: VideoPacketFormat,
    bytes: &[u8],
    purpose: Use,
) -> Result<PacketLen> {
    let bytes = PacketSlice::new(bytes)
        .ok_or_else(|| invalid_frame(crate::FrameDefect::CompressedVideoSize))?;
    validate_video_content(format, bytes, purpose)?;
    Ok(bytes.len())
}

fn validate_video_content(
    format: VideoPacketFormat,
    packet: PacketSlice<'_>,
    purpose: Use,
) -> Result<Option<EnvelopeExtents>> {
    let bytes = packet.as_slice();
    let len = packet.len();
    if format.codec.envelope_codec().is_none() && bytes.len() < SPEEDHQ_HEADER_BYTES {
        return Err(invalid_frame(crate::FrameDefect::SpeedHqTruncated));
    }
    match purpose {
        Use::Send => format.validate_for_send(bytes, len.to_usize()),
        Use::Receive => match format.codec.envelope_codec() {
            Some(codec) => EnvelopeHeader::read(bytes)?
                .validate(codec, len.to_usize(), purpose)
                .map(Some),
            None => Ok(None),
        },
    }
}

/// The sample rates an AAC configuration can name, in the order of its
/// sampling-frequency index.
const AAC_SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// The channel counts an AAC configuration can name, in the order of its
/// channel configuration, which starts at one.
const AAC_CHANNELS: [u32; 7] = [1, 2, 3, 4, 5, 6, 8];

/// The number of samples per channel in an AAC frame.
const AAC_FRAME_SAMPLES: u32 = 1024;

/// The sample rates Opus runs at.
const OPUS_SAMPLE_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// The durations of an Opus frame, in units of 2.5 ms.
const OPUS_FRAME_DURATIONS: [u64; 6] = [1, 2, 4, 8, 16, 24];

/// A compressed audio codec.
///
/// AAC travels in a packet envelope; Opus is sent as bare codec packets.
///
/// # Delivery
///
/// The SDK takes any compressed audio frame and silently discards the ones
/// whose parameters it does not accept, so this module refuses to build or
/// send those:
///
/// * Opus runs at 8, 12, 16, 24 or 48 kHz, and a frame holds 2.5, 5, 10, 20,
///   40 or 60 ms of audio.
/// * An AAC frame holds 1024 samples per channel and carries its
///   AudioSpecificConfig as codec configuration data. The sample rate and the
///   channel layout the configuration names must be the frame's own, which
///   allows the thirteen indexed sample rates from 7350 Hz to 96 kHz and one
///   to six or eight channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioCodec {
    /// AAC in a packet envelope.
    Aac,
    /// Opus, without an envelope.
    Opus,
}

crate::media::decoded_variants!(AudioCodec: Aac, Opus);

/// The codec, protection and decoded dimensions of one compressed audio frame.
///
/// Quantities are in `1..=i32::MAX`, the range of the SDK's signed fields.
/// Codec delivery rules are checked when a frame is admitted for sending,
/// preserving lossless inspection of received packets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AudioPacketFormat {
    codec: AudioCodec,
    protected: bool,
    sample_rate: SdkQuantity,
    num_channels: SdkQuantity,
    num_samples: SdkQuantity,
}

impl AudioPacketFormat {
    /// Describe a compressed audio frame of `codec` with the given quantities.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] unless each quantity is in `1..=i32::MAX`.
    pub fn new(
        codec: AudioCodec,
        sample_rate: u32,
        num_channels: u32,
        num_samples: u32,
    ) -> Result<Self> {
        Self::checked(
            codec,
            false,
            sample_rate.into(),
            num_channels.into(),
            num_samples.into(),
        )
        .map_err(|cause| Error::InvalidInput(crate::InputDefect::Layout(cause)))
    }

    /// The same format with the requested stream protection.
    #[must_use]
    pub fn with_protected(self, protected: bool) -> Self {
        Self { protected, ..self }
    }

    /// The compressed audio codec.
    pub fn codec(self) -> AudioCodec {
        self.codec
    }

    /// Whether the stream is protected.
    pub fn is_protected(self) -> bool {
        self.protected
    }

    /// Sample rate in hertz.
    pub fn sample_rate(self) -> u32 {
        self.sample_rate.get()
    }

    /// Number of channels.
    pub fn num_channels(self) -> u32 {
        self.num_channels.get()
    }

    /// Samples per channel the frame decodes to.
    pub fn num_samples(self) -> u32 {
        self.num_samples.get()
    }

    /// The format of a frame the SDK delivered.
    fn from_sdk(raw: &NDIlib_audio_frame_v3_t) -> Result<Self> {
        let (codec, protected) = AudioCodec::from_outer(raw.FourCC)
            .ok_or_else(|| invalid_frame(crate::FrameDefect::UnknownCompressedAudio))?;
        Self::checked(
            codec,
            protected,
            raw.sample_rate.into(),
            raw.no_channels.into(),
            raw.no_samples.into(),
        )
        .map_err(|cause| invalid_frame(crate::FrameDefect::Layout(cause)))
    }

    /// The one quantity check, for values from the API and from the SDK.
    fn checked(
        codec: AudioCodec,
        protected: bool,
        sample_rate: i64,
        num_channels: i64,
        num_samples: i64,
    ) -> crate::frames::LayoutResult<Self> {
        let sample_rate = crate::frames::validate_audio_sample_rate(sample_rate)?;
        let (num_channels, num_samples) =
            crate::frames::validate_positive_audio_counts(num_channels, num_samples)?;
        Ok(Self {
            codec,
            protected,
            sample_rate,
            num_channels,
            num_samples,
        })
    }

    /// A native descriptor without data, carrying this format and `extras`.
    pub(crate) fn descriptor(self, extras: &FrameExtras) -> NDIlib_audio_frame_v3_t {
        let (timecode, timestamp, p_metadata) = extras.to_sdk();
        NDIlib_audio_frame_v3_t {
            sample_rate: self.sample_rate.to_sdk(),
            no_channels: self.num_channels.to_sdk(),
            no_samples: self.num_samples.to_sdk(),
            FourCC: self.codec.outer(self.protected),
            timecode,
            timestamp,
            p_metadata,
            ..descriptors::audio_frame()
        }
    }
}

impl AudioCodec {
    /// The number of bytes at the start of a frame of this codec that hold
    /// its header: the envelope header for AAC, none for Opus.
    pub(crate) fn header_bytes(self) -> usize {
        match self {
            Self::Aac => ENVELOPE_HEADER_BYTES,
            Self::Opus => 0,
        }
    }

    /// Check that a frame of this codec with `parameters`, `total` bytes long
    /// and beginning with `header`, is one this module can send and the SDK
    /// delivers; see [`AudioCodec`]. `header` holds
    /// [`header_bytes`](Self::header_bytes) bytes or more, and `bytes_at`
    /// reads the frame's two bytes at an offset, when it has them.
    ///
    /// The one admission of compressed audio, whether the frame is held in
    /// one buffer or in several blocks, as
    /// [`VideoPacketFormat::validate_for_send`] is for video.
    pub(crate) fn validate_for_send(
        self,
        parameters: AudioPacketFormat,
        header: &[u8],
        total: usize,
        bytes_at: impl FnOnce(usize) -> Option<[u8; 2]>,
    ) -> Result<Option<EnvelopeExtents>> {
        self.validate_delivery(parameters)?;
        match self {
            Self::Opus => Ok(None),
            Self::Aac => {
                let header = EnvelopeHeader::read(header)?;
                let extents = header.validate(EnvelopeCodec::Aac, total, Use::Send)?;
                header.validate_aac(bytes_at(extents.extra_offset()), parameters)?;
                Ok(Some(extents))
            }
        }
    }

    /// Check the parameters a frame of this codec is delivered with; see
    /// [`AudioCodec`]. An AAC frame's configuration is checked separately,
    /// against its envelope.
    fn validate_delivery(self, parameters: AudioPacketFormat) -> Result<()> {
        let sample_rate = parameters.sample_rate();
        let num_channels = parameters.num_channels();
        let num_samples = parameters.num_samples();
        match self {
            Self::Opus => {
                if !OPUS_SAMPLE_RATES.contains(&sample_rate) {
                    return Err(invalid_frame(crate::FrameDefect::OpusSampleRate));
                }
                // One unit is 2.5 ms, which is a four-hundredth of a second.
                let units = u64::from(num_samples) * 400;
                if !OPUS_FRAME_DURATIONS
                    .iter()
                    .any(|duration| units == duration * u64::from(sample_rate))
                {
                    return Err(invalid_frame(crate::FrameDefect::OpusDuration));
                }
            }
            Self::Aac => {
                if num_samples != AAC_FRAME_SAMPLES {
                    return Err(invalid_frame(crate::FrameDefect::AacSampleCount));
                }
                if !AAC_SAMPLE_RATES.contains(&sample_rate) {
                    return Err(invalid_frame(crate::FrameDefect::AacSampleRate));
                }
                if !AAC_CHANNELS.contains(&num_channels) {
                    return Err(invalid_frame(crate::FrameDefect::AacChannels));
                }
            }
        }
        Ok(())
    }

    /// The FourCC a frame of this codec is sent with.
    pub(crate) const fn outer(self, protected: bool) -> u32 {
        (match self {
            Self::Aac => EnvelopeCodec::Aac.raw_fourcc(),
            Self::Opus => fourcc(b"Opus"),
        }) | if protected { PROTECTED } else { 0 }
    }

    /// The codec and protection a frame FourCC names, if it is compressed
    /// audio this module knows.
    pub(crate) fn from_outer(value: u32) -> Option<(Self, bool)> {
        Self::ALL.into_iter().find_map(|codec| {
            [false, true]
                .into_iter()
                .find(|protected| value == codec.outer(*protected))
                .map(|protected| (codec, protected))
        })
    }
}

/// One owned compressed audio frame.
///
/// Sending lends the bytes to the SDK, which writes to them, so a send takes
/// the frame exclusively; see [`CompressedVideoFrame`].
#[derive(Clone)]
pub struct CompressedAudioFrame {
    pub(crate) format: AudioPacketFormat,
    pub(crate) payload: PacketBytes,
    pub(crate) extras: FrameExtras,
}

packet_debug!(CompressedAudioFrame, "CompressedAudioFrame", frame, audio, frame.timestamp();
    "bytes" = frame.data().len());

impl CompressedAudioFrame {
    frame_extras_accessors!();
    envelope_accessor!(payload.extents());

    /// Take ownership of one audio frame's bytes: a packet envelope for AAC
    /// (see [`PacketEnvelope::encode`]), one bare packet for Opus.
    ///
    /// # Errors
    ///
    /// Returns the bytes with [`Error::InvalidFrame`] when the bytes are empty
    /// or too large, the format parameters are
    /// not ones the codec is delivered with (see [`AudioCodec`]), or an AAC
    /// envelope is malformed or disagrees with them.
    pub fn new(
        format: AudioPacketFormat,
        bytes: Vec<u8>,
    ) -> std::result::Result<Self, Rejected<Vec<u8>>> {
        let payload = PacketBytes::new(bytes).map_err(|bytes| {
            Rejected::new(
                invalid_frame(crate::FrameDefect::CompressedAudioSize),
                bytes,
            )
        })?;
        let extents = match validate_audio_content_for_send(format, payload.view()) {
            Ok(extents) => extents,
            Err(error) => return Err(Rejected::new(error, payload.into_vec())),
        };
        Ok(Self {
            format,
            payload: payload.with_envelope(extents),
            extras: FrameExtras::default(),
        })
    }

    /// The codec bytes.
    pub fn data(&self) -> &[u8] {
        self.payload.as_slice()
    }

    /// The codec and audio format.
    pub fn format(&self) -> AudioPacketFormat {
        self.format
    }

    /// When the sender submitted a received frame, as the SDK recorded it;
    /// `None` on a frame built for sending.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        self.extras.timestamp()
    }

    /// Take the codec bytes, retaining the original allocation and capacity.
    pub fn into_data(self) -> Vec<u8> {
        self.payload.into_vec()
    }

    /// A native descriptor that lends this frame's bytes and metadata to the
    /// SDK; see [`CompressedVideoFrame::raw`].
    pub(crate) fn raw(&mut self) -> NDIlib_audio_frame_v3_t {
        let mut raw = self.format.descriptor(&self.extras);
        let (data, size) = self.payload.lend();
        raw.p_data = data;
        raw.__bindgen_anon_1.data_size_in_bytes = size;
        raw
    }
}

#[cfg(test)]
fn validate_audio_for_send(format: AudioPacketFormat, bytes: &[u8]) -> Result<PacketLen> {
    let bytes = PacketSlice::new(bytes)
        .ok_or_else(|| invalid_frame(crate::FrameDefect::CompressedAudioSize))?;
    validate_audio_content_for_send(format, bytes)?;
    Ok(bytes.len())
}

fn validate_audio_content_for_send(
    format: AudioPacketFormat,
    packet: PacketSlice<'_>,
) -> Result<Option<EnvelopeExtents>> {
    let bytes = packet.as_slice();
    let len = packet.len();
    format
        .codec
        .validate_for_send(format, bytes, len.to_usize(), |offset| {
            bytes.get(offset..)?.first_chunk().copied()
        })
}

fn validate_received_audio(
    codec: AudioCodec,
    packet: PacketSlice<'_>,
) -> Result<Option<EnvelopeExtents>> {
    if codec == AudioCodec::Aac {
        return EnvelopeHeader::read(packet.as_slice())?
            .validate(EnvelopeCodec::Aac, packet.len().to_usize(), Use::Receive)
            .map(Some);
    }
    Ok(None)
}

#[path = "packets/received.rs"]
mod received;
pub use received::{ReceivedAudioPacket, ReceivedVideoPacket};

#[path = "packets/borrowed.rs"]
mod borrowed;
pub use borrowed::{CompressedAudioRef, CompressedVideoRef};

/// Copy a compressed video frame the SDK delivered.
///
/// # Safety
///
/// `raw` describes a compressed frame: its union holds the data size, its data
/// pointer is readable for that size, and its metadata pointer is null or a
/// readable string through its first NUL or the metadata cap.
#[cfg(test)]
pub(crate) unsafe fn copy_video_packet(
    raw: &NDIlib_video_frame_v2_t,
) -> Result<ReceivedVideoPacket> {
    // SAFETY: the test caller retains raw's storage, and the synthetic guard
    // borrows it until the copy completes; its release callback frees nothing.
    let guard =
        unsafe { crate::capture::Guard::with_test_free(std::ptr::null_mut(), *raw, |_, _| {}) };
    CompressedVideoRef::new(guard).map(|frame| frame.to_owned())
}

/// Copy a compressed audio frame the SDK delivered.
///
/// # Safety
///
/// As for [`copy_video_packet`].
#[cfg(test)]
pub(crate) unsafe fn copy_audio_packet(
    raw: &NDIlib_audio_frame_v3_t,
) -> Result<ReceivedAudioPacket> {
    // SAFETY: as for the video fixture above.
    let guard =
        unsafe { crate::capture::Guard::with_test_free(std::ptr::null_mut(), *raw, |_, _| {}) };
    CompressedAudioRef::new(guard).map(|frame| frame.to_owned())
}

// The values above are the NDI wire format, which the data types in this file
// implement in every build. Where the build has the Advanced SDK declarations,
// each value is checked against them.
#[cfg(ndi_has_advanced_header)]
const_assert!({
    // Enum constants of the Advanced-only headers take the target's C enum
    // type, which is signed on some targets; compare bit patterns.
    macro_rules! same_bits {
        ($ours:expr, $native:ident) => {
            crate::sys::advanced::declared! { $native {
                assert!($ours == u32::from_ne_bytes(
                    crate::sys::advanced_bindings::$native.to_ne_bytes()
                ));
            } else {} }
        };
    }

    same_bits!(
        VideoCodec::SpeedHq420.outer(StreamTier::Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ0_highest_bandwidth
    );
    same_bits!(
        VideoCodec::SpeedHq420.outer(StreamTier::Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ0_highest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::SpeedHq420.outer(StreamTier::Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ0_lowest_bandwidth
    );
    same_bits!(
        VideoCodec::SpeedHq420.outer(StreamTier::Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ0_lowest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::SpeedHq422.outer(StreamTier::Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ2_highest_bandwidth
    );
    same_bits!(
        VideoCodec::SpeedHq422.outer(StreamTier::Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ2_highest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::SpeedHq422.outer(StreamTier::Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ2_lowest_bandwidth
    );
    same_bits!(
        VideoCodec::SpeedHq422.outer(StreamTier::Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ2_lowest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::SpeedHq422Alpha.outer(StreamTier::Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ7_highest_bandwidth
    );
    same_bits!(
        VideoCodec::SpeedHq422Alpha.outer(StreamTier::Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ7_highest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::SpeedHq422Alpha.outer(StreamTier::Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ7_lowest_bandwidth
    );
    same_bits!(
        VideoCodec::SpeedHq422Alpha.outer(StreamTier::Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ7_lowest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::H264.outer(StreamTier::Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_highest_bandwidth
    );
    same_bits!(
        VideoCodec::H264.outer(StreamTier::Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_highest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::H264.outer(StreamTier::Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_lowest_bandwidth
    );
    same_bits!(
        VideoCodec::H264.outer(StreamTier::Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_lowest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::Hevc.outer(StreamTier::Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_highest_bandwidth
    );
    same_bits!(
        VideoCodec::Hevc.outer(StreamTier::Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_highest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::Hevc.outer(StreamTier::Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_lowest_bandwidth
    );
    same_bits!(
        VideoCodec::Hevc.outer(StreamTier::Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_lowest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::H264Alpha.outer(StreamTier::Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_alpha_highest_bandwidth
    );
    same_bits!(
        VideoCodec::H264Alpha.outer(StreamTier::Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_alpha_highest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::H264Alpha.outer(StreamTier::Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_alpha_lowest_bandwidth
    );
    same_bits!(
        VideoCodec::H264Alpha.outer(StreamTier::Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_alpha_lowest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::HevcAlpha.outer(StreamTier::Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_alpha_highest_bandwidth
    );
    same_bits!(
        VideoCodec::HevcAlpha.outer(StreamTier::Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_alpha_highest_bandwidth_protected
    );
    same_bits!(
        VideoCodec::HevcAlpha.outer(StreamTier::Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_alpha_lowest_bandwidth
    );
    same_bits!(
        VideoCodec::HevcAlpha.outer(StreamTier::Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_alpha_lowest_bandwidth_protected
    );

    same_bits!(
        AudioCodec::Aac.outer(false),
        NDIlib_FourCC_audio_type_ex_e_NDIlib_FourCC_audio_type_ex_AAC
    );
    same_bits!(
        AudioCodec::Aac.outer(true),
        NDIlib_FourCC_audio_type_ex_e_NDIlib_FourCC_audio_type_ex_AAC_protected
    );
    same_bits!(
        AudioCodec::Opus.outer(false),
        NDIlib_FourCC_audio_type_ex_e_NDIlib_FourCC_audio_type_ex_OPUS
    );
    same_bits!(
        AudioCodec::Opus.outer(true),
        NDIlib_FourCC_audio_type_ex_e_NDIlib_FourCC_audio_type_ex_OPUS_protected
    );

    same_bits!(
        EnvelopeCodec::H264.raw_fourcc(),
        NDIlib_compressed_FourCC_type_e_NDIlib_compressed_FourCC_type_H264
    );
    same_bits!(
        EnvelopeCodec::Hevc.raw_fourcc(),
        NDIlib_compressed_FourCC_type_e_NDIlib_compressed_FourCC_type_HEVC
    );
    same_bits!(
        EnvelopeCodec::Aac.raw_fourcc(),
        NDIlib_compressed_FourCC_type_e_NDIlib_compressed_FourCC_type_AAC
    );

    crate::sys::advanced::declared! { NDIlib_compressed_packet_flags_keyframe {
        assert!(KEYFRAME == crate::sys::advanced_bindings::NDIlib_compressed_packet_flags_keyframe);
    } else {} }
    crate::sys::advanced::declared! { NDIlib_compressed_packet_version_0 {
        assert!(ENVELOPE_HEADER_BYTES as i32 == crate::sys::advanced_bindings::NDIlib_compressed_packet_version_0);
    } else {} }
    crate::sys::advanced::declared! { NDIlib_compressed_packet_t {
    use crate::sys::advanced_bindings::NDIlib_compressed_packet_t;
    use std::mem::{offset_of, size_of};

    assert!(size_of::<NDIlib_compressed_packet_t>() == ENVELOPE_HEADER_BYTES);
    assert!(offset_of!(NDIlib_compressed_packet_t, version) == VERSION_OFFSET);
    assert!(offset_of!(NDIlib_compressed_packet_t, fourCC) == CODEC_OFFSET);
    assert!(offset_of!(NDIlib_compressed_packet_t, pts) == PTS_OFFSET);
    assert!(offset_of!(NDIlib_compressed_packet_t, dts) == DTS_OFFSET);
    assert!(offset_of!(NDIlib_compressed_packet_t, reserved) == RESERVED_OFFSET);
    assert!(offset_of!(NDIlib_compressed_packet_t, flags) == FLAGS_OFFSET);
    assert!(offset_of!(NDIlib_compressed_packet_t, data_size) == DATA_SIZE_OFFSET);
    assert!(offset_of!(NDIlib_compressed_packet_t, extra_data_size) == EXTRA_DATA_SIZE_OFFSET);
    } else {} }
});

/// Frames for this module's unit tests.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A two-byte AAC-LC AudioSpecificConfig for an indexed sample rate and a
    /// channel count the configuration can name.
    pub(crate) fn aac_config(sample_rate: u32, channels: u32) -> [u8; 2] {
        let position = |values: &[u32], value| {
            values
                .iter()
                .position(|candidate| *candidate == value)
                .expect("a value the AAC configuration can name") as u16
        };
        let frequency_index = position(&AAC_SAMPLE_RATES, sample_rate);
        let channel_configuration = position(&AAC_CHANNELS, channels) + 1;
        ((2 << 11) | (frequency_index << 7) | (channel_configuration << 3)).to_be_bytes()
    }

    /// An AAC packet envelope with a matching configuration.
    pub(crate) fn aac_packet(sample_rate: u32, channels: u32, payload: &[u8]) -> Vec<u8> {
        PacketEnvelope::encode(
            EnvelopeCodec::Aac,
            0,
            0,
            true,
            payload,
            &aac_config(sample_rate, channels),
        )
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::{testing::*, *};

    #[test]
    fn packet_lengths_keep_the_sdk_bound() {
        for bytes in [0, MAX_PACKET_BYTES + 1, usize::MAX] {
            assert_eq!(PacketLen::new(bytes), None);
        }
        for bytes in [1, MAX_PACKET_BYTES] {
            let len = PacketLen::new(bytes).unwrap();
            assert_eq!(len.to_usize(), bytes);
            assert_eq!(PacketLen::from_sdk(len.to_sdk()), Some(len));
        }
        for bytes in [
            -1,
            i32::MIN,
            0,
            i32::try_from(MAX_PACKET_BYTES + 1).unwrap(),
            i32::MAX,
        ] {
            assert_eq!(PacketLen::from_sdk(bytes), None);
        }
    }

    #[test]
    fn empty_packet_lengths_keep_their_container_causes() {
        let cause = |result: Result<()>| match result.unwrap_err() {
            Error::InvalidFrame(cause) => cause,
            error => panic!("unexpected packet error: {error:?}"),
        };
        assert_eq!(
            cause(PacketEnvelope::parse(&[]).map(|_| ())),
            crate::FrameDefect::PacketHeaderTruncated
        );
        assert_eq!(
            cause(PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, false, &[], &[]).map(|_| ())),
            crate::FrameDefect::PacketPayloadSize
        );
        assert_eq!(
            cause(validate_video_packet(h264_format(), &[], Use::Send).map(|_| ())),
            crate::FrameDefect::CompressedVideoSize
        );
        let audio = AudioPacketFormat::new(AudioCodec::Opus, 48_000, 2, 960).unwrap();
        assert_eq!(
            cause(validate_audio_for_send(audio, &[]).map(|_| ())),
            crate::FrameDefect::CompressedAudioSize
        );
        for (blocks, expected) in [
            (vec![], crate::FrameDefect::ScatterBlockCount),
            (vec![vec![]], crate::FrameDefect::EmptyScatterBlock),
        ] {
            let rejected = super::super::ScatterVideoFrame::new(h264_format(), blocks).unwrap_err();
            assert!(matches!(rejected.error(), Error::InvalidFrame(actual) if *actual == expected));
        }
        // SAFETY: a zero extent is refused before a slice is formed.
        assert_eq!(
            cause(
                unsafe { PacketSlice::from_sdk(std::ptr::NonNull::dangling().as_ptr(), 0) }
                    .map(|_| ())
            ),
            crate::FrameDefect::CompressedBuffer
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "the full packet extent is covered by the native test run"
    )]
    fn oversized_packet_lengths_keep_their_container_causes() {
        let bytes = vec![0; MAX_PACKET_BYTES + 1];
        let cause = |result: Result<()>| match result.unwrap_err() {
            Error::InvalidFrame(cause) => cause,
            error => panic!("unexpected packet error: {error:?}"),
        };
        assert_eq!(
            cause(PacketEnvelope::parse(&bytes).map(|_| ())),
            crate::FrameDefect::PacketLength
        );
        assert_eq!(
            cause(
                PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, false, &bytes, &[]).map(|_| ())
            ),
            crate::FrameDefect::PacketPayloadSize
        );
        let rejected = CompressedVideoFrame::new(h264_format(), bytes).unwrap_err();
        assert!(matches!(
            rejected.error(),
            Error::InvalidFrame(crate::FrameDefect::CompressedVideoSize)
        ));
        let audio = AudioPacketFormat::new(AudioCodec::Opus, 48_000, 2, 960).unwrap();
        let rejected = CompressedAudioFrame::new(audio, rejected.into_inner()).unwrap_err();
        assert!(matches!(
            rejected.error(),
            Error::InvalidFrame(crate::FrameDefect::CompressedAudioSize)
        ));
        let bytes = rejected.into_inner();
        // SAFETY: the allocation covers the declared size, which is refused
        // before packet_bytes forms a slice or reads any byte.
        let received =
            unsafe { PacketSlice::from_sdk(bytes.as_ptr(), i32::try_from(bytes.len()).unwrap()) };
        assert_eq!(
            cause(received.map(|_| ())),
            crate::FrameDefect::CompressedBuffer
        );
        let rejected =
            super::super::ScatterVideoFrame::new(h264_format(), vec![bytes]).unwrap_err();
        assert!(matches!(
            rejected.error(),
            Error::InvalidFrame(crate::FrameDefect::ScatterSize)
        ));
    }

    fn h264_format() -> VideoPacketFormat {
        VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap()
    }

    fn h264_packet(keyframe: bool, extra_data: &[u8]) -> Vec<u8> {
        PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, keyframe, &[1], extra_data).unwrap()
    }

    #[test]
    fn packet_debug_summarizes_every_leaf_and_public_holder() {
        use crate::advanced::{
            PacketItem, QueuedVideoFrame, ReceivedAudio, ReceivedVideo, ReclaimedVideo,
            ScatterAudioFrame, ScatterVideoFrame, SubmittedVideo,
        };
        use crate::capture::Guard;

        fn summaries(size: usize) -> Vec<String> {
            let bytes =
                PacketEnvelope::encode(EnvelopeCodec::H264, 17, 13, true, &vec![7; size], &[1, 2])
                    .unwrap();
            let mut video = CompressedVideoFrame::new(h264_format(), bytes.clone()).unwrap();
            let raw = video.raw();
            // SAFETY: video owns the readable descriptor storage until the
            // synthetic non-owning receive guard is dropped.
            let guard = unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
            let borrowed = CompressedVideoRef::new(guard).unwrap();
            let received = borrowed.to_owned();
            let debug = format!("{video:?}");
            for other in [format!("{borrowed:?}"), format!("{received:?}")] {
                assert_eq!(
                    debug.split_once(" {").unwrap().1,
                    other.split_once(" {").unwrap().1
                );
            }
            let scatter = ScatterVideoFrame::new(
                h264_format(),
                vec![bytes[..44].to_vec(), bytes[44..].to_vec()],
            )
            .unwrap();
            let mut result = vec![
                debug,
                format!("{borrowed:?}"),
                format!("{received:?}"),
                format!("{:?}", video.envelope().unwrap()),
                format!("{scatter:?}"),
                format!("{:?}", ReceivedVideo::Compressed(received.clone())),
                format!(
                    "{:?}",
                    PacketItem::Video(ReceivedVideo::Compressed(received))
                ),
                format!("{:?}", SubmittedVideo::Contiguous(video.clone())),
                format!("{:?}", SubmittedVideo::Scatter(scatter.clone())),
                format!("{:?}", QueuedVideoFrame::Compressed(video.clone())),
                format!("{:?}", QueuedVideoFrame::Scatter(scatter.clone())),
                format!(
                    "{:?}",
                    ReclaimedVideo {
                        program: Some(SubmittedVideo::Contiguous(video.clone())),
                        preview: Some(SubmittedVideo::Scatter(scatter)),
                    }
                ),
            ];
            for codec in AudioCodec::ALL {
                let (samples, bytes) = match codec {
                    AudioCodec::Aac => (1024, aac_packet(48_000, 2, &vec![7; size])),
                    AudioCodec::Opus => (960, vec![7; size]),
                };
                let mut audio = CompressedAudioFrame::new(
                    AudioPacketFormat::new(codec, 48_000, 2, samples).unwrap(),
                    bytes.clone(),
                )
                .unwrap();
                let raw = audio.raw();
                // SAFETY: audio owns the readable descriptor storage until
                // the synthetic non-owning receive guard is dropped.
                let guard = unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
                let borrowed = CompressedAudioRef::new(guard).unwrap();
                let received = borrowed.to_owned();
                let debug = format!("{audio:?}");
                for other in [format!("{borrowed:?}"), format!("{received:?}")] {
                    assert_eq!(
                        debug.split_once(" {").unwrap().1,
                        other.split_once(" {").unwrap().1
                    );
                }
                let scatter = ScatterAudioFrame::new(
                    AudioPacketFormat::new(codec, 48_000, 2, samples).unwrap(),
                    vec![bytes[..44].to_vec(), bytes[44..].to_vec()],
                )
                .unwrap();
                result.extend([
                    debug,
                    format!("{borrowed:?}"),
                    format!("{received:?}"),
                    format!("{scatter:?}"),
                    format!("{:?}", ReceivedAudio::Compressed(received.clone())),
                    format!(
                        "{:?}",
                        PacketItem::Audio(ReceivedAudio::Compressed(received))
                    ),
                ]);
            }
            result
        }

        crate::test_support::assert_bounded_debug(&summaries(1024), &summaries(1024 * 1024));
    }

    fn aac(sample_rate: u32, channels: u32, samples: u32, packet: Vec<u8>) -> Result<()> {
        let format = AudioPacketFormat::new(AudioCodec::Aac, sample_rate, channels, samples)?;
        CompressedAudioFrame::new(format, packet)
            .map(drop)
            .map_err(Rejected::into_error)
    }

    fn opus(sample_rate: u32, channels: u32, samples: u32) -> Result<()> {
        let format = AudioPacketFormat::new(AudioCodec::Opus, sample_rate, channels, samples)?;
        CompressedAudioFrame::new(format, vec![1; 8])
            .map(drop)
            .map_err(Rejected::into_error)
    }

    /// The text of an invalid-frame error.
    fn reason<T: std::fmt::Debug>(result: Result<T>) -> crate::FrameDefect {
        match result {
            Err(Error::InvalidFrame(reason)) => reason,
            other => panic!("expected an invalid frame, got {other:?}"),
        }
    }

    #[test]
    fn envelope_causes_agree_across_containers_and_directions() {
        use crate::advanced::{ScatterAudioFrame, ScatterVideoFrame};
        use crate::FrameDefect::*;
        let cases: &[(&str, usize, u32, Option<crate::FrameDefect>, bool)] = &[
            (
                "short header",
                usize::MAX,
                0,
                Some(PacketHeaderTruncated),
                false,
            ),
            (
                "invalid size",
                VERSION_OFFSET,
                43,
                Some(PacketHeaderSize),
                false,
            ),
            (
                "later header",
                VERSION_OFFSET,
                48,
                Some(PacketHeaderUnsupported),
                true,
            ),
            (
                "unknown flags",
                FLAGS_OFFSET,
                3,
                Some(PacketHeaderUnsupported),
                true,
            ),
            ("codec", CODEC_OFFSET, 0, Some(PacketCodecMismatch), false),
            ("empty", DATA_SIZE_OFFSET, 0, Some(EmptyCodecPayload), false),
            (
                "truncated",
                DATA_SIZE_OFFSET,
                9,
                Some(PacketTruncated),
                false,
            ),
            (
                "trailing",
                DATA_SIZE_OFFSET,
                7,
                Some(PacketLengthsMismatch),
                true,
            ),
        ];
        for audio in [false, true] {
            let codec = if audio {
                EnvelopeCodec::Aac
            } else {
                EnvelopeCodec::H264
            };
            for (label, offset, value, expected, receive_accepts) in cases {
                let mut bytes = PacketEnvelope::encode(
                    codec,
                    0,
                    0,
                    true,
                    &[1; 8],
                    if audio { &[0x11, 0x90] } else { &[1, 2] },
                )
                .unwrap();
                if *label == "later header" {
                    bytes.splice(ENVELOPE_HEADER_BYTES..ENVELOPE_HEADER_BYTES, [0; 4]);
                }
                if *offset == usize::MAX {
                    bytes.truncate(43);
                } else {
                    bytes[*offset..*offset + 4].copy_from_slice(&value.to_ne_bytes());
                }
                let send = if audio {
                    CompressedAudioFrame::new(
                        AudioPacketFormat::new(AudioCodec::Aac, 48_000, 2, 1024).unwrap(),
                        bytes.clone(),
                    )
                    .map(drop)
                    .map_err(Rejected::into_error)
                } else {
                    CompressedVideoFrame::new(h264_format(), bytes.clone())
                        .map(drop)
                        .map_err(Rejected::into_error)
                };
                assert_eq!(Some(reason(send)), *expected, "{label}, audio={audio}");
                if bytes.len() >= ENVELOPE_HEADER_BYTES {
                    let blocks = vec![
                        bytes[..ENVELOPE_HEADER_BYTES].to_vec(),
                        bytes[ENVELOPE_HEADER_BYTES..].to_vec(),
                    ];
                    let scatter = if audio {
                        ScatterAudioFrame::new(
                            AudioPacketFormat::new(AudioCodec::Aac, 48_000, 2, 1024).unwrap(),
                            blocks,
                        )
                        .map(drop)
                        .map_err(Rejected::into_error)
                    } else {
                        ScatterVideoFrame::new(h264_format(), blocks)
                            .map(drop)
                            .map_err(Rejected::into_error)
                    };
                    assert_eq!(
                        Some(reason(scatter)),
                        *expected,
                        "scatter {label}, audio={audio}"
                    );
                }
                // SAFETY: both descriptors refer to this live bytes allocation.
                let received = unsafe {
                    if audio {
                        let mut raw = AudioPacketFormat::new(AudioCodec::Aac, 48_000, 2, 1024)
                            .unwrap()
                            .descriptor(&FrameExtras::default());
                        raw.p_data = bytes.as_ptr().cast_mut();
                        raw.__bindgen_anon_1.data_size_in_bytes =
                            PacketLen::new(bytes.len()).unwrap().to_sdk();
                        copy_audio_packet(&raw).map(drop)
                    } else {
                        let mut raw = h264_format().raw();
                        raw.p_data = bytes.as_ptr().cast_mut();
                        raw.__bindgen_anon_1.data_size_in_bytes =
                            PacketLen::new(bytes.len()).unwrap().to_sdk();
                        copy_video_packet(&raw).map(drop)
                    }
                };
                if *receive_accepts {
                    assert!(
                        received.is_ok(),
                        "receive {label}, audio={audio}: {received:?}"
                    );
                } else {
                    assert_eq!(
                        Some(reason(received)),
                        *expected,
                        "receive {label}, audio={audio}"
                    );
                }
            }
        }
    }

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn envelope_length_overflow_is_independent_of_direction() {
        let mut bytes = h264_packet(false, &[]);
        bytes[DATA_SIZE_OFFSET..DATA_SIZE_OFFSET + 4].copy_from_slice(&u32::MAX.to_ne_bytes());
        for purpose in [Use::Send, Use::Receive] {
            assert_eq!(
                reason(EnvelopeHeader::read(&bytes).unwrap().validate(
                    EnvelopeCodec::H264,
                    bytes.len(),
                    purpose
                )),
                crate::FrameDefect::PacketLengthOverflow
            );
        }
    }

    #[test]
    fn received_packet_and_uncompressed_headers_report_the_same_invalid_fields() {
        let valid = NDIlib_video_frame_v2_t {
            FourCC: VideoCodec::H264.outer(StreamTier::Program, false),
            xres: 16,
            yres: 16,
            frame_rate_N: 60,
            frame_rate_D: 1,
            frame_format_type: ScanType::Progressive.to_sdk(),
            ..Default::default()
        };
        let invalid = [
            NDIlib_video_frame_v2_t {
                frame_rate_N: 0,
                ..valid
            },
            NDIlib_video_frame_v2_t {
                frame_rate_N: -1,
                ..valid
            },
            NDIlib_video_frame_v2_t {
                frame_rate_D: 0,
                ..valid
            },
            NDIlib_video_frame_v2_t {
                frame_rate_D: -1,
                ..valid
            },
            NDIlib_video_frame_v2_t {
                picture_aspect_ratio: -1.0,
                ..valid
            },
            NDIlib_video_frame_v2_t {
                picture_aspect_ratio: f32::NAN,
                ..valid
            },
            NDIlib_video_frame_v2_t {
                picture_aspect_ratio: f32::INFINITY,
                ..valid
            },
            NDIlib_video_frame_v2_t {
                frame_format_type: u32::MAX,
                ..valid
            },
        ];
        for raw in invalid {
            let packet = reason(VideoPacketFormat::from_sdk(&raw).map(drop));
            let frame = reason(VideoHeader::from_sdk(&raw).map(drop));
            match (&packet, &frame) {
                (
                    crate::FrameDefect::Layout(crate::LayoutDefect::PictureAspectRatio(a)),
                    crate::FrameDefect::Layout(crate::LayoutDefect::PictureAspectRatio(b)),
                ) => assert_eq!(a.to_bits(), b.to_bits()),
                _ => assert_eq!(packet, frame),
            }
        }
        assert!(matches!(
            reason(VideoHeader::from_sdk(&invalid[7]).map(drop)),
            crate::FrameDefect::UnknownScanType { code: 0xFFFFFFFF }
        ));
        for ratio in [-1.0, f32::NAN, f32::INFINITY] {
            assert!(matches!(
                PictureAspectRatio::new(ratio),
                Err(Error::InvalidInput(crate::InputDefect::Layout(
                    crate::LayoutDefect::PictureAspectRatio(value)
                ))) if value.to_bits() == ratio.to_bits()
            ));
        }
    }

    #[test]
    fn packed_header_parsing_preserves_extensions_and_checks_bounds() {
        let mut bytes =
            PacketEnvelope::encode(EnvelopeCodec::H264, 12, -4, true, &[1, 2], &[3]).unwrap();
        let p = PacketEnvelope::parse(&bytes).unwrap();
        assert_eq!(p.codec(), Some(EnvelopeCodec::H264));
        assert_eq!(p.fourcc(), FourCc::from(EnvelopeCodec::H264));
        assert_eq!(p.fourcc().raw().to_le_bytes(), *b"H264");
        assert_eq!(p.presentation_time(), 12);
        assert_eq!(p.decode_time(), -4);
        assert!(p.is_keyframe());
        assert_eq!(p.data(), &[1, 2]);
        assert_eq!(p.extra_data(), &[3]);
        bytes.splice(44..44, [9, 8]);
        bytes[..4].copy_from_slice(&46i32.to_ne_bytes());
        assert_eq!(
            PacketEnvelope::parse(&bytes).unwrap().header_extension(),
            &[9, 8]
        );
        assert!(validate_video_packet(h264_format(), &bytes, Use::Receive).is_ok());
        assert!(CompressedVideoFrame::new(h264_format(), bytes.clone()).is_err());
        bytes[36..40].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(PacketEnvelope::parse(&bytes).is_err());

        // An unknown codec is kept, by its FourCC.
        let mut unknown = h264_packet(false, &[]);
        unknown[CODEC_OFFSET..CODEC_OFFSET + 4].copy_from_slice(b"AV01");
        let p = PacketEnvelope::parse(&unknown).unwrap();
        assert_eq!(p.codec(), None);
        assert_eq!(p.fourcc(), FourCc::from_raw(u32::from_ne_bytes(*b"AV01")));
        for codec in EnvelopeCodec::ALL {
            assert_eq!(EnvelopeCodec::try_from(FourCc::from(codec)).unwrap(), codec);
        }
        assert!(matches!(
            EnvelopeCodec::try_from(p.fourcc()),
            Err(Error::InvalidFrame(crate::FrameDefect::UnknownEnvelopeCodec { fourcc }))
                if fourcc == p.fourcc().raw()
        ));
    }

    #[test]
    fn codec_outer_roundtrips_and_opus_is_not_an_envelope() {
        for codec in AudioCodec::ALL {
            for protected in [false, true] {
                assert_eq!(
                    AudioCodec::from_outer(codec.outer(protected)),
                    Some((codec, protected))
                );
            }
        }
        for codec in VideoCodec::ALL {
            for tier in [StreamTier::Program, StreamTier::Preview] {
                for protected in [false, true] {
                    assert_eq!(
                        VideoCodec::from_outer(codec.outer(tier, protected)),
                        Some((codec, tier, protected))
                    );
                    let format = VideoPacketFormat::new(codec, 16, 16)
                        .unwrap()
                        .with_tier(tier)
                        .with_protected(protected);
                    let bytes = match codec.envelope_codec() {
                        Some(code) => PacketEnvelope::encode(code, 0, 0, false, &[1], &[]).unwrap(),
                        None => vec![1, 2, 3, 4],
                    };
                    let mut frame = CompressedVideoFrame::new(format, bytes).unwrap();
                    let raw = frame.raw();
                    assert_eq!(raw.FourCC, codec.outer(tier, protected));
                    let copied = unsafe { copy_video_packet(&raw) }.unwrap();
                    assert_eq!(copied.format(), format);
                    assert_eq!(copied.format().codec(), codec);
                    assert_eq!(copied.format().tier(), tier);
                    assert_eq!(copied.format().is_protected(), protected);
                    assert_eq!(copied.data(), frame.data());
                }
            }
        }
        assert!(PacketEnvelope::parse(b"Opus").is_err());
    }

    #[test]
    fn validated_envelopes_agree_for_owned_borrowed_and_received_packets() {
        use crate::capture::Guard;

        for codec in VideoCodec::ALL {
            for tier in [StreamTier::Program, StreamTier::Preview] {
                for protected in [false, true] {
                    let format = VideoPacketFormat::new(codec, 16, 16)
                        .unwrap()
                        .with_tier(tier)
                        .with_protected(protected);
                    let bytes = codec.envelope_codec().map_or_else(
                        || vec![1, 2, 3, 4],
                        |codec| {
                            PacketEnvelope::encode(codec, 17, 13, true, &[1, 2], &[3, 4]).unwrap()
                        },
                    );
                    let mut frame = CompressedVideoFrame::new(format, bytes).unwrap();
                    if codec.envelope_codec().is_some() {
                        assert_eq!(frame.envelope().unwrap().reserved(), 0);
                        let raw = frame.raw();
                        // SAFETY: the initialized reserved field is inside
                        // this exclusively owned frame's validated envelope.
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                123u64.to_ne_bytes().as_ptr(),
                                raw.p_data.add(RESERVED_OFFSET),
                                8,
                            );
                        }
                        assert_eq!(frame.envelope().unwrap().reserved(), 123);
                    }
                    let raw = frame.raw();
                    // SAFETY: frame retains the descriptor's initialized
                    // bytes until the synthetic non-owning guard is dropped.
                    let guard =
                        unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
                    let borrowed = CompressedVideoRef::new(guard).unwrap();
                    let received = borrowed.to_owned();
                    let expected = codec
                        .envelope_codec()
                        .map(|_| PacketEnvelope::parse(frame.data()).unwrap());
                    assert_eq!(frame.envelope(), expected);
                    assert_eq!(borrowed.envelope(), expected);
                    assert_eq!(received.envelope(), expected);
                }
            }
        }

        for codec in AudioCodec::ALL {
            for protected in [false, true] {
                let (samples, bytes) = match codec {
                    AudioCodec::Aac => (1024, testing::aac_packet(48_000, 2, &[1, 2])),
                    AudioCodec::Opus => (960, vec![1, 2]),
                };
                let mut frame = CompressedAudioFrame::new(
                    AudioPacketFormat::new(codec, 48_000, 2, samples).unwrap(),
                    bytes,
                )
                .unwrap();
                frame.format = frame.format.with_protected(protected);
                let raw = frame.raw();
                // SAFETY: frame retains all descriptor storage until the
                // synthetic non-owning guard is dropped.
                let guard = unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
                let borrowed = CompressedAudioRef::new(guard).unwrap();
                let received = borrowed.to_owned();
                assert_eq!(borrowed.format(), frame.format());
                assert_eq!(received.format(), borrowed.format());
                let sendable = CompressedAudioFrame::try_from(received.clone()).unwrap();
                assert_eq!(sendable.format(), borrowed.format());
                let expected = (codec == AudioCodec::Aac)
                    .then(|| PacketEnvelope::parse(frame.data()).unwrap());
                assert_eq!(frame.envelope(), expected);
                assert_eq!(borrowed.envelope(), expected);
                assert_eq!(received.envelope(), expected);
            }
        }
    }

    #[test]
    fn audio_packet_formats_validate_quantities_and_preserve_protection() {
        fn value_traits<T: Copy + Eq + std::hash::Hash + Send + Sync>() {}
        value_traits::<AudioPacketFormat>();
        let max = i32::MAX as u32;
        let format = AudioPacketFormat::new(AudioCodec::Opus, max, max, max).unwrap();
        assert_eq!(
            (
                format.sample_rate(),
                format.num_channels(),
                format.num_samples()
            ),
            (max, max, max)
        );
        for (sample_rate, channels, samples) in [
            (0, 2, 960),
            (48_000, 0, 960),
            (48_000, 2, 0),
            (max + 1, 2, 960),
            (48_000, max + 1, 960),
            (48_000, 2, max + 1),
        ] {
            assert!(matches!(
                AudioPacketFormat::new(AudioCodec::Opus, sample_rate, channels, samples),
                Err(Error::InvalidInput(crate::InputDefect::Layout(_)))
            ));
        }
        let format = AudioPacketFormat::new(AudioCodec::Opus, 48_000, 2, 960).unwrap();
        assert!(!format.is_protected());
        assert!(format.with_protected(true).is_protected());
        assert_eq!(format.with_protected(true).with_protected(false), format);
        let protected = format.with_protected(true);
        assert_eq!(
            AudioPacketFormat::from_sdk(&protected.descriptor(&FrameExtras::default())).unwrap(),
            protected
        );
    }

    #[test]
    fn audio_packet_quantities_share_causes_for_api_and_sdk_input() {
        for (rate, channels, samples) in [(0, 2, 960), (48_000, 0, 960), (48_000, 2, 0)] {
            let api = AudioPacketFormat::new(AudioCodec::Opus, rate, channels, samples);
            let raw = NDIlib_audio_frame_v3_t {
                sample_rate: rate as i32,
                no_channels: channels as i32,
                no_samples: samples as i32,
                FourCC: AudioCodec::Opus.outer(false),
                ..Default::default()
            };
            let (
                Err(Error::InvalidInput(crate::InputDefect::Layout(api))),
                Err(Error::InvalidFrame(crate::FrameDefect::Layout(sdk))),
            ) = (api, AudioPacketFormat::from_sdk(&raw))
            else {
                panic!("expected paired layout causes")
            };
            assert_eq!(api, sdk);
            if rate == 0 {
                assert_eq!(api, crate::LayoutDefect::AudioRate(0.into()));
            }
        }
    }

    /// The dimensions of a format the caller describes are input; those of a
    /// frame the SDK delivered belong to the frame.
    #[test]
    fn packet_format_dimensions_are_checked_on_both_sides_of_the_sdk() {
        let max = i32::MAX.unsigned_abs();
        let format = VideoPacketFormat::new(VideoCodec::H264, max, 1).unwrap();
        assert_eq!((format.width(), format.height()), (max, 1));
        for (width, height) in [(0, 1), (1, 0), (max + 1, 1), (1, u32::MAX)] {
            assert!(matches!(
                VideoPacketFormat::new(VideoCodec::H264, width, height),
                Err(Error::InvalidInput(reason)) if matches!(reason, crate::InputDefect::Layout(crate::LayoutDefect::CompressedVideoDimensions {..}))
            ));
        }
        let delivered = h264_format().raw();
        for (xres, yres) in [(0, 16), (16, -1), (i32::MIN, 16)] {
            let raw = NDIlib_video_frame_v2_t {
                xres,
                yres,
                ..delivered
            };
            assert!(matches!(
                VideoPacketFormat::from_sdk(&raw),
                Err(Error::InvalidFrame(_))
            ));
        }
        let raw = NDIlib_video_frame_v2_t {
            frame_rate_D: 0,
            ..delivered
        };
        assert!(matches!(
            VideoPacketFormat::from_sdk(&raw),
            Err(Error::InvalidFrame(_))
        ));

        let rate = FrameRate::new(60_000, 1001).unwrap();
        let format = h264_format()
            .with_frame_rate(rate)
            .with_scan_type(ScanType::Interlaced);
        assert_eq!(h264_format().frame_rate(), FrameRate::new(60, 1).unwrap());
        assert_eq!(
            (format.frame_rate(), format.scan_type()),
            (rate, ScanType::Interlaced)
        );
        let raw = format.raw();
        assert_eq!((raw.frame_rate_N, raw.frame_rate_D), (60_000, 1001));
        assert_eq!(VideoPacketFormat::from_sdk(&raw).unwrap(), format);
    }

    #[test]
    fn a_zero_aspect_ratio_means_square_pixels_and_is_kept_when_received() {
        let format = h264_format();
        assert_eq!(
            format.picture_aspect_ratio(),
            PictureAspectRatio::SQUARE_PIXELS
        );
        assert_eq!(
            format
                .with_picture_aspect_ratio(PictureAspectRatio::SQUARE_PIXELS)
                .picture_aspect_ratio(),
            PictureAspectRatio::SQUARE_PIXELS
        );

        // A received frame whose descriptor leaves the ratio unset is kept
        // as it is.
        let mut frame = CompressedVideoFrame::new(format, h264_packet(false, &[])).unwrap();
        let raw = frame.raw();
        assert_eq!(raw.picture_aspect_ratio, 0.0);
        let received = unsafe { copy_video_packet(&raw) }.unwrap();
        assert_eq!(
            received.format().picture_aspect_ratio(),
            PictureAspectRatio::SQUARE_PIXELS
        );

        let wide = NDIlib_video_frame_v2_t {
            picture_aspect_ratio: 16.0 / 9.0,
            ..raw
        };
        let received = unsafe { copy_video_packet(&wide) }.unwrap();
        assert_eq!(
            received.format().picture_aspect_ratio(),
            PictureAspectRatio::new(16.0 / 9.0).unwrap()
        );

        let negative = NDIlib_video_frame_v2_t {
            picture_aspect_ratio: -1.0,
            ..raw
        };
        assert!(matches!(
            unsafe { copy_video_packet(&negative) },
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn rejected_constructors_return_their_input() {
        let rejected = CompressedVideoFrame::new(h264_format(), vec![7; 20]).unwrap_err();
        assert_eq!(rejected.into_inner(), vec![7; 20]);

        let format = AudioPacketFormat::new(AudioCodec::Opus, 44_100, 2, 960).unwrap();
        let rejected = CompressedAudioFrame::new(format, vec![1, 2]).unwrap_err();
        assert!(matches!(
            rejected.error(),
            Error::InvalidFrame(crate::FrameDefect::OpusSampleRate)
        ));
        assert_eq!(rejected.into_inner(), [1, 2]);

        let mut frame = CompressedVideoFrame::new(
            h264_format().with_tier(StreamTier::Preview),
            h264_packet(false, &[]),
        )
        .unwrap();
        assert!(matches!(
            frame
                .set_metadata("nul\0inside")
                .map_err(Rejected::into_error),
            Err(Error::InvalidCString(_))
        ));
        assert_eq!(frame.format().tier(), StreamTier::Preview);
        assert_eq!(frame.metadata(), None);
        frame.set_metadata("<ok/>").unwrap();
        assert_eq!(frame.metadata(), Some("<ok/>"));
    }

    #[test]
    fn frame_extras_retain_owned_metadata_storage_until_cleared() {
        fn check(extras: &mut FrameExtras) {
            let mut text = String::with_capacity(1024);
            text.push_str("<metadata label='é'/>");
            let (pointer, capacity) = (text.as_ptr(), text.capacity());
            let ((), allocations) =
                crate::test_alloc::allocation_count(|| extras.set_metadata(text).unwrap());
            assert_eq!(allocations, 0);
            assert_eq!(extras.metadata().unwrap().as_ptr(), pointer);
            assert_eq!(extras.retained_bytes(), capacity);
            assert_eq!(extras.to_sdk().2.cast::<u8>(), pointer);

            assert!(extras.set_metadata("bad\0metadata").is_err());
            assert_eq!(extras.metadata(), Some("<metadata label='é'/>"));
            assert_eq!(extras.metadata().unwrap().as_ptr(), pointer);
            assert_eq!(extras.retained_bytes(), capacity);
            extras.clear_metadata();
            assert_eq!(extras.metadata(), None);
            assert_eq!(extras.retained_bytes(), 0);
            assert!(extras.to_sdk().2.is_null());
        }

        check(&mut FrameExtras::default());
    }

    #[test]
    fn received_metadata_preserves_presence_and_omits_unreadable_text() {
        // SAFETY: all non-null pointers below name live terminated byte strings.
        unsafe {
            let absent = FrameExtras::received(123, 456, std::ptr::null()).unwrap();
            assert_eq!(absent.metadata(), None);
            assert_eq!(absent.metadata_status(), crate::FrameMetadataStatus::Absent);
            assert!(absent.to_sdk().2.is_null());
            for text in [c"", c"<received/>"] {
                let extras = FrameExtras::received(123, 456, text.as_ptr()).unwrap();
                assert_eq!(extras.metadata(), Some(text.to_str().unwrap()));
                assert_eq!(extras.metadata_status(), crate::FrameMetadataStatus::Text);
                assert_eq!(extras.retained_bytes(), text.to_bytes_with_nul().len());
                assert_eq!(std::ffi::CStr::from_ptr(extras.to_sdk().2), text);
                assert_eq!(extras.timecode().ticks(), 123);
                assert_eq!(extras.timestamp().unwrap().ticks(), 456);
            }
            let unterminated = vec![b'x'; crate::frames::MAX_METADATA_BYTES];
            for (pointer, status) in [
                (c"\xff".as_ptr(), crate::FrameMetadataStatus::NotUtf8),
                (
                    unterminated.as_ptr().cast(),
                    crate::FrameMetadataStatus::Unterminated,
                ),
            ] {
                let (extras, allocations) = crate::test_alloc::allocation_count(|| {
                    FrameExtras::received(123, 456, pointer).unwrap()
                });
                assert_eq!(allocations, 0);
                assert_eq!(extras.metadata(), None);
                assert_eq!(extras.metadata_status(), status);
                assert!(extras.to_sdk().2.is_null());
            }
        }
    }

    #[test]
    fn protected_aac_preserves_codec_high_bit() {
        let bytes = aac_packet(48_000, 2, &[1]);
        let mut frame = CompressedAudioFrame::new(
            AudioPacketFormat::new(AudioCodec::Aac, 48_000, 2, 1024).unwrap(),
            bytes,
        )
        .unwrap();
        frame.format = frame.format.with_protected(true);
        let raw = frame.raw();
        let copied = unsafe { copy_audio_packet(&raw) }.unwrap();
        assert_eq!(copied.format().codec(), AudioCodec::Aac);
        assert!(copied.format().is_protected());
        assert_eq!(
            (
                copied.format().sample_rate(),
                copied.format().num_channels(),
                copied.format().num_samples()
            ),
            (48_000, 2, 1024)
        );
        assert_eq!(copied.data(), frame.data());
    }

    #[test]
    fn unknown_native_enum_bits_are_rejected_without_truncation() {
        for bits in [0x8000_0000, PROTECTED, u32::MAX] {
            let video = NDIlib_video_frame_v2_t {
                FourCC: bits,
                ..Default::default()
            };
            let audio = NDIlib_audio_frame_v3_t {
                FourCC: bits,
                ..Default::default()
            };
            assert!(unsafe { copy_video_packet(&video) }.is_err());
            assert!(unsafe { copy_audio_packet(&audio) }.is_err());
        }
        let mut video = h264_format().raw();
        video.frame_format_type = u32::MAX;
        assert!(unsafe { copy_video_packet(&video) }.is_err());
    }

    /// Received audio whose parameters are not positive is an invalid frame,
    /// whatever its bytes.
    #[test]
    fn delivered_audio_parameters_are_validated_before_they_become_unsigned() {
        let mut frame = CompressedAudioFrame::new(
            AudioPacketFormat::new(AudioCodec::Opus, 48_000, 2, 960).unwrap(),
            vec![1; 8],
        )
        .unwrap();
        let raw = frame.raw();
        for (sample_rate, no_channels, no_samples) in
            [(0, 2, 960), (48_000, -2, 960), (48_000, 2, 0)]
        {
            let delivered = NDIlib_audio_frame_v3_t {
                sample_rate,
                no_channels,
                no_samples,
                ..raw
            };
            assert!(matches!(
                unsafe { copy_audio_packet(&delivered) },
                Err(Error::InvalidFrame(_))
            ));
        }
    }

    #[test]
    fn receiving_keeps_unknown_flags_but_sending_rejects_them() {
        let mut bytes = h264_packet(false, &[]);
        bytes[FLAGS_OFFSET..FLAGS_OFFSET + 4].copy_from_slice(&2u32.to_ne_bytes());
        let format = h264_format();
        assert_eq!(PacketEnvelope::parse(&bytes).unwrap().flags(), 2);
        assert!(validate_video_packet(format, &bytes, Use::Receive).is_ok());
        assert!(validate_video_packet(format, &bytes, Use::Send).is_err());
    }

    #[test]
    fn envelope_slices_keep_admitted_extents_after_native_header_writes() {
        let overwrite = |data: *mut u8| {
            // SAFETY: each caller passes the writable header from its own
            // live frame's descriptor, with a complete version-0 header.
            unsafe {
                data.add(VERSION_OFFSET).cast::<i32>().write_unaligned(-1);
                data.add(DATA_SIZE_OFFSET)
                    .cast::<u32>()
                    .write_unaligned(u32::MAX);
                data.add(EXTRA_DATA_SIZE_OFFSET)
                    .cast::<u32>()
                    .write_unaligned(u32::MAX);
                data.add(RESERVED_OFFSET).cast::<u64>().write_unaligned(42);
            }
        };
        let assert_envelope = |view: PacketEnvelope<'_>, codec, config: &[u8]| {
            assert_eq!(view.codec(), Some(codec));
            assert!(view.is_known_version());
            assert!(view.header_extension().is_empty());
            assert_eq!(view.data(), &[1, 2]);
            assert_eq!(view.extra_data(), config);
            assert!(view.trailing_bytes().is_empty());
            assert_eq!(view.reserved(), 42);
        };

        for (codec, envelope) in [
            (VideoCodec::H264, EnvelopeCodec::H264),
            (VideoCodec::Hevc, EnvelopeCodec::Hevc),
        ] {
            let format = VideoPacketFormat::new(codec, 1920, 1080).unwrap();
            let bytes = PacketEnvelope::encode(envelope, 0, 0, true, &[1, 2], &[3]).unwrap();
            let mut frame = CompressedVideoFrame::new(format, bytes).unwrap();
            overwrite(frame.raw().p_data);
            assert_envelope(frame.envelope().unwrap(), envelope, &[3]);
            let received = ReceivedVideoPacket::from(frame);
            assert_envelope(received.envelope().unwrap(), envelope, &[3]);
        }

        let mut frame = CompressedAudioFrame::new(
            AudioPacketFormat::new(AudioCodec::Aac, 48_000, 2, 1024).unwrap(),
            aac_packet(48_000, 2, &[1, 2]),
        )
        .unwrap();
        overwrite(frame.raw().p_data);
        assert_envelope(frame.envelope().unwrap(), EnvelopeCodec::Aac, &[0x11, 0x90]);
        let received = ReceivedAudioPacket::from(frame);
        assert_envelope(
            received.envelope().unwrap(),
            EnvelopeCodec::Aac,
            &[0x11, 0x90],
        );
    }

    /// The SDK stamps the reserved field of every envelope it sends, in the
    /// sender's buffer, and delivers packets with the field set. Neither a
    /// frame that has been sent nor one that was received may be refused for
    /// it.
    #[test]
    fn the_reserved_field_never_decides_whether_a_packet_can_be_sent() {
        let format = h264_format();
        let stamp = |bytes: &mut [u8], value: u64| {
            bytes[RESERVED_OFFSET..RESERVED_OFFSET + 8].copy_from_slice(&value.to_ne_bytes());
        };

        // As received: any value, on video and on audio.
        for value in [1, 1533, u64::MAX] {
            let mut video = h264_packet(true, &[2]);
            stamp(&mut video, value);
            assert_eq!(PacketEnvelope::parse(&video).unwrap().reserved(), value);
            let frame = CompressedVideoFrame::new(format, video).unwrap();
            ReceivedVideoPacket::from(frame.clone())
                .validate_for_send()
                .unwrap();

            let mut audio = aac_packet(48_000, 2, &[1]);
            stamp(&mut audio, value);
            let frame = CompressedAudioFrame::new(
                AudioPacketFormat::new(AudioCodec::Aac, 48_000, 2, 1024).unwrap(),
                audio,
            )
            .unwrap();
            ReceivedAudioPacket::from(frame.clone())
                .validate_for_send()
                .unwrap();
        }

        // As sent: the SDK writes through the pointer the descriptor lends.
        let mut frame = CompressedVideoFrame::new(format, h264_packet(true, &[2])).unwrap();
        for value in 1..=2u64 {
            let raw = frame.raw();
            // SAFETY: the descriptor lends the frame's own bytes, which hold a
            // complete header, and the frame is borrowed exclusively.
            unsafe {
                raw.p_data
                    .add(RESERVED_OFFSET)
                    .cast::<u64>()
                    .write_unaligned(value)
            };
            assert_eq!(
                PacketEnvelope::parse(frame.data()).unwrap().reserved(),
                value
            );
            ReceivedVideoPacket::from(frame.clone())
                .validate_for_send()
                .unwrap();
        }
    }

    #[test]
    fn video_is_admitted_only_when_the_sdk_delivers_it() {
        let video = |format: VideoPacketFormat, bytes: Vec<u8>| {
            CompressedVideoFrame::new(format, bytes)
                .map(drop)
                .map_err(Rejected::into_error)
        };
        // A SpeedHQ frame states its quality in its first byte.
        for codec in [
            VideoCodec::SpeedHq420,
            VideoCodec::SpeedHq422,
            VideoCodec::SpeedHq422Alpha,
        ] {
            for tier in [StreamTier::Program, StreamTier::Preview] {
                let format = VideoPacketFormat::new(codec, 1920, 1080)
                    .unwrap()
                    .with_tier(tier);
                for quality in [0, 1, 99] {
                    video(format, vec![quality, 0, 0, 0]).unwrap();
                }
                for quality in [100, 101, 255] {
                    assert!(matches!(
                        reason(video(format, vec![quality, 0, 0, 0])),
                        crate::FrameDefect::SpeedHqQuality
                    ));
                    // A received frame is kept whatever it holds.
                    assert!(
                        validate_video_packet(format, &[quality, 0, 0, 0], Use::Receive).is_ok()
                    );
                }
                assert!(matches!(
                    reason(video(format, vec![0; 3])),
                    crate::FrameDefect::SpeedHqTruncated
                ));
            }
        }

        // The preview stream of an envelope codec fits in 640 by 640.
        let keyframe = |codec: VideoCodec| {
            PacketEnvelope::encode(codec.envelope_codec().unwrap(), 0, 0, true, &[1], &[2]).unwrap()
        };
        for codec in [
            VideoCodec::H264,
            VideoCodec::Hevc,
            VideoCodec::H264Alpha,
            VideoCodec::HevcAlpha,
        ] {
            let preview = |width, height| {
                let format = VideoPacketFormat::new(codec, width, height)
                    .unwrap()
                    .with_tier(StreamTier::Preview);
                video(format, keyframe(codec))
            };
            for (width, height) in [(640, 360), (360, 640), (640, 640), (1, 1)] {
                preview(width, height).unwrap();
            }
            for (width, height) in [(641, 360), (640, 641), (1280, 720), (2, 2000)] {
                assert!(matches!(
                    reason(preview(width, height)),
                    crate::FrameDefect::PreviewVideoDimensions
                ));
                let format = VideoPacketFormat::new(codec, width, height).unwrap();
                // The program stream has no such limit, and neither has a
                // frame that was received.
                video(format, keyframe(codec)).unwrap();
                let format = format.with_tier(StreamTier::Preview);
                assert!(validate_video_packet(format, &keyframe(codec), Use::Receive).is_ok());
            }
        }
        // SpeedHQ previews are not limited.
        let format = VideoPacketFormat::new(VideoCodec::SpeedHq422, 1920, 1080)
            .unwrap()
            .with_tier(StreamTier::Preview);
        video(format, vec![0; 4]).unwrap();
    }

    #[test]
    fn an_aac_envelope_always_carries_the_keyframe_flag() {
        let config = aac_config(48_000, 2);
        for keyframe in [false, true] {
            let packet =
                PacketEnvelope::encode(EnvelopeCodec::Aac, 0, 0, keyframe, &[1], &config).unwrap();
            assert!(PacketEnvelope::parse(&packet).unwrap().is_keyframe());
            aac(48_000, 2, 1024, packet).unwrap();
        }
        // The flag stays the caller's choice for video.
        assert!(!PacketEnvelope::parse(&h264_packet(false, &[]))
            .unwrap()
            .is_keyframe());

        // An AAC packet from elsewhere without the flag is kept when received
        // and refused when sent: the SDK would drop it without a word.
        let mut packet = aac_packet(48_000, 2, &[1]);
        packet[FLAGS_OFFSET..FLAGS_OFFSET + 4].copy_from_slice(&0u32.to_ne_bytes());
        let payload = PacketBytes::new(packet.clone()).unwrap();
        let extents = validate_received_audio(AudioCodec::Aac, payload.view()).unwrap();
        let received = ReceivedAudioPacket {
            format: AudioPacketFormat::new(AudioCodec::Aac, 48_000, 2, 1024).unwrap(),
            payload: payload.with_envelope(extents),
            extras: FrameExtras::default(),
        };
        assert!(matches!(
            reason(received.validate_for_send()),
            crate::FrameDefect::AacKeyframeMissing
        ));
        assert!(matches!(
            reason(aac(48_000, 2, 1024, packet)),
            crate::FrameDefect::AacKeyframeMissing
        ));
    }

    #[test]
    fn opus_is_admitted_only_at_the_rates_and_durations_the_sdk_delivers() {
        for rate in [8_000, 12_000, 16_000, 24_000, 48_000] {
            // 2.5, 5, 10, 20, 40 and 60 ms.
            for units in [1, 2, 4, 8, 16, 24] {
                opus(rate, 2, rate / 400 * units).unwrap();
            }
            // 7.5, 15, 30, 80 and 120 ms.
            for units in [3, 6, 12, 32, 48] {
                assert!(matches!(
                    reason(opus(rate, 2, rate / 400 * units)),
                    crate::FrameDefect::OpusDuration
                ));
            }
            assert!(matches!(
                reason(opus(rate, 2, rate / 400 + 1)),
                crate::FrameDefect::OpusDuration
            ));
        }
        for rate in [11_025, 22_050, 32_000, 44_100, 96_000] {
            assert!(matches!(
                reason(opus(rate, 2, rate / 50)),
                crate::FrameDefect::OpusSampleRate
            ));
        }
        for channels in [1, 2, 6, 255] {
            opus(48_000, channels, 960).unwrap();
        }
        assert!(matches!(
            opus(48_000, 0, 960),
            Err(Error::InvalidInput(crate::InputDefect::Layout(
                crate::LayoutDefect::AudioCount {
                    kind: crate::AudioCountKind::Channels,
                    ..
                }
            )))
        ));
    }

    #[test]
    fn aac_is_admitted_only_with_a_configuration_that_matches_the_frame() {
        for rate in AAC_SAMPLE_RATES {
            aac(rate, 2, 1024, aac_packet(rate, 2, &[1])).unwrap();
        }
        for channels in AAC_CHANNELS {
            aac(48_000, channels, 1024, aac_packet(48_000, channels, &[1])).unwrap();
        }

        for samples in [1, 512, 960, 2048] {
            let result = aac(48_000, 2, samples, aac_packet(48_000, 2, &[1]));
            assert!(matches!(reason(result), crate::FrameDefect::AacSampleCount));
        }
        assert!(matches!(
            reason(aac(50_000, 2, 1024, aac_packet(48_000, 2, &[1]))),
            crate::FrameDefect::AacSampleRate
        ));
        assert!(matches!(
            reason(aac(48_000, 7, 1024, aac_packet(48_000, 8, &[1]))),
            crate::FrameDefect::AacChannels
        ));

        // A configuration for another rate or layout than the frame's.
        let result = aac(44_100, 2, 1024, aac_packet(48_000, 2, &[1]));
        assert!(matches!(
            reason(result),
            crate::FrameDefect::AacRateMismatch
        ));
        let result = aac(48_000, 1, 1024, aac_packet(48_000, 2, &[1]));
        assert!(matches!(
            reason(result),
            crate::FrameDefect::AacChannelsMismatch
        ));

        // No configuration, and one too short to name a rate and a layout.
        for config in [&[][..], &[0x11][..]] {
            let packet =
                PacketEnvelope::encode(EnvelopeCodec::Aac, 0, 0, true, &[1], config).unwrap();
            assert!(matches!(
                reason(aac(48_000, 2, 1024, packet)),
                crate::FrameDefect::AacConfigurationMissing
            ));
        }
        // A longer configuration is read by its first two bytes.
        let mut config = aac_config(48_000, 2).to_vec();
        config.extend([0x56, 0xe5, 0x00]);
        let packet = PacketEnvelope::encode(EnvelopeCodec::Aac, 0, 0, true, &[1], &config).unwrap();
        aac(48_000, 2, 1024, packet).unwrap();

        // Sample rates and layouts the configuration has no index for.
        let unindexed = |frequency_index: u16, channel_configuration: u16| {
            let config = ((2u16 << 11) | (frequency_index << 7) | (channel_configuration << 3))
                .to_be_bytes();
            let packet =
                PacketEnvelope::encode(EnvelopeCodec::Aac, 0, 0, true, &[1], &config).unwrap();
            reason(aac(48_000, 2, 1024, packet))
        };
        assert!(matches!(
            unindexed(15, 2),
            crate::FrameDefect::AacRateMismatch
        ));
        assert!(matches!(
            unindexed(3, 0),
            crate::FrameDefect::AacChannelsMismatch
        ));
        assert!(matches!(
            unindexed(3, 8),
            crate::FrameDefect::AacChannelsMismatch
        ));
    }

    /// A frame built for sending carries no timestamp, and its descriptor
    /// says so; a received frame keeps the timecode and timestamp the SDK
    /// delivered.
    #[test]
    fn received_frames_keep_their_timecode_and_timestamp() {
        let mut video = CompressedVideoFrame::new(h264_format(), h264_packet(false, &[])).unwrap();
        video.set_timecode(Timecode::from_ticks(17));
        assert_eq!(video.timestamp(), None);
        let raw = video.raw();
        assert_eq!(raw.timecode, 17);
        assert_eq!(raw.timestamp, NDIlib_recv_timestamp_undefined);
        let delivered = NDIlib_video_frame_v2_t {
            timestamp: 4242,
            ..raw
        };
        let received = unsafe { copy_video_packet(&delivered) }.unwrap();
        assert_eq!(received.timecode(), Timecode::from_ticks(17));
        assert_eq!(received.timestamp(), Some(SdkTimestamp::from_ticks(4242)));
        // Relaying a received frame hands the SDK what it delivered.
        assert_eq!(
            CompressedVideoFrame::try_from(received.clone())
                .unwrap()
                .raw()
                .timestamp,
            4242
        );
        let received = unsafe { copy_video_packet(&raw) }.unwrap();
        assert_eq!(received.timestamp(), None);

        let mut audio = CompressedAudioFrame::new(
            AudioPacketFormat::new(AudioCodec::Opus, 48_000, 2, 960).unwrap(),
            vec![1; 8],
        )
        .unwrap();
        audio.set_timecode(Timecode::from_ticks(-5));
        assert_eq!(audio.timecode(), Timecode::from_ticks(-5));
        let delivered = NDIlib_audio_frame_v3_t {
            timestamp: 99,
            ..audio.raw()
        };
        let received = unsafe { copy_audio_packet(&delivered) }.unwrap();
        assert_eq!(received.timecode(), Timecode::from_ticks(-5));
        assert_eq!(received.timestamp(), Some(SdkTimestamp::from_ticks(99)));
    }

    /// A clone owns its own copy of the bytes, so the SDK's writes to one
    /// never show in the other.
    #[test]
    fn a_cloned_frame_owns_a_copy_of_its_bytes() {
        let mut frame = CompressedVideoFrame::new(h264_format(), h264_packet(true, &[2])).unwrap();
        frame.set_metadata("<copy/>").unwrap();
        let mut copy = frame.clone();
        assert_eq!(copy.data(), frame.data());
        assert_ne!(copy.data().as_ptr(), frame.data().as_ptr());
        assert_eq!(copy.metadata(), Some("<copy/>"));
        let raw = copy.raw();
        // SAFETY: the descriptor lends the copy's own bytes, which hold a
        // complete header, and the copy is borrowed exclusively.
        unsafe { raw.p_data.add(RESERVED_OFFSET).write(7) };
        assert_eq!(copy.data()[RESERVED_OFFSET], 7);
        assert_eq!(frame.data()[RESERVED_OFFSET], 0);

        let mut audio = CompressedAudioFrame::new(
            AudioPacketFormat::new(AudioCodec::Opus, 48_000, 2, 960).unwrap(),
            vec![1; 8],
        )
        .unwrap();
        audio.format = audio.format.with_protected(true);
        let copy = audio.clone();
        assert_eq!(
            (
                copy.data(),
                copy.format().is_protected(),
                copy.format().num_samples()
            ),
            (audio.data(), true, 960)
        );
    }

    #[test]
    fn descriptors_keep_the_defaults_of_the_fields_they_do_not_set() {
        let video = h264_format().raw();
        assert_eq!(video.timecode, NDIlib_send_timecode_synthesize);
        assert!(video.p_data.is_null() && video.p_metadata.is_null());
        assert_eq!(video.timestamp, 0);

        let mut frame = CompressedAudioFrame::new(
            AudioPacketFormat::new(AudioCodec::Opus, 48_000, 2, 960).unwrap(),
            vec![1; 8],
        )
        .unwrap();
        let audio = frame.raw();
        assert_eq!(audio.timecode, NDIlib_send_timecode_synthesize);
        assert_eq!(audio.FourCC, AudioCodec::Opus.outer(false));
        // A frame built for sending has no timestamp; the SDK ignores the
        // field when it sends.
        assert_eq!(audio.timestamp, NDIlib_recv_timestamp_undefined);
        assert!(audio.p_metadata.is_null());
    }
    #[test]
    fn all_compressed_metadata_setters_return_the_original_text() {
        macro_rules! check {
            ($frame:expr) => {{
                let mut frame = $frame;
                frame.set_metadata("previous").unwrap();
                for text in [
                    "<ok/>\0tail".to_owned(),
                    "x".repeat(crate::frames::MAX_METADATA_BYTES),
                ] {
                    let pointer = text.as_ptr();
                    let capacity = text.capacity();
                    let oversized = text.len() == crate::frames::MAX_METADATA_BYTES;
                    let (result, allocations) =
                        crate::test_alloc::allocation_count(|| frame.set_metadata(text));
                    assert_eq!(allocations, if oversized { 1 } else { 2 });
                    let (error, text) = result.unwrap_err().into_parts();
                    assert_eq!(text.as_ptr(), pointer);
                    assert_eq!(text.capacity(), capacity);
                    assert!(matches!(
                        error,
                        Error::InvalidFrame(_) | Error::InvalidCString(_)
                    ));
                    assert_eq!(frame.metadata(), Some("previous"));
                    assert_eq!(frame.metadata_status(), crate::FrameMetadataStatus::Text);
                }
            }};
        }
        check!(CompressedVideoFrame::new(h264_format(), h264_packet(false, &[])).unwrap());
        check!(CompressedAudioFrame::new(
            AudioPacketFormat::new(AudioCodec::Opus, 48_000, 2, 960).unwrap(),
            vec![1, 2]
        )
        .unwrap());
        check!(super::super::send::ScatterVideoFrame::new(
            h264_format(),
            vec![h264_packet(false, &[])]
        )
        .unwrap());
        check!(super::super::send::ScatterAudioFrame::new(
            AudioPacketFormat::new(AudioCodec::Opus, 48_000, 2, 960).unwrap(),
            vec![vec![1, 2]]
        )
        .unwrap());
    }
}

#[cfg(any(ndi_test_packet_payload, ndi_test_packet_payload_mutation))]
#[path = "../../tests/ui/internal/packet_payload.rs"]
mod privacy_fixture;
