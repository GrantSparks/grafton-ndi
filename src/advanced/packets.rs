use super::ffi::{self, invalid};
use crate::{advanced::ffi::native::*, Result, ScanType};
use std::ffi::CString;

/// Maximum compressed frame bytes accepted by the default packet constructors.
pub const MAX_PACKET_BYTES: usize = 256 * 1024 * 1024;
const HEADER_BYTES: usize = 44;

const fn fourcc(bytes: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*bytes)
}

/// Codec identity; compressed bytes are never interpreted as pixel rows or floats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum VideoCodec {
    SpeedHq420,
    SpeedHq422,
    SpeedHq422Alpha,
    H264,
    Hevc,
    H264Alpha,
    HevcAlpha,
}

/// Program and preview have independent native send lifetimes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamTier {
    Program,
    Preview,
}

impl VideoCodec {
    pub(crate) fn outer(self, tier: StreamTier, protected: bool) -> u32 {
        let bytes = match (self, tier) {
            (Self::SpeedHq420, StreamTier::Program) => *b"SHQ0",
            (Self::SpeedHq420, StreamTier::Preview) => *b"shq0",
            (Self::SpeedHq422, StreamTier::Program) => *b"SHQ2",
            (Self::SpeedHq422, StreamTier::Preview) => *b"shq2",
            (Self::SpeedHq422Alpha, StreamTier::Program) => *b"SHQ7",
            (Self::SpeedHq422Alpha, StreamTier::Preview) => *b"shq7",
            (Self::H264, StreamTier::Program) => *b"H264",
            (Self::H264, StreamTier::Preview) => *b"h264",
            (Self::Hevc, StreamTier::Program) => *b"HEVC",
            (Self::Hevc, StreamTier::Preview) => *b"hevc",
            (Self::H264Alpha, StreamTier::Program) => *b"A264",
            (Self::H264Alpha, StreamTier::Preview) => *b"a264",
            (Self::HevcAlpha, StreamTier::Program) => *b"AEVC",
            (Self::HevcAlpha, StreamTier::Preview) => *b"aevc",
        };
        fourcc(&bytes) | if protected { 0x8080_8080 } else { 0 }
    }
    pub(crate) fn from_outer(value: u32) -> Option<(Self, StreamTier, bool)> {
        let protected = value & 0x8080_8080 == 0x8080_8080;
        for codec in [
            Self::SpeedHq420,
            Self::SpeedHq422,
            Self::SpeedHq422Alpha,
            Self::H264,
            Self::Hevc,
            Self::H264Alpha,
            Self::HevcAlpha,
        ] {
            for tier in [StreamTier::Program, StreamTier::Preview] {
                if value == codec.outer(tier, protected) {
                    return Some((codec, tier, protected));
                }
            }
        }
        None
    }
    pub(crate) fn envelope_codec(self) -> Option<u32> {
        match self {
            Self::H264 | Self::H264Alpha => Some(fourcc(b"H264")),
            Self::Hevc | Self::HevcAlpha => Some(fourcc(b"HEVC")),
            _ => None,
        }
    }
}

/// Bounds-checked view of an NDI compressed packet header and payload.
/// Unknown flags and header extension bytes are preserved when receiving.
#[derive(Clone, Copy, Debug)]
pub struct PacketEnvelope<'a> {
    bytes: &'a [u8],
    header: usize,
    data_len: usize,
    extra_len: usize,
}

