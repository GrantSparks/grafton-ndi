//! Owned compressed sending, including scatter storage and independent stream lifetimes.

use super::{
    ffi, AdvancedConfig, AudioCodec, CompressedAudioFrame, CompressedVideoFrame, QualityPlan,
    StreamTier, VideoPacketFormat, MAX_PACKET_BYTES,
};
use crate::{advanced::ffi::native::*, Result, Sender, SenderOptions, Tally, NDI};
use std::{ffi::CString, ptr, time::Duration};

const ENVELOPE_BYTES: usize = 44;
/// Maximum number of independently owned blocks in a scatter frame.
pub const MAX_SCATTER_BLOCKS: usize = 65_536;

#[derive(Debug)]
struct ScatterStorage {
    blocks: Box<[Box<[u8]>]>,
    #[cfg_attr(not(ndi_has_advanced_header), allow(dead_code))]
    pointers: Box<[*const u8]>,
    #[cfg_attr(not(ndi_has_advanced_header), allow(dead_code))]
    sizes: Box<[i32]>,
    total: usize,
}
// SAFETY: every pointer refers to immutable storage owned by this value. Moving
// the boxes never moves their allocations, and no native pointer is exposed.
unsafe impl Send for ScatterStorage {}
unsafe impl Sync for ScatterStorage {}
impl ScatterStorage {
    fn new(blocks: Vec<Vec<u8>>) -> Result<Self> {
        if blocks.is_empty() || blocks.len() > MAX_SCATTER_BLOCKS {
            return Err(ffi::invalid("Invalid scatter block count"));
        }
        let mut total = 0usize;
        for block in &blocks {
            if block.is_empty() || block.len() > i32::MAX as usize {
                return Err(ffi::invalid(
                    "Scatter blocks must have a positive native size",
                ));
            }
            total = total
                .checked_add(block.len())
                .ok_or_else(|| ffi::invalid("Scatter size overflow"))?;
            if total > MAX_PACKET_BYTES || total > i32::MAX as usize {
                return Err(ffi::invalid("Scatter frame exceeds the packet size limit"));
            }
        }
        let blocks: Box<[_]> = blocks.into_iter().map(Vec::into_boxed_slice).collect();
        let pointers = blocks
            .iter()
            .map(|b| b.as_ptr())
            .chain(std::iter::once(ptr::null()))
            .collect();
        let sizes = blocks
            .iter()
            .map(|b| b.len() as i32)
            .chain(std::iter::once(0))
            .collect();
        Ok(Self {
            blocks,
            pointers,
            sizes,
            total,
        })
    }
    fn validate_envelope(&self, codec: u32, always_extra: bool) -> Result<()> {
        let first = &self.blocks[0];
        if first.len() < ENVELOPE_BYTES {
            return Err(ffi::invalid(
                "The complete compressed packet header must fit in the first scatter block",
            ));
        }
        let header = i32::from_ne_bytes(first[0..4].try_into().unwrap());
        let actual_codec = u32::from_ne_bytes(first[4..8].try_into().unwrap());
        let flags = u32::from_ne_bytes(first[32..36].try_into().unwrap());
        let reserved = u64::from_ne_bytes(first[24..32].try_into().unwrap());
        let data = u32::from_ne_bytes(first[36..40].try_into().unwrap()) as usize;
        let extra = u32::from_ne_bytes(first[40..44].try_into().unwrap()) as usize;
        if header != ENVELOPE_BYTES as i32
            || actual_codec != codec
            || flags & !1 != 0
            || reserved != 0
        {
            return Err(ffi::invalid(
                "Scatter packet has an unsupported header or mismatched codec",
            ));
        }
        if data == 0
            || ENVELOPE_BYTES
                .checked_add(data)
                .and_then(|n| n.checked_add(extra))
                != Some(self.total)
        {
            return Err(ffi::invalid(
                "Scatter blocks do not match the complete packet length",
            ));
        }
        if (always_extra || flags & 1 != 0) && extra == 0 {
            return Err(ffi::invalid(
                "Scatter packet requires codec configuration data",
            ));
        }
        Ok(())
    }
    #[cfg(ndi_has_advanced_header)]
    fn raw(&self) -> ffi::Scatter {
        ffi::Scatter {
            p_data_blocks: self.pointers.as_ptr(),
            p_data_blocks_size: self.sizes.as_ptr(),
        }
    }
    fn copy_contiguous(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.total);
        for block in &self.blocks {
            bytes.extend_from_slice(block);
        }
        bytes
    }
}

