//! The storage and constructors for each compressed-packet role.
use super::*;
use crate::{
    capture::{AudioKind, FrameFree, Guard, VideoKind},
    frames::{
        descriptor_parts_sealed, DescriptorParts, FrameMetadataRef, MetadataText, SdkDescriptor,
        ValidatedFrame,
    },
    FrameMetadataStatus,
};
use std::fmt::{self, Debug};

use crate::advanced::send::ScatterStorage;
#[cfg(doc)]
use crate::advanced::MAX_SCATTER_BLOCKS;
#[cfg(ndi_has_advanced_scatter_send)]
use crate::sys::advanced::FrameScatter;

/// A compressed packet's checked media format.
///
/// This trait is sealed and implemented by [`VideoPacketFormat`] and
/// [`AudioPacketFormat`]. The format
/// determines the SDK descriptor and which packets can be received or sent.
#[allow(private_bounds)]
pub trait PacketFormat: NativePacketFormat + Copy + Debug + Send + Sync {}

impl PacketFormat for VideoPacketFormat {}
impl PacketFormat for AudioPacketFormat {}

/// The SDK representation and admission rules behind the public format bound.
pub(crate) trait NativePacketFormat: Sized {
    type Descriptor: SdkDescriptor;
    type Kind: FrameFree<RawFrame = Self::Descriptor>;

    const SIZE_DEFECT: crate::FrameDefect;

    fn from_sdk(descriptor: &Self::Descriptor) -> Result<Self>;
    fn descriptor(self, extras: &FrameExtras) -> Self::Descriptor;
    fn payload(descriptor: &Self::Descriptor) -> (*mut u8, i32);
    fn lend_payload(descriptor: &mut Self::Descriptor, data: *mut u8, size: i32);
    fn timecode(descriptor: &Self::Descriptor) -> Timecode;
    fn header_bytes(self) -> usize;
    fn envelope_codec(self) -> Option<EnvelopeCodec>;
    fn admit(self, packet: PacketContent<'_>, purpose: Use) -> Result<Option<EnvelopeExtents>>;
}

pub(crate) enum PacketContent<'a> {
    Contiguous(PacketSlice<'a>),
    Scatter {
        first: &'a [u8],
        total: PacketLen,
        rest: &'a [Vec<u8>],
    },
}

impl PacketContent<'_> {
    fn first(&self) -> &[u8] {
        match self {
            Self::Contiguous(packet) => packet.as_slice(),
            Self::Scatter { first, .. } => first,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Contiguous(packet) => packet.len(),
            Self::Scatter { total, .. } => *total,
        }
        .to_usize()
    }

    fn bytes_at<const N: usize>(&self, offset: usize) -> Option<[u8; N]> {
        match self {
            Self::Contiguous(packet) => packet.as_slice().get(offset..)?.first_chunk().copied(),
            Self::Scatter { first, rest, .. } => {
                crate::advanced::send::bytes_at(first, rest, offset)
            }
        }
    }
}

impl NativePacketFormat for VideoPacketFormat {
    type Descriptor = NDIlib_video_frame_v2_t;
    type Kind = VideoKind;

    const SIZE_DEFECT: crate::FrameDefect = crate::FrameDefect::CompressedVideoSize;

    fn from_sdk(descriptor: &Self::Descriptor) -> Result<Self> {
        Self::from_sdk(descriptor)
    }

    fn descriptor(self, extras: &FrameExtras) -> Self::Descriptor {
        self.descriptor(extras)
    }

    fn payload(descriptor: &Self::Descriptor) -> (*mut u8, i32) {
        // SAFETY: callers classify the FourCC as compressed before reading
        // the size, which is the member used by compressed descriptors.
        (descriptor.p_data, unsafe {
            descriptor.__bindgen_anon_1.data_size_in_bytes
        })
    }

