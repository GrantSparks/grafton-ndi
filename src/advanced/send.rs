//! Sending compressed frames, held in one buffer or in several.

use std::{ffi::CString, ptr, time::Duration};

use super::{
    invalid,
    packets::{frame_metadata, EnvelopeHeader, ENVELOPE_HEADER_BYTES},
    AdvancedConfig, AudioCodec, CompressedAudioFrame, CompressedVideoFrame, EnvelopeCodec,
    QualityPlan, StreamTier, VideoPacketFormat, MAX_PACKET_BYTES,
};
use crate::{
    sys::{
        self,
        advanced::{native, FrameScatter, ScatterSend},
        bindings::*,
    },
    Rejected, Result, Sender, SenderOptions, Tally, NDI,
};

/// Maximum number of blocks in one scatter frame.
pub const MAX_SCATTER_BLOCKS: usize = 65_536;

/// The blocks of one frame and the null-terminated arrays that describe them
/// to the SDK.
#[derive(Debug)]
struct ScatterStorage {
    blocks: Box<[Box<[u8]>]>,
    pointers: Box<[*const u8]>,
    sizes: Box<[i32]>,
    total: usize,
}

// SAFETY: every pointer refers to immutable storage this value owns. Moving
// the boxes never moves their allocations, and no pointer is handed out.
unsafe impl Send for ScatterStorage {}
// SAFETY: as above; the storage is never mutated after construction.
unsafe impl Sync for ScatterStorage {}

impl ScatterStorage {
    /// Check a block list and return its total size in bytes.
    fn total(blocks: &[Vec<u8>]) -> Result<usize> {
        if blocks.is_empty() || blocks.len() > MAX_SCATTER_BLOCKS {
            return Err(invalid("Invalid scatter block count"));
        }
        let mut total = 0usize;
        for block in blocks {
            if block.is_empty() {
                return Err(invalid("Scatter blocks must not be empty"));
            }
            total = total
                .checked_add(block.len())
                .filter(|total| *total <= MAX_PACKET_BYTES)
                .ok_or_else(|| invalid("Scatter frame exceeds the packet size limit"))?;
        }
        Ok(total)
    }

    /// Take ownership of blocks that [`total`](Self::total) accepted.
    fn new(blocks: Vec<Vec<u8>>, total: usize) -> Self {
        let blocks: Box<[_]> = blocks.into_iter().map(Vec::into_boxed_slice).collect();
        let pointers = blocks
            .iter()
            .map(|block| block.as_ptr())
            .chain(std::iter::once(ptr::null()))
            .collect();
        // Each block is no larger than the total, which is below i32::MAX.
        let sizes = blocks
            .iter()
            .map(|block| block.len() as i32)
            .chain(std::iter::once(0))
            .collect();
        Self {
            blocks,
            pointers,
            sizes,
            total,
        }
    }

    /// The native description of the blocks, borrowing this storage.
    fn descriptor(&self, table: &'static ScatterSend) -> FrameScatter {
        native!(table => FrameScatter {
            p_data_blocks: self.pointers.as_ptr(),
            p_data_blocks_size: self.sizes.as_ptr(),
        })
    }

    fn copy_contiguous(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.total);
        for block in &self.blocks {
            bytes.extend_from_slice(block);
        }
        bytes
    }
}

/// Check the packet envelope that begins a scatter frame of `total` bytes.
/// `needs_configuration` requires codec configuration data on every packet,
/// not only on keyframes.
fn validate_scatter_envelope(
    first: &[u8],
    total: usize,
    codec: EnvelopeCodec,
    needs_configuration: bool,
) -> Result<()> {
    if first.len() < ENVELOPE_HEADER_BYTES {
        return Err(invalid(
            "The complete compressed packet header must fit in the first scatter block",
        ));
    }
    let header = EnvelopeHeader::read(first)?;
    if !header.is_current_version() || header.codec() != codec.code() {
        return Err(invalid(
            "Scatter packet has an unsupported header or mismatched codec",
        ));
    }
    if header.data_len() == 0 || header.total_len() != Some(total) {
        return Err(invalid(
            "Scatter blocks do not match the complete packet length",
        ));
    }
    if (needs_configuration || header.is_keyframe()) && header.extra_len() == 0 {
        return Err(invalid("Scatter packet requires codec configuration data"));
    }
    Ok(())
}

