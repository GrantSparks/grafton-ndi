//! Lossless owned receive packets, distinct from packets admitted for sending.
use super::*;

/// An owned compressed video packet copied from a receiver without loss.
///
/// Later envelope versions, flags and trailing bytes remain intact. Convert
/// with [`CompressedVideoFrame::try_from`] before sending; a refusal returns
/// this packet and its storage in [`Rejected`].
#[derive(Clone, Debug)]
pub struct ReceivedVideoPacket {
    pub(crate) format: VideoPacketFormat,
    pub(crate) bytes: Vec<u8>,
    pub(crate) extras: FrameExtras,
}

impl ReceivedVideoPacket {
    frame_extras_accessors!();

    /// The complete codec bytes, including retained envelope extensions.
    pub fn data(&self) -> &[u8] {
        &self.bytes
    }

    /// The codec and video format delivered by the receiver.
    pub fn format(&self) -> VideoPacketFormat {
        self.format
    }

    /// The sender's submission time, when supplied by the SDK.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        self.extras.timestamp()
    }

    /// Take the original byte allocation and its capacity.
    pub fn into_data(self) -> Vec<u8> {
        self.bytes
    }

    /// Check whether this lossless packet can be admitted for sending.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when the envelope or codec parameters are not
    /// supported for sending. The packet remains unchanged.
    pub fn validate_for_send(&self) -> Result<()> {
        validate_video_packet(self.format, &self.bytes, Use::Send)
    }
}

impl TryFrom<ReceivedVideoPacket> for CompressedVideoFrame {
    type Error = Rejected<ReceivedVideoPacket>;

    fn try_from(packet: ReceivedVideoPacket) -> std::result::Result<Self, Self::Error> {
        if let Err(error) = packet.validate_for_send() {
            return Err(Rejected::new(error, packet));
        }
        let ReceivedVideoPacket {
            format,
            bytes,
            extras,
        } = packet;
        Ok(Self {
            format,
            bytes: AliasableVec::new(bytes),
            extras,
        })
    }
}

impl From<CompressedVideoFrame> for ReceivedVideoPacket {
    fn from(frame: CompressedVideoFrame) -> Self {
        let CompressedVideoFrame {
            format,
            bytes,
            extras,
        } = frame;
        Self {
            format,
            bytes: bytes.into_vec(),
            extras,
        }
    }
}

/// An owned compressed audio packet copied from a receiver without loss.
///
/// Convert with [`CompressedAudioFrame::try_from`] before sending. Unknown
/// envelope details remain available for inspection and storage even when
/// send admission refuses the packet.
#[derive(Clone, Debug)]
pub struct ReceivedAudioPacket {
    pub(crate) codec: AudioCodec,
    pub(crate) protected: bool,
    pub(crate) parameters: AudioParameters,
    pub(crate) bytes: Vec<u8>,
    pub(crate) extras: FrameExtras,
}

impl ReceivedAudioPacket {
    frame_extras_accessors!();

    /// The complete codec bytes, including retained envelope extensions.
    pub fn data(&self) -> &[u8] {
        &self.bytes
    }

    /// The codec delivered by the receiver.
    pub fn codec(&self) -> AudioCodec {
        self.codec
    }

    /// Whether the packet belongs to a protected stream.
    pub fn is_protected(&self) -> bool {
        self.protected
    }

    /// Set whether this packet belongs to a protected stream.
    pub fn set_protected(&mut self, protected: bool) {
        self.protected = protected;
    }

    /// Sample rate in hertz.
    pub fn sample_rate(&self) -> u32 {
        self.parameters.sample_rate
    }

    /// Number of channels.
    pub fn num_channels(&self) -> u32 {
        self.parameters.num_channels
    }

    /// Samples per channel the packet decodes to.
    pub fn num_samples(&self) -> u32 {
        self.parameters.num_samples
    }

    /// The sender's submission time, when supplied by the SDK.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        self.extras.timestamp()
    }

    /// Take the original byte allocation and its capacity.
    pub fn into_data(self) -> Vec<u8> {
        self.bytes
    }

    /// Check whether this packet's codec parameters and envelope can be sent.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] for unsupported delivery parameters or envelope
    /// details. The packet remains unchanged.
    pub fn validate_for_send(&self) -> Result<()> {
        validate_audio_for_send(self.codec, self.parameters, &self.bytes)
    }
}

impl TryFrom<ReceivedAudioPacket> for CompressedAudioFrame {
    type Error = Rejected<ReceivedAudioPacket>;

    fn try_from(packet: ReceivedAudioPacket) -> std::result::Result<Self, Self::Error> {
        if let Err(error) = packet.validate_for_send() {
            return Err(Rejected::new(error, packet));
        }
        let ReceivedAudioPacket {
            codec,
            protected,
            parameters,
            bytes,
            extras,
        } = packet;
        Ok(Self {
            codec,
            protected,
            parameters,
            bytes: AliasableVec::new(bytes),
            extras,
        })
    }
}

