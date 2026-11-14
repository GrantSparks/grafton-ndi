//! Sending compressed frames, held in one buffer or in several.

#[cfg(any(test, ndi_has_advanced_scatter_send))]
use std::ptr;

use super::{
    aliasable::AliasableVec,
    invalid_frame,
    packets::{frame_extras_accessors, AudioParameters, FrameExtras},
    AdvancedConfig, AudioCodec, CompressedAudioFrame, CompressedVideoFrame, VideoPacketFormat,
    MAX_PACKET_BYTES,
};
use crate::{
    sender::{
        in_flight::{self, Destroyed, InFlight, Lend, Lent},
        SendHandle,
    },
    sys::{
        advanced::{native, ScatterSend},
        bindings::*,
    },
    CompressedAttached, Ndi, Rejected, Result, Sender, SenderOptions, StreamTier,
};

#[cfg(any(test, ndi_has_advanced_scatter_send))]
use super::aliasable::AliasableBox;
#[cfg(ndi_has_advanced_scatter_send)]
use crate::sys::advanced::FrameScatter;

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
    blocks: Box<[AliasableVec<u8>]>,
    #[cfg(any(test, ndi_has_advanced_scatter_send))]
    pointers: AliasableBox<[*const u8]>,
    #[cfg(any(test, ndi_has_advanced_scatter_send))]
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

impl Clone for ScatterStorage {
    /// Copies of the blocks, with arrays that describe the copies.
    fn clone(&self) -> Self {
        Self::new(self.blocks().map(<[u8]>::to_vec).collect(), self.total)
    }
}

impl ScatterStorage {
    /// Check a block list and return its total size in bytes.
    fn total(blocks: &[Vec<u8>]) -> Result<usize> {
        if blocks.is_empty() || blocks.len() > MAX_SCATTER_BLOCKS {
            return Err(invalid_frame("Invalid scatter block count"));
        }
        let mut total = 0usize;
        for block in blocks {
            if block.is_empty() {
                return Err(invalid_frame("Scatter blocks must not be empty"));
            }
            total = total
                .checked_add(block.len())
                .filter(|total| *total <= MAX_PACKET_BYTES)
                .ok_or_else(|| invalid_frame("Scatter frame exceeds the packet size limit"))?;
        }
        Ok(total)
    }

    /// Take ownership of blocks that [`total`](Self::total) accepted.
    fn new(blocks: Vec<Vec<u8>>, total: usize) -> Self {
        let blocks: Box<[_]> = blocks.into_iter().map(AliasableVec::new).collect();
        #[cfg(any(test, ndi_has_advanced_scatter_send))]
        let mut blocks = blocks;
        // The SDK writes through these, whatever its declaration says.
        #[cfg(any(test, ndi_has_advanced_scatter_send))]
        let pointers = blocks
            .iter_mut()
            .map(|block| block.as_mut_ptr().cast::<u8>().cast_const())
            .chain(std::iter::once(ptr::null()))
            .collect();
        // Each block is no larger than the total, which is below i32::MAX.
        #[cfg(any(test, ndi_has_advanced_scatter_send))]
        let sizes = blocks
            .iter()
            .map(|block| block.len() as i32)
            .chain(std::iter::once(0))
            .collect();
        Self {
            blocks,
            #[cfg(any(test, ndi_has_advanced_scatter_send))]
            pointers: AliasableBox::new(pointers),
            #[cfg(any(test, ndi_has_advanced_scatter_send))]
            sizes: AliasableBox::new(sizes),
            total,
        }
    }

    /// The native description of the blocks, which lends them to the SDK.
    /// The SDK may write to the blocks for as long as it holds the
    /// description, so the caller keeps the storage exclusively for that long.
    #[cfg(ndi_has_advanced_scatter_send)]
    fn descriptor(&mut self, table: &'static ScatterSend) -> FrameScatter {
        native!(ScatterSend, table => FrameScatter {
            p_data_blocks: self.pointers.as_ptr().cast(),
            p_data_blocks_size: self.sizes.as_ptr().cast(),
        })
    }

