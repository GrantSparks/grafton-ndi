//! Compressed frames and the packet envelope that frames H.264, HEVC and AAC
//! payloads on the wire.

use std::ffi::c_char;

use super::{aliasable::AliasableVec, descriptors, invalid_frame, sdk_quantity};
use crate::{
    frames::FrameMetadata, sys::bindings::*, Error, FourCc, FrameRate, Rejected, Result, ScanType,
    SdkTimestamp, StreamTier, Timecode,
};

/// Maximum compressed frame size, in bytes, that this module sends or accepts.
pub const MAX_PACKET_BYTES: usize = 256 * 1024 * 1024;

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

impl VideoCodec {
    const ALL: [Self; 7] = [
        Self::SpeedHq420,
        Self::SpeedHq422,
        Self::SpeedHq422Alpha,
        Self::H264,
        Self::Hevc,
        Self::H264Alpha,
        Self::HevcAlpha,
    ];

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

impl EnvelopeCodec {
    /// The FourCC the envelope header carries for this codec.
    pub const fn fourcc(self) -> FourCc {
        FourCc::from_raw(match self {
            Self::H264 => fourcc(b"H264"),
            Self::Hevc => fourcc(b"HEVC"),
            Self::Aac => 0xff,
        })
    }

    /// The codec an envelope header's FourCC names, if this module knows it.
    pub fn from_fourcc(fourcc: FourCc) -> Option<Self> {
        [Self::H264, Self::Hevc, Self::Aac]
            .into_iter()
            .find(|codec| codec.fourcc() == fourcc)
    }
}

/// The fixed fields of an envelope header, read in native byte order.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EnvelopeHeader {
    size: usize,
    codec: FourCc,
    pts: i64,
    dts: i64,
    reserved: u64,
    flags: u32,
    data_len: usize,
    extra_len: usize,
}

impl EnvelopeHeader {
    /// Read the header at the start of `bytes`.
    pub(crate) fn read(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < ENVELOPE_HEADER_BYTES {
            return Err(invalid_frame("Truncated compressed packet header"));
        }
        let size = usize::try_from(i32::from_ne_bytes(field(bytes, VERSION_OFFSET)))
            .ok()
            .filter(|size| *size >= ENVELOPE_HEADER_BYTES)
            .ok_or_else(|| invalid_frame("Unsupported compressed packet header size"))?;
        Ok(Self {
            size,
            codec: FourCc::from_raw(u32::from_ne_bytes(field(bytes, CODEC_OFFSET))),
            pts: i64::from_ne_bytes(field(bytes, PTS_OFFSET)),
            dts: i64::from_ne_bytes(field(bytes, DTS_OFFSET)),
            reserved: u64::from_ne_bytes(field(bytes, RESERVED_OFFSET)),
            flags: u32::from_ne_bytes(field(bytes, FLAGS_OFFSET)),
            data_len: u32::from_ne_bytes(field(bytes, DATA_SIZE_OFFSET)) as usize,
            extra_len: u32::from_ne_bytes(field(bytes, EXTRA_DATA_SIZE_OFFSET)) as usize,
        })
    }

    pub(crate) fn is_keyframe(&self) -> bool {
        self.flags & KEYFRAME != 0
    }

    /// Header, payload and codec configuration together, if that does not overflow.
    pub(crate) fn total_len(&self) -> Option<usize> {
        self.size
            .checked_add(self.data_len)?
            .checked_add(self.extra_len)
    }

    /// Where the codec configuration starts, counted from the header's first
    /// byte.
    pub(crate) fn extra_offset(&self) -> usize {
        self.size + self.data_len
    }

    /// Check that this header begins a packet of `codec`, `total` bytes long,
    /// that this module can send: the header version and flags it knows, a
    /// payload, and lengths that account for every byte.
    ///
    /// The reserved field takes no part. It belongs to the SDK, which
    /// overwrites it in every packet it sends, so a packet that has been sent
    /// or received carries whatever the SDK left there.
    pub(crate) fn validate_for_send(&self, codec: EnvelopeCodec, total: usize) -> Result<()> {
        if self.size != ENVELOPE_HEADER_BYTES || self.flags & !KEYFRAME != 0 {
            return Err(invalid_frame(
                "Cannot send a packet with an unsupported header version or unknown flags",
            ));
        }
        if self.codec != codec.fourcc() {
            return Err(invalid_frame(
                "Packet codec does not match the frame's codec",
            ));
        }
        if self.data_len == 0 {
            return Err(invalid_frame("Empty codec payload"));
        }
        if self.total_len() != Some(total) {
            return Err(invalid_frame(
                "Packet header lengths do not match the size of the frame",
            ));
        }
        Ok(())
    }

