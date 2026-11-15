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
    sender::in_flight::{Destroyed, InFlight, Lend, Lending, Lent, LoanPair, Loans, Synchronized},
    sys::{
        advanced::{native, ScatterSend},
        bindings::*,
    },
    CompressedAttached, CompressedAudioSendMode, Ndi, Rejected, Result, Sender, SenderOptions,
    StreamTier,
};

#[cfg(any(test, ndi_has_advanced_scatter_send))]
use super::aliasable::AliasableBox;
#[cfg(ndi_has_advanced_scatter_send)]
use crate::sys::advanced::FrameScatter;

/// Maximum number of blocks in one scatter frame.
pub const MAX_SCATTER_BLOCKS: usize = 65_536;

#[cfg(all(test, ndi_has_advanced_scatter_send, ndi_has_advanced_send_completion))]
#[path = "scatter_completion_tests.rs"]
mod scatter_completion_tests;

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
    pointers: BlockPointers,
    #[cfg(any(test, ndi_has_advanced_scatter_send))]
    sizes: AliasableBox<[i32]>,
    total: usize,
}

/// Addresses lent to the SDK, never dereferenced by Rust.
#[cfg(any(test, ndi_has_advanced_scatter_send))]
#[derive(Debug)]
struct BlockPointers {
    array: AliasableBox<[*const u8]>,
    #[cfg(ndi_has_advanced_scatter_send)]
    list: AliasableBox<FrameScatter>,
}

// SAFETY: moving these owners moves neither the list nor the array nor the
// blocks they describe. Rust never dereferences their pointers;
// ScatterStorage::descriptor lends them to the SDK
// only under exclusive access, and the send slot owns the lent frame.
#[cfg(any(test, ndi_has_advanced_scatter_send))]
unsafe impl Send for BlockPointers {}
// SAFETY: shared access only reads the pointer values. The SDK receives them
// through ScatterStorage::descriptor(&mut self), whose exclusive loan lasts
// until the slot has synchronized or the sender has been destroyed.
#[cfg(any(test, ndi_has_advanced_scatter_send))]
unsafe impl Sync for BlockPointers {}

const _: () = {
    const fn send_and_sync<T: Send + Sync>() {}
    send_and_sync::<ScatterStorage>();
};

impl Clone for ScatterStorage {
    /// Copies of the blocks, with arrays that describe the copies.
    fn clone(&self) -> Self {
        Self::new(self.blocks().map(<[u8]>::to_vec).collect(), self.total)
    }
}

impl ScatterStorage {
    fn retained_bytes(&self) -> usize {
        let bytes = self.blocks.iter().fold(
            self.blocks.len() * size_of::<AliasableVec<u8>>(),
            |total, block| total.saturating_add(block.capacity()),
        );
        #[cfg(any(test, ndi_has_advanced_scatter_send))]
        let bytes = bytes
            .saturating_add(self.pointers.array.len() * size_of::<*const u8>())
            .saturating_add(self.sizes.len() * size_of::<i32>());
        #[cfg(ndi_has_advanced_scatter_send)]
        let bytes = bytes.saturating_add(size_of::<FrameScatter>());
        bytes
    }

