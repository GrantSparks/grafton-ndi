//! SDK descriptors paired with the parts validated from their own storage.

use super::*;
use std::mem::ManuallyDrop;

/// Validate an uncompressed video frame the SDK delivered: its pointer,
/// header, pixel format and layout.
///
/// Performs checked arithmetic for the buffer size and enforces
/// `MAX_VIDEO_BYTES`, returning [`Error::InvalidFrame`] for any violation.
pub(crate) fn validate_video_frame(
    raw: &NDIlib_video_frame_v2_t,
) -> Result<(ValidatedVideoLayout, VideoHeader)> {
    if raw.p_data.is_null() {
        return Err(Error::InvalidFrame(crate::FrameDefect::NullVideoData));
    }

    let header = VideoHeader::from_sdk(raw)?;
    let pixel_format = PixelFormat::try_from(FourCc::from_raw(raw.FourCC))?;
    // SAFETY: both members of the descriptor's union are `int`s at the same
    // offset, so reading either one reads the four bytes the SDK or the
    // caller wrote as a valid `i32`. `Processing.NDI.structs.h` makes the
    // member the line stride for a FourCC that is not a compressed type, and
    // `PixelFormat::try_from` just accepted an uncompressed format.
    let line_stride = unsafe { raw.__bindgen_anon_1.line_stride_in_bytes };
    let layout = ValidatedVideoLayout::from_sdk(pixel_format, raw.xres, raw.yres, line_stride)?;
    Ok((layout, header))
}

/// Validate audio frame layout from raw FFI fields.
///
/// This function performs all necessary validation including:
/// - Null pointer check for `p_data`
/// - Valid audio format (FourCC) conversion
/// - Positive sample rate, channel count, and sample count
/// - Checked arithmetic for sample count multiplication
/// - `MAX_AUDIO_BYTES` cap enforcement
///
/// # Arguments
///
/// * `raw` - Reference to the raw NDI audio frame
///
/// # Returns
///
/// `Ok(ValidatedAudioLayout)` if the frame is valid, or `Err(Error::InvalidFrame(...))` otherwise.
/// A descriptor without samples is invalid here; only
/// [`validate_returned_audio`] accepts one.
pub(crate) fn validate_audio_layout(raw: &NDIlib_audio_frame_v3_t) -> Result<ValidatedAudioLayout> {
    if raw.no_samples == 0 {
        // Empty captured audio is rejected before inspecting the pointer;
        // other count failures keep their existing place after format checks.
        validate_audio_count(raw.no_samples.into(), crate::AudioCountKind::Samples)?;
    }

    if raw.p_data.is_null() {
        return Err(Error::InvalidFrame(crate::FrameDefect::NullAudioData));
    }

    if !raw.p_data.cast::<f32>().is_aligned() {
        return Err(Error::InvalidFrame(crate::FrameDefect::AudioMisaligned));
    }

    let format = validate_audio_format(raw.FourCC)?;
    // SAFETY: FLTP uses the channel-stride member of this SDK union.
    let stride = unsafe { raw.__bindgen_anon_1.channel_stride_in_bytes };
    ValidatedAudioLayout::from_sdk(
        format,
        raw.sample_rate,
        raw.no_channels,
        raw.no_samples,
        Some(stride),
    )
}

/// Audio the SDK returned from a call that may legitimately deliver no
/// samples: a FrameSync input-format query, or an AVSync result with nothing
/// to deliver.
///
/// A frame of samples is its layout when only the shape was classified, and
/// a [`ValidatedFrame`] when [`ValidatedAudio::returned_audio`] validated its
/// metadata too.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ReturnedAudio<T = ValidatedAudioLayout> {
    /// A frame of samples, validated as [`validate_audio_layout`] validates.
    Samples(T),
    /// No samples. Carries the input format when the SDK reported one, and
    /// `None` when no source format is known yet.
    Empty(Option<AudioInputFormat>),
}

