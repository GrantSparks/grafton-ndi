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
    Rejected, Result, Sender, SenderOptions, Source, Tally, NDI,
};

use super::{aliasable::AliasableBox, descriptors};

/// Maximum number of blocks in one scatter frame.
pub const MAX_SCATTER_BLOCKS: usize = 65_536;

/// The blocks of one frame and the null-terminated arrays that describe them
/// to the SDK.
///
/// The SDK writes to the blocks it sends, as it does to a contiguous frame,
/// and keeps the addresses of the blocks and of both arrays while an
/// asynchronous send is pending.
#[derive(Debug)]
struct ScatterStorage {
    blocks: Box<[AliasableBox<[u8]>]>,
    pointers: AliasableBox<[*const u8]>,
    sizes: AliasableBox<[i32]>,
    total: usize,
}

// SAFETY: every pointer in `pointers` refers to a block this value owns, and
// moving the value moves none of its allocations.
unsafe impl Send for ScatterStorage {}
// SAFETY: a shared reference reads the blocks and never follows the stored
// pointers. They are followed only by the SDK, which is given them through
// `descriptor`, and that takes the storage exclusively.
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
        let mut blocks: Box<[_]> = blocks
            .into_iter()
            .map(|block| AliasableBox::new(block.into_boxed_slice()))
            .collect();
        // The SDK writes through these, whatever its declaration says.
        let pointers = blocks
            .iter_mut()
            .map(|block| block.as_mut_ptr().cast::<u8>().cast_const())
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
            pointers: AliasableBox::new(pointers),
            sizes: AliasableBox::new(sizes),
            total,
        }
    }

    /// The native description of the blocks, which lends them to the SDK.
    /// The SDK may write to the blocks for as long as it holds the
    /// description, so the caller keeps the storage exclusively for that long.
    fn descriptor(&mut self, table: &'static ScatterSend) -> FrameScatter {
        native!(ScatterSend, table => FrameScatter {
            p_data_blocks: self.pointers.as_ptr().cast(),
            p_data_blocks_size: self.sizes.as_ptr().cast(),
        })
    }

    fn blocks(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.blocks.iter().map(|block| &**block)
    }

    fn copy_contiguous(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.total);
        for block in self.blocks() {
            bytes.extend_from_slice(block);
        }
        bytes
    }
}

/// The first block of a scatter frame, which must hold the whole of the
/// frame's header of `header_bytes` bytes.
fn first_block(blocks: &[Vec<u8>], header_bytes: usize) -> Result<&[u8]> {
    let first = &blocks[0];
    if first.len() < header_bytes {
        return Err(invalid(
            "The first scatter block must hold the frame's complete header",
        ));
    }
    Ok(first)
}

/// The `N` bytes at `offset` of a frame held in `blocks`, if it has them.
fn bytes_at<const N: usize>(blocks: &[Vec<u8>], offset: usize) -> Option<[u8; N]> {
    let mut bytes = blocks.iter().flatten().skip(offset).copied();
    let mut value = [0; N];
    for byte in &mut value {
        *byte = bytes.next()?;
    }
    Some(value)
}