    /// Check a block list and return its total size in bytes.
    fn total(blocks: &[Vec<u8>]) -> Result<usize> {
        if blocks.is_empty() || blocks.len() > MAX_SCATTER_BLOCKS {
            return Err(invalid_frame(crate::FrameDefect::ScatterBlockCount));
        }
        let mut total = 0usize;
        for block in blocks {
            if block.is_empty() {
                return Err(invalid_frame(crate::FrameDefect::EmptyScatterBlock));
            }
            total = total
                .checked_add(block.len())
                .filter(|total| *total <= MAX_PACKET_BYTES)
                .ok_or_else(|| invalid_frame(crate::FrameDefect::ScatterSize))?;
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
        let pointers = AliasableBox::new(
            blocks
                .iter_mut()
                .map(|block| block.as_mut_ptr().cast::<u8>().cast_const())
                .chain(std::iter::once(ptr::null()))
                .collect(),
        );
        // Each block is no larger than the total, which is below i32::MAX.
        #[cfg(any(test, ndi_has_advanced_scatter_send))]
        let sizes = AliasableBox::new(
            blocks
                .iter()
                .map(|block| block.len() as i32)
                .chain(std::iter::once(0))
                .collect(),
        );
        #[cfg(ndi_has_advanced_scatter_send)]
        let list = AliasableBox::new(Box::new(FrameScatter {
            p_data_blocks: pointers.as_ptr().cast(),
            p_data_blocks_size: sizes.as_ptr().cast(),
        }));
        Self {
            blocks,
            #[cfg(any(test, ndi_has_advanced_scatter_send))]
            pointers: BlockPointers {
                array: pointers,
                #[cfg(ndi_has_advanced_scatter_send)]
                list,
            },
            #[cfg(any(test, ndi_has_advanced_scatter_send))]
            sizes,
            total,
        }
    }

    /// The native description of the blocks, which lends them to the SDK.
    /// The SDK may write to the blocks for as long as it holds the
    /// description, so the caller keeps the storage exclusively for that long.
    #[cfg(ndi_has_advanced_scatter_send)]
    fn descriptor(&mut self) -> *const FrameScatter {
        self.pointers.list.as_ptr()
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
        return Err(invalid_frame(crate::FrameDefect::ScatterHeader));
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

    pub(super) fn retained_bytes(&self) -> usize {
        self.storage
            .retained_bytes()
            .saturating_add(self.extras.retained_bytes())
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

/// Frames reclaimed from a [`CompressedSender`] when it finishes or closes.
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
pub(crate) enum PendingVideo {
    /// A frame submitted with [`CompressedSender::send_video_async`].
    Contiguous(CompressedVideoFrame),
    /// A frame submitted with [`CompressedSender::send_scatter_video_async`].
    #[cfg(ndi_has_advanced_scatter_send)]
    Scatter {
        frame: ScatterVideoFrame,
        table: &'static ScatterSend,
    },
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PendingVideo>();
};

impl PendingVideo {
    /// Lend a queue entry using the original first block as completion key.
    /// Supported runtimes ignore these fields for scatter input and report
    /// them unchanged in completion callbacks, including destruction callbacks.
    pub(super) fn lend_for_queue(&mut self) -> Lent {
        let lent = self.lend(());
        #[cfg(ndi_has_advanced_scatter_send)]
        {
            let mut lent = lent;
            if let Self::Scatter { frame, .. } = self {
                let first = &mut frame.storage.blocks[0];
                let descriptor = lent.descriptor_mut();
                descriptor.p_data = first.as_mut_ptr();
                descriptor.__bindgen_anon_1.data_size_in_bytes = first.len() as i32;
            }
            lent
        }
        #[cfg(not(ndi_has_advanced_scatter_send))]
        lent
    }

    /// The stream the frame is sent on.
    pub(crate) fn tier(&self) -> StreamTier {
        match self {
            Self::Contiguous(frame) => frame.format().tier(),
            #[cfg(ndi_has_advanced_scatter_send)]
            Self::Scatter { frame, .. } => frame.format().tier(),
        }
    }

    pub(super) fn into_submitted(self) -> SubmittedVideo {
        match self {
            Self::Contiguous(frame) => SubmittedVideo::Contiguous(frame),
            #[cfg(ndi_has_advanced_scatter_send)]
            Self::Scatter { frame, .. } => SubmittedVideo::Scatter(frame),
        }
    }
}

// SAFETY: Each variant owns the initialized payload and terminated metadata;
// Scatter additionally owns its stable list's allocation with the frame's
// pointer and size arrays. Moving PendingVideo
// moves none of those allocations. InFlight prevents access until release.
unsafe impl Lend for PendingVideo {
    type Authorization = ();

    fn lend(&mut self, (): ()) -> Lent {
        match self {
            Self::Contiguous(frame) => Lent::Frame(frame.raw()),
            #[cfg(ndi_has_advanced_scatter_send)]
            Self::Scatter { frame, table } => Lent::Scatter {
                table,
                frame: frame.descriptor(),
                scatter: frame.storage.descriptor(),
            },
        }
    }
}

#[derive(Default)]
/// The frame most recently submitted asynchronously on each stream.
///
/// `Processing.NDI.Send.h` makes subsequent video sends synchronizing calls
/// for the sender, across streams. The crate conservatively retains one frame
/// per stream until the replacement on that same stream returns, finishing
/// reclaims it, or the native sender is destroyed. A synchronizing send on
/// another stream can end the native loan earlier without releasing this
/// retained storage to the caller.
pub(crate) struct PendingStreams {
    program: InFlight<PendingVideo>,
    preview: InFlight<PendingVideo>,
}

impl PendingStreams {
    /// Recover only frames whose native loans have already ended.
    pub(crate) fn take_retained(&mut self) -> ReclaimedVideo {
        ReclaimedVideo {
            program: self
                .program
                .take_retained()
                .map(PendingVideo::into_submitted),
            preview: self
                .preview
                .take_retained()
                .map(PendingVideo::into_submitted),
        }
    }

    /// Select the retained slot; only its owning protocol can synchronize it.
    pub(crate) fn slot(&mut self, tier: StreamTier) -> &mut InFlight<PendingVideo> {
        match tier {
            StreamTier::Program => &mut self.program,
            StreamTier::Preview => &mut self.preview,
        }
    }

    /// Recover the previous frame with the proof minted by this owner's send.
    pub(crate) fn reclaim_displaced(
        &mut self,
        _tier: StreamTier,
        displaced: InFlight<PendingVideo>,
        synchronized: Synchronized,
    ) -> Option<SubmittedVideo> {
        displaced.release_with(synchronized, |frame, _proof| frame.into_submitted())
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

impl Loans for PendingStreams {
    type Recovered = ReclaimedVideo;

    fn recover(self, destroyed: &Destroyed) -> ReclaimedVideo {
        self.into_reclaimed(destroyed)
    }

    fn release(self, destroyed: &Destroyed) {
        drop(self.into_reclaimed(destroyed));
    }
}

/// A sender of compressed video and audio.
///
/// Each stream keeps the frame of its last asynchronous send until the next
/// asynchronous send on that stream returns it, or finishing or closing
/// reclaims it. The SDK has one asynchronous send outstanding per sender, so
/// a send on either stream may wait for the SDK to finish with the other
/// stream's frame. The crate still returns each frame through its own stream.
/// Which streams a codec needs, and how they must relate, is defined by the
/// NDI Advanced SDK documentation.
///
/// The native sender is owned exclusively by this value.
/// [`sender`](Self::sender) lends it in the [`CompressedAttached`] mode, which
/// has the operations every sender shares (the source, metadata in both
/// directions, tally, connections, failover, and the Advanced encoder
/// guidance, keyframe and tally controls) but none of the uncompressed sends,
/// and which cannot be registered with a
/// [`SendAdvertiser`](crate::discovery::SendAdvertiser).
///
/// # Threading
///
/// [`split`](Self::split) lends independent video and audio views to scoped
/// threads without an external lock. Each view is `Send` but not `Sync`, so
/// video submissions remain serialized and audio need not wait for them.
///
/// # Sending lends the frame
///
/// The SDK writes to the frames it sends: it stamps the reserved field of
/// their packet envelope. A synchronous send therefore borrows its frame
/// exclusively and an asynchronous one takes it, so no reference to a frame's
/// bytes exists while the SDK may change them, and a frame can be sent again
/// afterwards.
/// [`finish_video_async`](Self::finish_video_async) waits for pending video
/// and returns both streams' frames while keeping the source available.
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
/// sender.send_video(&mut frame);
/// # Ok(())
/// # }
/// ```
pub struct CompressedSender {
    lending: Lending<CompressedAttached, PendingStreams>,
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
        let pending = PendingStreams::default();
        let sender = Sender::new_configured(ndi, options, &config)?;
        Ok(Self {
            lending: Lending::new(sender, pending),
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
    /// registered with an advertiser. Use [`split`](Self::split) to reach
    /// these controls through the audio half while video is being sent.
    pub fn sender(&self) -> &Sender<CompressedAttached> {
        self.lending.sender()
    }

    /// Lend video and audio operations to separate scoped threads.
    ///
    /// Video keeps exclusive access to the pending streams. Each half is
    /// `Send`, but neither `Sync` nor `Clone`, and neither outlives this owner.
    pub fn split(&mut self) -> (CompressedVideo<'_>, CompressedAudio<'_>) {
        let pair = self.lending.parts();
        let sender = pair.sender();
        (
            CompressedVideo {
                pair,
                _not_sync: std::marker::PhantomData,
            },
            CompressedAudio {
                sender,
                _not_sync: std::marker::PhantomData,
            },
        )
    }

    /// Send a video frame and return when the SDK has finished with it.
    ///
    /// The frame is borrowed exclusively because the SDK writes to its bytes.
    pub fn send_video(&mut self, frame: &mut CompressedVideoFrame) {
        self.split().0.send_video(frame)
    }

    /// Submit a video frame without waiting for it to be sent.
    ///
    /// The sender keeps the frame until the next asynchronous send on the same
    /// stream, and returns the frame that this one replaces. The call can
    /// still block while the SDK applies back-pressure.
    pub fn send_video_async(&mut self, frame: CompressedVideoFrame) -> Option<SubmittedVideo> {
        self.split().0.send_video_async(frame)
    }

    /// Drain asynchronous video and return both streams' final frames.
    ///
    /// One null asynchronous flush waits for the SDK to finish with all
    /// pending video, without a timeout. The source stays available and can
    /// send again; the next asynchronous send on either drained stream
    /// returns `None`. Finishing an empty sender makes no native call.
    /// Reclamation does not report whether receivers received the frames.
    pub fn finish_video_async(&mut self) -> ReclaimedVideo {
        self.split().0.finish_video_async()
    }

    /// Send an audio frame and return when the SDK has finished with it.
    ///
    /// The frame is borrowed exclusively because the SDK writes to its bytes.
    pub fn send_audio(&mut self, frame: &mut CompressedAudioFrame) {
        self.split().1.send_audio(frame)
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
        self.split().0.send_scatter_video(_frame)
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
        self.split().0.send_scatter_video_async(frame)
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
        self.split().1.send_scatter_audio(_frame)
    }

    /// Destroy the sender and take back the frames it still held.
    ///
    /// Destroying the native sender waits for the SDK to finish with every
    /// pending frame, without a timeout.
    pub fn close(self) -> ReclaimedVideo {
        self.lending.close()
    }
}

/// Exclusive video operations borrowed from a [`CompressedSender`].
///
/// It is `Send`, but neither `Sync` nor `Clone`. Pending frames stay with the
/// owner when the view is dropped; closing or dropping the owner drains them.
pub struct CompressedVideo<'a> {
    pair: LoanPair<'a, CompressedAttached, &'a mut PendingStreams>,
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}

/// Audio operations and shared controls borrowed from a [`CompressedSender`].
///
/// It is `Send`, but neither `Sync` nor `Clone`, and may run on a scoped thread
/// separately from the video half.
pub struct CompressedAudio<'a> {
    sender: &'a Sender<CompressedAttached>,
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}

impl CompressedVideo<'_> {
    fn instance(&self) -> NDIlib_send_instance_t {
        self.pair.sender().native_instance()
    }

    /// See [`CompressedSender::send_video`].
    pub fn send_video(&mut self, frame: &mut CompressedVideoFrame) {
        let descriptor = frame.raw();
        // SAFETY: the sender is live, and the descriptor lends `frame`, which
        // is borrowed exclusively for longer than this synchronous call.
        unsafe {
            self.pair
                .sender()
                .core()
                .NDIlib_send_send_video_v2(self.instance(), &descriptor)
        };
    }

    /// See [`CompressedSender::send_video_async`].
    pub fn send_video_async(&mut self, frame: CompressedVideoFrame) -> Option<SubmittedVideo> {
        self.pair.submit_pending(frame)
    }

    /// See [`CompressedSender::finish_video_async`].
    pub fn finish_video_async(&mut self) -> ReclaimedVideo {
        self.pair.finish()
    }

    /// See [`CompressedSender::send_scatter_video`].
    ///
    /// # Errors
    ///
    /// As for [`CompressedSender::send_scatter_video`].
    pub fn send_scatter_video(&mut self, _frame: &mut ScatterVideoFrame) -> Result<()> {
        let table = self.pair.sender().runtime().table::<ScatterSend>()?;
        native!(ScatterSend, table => {
            let descriptor = _frame.descriptor();
            let scatter = _frame.storage.descriptor();
            // SAFETY: the sender is live, and both descriptors lend `frame`, which
            // is borrowed exclusively for longer than this synchronous call.
            unsafe {
                table.NDIlib_send_send_video_scatter(self.instance(), &descriptor, scatter)
            };
            Ok(())
        })
    }

    /// See [`CompressedSender::send_scatter_video_async`].
    ///
    /// # Errors
    ///
    /// As for [`CompressedSender::send_scatter_video_async`].
    pub fn send_scatter_video_async(
        &mut self,
        frame: ScatterVideoFrame,
    ) -> std::result::Result<Option<SubmittedVideo>, Rejected<ScatterVideoFrame>> {
        self.pair.submit_scatter(frame)
    }
}

impl CompressedAudio<'_> {
    /// The shared metadata, source, tally, connection and encoder controls.
    pub fn sender(&self) -> &Sender<CompressedAttached> {
        self.sender
    }

    /// See [`CompressedSender::send_audio`].
    pub fn send_audio(&mut self, frame: &mut CompressedAudioFrame) {
        self.sender.send_compressed_audio(frame);
    }

    /// See [`CompressedSender::send_scatter_audio`].
    ///
    /// # Errors
    ///
    /// As for [`CompressedSender::send_scatter_audio`].
    pub fn send_scatter_audio(&mut self, frame: &mut ScatterAudioFrame) -> Result<()> {
        self.sender.send_scatter_audio(frame)
    }
}

impl<M: CompressedAudioSendMode> Sender<M> {
    /// Send a compressed audio frame synchronously, alongside the owner's
    /// video. The frame is borrowed exclusively because the SDK writes to its
    /// envelope. The compressed or queued mode proves runtime availability;
    /// the frame's type proves send admission.
    pub fn send_compressed_audio(&self, frame: &mut CompressedAudioFrame) {
        let descriptor = frame.raw();
        // SAFETY: this live Advanced sender lends the exclusively borrowed
        // frame for the whole synchronous call, after which the SDK releases it.
        unsafe {
            self.core()
                .NDIlib_send_send_audio_v3(self.native_instance(), &descriptor)
        };
    }

    /// Send compressed audio held in separate blocks synchronously.
    /// The frame remains exclusively borrowed through native release.
    ///
    /// # Errors
    ///
    /// [`crate::Error::UnsupportedCapability`] when scatter sending is
    /// unavailable in the selected runtime or build headers.
    pub fn send_scatter_audio(&self, _frame: &mut ScatterAudioFrame) -> Result<()> {
        let table = self.runtime().table::<ScatterSend>()?;
        native!(ScatterSend, table => {
            let descriptor = _frame.descriptor();
            let scatter = _frame.storage.descriptor();
            // SAFETY: both descriptors point only into this exclusively
            // borrowed frame, retained until the synchronous call returns.
            unsafe {
                table.NDIlib_send_send_audio_scatter(self.native_instance(), &descriptor, scatter)
            };
            Ok(())
        })
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

    #[test]
    fn split_audio_adds_no_allocation_to_a_send() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let mut sender = CompressedSender::new(
            &ndi,
            &SenderOptions::new(crate::test_support::unique_name("split-audio-allocation"))
                .clock_audio(false),
        )
        .unwrap();
        let mut frame =
            CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![1; 64]).unwrap();
        let ((_video, mut audio), allocations) =
            crate::test_alloc::allocation_count(|| sender.split());
        assert_eq!(allocations, 0);
        let ((), allocations) =
            crate::test_alloc::allocation_count(|| audio.send_audio(&mut frame));
        assert_eq!(allocations, 0);
    }

    #[test]
    fn queued_compressed_audio_send_adds_no_allocation() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let queue = super::super::QueuedSender::new(
            &ndi,
            &SenderOptions::new(crate::test_support::unique_name("queued-compressed-audio"))
                .clock_audio(false),
            super::super::QueueOptions::default(),
        )
        .unwrap();
        let mut frame =
            CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![1; 64]).unwrap();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            queue.sender().send_compressed_audio(&mut frame)
        });
        assert_eq!(allocations, 0);
    }

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
        assert_eq!(frame.storage.pointers.array.len(), 4);
        assert!(frame.storage.pointers.array[3].is_null());
        assert_eq!(&*frame.storage.sizes, &[44, 3, 2, 0]);
        let moved = Box::new(frame);
        for (block, pointer) in moved
            .storage
            .blocks
            .iter()
            .zip(moved.storage.pointers.array.iter())
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
        assert!(matches!(
            reason(refused),
            crate::FrameDefect::SpeedHqQuality
        ));
        let refused = ScatterVideoFrame::new(speedhq, vec![vec![0, 0, 0], vec![1; 8]]);
        assert!(matches!(reason(refused), crate::FrameDefect::ScatterHeader));

        let (format, bytes) = h264();
        let preview = |width, height| {
            let format = VideoPacketFormat::new(VideoCodec::H264, width, height)
                .unwrap()
                .with_tier(StreamTier::Preview);
            ScatterVideoFrame::new(format, split(&bytes, &[44]))
        };
        preview(640, 640).unwrap();
        assert!(matches!(
            reason(preview(641, 360)),
            crate::FrameDefect::PreviewVideoDimensions
        ));
        assert!(matches!(
            reason(preview(640, 641)),
            crate::FrameDefect::PreviewVideoDimensions
        ));
        assert!(matches!(
            reason(ScatterVideoFrame::new(format, split(&bytes, &[43]))),
            crate::FrameDefect::ScatterHeader
        ));

        // A keyframe needs its parameter sets here too.
        let bare =
            PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, true, &[1, 2, 3], &[]).unwrap();
        let refused = ScatterVideoFrame::new(format, split(&bare, &[44]));
        assert!(matches!(
            reason(refused),
            crate::FrameDefect::KeyframeConfigurationMissing
        ));
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
        let blocks = copy
            .storage
            .blocks
            .iter()
            .zip(copy.storage.pointers.array.iter());
        for ((block, pointer), original) in blocks.zip(frame.storage.blocks.iter()) {
            assert_eq!(block.as_ptr().cast::<u8>(), *pointer);
            assert_ne!(block.as_ptr(), original.as_ptr());
            assert_eq!(**block, **original);
        }
        assert_eq!(&*copy.storage.sizes, &*frame.storage.sizes);
        assert!(copy.storage.pointers.array[3].is_null());
        #[cfg(ndi_has_advanced_scatter_send)]
        {
            assert_ne!(
                copy.storage.pointers.list.as_ptr(),
                frame.storage.pointers.list.as_ptr()
            );
            let list = &*copy.storage.pointers.list;
            assert_eq!(
                list.p_data_blocks.cast::<*const u8>(),
                copy.storage.pointers.array.as_ptr().cast::<*const u8>()
            );
            assert_eq!(
                list.p_data_blocks_size.cast::<i32>(),
                copy.storage.sizes.as_ptr().cast::<i32>()
            );
        }

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
        let first = frame.storage.pointers.array[0].cast_mut();
        let moved = Box::new(frame);
        // SAFETY: the pointer was created to be written through, the block
        // holds a complete header, and no reference to it exists.
        unsafe { first.add(24).cast::<u64>().write_unaligned(7) };
        let stamped = moved.blocks().next().unwrap().to_vec();
        assert_eq!(stamped[24..32], 7u64.to_ne_bytes());
        assert_eq!(stamped[..24], bytes[..24]);

        let joined = moved.into_contiguous_copy();
        super::super::ReceivedVideoPacket::from(joined.clone())
            .validate_for_send()
            .unwrap();
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
            super::super::ReceivedAudioPacket::from(aac.clone())
                .validate_for_send()
                .unwrap();
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
        assert!(matches!(
            aac(48_000, 2, 960, &packet),
            crate::FrameDefect::AacSampleCount
        ));
        assert!(matches!(
            aac(44_100, 2, 1024, &packet),
            crate::FrameDefect::AacRateMismatch
        ));
        assert!(matches!(
            aac(48_000, 1, 1024, &packet),
            crate::FrameDefect::AacChannelsMismatch
        ));

        let mut unflagged = packet.clone();
        unflagged[32..36].copy_from_slice(&0u32.to_ne_bytes());
        assert!(matches!(
            aac(48_000, 2, 1024, &unflagged),
            crate::FrameDefect::AacKeyframeMissing
        ));

        let config = aac_config(48_000, 2);
        for config in [&[][..], &config[..1]] {
            let bare =
                PacketEnvelope::encode(EnvelopeCodec::Aac, 0, 0, true, &[1, 2], config).unwrap();
            let result = scatter_audio(AudioCodec::Aac, 48_000, 2, 1024, split(&bare, &[44]));
            assert!(matches!(
                reason(result),
                crate::FrameDefect::AacConfigurationMissing
            ));
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
        super::super::ReceivedAudioPacket::from(frame.clone())
            .validate_for_send()
            .unwrap();
        assert!(matches!(
            reason(opus(44_100, 882)),
            crate::FrameDefect::OpusSampleRate
        ));
        assert!(matches!(
            reason(opus(48_000, 1440)),
            crate::FrameDefect::OpusDuration
        ));
        assert!(matches!(
            reason(opus(0, 960)),
            crate::FrameDefect::CompressedAudioQuantities { .. }
        ));
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
        let (lent, allocations) = crate::test_alloc::allocation_count(|| pending.lend(()));
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
        let pointers = frame.storage.pointers.array.as_ptr().cast::<*const u8>();
        let sizes = frame.storage.sizes.as_ptr().cast::<i32>();
        let (mut pending, allocations) =
            crate::test_alloc::allocation_count(|| PendingVideo::Scatter { frame, table });
        assert_eq!(allocations, 0, "the list already belongs to the frame");
        let Lent::Scatter { scatter, .. } = pending.lend(()) else {
            panic!("a scatter frame is lent with its scatter list");
        };
        // Moving the entry, as its stream's slot does, leaves the list where
        // the SDK was told it is, still naming the frame's arrays.
        let mut moved = Box::new(pending);
        let Lent::Scatter { scatter: after, .. } = moved.lend(()) else {
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

    #[cfg(ndi_has_advanced_scatter_send)]
    #[test]
    fn compressed_scatter_streams_lend_frame_owned_lists_without_allocating() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        let (format, bytes) = h264();
        let make =
            |tier| ScatterVideoFrame::new(format.with_tier(tier), split(&bytes, &[44])).unwrap();
        let mut sender = CompressedSender::new(
            &ndi,
            &SenderOptions::new(crate::test_support::unique_name("scatter-double-buffer"))
                .clock_video(false),
        )
        .unwrap();
        let list = |stream: &InFlight<PendingVideo>| match stream.frame().unwrap() {
            PendingVideo::Scatter { frame, .. } => frame.storage.pointers.list.as_ptr(),
            _ => panic!("scatter stream"),
        };
        let mut first = make(StreamTier::Program);
        let original = first.blocks().next().unwrap().as_ptr();
        let original_list = first.storage.descriptor();
        sender.send_scatter_video(&mut first).unwrap();
        assert_eq!(first.storage.descriptor(), original_list);
        sender.send_scatter_video_async(first).unwrap();
        let program = list(&sender.lending.loans().program);
        assert_eq!(program, original_list);
        sender
            .send_scatter_video_async(make(StreamTier::Preview))
            .unwrap();
        let preview = list(&sender.lending.loans().preview);
        assert_ne!(program, preview);
        let replacement = make(StreamTier::Program);
        let (returned, allocations) =
            crate::test_alloc::allocation_count(|| sender.send_scatter_video_async(replacement));
        assert_eq!(allocations, 0);
        let SubmittedVideo::Scatter(returned_program) = returned.unwrap().unwrap() else {
            panic!("scatter")
        };
        assert_eq!(returned_program.blocks().next().unwrap().as_ptr(), original);
        let next_program = list(&sender.lending.loans().program);
        assert_ne!(next_program, program);
        assert_ne!(next_program, preview);
        assert_eq!(list(&sender.lending.loans().preview), preview);
        let replacement = make(StreamTier::Preview);
        let (returned, allocations) =
            crate::test_alloc::allocation_count(|| sender.send_scatter_video_async(replacement));
        let SubmittedVideo::Scatter(returned_preview) = returned.unwrap().unwrap() else {
            panic!("scatter")
        };
        assert_eq!(allocations, 0);
        let next_preview = list(&sender.lending.loans().preview);
        assert_ne!(next_preview, preview);
        assert_ne!(next_preview, next_program);
        sender.send_scatter_video_async(returned_program).unwrap();
        assert_eq!(list(&sender.lending.loans().program), program);
        assert_eq!(list(&sender.lending.loans().preview), next_preview);
        // The recovered frame keeps its original list even across a
        // contiguous replacement and a later asynchronous submission.
        sender
            .send_video_async(
                CompressedVideoFrame::new(format.with_tier(StreamTier::Preview), bytes.clone())
                    .unwrap(),
            )
            .unwrap();
        sender.send_scatter_video_async(returned_preview).unwrap();
        assert_eq!(list(&sender.lending.loans().preview), preview);
        let reclaimed = sender.close();
        assert!(reclaimed.program.is_some());
        assert!(reclaimed.preview.is_some());
    }

    #[cfg(ndi_has_advanced_scatter_send)]
    #[test]
    fn scatter_construction_allocates_arrays_and_one_frame_owned_list() {
        let (format, bytes) = h264();
        // Exclude shrinking the caller's spare outer-vector capacity from the
        // frame's two arrays and one stable-list allocation.
        let blocks = split(&bytes, &[44]).into_boxed_slice().into_vec();
        let (frame, allocations) =
            crate::test_alloc::allocation_count(|| ScatterVideoFrame::new(format, blocks).unwrap());
        assert_eq!(allocations, 3, "pointer array, size array and stable list");
        assert_eq!(frame.total_bytes(), bytes.len());
    }

    #[test]
    fn finishing_reclaims_both_one_or_no_stream_without_allocating() {
        let Some(ndi) = crate::test_support::advanced_ndi() else {
            return;
        };
        for scatter in [false, true]
            .into_iter()
            .filter(|scatter| !scatter || cfg!(ndi_has_advanced_scatter_send))
        {
            for active in 0..4 {
                let mut sender = CompressedSender::new(
                    &ndi,
                    &SenderOptions::new(crate::test_support::unique_name("finish-streams"))
                        .clock_video(false),
                )
                .unwrap();
                let mut expected = [None; 2];
                for (index, tier) in [StreamTier::Program, StreamTier::Preview]
                    .into_iter()
                    .enumerate()
                {
                    if active & (1 << index) == 0 {
                        continue;
                    }
                    let format = VideoPacketFormat::new(VideoCodec::SpeedHq422, 16, 16)
                        .unwrap()
                        .with_tier(tier);
                    expected[index] = Some(if scatter {
                        let frame =
                            ScatterVideoFrame::new(format, vec![vec![1; 16], vec![2; 8]]).unwrap();
                        let pointer = frame.blocks().next().unwrap().as_ptr();
                        assert!(sender.send_scatter_video_async(frame).unwrap().is_none());
                        pointer
                    } else {
                        let frame = CompressedVideoFrame::new(format, vec![1; 16]).unwrap();
                        let pointer = frame.data().as_ptr();
                        assert!(sender.send_video_async(frame).is_none());
                        pointer
                    });
                }
                let (reclaimed, allocations) =
                    crate::test_alloc::allocation_count(|| sender.finish_video_async());
                assert_eq!(allocations, 0);
                for (returned, expected) in [reclaimed.program, reclaimed.preview]
                    .into_iter()
                    .zip(expected)
                {
                    let pointer = returned.map(|frame| match frame {
                        SubmittedVideo::Contiguous(frame) => {
                            assert!(!scatter);
                            frame.data().as_ptr()
                        }
                        SubmittedVideo::Scatter(frame) => {
                            assert!(scatter);
                            frame.blocks().next().unwrap().as_ptr()
                        }
                    });
                    assert_eq!(pointer, expected);
                }
                let empty = sender.split().0.finish_video_async();
                assert!(empty.program.is_none() && empty.preview.is_none());
                let closed = sender.close();
                assert!(closed.program.is_none() && closed.preview.is_none());
            }
        }
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
        assert!(sender.send_video_async(program).is_none());
        // A preview submission leaves the program frame in flight.
        let preview = sender.send_video_async(make(StreamTier::Preview));
        assert!(preview.is_none());
        assert!(sender.lending.loans().program.is_in_flight());
        let returned = sender.send_video_async(make(StreamTier::Program)).unwrap();
        let SubmittedVideo::Contiguous(returned) = returned else {
            panic!("a contiguous frame was submitted");
        };
        assert_eq!(returned.format().tier(), StreamTier::Program);
        assert_eq!(returned.data().as_ptr(), program_data);
        // The SDK may have stamped the reserved field, and nothing else.
        assert_eq!(returned.data()[..24], bytes[..24]);
        assert_eq!(returned.data()[32..], bytes[32..]);
        super::super::ReceivedVideoPacket::from(returned.clone())
            .validate_for_send()
            .unwrap();
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