    fn lend_payload(descriptor: &mut Self::Descriptor, data: *mut u8, size: i32) {
        descriptor.p_data = data;
        descriptor.__bindgen_anon_1.data_size_in_bytes = size;
    }

    fn timecode(descriptor: &Self::Descriptor) -> Timecode {
        Timecode::from_ticks(descriptor.timecode)
    }

    fn header_bytes(self) -> usize {
        match self.envelope_codec() {
            Some(_) => ENVELOPE_HEADER_BYTES,
            None => SPEEDHQ_HEADER_BYTES,
        }
    }

    fn envelope_codec(self) -> Option<EnvelopeCodec> {
        self.codec().envelope_codec()
    }

    fn admit(self, packet: PacketContent<'_>, purpose: Use) -> Result<Option<EnvelopeExtents>> {
        let Some(codec) = self.envelope_codec() else {
            if packet.len() < SPEEDHQ_HEADER_BYTES {
                return Err(invalid_frame(crate::FrameDefect::SpeedHqTruncated));
            }
            if purpose == Use::Send
                && packet
                    .first()
                    .first()
                    .is_none_or(|quality| *quality >= SPEEDHQ_QUALITY_LIMIT)
            {
                return Err(invalid_frame(crate::FrameDefect::SpeedHqQuality));
            }
            return Ok(None);
        };
        let header = EnvelopeHeader::read(packet.first())?;
        let extents = header.validate(codec, packet.len(), purpose)?;
        if purpose == Use::Send {
            header.validate_video_configuration()?;
            if self.tier() == StreamTier::Preview
                && (self.width() > MAX_PREVIEW_DIMENSION || self.height() > MAX_PREVIEW_DIMENSION)
            {
                return Err(invalid_frame(crate::FrameDefect::PreviewVideoDimensions));
            }
        }
        Ok(Some(extents))
    }
}

impl NativePacketFormat for AudioPacketFormat {
    type Descriptor = NDIlib_audio_frame_v3_t;
    type Kind = AudioKind;

    const SIZE_DEFECT: crate::FrameDefect = crate::FrameDefect::CompressedAudioSize;

    fn from_sdk(descriptor: &Self::Descriptor) -> Result<Self> {
        Self::from_sdk(descriptor)
    }

    fn descriptor(self, extras: &FrameExtras) -> Self::Descriptor {
        self.descriptor(extras)
    }

    fn payload(descriptor: &Self::Descriptor) -> (*mut u8, i32) {
        // SAFETY: callers classify the FourCC as compressed before reading
        // the size, which is the member used by compressed descriptors.
        (descriptor.p_data, unsafe {
            descriptor.__bindgen_anon_1.data_size_in_bytes
        })
    }

    fn lend_payload(descriptor: &mut Self::Descriptor, data: *mut u8, size: i32) {
        descriptor.p_data = data;
        descriptor.__bindgen_anon_1.data_size_in_bytes = size;
    }

    fn timecode(descriptor: &Self::Descriptor) -> Timecode {
        Timecode::from_ticks(descriptor.timecode)
    }

    fn header_bytes(self) -> usize {
        self.envelope_codec().map_or(0, |_| ENVELOPE_HEADER_BYTES)
    }

    fn envelope_codec(self) -> Option<EnvelopeCodec> {
        match self.codec() {
            AudioCodec::Aac => Some(EnvelopeCodec::Aac),
            AudioCodec::Opus => None,
        }
    }

    fn admit(self, packet: PacketContent<'_>, purpose: Use) -> Result<Option<EnvelopeExtents>> {
        if purpose == Use::Send {
            self.codec().validate_delivery(self)?;
        }
        let Some(codec) = self.envelope_codec() else {
            return Ok(None);
        };
        let header = EnvelopeHeader::read(packet.first())?;
        let extents = header.validate(codec, packet.len(), purpose)?;
        if purpose == Use::Send {
            header.validate_aac(packet.bytes_at(extents.extra_offset()), self)?;
        }
        Ok(Some(extents))
    }
}