/// Classify audio the SDK returned where "no samples" is a valid answer.
///
/// A descriptor with a nonzero sample count must pass
/// [`validate_audio_layout`]. One without samples must have a null data
/// pointer and a zero channel stride, and either a zero sample rate and
/// channel count (no source yet, where the FourCC may also be left zero) or a
/// positive sample rate and channel count with a supported FourCC.
///
/// # Errors
///
/// [`Error::InvalidFrame`] for any other shape. The descriptor's metadata is
/// not examined.
pub(crate) fn validate_returned_audio(raw: &NDIlib_audio_frame_v3_t) -> Result<ReturnedAudio> {
    if raw.no_samples != 0 {
        return validate_audio_layout(raw).map(ReturnedAudio::Samples);
    }

    if !raw.p_data.is_null() {
        return Err(Error::InvalidFrame(crate::FrameDefect::EmptyAudioHasData));
    }

    let sample_rate = optional_audio_quantity(raw.sample_rate.into(), validate_audio_sample_rate)?;
    let num_channels = optional_audio_quantity(raw.no_channels.into(), |value| {
        validate_audio_count(value, crate::AudioCountKind::Channels)
    })?;

    // SAFETY: both members of the descriptor's union are `int`s at the same
    // offset, so reading either one reads the four bytes the SDK wrote as a
    // valid `i32`, whichever member `Processing.NDI.structs.h` makes it for
    // this FourCC. Audio without samples has neither a channel stride nor a
    // buffer size, so the value must be zero whatever its meaning.
    let channel_stride_in_bytes = unsafe { raw.__bindgen_anon_1.channel_stride_in_bytes };
    if channel_stride_in_bytes != 0 {
        return Err(Error::InvalidFrame(crate::FrameDefect::EmptyAudioStride {
            stride: channel_stride_in_bytes,
        }));
    }

    let format = match (sample_rate, num_channels) {
        // No source format is known yet. The SDK may leave the FourCC unset.
        (None, None) => {
            if raw.FourCC != 0 {
                validate_audio_format(raw.FourCC)?;
            }
            None
        }
        (Some(sample_rate), Some(num_channels)) => {
            validate_audio_format(raw.FourCC)?;
            Some(AudioInputFormat::new(sample_rate, num_channels))
        }
        _ => {
            return Err(Error::InvalidFrame(crate::FrameDefect::EmptyAudioFormat {
                sample_rate: raw.sample_rate,
                channels: raw.no_channels,
            }))
        }
    };

    Ok(ReturnedAudio::Empty(format))
}

pub(super) fn validate_audio_format(fourcc: NDIlib_FourCC_audio_type_e) -> Result<AudioFormat> {
    AudioFormat::try_from(FourCc::from_raw(fourcc))
}

// ============================================================================
// SDK frames validated as a whole
// ============================================================================

/// The metadata pointer and optional timestamp of a held SDK descriptor.
pub(crate) trait SdkDescriptor {
    /// The descriptor's metadata text pointer, null for none.
    fn metadata_pointer(&self) -> *const c_char;

    /// Standalone metadata has no SDK submission timestamp.
    fn timestamp(&self) -> Option<i64> {
        None
    }

    /// Apply this descriptor's natural metadata rule.
    ///
    /// # Safety
    /// Media metadata must be readable through its first NUL or the cap;
    /// standalone metadata must retain its declared extent (or the same
    /// bounded C-string extent when its length is zero).
    unsafe fn scan_metadata(&self) -> Result<FrameMetadataState>;
}

impl SdkDescriptor for NDIlib_video_frame_v2_t {
    fn metadata_pointer(&self) -> *const c_char {
        self.p_metadata
    }

    fn timestamp(&self) -> Option<i64> {
        Some(self.timestamp)
    }

    unsafe fn scan_metadata(&self) -> Result<FrameMetadataState> {
        // SAFETY: the caller retains the bounded embedded C-string extent.
        Ok(unsafe { FrameMetadataState::scan(self.metadata_pointer()) })
    }
}

impl SdkDescriptor for NDIlib_audio_frame_v3_t {
    fn metadata_pointer(&self) -> *const c_char {
        self.p_metadata
    }

    fn timestamp(&self) -> Option<i64> {
        Some(self.timestamp)
    }

    unsafe fn scan_metadata(&self) -> Result<FrameMetadataState> {
        // SAFETY: the caller retains the bounded embedded C-string extent.
        Ok(unsafe { FrameMetadataState::scan(self.metadata_pointer()) })
    }
}

