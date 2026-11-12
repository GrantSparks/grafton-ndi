//! Compressed frames and the packet envelope that frames H.264, HEVC and AAC
//! payloads on the wire.

use std::ffi::CString;

use super::invalid;
use crate::{sys::bindings::*, Rejected, Result, ScanType};

use super::{aliasable::AliasableBox, descriptors};

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
const MAX_PREVIEW_DIMENSION: i32 = 640;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Which of a source's two video streams a compressed frame belongs to.
///
/// The SDK tracks asynchronous sends of the two tiers separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamTier {
    /// The full-quality stream.
    Program,
    /// The low-bandwidth stream.
    Preview,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// The code the envelope header carries for this codec.
    pub const fn code(self) -> u32 {
        match self {
            Self::H264 => fourcc(b"H264"),
            Self::Hevc => fourcc(b"HEVC"),
            Self::Aac => 0xff,
        }
    }

    /// The codec an envelope header code names, if this module knows it.
    pub fn from_code(code: u32) -> Option<Self> {
        [Self::H264, Self::Hevc, Self::Aac]
            .into_iter()
            .find(|codec| codec.code() == code)
    }
}

/// The fixed fields of an envelope header, read in native byte order.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EnvelopeHeader {
    size: usize,
    codec: u32,
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
            return Err(invalid("Truncated compressed packet header"));
        }
        let size = i32::from_ne_bytes(field(bytes, VERSION_OFFSET));
        if size < ENVELOPE_HEADER_BYTES as i32 {
            return Err(invalid("Unsupported compressed packet header size"));
        }
        Ok(Self {
            size: size as usize,
            codec: u32::from_ne_bytes(field(bytes, CODEC_OFFSET)),
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
            return Err(invalid(
                "Cannot send a packet with an unsupported header version or unknown flags",
            ));
        }
        if self.codec != codec.code() {
            return Err(invalid("Packet codec does not match the frame's codec"));
        }
        if self.data_len == 0 {
            return Err(invalid("Empty codec payload"));
        }
        if self.total_len() != Some(total) {
            return Err(invalid(
                "Packet header lengths do not match the size of the frame",
            ));
        }
        Ok(())
    }

    /// Check the codec configuration of a video packet that is about to be
    /// sent: a decoder cannot start on a keyframe without its parameter sets.
    pub(crate) fn validate_video_configuration(&self) -> Result<()> {
        if self.is_keyframe() && self.extra_len == 0 {
            return Err(invalid("Keyframes require codec configuration data"));
        }
        Ok(())
    }

    /// Check what the SDK needs of an AAC packet to deliver it as audio at
    /// `sample_rate` hertz with `channels` channels. `config` is the first two
    /// bytes of the packet's codec configuration, when it has that many.
    ///
    /// The SDK reads the sample rate and the channel layout from those bytes,
    /// where an AudioSpecificConfig keeps them, and discards a packet whose
    /// frame says otherwise.
    pub(crate) fn validate_aac(
        &self,
        config: Option<[u8; 2]>,
        sample_rate: i32,
        channels: i32,
    ) -> Result<()> {
        if !self.is_keyframe() {
            return Err(invalid("AAC packets must carry the keyframe flag"));
        }
        let Some(config) = config else {
            return Err(invalid(
                "AAC packets require their AudioSpecificConfig as codec configuration data",
            ));
        };
        // Five bits of object type, four of sampling-frequency index, four of
        // channel configuration.
        let bits = u16::from_be_bytes(config);
        let frequency_index = usize::from((bits >> 7) & 0xf);
        let channel_configuration = usize::from((bits >> 3) & 0xf);
        if AAC_SAMPLE_RATES.get(frequency_index) != Some(&sample_rate) {
            return Err(invalid(
                "The AAC configuration names a different sample rate than the frame",
            ));
        }
        if channel_configuration
            .checked_sub(1)
            .and_then(|index| AAC_CHANNELS.get(index))
            != Some(&channels)
        {
            return Err(invalid(
                "The AAC configuration names a different channel layout than the frame",
            ));
        }
        Ok(())
    }
}