fn metadata(value: impl Into<String>) -> Result<CString> {
    let value = value.into();
    if value.len() >= crate::frames::MAX_METADATA_BYTES {
        return Err(ffi::invalid("Packet metadata exceeds its size limit"));
    }
    Ok(CString::new(value)?)
}

/// Immutable owned compressed-video blocks. H.264/HEVC envelopes must have the
/// complete 44-byte header in the first block. The payload is not coalesced.
#[derive(Debug)]
pub struct ScatterVideoFrame {
    format: VideoPacketFormat,
    storage: ScatterStorage,
    timecode: i64,
    metadata: Option<CString>,
}
impl ScatterVideoFrame {
    pub fn new(format: VideoPacketFormat, blocks: Vec<Vec<u8>>) -> Result<Self> {
        let storage = ScatterStorage::new(blocks)?;
        if let Some(codec) = format.codec().envelope_codec() {
            storage.validate_envelope(codec, false)?;
        } else if storage.blocks[0].len() < 4 {
            return Err(ffi::invalid(
                "The first SpeedHQ scatter block must contain its header",
            ));
        }
        Ok(Self {
            format,
            storage,
            timecode: NDIlib_send_timecode_synthesize,
            metadata: None,
        })
    }
    pub fn format(&self) -> VideoPacketFormat {
        self.format
    }
    pub fn blocks(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.storage.blocks.iter().map(AsRef::as_ref)
    }
    pub fn total_bytes(&self) -> usize {
        self.storage.total
    }
    pub fn with_timecode(mut self, timecode: i64) -> Self {
        self.timecode = timecode;
        self
    }
    pub fn with_metadata(mut self, value: impl Into<String>) -> Result<Self> {
        self.metadata = Some(metadata(value)?);
        Ok(self)
    }
    /// Explicitly allocate and copy all blocks into a contiguous owned packet.
    pub fn into_contiguous_copy(self) -> CompressedVideoFrame {
        CompressedVideoFrame {
            format: self.format,
            bytes: self.storage.copy_contiguous().into_boxed_slice(),
            timecode: self.timecode,
            metadata: self.metadata,
        }
    }
    fn raw(&self) -> NDIlib_video_frame_v2_t {
        let mut raw = self.format.raw();
        raw.timecode = self.timecode;
        raw.p_metadata = self.metadata.as_ref().map_or(ptr::null(), |m| m.as_ptr());
        // Scatter operations obtain all payload pointers and sizes from the
        // generated scatter descriptor, never from these ignored frame fields.
        raw.p_data = ptr::null_mut();
        raw.__bindgen_anon_1.data_size_in_bytes = 0;
        raw
    }
}

/// Immutable owned AAC or Opus blocks. AAC requires its complete packet header
/// in the first block; Opus is supplied without an NDI envelope.
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
    pub fn new(
        codec: AudioCodec,
        sample_rate: i32,
        channels: i32,
        samples: i32,
        blocks: Vec<Vec<u8>>,
    ) -> Result<Self> {
        if sample_rate <= 0 || channels <= 0 || samples <= 0 {
            return Err(ffi::invalid("Compressed audio dimensions must be positive"));
        }
        let storage = ScatterStorage::new(blocks)?;
        if codec == AudioCodec::Aac {
            storage.validate_envelope(0xff, true)?;
        }
        Ok(Self {
            codec,
            storage,
            sample_rate,
            channels,
            samples,
            protected: false,
            timecode: NDIlib_send_timecode_synthesize,
            metadata: None,
        })
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
    pub fn blocks(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.storage.blocks.iter().map(AsRef::as_ref)
    }
    pub fn total_bytes(&self) -> usize {
        self.storage.total
    }
    pub fn with_timecode(mut self, timecode: i64) -> Self {
        self.timecode = timecode;
        self
    }
    pub fn with_metadata(mut self, value: impl Into<String>) -> Result<Self> {
        self.metadata = Some(metadata(value)?);
        Ok(self)
    }
    pub fn protected(mut self, protected: bool) -> Self {
        self.protected = protected;
        self
    }
    /// Explicitly allocate and copy all blocks into a contiguous owned packet.
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
    #[cfg(ndi_has_advanced_header)]
    fn raw(&self) -> NDIlib_audio_frame_v3_t {
        let codec = match self.codec {
            AudioCodec::Aac => 0xff,
            AudioCodec::Opus => u32::from_le_bytes(*b"Opus"),
        };
        NDIlib_audio_frame_v3_t {
            sample_rate: self.sample_rate,
            no_channels: self.channels,
            no_samples: self.samples,
            timecode: self.timecode,
            FourCC: ffi::audio_fourcc_native(codec | if self.protected { 0x8080_8080 } else { 0 }),
            p_metadata: self.metadata.as_ref().map_or(ptr::null(), |m| m.as_ptr()),
            ..Default::default()
        }
    }
}