impl From<CompressedAudioFrame> for ReceivedAudioPacket {
    fn from(frame: CompressedAudioFrame) -> Self {
        let CompressedAudioFrame {
            codec,
            protected,
            parameters,
            bytes,
            extras,
        } = frame;
        Self {
            codec,
            protected,
            parameters,
            bytes: bytes.into_vec(),
            extras,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{capture::Guard, test_alloc::allocation_count};

    fn video() -> CompressedVideoFrame {
        CompressedVideoFrame::new(
            VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap(),
            PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, false, &[1; 8], &[]).unwrap(),
        )
        .unwrap()
    }

    fn audio() -> CompressedAudioFrame {
        CompressedAudioFrame::new(
            AudioCodec::Aac,
            48_000,
            2,
            1024,
            testing::aac_packet(48_000, 2, &[1; 8]),
        )
        .unwrap()
    }

    #[test]
    fn moving_conversions_preserve_allocations_and_metadata() {
        let mut sent = video();
        sent.set_metadata("<video/>").unwrap();
        let pointer = sent.data().as_ptr();
        let metadata = sent.metadata().unwrap().as_ptr();
        let (received, allocations) = allocation_count(|| ReceivedVideoPacket::from(sent));
        assert_eq!(allocations, 0);
        let (sent, allocations) =
            allocation_count(|| CompressedVideoFrame::try_from(received).unwrap());
        assert_eq!(allocations, 0);
        assert_eq!(sent.data().as_ptr(), pointer);
        assert_eq!(sent.metadata().unwrap().as_ptr(), metadata);
        let (received, allocations) = allocation_count(|| ReceivedVideoPacket::from(sent));
        assert_eq!(allocations, 0);
        assert_eq!(received.data().as_ptr(), pointer);

        let mut sent = audio();
        sent.set_metadata("<audio/>").unwrap();
        sent.set_protected(true);
        let pointer = sent.data().as_ptr();
        let metadata = sent.metadata().unwrap().as_ptr();
        let (received, allocations) = allocation_count(|| ReceivedAudioPacket::from(sent));
        assert_eq!(allocations, 0);
        let (sent, allocations) =
            allocation_count(|| CompressedAudioFrame::try_from(received).unwrap());
        assert_eq!(allocations, 0);
        assert!(sent.is_protected());
        assert_eq!(sent.metadata().unwrap().as_ptr(), metadata);
        let (received, allocations) = allocation_count(|| ReceivedAudioPacket::from(sent));
        assert_eq!(allocations, 0);
        assert_eq!(received.data().as_ptr(), pointer);
    }

    #[test]
    fn received_packet_metadata_refusals_preserve_both_allocations() {
        fn returned(text: String, refusal: impl FnOnce(String) -> Rejected<String>) {
            let pointer = text.as_ptr();
            let capacity = text.capacity();
            let length = text.len();
            let text = refusal(text).into_inner();
            assert_eq!(
                (text.as_ptr(), text.capacity(), text.len()),
                (pointer, capacity, length)
            );
        }
        for text in [
            "bad\0metadata".to_owned(),
            "x".repeat(crate::frames::MAX_METADATA_BYTES),
        ] {
            let mut video = ReceivedVideoPacket::from(video());
            video.set_metadata("<old/>").unwrap();
            let pointer = video.metadata().unwrap().as_ptr();
            returned(text.clone(), |text| video.set_metadata(text).unwrap_err());
            assert_eq!(video.metadata(), Some("<old/>"));
            assert_eq!(video.metadata().unwrap().as_ptr(), pointer);
            let mut audio = ReceivedAudioPacket::from(audio());
            audio.set_metadata("<old/>").unwrap();
            let pointer = audio.metadata().unwrap().as_ptr();
            returned(text, |text| audio.set_metadata(text).unwrap_err());
            assert_eq!(audio.metadata(), Some("<old/>"));
            assert_eq!(audio.metadata().unwrap().as_ptr(), pointer);
        }
    }

    fn later_header(mut bytes: Vec<u8>) -> Vec<u8> {
        bytes.splice(ENVELOPE_HEADER_BYTES..ENVELOPE_HEADER_BYTES, [7; 4]);
        bytes[..4].copy_from_slice(&48i32.to_ne_bytes());
        bytes
    }

    #[test]
    fn later_video_headers_roundtrip_and_refuse_before_copying() {
        let source = video();
        let bytes = later_header(source.into_data());
        let mut raw = VideoPacketFormat::new(VideoCodec::H264, 32, 24)
            .unwrap()
            .raw();
        raw.p_data = bytes.as_ptr().cast_mut();
        raw.__bindgen_anon_1.data_size_in_bytes = bytes.len() as i32;
        raw.p_metadata = c"<received/>".as_ptr();
        // SAFETY: bytes and the terminated metadata remain live until the guard drops.
        let guard = unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
        let borrowed = CompressedVideoRef::new(guard).unwrap();
        let received = borrowed.to_owned();
        assert_eq!(received.data(), bytes);
        let pointer = received.data().as_ptr();
        let refused = CompressedVideoFrame::try_from(received)
            .unwrap_err()
            .into_inner();
        assert_eq!(refused.data().as_ptr(), pointer);
        assert_eq!(refused.data(), bytes);
        assert_eq!(refused.metadata(), Some("<received/>"));
        let mut destination = video();
        destination.set_metadata("<original/>").unwrap();
        let original = destination.clone();
        let pointer = destination.data().as_ptr();
        assert!(borrowed.try_copy_into(&mut destination).is_err());
        assert!(CompressedVideoFrame::try_from(&borrowed).is_err());
        assert_eq!(destination.data(), original.data());
        assert_eq!(destination.data().as_ptr(), pointer);
        assert_eq!(destination.format(), original.format());
        assert_eq!(destination.metadata(), original.metadata());
        assert_eq!(destination.timestamp(), original.timestamp());
    }

    #[test]
    fn later_audio_headers_roundtrip_and_refuse_before_copying() {
        let mut source = audio();
        let mut raw = source.raw();
        let bytes = later_header(source.into_data());
        raw.p_data = bytes.as_ptr().cast_mut();
        raw.__bindgen_anon_1.data_size_in_bytes = bytes.len() as i32;
        raw.no_channels = 6;
        raw.p_metadata = c"<received/>".as_ptr();
        // SAFETY: bytes and terminated metadata remain live until the guard drops.
        let guard = unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
        let borrowed = CompressedAudioRef::new(guard).unwrap();
        let received = borrowed.to_owned();
        assert_eq!(received.data(), bytes);
        let pointer = received.data().as_ptr();
        let refused = CompressedAudioFrame::try_from(received)
            .unwrap_err()
            .into_inner();
        assert_eq!(refused.data().as_ptr(), pointer);
        assert_eq!(refused.data(), bytes);
        let mut destination = audio();
        destination.set_metadata("<original/>").unwrap();
        let original = destination.clone();
        let pointer = destination.data().as_ptr();
        assert!(borrowed.try_copy_into(&mut destination).is_err());
        assert!(CompressedAudioFrame::try_from(&borrowed).is_err());
        assert_eq!(destination.data(), original.data());
        assert_eq!(destination.data().as_ptr(), pointer);
        assert_eq!(destination.num_channels(), original.num_channels());
        assert_eq!(destination.metadata(), original.metadata());
        assert_eq!(destination.timestamp(), original.timestamp());
    }

    #[test]
    fn borrowed_send_copies_reuse_capacity_and_replace_all_observations() {
        let mut source = video();
        source.set_metadata("<new/>").unwrap();
        source.set_timecode(Timecode::from_ticks(123));
        let mut raw = source.raw();
        raw.timestamp = 456;
        // SAFETY: source storage remains live and unchanged through the borrow.
        let guard = unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
        let borrowed = CompressedVideoRef::new(guard).unwrap();
        let mut destination = video();
        destination.set_metadata("<long-original/>").unwrap();
        let pointer = destination.data().as_ptr();
        let (result, allocations) = allocation_count(|| borrowed.try_copy_into(&mut destination));
        result.unwrap();
        assert_eq!(allocations, 0);
        assert_eq!(destination.data().as_ptr(), pointer);
        assert_eq!(destination.format(), borrowed.format());
        assert_eq!(destination.timecode(), borrowed.timecode());
        assert_eq!(destination.timestamp(), borrowed.timestamp());
        assert_eq!(destination.metadata(), borrowed.metadata());
        assert_eq!(destination.metadata_status(), borrowed.metadata_status());

        let mut source =
            CompressedAudioFrame::new(AudioCodec::Opus, 24_000, 6, 480, vec![1; 8]).unwrap();
        source.set_protected(true);
        source.set_timecode(Timecode::from_ticks(789));
        let mut raw = source.raw();
        raw.timestamp = 987;
        // SAFETY: source storage remains live and unchanged through the borrow.
        let guard = unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
        let borrowed = CompressedAudioRef::new(guard).unwrap();
        let mut destination = audio();
        destination.set_metadata("<old/>").unwrap();
        let pointer = destination.data().as_ptr();
        let (result, allocations) = allocation_count(|| borrowed.try_copy_into(&mut destination));
        result.unwrap();
        assert_eq!(allocations, 0);
        assert_eq!(destination.data().as_ptr(), pointer);
        assert_eq!(destination.codec(), borrowed.codec());
        assert_eq!(destination.is_protected(), borrowed.is_protected());
        assert_eq!(destination.sample_rate(), borrowed.sample_rate());
        assert_eq!(destination.num_channels(), borrowed.num_channels());
        assert_eq!(destination.num_samples(), borrowed.num_samples());
        assert_eq!(destination.timecode(), borrowed.timecode());
        assert_eq!(destination.timestamp(), borrowed.timestamp());
        assert_eq!(destination.metadata(), borrowed.metadata());
        assert_eq!(destination.metadata_status(), borrowed.metadata_status());
    }
}
