//! Compressed frames and the packet envelope that frames H.264, HEVC and AAC
//! payloads on the wire.

use std::ffi::CString;

use super::{invalid, Rejected};
use crate::{sys::bindings::*, Result, ScanType};

/// Maximum compressed frame size, in bytes, that this module sends or accepts.
pub const MAX_PACKET_BYTES: usize = 256 * 1024 * 1024;

/// Size in bytes of the version-0 envelope header.
pub(crate) const ENVELOPE_HEADER_BYTES: usize = 44;

/// Set in every byte of the FourCC of a protected stream.
pub(crate) const PROTECTED: u32 = 0x8080_8080;

/// The envelope flag that marks a keyframe.
const KEYFRAME: u32 = 1;

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

    pub(crate) fn codec(&self) -> u32 {
        self.codec
    }

    pub(crate) fn data_len(&self) -> usize {
        self.data_len
    }

    pub(crate) fn extra_len(&self) -> usize {
        self.extra_len
    }

    pub(crate) fn is_keyframe(&self) -> bool {
        self.flags & KEYFRAME != 0
    }

    /// Whether this is the header version and the flags this module can send.
    /// Received packets may carry later versions; those are kept but not sent.
    pub(crate) fn is_current_version(&self) -> bool {
        self.size == ENVELOPE_HEADER_BYTES && self.flags & !KEYFRAME == 0 && self.reserved == 0
    }

    /// Header, payload and codec configuration together, if that does not overflow.
    pub(crate) fn total_len(&self) -> Option<usize> {
        self.size
            .checked_add(self.data_len)?
            .checked_add(self.extra_len)
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

    /// Whether the packet is one this module can send: version 0, no unknown
    /// flags, a zero reserved field and nothing after the declared lengths.
    fn is_sendable(self) -> bool {
        self.header.is_current_version() && self.trailing_bytes().is_empty()
    }
}

/// Build a version-0 packet envelope around one codec payload.
///
/// `pts` and `dts` are in the stream's own units. `extra_data` is the codec
/// configuration (parameter sets for H.264 and HEVC, the audio configuration
/// for AAC); video keyframes and every AAC packet need it to be sendable.
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
#[derive(Clone, Copy, Debug)]
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
    /// picture). `0.0` means square pixels, as the SDK defines it.
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
            timecode: NDIlib_send_timecode_synthesize,
            ..Default::default()
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