/// Storage returned after the native sender releases an earlier asynchronous
/// submission on the same program/preview stream, or after sender destruction.
#[derive(Debug)]
#[non_exhaustive]
pub enum SubmittedVideo {
    Contiguous(CompressedVideoFrame),
    Scatter(ScatterVideoFrame),
}
impl SubmittedVideo {
    pub fn format(&self) -> VideoPacketFormat {
        match self {
            Self::Contiguous(f) => f.format(),
            Self::Scatter(f) => f.format(),
        }
    }
}

struct PendingVideo {
    frame: SubmittedVideo,
    raw: Box<NDIlib_video_frame_v2_t>,
    #[cfg(ndi_has_advanced_header)]
    scatter: Option<Box<ffi::Scatter>>,
}
// SAFETY: raw descriptors and every referenced byte/metadata/array allocation
// belong to this immutable pending entry. Moving its boxes across threads does
// not move those allocations. Only the exclusively borrowed sender can release
// an entry, after the native synchronization/destruction barrier has returned.
unsafe impl Send for PendingVideo {}
impl PendingVideo {
    fn new(frame: SubmittedVideo) -> Self {
        let raw = Box::new(match &frame {
            SubmittedVideo::Contiguous(f) => f.raw(),
            SubmittedVideo::Scatter(f) => f.raw(),
        });
        #[cfg(ndi_has_advanced_header)]
        let scatter = match &frame {
            SubmittedVideo::Contiguous(_) => None,
            SubmittedVideo::Scatter(f) => Some(Box::new(f.storage.raw())),
        };
        Self {
            frame,
            raw,
            #[cfg(ndi_has_advanced_header)]
            scatter,
        }
    }
}
#[derive(Default)]
struct PendingStreams {
    program: Option<PendingVideo>,
    preview: Option<PendingVideo>,
}
impl PendingStreams {
    fn submit(
        &mut self,
        next: PendingVideo,
        submit: impl FnOnce(&PendingVideo),
    ) -> Option<SubmittedVideo> {
        // Both previous stream allocations remain alive throughout the native
        // call. Only the matching stream can be released after that call returns.
        submit(&next);
        let slot = match next.frame.format().stream_tier() {
            StreamTier::Program => &mut self.program,
            StreamTier::Preview => &mut self.preview,
        };
        slot.replace(next).map(|old| old.frame)
    }
    fn take(&mut self) -> [Option<SubmittedVideo>; 2] {
        [
            self.program.take().map(|p| p.frame),
            self.preview.take().map(|p| p.frame),
        ]
    }
}

type SendVideo = unsafe extern "C" fn(NDIlib_send_instance_t, *const NDIlib_video_frame_v2_t);
type SendAudio = unsafe extern "C" fn(NDIlib_send_instance_t, *const NDIlib_audio_frame_v3_t);
#[cfg(ndi_has_advanced_header)]
type ScatterVideo = unsafe extern "C" fn(
    NDIlib_send_instance_t,
    *const NDIlib_video_frame_v2_t,
    *const ffi::Scatter,
);
#[cfg(ndi_has_advanced_header)]
type ScatterAudio = unsafe extern "C" fn(
    NDIlib_send_instance_t,
    *const NDIlib_audio_frame_v3_t,
    *const ffi::Scatter,
);

