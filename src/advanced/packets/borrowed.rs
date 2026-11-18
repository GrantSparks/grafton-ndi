//! Compressed payloads retained by the same receive guards as decoded media.
use super::*;
use crate::{
    capture::{AudioKind, Guard, VideoKind},
    frames::{
        descriptor_parts_sealed, DescriptorParts, FrameMetadataRef, MetadataText, ValidatedFrame,
    },
    FrameMetadataStatus,
};

/// A borrowed compressed video frame captured by a packet receiver.
///
/// The receive guard keeps payload and metadata in SDK or application-allocated
/// memory until this reference is dropped. Capture validates once and allocates
/// nothing. Keeping the reference retains a custom allocator's buffer and its
/// usage charge. Copy into an owned frame before sending: the SDK may write to
/// sent packets. The reference is `Send + Sync`, like decoded receiver frames.
pub struct CompressedVideoRef<'rx> {
    frame: ValidatedFrame<Guard<'rx, VideoKind>, VideoPacketParts>,
}

/// Borrowed compressed audio, with the ownership rules of [`CompressedVideoRef`].
pub struct CompressedAudioRef<'rx> {
    frame: ValidatedFrame<Guard<'rx, AudioKind>, AudioPacketParts>,
}

struct VideoPacketParts {
    format: VideoPacketFormat,
    len: PacketLen,
    extents: Option<EnvelopeExtents>,
}

struct AudioPacketParts {
    format: AudioPacketFormat,
    len: PacketLen,
    extents: Option<EnvelopeExtents>,
}

macro_rules! borrowed_extras {
    () => {
        /// The codec payload, borrowed in place without copying.
        pub fn data(&self) -> &[u8] {
            self.packet_slice().as_slice()
        }

        fn packet_slice(&self) -> PacketSlice<'_> {
            // SAFETY: construction validated the positive payload extent; the
            // guard retains its initialized, immutable allocation for this borrow.
            unsafe {
                PacketSlice::from_validated(self.frame.descriptor().p_data, self.frame.parts().len)
            }
        }

        /// The frame timecode, as delivered by the source.
        pub fn timecode(&self) -> Timecode {
            Timecode::from_ticks(self.frame.descriptor().timecode)
        }

        /// The sender's submission time, when the SDK supplies it.
        pub fn timestamp(&self) -> Option<SdkTimestamp> {
            self.frame.timestamp()
        }

        /// Optional UTF-8 metadata. Unreadable text is withheld without losing
        /// the packet; [`Self::metadata_status`] reports its condition.
        pub fn metadata(&self) -> Option<&str> {
            self.frame_metadata().text().map(MetadataText::as_str)
        }

        /// Cached metadata presence or the reason unreadable text was withheld.
        pub fn metadata_status(&self) -> FrameMetadataStatus {
            self.frame_metadata().status()
        }

        fn frame_metadata(&self) -> FrameMetadataRef<'_> {
            self.frame.frame_metadata()
        }
    };
}

impl descriptor_parts_sealed::Sealed for VideoPacketParts {}
// SAFETY: the codec, parameters and payload length are validated from this
// exact descriptor. No returned extent can exceed its declared allocation.
unsafe impl DescriptorParts<NDIlib_video_frame_v2_t> for VideoPacketParts {
    unsafe fn validate(raw: &NDIlib_video_frame_v2_t) -> Result<Self> {
        let format = VideoPacketFormat::from_sdk(raw)?;
        // SAFETY: the delivered size union names the guard's readable
        // initialized allocation; PacketSlice checks its extent.
        let bytes =
            unsafe { PacketSlice::from_sdk(raw.p_data, raw.__bindgen_anon_1.data_size_in_bytes)? };
        let extents = validate_video_content(format, bytes, Use::Receive)?;
        Ok(Self {
            format,
            len: bytes.len(),
            extents,
        })
    }
}

impl<'rx> CompressedVideoRef<'rx> {
    pub(crate) fn new(frame: Guard<'rx, VideoKind>) -> Result<Self> {
        let frame = ValidatedFrame::validate(frame)?;
        Ok(Self { frame })
    }

    borrowed_extras!();
    envelope_accessor!(frame.parts().extents);

    /// The packet's codec and video format.
    pub fn format(&self) -> VideoPacketFormat {
        self.frame.parts().format
    }

    /// Copy payload and readable metadata into independently owned storage.
    /// Withheld metadata keeps its status without retaining unreadable text.
    pub fn to_owned(&self) -> ReceivedVideoPacket {
        let mut extras = FrameExtras::default();
        extras.copy_received(self.timecode(), self.timestamp(), self.frame_metadata());
        ReceivedVideoPacket {
            format: self.format(),
            payload: PacketBytes::copied(self.packet_slice(), self.frame.parts().extents),
            extras,
        }
    }

    /// Make the same owned copy while reusing destination payload and metadata
    /// allocations. Once capacities suffice, repeated copies allocate nothing,
    /// including when metadata alternates between present and absent.
    pub fn copy_into(&self, destination: &mut ReceivedVideoPacket) {
        let ReceivedVideoPacket {
            format,
            payload,
            extras,
        } = destination;
        payload.refill(self.packet_slice(), self.frame.parts().extents);
        extras.copy_received(self.timecode(), self.timestamp(), self.frame_metadata());
        *format = self.format();
    }