/// One compressed video frame held in several owned blocks.
///
/// The blocks are sent as they are, without being joined: an encoder can hand
/// over its header, payload and parameter sets separately. For H.264 and HEVC
/// the first block must hold the complete 44-byte packet envelope header (see
/// [`packet_envelope`](super::packet_envelope) for the layout).
///
/// # Examples
///
/// ```
/// use grafton_ndi::advanced::{
///     packet_envelope, EnvelopeCodec, ScatterVideoFrame, VideoCodec, VideoPacketFormat,
/// };
///
/// let packet = packet_envelope(EnvelopeCodec::H264, 0, 0, true, &[0x65; 64], &[0x67; 8])?;
/// let (header, rest) = packet.split_at(44);
/// let frame = ScatterVideoFrame::new(
///     VideoPacketFormat::new(VideoCodec::H264, 1280, 720)?,
///     vec![header.to_vec(), rest.to_vec()],
/// )?;
/// assert_eq!(frame.total_bytes(), packet.len());
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
#[derive(Debug)]
pub struct ScatterVideoFrame {
    format: VideoPacketFormat,
    storage: ScatterStorage,
    timecode: i64,
    metadata: Option<CString>,
}

impl ScatterVideoFrame {
    /// Take ownership of one frame's blocks, in order.
    ///
    /// # Errors
    ///
    /// Returns the blocks with [`Error::InvalidFrame`](crate::Error::InvalidFrame)
    /// when there are none or more than [`MAX_SCATTER_BLOCKS`], a block is
    /// empty, the frame exceeds [`MAX_PACKET_BYTES`], or the first block does
    /// not begin a sendable frame of the format's codec.
    pub fn new(
        format: VideoPacketFormat,
        blocks: Vec<Vec<u8>>,
    ) -> std::result::Result<Self, Rejected<Vec<Vec<u8>>>> {
        let check = || {
            let total = ScatterStorage::total(&blocks)?;
            match format.codec().envelope_codec() {
                Some(codec) => validate_scatter_envelope(&blocks[0], total, codec, false)?,
                None if blocks[0].len() < 4 => {
                    return Err(invalid(
                        "The first SpeedHQ scatter block must contain its header",
                    ))
                }
                None => {}
            }
            Ok(total)
        };
        match check() {
            Ok(total) => Ok(Self {
                format,
                storage: ScatterStorage::new(blocks, total),
                timecode: NDIlib_send_timecode_synthesize,
                metadata: None,
            }),
            Err(error) => Err(Rejected::new(error, blocks)),
        }
    }

    /// The format.
    pub fn format(&self) -> VideoPacketFormat {
        self.format
    }

    /// The blocks, in order.
    pub fn blocks(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.storage.blocks.iter().map(AsRef::as_ref)
    }

    /// The size of the whole frame in bytes.
    pub fn total_bytes(&self) -> usize {
        self.storage.total
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

    /// Join the blocks into one newly allocated contiguous frame.
    pub fn into_contiguous_copy(self) -> CompressedVideoFrame {
        CompressedVideoFrame {
            format: self.format,
            bytes: self.storage.copy_contiguous().into_boxed_slice(),
            timecode: self.timecode,
            metadata: self.metadata,
        }
    }

    /// A native descriptor without data: a scatter send takes the payload
    /// from the block arrays and ignores the frame's own data fields.
    fn descriptor(&self) -> NDIlib_video_frame_v2_t {
        let mut raw = self.format.raw();
        raw.timecode = self.timecode;
        raw.p_metadata = self.metadata.as_ref().map_or(ptr::null(), |m| m.as_ptr());
        raw
    }
}

/// One compressed audio frame held in several owned blocks.
///
/// For AAC the first block must hold the complete 44-byte packet envelope
/// header; Opus has no envelope.
#[derive(Debug)]
pub struct ScatterAudioFrame {
    codec: AudioCodec,
    storage: ScatterStorage,
    sample_rate: i32,
    channels: i32,
    samples: i32,
    protected: bool,
    timecode: i64,
    metadata: Option<CString>,
}

impl ScatterAudioFrame {
    /// Take ownership of one frame's blocks, in order. `samples` is the number
    /// of samples per channel the frame decodes to.
    ///
    /// # Errors
    ///
    /// Returns the blocks with [`Error::InvalidFrame`](crate::Error::InvalidFrame)
    /// when a dimension is not positive, the block list is invalid (as for
    /// [`ScatterVideoFrame::new`]), or the first block does not begin a
    /// sendable AAC packet.
    pub fn new(
        codec: AudioCodec,
        sample_rate: i32,
        channels: i32,
        samples: i32,
        blocks: Vec<Vec<u8>>,
    ) -> std::result::Result<Self, Rejected<Vec<Vec<u8>>>> {
        let check = || {
            if sample_rate <= 0 || channels <= 0 || samples <= 0 {
                return Err(invalid("Compressed audio dimensions must be positive"));
            }
            let total = ScatterStorage::total(&blocks)?;
            if codec == AudioCodec::Aac {
                validate_scatter_envelope(&blocks[0], total, EnvelopeCodec::Aac, true)?;
            }
            Ok(total)
        };
        match check() {
            Ok(total) => Ok(Self {
                codec,
                storage: ScatterStorage::new(blocks, total),
                sample_rate,
                channels,
                samples,
                protected: false,
                timecode: NDIlib_send_timecode_synthesize,
                metadata: None,
            }),
            Err(error) => Err(Rejected::new(error, blocks)),
        }
    }