impl<'a> PacketEnvelope<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() < HEADER_BYTES || bytes.len() > MAX_PACKET_BYTES {
            return Err(invalid("Invalid compressed packet length"));
        }
        let header = i32::from_ne_bytes(bytes[0..4].try_into().unwrap());
        if header < HEADER_BYTES as i32 {
            return Err(invalid("Unsupported compressed packet header size"));
        }
        let header = header as usize;
        let data_len = u32::from_ne_bytes(bytes[36..40].try_into().unwrap()) as usize;
        let extra_len = u32::from_ne_bytes(bytes[40..44].try_into().unwrap()) as usize;
        let end = header
            .checked_add(data_len)
            .and_then(|n| n.checked_add(extra_len))
            .ok_or_else(|| invalid("Compressed packet length overflow"))?;
        if end > bytes.len() {
            return Err(invalid("Truncated compressed packet"));
        }
        Ok(Self {
            bytes,
            header,
            data_len,
            extra_len,
        })
    }
    pub fn codec_code(self) -> u32 {
        u32::from_ne_bytes(self.bytes[4..8].try_into().unwrap())
    }
    pub fn presentation_time(self) -> i64 {
        i64::from_ne_bytes(self.bytes[8..16].try_into().unwrap())
    }
    pub fn decode_time(self) -> i64 {
        i64::from_ne_bytes(self.bytes[16..24].try_into().unwrap())
    }
    pub fn flags(self) -> u32 {
        u32::from_ne_bytes(self.bytes[32..36].try_into().unwrap())
    }
    pub fn reserved(self) -> u64 {
        u64::from_ne_bytes(self.bytes[24..32].try_into().unwrap())
    }
    pub fn header_extension(self) -> &'a [u8] {
        &self.bytes[HEADER_BYTES..self.header]
    }
    pub fn data(self) -> &'a [u8] {
        &self.bytes[self.header..self.header + self.data_len]
    }
    pub fn extra_data(self) -> &'a [u8] {
        &self.bytes[self.header + self.data_len..self.header + self.data_len + self.extra_len]
    }
    pub fn trailing_bytes(self) -> &'a [u8] {
        &self.bytes[self.header + self.data_len + self.extra_len..]
    }
    pub fn is_known_version(self) -> bool {
        self.header == HEADER_BYTES
    }
}

/// Construct a current-version H.264, HEVC or AAC envelope without packed references.
pub fn packet_envelope(
    codec_code: u32,
    pts: i64,
    dts: i64,
    keyframe: bool,
    data: &[u8],
    extra_data: &[u8],
) -> Result<Vec<u8>> {
    if ![fourcc(b"H264"), fourcc(b"HEVC"), 0xff].contains(&codec_code) {
        return Err(invalid("Unknown packet envelope codec"));
    }
    let size = HEADER_BYTES
        .checked_add(data.len())
        .and_then(|n| n.checked_add(extra_data.len()))
        .ok_or_else(|| invalid("Packet size overflow"))?;
    if data.is_empty() || size > MAX_PACKET_BYTES {
        return Err(invalid("Invalid compressed packet payload size"));
    }
    let mut bytes = vec![0; size];
    bytes[0..4].copy_from_slice(&(HEADER_BYTES as i32).to_ne_bytes());
    bytes[4..8].copy_from_slice(&codec_code.to_ne_bytes());
    bytes[8..16].copy_from_slice(&pts.to_ne_bytes());
    bytes[16..24].copy_from_slice(&dts.to_ne_bytes());
    bytes[32..36].copy_from_slice(&u32::from(keyframe).to_ne_bytes());
    bytes[36..40].copy_from_slice(&(data.len() as u32).to_ne_bytes());
    bytes[40..44].copy_from_slice(&(extra_data.len() as u32).to_ne_bytes());
    bytes[HEADER_BYTES..HEADER_BYTES + data.len()].copy_from_slice(data);
    bytes[HEADER_BYTES + data.len()..].copy_from_slice(extra_data);
    Ok(bytes)
}

