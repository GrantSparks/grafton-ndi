//! Compressed payloads retained by the same receive guards as decoded media.
use super::*;
use crate::{
    capture::{AudioKind, Guard, VideoKind},
    frames::{FrameMetadataRef, MetadataText, ValidatedFrame},
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
    len: usize,
}

struct AudioPacketParts {
    codec: AudioCodec,
    protected: bool,
    parameters: AudioParameters,
    len: usize,
}

macro_rules! borrowed_extras {
    () => {
        /// The codec payload, borrowed in place without copying.
        pub fn data(&self) -> &[u8] {
            // SAFETY: construction validated the positive payload extent; the
            // guard retains its initialized, immutable allocation for this borrow.
            unsafe {
                std::slice::from_raw_parts(self.frame.descriptor().p_data, self.frame.parts().len)
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

impl<'rx> CompressedVideoRef<'rx> {
    pub(crate) fn new(frame: Guard<'rx, VideoKind>) -> Result<Self> {
        let frame = ValidatedFrame::validate(frame, |raw| {
            let format = VideoPacketFormat::from_sdk(raw)?;
            // SAFETY: the delivered size union names the guard's readable
            // initialized allocation; packet_bytes checks its extent.
            let bytes =
                unsafe { packet_bytes(raw.p_data, raw.__bindgen_anon_1.data_size_in_bytes)? };
            validate_video_packet(format, bytes, Use::Receive)?;
            Ok(VideoPacketParts {
                format,
                len: bytes.len(),
            })
        })?;
        Ok(Self { frame })
    }

    borrowed_extras!();
    video_envelope_accessor!();

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
            bytes: self.data().to_vec(),
            extras,
        }
    }

    /// Make the same owned copy while reusing destination payload and metadata
    /// allocations. Once capacities suffice, repeated copies allocate nothing,
    /// including when metadata alternates between present and absent.
    pub fn copy_into(&self, destination: &mut ReceivedVideoPacket) {
        let ReceivedVideoPacket {
            format,
            bytes,
            extras,
        } = destination;
        bytes.clear();
        bytes.extend_from_slice(self.data());
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
        validate_video_packet(self.format(), self.data(), Use::Send)?;
        let CompressedVideoFrame {
            format,
            bytes,
            extras,
        } = destination;
        bytes.copy_from_slice(self.data());
        extras.copy_received(self.timecode(), self.timestamp(), self.frame_metadata());
        *format = self.format();
        Ok(())
    }
}

impl<'rx> CompressedAudioRef<'rx> {
    pub(crate) fn new(frame: Guard<'rx, AudioKind>) -> Result<Self> {
        let frame = ValidatedFrame::validate(frame, |raw| {
            let (codec, protected) = AudioCodec::from_outer(raw.FourCC)
                .ok_or_else(|| invalid_frame(crate::FrameDefect::UnknownCompressedAudio))?;
            let parameters = AudioParameters::from_sdk(raw)?;
            // SAFETY: this delivered union names the guard's initialized
            // readable payload; packet_bytes checks the positive extent.
            let bytes =
                unsafe { packet_bytes(raw.p_data, raw.__bindgen_anon_1.data_size_in_bytes)? };
            validate_received_audio(codec, bytes)?;
            Ok(AudioPacketParts {
                codec,
                protected,
                parameters,
                len: bytes.len(),
            })
        })?;
        Ok(Self { frame })
    }

    borrowed_extras!();
    audio_envelope_accessor!();

    /// The compressed audio codec.
    pub fn codec(&self) -> AudioCodec {
        self.frame.parts().codec
    }

    /// Whether the source stream is protected.
    pub fn is_protected(&self) -> bool {
        self.frame.parts().protected
    }

    /// Samples per second.
    pub fn sample_rate(&self) -> u32 {
        self.frame.parts().parameters.sample_rate
    }

    /// Number of audio channels.
    pub fn num_channels(&self) -> u32 {
        self.frame.parts().parameters.num_channels
    }

    /// Decoded samples per channel represented by this packet.
    pub fn num_samples(&self) -> u32 {
        self.frame.parts().parameters.num_samples
    }

    /// Copy payload and readable metadata into independently owned storage.
    /// Withheld metadata keeps its status without retaining unreadable text.
    pub fn to_owned(&self) -> ReceivedAudioPacket {
        let mut extras = FrameExtras::default();
        extras.copy_received(self.timecode(), self.timestamp(), self.frame_metadata());
        ReceivedAudioPacket {
            codec: self.codec(),
            protected: self.is_protected(),
            parameters: self.frame.parts().parameters,
            bytes: self.data().to_vec(),
            extras,
        }
    }

    /// Reuse destination payload and metadata storage for the same copy as
    /// [`Self::to_owned`]. Sufficient capacities permit allocation-free copies.
    pub fn copy_into(&self, destination: &mut ReceivedAudioPacket) {
        let ReceivedAudioPacket {
            codec,
            protected,
            parameters,
            bytes,
            extras,
        } = destination;
        bytes.clear();
        bytes.extend_from_slice(self.data());
        extras.copy_received(self.timecode(), self.timestamp(), self.frame_metadata());
        *codec = self.codec();
        *protected = self.is_protected();
        *parameters = self.frame.parts().parameters;
    }

    /// Copy into sendable storage after checking this borrowed packet.
    /// A refusal leaves every destination field unchanged. Sufficient
    /// capacities permit allocation-free copying.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when this packet cannot be sent.
    pub fn try_copy_into(&self, destination: &mut CompressedAudioFrame) -> Result<()> {
        validate_audio_for_send(self.codec(), self.frame.parts().parameters, self.data())?;
        let CompressedAudioFrame {
            codec,
            protected,
            parameters,
            bytes,
            extras,
        } = destination;
        bytes.copy_from_slice(self.data());
        extras.copy_received(self.timecode(), self.timestamp(), self.frame_metadata());
        *codec = self.codec();
        *protected = self.is_protected();
        *parameters = self.frame.parts().parameters;
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
        validate_video_packet(packet.format(), packet.data(), Use::Send)?;
        // The same borrowed packet was admitted above; no second check or copy.
        let ReceivedVideoPacket {
            format,
            bytes,
            extras,
        } = packet.to_owned();
        Ok(Self {
            format,
            bytes: AliasableVec::new(bytes),
            extras,
        })
    }
}

impl TryFrom<&CompressedAudioRef<'_>> for CompressedAudioFrame {
    type Error = Error;

    /// Admit the borrowed packet before allocating its owned copy.
    fn try_from(packet: &CompressedAudioRef<'_>) -> Result<Self> {
        validate_audio_for_send(
            packet.codec(),
            packet.frame.parts().parameters,
            packet.data(),
        )?;
        let ReceivedAudioPacket {
            codec,
            protected,
            parameters,
            bytes,
            extras,
        } = packet.to_owned();
        Ok(Self {
            codec,
            protected,
            parameters,
            bytes: AliasableVec::new(bytes),
            extras,
        })
    }
}