    /// Check the codec configuration of a video packet that is about to be
    /// sent: a decoder cannot start on a keyframe without its parameter sets.
    pub(crate) fn validate_video_configuration(&self) -> Result<()> {
        if self.is_keyframe() && self.extra_len == 0 {
            return Err(invalid_frame("Keyframes require codec configuration data"));
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
        parameters: AudioParameters,
    ) -> Result<()> {
        if !self.is_keyframe() {
            return Err(invalid_frame("AAC packets must carry the keyframe flag"));
        }
        let Some(config) = config else {
            return Err(invalid_frame(
                "AAC packets require their AudioSpecificConfig as codec configuration data",
            ));
        };
        // Five bits of object type, four of sampling-frequency index, four of
        // channel configuration.
        let bits = u16::from_be_bytes(config);
        let frequency_index = usize::from((bits >> 7) & 0xf);
        let channel_configuration = usize::from((bits >> 3) & 0xf);
        if AAC_SAMPLE_RATES.get(frequency_index) != Some(&parameters.sample_rate) {
            return Err(invalid_frame(
                "The AAC configuration names a different sample rate than the frame",
            ));
        }
        if channel_configuration
            .checked_sub(1)
            .and_then(|index| AAC_CHANNELS.get(index))
            != Some(&parameters.num_channels)
        {
            return Err(invalid_frame(
                "The AAC configuration names a different channel layout than the frame",
            ));
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
#[derive(Clone, Copy, Debug)]
pub struct PacketEnvelope<'a> {
    bytes: &'a [u8],
    header: EnvelopeHeader,
}

impl<'a> PacketEnvelope<'a> {
    /// Parse `bytes` as one packet envelope.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when the slice is shorter than a header, longer
    /// than [`MAX_PACKET_BYTES`], or shorter than the lengths its header
    /// declares.
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() > MAX_PACKET_BYTES {
            return Err(invalid_frame("Invalid compressed packet length"));
        }
        let header = EnvelopeHeader::read(bytes)?;
        let end = header
            .total_len()
            .ok_or_else(|| invalid_frame("Compressed packet length overflow"))?;
        if end > bytes.len() {
            return Err(invalid_frame("Truncated compressed packet"));
        }
        Ok(Self { bytes, header })
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
            .ok_or_else(|| invalid_frame("Packet size overflow"))?;
        if data.is_empty() || size > MAX_PACKET_BYTES {
            return Err(invalid_frame("Invalid compressed packet payload size"));
        }
        let mut bytes = vec![0; size];
        let mut put = |offset: usize, value: &[u8]| {
            bytes[offset..offset + value.len()].copy_from_slice(value);
        };
        put(
            VERSION_OFFSET,
            &(ENVELOPE_HEADER_BYTES as i32).to_ne_bytes(),
        );
        put(CODEC_OFFSET, &codec.fourcc().raw().to_ne_bytes());
        put(PTS_OFFSET, &presentation_time.to_ne_bytes());
        put(DTS_OFFSET, &decode_time.to_ne_bytes());
        // The SDK delivers no AAC packet without the flag.
        let keyframe = keyframe || codec == EnvelopeCodec::Aac;
        put(
            FLAGS_OFFSET,
            &(if keyframe { KEYFRAME } else { 0 }).to_ne_bytes(),
        );
        // The size check above bounds both lengths well below u32::MAX.
        put(DATA_SIZE_OFFSET, &(data.len() as u32).to_ne_bytes());
        put(
            EXTRA_DATA_SIZE_OFFSET,
            &(extra_data.len() as u32).to_ne_bytes(),
        );
        put(ENVELOPE_HEADER_BYTES, data);
        put(ENVELOPE_HEADER_BYTES + data.len(), extra_data);
        Ok(bytes)
    }

    /// The codec, when the header names one this module knows.
    pub fn codec(self) -> Option<EnvelopeCodec> {
        EnvelopeCodec::from_fourcc(self.header.codec)
    }

    /// The codec's FourCC exactly as the header carries it.
    pub fn fourcc(self) -> FourCc {
        self.header.codec
    }

    /// Presentation timestamp, in the stream's own units.
    pub fn presentation_time(self) -> i64 {
        self.header.pts
    }

    /// Decode timestamp, in the stream's own units.
    pub fn decode_time(self) -> i64 {
        self.header.dts
    }

    /// Whether the keyframe flag is set.
    pub fn is_keyframe(self) -> bool {
        self.header.is_keyframe()
    }

    /// The header's flags field, every bit as the packet carries it,
    /// including bits this module does not interpret.
    /// [`is_keyframe`](Self::is_keyframe) reads the one it does.
    pub fn flags(self) -> u32 {
        self.header.flags
    }

    /// The header's reserved field.
    ///
    /// The field belongs to the SDK, which overwrites it in every packet it
    /// sends. Its value says nothing about whether a packet can be sent.
    pub fn reserved(self) -> u64 {
        self.header.reserved
    }

    /// Header bytes beyond the version-0 fields. Empty for version 0.
    pub fn header_extension(self) -> &'a [u8] {
        &self.bytes[ENVELOPE_HEADER_BYTES..self.header.size]
    }

    /// The codec payload.
    pub fn data(self) -> &'a [u8] {
        let start = self.header.size;
        &self.bytes[start..start + self.header.data_len]
    }

    /// Codec configuration data (parameter sets, or the AAC configuration),
    /// which follows the payload. Empty when the packet carries none.
    pub fn extra_data(self) -> &'a [u8] {
        let start = self.header.size + self.header.data_len;
        &self.bytes[start..start + self.header.extra_len]
    }

    /// Bytes after the lengths the header declares.
    pub fn trailing_bytes(self) -> &'a [u8] {
        &self.bytes[self.header.size + self.header.data_len + self.header.extra_len..]
    }

    /// Whether the header is exactly version 0, the version this module writes.
    pub fn is_known_version(self) -> bool {
        self.header.size == ENVELOPE_HEADER_BYTES
    }
}