/// One owned compressed video frame: a [`VideoPacketFormat`] and the codec
/// bytes, with optional metadata.
///
/// Construction checks the memory layout the SDK relies on (sizes and, for
/// H.264 and HEVC, the packet envelope). Whether the codec bitstream itself is
/// decodable is the application's responsibility.
#[derive(Debug)]
pub struct CompressedVideoFrame {
    pub(crate) format: VideoPacketFormat,
    pub(crate) bytes: Box<[u8]>,
    pub(crate) metadata: Option<CString>,
    pub(crate) timecode: i64,
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
    /// or an envelope this module cannot send (a later header version, unknown
    /// flags, trailing bytes, or a keyframe without codec configuration).
    pub fn new(
        format: VideoPacketFormat,
        bytes: Vec<u8>,
    ) -> std::result::Result<Self, Rejected<Vec<u8>>> {
        match validate_video_packet(format, &bytes, Use::Send) {
            Ok(()) => Ok(Self {
                format,
                bytes: bytes.into_boxed_slice(),
                metadata: None,
                timecode: NDIlib_send_timecode_synthesize,
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
        mut self,
        value: impl Into<String>,
    ) -> std::result::Result<Self, Rejected<Self>> {
        match frame_metadata(value.into()) {
            Ok(metadata) => {
                self.metadata = Some(metadata);
                Ok(self)
            }
            Err(error) => Err(Rejected::new(error, self)),
        }
    }

    /// Set the timecode, in 100-nanosecond units.
    pub fn with_timecode(mut self, timecode: i64) -> Self {
        self.timecode = timecode;
        self
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
        self.timecode
    }

    /// The per-frame metadata, if any.
    pub fn metadata(&self) -> Option<&str> {
        // Metadata is only ever stored from a `String` or from validated UTF-8.
        self.metadata.as_deref().and_then(|text| text.to_str().ok())
    }

    /// Take the codec bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes.into_vec()
    }

    /// Check that this frame can be sent.
    ///
    /// A frame built with [`new`](Self::new) always can. A received frame may
    /// carry an envelope from a later SDK that this module keeps but will not
    /// send.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) with the reason.
    pub fn validate_for_send(&self) -> Result<()> {
        validate_video_packet(self.format, &self.bytes, Use::Send)
    }

    /// Bytes this frame keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.bytes.len()
            + self
                .metadata
                .as_ref()
                .map_or(0, |metadata| metadata.as_bytes_with_nul().len())
    }

    /// A native descriptor borrowing this frame's bytes and metadata.
    pub(crate) fn raw(&self) -> NDIlib_video_frame_v2_t {
        let mut raw = self.format.raw();
        raw.p_data = self.bytes.as_ptr().cast_mut();
        // Construction bounds the length by MAX_PACKET_BYTES, below i32::MAX.
        raw.__bindgen_anon_1.data_size_in_bytes = self.bytes.len() as i32;
        raw.p_metadata = self
            .metadata
            .as_ref()
            .map_or(std::ptr::null(), |s| s.as_ptr());
        raw.timecode = self.timecode;
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
    let Some(codec) = format.codec.envelope_codec() else {
        if bytes.len() < 4 {
            return Err(invalid("Truncated SpeedHQ frame"));
        }
        return Ok(());
    };
    let packet = PacketEnvelope::parse(bytes)?;
    if packet.codec() != Some(codec) {
        return Err(invalid("Packet codec does not match video descriptor"));
    }
    if packet.data().is_empty() {
        return Err(invalid("Empty codec payload"));
    }
    if purpose == Use::Send {
        if !packet.is_sendable() {
            return Err(invalid(
                "Cannot send an unsupported packet header version or trailing bytes",
            ));
        }
        if packet.is_keyframe() && packet.extra_data().is_empty() {
            return Err(invalid("Keyframes require codec configuration data"));
        }
    }
    Ok(())
}

/// A compressed audio codec.
///
/// AAC travels in a packet envelope; Opus is sent as bare codec packets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioCodec {
    /// AAC in a packet envelope.
    Aac,
    /// Opus, without an envelope.
    Opus,
}

impl AudioCodec {
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
#[derive(Debug)]
pub struct CompressedAudioFrame {
    pub(crate) codec: AudioCodec,
    pub(crate) protected: bool,
    pub(crate) bytes: Box<[u8]>,
    pub(crate) sample_rate: i32,
    pub(crate) channels: i32,
    pub(crate) samples: i32,
    pub(crate) timecode: i64,
    pub(crate) metadata: Option<CString>,
}

impl CompressedAudioFrame {
    /// Take ownership of one audio frame's bytes: a packet envelope for AAC
    /// (see [`packet_envelope`]), one bare packet for Opus. `samples` is the
    /// number of samples per channel the frame decodes to.
    ///
    /// # Errors
    ///
    /// Returns the bytes with [`Error::InvalidFrame`](crate::Error::InvalidFrame)
    /// when a dimension is not positive, the bytes are empty or too large, or
    /// an AAC envelope is malformed or lacks its codec configuration.
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
            bytes: bytes.into_boxed_slice(),
            sample_rate,
            channels,
            samples,
            timecode: NDIlib_send_timecode_synthesize,
            metadata: None,
        };
        match frame.validate_for_send() {
            Ok(()) => Ok(frame),
            Err(error) => Err(Rejected::new(error, frame.bytes.into_vec())),
        }
    }

    /// Set the timecode, in 100-nanosecond units.
    pub fn with_timecode(mut self, timecode: i64) -> Self {
        self.timecode = timecode;
        self
    }

    /// Attach per-frame metadata.
    ///
    /// # Errors
    ///
    /// Returns the frame unchanged when the text is too long or contains a NUL.
    pub fn with_metadata(
        mut self,
        metadata: impl Into<String>,
    ) -> std::result::Result<Self, Rejected<Self>> {
        match frame_metadata(metadata.into()) {
            Ok(metadata) => {
                self.metadata = Some(metadata);
                Ok(self)
            }
            Err(error) => Err(Rejected::new(error, self)),
        }
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
        self.timecode
    }

    /// The per-frame metadata, if any.
    pub fn metadata(&self) -> Option<&str> {
        // Metadata is only ever stored from a `String` or from validated UTF-8.
        self.metadata.as_deref().and_then(|text| text.to_str().ok())
    }