/// Validated compressed-video timing and geometry, independent of payload ownership.
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
            aspect: width as f32 / height as f32,
            scan: ScanType::Progressive,
        })
    }
    pub fn frame_rate(mut self, numerator: i32, denominator: i32) -> Result<Self> {
        if numerator <= 0 || denominator <= 0 {
            return Err(invalid("Frame rate must be positive"));
        }
        self.rate_n = numerator;
        self.rate_d = denominator;
        Ok(self)
    }
    pub fn aspect_ratio(mut self, value: f32) -> Result<Self> {
        if !value.is_finite() || value <= 0.0 {
            return Err(invalid("Aspect ratio must be finite and positive"));
        }
        self.aspect = value;
        Ok(self)
    }
    pub fn tier(mut self, tier: StreamTier) -> Self {
        self.tier = tier;
        self
    }
    pub fn scan_type(mut self, scan: ScanType) -> Self {
        self.scan = scan;
        self
    }
    pub fn protected(mut self, protected: bool) -> Self {
        self.protected = protected;
        self
    }
    pub fn codec(self) -> VideoCodec {
        self.codec
    }
    pub fn stream_tier(self) -> StreamTier {
        self.tier
    }
    pub fn width(self) -> i32 {
        self.width
    }
    pub fn height(self) -> i32 {
        self.height
    }
    pub(crate) fn raw(self) -> NDIlib_video_frame_v2_t {
        NDIlib_video_frame_v2_t {
            xres: self.width,
            yres: self.height,
            FourCC: ffi::video_fourcc_native(self.codec.outer(self.tier, self.protected)),
            frame_rate_N: self.rate_n,
            frame_rate_D: self.rate_d,
            picture_aspect_ratio: self.aspect,
            frame_format_type: self.scan.into(),
            timecode: NDIlib_send_timecode_synthesize,
            ..Default::default()
        }
    }
}

/// Owned compressed video. Constructor validation covers its native memory envelope.
/// Applications remain responsible for supplying an interoperable codec bitstream.
#[derive(Debug)]
pub struct CompressedVideoFrame {
    pub(crate) format: VideoPacketFormat,
    pub(crate) bytes: Box<[u8]>,
    pub(crate) metadata: Option<CString>,
    pub(crate) timecode: i64,
}

impl CompressedVideoFrame {
    pub fn new(format: VideoPacketFormat, bytes: Vec<u8>) -> Result<Self> {
        validate_video_packet(format, &bytes, true)?;
        Ok(Self {
            format,
            bytes: bytes.into_boxed_slice(),
            metadata: None,
            timecode: NDIlib_send_timecode_synthesize,
        })
    }
    pub fn with_metadata(mut self, value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() >= crate::frames::MAX_METADATA_BYTES {
            return Err(invalid("Packet metadata exceeds its size limit"));
        }
        self.metadata = Some(CString::new(value)?);
        Ok(self)
    }
    pub fn with_timecode(mut self, timecode: i64) -> Self {
        self.timecode = timecode;
        self
    }
    pub fn data(&self) -> &[u8] {
        &self.bytes
    }
    pub fn format(&self) -> VideoPacketFormat {
        self.format
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes.into_vec()
    }
    /// Check whether this received packet can be sent by this wrapper version.
    pub fn validate_for_send(&self) -> Result<()> {
        validate_video_packet(self.format, &self.bytes, true)
    }
    pub(crate) fn raw(&self) -> NDIlib_video_frame_v2_t {
        let mut raw = self.format.raw();
        raw.p_data = self.bytes.as_ptr().cast_mut();
        raw.__bindgen_anon_1.data_size_in_bytes = self.bytes.len() as i32;
        raw.p_metadata = self
            .metadata
            .as_ref()
            .map_or(std::ptr::null(), |s| s.as_ptr());
        raw.timecode = self.timecode;
        raw
    }
}