/// One owned compressed frame that has been admitted for sending.
///
/// Construction checks packet bounds and, where present, the envelope's codec
/// fields. The application supplies a decodable codec bitstream. The SDK can
/// write to the bytes, so sending takes the frame exclusively or by value.
#[derive(Clone)]
pub struct CompressedFrame<F: PacketFormat> {
    format: F,
    payload: PacketBytes,
    extras: FrameExtras,
}

impl CompressedFrame<VideoPacketFormat> {
    /// Take ownership of one frame's bytes: a packet envelope for H.264 and
    /// HEVC (see [`PacketEnvelope::encode`]), a SpeedHQ frame otherwise.
    ///
    /// # Errors
    ///
    /// Returns the bytes with [`Error::InvalidFrame`] if their extent or
    /// envelope is invalid, or for a frame the SDK does not deliver (see
    /// [`VideoCodec`]).
    pub fn new(
        format: VideoPacketFormat,
        bytes: Vec<u8>,
    ) -> std::result::Result<Self, Rejected<Vec<u8>>> {
        Self::admitted(format, bytes)
    }

    /// Bytes this frame keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.payload.capacity() + self.extras.retained_bytes()
    }
}

impl CompressedFrame<AudioPacketFormat> {
    /// Take ownership of one audio frame's bytes: a packet envelope for AAC
    /// (see [`PacketEnvelope::encode`]), one bare packet for Opus.
    ///
    /// # Errors
    ///
    /// Returns the bytes with [`Error::InvalidFrame`] if their extent is
    /// invalid, the delivery parameters are unsupported (see [`AudioCodec`]),
    /// or the AAC envelope is malformed or disagrees with those parameters.
    pub fn new(
        format: AudioPacketFormat,
        bytes: Vec<u8>,
    ) -> std::result::Result<Self, Rejected<Vec<u8>>> {
        Self::admitted(format, bytes)
    }
}

impl<F: PacketFormat> CompressedFrame<F> {
    /// The timecode: [`crate::Timecode::SYNTHESIZE`] on a frame built for
    /// sending without an explicit timecode.
    pub fn timecode(&self) -> crate::Timecode {
        self.extras.timecode()
    }

    /// Replace the frame's timecode.
    pub fn set_timecode(&mut self, timecode: crate::Timecode) {
        self.extras.set_timecode(timecode);
    }

    /// The per-frame metadata, if any. Unreadable received text is omitted.
    pub fn metadata(&self) -> Option<&str> {
        self.extras.metadata()
    }

    /// Cached metadata presence or the reason unreadable text was withheld.
    pub fn metadata_status(&self) -> crate::FrameMetadataStatus {
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
    ) -> std::result::Result<(), crate::Rejected<String>> {
        self.extras.set_metadata(metadata)
    }

    /// Remove the frame metadata.
    pub fn clear_metadata(&mut self) {
        self.extras.clear_metadata();
    }

    fn admitted(format: F, bytes: Vec<u8>) -> std::result::Result<Self, Rejected<Vec<u8>>> {
        let payload = PacketBytes::new(bytes)
            .map_err(|bytes| Rejected::new(invalid_frame(F::SIZE_DEFECT), bytes))?;
        let extents = match format.admit(PacketContent::Contiguous(payload.view()), Use::Send) {
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

    /// The packet's codec and media format.
    pub fn format(&self) -> F {
        self.format
    }

    /// When the original sender submitted the frame, if supplied at capture.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        self.extras.timestamp()
    }

    /// The validated envelope, if the codec uses one.
    pub fn envelope(&self) -> Option<PacketEnvelope<'_>> {
        self.payload
            .extents()
            .map(|extents| PacketEnvelope::from_validated(self.data(), extents))
    }

    /// Take the codec bytes, retaining the original allocation and capacity.
    pub fn into_data(self) -> Vec<u8> {
        self.payload.into_vec()
    }