impl SdkDescriptor for NDIlib_metadata_frame_t {
    fn metadata_pointer(&self) -> *const c_char {
        self.p_data
    }

    unsafe fn scan_metadata(&self) -> Result<FrameMetadataState> {
        // SAFETY: the caller retains this standalone descriptor's declared
        // extent, or its bounded C string when the declared length is zero.
        Ok(FrameMetadataState::from_text(unsafe {
            validate_metadata_layout(self)?
        }))
    }
}

/// A holder of one SDK frame descriptor: a capture [`Guard`], which owns the
/// frame and frees it when dropped, or a borrow of a descriptor.
pub(crate) trait HoldsDescriptor {
    /// The descriptor it holds.
    type Descriptor: SdkDescriptor;

    /// The descriptor.
    fn descriptor(&self) -> &Self::Descriptor;

    /// Select metadata validation from this holder's capture family.
    ///
    /// # Safety
    /// The holder must retain the readable metadata extent guaranteed by its
    /// capture family until dropped, unchanged for the returned observation.
    unsafe fn scan_metadata(&self) -> Result<FrameMetadataState> {
        // SAFETY: the caller retains the descriptor's natural metadata extent.
        unsafe { self.descriptor().scan_metadata() }
    }

    /// Normalize an audio timestamp for this descriptor's capture family.
    fn audio_timestamp(&self, ticks: i64) -> Option<SdkTimestamp> {
        SdkTimestamp::from_sdk(ticks)
    }
}

impl<S: FrameFree> HoldsDescriptor for Guard<'_, S>
where
    S::RawFrame: SdkDescriptor,
{
    type Descriptor = S::RawFrame;

    fn descriptor(&self) -> &S::RawFrame {
        self.frame()
    }

    unsafe fn scan_metadata(&self) -> Result<FrameMetadataState> {
        // SAFETY: Guard's construction retains this family's metadata extent.
        unsafe { S::scan_metadata(self.frame()) }
    }

    fn audio_timestamp(&self, ticks: i64) -> Option<SdkTimestamp> {
        if ticks == S::AUDIO_TIMESTAMP_UNDEFINED {
            None
        } else {
            SdkTimestamp::from_sdk(ticks)
        }
    }
}

#[cfg(test)]
impl<D: SdkDescriptor> HoldsDescriptor for &D {
    type Descriptor = D;

    fn descriptor(&self) -> &D {
        self
    }
}

/// Implementations are restricted to the crate's audited descriptor validators.
pub(crate) mod descriptor_parts_sealed {
    pub trait Sealed {}
}

/// Parts validated from the exact descriptor whose storage a holder retains.
///
/// # Safety
///
/// A successful validator must derive every extent, offset and format in its
/// result from `descriptor`, and never describe storage outside the allocation
/// it names. Metadata validation is selected separately by the holder.
pub(crate) unsafe trait DescriptorParts<D: SdkDescriptor>:
    descriptor_parts_sealed::Sealed + Sized
{
    /// Derive the parts that downstream views may trust without revalidation.
    ///
    /// # Safety
    ///
    /// Every non-null pointer in `descriptor` must retain the readable,
    /// initialized active payload it describes for this call; row/channel
    /// padding need not be initialized. Shape validation may reject a
    /// descriptor, but cannot establish the validity of its raw pointers.
    unsafe fn validate(descriptor: &D) -> Result<Self>;
}

impl descriptor_parts_sealed::Sealed for (ValidatedVideoLayout, VideoHeader) {}
// SAFETY: the validator derives this layout and header from this descriptor,
// checking every dimension, stride and active extent before returning them.
unsafe impl DescriptorParts<NDIlib_video_frame_v2_t> for (ValidatedVideoLayout, VideoHeader) {
    unsafe fn validate(descriptor: &NDIlib_video_frame_v2_t) -> Result<Self> {
        validate_video_frame(descriptor)
    }
}

impl descriptor_parts_sealed::Sealed for ValidatedAudioLayout {}
// SAFETY: the validator checks this descriptor's format, alignment, sample
// counts and channel stride, and returns only its checked sample layout.
unsafe impl DescriptorParts<NDIlib_audio_frame_v3_t> for ValidatedAudioLayout {
    unsafe fn validate(descriptor: &NDIlib_audio_frame_v3_t) -> Result<Self> {
        validate_audio_layout(descriptor)
    }
}