/// One compressed video frame held in several owned blocks.
///
/// The blocks are sent as they are, without being joined: an encoder can hand
/// over its header, payload and parameter sets separately. The first block
/// must hold the frame's complete header: for H.264 and HEVC the 44-byte
/// packet envelope header (see [`packet_envelope`](super::packet_envelope)
/// for the layout), for SpeedHQ its first four bytes.
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
    /// not begin a frame of the format's codec that can be sent and is
    /// delivered (see [`VideoCodec`](super::VideoCodec)).
    pub fn new(
        format: VideoPacketFormat,
        blocks: Vec<Vec<u8>>,
    ) -> std::result::Result<Self, Rejected<Vec<Vec<u8>>>> {
        let check = || {
            let total = ScatterStorage::total(&blocks)?;
            format.validate_for_send(first_block(&blocks, format.header_bytes())?, total)?;
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
        self.storage.blocks()
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
            bytes: AliasableBox::new(self.storage.copy_contiguous().into_boxed_slice()),
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
    /// when a dimension is not positive, the parameters are not ones the
    /// codec is delivered with (see [`AudioCodec`]), the block list is invalid
    /// (as for [`ScatterVideoFrame::new`]), or the blocks do not hold a
    /// sendable AAC packet that agrees with the parameters.
    pub fn new(
        codec: AudioCodec,
        sample_rate: i32,
        channels: i32,
        samples: i32,
        blocks: Vec<Vec<u8>>,
    ) -> std::result::Result<Self, Rejected<Vec<Vec<u8>>>> {
        let check = || {
            codec.validate_delivery(sample_rate, channels, samples)?;
            let total = ScatterStorage::total(&blocks)?;
            if codec == AudioCodec::Aac {
                let header = EnvelopeHeader::read(first_block(&blocks, ENVELOPE_HEADER_BYTES)?)?;
                header.validate_for_send(EnvelopeCodec::Aac, total)?;
                let config = bytes_at(&blocks, header.extra_offset());
                header.validate_aac(config, sample_rate, channels)?;
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
        self.storage.blocks()
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
            bytes: AliasableBox::new(self.storage.copy_contiguous().into_boxed_slice()),
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
            ..descriptors::audio_frame()
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

/// A frame the SDK may still be using, with the descriptors it was given.
///
/// The entry owns the frame it lent, so nothing can read the frame's bytes
/// while the SDK may still write to them.
struct PendingVideo {
    frame: SubmittedVideo,
    descriptor: AliasableBox<NDIlib_video_frame_v2_t>,
    // Kept only so that the address given to the SDK stays valid.
    _scatter: Option<AliasableBox<FrameScatter>>,
}

// SAFETY: the descriptors point only into allocations this entry owns, and
// moving the entry moves none of them. Until the sender that owns the entry
// takes the frame back, once the SDK has finished with it, only the SDK reads
// or writes those allocations.
unsafe impl Send for PendingVideo {}

impl PendingVideo {
    fn contiguous(mut frame: CompressedVideoFrame) -> Self {
        Self {
            descriptor: AliasableBox::new(Box::new(frame.raw())),
            _scatter: None,
            frame: SubmittedVideo::Contiguous(frame),
        }
    }

    /// The entry for a scatter frame, and the address of its block descriptor,
    /// which stays valid for as long as the entry does.
    fn scatter(
        mut frame: ScatterVideoFrame,
        table: &'static ScatterSend,
    ) -> (Self, *const FrameScatter) {
        let scatter = AliasableBox::new(Box::new(frame.storage.descriptor(table)));
        let address = scatter.as_ptr();
        let pending = Self {
            descriptor: AliasableBox::new(Box::new(frame.descriptor())),
            _scatter: Some(scatter),
            frame: SubmittedVideo::Scatter(frame),
        };
        (pending, address)
    }
}

/// The frame most recently submitted asynchronously on each stream.
///
/// The SDK uses an asynchronously sent frame until the next send on the same
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
/// # Sending lends the frame
///
/// The SDK writes to the frames it sends: it stamps the reserved field of
/// their packet envelope. A synchronous send therefore borrows its frame
/// exclusively and an asynchronous one takes it, so no reference to a frame's
/// bytes exists while the SDK may change them, and a frame can be sent again
/// afterwards.
///
/// ```compile_fail,E0502
/// use grafton_ndi::advanced::{CompressedSender, CompressedVideoFrame};
///
/// fn send(sender: &mut CompressedSender, frame: &mut CompressedVideoFrame) {
///     let bytes = frame.data();
///     let _ = sender.send_video(frame);
///     // The SDK may have written to what `bytes` refers to.
///     let _ = bytes.len();
/// }
/// ```
///
/// # What a receiver gets
///
/// The SDK reports nothing about a frame it does not deliver. The frames it
/// would discard for what they are (see [`VideoCodec`](super::VideoCodec) and
/// [`AudioCodec`]) are refused when they are built and when they are sent.
/// Every other frame is delivered, subject to two rules about the stream it
/// belongs to:
///
/// * A program-tier frame reaches the receivers that asked for the
///   full-quality stream, and a preview-tier frame those that asked for the
///   low-bandwidth one. The SDK documentation asks a source to send both.
/// * An H.264 or HEVC stream starts with a keyframe. Until a receiver has had
///   one, the frames that depend on it are not delivered;
///   [`keyframe_required`](Self::keyframe_required) says when a receiver is
///   waiting.
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
/// let mut frame = CompressedVideoFrame::new(format, packet)?;
/// sender.send_video(&mut frame)?;
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

    /// The source this sender advertises; see [`Sender::source`].
    ///
    /// # Errors
    ///
    /// As for [`Sender::source`].
    pub fn source(&self) -> Result<Source> {
        self.sender.source()
    }

    /// Send a video frame and return when the SDK has finished with it.
    ///
    /// The frame is borrowed exclusively because the SDK writes to its bytes.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when the frame is
    /// not sendable; see [`CompressedVideoFrame::validate_for_send`].
    pub fn send_video(&mut self, frame: &mut CompressedVideoFrame) -> Result<()> {
        frame.validate_for_send()?;
        let descriptor = frame.raw();
        // SAFETY: the sender is live, and the descriptor lends `frame`, which
        // is borrowed exclusively for longer than this synchronous call.
        unsafe {
            self.core
                .NDIlib_send_send_video_v2(self.instance(), &descriptor)
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
                // SAFETY: the sender is live, and the pending entry owns the
                // descriptor and the bytes it lends until the SDK has finished
                // with them.
                unsafe { core.NDIlib_send_send_video_async_v2(instance, next.descriptor.as_ptr()) }
            }))
    }

    /// Send an audio frame and return when the SDK has finished with it.
    ///
    /// The frame is borrowed exclusively because the SDK writes to its bytes.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when the frame is
    /// not sendable; see [`CompressedAudioFrame::validate_for_send`].
    pub fn send_audio(&mut self, frame: &mut CompressedAudioFrame) -> Result<()> {
        frame.validate_for_send()?;
        let descriptor = frame.raw();
        // SAFETY: the sender is live, and the descriptor lends `frame`, which
        // is borrowed exclusively for longer than this synchronous call.
        unsafe {
            self.core
                .NDIlib_send_send_audio_v3(self.instance(), &descriptor)
        };
        Ok(())
    }

    /// Send a video frame held in several blocks and return when the SDK has
    /// finished with it.
    ///
    /// The frame is borrowed exclusively because the SDK writes to its blocks.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// when the runtime has no scatter sending.
    pub fn send_scatter_video(&mut self, frame: &mut ScatterVideoFrame) -> Result<()> {
        let table = self.sender.runtime().table::<ScatterSend>()?;
        let descriptor = frame.descriptor();
        let scatter = frame.storage.descriptor(table);
        // SAFETY: the sender is live, and both descriptors lend `frame`, which
        // is borrowed exclusively for longer than this synchronous call.
        native!(ScatterSend, table => unsafe {
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
            // SAFETY: the sender is live, and the pending entry owns both
            // descriptors and the blocks they lend until the SDK has finished
            // with them.
            native!(ScatterSend, table => unsafe {
                table.NDIlib_send_send_video_scatter_async(
                    instance,
                    next.descriptor.as_ptr(),
                    scatter,
                )
            })
        }))
    }

    /// Send an audio frame held in several blocks and return when the SDK has
    /// finished with it.
    ///
    /// The frame is borrowed exclusively because the SDK writes to its blocks.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// when the runtime has no scatter sending.
    pub fn send_scatter_audio(&mut self, frame: &mut ScatterAudioFrame) -> Result<()> {
        let table = self.sender.runtime().table::<ScatterSend>()?;
        let descriptor = frame.descriptor();
        let scatter = frame.storage.descriptor(table);
        // SAFETY: the sender is live, and both descriptors lend `frame`, which
        // is borrowed exclusively for longer than this synchronous call.
        native!(ScatterSend, table => unsafe {
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
    use crate::advanced::{
        packet_envelope,
        packets::testing::{aac_config, aac_packet},
        VideoCodec,
    };

    fn h264() -> (VideoPacketFormat, Vec<u8>) {
        (
            VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap(),
            packet_envelope(EnvelopeCodec::H264, 10, 9, true, &[1, 2, 3], &[4, 5]).unwrap(),
        )
    }

    /// `bytes` cut into blocks at each of `cuts`.
    fn split(bytes: &[u8], cuts: &[usize]) -> Vec<Vec<u8>> {
        let mut blocks = Vec::new();
        let mut start = 0;
        for end in cuts.iter().copied().chain([bytes.len()]) {
            blocks.push(bytes[start..end].to_vec());
            start = end;
        }
        blocks
    }

    fn scatter_audio(
        codec: AudioCodec,
        sample_rate: i32,
        channels: i32,
        samples: i32,
        blocks: Vec<Vec<u8>>,
    ) -> Result<ScatterAudioFrame> {
        ScatterAudioFrame::new(codec, sample_rate, channels, samples, blocks)
            .map_err(Rejected::into_error)
    }

    #[test]
    fn validates_complete_first_header_and_segmented_total() {
        let (format, bytes) = h264();
        assert!(ScatterVideoFrame::new(format, split(&bytes, &[43])).is_err());
        assert!(
            ScatterVideoFrame::new(format, vec![bytes[..44].to_vec(), bytes[44..48].to_vec()])
                .is_err()
        );
        assert!(ScatterVideoFrame::new(format, vec![bytes.clone(), vec![0]]).is_err());
        assert!(ScatterVideoFrame::new(format, vec![bytes.clone(), vec![]]).is_err());
        let frame = ScatterVideoFrame::new(format, split(&bytes, &[44, 47])).unwrap();
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
            assert_eq!(block.as_ptr().cast::<u8>(), *pointer);
        }
        assert_eq!(moved.into_contiguous_copy().data(), bytes);
        fn send_sync<T: Send + Sync>() {}
        send_sync::<ScatterVideoFrame>();
        send_sync::<ScatterAudioFrame>();
        send_sync::<CompressedVideoFrame>();
        send_sync::<CompressedAudioFrame>();
        fn send<T: Send>() {}
        send::<CompressedSender>();
    }

    #[test]
    fn scatter_video_is_admitted_on_the_same_terms_as_contiguous_video() {
        let reason =
            |result: std::result::Result<ScatterVideoFrame, Rejected<Vec<Vec<u8>>>>| match result
                .map_err(Rejected::into_error)
            {
                Err(crate::Error::InvalidFrame(reason)) => reason,
                other => panic!("expected an invalid frame, got {other:?}"),
            };
        let speedhq = VideoPacketFormat::new(VideoCodec::SpeedHq422, 1920, 1080).unwrap();
        ScatterVideoFrame::new(speedhq, vec![vec![99, 0, 0, 0], vec![1; 8]]).unwrap();
        let refused = ScatterVideoFrame::new(speedhq, vec![vec![100, 0, 0, 0], vec![1; 8]]);
        assert!(reason(refused).contains("quality"));
        let refused = ScatterVideoFrame::new(speedhq, vec![vec![0, 0, 0], vec![1; 8]]);
        assert!(reason(refused).contains("complete header"));

        let (format, bytes) = h264();
        let preview = |width, height| {
            let format = VideoPacketFormat::new(VideoCodec::H264, width, height)
                .unwrap()
                .tier(StreamTier::Preview);
            ScatterVideoFrame::new(format, split(&bytes, &[44]))
        };
        preview(640, 640).unwrap();
        assert!(reason(preview(641, 360)).contains("640 pixels"));
        assert!(reason(preview(640, 641)).contains("640 pixels"));
        assert!(reason(ScatterVideoFrame::new(format, split(&bytes, &[43]))).contains("header"));

        // A keyframe needs its parameter sets here too.
        let bare = packet_envelope(EnvelopeCodec::H264, 0, 0, true, &[1, 2, 3], &[]).unwrap();
        let refused = ScatterVideoFrame::new(format, split(&bare, &[44]));
        assert!(reason(refused).contains("codec configuration"));
    }

    #[test]
    fn rejected_blocks_are_returned_in_order() {
        let (format, bytes) = h264();
        let blocks = split(&bytes, &[43]);
        let rejected = ScatterVideoFrame::new(format, blocks.clone()).unwrap_err();
        assert_eq!(rejected.into_inner(), blocks);

        let rejected =
            ScatterAudioFrame::new(AudioCodec::Opus, 48_000, 0, 960, vec![vec![1], vec![2]])
                .unwrap_err();
        assert_eq!(rejected.into_inner(), [[1], [2]]);
    }

    /// The SDK stamps the reserved field of the envelope in the first block
    /// through the pointer the block array gives it, after the frame has
    /// moved to wherever the pending send keeps it.
    #[test]
    fn a_scatter_frame_the_sdk_has_written_to_is_read_back_and_sendable() {
        let (format, bytes) = h264();
        let frame = ScatterVideoFrame::new(format, split(&bytes, &[44])).unwrap();
        let first = frame.storage.pointers[0].cast_mut();
        let moved = Box::new(frame);
        // SAFETY: the pointer was created to be written through, the block
        // holds a complete header, and no reference to it exists.
        unsafe { first.add(24).cast::<u64>().write_unaligned(7) };
        let stamped = moved.blocks().next().unwrap().to_vec();
        assert_eq!(stamped[24..32], 7u64.to_ne_bytes());
        assert_eq!(stamped[..24], bytes[..24]);

        let joined = moved.into_contiguous_copy();
        joined.validate_for_send().unwrap();
        assert_eq!(joined.data()[24..32], 7u64.to_ne_bytes());
    }

    #[test]
    fn scatter_audio_is_admitted_on_the_same_terms_as_contiguous_audio() {
        let packet = aac_packet(48_000, 2, &[1, 2]);
        let config_at = packet.len() - 2;
        // The configuration may lie in any block, or across two.
        for cuts in [
            &[][..],
            &[44][..],
            &[44, config_at][..],
            &[44, config_at + 1][..],
        ] {
            let aac = scatter_audio(AudioCodec::Aac, 48_000, 2, 1024, split(&packet, cuts))
                .unwrap()
                .into_contiguous_copy();
            assert_eq!(aac.data(), packet);
            aac.validate_for_send().unwrap();
        }

        let reason = |result: Result<ScatterAudioFrame>| match result {
            Err(crate::Error::InvalidFrame(reason)) => reason,
            other => panic!("expected an invalid frame, got {other:?}"),
        };
        let aac = |sample_rate, channels, samples, packet: &[u8]| {
            reason(scatter_audio(
                AudioCodec::Aac,
                sample_rate,
                channels,
                samples,
                split(packet, &[44, config_at + 1]),
            ))
        };
        assert!(aac(48_000, 2, 960, &packet).contains("1024 samples"));
        assert!(aac(44_100, 2, 1024, &packet).contains("sample rate"));
        assert!(aac(48_000, 1, 1024, &packet).contains("channel layout"));

        let mut unflagged = packet.clone();
        unflagged[32..36].copy_from_slice(&0u32.to_ne_bytes());
        assert!(aac(48_000, 2, 1024, &unflagged).contains("keyframe flag"));

        let config = aac_config(48_000, 2);
        for config in [&[][..], &config[..1]] {
            let bare = packet_envelope(EnvelopeCodec::Aac, 0, 0, true, &[1, 2], config).unwrap();
            let result = scatter_audio(AudioCodec::Aac, 48_000, 2, 1024, split(&bare, &[44]));
            assert!(reason(result).contains("AudioSpecificConfig"));
        }

        let opus = |sample_rate, samples| {
            scatter_audio(
                AudioCodec::Opus,
                sample_rate,
                2,
                samples,
                vec![vec![1], vec![2]],
            )
        };
        let frame = opus(48_000, 960).unwrap().into_contiguous_copy();
        assert_eq!(frame.data(), &[1, 2]);
        frame.validate_for_send().unwrap();
        assert!(reason(opus(44_100, 882)).contains("48000 Hz"));
        assert!(reason(opus(48_000, 1440)).contains("ms of audio"));
        assert!(reason(opus(0, 960)).contains("positive"));
    }

    #[test]
    fn bytes_are_found_wherever_the_blocks_put_them() {
        let blocks = vec![vec![0, 1], vec![2], vec![3, 4, 5]];
        assert_eq!(bytes_at(&blocks, 0), Some([0, 1]));
        assert_eq!(bytes_at(&blocks, 1), Some([1, 2, 3]));
        assert_eq!(bytes_at(&blocks, 4), Some([4, 5]));
        assert_eq!(bytes_at::<2>(&blocks, 5), None);
        assert_eq!(bytes_at::<1>(&blocks, 6), None);
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
        let old_program_header = first.descriptor.as_ptr();
        assert!(streams.submit(first, |_| {}).is_none());
        assert!(streams
            .submit(make(StreamTier::Preview), |_| {
                // The earlier program descriptor and bytes survive both moving
                // their owners and a preview submission, as the SDK requires,
                // and the SDK can still write to the bytes.
                unsafe {
                    assert_eq!((*old_program_header).p_data, old_program_data);
                    old_program_data.add(24).write(9);
                }
            })
            .is_none());
        let returned = streams.submit(make(StreamTier::Program), |_| {}).unwrap();
        assert_eq!(returned.format().stream_tier(), StreamTier::Program);
        let SubmittedVideo::Contiguous(returned) = returned else {
            panic!("a contiguous frame was submitted");
        };
        assert_eq!(returned.data()[24], 9);
        assert_eq!(returned.data()[..24], bytes[..24]);
        assert_eq!(returned.data()[25..], bytes[25..]);
        returned.validate_for_send().unwrap();
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
