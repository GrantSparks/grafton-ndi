//! Compressed payloads retained by the same receive guards as decoded media.
use super::*;
use crate::{
    capture::{AudioKind, Guard, VideoKind},
    frames::{FrameMetadataState, MetadataText},
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
    frame: Guard<'rx, VideoKind>,
    format: VideoPacketFormat,
    len: usize,
    metadata: FrameMetadataState,
}

/// Borrowed compressed audio, with the ownership rules of [`CompressedVideoRef`].
pub struct CompressedAudioRef<'rx> {
    frame: Guard<'rx, AudioKind>,
    codec: AudioCodec,
    protected: bool,
    parameters: AudioParameters,
    len: usize,
    metadata: FrameMetadataState,
}

macro_rules! borrowed_extras {
    () => {
        /// The codec payload, borrowed in place without copying.
        pub fn data(&self) -> &[u8] {
            // SAFETY: construction validated the positive payload extent; the
            // guard retains its initialized, immutable allocation for this borrow.
            unsafe { std::slice::from_raw_parts(self.frame.frame().p_data, self.len) }
        }

        /// The frame timecode, as delivered by the source.
        pub fn timecode(&self) -> Timecode {
            Timecode::from_ticks(self.frame.frame().timecode)
        }

        /// The sender's submission time, when the SDK supplies it.
        pub fn timestamp(&self) -> Option<SdkTimestamp> {
            SdkTimestamp::from_sdk(self.frame.frame().timestamp)
        }

        /// Optional UTF-8 metadata. Unreadable text is withheld without losing
        /// the packet; [`Self::metadata_status`] reports its condition.
        pub fn metadata(&self) -> Option<&str> {
            self.metadata_text().map(MetadataText::as_str)
        }

        /// Cached metadata presence or the reason unreadable text was withheld.
        pub fn metadata_status(&self) -> FrameMetadataStatus {
            self.metadata.status()
        }

        fn metadata_text(&self) -> Option<MetadataText<'_>> {
            // SAFETY: construction scanned this exact pointer, which the guard
            // keeps alive and unchanged until the reference is dropped.
            unsafe { self.metadata.text(self.frame.frame().p_metadata) }
        }
    };
}

impl<'rx> CompressedVideoRef<'rx> {
    pub(crate) fn new(frame: Guard<'rx, VideoKind>) -> Result<Self> {
        let raw = frame.frame();
        let format = VideoPacketFormat::from_sdk(raw)?;
        // SAFETY: a delivered compressed descriptor contains its readable byte
        // extent in the size union; the guard retains that SDK allocation.
        let bytes = unsafe { packet_bytes(raw.p_data, raw.__bindgen_anon_1.data_size_in_bytes)? };
        validate_video_packet(format, bytes, Use::Receive)?;
        let len = bytes.len();
        // SAFETY: delivered metadata is readable through NUL or the cap and
        // stays live and unchanged until this guard is released.
        let metadata = unsafe { FrameMetadataState::scan(raw.p_metadata) };
        Ok(Self {
            frame,
            format,
            len,
            metadata,
        })
    }

    borrowed_extras!();

    /// The packet's codec and video format.
    pub fn format(&self) -> VideoPacketFormat {
        self.format
    }

    /// Copy payload and readable metadata into independently owned storage.
    /// Unreadable metadata and its status are omitted from the owned copy.
    pub fn to_owned(&self) -> CompressedVideoFrame {
        let mut extras = FrameExtras::default();
        extras.copy_received(self.timecode(), self.timestamp(), self.metadata_text());
        CompressedVideoFrame {
            format: self.format,
            bytes: AliasableVec::new(self.data().to_vec()),
            extras,
        }
    }

    /// Make the same owned copy while reusing destination payload and metadata
    /// allocations. Once capacities suffice, repeated copies allocate nothing,
    /// including when metadata alternates between present and absent.
    pub fn copy_into(&self, destination: &mut CompressedVideoFrame) {
        destination.bytes.copy_from_slice(self.data());
        destination
            .extras
            .copy_received(self.timecode(), self.timestamp(), self.metadata_text());
        destination.format = self.format;
    }
}

impl<'rx> CompressedAudioRef<'rx> {
    pub(crate) fn new(frame: Guard<'rx, AudioKind>) -> Result<Self> {
        let raw = frame.frame();
        let (codec, protected) = AudioCodec::from_outer(raw.FourCC)
            .ok_or_else(|| invalid_frame("Unknown compressed audio FourCC"))?;
        let parameters = AudioParameters::from_sdk(raw)?;
        // SAFETY: as for compressed video; the audio size union names the
        // guard's initialized, readable payload extent.
        let bytes = unsafe { packet_bytes(raw.p_data, raw.__bindgen_anon_1.data_size_in_bytes)? };
        validate_received_audio(codec, bytes)?;
        let len = bytes.len();
        // SAFETY: the guard retains the SDK metadata through NUL or the cap.
        let metadata = unsafe { FrameMetadataState::scan(raw.p_metadata) };
        Ok(Self {
            frame,
            codec,
            protected,
            parameters,
            len,
            metadata,
        })
    }

    borrowed_extras!();

    /// The compressed audio codec.
    pub fn codec(&self) -> AudioCodec {
        self.codec
    }

    /// Whether the source stream is protected.
    pub fn is_protected(&self) -> bool {
        self.protected
    }

    /// Samples per second.
    pub fn sample_rate(&self) -> u32 {
        self.parameters.sample_rate
    }

    /// Number of audio channels.
    pub fn num_channels(&self) -> u32 {
        self.parameters.num_channels
    }

    /// Decoded samples per channel represented by this packet.
    pub fn num_samples(&self) -> u32 {
        self.parameters.num_samples
    }

    /// Copy payload and readable metadata into independently owned storage.
    /// Unreadable metadata and its status are omitted from the owned copy.
    pub fn to_owned(&self) -> CompressedAudioFrame {
        let mut extras = FrameExtras::default();
        extras.copy_received(self.timecode(), self.timestamp(), self.metadata_text());
        CompressedAudioFrame {
            codec: self.codec,
            protected: self.protected,
            parameters: self.parameters,
            bytes: AliasableVec::new(self.data().to_vec()),
            extras,
        }
    }

    /// Reuse destination payload and metadata storage for the same copy as
    /// [`Self::to_owned`]. Sufficient capacities permit allocation-free copies.
    pub fn copy_into(&self, destination: &mut CompressedAudioFrame) {
        destination.bytes.copy_from_slice(self.data());
        destination
            .extras
            .copy_received(self.timecode(), self.timestamp(), self.metadata_text());
        destination.codec = self.codec;
        destination.protected = self.protected;
        destination.parameters = self.parameters;
    }
}

impl std::fmt::Debug for CompressedVideoRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompressedVideoRef")
            .field("format", &self.format)
            .field("bytes", &self.len)
            .field("timecode", &self.timecode())
            .field("timestamp", &self.timestamp())
            .field("metadata_status", &self.metadata_status())
            .finish()
    }
}

impl std::fmt::Debug for CompressedAudioRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompressedAudioRef")
            .field("codec", &self.codec)
            .field("protected", &self.protected)
            .field("parameters", &self.parameters)
            .field("bytes", &self.len)
            .field("timecode", &self.timecode())
            .field("timestamp", &self.timestamp())
            .field("metadata_status", &self.metadata_status())
            .finish()
    }
}