pub(crate) fn validate_video_packet(
    format: VideoPacketFormat,
    bytes: &[u8],
    sending: bool,
) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_PACKET_BYTES || bytes.len() > i32::MAX as usize {
        return Err(invalid("Invalid compressed video size"));
    }
    if let Some(codec) = format.codec.envelope_codec() {
        let packet = PacketEnvelope::parse(bytes)?;
        if packet.codec_code() != codec {
            return Err(invalid("Packet codec does not match video descriptor"));
        }
        if sending
            && (!packet.is_known_version()
                || !packet.trailing_bytes().is_empty()
                || packet.flags() & !1 != 0
                || packet.reserved() != 0)
        {
            return Err(invalid(
                "Cannot send an unsupported packet header version or trailing bytes",
            ));
        }
        if packet.data().is_empty() {
            return Err(invalid("Empty codec payload"));
        }
        if sending && packet.flags() & 1 != 0 && packet.extra_data().is_empty() {
            return Err(invalid("Keyframes require codec configuration data"));
        }
    } else if bytes.len() < 4 {
        return Err(invalid("Truncated SpeedHQ frame"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packed_header_parsing_preserves_extensions_and_checks_bounds() {
        let mut bytes = packet_envelope(fourcc(b"H264"), 12, -4, true, &[1, 2], &[3]).unwrap();
        let p = PacketEnvelope::parse(&bytes).unwrap();
        assert_eq!(p.presentation_time(), 12);
        assert_eq!(p.decode_time(), -4);
        assert_eq!(p.data(), &[1, 2]);
        assert_eq!(p.extra_data(), &[3]);
        bytes.splice(44..44, [9, 8]);
        bytes[..4].copy_from_slice(&46i32.to_ne_bytes());
        assert_eq!(
            PacketEnvelope::parse(&bytes).unwrap().header_extension(),
            &[9, 8]
        );
        let format = VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap();
        assert!(validate_video_packet(format, &bytes, false).is_ok());
        assert!(CompressedVideoFrame::new(format, bytes.clone()).is_err());
        bytes[36..40].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(PacketEnvelope::parse(&bytes).is_err());
    }
    #[test]
    fn codec_outer_roundtrips_and_opus_is_not_an_envelope() {
        for codec in [
            VideoCodec::SpeedHq420,
            VideoCodec::SpeedHq422,
            VideoCodec::SpeedHq422Alpha,
            VideoCodec::H264,
            VideoCodec::Hevc,
            VideoCodec::H264Alpha,
            VideoCodec::HevcAlpha,
        ] {
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
                    assert_eq!(
                        raw.FourCC.to_ne_bytes(),
                        codec.outer(tier, protected).to_ne_bytes()
                    );
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
}

/// Compressed audio codecs have different framing: Opus has no NDI packet prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    Opus,
}

/// Owned compressed audio with checked metadata and native size conversions.
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
    pub fn new(
        codec: AudioCodec,
        sample_rate: i32,
        channels: i32,
        samples: i32,
        bytes: Vec<u8>,
    ) -> Result<Self> {
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
        frame.validate_for_send()?;
        Ok(frame)
    }
    pub fn with_timecode(mut self, timecode: i64) -> Self {
        self.timecode = timecode;
        self
    }
    pub fn with_metadata(mut self, metadata: impl Into<String>) -> Result<Self> {
        let metadata = metadata.into();
        if metadata.len() >= crate::frames::MAX_METADATA_BYTES {
            return Err(invalid("Packet metadata exceeds its size limit"));
        }
        self.metadata = Some(CString::new(metadata)?);
        Ok(self)
    }
    pub fn protected(mut self, protected: bool) -> Self {
        self.protected = protected;
        self
    }
    pub fn data(&self) -> &[u8] {
        &self.bytes
    }
    pub fn codec(&self) -> AudioCodec {
        self.codec
    }
    pub fn sample_rate(&self) -> i32 {
        self.sample_rate
    }
    pub fn channels(&self) -> i32 {
        self.channels
    }
    pub fn samples(&self) -> i32 {
        self.samples
    }
    pub fn validate_for_send(&self) -> Result<()> {
        self.validate()?;
        if self.codec == AudioCodec::Aac {
            let packet = PacketEnvelope::parse(&self.bytes)?;
            if !packet.is_known_version()
                || !packet.trailing_bytes().is_empty()
                || packet.flags() & !1 != 0
                || packet.reserved() != 0
                || packet.extra_data().is_empty()
            {
                return Err(invalid(
                    "AAC sending requires a known envelope and codec configuration",
                ));
            }
        }
        Ok(())
    }
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
            if packet.codec_code() != 0xff || packet.data().is_empty() {
                return Err(invalid("Invalid AAC packet envelope"));
            }
        }
        Ok(())
    }
    pub(crate) fn raw(&self) -> NDIlib_audio_frame_v3_t {
        let mut raw = NDIlib_audio_frame_v3_t {
            sample_rate: self.sample_rate,
            no_channels: self.channels,
            no_samples: self.samples,
            timecode: self.timecode,
            FourCC: ffi::audio_fourcc_native(self.outer()),
            p_data: self.bytes.as_ptr().cast_mut(),
            p_metadata: self
                .metadata
                .as_ref()
                .map_or(std::ptr::null(), |m| m.as_ptr()),
            ..Default::default()
        };
        raw.__bindgen_anon_1.data_size_in_bytes = self.bytes.len() as i32;
        raw
    }
    fn outer(&self) -> u32 {
        (match self.codec {
            AudioCodec::Aac => 0xff,
            AudioCodec::Opus => fourcc(b"Opus"),
        }) | if self.protected { 0x8080_8080 } else { 0 }
    }
}