    fn blocks(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.blocks.iter().map(|block| &**block)
    }

    fn into_blocks(self) -> Vec<Vec<u8>> {
        self.blocks
            .into_vec()
            .into_iter()
            .map(AliasableVec::into_vec)
            .collect()
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
        return Err(invalid_frame(
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
/// packet envelope header (see [`PacketEnvelope`](super::PacketEnvelope)),
/// for SpeedHQ its first four bytes.
///
/// # Examples
///
/// ```
/// use grafton_ndi::advanced::{
///     EnvelopeCodec, PacketEnvelope, ScatterVideoFrame, VideoCodec, VideoPacketFormat,
/// };
///
/// let packet =
///     PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, true, &[0x65; 64], &[0x67; 8])?;
/// let (header, rest) = packet.split_at(44);
/// let frame = ScatterVideoFrame::new(
///     VideoPacketFormat::new(VideoCodec::H264, 1280, 720)?,
///     vec![header.to_vec(), rest.to_vec()],
/// )?;
/// assert_eq!(frame.total_bytes(), packet.len());
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct ScatterVideoFrame {
    format: VideoPacketFormat,
    storage: ScatterStorage,
    extras: FrameExtras,
}

impl ScatterVideoFrame {
    frame_extras_accessors!();

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
                extras: FrameExtras::default(),
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

    /// Recover each original block, preserving order, allocation and capacity.
    ///
    /// The outer vector may be newly allocated; block payloads are not copied.
    /// Any SDK envelope stamping performed during sending is retained.
    pub fn into_blocks(self) -> Vec<Vec<u8>> {
        self.storage.into_blocks()
    }

    /// The size of the whole frame in bytes.
    pub fn total_bytes(&self) -> usize {
        self.storage.total
    }

    /// Join the blocks into one newly allocated contiguous frame.
    pub fn into_contiguous_copy(self) -> CompressedVideoFrame {
        CompressedVideoFrame {
            format: self.format,
            bytes: AliasableVec::new(self.storage.copy_contiguous()),
            extras: self.extras,
        }
    }

    /// A native descriptor without data: a scatter send takes the payload
    /// from the block arrays and ignores the frame's own data fields.
    #[cfg(ndi_has_advanced_scatter_send)]
    fn descriptor(&self) -> NDIlib_video_frame_v2_t {
        self.format.descriptor(&self.extras)
    }
}

/// One compressed audio frame held in several owned blocks.
///
/// For AAC the first block must hold the complete 44-byte packet envelope
/// header; Opus has no envelope.
#[derive(Clone, Debug)]
pub struct ScatterAudioFrame {
    codec: AudioCodec,
    storage: ScatterStorage,
    parameters: AudioParameters,
    protected: bool,
    extras: FrameExtras,
}

impl ScatterAudioFrame {
    frame_extras_accessors!();

    /// Take ownership of one frame's blocks, in order. `num_samples` is the
    /// number of samples per channel the frame decodes to.
    ///
    /// # Errors
    ///
    /// Returns the blocks with [`Error::InvalidFrame`](crate::Error::InvalidFrame)
    /// when a parameter is not in `1..=i32::MAX`, the parameters are not ones
    /// the codec is delivered with (see [`AudioCodec`]), the block list is
    /// invalid (as for [`ScatterVideoFrame::new`]), or the blocks do not hold
    /// a sendable AAC packet that agrees with the parameters.
    pub fn new(
        codec: AudioCodec,
        sample_rate: u32,
        num_channels: u32,
        num_samples: u32,
        blocks: Vec<Vec<u8>>,
    ) -> std::result::Result<Self, Rejected<Vec<Vec<u8>>>> {
        let check = || {
            let parameters = AudioParameters::new(sample_rate, num_channels, num_samples)?;
            let total = ScatterStorage::total(&blocks)?;
            let header = first_block(&blocks, codec.header_bytes())?;
            codec.validate_for_send(parameters, header, total, |offset| {
                bytes_at(&blocks, offset)
            })?;
            Ok((parameters, total))
        };
        match check() {
            Ok((parameters, total)) => Ok(Self {
                codec,
                storage: ScatterStorage::new(blocks, total),
                parameters,
                protected: false,
                extras: FrameExtras::default(),
            }),
            Err(error) => Err(Rejected::new(error, blocks)),
        }
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
    pub fn sample_rate(&self) -> u32 {
        self.parameters.sample_rate
    }

    /// Number of channels.
    pub fn num_channels(&self) -> u32 {
        self.parameters.num_channels
    }

    /// Samples per channel the frame decodes to.
    pub fn num_samples(&self) -> u32 {
        self.parameters.num_samples
    }

    /// The blocks, in order.
    pub fn blocks(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.storage.blocks()
    }

    /// Recover each original block, preserving order, allocation and capacity.
    ///
    /// The outer vector may be newly allocated; block payloads are not copied.
    /// Any SDK envelope stamping performed during sending is retained.
    pub fn into_blocks(self) -> Vec<Vec<u8>> {
        self.storage.into_blocks()
    }

    /// The size of the whole frame in bytes.
    pub fn total_bytes(&self) -> usize {
        self.storage.total
    }

    /// Set whether the frame belongs to a protected stream.
    pub fn set_protected(&mut self, protected: bool) {
        self.protected = protected;
    }

    /// Join the blocks into one newly allocated contiguous frame.
    pub fn into_contiguous_copy(self) -> CompressedAudioFrame {
        CompressedAudioFrame {
            codec: self.codec,
            protected: self.protected,
            bytes: AliasableVec::new(self.storage.copy_contiguous()),
            parameters: self.parameters,
            extras: self.extras,
        }
    }

    /// A native descriptor without data; see [`ScatterVideoFrame::descriptor`].
    #[cfg(ndi_has_advanced_scatter_send)]
    fn descriptor(&self) -> NDIlib_audio_frame_v3_t {
        self.parameters
            .descriptor(self.codec, self.protected, &self.extras)
    }
}

/// A video frame that was submitted asynchronously and that the SDK has
/// finished with.
#[derive(Clone, Debug)]
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

/// Frames reclaimed from a [`CompressedSender`] when it closes.
///
/// The SDK has finished accessing these frames, so their storage can be reused.
/// Reclaiming them does not report whether they were delivered to a receiver.
#[derive(Clone, Debug, Default)]
pub struct ReclaimedVideo {
    /// The last frame submitted asynchronously on the program stream.
    pub program: Option<SubmittedVideo>,
    /// The last frame submitted asynchronously on the preview stream.
    pub preview: Option<SubmittedVideo>,
}

/// A frame the SDK may still be using, with what it accesses through the
/// descriptor it was given.
///
/// `NDIlib_send_send_video_async_v2` (Processing.NDI.Send.h), whose rules the
/// scatter variant (Processing.NDI.Advanced.h) follows, reads the video
/// descriptor during the call: the SDK's own asynchronous sending example
/// rewrites one descriptor while the previous frame is still in flight. What
/// the descriptor points to must not be freed or reused until the next
/// synchronizing event. For a contiguous frame that is its bytes and
/// metadata. For a scatter frame it is also the scatter list, which no header
/// says is copied: the `NDIlib_frame_scatter_t` and the pointer and size
/// arrays it refers to.
///
/// The entry owns all of these, so nothing can read the frame's bytes while
/// the SDK may still write to them.
enum PendingVideo {
    /// A frame submitted with [`CompressedSender::send_video_async`].
    Contiguous(CompressedVideoFrame),
    /// A frame submitted with [`CompressedSender::send_scatter_video_async`].
    #[cfg(ndi_has_advanced_scatter_send)]
    Scatter {
        frame: ScatterVideoFrame,
        // The scatter list given to the SDK, kept only so that its address
        // stays valid. Its arrays belong to the frame.
        list: AliasableBox<FrameScatter>,
        table: &'static ScatterSend,
    },
}

// SAFETY: the scatter list points only into allocations this entry owns, and
// moving the entry moves none of them. Until the sender that owns the entry
// takes the frame back, once the SDK has finished with it, only the SDK reads
// or writes those allocations.
unsafe impl Send for PendingVideo {}
// SAFETY: a shared reference to an entry reads only its frame's format, which
// the SDK never writes; it never dereferences the scatter list's pointers.
// Everything that lends the entry to the SDK or takes it back goes through
// `&mut` or by value. The generated `NDIlib_frame_scatter_t` holds raw
// pointers, which would otherwise make this entry, and with it
// `CompressedSender`, `Sync` only in builds without the Advanced headers.
unsafe impl Sync for PendingVideo {}

impl PendingVideo {
    /// The entry for a scatter frame, sent through `table`, with a scatter
    /// list that stays where it is for as long as the entry exists.
    #[cfg(ndi_has_advanced_scatter_send)]
    fn scatter(mut frame: ScatterVideoFrame, table: &'static ScatterSend) -> Self {
        let list = AliasableBox::new(Box::new(frame.storage.descriptor(table)));
        Self::Scatter { frame, list, table }
    }

    /// The stream the frame is sent on.
    fn tier(&self) -> StreamTier {
        match self {
            Self::Contiguous(frame) => frame.format().tier(),
            #[cfg(ndi_has_advanced_scatter_send)]
            Self::Scatter { frame, .. } => frame.format().tier(),
        }
    }

    fn into_submitted(self) -> SubmittedVideo {
        match self {
            Self::Contiguous(frame) => SubmittedVideo::Contiguous(frame),
            #[cfg(ndi_has_advanced_scatter_send)]
            Self::Scatter { frame, .. } => SubmittedVideo::Scatter(frame),
        }
    }
}

impl Lend for PendingVideo {
    /// The video descriptor to submit this entry with, which the SDK reads
    /// only during the call, and for a scatter frame its scatter list. The
    /// descriptor lends the frame's bytes, or for a scatter frame only its
    /// metadata; the list lends the blocks. All of them belong to the entry.
    fn lend(&mut self) -> Lent {
        match self {
            Self::Contiguous(frame) => Lent::Frame(frame.raw()),
            #[cfg(ndi_has_advanced_scatter_send)]
            Self::Scatter { frame, list, table } => Lent::Scatter {
                table,
                frame: frame.descriptor(),
                scatter: list.as_ptr(),
            },
        }
    }
}

/// The frame most recently submitted asynchronously on each stream.
///
/// The SDK uses an asynchronously sent frame until the next send on the same
/// stream returns, so each stream keeps exactly one frame in flight, and an
/// earlier frame comes back only with the proof that the submission that
/// replaced it has returned, or that the native sender was destroyed.
#[derive(Default)]
struct PendingStreams {
    program: InFlight<PendingVideo>,
    preview: InFlight<PendingVideo>,
}

impl PendingStreams {
    /// Hand `next` to the SDK through `sender`, then take back the frame it
    /// replaces on the same stream.
    fn submit(&mut self, sender: SendHandle, next: PendingVideo) -> Option<SubmittedVideo> {
        let slot = match next.tier() {
            StreamTier::Program => &mut self.program,
            StreamTier::Preview => &mut self.preview,
        };
        // The other stream's frame stays in flight in its own slot.
        let (displaced, synchronized) = slot.submit(sender, next);
        displaced
            .release(synchronized)
            .map(PendingVideo::into_submitted)
    }

    /// Both streams' frames, once their native sender has been destroyed.
    fn into_reclaimed(self, destroyed: &Destroyed) -> ReclaimedVideo {
        let release = |slot: InFlight<PendingVideo>| {
            slot.into_released(destroyed)
                .map(PendingVideo::into_submitted)
        };
        ReclaimedVideo {
            program: release(self.program),
            preview: release(self.preview),
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
/// The native sender is owned exclusively by this value.
/// [`sender`](Self::sender) lends it in the [`CompressedAttached`] mode, which
/// has the operations every sender shares (the source, metadata in both
/// directions, tally, connections, failover, and the Advanced encoder
/// guidance, keyframe and tally controls) but none of the uncompressed sends,
/// and which cannot be registered with a
/// [`SendAdvertiser`](crate::discovery::SendAdvertiser).
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
///   [`keyframe_required`](Sender::keyframe_required) on
///   [`sender`](Self::sender) says when a receiver is waiting.
///
/// # Examples
///
/// ```no_run
/// use grafton_ndi::{
///     advanced::{
///         CompressedSender, CompressedVideoFrame, EnvelopeCodec, PacketEnvelope, VideoCodec,
///         VideoPacketFormat,
///     },
///     FrameRate, Ndi, SenderOptions,
/// };
///
/// # fn encoded_access_unit() -> (Vec<u8>, Vec<u8>) { (vec![0], vec![0]) }
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = Ndi::new()?;
/// let mut sender = CompressedSender::new(&ndi, &SenderOptions::new("Encoder"))?;
/// let format = VideoPacketFormat::new(VideoCodec::H264, 1920, 1080)?
///     .with_frame_rate(FrameRate::new(60, 1)?);
///
/// let (payload, parameter_sets) = encoded_access_unit();
/// let packet =
///     PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, true, &payload, &parameter_sets)?;
/// let mut frame = CompressedVideoFrame::new(format, packet)?;
/// sender.send_video(&mut frame)?;
/// # Ok(())
/// # }
/// ```
pub struct CompressedSender {
    // Declared first so that it is destroyed first: destroying the native
    // sender is what ends the SDK's use of the pending frames. Its mode owns
    // the native sender exclusively, so no other owner, such as an advertiser
    // registration, can keep it alive after they are freed.
    sender: Sender<CompressedAttached>,
    pending: PendingStreams,
}

impl CompressedSender {
    /// Create a sender with the options' Advanced SDK
    /// [configuration](SenderOptions::config), or the SDK's default one when
    /// they carry none.
    ///
    /// # Errors
    ///
    /// As for [`Sender::new`], and
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime (or in a build without the Advanced SDK).
    pub fn new(ndi: &Ndi, options: &SenderOptions) -> Result<Self> {
        let config = AdvancedConfig::or_empty(options.config.as_ref());
        Ok(Self {
            sender: Sender::new_configured(ndi, options, &config)?,
            pending: PendingStreams::default(),
        })
    }

    /// The sender this value sends through, for the operations that do not
    /// send video or audio: [`source`](Sender::source),
    /// [`send_metadata`](Sender::send_metadata) and
    /// [`capture_metadata`](Sender::capture_metadata),
    /// [`tally`](Sender::tally), [`connection_count`](Sender::connection_count),
    /// connection metadata, failover, and the Advanced controls such as
    /// [`keyframe_required`](Sender::keyframe_required) and
    /// [`set_external_tally`](Sender::set_external_tally).
    ///
    /// Its mode has none of the uncompressed sends, and it cannot be
    /// registered with an advertiser. It is `Sync`, but it borrows this value,
    /// so it is used between sends rather than during one.
    pub fn sender(&self) -> &Sender<CompressedAttached> {
        &self.sender
    }

    fn instance(&self) -> NDIlib_send_instance_t {
        self.sender.native_instance()
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
            self.sender
                .core()
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
        Ok(self
            .pending
            .submit(self.sender.handle(), PendingVideo::Contiguous(frame)))
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
            self.sender
                .core()
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
    pub fn send_scatter_video(&mut self, _frame: &mut ScatterVideoFrame) -> Result<()> {
        let table = self.sender.runtime().table::<ScatterSend>()?;
        native!(ScatterSend, table => {
            let descriptor = _frame.descriptor();
            let scatter = _frame.storage.descriptor(table);
            // SAFETY: the sender is live, and both descriptors lend `frame`, which
            // is borrowed exclusively for longer than this synchronous call.
            unsafe {
                table.NDIlib_send_send_video_scatter(self.instance(), &descriptor, &scatter)
            };
            Ok(())
        })
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
        native!(ScatterSend, table => {
            Ok(self.pending.submit(self.sender.handle(), PendingVideo::scatter(frame, table)))
        })
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
    pub fn send_scatter_audio(&mut self, _frame: &mut ScatterAudioFrame) -> Result<()> {
        let table = self.sender.runtime().table::<ScatterSend>()?;
        native!(ScatterSend, table => {
            let descriptor = _frame.descriptor();
            let scatter = _frame.storage.descriptor(table);
            // SAFETY: the sender is live, and both descriptors lend `frame`, which
            // is borrowed exclusively for longer than this synchronous call.
            unsafe {
                table.NDIlib_send_send_audio_scatter(self.instance(), &descriptor, &scatter)
            };
            Ok(())
        })
    }

    /// Destroy the sender and take back the frames it still held.
    ///
    /// Destroying the native sender waits for the SDK to finish with every
    /// pending frame, without a timeout.
    pub fn close(self) -> ReclaimedVideo {
        let Self { sender, pending } = self;
        pending.into_reclaimed(&in_flight::destroy(sender))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advanced::{
        packets::testing::{aac_config, aac_packet},
        EnvelopeCodec, PacketEnvelope, VideoCodec,
    };
    use crate::Timecode;

    fn h264() -> (VideoPacketFormat, Vec<u8>) {
        (
            VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap(),
            PacketEnvelope::encode(EnvelopeCodec::H264, 10, 9, true, &[1, 2, 3], &[4, 5]).unwrap(),
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
        sample_rate: u32,
        channels: u32,
        samples: u32,
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
        // With and without the Advanced headers alike.
        send_sync::<CompressedSender>();
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
                .with_tier(StreamTier::Preview);
            ScatterVideoFrame::new(format, split(&bytes, &[44]))
        };
        preview(640, 640).unwrap();
        assert!(reason(preview(641, 360)).contains("640 pixels"));
        assert!(reason(preview(640, 641)).contains("640 pixels"));
        assert!(reason(ScatterVideoFrame::new(format, split(&bytes, &[43]))).contains("header"));

        // A keyframe needs its parameter sets here too.
        let bare =
            PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, true, &[1, 2, 3], &[]).unwrap();
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

    /// A clone owns copies of the blocks, and its arrays describe the copies.
    #[test]
    fn a_cloned_scatter_frame_describes_its_own_blocks() {
        let (format, bytes) = h264();
        let mut frame = ScatterVideoFrame::new(format, split(&bytes, &[44, 47])).unwrap();
        frame.set_timecode(Timecode::from_ticks(3));
        frame.set_metadata("<scatter/>").unwrap();
        let copy = frame.clone();
        assert_eq!(
            (copy.timecode(), copy.metadata()),
            (Timecode::from_ticks(3), Some("<scatter/>"))
        );
        let blocks = copy.storage.blocks.iter().zip(copy.storage.pointers.iter());
        for ((block, pointer), original) in blocks.zip(frame.storage.blocks.iter()) {
            assert_eq!(block.as_ptr().cast::<u8>(), *pointer);
            assert_ne!(block.as_ptr(), original.as_ptr());
            assert_eq!(**block, **original);
        }
        assert_eq!(&*copy.storage.sizes, &*frame.storage.sizes);
        assert!(copy.storage.pointers[3].is_null());

        let mut audio =
            scatter_audio(AudioCodec::Opus, 48_000, 2, 960, vec![vec![1], vec![2]]).unwrap();
        audio.set_protected(true);
        let copy = audio.clone();
        assert!(copy.is_protected());
        assert_eq!(
            (copy.sample_rate(), copy.num_channels(), copy.num_samples()),
            (48_000, 2, 960)
        );
        assert_eq!(copy.into_contiguous_copy().data(), &[1, 2]);
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
            let bare =
                PacketEnvelope::encode(EnvelopeCodec::Aac, 0, 0, true, &[1, 2], config).unwrap();
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
        assert!(reason(opus(0, 960)).contains("1..="));
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

    /// The SDK reads an asynchronous send's video descriptor during the call,
    /// so a submission lends what the descriptor points to and not the
    /// descriptor: the frame, and for a scatter frame its scatter list, all of
    /// which the pending entry owns.
    #[test]
    fn async_submission_lends_what_the_pending_entry_owns() {
        let (format, bytes) = h264();
        let frame = CompressedVideoFrame::new(format, bytes.clone()).unwrap();
        let data = frame.data().as_ptr();
        let mut pending = PendingVideo::Contiguous(frame);
        let (lent, allocations) = crate::test_alloc::allocation_count(|| pending.lend());
        assert_eq!(allocations, 0);
        #[cfg(not(ndi_has_advanced_scatter_send))]
        let Lent::Frame(descriptor) = lent;
        #[cfg(ndi_has_advanced_scatter_send)]
        let Lent::Frame(descriptor) = lent
        else {
            panic!("a contiguous frame is lent through one descriptor");
        };
        assert_eq!(descriptor.p_data.cast_const(), data);
    }

    #[test]
    #[cfg(ndi_has_advanced_scatter_send)]
    fn async_scatter_submission_lends_what_the_pending_entry_owns() {
        let (format, bytes) = h264();
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let table = ndi.table::<ScatterSend>().unwrap();
        let frame = ScatterVideoFrame::new(format, split(&bytes, &[44])).unwrap();
        let pointers = frame.storage.pointers.as_ptr().cast::<*const u8>();
        let sizes = frame.storage.sizes.as_ptr().cast::<i32>();
        let (mut pending, allocations) =
            crate::test_alloc::allocation_count(|| PendingVideo::scatter(frame, table));
        // The scatter list is the only new allocation: its arrays belong to
        // the frame.
        assert_eq!(allocations, 1);
        let Lent::Scatter { scatter, .. } = pending.lend() else {
            panic!("a scatter frame is lent with its scatter list");
        };
        // Moving the entry, as its stream's slot does, leaves the list where
        // the SDK was told it is, still naming the frame's arrays.
        let mut moved = Box::new(pending);
        let Lent::Scatter { scatter: after, .. } = moved.lend() else {
            panic!("a scatter frame is lent with its scatter list");
        };
        assert_eq!(after, scatter);
        native!(ScatterSend, table => {
            // SAFETY: the moved entry owns the list.
            let list = unsafe { &*scatter };
            assert_eq!(list.p_data_blocks.cast::<*const u8>(), pointers);
            assert_eq!(list.p_data_blocks_size.cast::<i32>(), sizes);
        });
    }

    /// Each stream keeps its own frame in flight: a submission hands back the
    /// earlier frame of its own stream, the very allocation the SDK was lent,
    /// and closing hands back the frame each stream still held.
    #[test]
    fn async_frames_are_retained_per_stream_until_synchronized() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let (format, bytes) = h264();
        let make = |tier| CompressedVideoFrame::new(format.with_tier(tier), bytes.clone()).unwrap();
        let options = SenderOptions::new(crate::test_support::unique_name("Per-stream retention"))
            .clock_video(false);
        let mut sender = CompressedSender::new(&ndi, &options).unwrap();
        let program = make(StreamTier::Program);
        let program_data = program.data().as_ptr();
        assert!(sender.send_video_async(program).unwrap().is_none());
        // A preview submission leaves the program frame in flight.
        let preview = sender.send_video_async(make(StreamTier::Preview));
        assert!(preview.unwrap().is_none());
        assert!(sender.pending.program.is_in_flight());
        let returned = sender
            .send_video_async(make(StreamTier::Program))
            .unwrap()
            .unwrap();
        let SubmittedVideo::Contiguous(returned) = returned else {
            panic!("a contiguous frame was submitted");
        };
        assert_eq!(returned.format().tier(), StreamTier::Program);
        assert_eq!(returned.data().as_ptr(), program_data);
        // The SDK may have stamped the reserved field, and nothing else.
        assert_eq!(returned.data()[..24], bytes[..24]);
        assert_eq!(returned.data()[32..], bytes[32..]);
        returned.validate_for_send().unwrap();
        let reclaimed = sender.close();
        assert_eq!(
            reclaimed.program.unwrap().format().tier(),
            StreamTier::Program
        );
        assert_eq!(
            reclaimed.preview.unwrap().format().tier(),
            StreamTier::Preview
        );
    }
}