impl descriptor_parts_sealed::Sealed for () {}
// SAFETY: metadata has no media parts. Its holder's capture family supplies
// the text rule separately from these empty parts.
unsafe impl DescriptorParts<NDIlib_metadata_frame_t> for () {
    unsafe fn validate(_: &NDIlib_metadata_frame_t) -> Result<Self> {
        Ok(())
    }
}

/// An SDK frame validated as a whole: the holder `F` of its descriptor, what
/// was validated from that descriptor, `V`, and its cached metadata state.
///
/// Its fields are private to this module. The validation entry points pair
/// the parts with the exact descriptor its holder retains: [`ValidatedFrame::validate`],
/// [`ValidatedAudio::returned_audio`] and [`ValidatedMetadata::metadata_frame`].
/// A layout or text validated from
/// another descriptor therefore cannot be paired with it, and every view and
/// owned copy of a received frame takes its parts from here.
///
/// The text is kept as a length and borrowed again from the descriptor's
/// pointer, so that this type adds no pointer of its own: a frame reference
/// that holds one keeps the thread and unwind auto traits of its guard. `V`
/// is a parameter rather than a projection from `F` so that a reference
/// stays covariant in the lifetime of its guard.
pub(crate) struct ValidatedFrame<F, V> {
    frame: F,
    validated: V,
    /// Text length when valid, or the reason optional metadata was withheld.
    metadata_state: FrameMetadataState,
}

/// A video frame validated with its layout and header.
pub(crate) type ValidatedVideo<F> = ValidatedFrame<F, (ValidatedVideoLayout, VideoHeader)>;

/// A frame of audio samples validated with its layout.
pub(crate) type ValidatedAudio<F> = ValidatedFrame<F, ValidatedAudioLayout>;

/// A standalone metadata frame, which is its text alone.
pub(crate) type ValidatedMetadata<F> = ValidatedFrame<F, ()>;

impl<F: HoldsDescriptor, V> ValidatedFrame<F, V> {
    /// Validate parts from the one held descriptor, then scan its metadata.
    ///
    /// # Safety
    ///
    /// `frame` must retain this descriptor and every allocation the returned
    /// parts describe, initialized and unchanged until it is dropped.
    /// Metadata must retain the readable extent guaranteed by the holder's
    /// capture family: bounded C strings for media and sender metadata, or
    /// the declared extent for received standalone metadata.
    unsafe fn validate_held(frame: F) -> Result<Self>
    where
        V: DescriptorParts<F::Descriptor>,
    {
        // SAFETY: the holder's storage validity is the caller's obligation;
        // DescriptorParts derives its result only from this exact descriptor.
        let validated = unsafe { V::validate(frame.descriptor())? };
        // SAFETY: the caller retains the metadata extent for this holder's
        // capture family, independently of the validated media parts.
        unsafe { Self::with_frame_metadata(frame, validated) }
    }

    /// Parts validated from this exact held descriptor.
    #[cfg(feature = "advanced")]
    pub(crate) fn parts(&self) -> &V {
        &self.validated
    }

    /// Normalize the held descriptor's timestamp for its capture family.
    pub(crate) fn timestamp(&self) -> Option<SdkTimestamp> {
        self.frame.audio_timestamp(self.descriptor().timestamp()?)
    }

    /// Pair `frame` with `validated`, which the caller validated from its
    /// descriptor, and the per-frame metadata text the descriptor points to.
    ///
    /// # Safety
    ///
    /// `validated` must have been derived from this exact held descriptor.
    /// The holder must keep its descriptor and every allocation described by
    /// those parts initialized, live and unchanged until it is dropped. Its
    /// metadata must retain the readable extent guaranteed by the holder's
    /// capture family.
    unsafe fn with_frame_metadata(frame: F, validated: V) -> Result<Self> {
        // SAFETY: the caller retains the metadata extent for the holder's
        // statically selected family rule, including declared standalone bytes.
        let metadata_state = unsafe { frame.scan_metadata()? };
        Ok(Self {
            metadata_state,
            frame,
            validated,
        })
    }