#[cfg(ndi_has_advanced_header)]
mod abi_signatures {
    use super::*;
    use crate::sys::{advanced_declarations as advanced, declarations as common};
    // Anonymous unused constants check the exact generated signatures while
    // preserving optional runtime resolution (no emitted linker references).
    const _: SendVideo = common::NDIlib_send_send_video_v2;
    const _: SendVideo = common::NDIlib_send_send_video_async_v2;
    const _: SendAudio = common::NDIlib_send_send_audio_v3;
    const _: ScatterVideo = advanced::NDIlib_send_send_video_scatter;
    const _: ScatterVideo = advanced::NDIlib_send_send_video_scatter_async;
    const _: ScatterAudio = advanced::NDIlib_send_send_audio_scatter;
}

/// Exclusive sender for compressed packets. Program and preview retain separate
/// asynchronous buffers. Supply both streams according to the codec's SDK rules.
/// This type cannot share its sender with callbacks, advertisers or other senders.
pub struct CompressedSender {
    sender: Option<Sender>,
    pending: PendingStreams,
    video: SendVideo,
    video_async: SendVideo,
    audio: SendAudio,
}
impl CompressedSender {
    pub fn new(ndi: &NDI, options: &SenderOptions) -> Result<Self> {
        Self::new_with_config(ndi, options, &AdvancedConfig::parse("{}")?)
    }
    pub fn new_with_config(
        ndi: &NDI,
        options: &SenderOptions,
        config: &AdvancedConfig,
    ) -> Result<Self> {
        let video = unsafe { ffi::required(ndi, b"NDIlib_send_send_video_v2\0")? };
        let video_async = unsafe { ffi::required(ndi, b"NDIlib_send_send_video_async_v2\0")? };
        let audio = unsafe { ffi::required(ndi, b"NDIlib_send_send_audio_v3\0")? };
        let sender = Sender::new_with_config(ndi, options, config)?;
        Ok(Self {
            sender: Some(sender),
            pending: PendingStreams::default(),
            video,
            video_async,
            audio,
        })
    }
    fn sender(&self) -> &Sender {
        self.sender.as_ref().expect("live compressed sender")
    }
    /// Synchronous sending borrows storage only for this call. Previously queued
    /// async storage stays retained until a matching async send or close.
    pub fn send_video(&mut self, frame: &CompressedVideoFrame) -> Result<()> {
        frame.validate_for_send()?;
        unsafe { (self.video)(self.sender().native_instance(), &frame.raw()) };
        Ok(())
    }
    /// Submit owned video, returning only the preceding async frame on this tier.
    /// Native backpressure may block. No deadline or cancellation is implied.
    pub fn send_video_async(
        &mut self,
        frame: CompressedVideoFrame,
    ) -> Result<Option<SubmittedVideo>> {
        frame.validate_for_send()?;
        let instance = self.sender().native_instance();
        let send = self.video_async;
        Ok(self.pending.submit(
            PendingVideo::new(SubmittedVideo::Contiguous(frame)),
            |next| unsafe { send(instance, next.raw.as_ref()) },
        ))
    }
    pub fn send_audio(&mut self, frame: &CompressedAudioFrame) -> Result<()> {
        frame.validate_for_send()?;
        unsafe { (self.audio)(self.sender().native_instance(), &frame.raw()) };
        Ok(())
    }
    pub fn send_scatter_video(&mut self, frame: &ScatterVideoFrame) -> Result<()> {
        #[cfg(ndi_has_advanced_header)]
        {
            let send: ScatterVideo = unsafe {
                ffi::required(self.sender().runtime(), b"NDIlib_send_send_video_scatter\0")?
            };
            unsafe {
                send(
                    self.sender().native_instance(),
                    &frame.raw(),
                    &frame.storage.raw(),
                )
            };
            Ok(())
        }
        #[cfg(not(ndi_has_advanced_header))]
        {
            let _ = frame;
            Err(ffi::build_unavailable())
        }
    }
    pub fn send_scatter_video_async(
        &mut self,
        frame: ScatterVideoFrame,
    ) -> Result<Option<SubmittedVideo>> {
        #[cfg(ndi_has_advanced_header)]
        {
            let send: ScatterVideo = unsafe {
                ffi::required(
                    self.sender().runtime(),
                    b"NDIlib_send_send_video_scatter_async\0",
                )?
            };
            let instance = self.sender().native_instance();
            Ok(self.pending.submit(
                PendingVideo::new(SubmittedVideo::Scatter(frame)),
                |next| unsafe {
                    send(
                        instance,
                        next.raw.as_ref(),
                        next.scatter.as_deref().expect("scatter submission"),
                    )
                },
            ))
        }
        #[cfg(not(ndi_has_advanced_header))]
        {
            let _ = frame;
            Err(ffi::build_unavailable())
        }
    }
    pub fn send_scatter_audio(&mut self, frame: &ScatterAudioFrame) -> Result<()> {
        #[cfg(ndi_has_advanced_header)]
        {
            let send: ScatterAudio = unsafe {
                ffi::required(self.sender().runtime(), b"NDIlib_send_send_audio_scatter\0")?
            };
            unsafe {
                send(
                    self.sender().native_instance(),
                    &frame.raw(),
                    &frame.storage.raw(),
                )
            };
            Ok(())
        }
        #[cfg(not(ndi_has_advanced_header))]
        {
            let _ = frame;
            Err(ffi::build_unavailable())
        }
    }
    pub fn set_external_tally(&self, tally: Tally) -> Result<bool> {
        self.sender().set_external_tally(tally)
    }
    pub fn target_frame_bytes(&self, format: VideoPacketFormat) -> Result<u32> {
        self.sender().target_frame_bytes(format)
    }
    pub fn target_bit_rate(&self, format: VideoPacketFormat) -> Result<u32> {
        self.sender().target_bit_rate(format)
    }
    pub fn legacy_quality(&self, format: VideoPacketFormat) -> Result<u32> {
        self.sender().legacy_quality(format)
    }
    pub fn quality_plan(
        &self,
        format: VideoPacketFormat,
        supports_new_format: bool,
    ) -> Result<QualityPlan> {
        self.sender().quality_plan(format, supports_new_format)
    }
    pub fn keyframe_required(&self, format: Option<VideoPacketFormat>) -> Result<bool> {
        self.sender().keyframe_required(format)
    }
    pub fn wait_for_keyframe_change(
        &self,
        timeout: Duration,
        format: Option<VideoPacketFormat>,
    ) -> Result<bool> {
        self.sender().wait_for_keyframe_change(timeout, format)
    }
    /// Destroy the native instance, then recover pending [program, preview]
    /// packets. Destruction is the drain barrier and may block without a timeout.
    pub fn close(mut self) -> [Option<SubmittedVideo>; 2] {
        drop(self.sender.take());
        self.pending.take()
    }
}
impl Drop for CompressedSender {
    fn drop(&mut self) {
        // Fresh private ownership means this destroys the actual native sender.
        // Do not use a null-frame flush: it cannot prove both streams are drained.
        drop(self.sender.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advanced::{packet_envelope, VideoCodec};
    fn h264() -> (VideoPacketFormat, Vec<u8>) {
        (
            VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap(),
            packet_envelope(
                u32::from_le_bytes(*b"H264"),
                10,
                9,
                true,
                &[1, 2, 3],
                &[4, 5],
            )
            .unwrap(),
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
    fn aac_requires_extra_data_but_opus_has_no_packet_prefix() {
        let bytes = packet_envelope(0xff, 0, 0, false, &[1, 2], &[]).unwrap();
        assert!(ScatterAudioFrame::new(AudioCodec::Aac, 48000, 2, 1024, vec![bytes]).is_err());
        let bytes = packet_envelope(0xff, 0, 0, false, &[1, 2], &[3]).unwrap();
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
            PendingVideo::new(SubmittedVideo::Contiguous(
                CompressedVideoFrame::new(format.tier(tier), bytes.clone()).unwrap(),
            ))
        };
        let mut streams = PendingStreams::default();
        let first = make(StreamTier::Program);
        let old_program_data = first.raw.p_data;
        let old_program_header = first.raw.as_ref() as *const NDIlib_video_frame_v2_t;
        assert!(streams.submit(first, |_| {}).is_none());
        assert!(streams
            .submit(make(StreamTier::Preview), |_| {
                // The earlier program header and bytes survive both moving their
                // owners and a preview submission, just as the SDK requires.
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
        let [program, preview] = streams.take();
        assert_eq!(program.unwrap().format().stream_tier(), StreamTier::Program);
        assert_eq!(preview.unwrap().format().stream_tier(), StreamTier::Preview);
    }
}