pub(crate) unsafe fn copy_metadata(pointer: *const std::ffi::c_char) -> Result<Option<CString>> {
    let layout = unsafe { crate::frames::validate_frame_metadata(pointer)? };
    Ok(
        unsafe { crate::frames::frame_metadata_str(pointer, layout) }
            .map(|s| CString::new(s).expect("validated frame metadata")),
    )
}

pub(crate) unsafe fn copy_video_packet(
    raw: &NDIlib_video_frame_v2_t,
) -> Result<CompressedVideoFrame> {
    let (codec, tier, protected) = VideoCodec::from_outer(ffi::video_fourcc_bits(raw.FourCC))
        .ok_or_else(|| invalid("Unknown compressed video FourCC"))?;
    let format = VideoPacketFormat::new(codec, raw.xres, raw.yres)?
        .frame_rate(raw.frame_rate_N, raw.frame_rate_D)?
        .aspect_ratio(raw.picture_aspect_ratio)?
        .tier(tier)
        .protected(protected)
        .scan_type(
            crate::ScanType::try_from(u32::from_ne_bytes(raw.frame_format_type.to_ne_bytes()))
                .map_err(|_| invalid("Unknown packet scan type"))?,
        );
    let size = unsafe { raw.__bindgen_anon_1.data_size_in_bytes };
    if size <= 0 || size as usize > MAX_PACKET_BYTES || raw.p_data.is_null() {
        return Err(invalid("Invalid compressed video buffer"));
    }
    let bytes = unsafe { std::slice::from_raw_parts(raw.p_data, size as usize) };
    validate_video_packet(format, bytes, false)?;
    Ok(CompressedVideoFrame {
        format,
        bytes: bytes.to_vec().into_boxed_slice(),
        metadata: unsafe { copy_metadata(raw.p_metadata)? },
        timecode: raw.timecode,
    })
}

pub(crate) unsafe fn copy_audio_packet(
    raw: &NDIlib_audio_frame_v3_t,
) -> Result<CompressedAudioFrame> {
    let fourcc_bits = ffi::audio_fourcc_bits(raw.FourCC);
    let protected = fourcc_bits & 0x8080_8080 == 0x8080_8080;
    let codec = if [0xff, 0xff | 0x8080_8080].contains(&fourcc_bits) {
        AudioCodec::Aac
    } else if [fourcc(b"Opus"), fourcc(b"Opus") | 0x8080_8080].contains(&fourcc_bits) {
        AudioCodec::Opus
    } else {
        return Err(invalid("Unknown compressed audio FourCC"));
    };
    let size = unsafe { raw.__bindgen_anon_1.data_size_in_bytes };
    if size <= 0 || size as usize > MAX_PACKET_BYTES || raw.p_data.is_null() {
        return Err(invalid("Invalid compressed audio buffer"));
    }
    let frame = CompressedAudioFrame {
        codec,
        protected,
        bytes: unsafe { std::slice::from_raw_parts(raw.p_data, size as usize) }
            .to_vec()
            .into_boxed_slice(),
        sample_rate: raw.sample_rate,
        channels: raw.no_channels,
        samples: raw.no_samples,
        timecode: raw.timecode,
        metadata: unsafe { copy_metadata(raw.p_metadata)? },
    };
    frame.validate()?;
    Ok(frame)
}