/// A bounds-checked view of a packet envelope: its header, codec payload and
/// codec configuration data.
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
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when the slice is
    /// shorter than a header, longer than [`MAX_PACKET_BYTES`], or shorter than
    /// the lengths its header declares.
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() > MAX_PACKET_BYTES {
            return Err(invalid("Invalid compressed packet length"));
        }
        let header = EnvelopeHeader::read(bytes)?;
        let end = header
            .total_len()
            .ok_or_else(|| invalid("Compressed packet length overflow"))?;
        if end > bytes.len() {
            return Err(invalid("Truncated compressed packet"));
        }
        Ok(Self { bytes, header })
    }

    /// The codec, when the header names one this module knows.
    pub fn codec(self) -> Option<EnvelopeCodec> {
        EnvelopeCodec::from_code(self.header.codec)
    }

    /// The codec code exactly as the header carries it.
    pub fn codec_code(self) -> u32 {
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

    /// Every flag bit, including ones this module does not interpret.
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

/// Build a version-0 packet envelope around one codec payload.
///
/// `pts` and `dts` are in the stream's own units. `keyframe` marks a video
/// keyframe; AAC has no other kind of packet, so an AAC envelope carries the
/// flag whatever `keyframe` says. `extra_data` is the codec configuration
/// (parameter sets for H.264 and HEVC, the AudioSpecificConfig for AAC); video
/// keyframes and every AAC packet need it to be sendable.
///
/// # Errors
///
/// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when `data` is empty or
/// the packet would exceed [`MAX_PACKET_BYTES`].
///
/// # Examples
///
/// ```
/// use grafton_ndi::advanced::{packet_envelope, EnvelopeCodec, PacketEnvelope};
///
/// let packet = packet_envelope(EnvelopeCodec::H264, 0, 0, true, &[0x65, 0x88], &[0x67, 0x42])?;
/// let view = PacketEnvelope::parse(&packet)?;
/// assert!(view.is_keyframe());
/// assert_eq!(view.data(), [0x65, 0x88]);
/// assert_eq!(view.extra_data(), [0x67, 0x42]);
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
pub fn packet_envelope(
    codec: EnvelopeCodec,
    pts: i64,
    dts: i64,
    keyframe: bool,
    data: &[u8],
    extra_data: &[u8],
) -> Result<Vec<u8>> {
    let size = ENVELOPE_HEADER_BYTES
        .checked_add(data.len())
        .and_then(|n| n.checked_add(extra_data.len()))
        .ok_or_else(|| invalid("Packet size overflow"))?;
    if data.is_empty() || size > MAX_PACKET_BYTES {
        return Err(invalid("Invalid compressed packet payload size"));
    }
    let mut bytes = vec![0; size];
    let mut put = |offset: usize, value: &[u8]| {
        bytes[offset..offset + value.len()].copy_from_slice(value);
    };
    put(
        VERSION_OFFSET,
        &(ENVELOPE_HEADER_BYTES as i32).to_ne_bytes(),
    );
    put(CODEC_OFFSET, &codec.code().to_ne_bytes());
    put(PTS_OFFSET, &pts.to_ne_bytes());
    put(DTS_OFFSET, &dts.to_ne_bytes());
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

/// The codec, geometry and timing of compressed video, independent of any
/// one frame's bytes.
///
/// # Examples
///
/// ```
/// use grafton_ndi::advanced::{StreamTier, VideoCodec, VideoPacketFormat};
///
/// let format = VideoPacketFormat::new(VideoCodec::Hevc, 1920, 1080)?
///     .frame_rate(60_000, 1001)?
///     .tier(StreamTier::Program);
/// assert_eq!(format.frame_rate_parts(), (60_000, 1001));
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoPacketFormat {
    pub(crate) codec: VideoCodec,
    pub(crate) tier: StreamTier,
    pub(crate) protected: bool,
    width: i32,
    height: i32,
    rate_n: i32,
    rate_d: i32,
    aspect: f32,
    scan: ScanType,
}

impl VideoPacketFormat {
    /// A progressive program-tier format at 30 frames per second with square
    /// pixels. Adjust it with the other methods.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when a dimension is
    /// not positive.
    pub fn new(codec: VideoCodec, width: i32, height: i32) -> Result<Self> {
        if width <= 0 || height <= 0 {
            return Err(invalid("Compressed video dimensions must be positive"));
        }
        Ok(Self {
            codec,
            tier: StreamTier::Program,
            protected: false,
            width,
            height,
            rate_n: 30,
            rate_d: 1,
            aspect: 0.0,
            scan: ScanType::Progressive,
        })
    }

    /// Set the frame rate as a fraction, for example `30_000 / 1001`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when either part is
    /// not positive.
    pub fn frame_rate(mut self, numerator: i32, denominator: i32) -> Result<Self> {
        if numerator <= 0 || denominator <= 0 {
            return Err(invalid("Frame rate must be positive"));
        }
        self.rate_n = numerator;
        self.rate_d = denominator;
        Ok(self)
    }

    /// Set the picture aspect ratio (width over height of the displayed
    /// picture). `0.0` means square pixels, as the SDK defines it; a receiver
    /// is then given the ratio of the frame's width to its height.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when the value is
    /// negative or not finite.
    pub fn aspect_ratio(mut self, value: f32) -> Result<Self> {
        if !value.is_finite() || value < 0.0 {
            return Err(invalid(
                "Aspect ratio must be finite and not negative; zero selects square pixels",
            ));
        }
        self.aspect = value;
        Ok(self)
    }

    /// Select the program or preview stream.
    pub fn tier(mut self, tier: StreamTier) -> Self {
        self.tier = tier;
        self
    }

    /// Set the scan type.
    pub fn scan_type(mut self, scan: ScanType) -> Self {
        self.scan = scan;
        self
    }

    /// Mark the stream as protected.
    pub fn protected(mut self, protected: bool) -> Self {
        self.protected = protected;
        self
    }

    /// The codec.
    pub fn codec(self) -> VideoCodec {
        self.codec
    }

    /// The stream this format belongs to.
    pub fn stream_tier(self) -> StreamTier {
        self.tier
    }

    /// Whether the stream is protected.
    pub fn is_protected(self) -> bool {
        self.protected
    }

    /// Width in pixels.
    pub fn width(self) -> i32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(self) -> i32 {
        self.height
    }

    /// The frame rate as `(numerator, denominator)`.
    pub fn frame_rate_parts(self) -> (i32, i32) {
        (self.rate_n, self.rate_d)
    }

    /// The picture aspect ratio; `0.0` means square pixels.
    pub fn picture_aspect_ratio(self) -> f32 {
        self.aspect
    }

    /// The scan type.
    pub fn scan(self) -> ScanType {
        self.scan
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
                return Err(invalid(
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
            return Err(invalid(
                "An H.264 or HEVC picture on the preview stream is at most 640 pixels wide and 640 high",
            ));
        }
        Ok(())
    }

    /// A native frame descriptor with this format and no data.
    pub(crate) fn raw(self) -> NDIlib_video_frame_v2_t {
        NDIlib_video_frame_v2_t {
            xres: self.width,
            yres: self.height,
            FourCC: self.codec.outer(self.tier, self.protected),
            frame_rate_N: self.rate_n,
            frame_rate_D: self.rate_d,
            picture_aspect_ratio: self.aspect,
            frame_format_type: self.scan.into(),
            ..descriptors::video_frame()
        }
    }
}

/// Text attached to a frame, checked against the frame metadata limit.
pub(crate) fn frame_metadata(value: String) -> Result<CString> {
    if value.len() >= crate::frames::MAX_METADATA_BYTES {
        return Err(invalid("Packet metadata exceeds its size limit"));
    }
    Ok(CString::new(value)?)
}

/// The per-frame timecode and metadata every compressed and scatter frame
/// carries, with the single checked way to set the metadata.
#[derive(Debug)]
pub(crate) struct FrameExtras {
    timecode: i64,
    metadata: Option<CString>,
}

impl FrameExtras {
    /// A frame built for sending: the SDK synthesizes the timecode.
    pub(crate) fn new() -> Self {
        Self::received(NDIlib_send_timecode_synthesize, None)
    }

    /// The values the SDK delivered with a received frame.
    pub(crate) fn received(timecode: i64, metadata: Option<CString>) -> Self {
        Self { timecode, metadata }
    }

    pub(crate) fn timecode(&self) -> i64 {
        self.timecode
    }

    /// The metadata text, if any.
    pub(crate) fn metadata(&self) -> Option<&str> {
        // Metadata is only ever stored from a `String` or from validated UTF-8.
        self.metadata.as_deref().and_then(|text| text.to_str().ok())
    }

    /// Bytes the metadata keeps allocated, terminator included.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.metadata
            .as_ref()
            .map_or(0, |metadata| metadata.as_bytes_with_nul().len())
    }

    /// The pointer a native descriptor lends to the SDK, null without
    /// metadata. It is valid for as long as these extras are.
    pub(crate) fn metadata_ptr(&self) -> *const std::ffi::c_char {
        self.metadata
            .as_ref()
            .map_or(std::ptr::null(), |metadata| metadata.as_ptr())
    }
}

/// A frame that carries [`FrameExtras`].
pub(crate) trait HasExtras: Sized {
    fn extras_mut(&mut self) -> &mut FrameExtras;
}

/// Set a frame's timecode.
pub(crate) fn attach_timecode<T: HasExtras>(mut frame: T, timecode: i64) -> T {
    frame.extras_mut().timecode = timecode;
    frame
}

/// Attach checked metadata, returning the frame unchanged when it is refused.
pub(crate) fn attach_metadata<T: HasExtras>(
    mut frame: T,
    value: String,
) -> std::result::Result<T, Rejected<T>> {
    match frame_metadata(value) {
        Ok(metadata) => {
            frame.extras_mut().metadata = Some(metadata);
            Ok(frame)
        }
        Err(error) => Err(Rejected::new(error, frame)),
    }
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
#[derive(Debug)]
pub struct CompressedVideoFrame {
    pub(crate) format: VideoPacketFormat,
    pub(crate) bytes: AliasableBox<[u8]>,
    pub(crate) extras: FrameExtras,
}

impl HasExtras for CompressedVideoFrame {
    fn extras_mut(&mut self) -> &mut FrameExtras {
        &mut self.extras
    }
}

impl CompressedVideoFrame {
    /// Take ownership of one frame's bytes: a packet envelope for H.264 and
    /// HEVC (see [`packet_envelope`]), a SpeedHQ frame otherwise. The timecode
    /// is synthesized by the SDK unless [`with_timecode`](Self::with_timecode)
    /// sets one.
    ///
    /// # Errors
    ///
    /// Returns the bytes with [`Error::InvalidFrame`](crate::Error::InvalidFrame)
    /// when they are empty, too large, not an envelope of the format's codec,
    /// an envelope this module cannot send (a later header version, unknown
    /// flags, trailing bytes, or a keyframe without codec configuration), or
    /// a frame the SDK does not deliver (see [`VideoCodec`]).
    pub fn new(
        format: VideoPacketFormat,
        bytes: Vec<u8>,
    ) -> std::result::Result<Self, Rejected<Vec<u8>>> {
        match validate_video_packet(format, &bytes, Use::Send) {
            Ok(()) => Ok(Self {
                format,
                bytes: AliasableBox::new(bytes.into_boxed_slice()),
                extras: FrameExtras::new(),
            }),
            Err(error) => Err(Rejected::new(error, bytes)),
        }
    }

    /// Attach per-frame metadata.
    ///
    /// # Errors
    ///
    /// Returns the frame unchanged when the text is too long or contains a NUL.
    pub fn with_metadata(
        self,
        value: impl Into<String>,
    ) -> std::result::Result<Self, Rejected<Self>> {
        attach_metadata(self, value.into())
    }

    /// Set the timecode, in 100-nanosecond units.
    pub fn with_timecode(self, timecode: i64) -> Self {
        attach_timecode(self, timecode)
    }

    /// The codec bytes.
    pub fn data(&self) -> &[u8] {
        &self.bytes
    }

    /// The format.
    pub fn format(&self) -> VideoPacketFormat {
        self.format
    }

    /// The timecode in 100-nanosecond units, or the SDK's "synthesize" value
    /// on a frame built for sending without one.
    pub fn timecode(&self) -> i64 {
        self.extras.timecode()
    }

    /// The per-frame metadata, if any.
    pub fn metadata(&self) -> Option<&str> {
        self.extras.metadata()
    }

    /// Take the codec bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes.into_box().into_vec()
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
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) with the reason.
    pub fn validate_for_send(&self) -> Result<()> {
        validate_video_packet(self.format, &self.bytes, Use::Send)
    }

    /// Bytes this frame keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.bytes.len() + self.extras.retained_bytes()
    }

    /// A native descriptor that lends this frame's bytes and metadata to the
    /// SDK. The SDK may write to the bytes for as long as it holds the
    /// descriptor, so the caller keeps the frame exclusively for that long.
    pub(crate) fn raw(&mut self) -> NDIlib_video_frame_v2_t {
        let mut raw = self.format.raw();
        // Construction bounds the length by MAX_PACKET_BYTES, below i32::MAX.
        raw.__bindgen_anon_1.data_size_in_bytes = self.bytes.len() as i32;
        raw.p_data = self.bytes.as_mut_ptr().cast();
        raw.p_metadata = self.extras.metadata_ptr();
        raw.timecode = self.extras.timecode();
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
        return Err(invalid("Invalid compressed video size"));
    }
    match format.codec.envelope_codec() {
        Some(codec) => {
            let packet = PacketEnvelope::parse(bytes)?;
            if packet.codec() != Some(codec) {
                return Err(invalid("Packet codec does not match video descriptor"));
            }
            if packet.data().is_empty() {
                return Err(invalid("Empty codec payload"));
            }
        }
        None if bytes.len() < SPEEDHQ_HEADER_BYTES => {
            return Err(invalid("Truncated SpeedHQ frame"));
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
const AAC_SAMPLE_RATES: [i32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// The channel counts an AAC configuration can name, in the order of its
/// channel configuration, which starts at one.
const AAC_CHANNELS: [i32; 7] = [1, 2, 3, 4, 5, 6, 8];

/// The number of samples per channel in an AAC frame.
const AAC_FRAME_SAMPLES: i32 = 1024;

/// The sample rates Opus runs at.
const OPUS_SAMPLE_RATES: [i32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// The durations of an Opus frame, in units of 2.5 ms.
const OPUS_FRAME_DURATIONS: [i64; 6] = [1, 2, 4, 8, 16, 24];

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioCodec {
    /// AAC in a packet envelope.
    Aac,
    /// Opus, without an envelope.
    Opus,
}

/// Check that every dimension of a compressed audio frame is positive.
fn validate_audio_dimensions(sample_rate: i32, channels: i32, samples: i32) -> Result<()> {
    if sample_rate <= 0 || channels <= 0 || samples <= 0 {
        return Err(invalid("Compressed audio dimensions must be positive"));
    }
    Ok(())
}

impl AudioCodec {
    /// Check the parameters a frame of this codec is delivered with; see
    /// [`AudioCodec`]. An AAC frame's configuration is checked separately,
    /// against its envelope.
    pub(crate) fn validate_delivery(
        self,
        sample_rate: i32,
        channels: i32,
        samples: i32,
    ) -> Result<()> {
        validate_audio_dimensions(sample_rate, channels, samples)?;
        match self {
            Self::Opus => {
                if !OPUS_SAMPLE_RATES.contains(&sample_rate) {
                    return Err(invalid(
                        "Opus is sent at 8000, 12000, 16000, 24000 or 48000 Hz",
                    ));
                }
                // One unit is 2.5 ms, which is a four-hundredth of a second.
                let units = i64::from(samples) * 400;
                if !OPUS_FRAME_DURATIONS
                    .iter()
                    .any(|duration| units == duration * i64::from(sample_rate))
                {
                    return Err(invalid(
                        "An Opus frame holds 2.5, 5, 10, 20, 40 or 60 ms of audio",
                    ));
                }
            }
            Self::Aac => {
                if samples != AAC_FRAME_SAMPLES {
                    return Err(invalid("An AAC frame holds 1024 samples per channel"));
                }
                if !AAC_SAMPLE_RATES.contains(&sample_rate) {
                    return Err(invalid(
                        "AAC is sent at a sample rate its configuration can name, from 7350 to 96000 Hz",
                    ));
                }
                if !AAC_CHANNELS.contains(&channels) {
                    return Err(invalid("AAC is sent with one to six or eight channels"));
                }
            }
        }
        Ok(())
    }

    /// The FourCC a frame of this codec is sent with.
    pub(crate) const fn outer(self, protected: bool) -> u32 {
        (match self {
            Self::Aac => EnvelopeCodec::Aac.code(),
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
#[derive(Debug)]
pub struct CompressedAudioFrame {
    pub(crate) codec: AudioCodec,
    pub(crate) protected: bool,
    pub(crate) bytes: AliasableBox<[u8]>,
    pub(crate) sample_rate: i32,
    pub(crate) channels: i32,
    pub(crate) samples: i32,
    pub(crate) extras: FrameExtras,
}

impl HasExtras for CompressedAudioFrame {
    fn extras_mut(&mut self) -> &mut FrameExtras {
        &mut self.extras
    }
}

impl CompressedAudioFrame {
    /// Take ownership of one audio frame's bytes: a packet envelope for AAC
    /// (see [`packet_envelope`]), one bare packet for Opus. `samples` is the
    /// number of samples per channel the frame decodes to.
    ///
    /// # Errors
    ///
    /// Returns the bytes with [`Error::InvalidFrame`](crate::Error::InvalidFrame)
    /// when a dimension is not positive, the bytes are empty or too large, the
    /// parameters are not ones the codec is delivered with (see
    /// [`AudioCodec`]), or an AAC envelope is malformed or disagrees with them.
    pub fn new(
        codec: AudioCodec,
        sample_rate: i32,
        channels: i32,
        samples: i32,
        bytes: Vec<u8>,
    ) -> std::result::Result<Self, Rejected<Vec<u8>>> {
        let frame = Self {
            codec,
            protected: false,
            bytes: AliasableBox::new(bytes.into_boxed_slice()),
            sample_rate,
            channels,
            samples,
            extras: FrameExtras::new(),
        };
        match frame.validate_for_send() {
            Ok(()) => Ok(frame),
            Err(error) => Err(Rejected::new(error, frame.into_bytes())),
        }
    }

    /// Set the timecode, in 100-nanosecond units.
    pub fn with_timecode(self, timecode: i64) -> Self {
        attach_timecode(self, timecode)
    }

    /// Attach per-frame metadata.
    ///
    /// # Errors
    ///
    /// Returns the frame unchanged when the text is too long or contains a NUL.
    pub fn with_metadata(
        self,
        metadata: impl Into<String>,
    ) -> std::result::Result<Self, Rejected<Self>> {
        attach_metadata(self, metadata.into())
    }

    /// Mark the stream as protected.
    pub fn protected(mut self, protected: bool) -> Self {
        self.protected = protected;
        self
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
    pub fn sample_rate(&self) -> i32 {
        self.sample_rate
    }

    /// Number of channels.
    pub fn channels(&self) -> i32 {
        self.channels
    }

    /// Samples per channel the frame decodes to.
    pub fn samples(&self) -> i32 {
        self.samples
    }

    /// The timecode in 100-nanosecond units, or the SDK's "synthesize" value
    /// on a frame built for sending without one.
    pub fn timecode(&self) -> i64 {
        self.extras.timecode()
    }

    /// The per-frame metadata, if any.
    pub fn metadata(&self) -> Option<&str> {
        self.extras.metadata()
    }

    /// Take the codec bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes.into_box().into_vec()
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
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) with the reason.
    pub fn validate_for_send(&self) -> Result<()> {
        let envelope = self.validate()?;
        self.codec
            .validate_delivery(self.sample_rate, self.channels, self.samples)?;
        if let Some(packet) = envelope {
            packet
                .header
                .validate_for_send(EnvelopeCodec::Aac, self.bytes.len())?;
            packet.header.validate_aac(
                packet.extra_data().first_chunk().copied(),
                self.sample_rate,
                self.channels,
            )?;
        }
        Ok(())
    }

    /// The checks that hold for received frames as well. Returns the envelope
    /// of a frame whose codec has one.
    pub(crate) fn validate(&self) -> Result<Option<PacketEnvelope<'_>>> {
        validate_audio_dimensions(self.sample_rate, self.channels, self.samples)?;
        if self.bytes.is_empty() || self.bytes.len() > MAX_PACKET_BYTES {
            return Err(invalid("Invalid compressed audio size"));
        }
        if self.codec != AudioCodec::Aac {
            return Ok(None);
        }
        let packet = PacketEnvelope::parse(&self.bytes)?;
        if packet.codec() != Some(EnvelopeCodec::Aac) || packet.data().is_empty() {
            return Err(invalid("Invalid AAC packet envelope"));
        }
        Ok(Some(packet))
    }

    /// A native descriptor that lends this frame's bytes and metadata to the
    /// SDK; see [`CompressedVideoFrame::raw`].
    pub(crate) fn raw(&mut self) -> NDIlib_audio_frame_v3_t {
        let mut raw = NDIlib_audio_frame_v3_t {
            sample_rate: self.sample_rate,
            no_channels: self.channels,
            no_samples: self.samples,
            timecode: self.extras.timecode(),
            FourCC: self.codec.outer(self.protected),
            p_data: self.bytes.as_mut_ptr().cast(),
            p_metadata: self.extras.metadata_ptr(),
            ..descriptors::audio_frame()
        };
        // Validation bounds the length by MAX_PACKET_BYTES, below i32::MAX.
        raw.__bindgen_anon_1.data_size_in_bytes = self.bytes.len() as i32;
        raw
    }
}

/// Copy a frame's metadata text.
///
/// # Safety
///
/// `pointer` is null or a readable NUL-terminated string.
pub(crate) unsafe fn copy_metadata(pointer: *const std::ffi::c_char) -> Result<Option<CString>> {
    // SAFETY: forwarded to the caller's obligation.
    let layout = unsafe { crate::frames::validate_frame_metadata(pointer)? };
    Ok(
        // SAFETY: the layout was validated for this pointer just above.
        unsafe { crate::frames::frame_metadata_str(pointer, layout) }
            .map(|s| CString::new(s).expect("validated frame metadata")),
    )
}

/// The bytes of a compressed native frame.
///
/// # Safety
///
/// When `data` is not null it is readable for `size` bytes.
unsafe fn packet_bytes<'a>(data: *const u8, size: i32) -> Result<&'a [u8]> {
    if size <= 0 || size as usize > MAX_PACKET_BYTES || data.is_null() {
        return Err(invalid("Invalid compressed frame buffer"));
    }
    // SAFETY: non-null and a positive in-range size were checked; the caller
    // guarantees readability for that size.
    Ok(unsafe { std::slice::from_raw_parts(data, size as usize) })
}

/// Copy a compressed video frame the SDK delivered.
///
/// # Safety
///
/// `raw` describes a compressed frame: its union holds the data size, its data
/// pointer is readable for that size, and its metadata pointer is null or a
/// NUL-terminated string.
pub(crate) unsafe fn copy_video_packet(
    raw: &NDIlib_video_frame_v2_t,
) -> Result<CompressedVideoFrame> {
    let (codec, tier, protected) = VideoCodec::from_outer(raw.FourCC)
        .ok_or_else(|| invalid("Unknown compressed video FourCC"))?;
    let format = VideoPacketFormat::new(codec, raw.xres, raw.yres)?
        .frame_rate(raw.frame_rate_N, raw.frame_rate_D)?
        .aspect_ratio(raw.picture_aspect_ratio)?
        .tier(tier)
        .protected(protected)
        .scan_type(
            ScanType::try_from(raw.frame_format_type)
                .map_err(|_| invalid("Unknown packet scan type"))?,
        );
    // SAFETY: a compressed frame's union holds its size, and the caller
    // guarantees the buffer is readable for it.
    let bytes = unsafe { packet_bytes(raw.p_data, raw.__bindgen_anon_1.data_size_in_bytes)? };
    validate_video_packet(format, bytes, Use::Receive)?;
    Ok(CompressedVideoFrame {
        format,
        bytes: AliasableBox::new(bytes.into()),
        // SAFETY: forwarded to the caller's obligation.
        extras: FrameExtras::received(raw.timecode, unsafe { copy_metadata(raw.p_metadata)? }),
    })
}

/// Copy a compressed audio frame the SDK delivered.
///
/// # Safety
///
/// As for [`copy_video_packet`].
pub(crate) unsafe fn copy_audio_packet(
    raw: &NDIlib_audio_frame_v3_t,
) -> Result<CompressedAudioFrame> {
    let (codec, protected) = AudioCodec::from_outer(raw.FourCC)
        .ok_or_else(|| invalid("Unknown compressed audio FourCC"))?;
    // SAFETY: a compressed frame's union holds its size, and the caller
    // guarantees the buffer is readable for it.
    let bytes = unsafe { packet_bytes(raw.p_data, raw.__bindgen_anon_1.data_size_in_bytes)? };
    let frame = CompressedAudioFrame {
        codec,
        protected,
        bytes: AliasableBox::new(bytes.into()),
        sample_rate: raw.sample_rate,
        channels: raw.no_channels,
        samples: raw.no_samples,
        // SAFETY: forwarded to the caller's obligation.
        extras: FrameExtras::received(raw.timecode, unsafe { copy_metadata(raw.p_metadata)? }),
    };
    frame.validate()?;
    Ok(frame)
}

// The values above are the NDI wire format, which the data types in this file
// implement in every build. Where the build has the Advanced SDK declarations,
// each value is checked against them.
#[cfg(ndi_has_advanced_header)]
const _: () = {
    use crate::sys::advanced_bindings::*;
    use std::mem::{offset_of, size_of};

    // Enum constants of the Advanced-only headers take the target's C enum
    // type, which is signed on some targets; compare bit patterns.
    macro_rules! same_bits {
        ($ours:expr, $native:ident) => {
            crate::sys::advanced::declared! { $native {
                assert!($ours == u32::from_ne_bytes($native.to_ne_bytes()));
            } else {} }
        };
    }

    use StreamTier::{Preview, Program};
    use VideoCodec::*;
    same_bits!(
        SpeedHq420.outer(Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ0_highest_bandwidth
    );
    same_bits!(
        SpeedHq420.outer(Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ0_highest_bandwidth_protected
    );
    same_bits!(
        SpeedHq420.outer(Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ0_lowest_bandwidth
    );
    same_bits!(
        SpeedHq420.outer(Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ0_lowest_bandwidth_protected
    );
    same_bits!(
        SpeedHq422.outer(Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ2_highest_bandwidth
    );
    same_bits!(
        SpeedHq422.outer(Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ2_highest_bandwidth_protected
    );
    same_bits!(
        SpeedHq422.outer(Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ2_lowest_bandwidth
    );
    same_bits!(
        SpeedHq422.outer(Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ2_lowest_bandwidth_protected
    );
    same_bits!(
        SpeedHq422Alpha.outer(Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ7_highest_bandwidth
    );
    same_bits!(
        SpeedHq422Alpha.outer(Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ7_highest_bandwidth_protected
    );
    same_bits!(
        SpeedHq422Alpha.outer(Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ7_lowest_bandwidth
    );
    same_bits!(
        SpeedHq422Alpha.outer(Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_SHQ7_lowest_bandwidth_protected
    );
    same_bits!(
        H264.outer(Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_highest_bandwidth
    );
    same_bits!(
        H264.outer(Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_highest_bandwidth_protected
    );
    same_bits!(
        H264.outer(Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_lowest_bandwidth
    );
    same_bits!(
        H264.outer(Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_lowest_bandwidth_protected
    );
    same_bits!(
        Hevc.outer(Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_highest_bandwidth
    );
    same_bits!(
        Hevc.outer(Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_highest_bandwidth_protected
    );
    same_bits!(
        Hevc.outer(Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_lowest_bandwidth
    );
    same_bits!(
        Hevc.outer(Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_lowest_bandwidth_protected
    );
    same_bits!(
        H264Alpha.outer(Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_alpha_highest_bandwidth
    );
    same_bits!(
        H264Alpha.outer(Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_alpha_highest_bandwidth_protected
    );
    same_bits!(
        H264Alpha.outer(Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_alpha_lowest_bandwidth
    );
    same_bits!(
        H264Alpha.outer(Preview, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_H264_alpha_lowest_bandwidth_protected
    );
    same_bits!(
        HevcAlpha.outer(Program, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_alpha_highest_bandwidth
    );
    same_bits!(
        HevcAlpha.outer(Program, true),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_alpha_highest_bandwidth_protected
    );
    same_bits!(
        HevcAlpha.outer(Preview, false),
        NDIlib_FourCC_video_type_ex_e_NDIlib_FourCC_video_type_ex_HEVC_alpha_lowest_bandwidth
    );
    same_bits!(
        HevcAlpha.outer(Preview, true),
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
        EnvelopeCodec::H264.code(),
        NDIlib_compressed_FourCC_type_e_NDIlib_compressed_FourCC_type_H264
    );
    same_bits!(
        EnvelopeCodec::Hevc.code(),
        NDIlib_compressed_FourCC_type_e_NDIlib_compressed_FourCC_type_HEVC
    );
    same_bits!(
        EnvelopeCodec::Aac.code(),
        NDIlib_compressed_FourCC_type_e_NDIlib_compressed_FourCC_type_AAC
    );

    crate::sys::advanced::declared! { NDIlib_compressed_packet_t {
    assert!(KEYFRAME == NDIlib_compressed_packet_flags_keyframe);
    assert!(ENVELOPE_HEADER_BYTES as i32 == NDIlib_compressed_packet_version_0);
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
    pub(crate) fn aac_config(sample_rate: i32, channels: i32) -> [u8; 2] {
        let position = |values: &[i32], value| {
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
    pub(crate) fn aac_packet(sample_rate: i32, channels: i32, payload: &[u8]) -> Vec<u8> {
        packet_envelope(
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

    fn aac(sample_rate: i32, channels: i32, samples: i32, packet: Vec<u8>) -> Result<()> {
        CompressedAudioFrame::new(AudioCodec::Aac, sample_rate, channels, samples, packet)
            .map(drop)
            .map_err(Rejected::into_error)
    }

    fn opus(sample_rate: i32, channels: i32, samples: i32) -> Result<()> {
        CompressedAudioFrame::new(AudioCodec::Opus, sample_rate, channels, samples, vec![1; 8])
            .map(drop)
            .map_err(Rejected::into_error)
    }

    /// The text of an invalid-frame error.
    fn reason(result: Result<()>) -> String {
        match result {
            Err(crate::Error::InvalidFrame(reason)) => reason,
            other => panic!("expected an invalid frame, got {other:?}"),
        }
    }

    #[test]
    fn packed_header_parsing_preserves_extensions_and_checks_bounds() {
        let mut bytes = packet_envelope(EnvelopeCodec::H264, 12, -4, true, &[1, 2], &[3]).unwrap();
        let p = PacketEnvelope::parse(&bytes).unwrap();
        assert_eq!(p.codec(), Some(EnvelopeCodec::H264));
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
                        .tier(tier)
                        .protected(protected);
                    let bytes = match codec.envelope_codec() {
                        Some(code) => packet_envelope(code, 0, 0, false, &[1], &[]).unwrap(),
                        None => vec![1, 2, 3, 4],
                    };
                    let mut frame = CompressedVideoFrame::new(format, bytes).unwrap();
                    let raw = frame.raw();
                    assert_eq!(raw.FourCC, codec.outer(tier, protected));
                    let copied = unsafe { copy_video_packet(&raw) }.unwrap();
                    assert_eq!(copied.format.codec, codec);
                    assert_eq!(copied.format.tier, tier);
                    assert_eq!(copied.format.protected, protected);
                    assert_eq!(copied.data(), frame.data());
                }
            }
        }
        assert!(PacketEnvelope::parse(b"Opus").is_err());
        assert!(VideoPacketFormat::new(VideoCodec::H264, 0, 1).is_err());
    }

    #[test]
    fn a_zero_aspect_ratio_means_square_pixels_and_is_kept_when_received() {
        let format = h264_format();
        assert_eq!(format.picture_aspect_ratio(), 0.0);
        assert_eq!(
            format.aspect_ratio(0.0).unwrap().picture_aspect_ratio(),
            0.0
        );
        assert!(format.aspect_ratio(-1.0).is_err());
        assert!(format.aspect_ratio(f32::NAN).is_err());
        assert!(format.aspect_ratio(f32::INFINITY).is_err());

        // A received frame whose descriptor leaves the ratio unset is kept
        // as it is.
        let bytes = packet_envelope(EnvelopeCodec::H264, 0, 0, false, &[1], &[]).unwrap();
        let mut frame = CompressedVideoFrame::new(format, bytes).unwrap();
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
    }

    #[test]
    fn rejected_constructors_return_their_input() {
        let rejected = CompressedVideoFrame::new(h264_format(), vec![7; 20]).unwrap_err();
        assert_eq!(rejected.into_inner(), vec![7; 20]);

        let rejected =
            CompressedAudioFrame::new(AudioCodec::Opus, 0, 2, 960, vec![1, 2]).unwrap_err();
        assert_eq!(rejected.into_inner(), [1, 2]);

        let frame = CompressedVideoFrame::new(h264_format().tier(StreamTier::Preview), {
            packet_envelope(EnvelopeCodec::H264, 0, 0, false, &[1], &[]).unwrap()
        })
        .unwrap();
        let frame = frame.with_metadata("nul\0inside").unwrap_err().into_inner();
        assert_eq!(frame.format().stream_tier(), StreamTier::Preview);
        assert_eq!(frame.metadata(), None);
        let frame = frame.with_metadata("<ok/>").unwrap();
        assert_eq!(frame.metadata(), Some("<ok/>"));
    }

    #[test]
    fn protected_aac_preserves_codec_high_bit() {
        let bytes = aac_packet(48_000, 2, &[1]);
        let mut frame = CompressedAudioFrame::new(AudioCodec::Aac, 48_000, 2, 1024, bytes)
            .unwrap()
            .protected(true);
        let raw = frame.raw();
        let copied = unsafe { copy_audio_packet(&raw) }.unwrap();
        assert_eq!(copied.codec(), AudioCodec::Aac);
        assert!(copied.is_protected());
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

    #[test]
    fn receiving_keeps_unknown_flags_but_sending_rejects_them() {
        let mut bytes = packet_envelope(EnvelopeCodec::H264, 0, 0, false, &[1], &[]).unwrap();
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
            let mut video = packet_envelope(EnvelopeCodec::H264, 0, 0, true, &[1], &[2]).unwrap();
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
        let bytes = packet_envelope(EnvelopeCodec::H264, 0, 0, true, &[1], &[2]).unwrap();
        let mut frame = CompressedVideoFrame::new(format, bytes).unwrap();
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
                    .tier(tier);
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
            packet_envelope(codec.envelope_codec().unwrap(), 0, 0, true, &[1], &[2]).unwrap()
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
                    .tier(StreamTier::Preview);
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
                let format = format.tier(StreamTier::Preview);
                assert!(validate_video_packet(format, &keyframe(codec), Use::Receive).is_ok());
            }
        }
        // SpeedHQ previews are not limited.
        let format = VideoPacketFormat::new(VideoCodec::SpeedHq422, 1920, 1080)
            .unwrap()
            .tier(StreamTier::Preview);
        video(format, vec![0; 4]).unwrap();
    }

    #[test]
    fn an_aac_envelope_always_carries_the_keyframe_flag() {
        let config = aac_config(48_000, 2);
        for keyframe in [false, true] {
            let packet =
                packet_envelope(EnvelopeCodec::Aac, 0, 0, keyframe, &[1], &config).unwrap();
            assert!(PacketEnvelope::parse(&packet).unwrap().is_keyframe());
            aac(48_000, 2, 1024, packet).unwrap();
        }
        // The flag stays the caller's choice for video.
        let packet = packet_envelope(EnvelopeCodec::H264, 0, 0, false, &[1], &[]).unwrap();
        assert!(!PacketEnvelope::parse(&packet).unwrap().is_keyframe());

        // An AAC packet from elsewhere without the flag is kept when received
        // and refused when sent: the SDK would drop it without a word.
        let mut packet = aac_packet(48_000, 2, &[1]);
        packet[FLAGS_OFFSET..FLAGS_OFFSET + 4].copy_from_slice(&0u32.to_ne_bytes());
        let received = CompressedAudioFrame {
            codec: AudioCodec::Aac,
            protected: false,
            bytes: AliasableBox::new(packet.clone().into_boxed_slice()),
            sample_rate: 48_000,
            channels: 2,
            samples: 1024,
            extras: FrameExtras::received(0, None),
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
        assert!(reason(opus(48_000, 0, 960)).contains("positive"));
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
            let packet = packet_envelope(EnvelopeCodec::Aac, 0, 0, true, &[1], config).unwrap();
            assert!(reason(aac(48_000, 2, 1024, packet)).contains("AudioSpecificConfig"));
        }
        // A longer configuration is read by its first two bytes.
        let mut config = aac_config(48_000, 2).to_vec();
        config.extend([0x56, 0xe5, 0x00]);
        let packet = packet_envelope(EnvelopeCodec::Aac, 0, 0, true, &[1], &config).unwrap();
        aac(48_000, 2, 1024, packet).unwrap();

        // Sample rates and layouts the configuration has no index for.
        let unindexed = |frequency_index: u16, channel_configuration: u16| {
            let config = ((2u16 << 11) | (frequency_index << 7) | (channel_configuration << 3))
                .to_be_bytes();
            let packet = packet_envelope(EnvelopeCodec::Aac, 0, 0, true, &[1], &config).unwrap();
            reason(aac(48_000, 2, 1024, packet))
        };
        assert!(unindexed(15, 2).contains("sample rate"));
        assert!(unindexed(3, 0).contains("channel layout"));
        assert!(unindexed(3, 8).contains("channel layout"));
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
        assert_eq!(audio.timestamp, 0);
        assert!(audio.p_metadata.is_null());
    }
}