    /// The codec.
    pub fn codec(&self) -> AudioCodec {
        self.codec
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

    /// The blocks, in order.
    pub fn blocks(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.storage.blocks.iter().map(AsRef::as_ref)
    }

    /// The size of the whole frame in bytes.
    pub fn total_bytes(&self) -> usize {
        self.storage.total
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

    /// Mark the stream as protected.
    pub fn protected(mut self, protected: bool) -> Self {
        self.protected = protected;
        self
    }

    /// Join the blocks into one newly allocated contiguous frame.
    pub fn into_contiguous_copy(self) -> CompressedAudioFrame {
        CompressedAudioFrame {
            codec: self.codec,
            protected: self.protected,
            bytes: self.storage.copy_contiguous().into_boxed_slice(),
            sample_rate: self.sample_rate,
            channels: self.channels,
            samples: self.samples,
            timecode: self.timecode,
            metadata: self.metadata,
        }
    }

    /// A native descriptor without data; see [`ScatterVideoFrame::descriptor`].
    fn descriptor(&self) -> NDIlib_audio_frame_v3_t {
        NDIlib_audio_frame_v3_t {
            sample_rate: self.sample_rate,
            no_channels: self.channels,
            no_samples: self.samples,
            timecode: self.timecode,
            FourCC: self.codec.outer(self.protected),
            p_metadata: self.metadata.as_ref().map_or(ptr::null(), |m| m.as_ptr()),
            ..Default::default()
        }
    }
}

/// A video frame that was submitted asynchronously and that the SDK has
/// finished with.
#[derive(Debug)]
#[non_exhaustive]
pub enum SubmittedVideo {
    /// A frame submitted with [`CompressedSender::send_video_async`].
    Contiguous(CompressedVideoFrame),
    /// A frame submitted with [`CompressedSender::send_scatter_video_async`].
    Scatter(ScatterVideoFrame),
}

impl SubmittedVideo {
    /// The frame's format.
    pub fn format(&self) -> VideoPacketFormat {
        match self {
            Self::Contiguous(frame) => frame.format(),
            Self::Scatter(frame) => frame.format(),
        }
    }
}

/// The frames a [`CompressedSender`] still held when it was closed.
#[derive(Debug, Default)]
pub struct UnsentVideo {
    /// The last frame submitted asynchronously on the program stream.
    pub program: Option<SubmittedVideo>,
    /// The last frame submitted asynchronously on the preview stream.
    pub preview: Option<SubmittedVideo>,
}

/// A frame the SDK may still be reading, with the descriptors it was given.
struct PendingVideo {
    frame: SubmittedVideo,
    descriptor: Box<NDIlib_video_frame_v2_t>,
    // Kept only so that the address given to the SDK stays valid.
    _scatter: Option<Box<FrameScatter>>,
}

// SAFETY: the descriptors point only into allocations this entry owns, and
// moving the entry does not move them. Only the sender that owns the entry
// releases it, after the SDK has stopped reading it.
unsafe impl Send for PendingVideo {}

impl PendingVideo {
    fn contiguous(frame: CompressedVideoFrame) -> Self {
        Self {
            descriptor: Box::new(frame.raw()),
            _scatter: None,
            frame: SubmittedVideo::Contiguous(frame),
        }
    }

    /// The entry for a scatter frame, and the address of its block descriptor,
    /// which stays valid for as long as the entry does.
    fn scatter(
        frame: ScatterVideoFrame,
        table: &'static ScatterSend,
    ) -> (Self, *const FrameScatter) {
        let scatter = Box::new(frame.storage.descriptor(table));
        let address: *const FrameScatter = &*scatter;
        let pending = Self {
            descriptor: Box::new(frame.descriptor()),
            _scatter: Some(scatter),
            frame: SubmittedVideo::Scatter(frame),
        };
        (pending, address)
    }
}

/// The frame most recently submitted asynchronously on each stream.
///
/// The SDK reads an asynchronously sent frame until the next send on the same
/// stream returns, so each stream keeps exactly one frame alive.
#[derive(Default)]
struct PendingStreams {
    program: Option<PendingVideo>,
    preview: Option<PendingVideo>,
}

impl PendingStreams {
    /// Hand `next` to the SDK through `submit`, then take back the frame it
    /// replaces on the same stream.
    fn submit(
        &mut self,
        next: PendingVideo,
        submit: impl FnOnce(&PendingVideo),
    ) -> Option<SubmittedVideo> {
        // Both streams' earlier frames stay alive throughout the native call;
        // only the matching stream's frame is released once it has returned.
        submit(&next);
        let slot = match next.frame.format().stream_tier() {
            StreamTier::Program => &mut self.program,
            StreamTier::Preview => &mut self.preview,
        };
        slot.replace(next).map(|earlier| earlier.frame)
    }

    fn take(&mut self) -> UnsentVideo {
        UnsentVideo {
            program: self.program.take().map(|pending| pending.frame),
            preview: self.preview.take().map(|pending| pending.frame),
        }
    }
}

/// A sender of compressed video and audio.
///
/// The program and preview streams are independent: each asynchronous send
/// keeps its frame until the next asynchronous send on the same stream, which
/// returns the earlier frame for reuse. Which streams a codec needs, and how
/// they must relate, is defined by the NDI Advanced SDK documentation.
///
/// The sender is owned exclusively by this value; it is not registered with
/// advertisers or shared.
///
/// # Examples
///
/// ```no_run
/// use grafton_ndi::{
///     advanced::{
///         packet_envelope, CompressedSender, CompressedVideoFrame, EnvelopeCodec, VideoCodec,
///         VideoPacketFormat,
///     },
///     SenderOptions, NDI,
/// };
///
/// # fn encoded_access_unit() -> (Vec<u8>, Vec<u8>) { (vec![0], vec![0]) }
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = NDI::new()?;
/// let mut sender = CompressedSender::new(&ndi, &SenderOptions::builder("Encoder").build())?;
/// let format = VideoPacketFormat::new(VideoCodec::H264, 1920, 1080)?.frame_rate(60, 1)?;
///
/// let (payload, parameter_sets) = encoded_access_unit();
/// let packet = packet_envelope(EnvelopeCodec::H264, 0, 0, true, &payload, &parameter_sets)?;
/// sender.send_video(&CompressedVideoFrame::new(format, packet)?)?;
/// # Ok(())
/// # }
/// ```
pub struct CompressedSender {
    // Declared first so that it is destroyed first: destroying the native
    // sender is what ends the SDK's use of the pending frames.
    sender: Sender,
    pending: PendingStreams,
    core: &'static sys::Core,
}

impl CompressedSender {
    /// Create a sender with the SDK's default configuration.
    ///
    /// # Errors
    ///
    /// As for [`Sender::new_with_config`].
    pub fn new(ndi: &NDI, options: &SenderOptions) -> Result<Self> {
        Self::new_with_config(ndi, options, &AdvancedConfig::parse("{}")?)
    }

    /// Create a sender with a per-instance Advanced SDK configuration.
    ///
    /// # Errors
    ///
    /// As for [`Sender::new_with_config`].
    pub fn new_with_config(
        ndi: &NDI,
        options: &SenderOptions,
        config: &AdvancedConfig,
    ) -> Result<Self> {
        Ok(Self {
            sender: Sender::new_with_config(ndi, options, config)?,
            pending: PendingStreams::default(),
            core: &ndi.api().core,
        })
    }

    fn instance(&self) -> NDIlib_send_instance_t {
        self.sender.native_instance()
    }

    /// Send a video frame and return when the SDK has finished with it.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when the frame is
    /// not sendable; see [`CompressedVideoFrame::validate_for_send`].
    pub fn send_video(&mut self, frame: &CompressedVideoFrame) -> Result<()> {
        frame.validate_for_send()?;
        // SAFETY: the sender is live and the descriptor borrows `frame`, which
        // outlives this synchronous call.
        unsafe {
            self.core
                .NDIlib_send_send_video_v2(self.instance(), &frame.raw())
        };
        Ok(())
    }

    /// Submit a video frame without waiting for it to be sent.
    ///
    /// The sender keeps the frame until the next asynchronous send on the same
    /// stream, and returns the frame that this one replaces. The call can
    /// still block while the SDK applies back-pressure.
    ///
    /// # Errors
    ///
    /// Returns the frame when it is not sendable; see
    /// [`CompressedVideoFrame::validate_for_send`].
    pub fn send_video_async(
        &mut self,
        frame: CompressedVideoFrame,
    ) -> std::result::Result<Option<SubmittedVideo>, Rejected<CompressedVideoFrame>> {
        if let Err(error) = frame.validate_for_send() {
            return Err(Rejected::new(error, frame));
        }
        let (core, instance) = (self.core, self.instance());
        Ok(self
            .pending
            .submit(PendingVideo::contiguous(frame), |next| {
                // SAFETY: the sender is live, and the pending entry keeps the
                // descriptor and the bytes it points to alive until the SDK
                // has finished with them.
                unsafe { core.NDIlib_send_send_video_async_v2(instance, next.descriptor.as_ref()) }
            }))
    }

    /// Send an audio frame and return when the SDK has finished with it.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when the frame is
    /// not sendable; see [`CompressedAudioFrame::validate_for_send`].
    pub fn send_audio(&mut self, frame: &CompressedAudioFrame) -> Result<()> {
        frame.validate_for_send()?;
        // SAFETY: the sender is live and the descriptor borrows `frame`, which
        // outlives this synchronous call.
        unsafe {
            self.core
                .NDIlib_send_send_audio_v3(self.instance(), &frame.raw())
        };
        Ok(())
    }

    /// Send a video frame held in several blocks and return when the SDK has
    /// finished with it.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// when the runtime has no scatter sending.
    pub fn send_scatter_video(&mut self, frame: &ScatterVideoFrame) -> Result<()> {
        let table = self.sender.runtime().table::<ScatterSend>()?;
        let descriptor = frame.descriptor();
        let scatter = frame.storage.descriptor(table);
        // SAFETY: the sender is live, and both descriptors borrow `frame`,
        // which outlives this synchronous call.
        native!(table => unsafe {
            table.NDIlib_send_send_video_scatter(self.instance(), &descriptor, &scatter)
        });
        Ok(())
    }

    /// Submit a video frame held in several blocks without waiting for it to
    /// be sent; see [`send_video_async`](Self::send_video_async).
    ///
    /// # Errors
    ///
    /// Returns the frame when the runtime has no scatter sending.
    pub fn send_scatter_video_async(
        &mut self,
        frame: ScatterVideoFrame,
    ) -> std::result::Result<Option<SubmittedVideo>, Rejected<ScatterVideoFrame>> {
        let table = match self.sender.runtime().table::<ScatterSend>() {
            Ok(table) => table,
            Err(error) => return Err(Rejected::new(error, frame)),
        };
        let instance = self.instance();
        let (pending, scatter) = PendingVideo::scatter(frame, table);
        Ok(self.pending.submit(pending, |next| {
            // SAFETY: the sender is live, and the pending entry keeps both
            // descriptors and the blocks they point to alive until the SDK
            // has finished with them.
            native!(table => unsafe {
                table.NDIlib_send_send_video_scatter_async(
                    instance,
                    next.descriptor.as_ref(),
                    scatter,
                )
            })
        }))
    }

    /// Send an audio frame held in several blocks and return when the SDK has
    /// finished with it.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// when the runtime has no scatter sending.
    pub fn send_scatter_audio(&mut self, frame: &ScatterAudioFrame) -> Result<()> {
        let table = self.sender.runtime().table::<ScatterSend>()?;
        let descriptor = frame.descriptor();
        let scatter = frame.storage.descriptor(table);
        // SAFETY: the sender is live, and both descriptors borrow `frame`,
        // which outlives this synchronous call.
        native!(table => unsafe {
            table.NDIlib_send_send_audio_scatter(self.instance(), &descriptor, &scatter)
        });
        Ok(())
    }

    /// Set this sender's own tally contribution; see
    /// [`Sender::set_external_tally`].
    ///
    /// # Errors
    ///
    /// As for [`Sender::set_external_tally`].
    pub fn set_external_tally(&self, tally: Tally) -> Result<bool> {
        self.sender.set_external_tally(tally)
    }

    /// The frame size an encoder should aim for; see
    /// [`Sender::target_frame_bytes`].
    ///
    /// # Errors
    ///
    /// As for [`Sender::target_frame_bytes`].
    pub fn target_frame_bytes(&self, format: VideoPacketFormat) -> Result<u32> {
        self.sender.target_frame_bytes(format)
    }

    /// The bit rate an encoder should aim for; see [`Sender::target_bit_rate`].
    ///
    /// # Errors
    ///
    /// As for [`Sender::target_bit_rate`].
    pub fn target_bit_rate(&self, format: VideoPacketFormat) -> Result<u32> {
        self.sender.target_bit_rate(format)
    }

    /// The SpeedHQ quality factor; see [`Sender::legacy_quality`].
    ///
    /// # Errors
    ///
    /// As for [`Sender::legacy_quality`].
    pub fn legacy_quality(&self, format: VideoPacketFormat) -> Result<u32> {
        self.sender.legacy_quality(format)
    }

    /// The SpeedHQ quality and permitted format; see [`Sender::quality_plan`].
    ///
    /// # Errors
    ///
    /// As for [`Sender::quality_plan`].
    pub fn quality_plan(
        &self,
        format: VideoPacketFormat,
        supports_new_format: bool,
    ) -> Result<QualityPlan> {
        self.sender.quality_plan(format, supports_new_format)
    }

    /// Whether a receiver needs a keyframe; see [`Sender::keyframe_required`].
    ///
    /// # Errors
    ///
    /// As for [`Sender::keyframe_required`].
    pub fn keyframe_required(&self, format: Option<VideoPacketFormat>) -> Result<bool> {
        self.sender.keyframe_required(format)
    }

    /// Wait for keyframe demand to change; see
    /// [`Sender::wait_for_keyframe_change`].
    ///
    /// # Errors
    ///
    /// As for [`Sender::wait_for_keyframe_change`].
    pub fn wait_for_keyframe_change(
        &self,
        timeout: Duration,
        format: Option<VideoPacketFormat>,
    ) -> Result<bool> {
        self.sender.wait_for_keyframe_change(timeout, format)
    }

    /// Destroy the sender and take back the frames it still held.
    ///
    /// Destroying the native sender waits for the SDK to finish with every
    /// pending frame, without a timeout.
    pub fn close(self) -> UnsentVideo {
        let Self {
            sender,
            mut pending,
            ..
        } = self;
        drop(sender);
        pending.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advanced::{packet_envelope, VideoCodec};

    fn h264() -> (VideoPacketFormat, Vec<u8>) {
        (
            VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap(),
            packet_envelope(EnvelopeCodec::H264, 10, 9, true, &[1, 2, 3], &[4, 5]).unwrap(),
        )
    }

    #[test]
    fn validates_complete_first_header_and_segmented_total() {
        let (format, bytes) = h264();
        assert!(
            ScatterVideoFrame::new(format, vec![bytes[..43].to_vec(), bytes[43..].to_vec()])
                .is_err()
        );
        assert!(
            ScatterVideoFrame::new(format, vec![bytes[..44].to_vec(), bytes[44..48].to_vec()])
                .is_err()
        );
        assert!(ScatterVideoFrame::new(format, vec![bytes.clone(), vec![0]]).is_err());
        assert!(ScatterVideoFrame::new(format, vec![bytes.clone(), vec![]]).is_err());
        let frame = ScatterVideoFrame::new(
            format,
            vec![
                bytes[..44].to_vec(),
                bytes[44..47].to_vec(),
                bytes[47..].to_vec(),
            ],
        )
        .unwrap();
        assert_eq!(frame.total_bytes(), bytes.len());
        assert_eq!(frame.storage.pointers.len(), 4);
        assert!(frame.storage.pointers[3].is_null());
        assert_eq!(&*frame.storage.sizes, &[44, 3, 2, 0]);
        let moved = Box::new(frame);
        for (block, pointer) in moved
            .storage
            .blocks
            .iter()
            .zip(moved.storage.pointers.iter())
        {
            assert_eq!(block.as_ptr(), *pointer);
        }
        assert_eq!(moved.into_contiguous_copy().data(), bytes);
        fn send_sync<T: Send + Sync>() {}
        send_sync::<ScatterVideoFrame>();
        send_sync::<ScatterAudioFrame>();
        fn send<T: Send>() {}
        send::<CompressedSender>();
    }

    #[test]
    fn rejected_blocks_are_returned_in_order() {
        let (format, bytes) = h264();
        let blocks = vec![bytes[..43].to_vec(), bytes[43..].to_vec()];
        let rejected = ScatterVideoFrame::new(format, blocks.clone()).unwrap_err();
        assert_eq!(rejected.into_inner(), blocks);

        let rejected =
            ScatterAudioFrame::new(AudioCodec::Opus, 48_000, 0, 960, vec![vec![1], vec![2]])
                .unwrap_err();
        assert_eq!(rejected.into_inner(), [[1], [2]]);
    }

    #[test]
    fn aac_requires_extra_data_but_opus_has_no_packet_prefix() {
        let bytes = packet_envelope(EnvelopeCodec::Aac, 0, 0, false, &[1, 2], &[]).unwrap();
        assert!(ScatterAudioFrame::new(AudioCodec::Aac, 48000, 2, 1024, vec![bytes]).is_err());
        let bytes = packet_envelope(EnvelopeCodec::Aac, 0, 0, false, &[1, 2], &[3]).unwrap();
        let aac = ScatterAudioFrame::new(
            AudioCodec::Aac,
            48000,
            2,
            1024,
            vec![bytes[..44].to_vec(), bytes[44..].to_vec()],
        )
        .unwrap()
        .into_contiguous_copy();
        assert!(aac.validate_for_send().is_ok());
        let opus = ScatterAudioFrame::new(AudioCodec::Opus, 48000, 2, 960, vec![vec![1], vec![2]])
            .unwrap()
            .into_contiguous_copy();
        assert_eq!(opus.data(), &[1, 2]);
        assert!(opus.validate_for_send().is_ok());
        assert!(ScatterAudioFrame::new(AudioCodec::Opus, 0, 2, 960, vec![vec![1]]).is_err());
    }

    #[test]
    fn async_storage_is_retained_per_tier_through_native_submission() {
        let (format, bytes) = h264();
        let make = |tier| {
            PendingVideo::contiguous(
                CompressedVideoFrame::new(format.tier(tier), bytes.clone()).unwrap(),
            )
        };
        let mut streams = PendingStreams::default();
        let first = make(StreamTier::Program);
        let old_program_data = first.descriptor.p_data;
        let old_program_header = first.descriptor.as_ref() as *const NDIlib_video_frame_v2_t;
        assert!(streams.submit(first, |_| {}).is_none());
        assert!(streams
            .submit(make(StreamTier::Preview), |_| {
                // The earlier program descriptor and bytes survive both moving
                // their owners and a preview submission, as the SDK requires.
                unsafe {
                    assert_eq!((*old_program_header).p_data, old_program_data);
                    assert_eq!(
                        std::slice::from_raw_parts(old_program_data, bytes.len()),
                        bytes
                    );
                }
            })
            .is_none());
        let returned = streams
            .submit(make(StreamTier::Program), |_| unsafe {
                assert_eq!(
                    std::slice::from_raw_parts(old_program_data, bytes.len()),
                    bytes
                );
            })
            .unwrap();
        assert_eq!(returned.format().stream_tier(), StreamTier::Program);
        assert!(streams.preview.is_some());
        let unsent = streams.take();
        assert_eq!(
            unsent.program.unwrap().format().stream_tier(),
            StreamTier::Program
        );
        assert_eq!(
            unsent.preview.unwrap().format().stream_tier(),
            StreamTier::Preview
        );
    }
}