    /// Copy into sendable storage after checking this borrowed packet.
    /// A refusal leaves every field and allocation of `destination` unchanged.
    /// Sufficient capacities permit allocation-free copying.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when this packet cannot be sent.
    pub fn try_copy_into(&self, destination: &mut CompressedVideoFrame) -> Result<()> {
        let extents = validate_video_content(self.format(), self.packet_slice(), Use::Send)?;
        let CompressedVideoFrame {
            format,
            payload,
            extras,
        } = destination;
        payload.refill(self.packet_slice(), extents);
        extras.copy_received(self.timecode(), self.timestamp(), self.frame_metadata());
        *format = self.format();
        Ok(())
    }
}

impl descriptor_parts_sealed::Sealed for AudioPacketParts {}
// SAFETY: the codec, parameters and payload length are validated from this
// exact descriptor. No returned extent can exceed its declared allocation.
unsafe impl DescriptorParts<NDIlib_audio_frame_v3_t> for AudioPacketParts {
    unsafe fn validate(raw: &NDIlib_audio_frame_v3_t) -> Result<Self> {
        let format = AudioPacketFormat::from_sdk(raw)?;
        // SAFETY: this delivered union names the guard's initialized
        // readable payload; PacketSlice checks the positive extent.
        let bytes =
            unsafe { PacketSlice::from_sdk(raw.p_data, raw.__bindgen_anon_1.data_size_in_bytes)? };
        let extents = validate_received_audio(format.codec(), bytes)?;
        Ok(Self {
            format,
            len: bytes.len(),
            extents,
        })
    }
}

impl<'rx> CompressedAudioRef<'rx> {
    pub(crate) fn new(frame: Guard<'rx, AudioKind>) -> Result<Self> {
        let frame = ValidatedFrame::validate(frame)?;
        Ok(Self { frame })
    }

    borrowed_extras!();
    envelope_accessor!(frame.parts().extents);

    /// The codec and audio format delivered by the receiver.
    pub fn format(&self) -> AudioPacketFormat {
        self.frame.parts().format
    }

    /// Copy payload and readable metadata into independently owned storage.
    /// Withheld metadata keeps its status without retaining unreadable text.
    pub fn to_owned(&self) -> ReceivedAudioPacket {
        let mut extras = FrameExtras::default();
        extras.copy_received(self.timecode(), self.timestamp(), self.frame_metadata());
        ReceivedAudioPacket {
            format: self.format(),
            payload: PacketBytes::copied(self.packet_slice(), self.frame.parts().extents),
            extras,
        }
    }

    /// Reuse destination payload and metadata storage for the same copy as
    /// [`Self::to_owned`]. Sufficient capacities permit allocation-free copies.
    pub fn copy_into(&self, destination: &mut ReceivedAudioPacket) {
        let ReceivedAudioPacket {
            format,
            payload,
            extras,
        } = destination;
        payload.refill(self.packet_slice(), self.frame.parts().extents);
        extras.copy_received(self.timecode(), self.timestamp(), self.frame_metadata());
        *format = self.format();
    }

    /// Copy into sendable storage after checking this borrowed packet.
    /// A refusal leaves every destination field unchanged. Sufficient
    /// capacities permit allocation-free copying.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when this packet cannot be sent.
    pub fn try_copy_into(&self, destination: &mut CompressedAudioFrame) -> Result<()> {
        let extents = validate_audio_content_for_send(self.format(), self.packet_slice())?;
        let CompressedAudioFrame {
            format,
            payload,
            extras,
        } = destination;
        payload.refill(self.packet_slice(), extents);
        extras.copy_received(self.timecode(), self.timestamp(), self.frame_metadata());
        *format = self.format();
        Ok(())
    }
}

packet_debug!(CompressedVideoRef<'_>, "CompressedVideoRef", frame, video, frame.timestamp();
    "bytes" = frame.data().len());
packet_debug!(CompressedAudioRef<'_>, "CompressedAudioRef", frame, audio, frame.timestamp();
    "bytes" = frame.data().len());

impl TryFrom<&CompressedVideoRef<'_>> for CompressedVideoFrame {
    type Error = Error;

    /// Admit the borrowed packet before allocating its owned copy.
    fn try_from(packet: &CompressedVideoRef<'_>) -> Result<Self> {
        let extents = validate_video_content(packet.format(), packet.packet_slice(), Use::Send)?;
        // The same borrowed packet was admitted above; no second check or copy.
        let ReceivedVideoPacket {
            format,
            payload,
            extras,
        } = packet.to_owned();
        Ok(Self {
            format,
            payload: payload.with_envelope(extents),
            extras,
        })
    }
}

impl TryFrom<&CompressedAudioRef<'_>> for CompressedAudioFrame {
    type Error = Error;

    /// Admit the borrowed packet before allocating its owned copy.
    fn try_from(packet: &CompressedAudioRef<'_>) -> Result<Self> {
        let extents = validate_audio_content_for_send(packet.format(), packet.packet_slice())?;
        let ReceivedAudioPacket {
            format,
            payload,
            extras,
        } = packet.to_owned();
        Ok(Self {
            format,
            payload: payload.with_envelope(extents),
            extras,
        })
    }
}