#[cfg(ndi_has_advanced_header)]
const _: () = {
    use crate::advanced::ffi::native::NDIlib_compressed_packet_t as Packet;
    assert!(std::mem::size_of::<Packet>() == HEADER_BYTES);
    assert!(std::mem::offset_of!(Packet, version) == 0);
    assert!(std::mem::offset_of!(Packet, fourCC) == 4);
    assert!(std::mem::offset_of!(Packet, pts) == 8);
    assert!(std::mem::offset_of!(Packet, dts) == 16);
    assert!(std::mem::offset_of!(Packet, reserved) == 24);
    assert!(std::mem::offset_of!(Packet, flags) == 32);
    assert!(std::mem::offset_of!(Packet, data_size) == 36);
    assert!(std::mem::offset_of!(Packet, extra_data_size) == 40);
};

#[cfg(test)]
mod audio_and_future_header_tests {
    use super::*;
    #[test]
    fn protected_aac_preserves_codec_high_bit() {
        let bytes = packet_envelope(0xff, 0, 0, false, &[1], &[2]).unwrap();
        let frame = CompressedAudioFrame::new(AudioCodec::Aac, 48_000, 2, 1024, bytes)
            .unwrap()
            .protected(true);
        let raw = frame.raw();
        let copied = unsafe { copy_audio_packet(&raw) }.unwrap();
        assert_eq!(copied.codec(), AudioCodec::Aac);
        assert!(copied.protected);
        assert_eq!(copied.data(), frame.data());
    }
    #[test]
    fn unknown_native_enum_bits_are_rejected_without_truncation() {
        for bits in [0x8000_0000, 0x8080_8080, u32::MAX] {
            let video = NDIlib_video_frame_v2_t {
                FourCC: ffi::video_fourcc_native(bits),
                ..Default::default()
            };
            let audio = NDIlib_audio_frame_v3_t {
                FourCC: ffi::audio_fourcc_native(bits),
                ..Default::default()
            };
            assert_eq!(video.FourCC.to_ne_bytes(), bits.to_ne_bytes());
            assert_eq!(audio.FourCC.to_ne_bytes(), bits.to_ne_bytes());
            assert!(unsafe { copy_video_packet(&video) }.is_err());
            assert!(unsafe { copy_audio_packet(&audio) }.is_err());
        }
        let mut video = VideoPacketFormat::new(VideoCodec::H264, 16, 16)
            .unwrap()
            .raw();
        video.frame_format_type = NDIlib_frame_format_type_e::from_ne_bytes(u32::MAX.to_ne_bytes());
        assert!(unsafe { copy_video_packet(&video) }.is_err());
    }
    #[test]
    fn receiving_preserves_future_semantics_but_sending_rejects_them() {
        let mut bytes = packet_envelope(fourcc(b"H264"), 0, 0, false, &[1], &[]).unwrap();
        bytes[32..36].copy_from_slice(&2u32.to_ne_bytes());
        let format = VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap();
        assert_eq!(PacketEnvelope::parse(&bytes).unwrap().flags(), 2);
        assert!(validate_video_packet(format, &bytes, false).is_ok());
        assert!(validate_video_packet(format, &bytes, true).is_err());
        bytes[32..36].copy_from_slice(&0u32.to_ne_bytes());
        bytes[24..32].copy_from_slice(&1u64.to_ne_bytes());
        assert!(validate_video_packet(format, &bytes, false).is_ok());
        assert!(validate_video_packet(format, &bytes, true).is_err());
    }
}