/// A picture aspect ratio this module accepts: finite and not negative.
fn checked_aspect_ratio(ratio: f32) -> std::result::Result<f32, String> {
    if ratio.is_finite() && ratio >= 0.0 {
        Ok(ratio)
    } else {
        Err(format!(
            "Picture aspect ratio {ratio} must be finite and not negative; zero selects square pixels"
        ))
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoPacketFormat {
    codec: VideoCodec,
    tier: StreamTier,
    protected: bool,
    // Both in 1..=i32::MAX, the range of the SDK's signed fields.
    width: u32,
    height: u32,
    frame_rate: FrameRate,
    picture_aspect_ratio: f32,
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
        Self::checked(codec, width.into(), height.into()).map_err(Error::InvalidInput)
    }

    /// The one dimension check, for values from the API and from the SDK.
    /// The error is the reason, for the caller to report as its own variant.
    fn checked(codec: VideoCodec, width: i64, height: i64) -> std::result::Result<Self, String> {
        match (sdk_quantity(width), sdk_quantity(height)) {
            (Some(width), Some(height)) => Ok(Self {
                codec,
                tier: StreamTier::Program,
                protected: false,
                width,
                height,
                frame_rate: crate::media::DEFAULT_VIDEO_FRAME_RATE,
                picture_aspect_ratio: 0.0,
                scan_type: ScanType::Progressive,
            }),
            _ => Err(format!(
                "Compressed video dimensions {width}x{height} must each be in 1..={}",
                i32::MAX
            )),
        }
    }

    /// The format of a compressed frame the SDK delivered; a value out of
    /// range makes the frame invalid.
    pub(crate) fn from_sdk(raw: &NDIlib_video_frame_v2_t) -> Result<Self> {
        let (codec, tier, protected) = VideoCodec::from_outer(raw.FourCC)
            .ok_or_else(|| invalid_frame("Unknown compressed video FourCC"))?;
        let format =
            Self::checked(codec, raw.xres.into(), raw.yres.into()).map_err(Error::InvalidFrame)?;
        Ok(Self {
            tier,
            protected,
            frame_rate: FrameRate::from_sdk(raw.frame_rate_N, raw.frame_rate_D)?,
            picture_aspect_ratio: checked_aspect_ratio(raw.picture_aspect_ratio)
                .map_err(Error::InvalidFrame)?,
            scan_type: ScanType::from_sdk(raw.frame_format_type)
                .ok_or_else(|| invalid_frame("Unknown packet scan type"))?,
            ..format
        })
    }

    /// The same format at `frame_rate`.
    #[must_use]
    pub fn with_frame_rate(self, frame_rate: FrameRate) -> Self {
        Self { frame_rate, ..self }
    }

    /// The same format with picture aspect ratio `ratio` (width over height
    /// of the displayed picture). `0.0` means square pixels, as the SDK
    /// defines it; a receiver is then given the ratio of the frame's width to
    /// its height.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the value is negative or not finite.
    pub fn with_picture_aspect_ratio(self, ratio: f32) -> Result<Self> {
        let picture_aspect_ratio = checked_aspect_ratio(ratio).map_err(Error::InvalidInput)?;
        Ok(Self {
            picture_aspect_ratio,
            ..self
        })
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
        self.width
    }

    /// Height in pixels.
    pub fn height(self) -> u32 {
        self.height
    }

    /// The frame rate.
    pub fn frame_rate(self) -> FrameRate {
        self.frame_rate
    }

    /// The picture aspect ratio; `0.0` means square pixels.
    pub fn picture_aspect_ratio(self) -> f32 {
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
    pub(crate) fn validate_for_send(self, header: &[u8], total: usize) -> Result<()> {
        let Some(codec) = self.codec.envelope_codec() else {
            if header
                .first()
                .is_none_or(|quality| *quality >= SPEEDHQ_QUALITY_LIMIT)
            {
                return Err(invalid_frame(
                    "A SpeedHQ frame begins with its quality, a byte below 100",
                ));
            }
            return Ok(());
        };
        let header = EnvelopeHeader::read(header)?;
        header.validate_for_send(codec, total)?;
        header.validate_video_configuration()?;
        if self.tier == StreamTier::Preview
            && (self.width > MAX_PREVIEW_DIMENSION || self.height > MAX_PREVIEW_DIMENSION)
        {
            return Err(invalid_frame(
                "An H.264 or HEVC picture on the preview stream is at most 640 pixels wide and 640 high",
            ));
        }
        Ok(())
    }

    /// A native frame descriptor with this format and no data, no timecode
    /// and no metadata.
    pub(crate) fn raw(self) -> NDIlib_video_frame_v2_t {
        let (numerator, denominator) = self.frame_rate.to_sdk();
        NDIlib_video_frame_v2_t {
            // Lossless: both dimensions are at most i32::MAX.
            xres: self.width as i32,
            yres: self.height as i32,
            FourCC: self.codec.outer(self.tier, self.protected),
            frame_rate_N: numerator,
            frame_rate_D: denominator,
            picture_aspect_ratio: self.picture_aspect_ratio,
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
    metadata: Option<FrameMetadata>,
}

impl FrameExtras {
    /// The values the SDK delivered with a received frame.
    ///
    /// # Safety
    ///
    /// `metadata` is null or readable through its first NUL or the metadata cap.
    #[cfg(test)]
    unsafe fn received(timecode: i64, timestamp: i64, metadata: *const c_char) -> Result<Self> {
        Ok(Self {
            timecode: Timecode::from_ticks(timecode),
            timestamp: SdkTimestamp::from_sdk(timestamp),
            // SAFETY: forwarded to the caller's obligation.
            metadata: unsafe { crate::frames::FrameMetadataState::scan(metadata).text(metadata) }
                .map(|text| FrameMetadata::copy_from(text, None)),
        })
    }

    /// Reuse storage while copying already validated borrowed extras.
    fn copy_received(
        &mut self,
        timecode: Timecode,
        timestamp: Option<SdkTimestamp>,
        text: Option<crate::frames::MetadataText<'_>>,
    ) {
        match text {
            Some(text) => {
                self.metadata = Some(FrameMetadata::copy_from(text, self.metadata.take()))
            }
            None => {
                if let Some(storage) = &mut self.metadata {
                    storage.clear();
                }
            }
        }
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
            .as_ref()
            .and_then(FrameMetadata::optional_text)
            .map(crate::frames::MetadataText::as_str)
    }

    pub(crate) fn set_timecode(&mut self, timecode: Timecode) {
        self.timecode = timecode;
    }

    pub(crate) fn set_metadata(&mut self, metadata: impl Into<String>) -> Result<()> {
        self.metadata = Some(FrameMetadata::new(metadata)?);
        Ok(())
    }

    pub(crate) fn clear_metadata(&mut self) {
        self.metadata = None;
    }

    /// Bytes the metadata keeps allocated, including terminator and spare capacity.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.metadata
            .as_ref()
            .map_or(0, FrameMetadata::retained_bytes)
    }

    /// The timecode, timestamp and metadata fields of a native descriptor.
    /// The metadata pointer is null without metadata, and valid for as long
    /// as these extras are.
    fn to_sdk(&self) -> (i64, i64, *const c_char) {
        (
            self.timecode.ticks(),
            SdkTimestamp::to_sdk(self.timestamp),
            self.metadata
                .as_ref()
                .map_or(std::ptr::null(), |metadata| metadata.as_ptr()),
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

        /// Replace frame metadata. Use [`Self::clear_metadata`] to remove it.
        ///
        /// Invalid replacement text leaves the existing metadata unchanged.
        ///
        /// # Errors
        ///
        /// Returns [`crate::Error::InvalidCString`] for an interior NUL, or
        /// [`crate::Error::InvalidFrame`] when the emitted C string would
        /// exceed the metadata size cap.
        pub fn set_metadata(&mut self, metadata: impl Into<String>) -> $crate::Result<()> {
            self.extras.set_metadata(metadata)
        }

        /// Remove the frame metadata.
        pub fn clear_metadata(&mut self) {
            self.extras.clear_metadata();
        }
    };
}
pub(crate) use frame_extras_accessors;

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
#[derive(Clone, Debug)]
pub struct CompressedVideoFrame {
    pub(crate) format: VideoPacketFormat,
    pub(crate) bytes: AliasableVec<u8>,
    pub(crate) extras: FrameExtras,
}

impl CompressedVideoFrame {
    frame_extras_accessors!();

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
        match validate_video_packet(format, &bytes, Use::Send) {
            Ok(()) => Ok(Self {
                format,
                bytes: AliasableVec::new(bytes),
                extras: FrameExtras::default(),
            }),
            Err(error) => Err(Rejected::new(error, bytes)),
        }
    }

    /// The codec bytes.
    pub fn data(&self) -> &[u8] {
        &self.bytes
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
        self.bytes.into_vec()
    }

    /// Check that this frame can be sent.
    ///
    /// A frame built with [`new`](Self::new) always can, before and after it
    /// has been sent. A received frame may carry an envelope from a later SDK
    /// that this module keeps but will not send, or be one the SDK does not
    /// deliver when it is sent (see [`VideoCodec`]).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] with the reason.
    pub fn validate_for_send(&self) -> Result<()> {
        validate_video_packet(self.format, &self.bytes, Use::Send)
    }

    /// Bytes this frame keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.bytes.capacity() + self.extras.retained_bytes()
    }

    /// A native descriptor that lends this frame's bytes and metadata to the
    /// SDK. The SDK may write to the bytes for as long as it holds the
    /// descriptor, so the caller keeps the frame exclusively for that long.
    pub(crate) fn raw(&mut self) -> NDIlib_video_frame_v2_t {
        let mut raw = self.format.descriptor(&self.extras);
        // Construction bounds the length by MAX_PACKET_BYTES, below i32::MAX.
        raw.__bindgen_anon_1.data_size_in_bytes = self.bytes.len() as i32;
        raw.p_data = self.bytes.as_mut_ptr().cast();
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

pub(crate) fn validate_video_packet(
    format: VideoPacketFormat,
    bytes: &[u8],
    purpose: Use,
) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_PACKET_BYTES {
        return Err(invalid_frame("Invalid compressed video size"));
    }
    match format.codec.envelope_codec() {
        Some(codec) => {
            let packet = PacketEnvelope::parse(bytes)?;
            if packet.codec() != Some(codec) {
                return Err(invalid_frame(
                    "Packet codec does not match video descriptor",
                ));
            }
            if packet.data().is_empty() {
                return Err(invalid_frame("Empty codec payload"));
            }
        }
        None if bytes.len() < SPEEDHQ_HEADER_BYTES => {
            return Err(invalid_frame("Truncated SpeedHQ frame"));
        }
        None => {}
    }
    match purpose {
        Use::Send => format.validate_for_send(bytes, bytes.len()),
        Use::Receive => Ok(()),
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

/// The sample rate, channel count and samples per channel of one compressed
/// audio frame, each in `1..=i32::MAX`, the range of the SDK's signed fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AudioParameters {
    pub(crate) sample_rate: u32,
    pub(crate) num_channels: u32,
    pub(crate) num_samples: u32,
}

impl AudioParameters {
    /// The parameters of a frame being built.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] unless each is in `1..=i32::MAX`.
    pub(crate) fn new(sample_rate: u32, num_channels: u32, num_samples: u32) -> Result<Self> {
        Self::checked(sample_rate.into(), num_channels.into(), num_samples.into())
    }

    /// The parameters of a frame the SDK delivered.
    fn from_sdk(raw: &NDIlib_audio_frame_v3_t) -> Result<Self> {
        Self::checked(
            raw.sample_rate.into(),
            raw.no_channels.into(),
            raw.no_samples.into(),
        )
    }

    /// The one check, for values from the API and from the SDK.
    fn checked(sample_rate: i64, num_channels: i64, num_samples: i64) -> Result<Self> {
        match (
            sdk_quantity(sample_rate),
            sdk_quantity(num_channels),
            sdk_quantity(num_samples),
        ) {
            (Some(sample_rate), Some(num_channels), Some(num_samples)) => Ok(Self {
                sample_rate,
                num_channels,
                num_samples,
            }),
            _ => Err(invalid_frame(format!(
                "Compressed audio of {sample_rate} Hz, {num_channels} channels and \
                 {num_samples} samples per channel: each must be in 1..={}",
                i32::MAX
            ))),
        }
    }

    /// A native descriptor of a compressed audio frame of `codec` with these
    /// parameters and `extras`, without data.
    pub(crate) fn descriptor(
        self,
        codec: AudioCodec,
        protected: bool,
        extras: &FrameExtras,
    ) -> NDIlib_audio_frame_v3_t {
        let (timecode, timestamp, p_metadata) = extras.to_sdk();
        NDIlib_audio_frame_v3_t {
            // Lossless: each is at most i32::MAX.
            sample_rate: self.sample_rate as i32,
            no_channels: self.num_channels as i32,
            no_samples: self.num_samples as i32,
            FourCC: codec.outer(protected),
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
        parameters: AudioParameters,
        header: &[u8],
        total: usize,
        bytes_at: impl FnOnce(usize) -> Option<[u8; 2]>,
    ) -> Result<()> {
        self.validate_delivery(parameters)?;
        match self {
            Self::Opus => Ok(()),
            Self::Aac => {
                let header = EnvelopeHeader::read(header)?;
                header.validate_for_send(EnvelopeCodec::Aac, total)?;
                header.validate_aac(bytes_at(header.extra_offset()), parameters)
            }
        }
    }

    /// Check the parameters a frame of this codec is delivered with; see
    /// [`AudioCodec`]. An AAC frame's configuration is checked separately,
    /// against its envelope.
    fn validate_delivery(self, parameters: AudioParameters) -> Result<()> {
        let AudioParameters {
            sample_rate,
            num_channels,
            num_samples,
        } = parameters;
        match self {
            Self::Opus => {
                if !OPUS_SAMPLE_RATES.contains(&sample_rate) {
                    return Err(invalid_frame(
                        "Opus is sent at 8000, 12000, 16000, 24000 or 48000 Hz",
                    ));
                }
                // One unit is 2.5 ms, which is a four-hundredth of a second.
                let units = u64::from(num_samples) * 400;
                if !OPUS_FRAME_DURATIONS
                    .iter()
                    .any(|duration| units == duration * u64::from(sample_rate))
                {
                    return Err(invalid_frame(
                        "An Opus frame holds 2.5, 5, 10, 20, 40 or 60 ms of audio",
                    ));
                }
            }
            Self::Aac => {
                if num_samples != AAC_FRAME_SAMPLES {
                    return Err(invalid_frame("An AAC frame holds 1024 samples per channel"));
                }
                if !AAC_SAMPLE_RATES.contains(&sample_rate) {
                    return Err(invalid_frame(
                        "AAC is sent at a sample rate its configuration can name, from 7350 to 96000 Hz",
                    ));
                }
                if !AAC_CHANNELS.contains(&num_channels) {
                    return Err(invalid_frame(
                        "AAC is sent with one to six or eight channels",
                    ));
                }
            }
        }
        Ok(())
    }

    /// The FourCC a frame of this codec is sent with.
    pub(crate) const fn outer(self, protected: bool) -> u32 {
        (match self {
            Self::Aac => EnvelopeCodec::Aac.fourcc().raw(),
            Self::Opus => fourcc(b"Opus"),
        }) | if protected { PROTECTED } else { 0 }
    }

    /// The codec and protection a frame FourCC names, if it is compressed
    /// audio this module knows.
    pub(crate) fn from_outer(value: u32) -> Option<(Self, bool)> {
        [Self::Aac, Self::Opus].into_iter().find_map(|codec| {
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
#[derive(Clone, Debug)]
pub struct CompressedAudioFrame {
    pub(crate) codec: AudioCodec,
    pub(crate) protected: bool,
    pub(crate) bytes: AliasableVec<u8>,
    pub(crate) parameters: AudioParameters,
    pub(crate) extras: FrameExtras,
}

impl CompressedAudioFrame {
    frame_extras_accessors!();

    /// Take ownership of one audio frame's bytes: a packet envelope for AAC
    /// (see [`PacketEnvelope::encode`]), one bare packet for Opus.
    /// `num_samples` is the number of samples per channel the frame decodes
    /// to.
    ///
    /// # Errors
    ///
    /// Returns the bytes with [`Error::InvalidFrame`] when a parameter is not
    /// in `1..=i32::MAX`, the bytes are empty or too large, the parameters are
    /// not ones the codec is delivered with (see [`AudioCodec`]), or an AAC
    /// envelope is malformed or disagrees with them.
    pub fn new(
        codec: AudioCodec,
        sample_rate: u32,
        num_channels: u32,
        num_samples: u32,
        bytes: Vec<u8>,
    ) -> std::result::Result<Self, Rejected<Vec<u8>>> {
        let parameters = match AudioParameters::new(sample_rate, num_channels, num_samples) {
            Ok(parameters) => parameters,
            Err(error) => return Err(Rejected::new(error, bytes)),
        };
        let frame = Self {
            codec,
            protected: false,
            bytes: AliasableVec::new(bytes),
            parameters,
            extras: FrameExtras::default(),
        };
        match frame.validate_for_send() {
            Ok(()) => Ok(frame),
            Err(error) => Err(Rejected::new(error, frame.into_data())),
        }
    }

    /// Set whether the frame belongs to a protected stream.
    pub fn set_protected(&mut self, protected: bool) {
        self.protected = protected;
    }

    /// The codec bytes.
    pub fn data(&self) -> &[u8] {
        &self.bytes
    }

    /// The codec.
    pub fn codec(&self) -> AudioCodec {
        self.codec
    }

    /// Whether the stream is protected.
    pub fn is_protected(&self) -> bool {
        self.protected
    }

    /// Sample rate in hertz.
    pub fn sample_rate(&self) -> u32 {
        self.parameters.sample_rate
    }

    /// Number of channels.
    pub fn num_channels(&self) -> u32 {
        self.parameters.num_channels
    }

    /// Samples per channel the frame decodes to.
    pub fn num_samples(&self) -> u32 {
        self.parameters.num_samples
    }

    /// When the sender submitted a received frame, as the SDK recorded it;
    /// `None` on a frame built for sending.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        self.extras.timestamp()
    }

    /// Take the codec bytes, retaining the original allocation and capacity.
    pub fn into_data(self) -> Vec<u8> {
        self.bytes.into_vec()
    }

    /// Check that this frame can be sent.
    ///
    /// A frame built with [`new`](Self::new) always can, before and after it
    /// has been sent. A received frame may have parameters the SDK does not
    /// deliver when they are sent (see [`AudioCodec`]), or an AAC envelope
    /// from a later SDK that this module keeps but will not send.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] with the reason.
    pub fn validate_for_send(&self) -> Result<()> {
        self.validate()?;
        let bytes = &*self.bytes;
        self.codec
            .validate_for_send(self.parameters, bytes, bytes.len(), |offset| {
                bytes.get(offset..)?.first_chunk().copied()
            })
    }

    /// The checks that hold for received frames as well.
    pub(crate) fn validate(&self) -> Result<()> {
        validate_received_audio(self.codec, &self.bytes)
    }

    /// A native descriptor that lends this frame's bytes and metadata to the
    /// SDK; see [`CompressedVideoFrame::raw`].
    pub(crate) fn raw(&mut self) -> NDIlib_audio_frame_v3_t {
        let mut raw = self
            .parameters
            .descriptor(self.codec, self.protected, &self.extras);
        raw.p_data = self.bytes.as_mut_ptr().cast();
        // Validation bounds the length by MAX_PACKET_BYTES, below i32::MAX.
        raw.__bindgen_anon_1.data_size_in_bytes = self.bytes.len() as i32;
        raw
    }
}

fn validate_received_audio(codec: AudioCodec, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_PACKET_BYTES {
        return Err(invalid_frame("Invalid compressed audio size"));
    }
    if codec == AudioCodec::Aac {
        let packet = PacketEnvelope::parse(bytes)?;
        if packet.codec() != Some(EnvelopeCodec::Aac) || packet.data().is_empty() {
            return Err(invalid_frame("Invalid AAC packet envelope"));
        }
    }
    Ok(())
}

#[path = "packets/borrowed.rs"]
mod borrowed;
pub use borrowed::{CompressedAudioRef, CompressedVideoRef};

/// The bytes of a compressed native frame.
///
/// # Safety
///
/// When `data` is not null it is readable for `size` bytes.
unsafe fn packet_bytes<'a>(data: *const u8, size: i32) -> Result<&'a [u8]> {
    let size = usize::try_from(size)
        .ok()
        .filter(|size| !data.is_null() && (1..=MAX_PACKET_BYTES).contains(size))
        .ok_or_else(|| invalid_frame("Invalid compressed frame buffer"))?;
    // SAFETY: non-null and a positive in-range size were checked; the caller
    // guarantees readability for that size.
    Ok(unsafe { std::slice::from_raw_parts(data, size) })
}

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
) -> Result<CompressedVideoFrame> {
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
) -> Result<CompressedAudioFrame> {
    // SAFETY: as for the video fixture above.
    let guard =
        unsafe { crate::capture::Guard::with_test_free(std::ptr::null_mut(), *raw, |_, _| {}) };
    CompressedAudioRef::new(guard).map(|frame| frame.to_owned())
}

// The values above are the NDI wire format, which the data types in this file
// implement in every build. Where the build has the Advanced SDK declarations,
// each value is checked against them.
#[cfg(ndi_has_advanced_header)]
const _: () = {
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
        EnvelopeCodec::H264.fourcc().raw(),
        NDIlib_compressed_FourCC_type_e_NDIlib_compressed_FourCC_type_H264
    );
    same_bits!(
        EnvelopeCodec::Hevc.fourcc().raw(),
        NDIlib_compressed_FourCC_type_e_NDIlib_compressed_FourCC_type_HEVC
    );
    same_bits!(
        EnvelopeCodec::Aac.fourcc().raw(),
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
};

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

    fn h264_format() -> VideoPacketFormat {
        VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap()
    }

    fn h264_packet(keyframe: bool, extra_data: &[u8]) -> Vec<u8> {
        PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, keyframe, &[1], extra_data).unwrap()
    }

    fn aac(sample_rate: u32, channels: u32, samples: u32, packet: Vec<u8>) -> Result<()> {
        CompressedAudioFrame::new(AudioCodec::Aac, sample_rate, channels, samples, packet)
            .map(drop)
            .map_err(Rejected::into_error)
    }

    fn opus(sample_rate: u32, channels: u32, samples: u32) -> Result<()> {
        CompressedAudioFrame::new(AudioCodec::Opus, sample_rate, channels, samples, vec![1; 8])
            .map(drop)
            .map_err(Rejected::into_error)
    }

    /// The text of an invalid-frame error.
    fn reason(result: Result<()>) -> String {
        match result {
            Err(Error::InvalidFrame(reason)) => reason,
            other => panic!("expected an invalid frame, got {other:?}"),
        }
    }

    #[test]
    fn packed_header_parsing_preserves_extensions_and_checks_bounds() {
        let mut bytes =
            PacketEnvelope::encode(EnvelopeCodec::H264, 12, -4, true, &[1, 2], &[3]).unwrap();
        let p = PacketEnvelope::parse(&bytes).unwrap();
        assert_eq!(p.codec(), Some(EnvelopeCodec::H264));
        assert_eq!(p.fourcc(), EnvelopeCodec::H264.fourcc());
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
        for codec in [EnvelopeCodec::H264, EnvelopeCodec::Hevc, EnvelopeCodec::Aac] {
            assert_eq!(EnvelopeCodec::from_fourcc(codec.fourcc()), Some(codec));
        }
    }

    #[test]
    fn codec_outer_roundtrips_and_opus_is_not_an_envelope() {
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
                Err(Error::InvalidInput(reason)) if reason.contains("1..=")
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
        assert_eq!(format.picture_aspect_ratio(), 0.0);
        assert_eq!(
            format
                .with_picture_aspect_ratio(0.0)
                .unwrap()
                .picture_aspect_ratio(),
            0.0
        );
        for ratio in [-1.0, f32::NAN, f32::INFINITY] {
            assert!(matches!(
                format.with_picture_aspect_ratio(ratio),
                Err(Error::InvalidInput(_))
            ));
        }

        // A received frame whose descriptor leaves the ratio unset is kept
        // as it is.
        let mut frame = CompressedVideoFrame::new(format, h264_packet(false, &[])).unwrap();
        let raw = frame.raw();
        assert_eq!(raw.picture_aspect_ratio, 0.0);
        let received = unsafe { copy_video_packet(&raw) }.unwrap();
        assert_eq!(received.format().picture_aspect_ratio(), 0.0);

        let wide = NDIlib_video_frame_v2_t {
            picture_aspect_ratio: 16.0 / 9.0,
            ..raw
        };
        let received = unsafe { copy_video_packet(&wide) }.unwrap();
        assert_eq!(received.format().picture_aspect_ratio(), 16.0 / 9.0);

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

        for (sample_rate, channels) in [(0, 2), (48_000, i32::MAX.unsigned_abs() + 1)] {
            let rejected =
                CompressedAudioFrame::new(AudioCodec::Opus, sample_rate, channels, 960, vec![1, 2])
                    .unwrap_err();
            assert!(
                matches!(rejected.error(), Error::InvalidFrame(reason) if reason.contains("1..="))
            );
            assert_eq!(rejected.into_inner(), [1, 2]);
        }

        let mut frame = CompressedVideoFrame::new(
            h264_format().with_tier(StreamTier::Preview),
            h264_packet(false, &[]),
        )
        .unwrap();
        assert!(matches!(
            frame.set_metadata("nul\0inside"),
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
            assert!(absent.to_sdk().2.is_null());
            for text in [c"", c"<received/>"] {
                let extras = FrameExtras::received(123, 456, text.as_ptr()).unwrap();
                assert_eq!(extras.metadata(), Some(text.to_str().unwrap()));
                assert_eq!(extras.retained_bytes(), text.to_bytes_with_nul().len());
                assert_eq!(std::ffi::CStr::from_ptr(extras.to_sdk().2), text);
                assert_eq!(extras.timecode().ticks(), 123);
                assert_eq!(extras.timestamp().unwrap().ticks(), 456);
            }
            let unterminated = vec![b'x'; crate::frames::MAX_METADATA_BYTES];
            for pointer in [c"\xff".as_ptr(), unterminated.as_ptr().cast()] {
                let (extras, allocations) = crate::test_alloc::allocation_count(|| {
                    FrameExtras::received(123, 456, pointer).unwrap()
                });
                assert_eq!(allocations, 0);
                assert_eq!(extras.metadata(), None);
                assert!(extras.to_sdk().2.is_null());
            }
        }
    }

    #[test]
    fn protected_aac_preserves_codec_high_bit() {
        let bytes = aac_packet(48_000, 2, &[1]);
        let mut frame = CompressedAudioFrame::new(AudioCodec::Aac, 48_000, 2, 1024, bytes).unwrap();
        frame.set_protected(true);
        let raw = frame.raw();
        let copied = unsafe { copy_audio_packet(&raw) }.unwrap();
        assert_eq!(copied.codec(), AudioCodec::Aac);
        assert!(copied.is_protected());
        assert_eq!(
            (
                copied.sample_rate(),
                copied.num_channels(),
                copied.num_samples()
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
        let mut frame =
            CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![1; 8]).unwrap();
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
            frame.validate_for_send().unwrap();

            let mut audio = aac_packet(48_000, 2, &[1]);
            stamp(&mut audio, value);
            let frame = CompressedAudioFrame::new(AudioCodec::Aac, 48_000, 2, 1024, audio).unwrap();
            frame.validate_for_send().unwrap();
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
            frame.validate_for_send().unwrap();
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
                    assert!(reason(video(format, vec![quality, 0, 0, 0])).contains("quality"));
                    // A received frame is kept whatever it holds.
                    assert!(
                        validate_video_packet(format, &[quality, 0, 0, 0], Use::Receive).is_ok()
                    );
                }
                assert!(reason(video(format, vec![0; 3])).contains("Truncated"));
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
                assert!(reason(preview(width, height)).contains("640 pixels"));
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
        let received = CompressedAudioFrame {
            codec: AudioCodec::Aac,
            protected: false,
            bytes: AliasableVec::new(packet.clone()),
            parameters: AudioParameters::new(48_000, 2, 1024).unwrap(),
            extras: FrameExtras::default(),
        };
        assert!(received.validate().is_ok());
        assert!(reason(received.validate_for_send()).contains("keyframe flag"));
        assert!(reason(aac(48_000, 2, 1024, packet)).contains("keyframe flag"));
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
                assert!(reason(opus(rate, 2, rate / 400 * units)).contains("ms of audio"));
            }
            assert!(reason(opus(rate, 2, rate / 400 + 1)).contains("ms of audio"));
        }
        for rate in [11_025, 22_050, 32_000, 44_100, 96_000] {
            assert!(reason(opus(rate, 2, rate / 50)).contains("48000 Hz"));
        }
        for channels in [1, 2, 6, 255] {
            opus(48_000, channels, 960).unwrap();
        }
        assert!(reason(opus(48_000, 0, 960)).contains("1..="));
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
            assert!(reason(result).contains("1024 samples"));
        }
        assert!(reason(aac(50_000, 2, 1024, aac_packet(48_000, 2, &[1]))).contains("7350"));
        assert!(reason(aac(48_000, 7, 1024, aac_packet(48_000, 8, &[1]))).contains("channels"));

        // A configuration for another rate or layout than the frame's.
        let result = aac(44_100, 2, 1024, aac_packet(48_000, 2, &[1]));
        assert!(reason(result).contains("sample rate"));
        let result = aac(48_000, 1, 1024, aac_packet(48_000, 2, &[1]));
        assert!(reason(result).contains("channel layout"));

        // No configuration, and one too short to name a rate and a layout.
        for config in [&[][..], &[0x11][..]] {
            let packet =
                PacketEnvelope::encode(EnvelopeCodec::Aac, 0, 0, true, &[1], config).unwrap();
            assert!(reason(aac(48_000, 2, 1024, packet)).contains("AudioSpecificConfig"));
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
        assert!(unindexed(15, 2).contains("sample rate"));
        assert!(unindexed(3, 0).contains("channel layout"));
        assert!(unindexed(3, 8).contains("channel layout"));
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
        assert_eq!(received.clone().raw().timestamp, 4242);
        let received = unsafe { copy_video_packet(&raw) }.unwrap();
        assert_eq!(received.timestamp(), None);

        let mut audio =
            CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![1; 8]).unwrap();
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

        let mut audio =
            CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![1; 8]).unwrap();
        audio.set_protected(true);
        let copy = audio.clone();
        assert_eq!(
            (copy.data(), copy.is_protected(), copy.num_samples()),
            (audio.data(), true, 960)
        );
    }

    #[test]
    fn descriptors_keep_the_defaults_of_the_fields_they_do_not_set() {
        let video = h264_format().raw();
        assert_eq!(video.timecode, NDIlib_send_timecode_synthesize);
        assert!(video.p_data.is_null() && video.p_metadata.is_null());
        assert_eq!(video.timestamp, 0);

        let mut frame =
            CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![1; 8]).unwrap();
        let audio = frame.raw();
        assert_eq!(audio.timecode, NDIlib_send_timecode_synthesize);
        assert_eq!(audio.FourCC, AudioCodec::Opus.outer(false));
        // A frame built for sending has no timestamp; the SDK ignores the
        // field when it sends.
        assert_eq!(audio.timestamp, NDIlib_recv_timestamp_undefined);
        assert!(audio.p_metadata.is_null());
    }
}