    /// The descriptor, exactly as the SDK delivered it.
    pub(crate) fn descriptor(&self) -> &F::Descriptor {
        self.frame.descriptor()
    }

    /// Reborrow the observation scanned from this exact held descriptor.
    pub(crate) fn frame_metadata(&self) -> FrameMetadataRef<'_> {
        // SAFETY: construction scanned this descriptor's pointer; the holder
        // keeps the same bytes allocated and unchanged for this borrow.
        unsafe {
            self.metadata_state
                .view(self.descriptor().metadata_pointer())
        }
    }

    pub(crate) fn metadata(&self) -> Option<MetadataText<'_>> {
        self.frame_metadata().text()
    }
}

impl<F: HoldsDescriptor<Descriptor = NDIlib_video_frame_v2_t>> ValidatedVideo<F> {
    /// Validate the uncompressed video frame `frame` holds: its pointer,
    /// header, pixel format and layout, as [`validate_video_frame`] does, and
    /// its metadata text.
    ///
    /// # Safety
    ///
    /// `frame` must hold one descriptor for as long as it lives, and until
    /// it is dropped, its pixel allocation must cover the layout the
    /// descriptor describes, with every active row initialized, and its
    /// metadata pointer must be null or readable up to its first NUL or
    /// [`MAX_METADATA_BYTES`], all live and unchanged. Row padding need not
    /// be initialized.
    #[cfg(test)]
    unsafe fn video_held(frame: F) -> Result<Self> {
        // SAFETY: forwarded to the caller for this held video descriptor.
        unsafe { Self::validate_held(frame) }
    }
}

impl<F: HoldsDescriptor<Descriptor = NDIlib_audio_frame_v3_t>> ValidatedAudio<F> {
    /// Validate the frame of audio samples `frame` holds, as
    /// [`validate_audio_layout`] does, and its metadata text.
    ///
    /// # Safety
    ///
    /// `frame` must hold one descriptor for as long as it lives, and until
    /// it is dropped, every channel's samples at the offsets the descriptor
    /// describes must be initialized, and its metadata pointer must be null
    /// or readable up to its first NUL or [`MAX_METADATA_BYTES`], all live
    /// and unchanged. Padding between channels need not be initialized.
    #[cfg(test)]
    unsafe fn audio_held(frame: F) -> Result<Self> {
        // SAFETY: forwarded to the caller for this held audio descriptor.
        unsafe { Self::validate_held(frame) }
    }

    /// Validate audio the SDK returned where no samples is a valid answer:
    /// its shape, as [`validate_returned_audio`] classifies it, and the
    /// metadata text of a frame of samples.
    ///
    /// # Safety
    ///
    /// As for [`Self::validate_held`], for a descriptor that holds samples.
    #[cfg(all(feature = "advanced", any(test, ndi_has_advanced_av_sync)))]
    unsafe fn returned_audio_held(frame: F) -> Result<ReturnedAudio<Self>> {
        Ok(match validate_returned_audio(frame.descriptor())? {
            ReturnedAudio::Empty(format) => ReturnedAudio::Empty(format),
            // SAFETY: forwarded to the caller for a frame of samples.
            ReturnedAudio::Samples(validated) => {
                ReturnedAudio::Samples(unsafe { Self::with_frame_metadata(frame, validated)? })
            }
        })
    }
}

impl<F: HoldsDescriptor<Descriptor = NDIlib_metadata_frame_t>> ValidatedMetadata<F> {
    /// Validate the metadata frame using its holder's capture-family rule.
    ///
    /// # Safety
    ///
    /// `frame` must hold one descriptor for as long as it lives, and until
    /// it is dropped, its data pointer must be null or reference readable
    /// storage for its family's declared-length or bounded C-string rule,
    /// live and unchanged.
    unsafe fn metadata_frame_held(frame: F) -> Result<Self> {
        // SAFETY: the caller retains this standalone descriptor's text.
        unsafe { Self::validate_held(frame) }
    }
}