    /// Take the codec bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes.into_vec()
    }

    /// Check that this frame can be sent.
    ///
    /// A frame built with [`new`](Self::new) always can. A received AAC frame
    /// may carry an envelope from a later SDK that this module keeps but will
    /// not send.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) with the reason.
    pub fn validate_for_send(&self) -> Result<()> {
        self.validate()?;
        if self.codec == AudioCodec::Aac {
            let packet = PacketEnvelope::parse(&self.bytes)?;
            if !packet.is_sendable() || packet.extra_data().is_empty() {
                return Err(invalid(
                    "AAC sending requires a known envelope and codec configuration",
                ));
            }
        }
        Ok(())
    }

    /// The checks that hold for received frames as well.
    pub(crate) fn validate(&self) -> Result<()> {
        if self.sample_rate <= 0
            || self.channels <= 0
            || self.samples <= 0
            || self.bytes.is_empty()
            || self.bytes.len() > MAX_PACKET_BYTES
        {
            return Err(invalid("Invalid compressed audio dimensions or length"));
        }
        if self.codec == AudioCodec::Aac {
            let packet = PacketEnvelope::parse(&self.bytes)?;
            if packet.codec() != Some(EnvelopeCodec::Aac) || packet.data().is_empty() {
                return Err(invalid("Invalid AAC packet envelope"));
            }
        }
        Ok(())
    }

    /// A native descriptor borrowing this frame's bytes and metadata.
    pub(crate) fn raw(&self) -> NDIlib_audio_frame_v3_t {
        let mut raw = NDIlib_audio_frame_v3_t {
            sample_rate: self.sample_rate,
            no_channels: self.channels,
            no_samples: self.samples,
            timecode: self.timecode,
            FourCC: self.codec.outer(self.protected),
            p_data: self.bytes.as_ptr().cast_mut(),
            p_metadata: self
                .metadata
                .as_ref()
                .map_or(std::ptr::null(), |m| m.as_ptr()),
            ..Default::default()
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
        bytes: bytes.into(),
        // SAFETY: forwarded to the caller's obligation.
        metadata: unsafe { copy_metadata(raw.p_metadata)? },
        timecode: raw.timecode,
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
        bytes: bytes.into(),
        sample_rate: raw.sample_rate,
        channels: raw.no_channels,
        samples: raw.no_samples,
        timecode: raw.timecode,
        // SAFETY: forwarded to the caller's obligation.
        metadata: unsafe { copy_metadata(raw.p_metadata)? },
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
        ($ours:expr, $native:expr) => {
            assert!($ours == u32::from_ne_bytes($native.to_ne_bytes()))
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
};

#[cfg(test)]
mod tests {
    use super::*;

    fn h264_format() -> VideoPacketFormat {
        VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap()
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
                    let frame = CompressedVideoFrame::new(format, bytes).unwrap();
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
    fn a_zero_aspect_ratio_means_square_pixels_and_is_received_as_sent() {
        let format = h264_format();
        assert_eq!(format.picture_aspect_ratio(), 0.0);
        assert_eq!(
            format.aspect_ratio(0.0).unwrap().picture_aspect_ratio(),
            0.0
        );
        assert!(format.aspect_ratio(-1.0).is_err());
        assert!(format.aspect_ratio(f32::NAN).is_err());
        assert!(format.aspect_ratio(f32::INFINITY).is_err());

        // A frame as the SDK delivers it when the source left the ratio unset.
        let bytes = packet_envelope(EnvelopeCodec::H264, 0, 0, false, &[1], &[]).unwrap();
        let frame = CompressedVideoFrame::new(format, bytes).unwrap();
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
        let bytes = packet_envelope(EnvelopeCodec::Aac, 0, 0, false, &[1], &[2]).unwrap();
        let frame = CompressedAudioFrame::new(AudioCodec::Aac, 48_000, 2, 1024, bytes)
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
    fn receiving_preserves_future_semantics_but_sending_rejects_them() {
        let mut bytes = packet_envelope(EnvelopeCodec::H264, 0, 0, false, &[1], &[]).unwrap();
        bytes[32..36].copy_from_slice(&2u32.to_ne_bytes());
        let format = h264_format();
        assert_eq!(PacketEnvelope::parse(&bytes).unwrap().flags(), 2);
        assert!(validate_video_packet(format, &bytes, Use::Receive).is_ok());
        assert!(validate_video_packet(format, &bytes, Use::Send).is_err());
        bytes[32..36].copy_from_slice(&0u32.to_ne_bytes());
        bytes[24..32].copy_from_slice(&1u64.to_ne_bytes());
        assert!(validate_video_packet(format, &bytes, Use::Receive).is_ok());
        assert!(validate_video_packet(format, &bytes, Use::Send).is_err());
    }
}