    /// A native descriptor that lends this frame's bytes and metadata to the
    /// SDK. The caller keeps the frame exclusively while the SDK can write.
    pub(crate) fn raw(&mut self) -> F::Descriptor {
        let mut raw = self.format.descriptor(&self.extras);
        let (data, size) = self.payload.lend();
        F::lend_payload(&mut raw, data, size);
        raw
    }
}

/// An owned compressed packet copied from a receiver without loss.
///
/// Later envelope versions, flags and trailing bytes remain intact. Convert
/// with [`CompressedFrame::try_from`] before sending. A refusal returns this
/// packet and its storage in [`Rejected`].
#[derive(Clone)]
pub struct ReceivedPacket<F: PacketFormat> {
    format: F,
    payload: PacketBytes,
    extras: FrameExtras,
}

impl<F: PacketFormat> ReceivedPacket<F> {
    /// The timecode: [`crate::Timecode::SYNTHESIZE`] on a frame built for
    /// sending without an explicit timecode.
    pub fn timecode(&self) -> crate::Timecode {
        self.extras.timecode()
    }

    /// Replace the frame's timecode.
    pub fn set_timecode(&mut self, timecode: crate::Timecode) {
        self.extras.set_timecode(timecode);
    }

    /// The per-frame metadata, if any. Unreadable received text is omitted.
    pub fn metadata(&self) -> Option<&str> {
        self.extras.metadata()
    }

    /// Cached metadata presence or the reason unreadable text was withheld.
    pub fn metadata_status(&self) -> crate::FrameMetadataStatus {
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
    ) -> std::result::Result<(), crate::Rejected<String>> {
        self.extras.set_metadata(metadata)
    }

    /// Remove the frame metadata.
    pub fn clear_metadata(&mut self) {
        self.extras.clear_metadata();
    }

    /// The complete codec bytes, including retained envelope extensions.
    pub fn data(&self) -> &[u8] {
        self.payload.as_slice()
    }

    /// The packet's codec and media format.
    pub fn format(&self) -> F {
        self.format
    }

    /// The sender's submission time, when supplied by the SDK.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        self.extras.timestamp()
    }

    /// The validated envelope, if the codec uses one.
    pub fn envelope(&self) -> Option<PacketEnvelope<'_>> {
        self.payload
            .extents()
            .map(|extents| PacketEnvelope::from_validated(self.data(), extents))
    }

    /// Take the original byte allocation and its capacity.
    pub fn into_data(self) -> Vec<u8> {
        self.payload.into_vec()
    }

    /// Check send admission without consuming or changing this packet.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] for unsupported codec parameters or envelope details.
    pub fn validate_for_send(&self) -> Result<()> {
        self.send_extents().map(drop)
    }

    fn send_extents(&self) -> Result<Option<EnvelopeExtents>> {
        self.format
            .admit(PacketContent::Contiguous(self.payload.view()), Use::Send)
    }
}

impl<F: PacketFormat> TryFrom<ReceivedPacket<F>> for CompressedFrame<F> {
    type Error = Rejected<ReceivedPacket<F>>;

    fn try_from(packet: ReceivedPacket<F>) -> std::result::Result<Self, Self::Error> {
        let extents = match packet.send_extents() {
            Ok(extents) => extents,
            Err(error) => return Err(Rejected::new(error, packet)),
        };
        let ReceivedPacket {
            format,
            payload,
            extras,
        } = packet;
        Ok(Self {
            format,
            payload: payload.with_envelope(extents),
            extras,
        })
    }
}

impl<F: PacketFormat> From<CompressedFrame<F>> for ReceivedPacket<F> {
    fn from(frame: CompressedFrame<F>) -> Self {
        let CompressedFrame {
            format,
            payload,
            extras,
        } = frame;
        Self {
            format,
            payload,
            extras,
        }
    }
}