// Safe production entry points accept only the holder whose construction
// establishes delivered storage validity. Raw descriptor borrows are test-only.
impl<'a, S: FrameFree, V> ValidatedFrame<Guard<'a, S>, V>
where
    S::RawFrame: SdkDescriptor,
    V: DescriptorParts<S::RawFrame>,
{
    /// Validate this delivered guard's descriptor and cache its metadata.
    pub(crate) fn validate(frame: Guard<'a, S>) -> Result<Self> {
        // SAFETY: Guard retains its descriptor and initialized storage until
        // matching release. The sealed validator derives V from that descriptor.
        unsafe { Self::validate_held(frame) }
    }
}

impl<'a, S: FrameFree<RawFrame = NDIlib_audio_frame_v3_t>> ValidatedAudio<Guard<'a, S>> {
    /// Validate a guarded synchronization result, including an empty answer.
    #[cfg(all(feature = "advanced", any(test, ndi_has_advanced_av_sync)))]
    pub(crate) fn returned_audio(frame: Guard<'a, S>) -> Result<ReturnedAudio<Self>> {
        // SAFETY: null pointers are permitted by Guard; each non-null pointer
        // retains the initialized sample or metadata extent it describes.
        unsafe { Self::returned_audio_held(frame) }
    }
}

impl<'a, S: FrameFree<RawFrame = NDIlib_metadata_frame_t>> ValidatedMetadata<Guard<'a, S>> {
    /// Validate the standalone metadata descriptor and text Guard retains.
    pub(crate) fn metadata_frame(frame: Guard<'a, S>) -> Result<Self> {
        // SAFETY: Guard retains the readable initialized metadata extent
        // guaranteed by S, whose associated method selects the text rule.
        unsafe { Self::metadata_frame_held(frame) }
    }
}

#[cfg(all(test, feature = "advanced"))]
impl<'a, D: SdkDescriptor, V> ValidatedFrame<&'a D, V> {
    /// Validate a synthetic descriptor whose storage the test retains.
    ///
    /// # Safety
    /// Every pointer and initialized extent described by `frame` must stay
    /// valid and unchanged for `'a`, including bounded metadata text. The
    /// validator must derive its parts from the descriptor it receives.
    pub(crate) unsafe fn validate_descriptor(
        frame: &'a D,
        validate: impl FnOnce(&D) -> Result<V>,
    ) -> Result<Self> {
        let validated = validate(frame)?;
        // SAFETY: the test caller retains this descriptor and guarantees its
        // validator derives these parts from that descriptor.
        unsafe { Self::with_frame_metadata(frame, validated) }
    }
}

#[cfg(test)]
impl<'a> ValidatedVideo<&'a NDIlib_video_frame_v2_t> {
    /// # Safety
    /// The test retains initialized active pixels and readable metadata for `'a`.
    pub(crate) unsafe fn video_descriptor(frame: &'a NDIlib_video_frame_v2_t) -> Result<Self> {
        // SAFETY: forwarded to the test caller.
        unsafe { Self::video_held(frame) }
    }
}

#[cfg(test)]
impl<'a> ValidatedAudio<&'a NDIlib_audio_frame_v3_t> {
    /// # Safety
    /// The test retains initialized active samples and readable metadata for `'a`.
    pub(crate) unsafe fn audio_descriptor(frame: &'a NDIlib_audio_frame_v3_t) -> Result<Self> {
        // SAFETY: forwarded to the test caller.
        unsafe { Self::audio_held(frame) }
    }

    /// # Safety
    /// The test retains each non-null sample/metadata extent for `'a`.
    #[cfg(feature = "advanced")]
    pub(crate) unsafe fn returned_audio_descriptor(
        frame: &'a NDIlib_audio_frame_v3_t,
    ) -> Result<ReturnedAudio<Self>> {
        // SAFETY: forwarded to the test caller.
        unsafe { Self::returned_audio_held(frame) }
    }
}

// SAFETY: video validates this descriptor's layout and metadata; its holder's
// invariant guarantees initialized active pixels and live unchanged storage for
// F's lifetime. Views borrow this holder. Default descriptors and direct-send
// checks never extend the initialized active span into padding or tail room:
// captures report Initialized::ActiveRows.
unsafe impl<F: HoldsDescriptor<Descriptor = NDIlib_video_frame_v2_t>> VideoParts
    for ValidatedVideo<F>
{
    #[inline]
    fn layout(&self) -> &ValidatedVideoLayout {
        &self.validated.0
    }
    #[inline]
    fn header(&self) -> &VideoHeader {
        &self.validated.1
    }
    #[inline]
    fn pixels(&self) -> *const u8 {
        self.descriptor().p_data
    }
    #[inline]
    fn frame_metadata(&self) -> FrameMetadataRef<'_> {
        ValidatedFrame::frame_metadata(self)
    }

    #[inline]
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
        self.timestamp()
    }
}