/// A borrowed compressed packet captured by a [`PacketReceiver`](crate::advanced::PacketReceiver).
///
/// Capture validates once without allocating. The receive guard retains
/// payload and metadata, including an application allocator's usage charge,
/// until the reference drops. Copy into an owned frame before sending because
/// the SDK can write to frames it sends.
pub struct CompressedRef<'rx, F: PacketFormat> {
    frame: ValidatedFrame<Guard<'rx, F::Kind>, PacketParts<F>>,
}

struct PacketParts<F: PacketFormat> {
    format: F,
    len: PacketLen,
    extents: Option<EnvelopeExtents>,
}

impl<F: PacketFormat> descriptor_parts_sealed::Sealed for PacketParts<F> {}

// SAFETY: the sealed format validates the codec and parameters from this exact
// descriptor. PacketSlice validates its payload extent, and receive admission
// validates any envelope within that extent.
unsafe impl<F: PacketFormat> DescriptorParts<F::Descriptor> for PacketParts<F> {
    unsafe fn validate(raw: &F::Descriptor) -> Result<Self> {
        let format = F::from_sdk(raw)?;
        let (data, size) = F::payload(raw);
        // SAFETY: the guard retains the initialized allocation described by the
        // checked compressed format. PacketSlice checks its declared extent.
        let bytes = unsafe { PacketSlice::from_sdk(data, size)? };
        let extents = format.admit(PacketContent::Contiguous(bytes), Use::Receive)?;
        Ok(Self {
            format,
            len: bytes.len(),
            extents,
        })
    }
}

impl<'rx, F: PacketFormat> CompressedRef<'rx, F> {
    pub(crate) fn new(guard: Guard<'rx, F::Kind>) -> Result<Self> {
        let frame = ValidatedFrame::validate(guard)?;
        Ok(Self { frame })
    }

    /// The complete codec bytes, borrowed in place.
    pub fn data(&self) -> &[u8] {
        self.packet_slice().as_slice()
    }

    fn packet_slice(&self) -> PacketSlice<'_> {
        let (data, _) = F::payload(self.frame.descriptor());
        // SAFETY: the validated parts cover this descriptor's immutable
        // payload, retained by the guard for the returned borrow.
        unsafe { PacketSlice::from_validated(data, self.frame.parts().len) }
    }

    /// The packet's codec and media format.
    pub fn format(&self) -> F {
        self.frame.parts().format
    }

    /// The timecode supplied by the source.
    pub fn timecode(&self) -> Timecode {
        F::timecode(self.frame.descriptor())
    }

    /// The sender's submission time, when supplied by the SDK.
    pub fn timestamp(&self) -> Option<SdkTimestamp> {
        self.frame.timestamp()
    }

    /// Optional UTF-8 metadata. [`Self::metadata_status`] reports withheld text.
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

    /// The validated envelope, if the codec uses one.
    pub fn envelope(&self) -> Option<PacketEnvelope<'_>> {
        self.frame
            .parts()
            .extents
            .map(|extents| PacketEnvelope::from_validated(self.data(), extents))
    }

    fn copy_extras(&self, extras: &mut FrameExtras) {
        extras.copy_received(self.timecode(), self.timestamp(), self.frame_metadata());
    }

    /// Copy payload and readable metadata into independent storage, retaining
    /// the status of any withheld metadata.
    pub fn to_owned(&self) -> ReceivedPacket<F> {
        let mut extras = FrameExtras::default();
        self.copy_extras(&mut extras);
        ReceivedPacket {
            format: self.format(),
            payload: PacketBytes::copied(self.packet_slice(), self.frame.parts().extents),
            extras,
        }
    }

    /// Make a lossless copy into existing storage. Once the destination has
    /// sufficient capacity, repeated copies allocate nothing.
    pub fn copy_into(&self, destination: &mut ReceivedPacket<F>) {
        destination
            .payload
            .refill(self.packet_slice(), self.frame.parts().extents);
        self.copy_extras(&mut destination.extras);
        destination.format = self.format();
    }

    /// Admit this packet for sending, then copy it into existing storage.
    /// A refusal leaves every destination field and allocation unchanged.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when this packet cannot be sent.
    pub fn try_copy_into(&self, destination: &mut CompressedFrame<F>) -> Result<()> {
        let bytes = self.packet_slice();
        let format = self.format();
        let extents = format.admit(PacketContent::Contiguous(bytes), Use::Send)?;
        destination.payload.refill(bytes, extents);
        self.copy_extras(&mut destination.extras);
        destination.format = format;
        Ok(())
    }
}