// SAFETY: audio/returned_audio validate this descriptor's layout and text;
// the holder's invariant guarantees aligned initialized active samples and live
// unchanged storage for F's lifetime. Views and default descriptors borrow
// this holder and address only that validated sample extent and metadata.
unsafe impl<F: HoldsDescriptor<Descriptor = NDIlib_audio_frame_v3_t>> AudioParts
    for ValidatedAudio<F>
{
    #[inline]
    fn layout(&self) -> &ValidatedAudioLayout {
        &self.validated
    }
    #[inline]
    fn audio_timecode(&self) -> Timecode {
        Timecode::from_ticks(self.descriptor().timecode)
    }
    #[inline]
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
        self.timestamp()
    }
    #[inline]
    fn samples(&self) -> *const f32 {
        self.descriptor().p_data.cast()
    }
    #[inline]
    fn frame_metadata(&self) -> FrameMetadataRef<'_> {
        ValidatedFrame::frame_metadata(self)
    }
}

pub(super) struct SenderMetadataGuard<'a> {
    guard: ManuallyDrop<Guard<'a, SenderMetadataFree>>,
    lease: ManuallyDrop<crate::sender::SenderMetadataLease<'a>>,
}

impl Drop for SenderMetadataGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: both fields are initialized once and taken only here. The
        // native guard is freed while the capture lease still excludes other
        // captures; only then may releasing the lease wake another capture.
        // This order also holds on validation failure and during unwinding.
        unsafe {
            drop(ManuallyDrop::take(&mut self.guard));
            drop(ManuallyDrop::take(&mut self.lease));
        }
    }
}

impl HoldsDescriptor for SenderMetadataGuard<'_> {
    type Descriptor = NDIlib_metadata_frame_t;
    fn descriptor(&self) -> &Self::Descriptor {
        self.guard.frame()
    }

    unsafe fn scan_metadata(&self) -> Result<FrameMetadataState> {
        // SAFETY: this wrapper keeps its sender guard and capture lease live.
        unsafe { self.guard.scan_metadata() }
    }
}

impl<'a> ValidatedMetadata<SenderMetadataGuard<'a>> {
    /// The retained sender guard establishes its family's text extent.
    fn validate_sender(frame: SenderMetadataGuard<'a>) -> Result<Self> {
        // SAFETY: the wrapper retains its Guard and capture lease until release.
        unsafe { Self::validate_held(frame) }
    }
}

pub(super) enum MetadataLoan<'a> {
    Receiver(ValidatedMetadata<RecvMetadataGuard<'a>>),
    Sender(ValidatedMetadata<SenderMetadataGuard<'a>>),
}

impl<'a> MetadataLoan<'a> {
    pub(super) fn receiver(guard: RecvMetadataGuard<'a>) -> Result<Self> {
        Ok(Self::Receiver(ValidatedMetadata::metadata_frame(guard)?))
    }
    pub(super) fn sender(
        guard: Guard<'a, SenderMetadataFree>,
        lease: crate::sender::SenderMetadataLease<'a>,
    ) -> Result<Self> {
        let guard = SenderMetadataGuard {
            guard: ManuallyDrop::new(guard),
            lease: ManuallyDrop::new(lease),
        };
        Ok(Self::Sender(ValidatedMetadata::validate_sender(guard)?))
    }
    pub(super) fn descriptor(&self) -> &NDIlib_metadata_frame_t {
        match self {
            Self::Receiver(frame) => frame.descriptor(),
            Self::Sender(frame) => frame.descriptor(),
        }
    }
    pub(super) fn metadata(&self) -> Option<MetadataText<'_>> {
        match self {
            Self::Receiver(frame) => frame.metadata(),
            Self::Sender(frame) => frame.metadata(),
        }
    }
}