impl<F: PacketFormat> TryFrom<&CompressedRef<'_, F>> for CompressedFrame<F> {
    type Error = Error;

    fn try_from(packet: &CompressedRef<'_, F>) -> Result<Self> {
        let bytes = packet.packet_slice();
        let format = packet.format();
        let extents = format.admit(PacketContent::Contiguous(bytes), Use::Send)?;
        let mut extras = FrameExtras::default();
        packet.copy_extras(&mut extras);
        Ok(Self {
            format,
            payload: PacketBytes::copied(bytes, extents),
            extras,
        })
    }
}

/// A sendable compressed frame held in ordered blocks, without joining them.
///
/// The first block holds the complete 44-byte envelope header for H.264, HEVC
/// or AAC, or the first four bytes for SpeedHQ. Opus has no header.
///
/// # Examples
///
/// ```
/// use grafton_ndi::advanced::{
///     EnvelopeCodec, PacketEnvelope, ScatterFrame, VideoCodec, VideoPacketFormat,
/// };
///
/// let packet =
///     PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, true, &[0x65; 64], &[0x67; 8])?;
/// let (header, rest) = packet.split_at(44);
/// let frame = ScatterFrame::new(
///     VideoPacketFormat::new(VideoCodec::H264, 1280, 720)?,
///     vec![header.to_vec(), rest.to_vec()],
/// )?;
/// assert_eq!(frame.total_bytes(), packet.len());
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
#[derive(Clone)]
pub struct ScatterFrame<F: PacketFormat> {
    format: F,
    storage: ScatterStorage,
    extents: Option<EnvelopeExtents>,
    extras: FrameExtras,
}

impl<F: PacketFormat> ScatterFrame<F> {
    /// The timecode: [`crate::Timecode::SYNTHESIZE`] on a frame built for
    /// sending without an explicit timecode.
    pub fn timecode(&self) -> crate::Timecode {
        self.extras.timecode()
    }

    /// Replace the frame's timecode.
    pub fn set_timecode(&mut self, timecode: crate::Timecode) {
        self.extras.set_timecode(timecode);
    }

    /// The per-frame metadata, if any. Unreadable received text is omitted.
    pub fn metadata(&self) -> Option<&str> {
        self.extras.metadata()
    }

    /// Cached metadata presence or the reason unreadable text was withheld.
    pub fn metadata_status(&self) -> crate::FrameMetadataStatus {
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
    ) -> std::result::Result<(), crate::Rejected<String>> {
        self.extras.set_metadata(metadata)
    }

    /// Remove the frame metadata.
    pub fn clear_metadata(&mut self) {
        self.extras.clear_metadata();
    }

    /// Take ownership of one frame's blocks, in order.
    ///
    /// # Errors
    ///
    /// Returns the original blocks with [`Error::InvalidFrame`] if the list is
    /// empty or exceeds [`MAX_SCATTER_BLOCKS`], a block is empty, the frame
    /// exceeds [`MAX_PACKET_BYTES`], or the packet is not sendable in this format.
    pub fn new(
        format: F,
        mut blocks: Vec<Vec<u8>>,
    ) -> std::result::Result<Self, Rejected<Vec<Vec<u8>>>> {
        let mut check = || {
            let checked = ScatterStorage::check(&mut blocks, format.header_bytes())?;
            let packet = PacketContent::Scatter {
                first: checked.first,
                total: checked.total,
                rest: checked.rest,
            };
            let extents = format.admit(packet, Use::Send)?;
            Ok(Self {
                format,
                storage: checked.into_storage(),
                extents,
                extras: FrameExtras::default(),
            })
        };
        match check() {
            Ok(frame) => Ok(frame),
            Err(error) => Err(Rejected::new(error, blocks)),
        }
    }

    /// The packet's codec and media format.
    pub fn format(&self) -> F {
        self.format
    }

    /// The blocks, in order.
    pub fn blocks(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.storage.blocks()
    }

    /// Recover each original block, preserving order, allocation and capacity.
    /// The SDK's writes remain in those blocks.
    pub fn into_blocks(self) -> Vec<Vec<u8>> {
        self.storage.into_blocks()
    }

    /// The combined size of the blocks, in bytes.
    pub fn total_bytes(&self) -> usize {
        self.storage.total_bytes()
    }

    /// Join the blocks into one newly allocated contiguous frame.
    pub fn into_contiguous_copy(self) -> CompressedFrame<F> {
        CompressedFrame {
            format: self.format,
            payload: self.storage.copy_contiguous(self.extents),
            extras: self.extras,
        }
    }

    /// Lend the native scatter array and a descriptor whose data fields are unused.
    #[cfg(ndi_has_advanced_scatter_send)]
    pub(in crate::advanced) fn lend_scatter(&mut self) -> (F::Descriptor, *const FrameScatter) {
        (
            self.format.descriptor(&self.extras),
            self.storage.descriptor(),
        )
    }

    #[cfg(ndi_has_advanced_scatter_send)]
    pub(in crate::advanced) fn lend_first_block(&mut self) -> (*mut u8, i32) {
        self.storage.lend_first_block()
    }

    #[cfg(test)]
    pub(in crate::advanced) fn storage_for_test(&self) -> &ScatterStorage {
        &self.storage
    }
}

impl ScatterFrame<VideoPacketFormat> {
    pub(in crate::advanced) fn retained_bytes(&self) -> usize {
        self.storage
            .retained_bytes()
            .saturating_add(self.extras.retained_bytes())
    }
}

/// Format an owned or borrowed packet without traversing its payload.
fn debug_packet(
    f: &mut fmt::Formatter<'_>,
    name: &str,
    format: impl Debug,
    bytes: usize,
    timecode: Timecode,
    timestamp: Option<SdkTimestamp>,
    metadata_status: FrameMetadataStatus,
) -> fmt::Result {
    f.debug_struct(name)
        .field("format", &format)
        .field("bytes", &bytes)
        .field("timecode", &timecode)
        .field("timestamp", &timestamp)
        .field("metadata_status", &metadata_status)
        .finish()
}

impl<F: PacketFormat> Debug for CompressedFrame<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_packet(
            f,
            "CompressedFrame",
            self.format(),
            self.data().len(),
            self.timecode(),
            self.timestamp(),
            self.metadata_status(),
        )
    }
}

impl<F: PacketFormat> Debug for ReceivedPacket<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_packet(
            f,
            "ReceivedPacket",
            self.format(),
            self.data().len(),
            self.timecode(),
            self.timestamp(),
            self.metadata_status(),
        )
    }
}

impl<F: PacketFormat> Debug for CompressedRef<'_, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_packet(
            f,
            "CompressedRef",
            self.format(),
            self.data().len(),
            self.timecode(),
            self.timestamp(),
            self.metadata_status(),
        )
    }
}

impl<F: PacketFormat> Debug for ScatterFrame<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScatterFrame")
            .field("format", &self.format())
            .field("blocks", &self.blocks().len())
            .field("bytes", &self.total_bytes())
            .field("timecode", &self.timecode())
            .field("timestamp", &self.extras.timestamp())
            .field("metadata_status", &self.metadata_status())
            .finish()
    }
}
