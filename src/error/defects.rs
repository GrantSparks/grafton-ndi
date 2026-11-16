//! Allocation-free causes and the rare diagnostic payloads errors retain.
use crate::frames::{MAX_AUDIO_BYTES, MAX_METADATA_BYTES, MAX_VIDEO_BYTES};
use crate::{ColorInfo, FrameMetadataStatus, PixelFormat, TransferFunction};
use std::time::Duration;
use thiserror::Error;

/// The syntactic defect in an `ndi_color_info` element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum ColorMetadataDefect {
    /// The element is not closed.
    #[error("the element is not closed")]
    UnclosedElement,
    /// An attribute cannot start at this byte.
    #[error("an attribute cannot start at this byte")]
    AttributeStart,
    /// An attribute has no value.
    #[error("an attribute has no value")]
    MissingValue,
    /// An attribute value is not quoted.
    #[error("an attribute value is not quoted")]
    UnquotedValue,
    /// An attribute value has no closing quote.
    #[error("an attribute value has no closing quote")]
    UnclosedValue,
    /// An attribute occurs twice.
    #[error("an attribute occurs twice")]
    DuplicateAttribute,
    /// A color attribute has an undefined value.
    #[error("a color attribute has an undefined value")]
    UndefinedValue,
}

/// The image encoder that refused output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum ImageCodecKind {
    /// PNG.
    #[error("PNG")]
    Png,
    /// JPEG.
    #[error("JPEG")]
    Jpeg,
    /// WebP.
    #[error("WebP")]
    Webp,
}

/// The rejected native Discovery Server address, retained only for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
#[error(
    "the SDK created no {object}: it does not accept {address:?} as a Discovery Server address"
)]
pub struct DiscoveryAddressFailure {
    /// The kind of object the SDK refused to create.
    pub object: &'static str,
    /// The address the caller supplied.
    pub address: Box<str>,
}

/// The malformed native discovery data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum DiscoveryDefect {
    /// An array's span overflows.
    #[error("SDK discovery array span overflow")]
    ArraySpanOverflow,
    /// A nonempty array has no data.
    #[error("SDK discovery array is null with a nonzero count")]
    NullArray,
}
/// A refusal of the shared media layout and value checks.
///
/// Frame validation nests this cause in [`FrameDefect::Layout`]. Standalone
/// aspect ratios, frame rates, compressed video formats and allocation strides
/// instead use [`InputDefect::Layout`]. Layout validators used by SDK allocator
/// callbacks construct only allocation-free variants. The two wide diagnostic
/// variants are constructed only by their standalone value checks.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum LayoutDefect {
    /// Both parts of the frame rate must fit a positive SDK quantity.
    #[error("{detail}")]
    FrameRate {
        /// Exact values, boxed only on 32-bit targets.
        detail: CompactDiagnostic<FrameRateDefect>,
    },
    /// Compressed dimensions must fit positive SDK quantities.
    #[error("{detail}")]
    CompressedVideoDimensions {
        /// Exact values, boxed only on 32-bit targets.
        detail: CompactDiagnostic<VideoDimensionsDefect>,
    },

    /// Video line stride must be positive, got 0.
    #[error("Video line stride must be positive, got 0")]
    VideoZeroStride,
    /// Video buffer size overflow.
    #[error("Video buffer size overflow")]
    VideoBufferOverflow,
    /// Audio channel stride exceeds i32 range.
    #[error("Audio channel stride exceeds i32 range")]
    AudioStrideRange,
    /// Video frame has invalid width: {0}.
    #[error("Video frame has invalid width: {0}")]
    VideoWidth(DiagnosticInteger),
    /// NDI video frames require even width, got {0}.
    #[error("NDI video frames require even width, got {0}")]
    VideoOddWidth(DiagnosticInteger),
    /// Video frame has invalid height: {0}.
    #[error("Video frame has invalid height: {0}")]
    VideoHeight(DiagnosticInteger),
    /// Uncompressed video frame has invalid line_stride_in_bytes: {0}.
    #[error("Uncompressed video frame has invalid line_stride_in_bytes: {0}")]
    VideoSdkStride(i32),
    /// Video line_stride_in_bytes {stride} is smaller than minimum row size {minimum} for {format:?} width {width}.
    #[error("Video line_stride_in_bytes {stride} is smaller than minimum row size {minimum} for {format:?} width {width}")]
    VideoShortStride {
        /// The stride.
        stride: usize,
        /// The minimum.
        minimum: usize,
        /// The format.
        format: PixelFormat,
        /// The width.
        width: u32,
    },
    /// Video line stride overflow for {format:?} width {width}.
    #[error("Video line stride overflow for {format:?} width {width}")]
    VideoStrideOverflow {
        /// The format.
        format: PixelFormat,
        /// The width.
        width: usize,
    },
    /// Video frame exceeds maximum size: {0} bytes > {MAX_VIDEO_BYTES} bytes.
    #[error("Video frame exceeds maximum size: {0} bytes > {MAX_VIDEO_BYTES} bytes")]
    VideoBytes(usize),
    /// Planar 4:2:0 (YV12/I420) video frames require even height and line stride, got height {height} and stride {stride}.
    #[error("Planar 4:2:0 (YV12/I420) video frames require even height and line stride, got height {height} and stride {stride}")]
    VideoPlanar420 {
        /// The height.
        height: usize,
        /// The stride.
        stride: usize,
    },
    /// Semi-planar 4:2:0 (NV12) video frames require even height, got {0}.
    #[error("Semi-planar 4:2:0 (NV12) video frames require even height, got {0}")]
    VideoSemiPlanar420(usize),
    /// UYVA video frames require even line stride, got {0}.
    #[error("UYVA video frames require even line stride, got {0}")]
    VideoUyvaStride(usize),
    /// Video frame has invalid picture aspect ratio: {0}.
    #[error("Video frame has invalid picture aspect ratio: {0}")]
    PictureAspectRatio(f32),
    /// Invalid sample rate: {0}.
    #[error("Invalid sample rate: {0}")]
    AudioRate(DiagnosticInteger),
    /// Invalid number of {kind}: {value}.
    #[error("Invalid number of {kind}: {value}")]
    AudioCount {
        /// The value.
        value: DiagnosticInteger,
        /// The kind.
        kind: AudioCountKind,
    },
    /// Invalid channel_stride_in_bytes: {0}.
    #[error("Invalid channel_stride_in_bytes: {0}")]
    AudioStride(DiagnosticInteger),
    /// channel_stride_in_bytes must be a multiple of 4, got {0}.
    #[error("channel_stride_in_bytes must be a multiple of 4, got {0}")]
    AudioStrideAlignment(usize),
    /// channel_stride_in_bytes {stride} is smaller than one channel of audio samples {minimum}.
    #[error(
        "channel_stride_in_bytes {stride} is smaller than one channel of audio samples {minimum}"
    )]
    AudioShortStride {
        /// The stride.
        stride: usize,
        /// The minimum.
        minimum: usize,
    },
    /// Audio channel stride overflow: {0} samples × 4 bytes.
    #[error("Audio channel stride overflow: {0} samples × 4 bytes")]
    AudioStrideOverflow(usize),
    /// Audio channel offset overflow: {channels} channels × {stride} stride samples.
    #[error("Audio channel offset overflow: {channels} channels × {stride} stride samples")]
    AudioChannelOffset {
        /// The channels.
        channels: usize,
        /// The stride.
        stride: usize,
    },
    /// Audio backing sample count overflow: channel offset {offset} + {samples} samples.
    #[error("Audio backing sample count overflow: channel offset {offset} + {samples} samples")]
    AudioSampleCount {
        /// The offset.
        offset: usize,
        /// The samples.
        samples: usize,
    },
    /// Audio byte size overflow: {0} samples × 4 bytes.
    #[error("Audio byte size overflow: {0} samples × 4 bytes")]
    AudioByteSize(usize),
    /// Audio frame exceeds maximum size: {0} bytes > {MAX_AUDIO_BYTES} bytes.
    #[error("Audio frame exceeds maximum size: {0} bytes > {MAX_AUDIO_BYTES} bytes")]
    AudioBytes(usize),
    /// Video line stride {stride} exceeds i32 range.
    #[error("Video line stride {stride} exceeds i32 range")]
    VideoStrideRange {
        /// The stride.
        stride: usize,
    },
}

/// Typed causes of [`crate::Error::InvalidFrame`].
///
/// Shared layout refusals use [`FrameDefect::Layout`] and [`LayoutDefect`].
/// Ordinary causes retain their evidence inline without allocating. Boxed
/// diagnostic fields retain caller or SDK text only for the documented exceptions.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum FrameDefect {
    /// A FourCC does not identify a supported packet-envelope codec.
    #[error("Unknown packet envelope codec FourCC: 0x{fourcc:08X}")]
    UnknownEnvelopeCodec {
        /// The unrecognized FourCC value.
        fourcc: u32,
    },
    /// A refusal of the shared media layout or value checks.
    #[error(transparent)]
    Layout(LayoutDefect),
    /// Scratch dimensions overflow the address space.
    #[error("image scratch size overflow")]
    ImageScratchOverflow,
    /// A scaled image dimension cannot fit u32.
    #[error("image dimension overflow")]
    ImageDimensionOverflow,
    /// A codec cannot encode this dimension.
    #[error("{codec} {dimension} {length} exceeds maximum supported value {maximum}")]
    ImageDimension {
        /// The codec.
        codec: ImageCodecKind,
        /// The dimension.
        dimension: ImageAxisKind,
        /// The actual length.
        length: usize,
        /// The codec's maximum.
        maximum: usize,
    },
    /// Metadata frame length does not include a trailing NUL terminator.
    #[error("Metadata frame length does not include a trailing NUL terminator")]
    MetadataMissingTerminator,
    /// Metadata frame contains an interior NUL byte.
    #[error("Metadata frame contains an interior NUL byte")]
    MetadataInteriorNul,
    /// Metadata frame has non-zero length with null data pointer.
    #[error("Metadata frame has non-zero length with null data pointer")]
    MetadataNullData,
    /// Video frame has null data pointer.
    #[error("Video frame has null data pointer")]
    NullVideoData,
    /// Audio frame has null data pointer.
    #[error("Audio frame has null data pointer")]
    NullAudioData,
    /// Audio data pointer is not aligned for f32.
    #[error("Audio data pointer is not aligned for f32")]
    AudioMisaligned,
    /// Zero-length audio frame has non-null data pointer.
    #[error("Zero-length audio frame has non-null data pointer")]
    EmptyAudioHasData,
    /// V210 active row size overflow.
    #[error("V210 active row size overflow")]
    V210RowOverflow,
    /// V210 stride must be a multiple of four and cover every complete pixel group.
    #[error("V210 stride must be a multiple of four and cover every complete pixel group")]
    V210Stride,
    /// V210 storage exceeds the video limit.
    #[error("V210 storage exceeds the video limit")]
    V210Storage,
    /// V210 conversion requires a positive width divisible by six.
    #[error("V210 conversion requires a positive width divisible by six")]
    V210Width,
    /// P216 frame format and dimensions must match the V210 layout.
    #[error("P216 frame format and dimensions must match the V210 layout")]
    P216LayoutMismatch,
    /// planar destination rate, channels and samples must match the interleaved source.
    #[error("planar destination rate, channels and samples must match the interleaved source")]
    PlanarAudioMismatch,
    /// Truncated compressed packet header.
    #[error("Truncated compressed packet header")]
    PacketHeaderTruncated,
    /// Unsupported compressed packet header size.
    #[error("Unsupported compressed packet header size")]
    PacketHeaderSize,
    /// Cannot send a packet with an unsupported header version or unknown flags.
    #[error("Cannot send a packet with an unsupported header version or unknown flags")]
    PacketHeaderUnsupported,
    /// Packet codec does not match the frame's codec.
    #[error("Packet codec does not match the frame's codec")]
    PacketCodecMismatch,
    /// Empty codec payload.
    #[error("Empty codec payload")]
    EmptyCodecPayload,
    /// Packet header lengths do not match the size of the frame.
    #[error("Packet header lengths do not match the size of the frame")]
    PacketLengthsMismatch,
    /// Keyframes require codec configuration data.
    #[error("Keyframes require codec configuration data")]
    KeyframeConfigurationMissing,
    /// AAC packets must carry the keyframe flag.
    #[error("AAC packets must carry the keyframe flag")]
    AacKeyframeMissing,
    /// AAC packets require their AudioSpecificConfig as codec configuration data.
    #[error("AAC packets require their AudioSpecificConfig as codec configuration data")]
    AacConfigurationMissing,
    /// The AAC configuration names a different sample rate than the frame.
    #[error("The AAC configuration names a different sample rate than the frame")]
    AacRateMismatch,
    /// The AAC configuration names a different channel layout than the frame.
    #[error("The AAC configuration names a different channel layout than the frame")]
    AacChannelsMismatch,
    /// Invalid compressed packet length.
    #[error("Invalid compressed packet length")]
    PacketLength,
    /// Compressed packet length overflow.
    #[error("Compressed packet length overflow")]
    PacketLengthOverflow,
    /// Truncated compressed packet.
    #[error("Truncated compressed packet")]
    PacketTruncated,
    /// Packet size overflow.
    #[error("Packet size overflow")]
    PacketSizeOverflow,
    /// Invalid compressed packet payload size.
    #[error("Invalid compressed packet payload size")]
    PacketPayloadSize,
    /// Unknown compressed video FourCC.
    #[error("Unknown compressed video FourCC")]
    UnknownCompressedVideo,
    /// A SpeedHQ frame begins with its quality, a byte below 100.
    #[error("A SpeedHQ frame begins with its quality, a byte below 100")]
    SpeedHqQuality,
    /// An H.264 or HEVC picture on the preview stream is at most 640 pixels wide and 640 high.
    #[error(
        "An H.264 or HEVC picture on the preview stream is at most 640 pixels wide and 640 high"
    )]
    PreviewVideoDimensions,
    /// Invalid compressed video size.
    #[error("Invalid compressed video size")]
    CompressedVideoSize,
    /// Truncated SpeedHQ frame.
    #[error("Truncated SpeedHQ frame")]
    SpeedHqTruncated,
    /// Opus is sent at 8000, 12000, 16000, 24000 or 48000 Hz.
    #[error("Opus is sent at 8000, 12000, 16000, 24000 or 48000 Hz")]
    OpusSampleRate,
    /// An Opus frame holds 2.5, 5, 10, 20, 40 or 60 ms of audio.
    #[error("An Opus frame holds 2.5, 5, 10, 20, 40 or 60 ms of audio")]
    OpusDuration,
    /// An AAC frame holds 1024 samples per channel.
    #[error("An AAC frame holds 1024 samples per channel")]
    AacSampleCount,
    /// AAC is sent at a sample rate its configuration can name, from 7350 to 96000 Hz.
    #[error("AAC is sent at a sample rate its configuration can name, from 7350 to 96000 Hz")]
    AacSampleRate,
    /// AAC is sent with one to six or eight channels.
    #[error("AAC is sent with one to six or eight channels")]
    AacChannels,
    /// Invalid compressed audio size.
    #[error("Invalid compressed audio size")]
    CompressedAudioSize,
    /// Invalid compressed frame buffer.
    #[error("Invalid compressed frame buffer")]
    CompressedBuffer,
    /// Invalid scatter block count.
    #[error("Invalid scatter block count")]
    ScatterBlockCount,
    /// Scatter blocks must not be empty.
    #[error("Scatter blocks must not be empty")]
    EmptyScatterBlock,
    /// Scatter frame exceeds the packet size limit.
    #[error("Scatter frame exceeds the packet size limit")]
    ScatterSize,
    /// The first scatter block must hold the frame's complete header.
    #[error("The first scatter block must hold the frame's complete header")]
    ScatterHeader,
    /// Unknown compressed audio FourCC.
    #[error("Unknown compressed audio FourCC")]
    UnknownCompressedAudio,
    /// Unknown scan type: 0x{code:08X}.
    #[error("Unknown scan type: 0x{code:08X}")]
    UnknownScanType {
        /// The code.
        code: u32,
    },
    /// Audio buffer has {actual} samples, requires at least {minimum}.
    #[error("Audio buffer has {actual} samples, requires at least {minimum}")]
    AudioBufferTooSmall {
        /// The actual.
        actual: usize,
        /// The minimum.
        minimum: usize,
    },
    /// Metadata frame has negative length: {length}.
    #[error("Metadata frame has negative length: {length}")]
    MetadataNegativeLength {
        /// The length.
        length: i32,
    },
    /// Metadata exceeds maximum size: {length} bytes > {MAX_METADATA_BYTES} bytes.
    #[error("Metadata exceeds maximum size: {length} bytes > {MAX_METADATA_BYTES} bytes")]
    MetadataTooLarge {
        /// The length.
        length: usize,
    },
    /// Frame metadata is missing a NUL terminator within {MAX_METADATA_BYTES} bytes.
    #[error("Frame metadata is missing a NUL terminator within {MAX_METADATA_BYTES} bytes")]
    MetadataUnterminated,
    /// Video data length {actual}, expected {expected} bytes for validated layout.
    #[error("Video data length {actual}, expected {expected} bytes for validated layout")]
    VideoDataLength {
        /// The actual.
        actual: usize,
        /// The expected.
        expected: usize,
    },
    /// Audio data length {actual}, expected {expected} samples ({channels} channels x {samples} samples) for validated layout.
    #[error("{detail}")]
    AudioDataLength {
        /// Exact values, boxed only on 32-bit targets.
        detail: CompactDiagnostic<AudioLengthDefect>,
    },
    /// Zero-length audio frame has non-zero channel_stride_in_bytes: {stride}.
    #[error("Zero-length audio frame has non-zero channel_stride_in_bytes: {stride}")]
    EmptyAudioStride {
        /// The stride.
        stride: i32,
    },
    /// Invalid zero-length audio state: sample_rate={sample_rate}, no_channels={channels}.
    #[error("Invalid zero-length audio state: sample_rate={sample_rate}, no_channels={channels}")]
    EmptyAudioFormat {
        /// The sample rate.
        sample_rate: i32,
        /// The channels.
        channels: i32,
    },
    /// Unknown audio format FourCC: 0x{fourcc:08X}.
    #[error("Unknown audio format FourCC: 0x{fourcc:08X}")]
    UnknownAudioFormat {
        /// The fourcc.
        fourcc: u32,
    },
    /// Unknown pixel format FourCC: 0x{fourcc:08X}.
    #[error("Unknown pixel format FourCC: 0x{fourcc:08X}")]
    UnknownPixelFormat {
        /// The fourcc.
        fourcc: u32,
    },
    /// FrameSync audio input-format query returned {channels} channels of {samples} samples.
    #[error(
        "FrameSync audio input-format query returned {channels} channels of {samples} samples"
    )]
    FrameSyncAudioInput {
        /// The channels.
        channels: usize,
        /// The samples.
        samples: usize,
    },
    /// Buffer too small for {format:?}: got {actual} bytes, expected at least {minimum}.
    #[error("Buffer too small for {format:?}: got {actual} bytes, expected at least {minimum}")]
    VideoBufferTooSmall {
        /// The format.
        format: PixelFormat,
        /// The actual.
        actual: usize,
        /// The minimum.
        minimum: usize,
    },
    /// V210 buffer has {actual} words, requires {minimum}.
    #[error("V210 buffer has {actual} words, requires {minimum}")]
    V210BufferTooSmall {
        /// The actual.
        actual: usize,
        /// The minimum.
        minimum: usize,
    },
    /// interleaved audio has {actual} samples, which is not a multiple of {channels} channels.
    #[error(
        "interleaved audio has {actual} samples, which is not a multiple of {channels} channels"
    )]
    InterleavedAudioLength {
        /// The actual.
        actual: usize,
        /// The channels.
        channels: u32,
    },
    /// audio destination has {actual} samples, requires {minimum}.
    #[error("audio destination has {actual} samples, requires {minimum}")]
    AudioDestinationLength {
        /// The actual.
        actual: usize,
        /// The minimum.
        minimum: usize,
    },
    /// Compressed audio of {sample_rate} Hz, {channels} channels and {samples} samples per channel: each must be in 1..=2147483647.
    #[error("{detail}")]
    CompressedAudioQuantities {
        /// Exact values, boxed only on 32-bit targets.
        detail: CompactDiagnostic<CompressedAudioDefect>,
    },
    /// The frame's color description cannot be read because its metadata was withheld ({status:?}); ColorPolicy::Override explicitly supplies its color conversion.
    #[error("The frame's color description cannot be read because its metadata was withheld ({status:?}); ColorPolicy::Override explicitly supplies its color conversion")]
    ColorMetadataWithheld {
        /// The status.
        status: FrameMetadataStatus,
    },
    /// The frame's metadata holds ndi_color_info elements that differ: {first:?} and {second:?}.
    #[error(
        "The frame's metadata holds ndi_color_info elements that differ: {first:?} and {second:?}"
    )]
    ConflictingColorMetadata {
        /// The first.
        first: ColorInfo,
        /// The second.
        second: ColorInfo,
    },
    /// The frame's ndi_color_info gives the {transfer} transfer function; HdrPolicy::CodeValues encodes its code values to a tagged PNG.
    #[error("The frame's ndi_color_info gives the {transfer} transfer function; HdrPolicy::CodeValues encodes its code values to a tagged PNG")]
    HdrRefused {
        /// The transfer.
        transfer: TransferFunction,
    },

    /// Malformed color text; the issue is typed and its original diagnostic is boxed.
    #[error("{diagnostic}")]
    ColorMetadata {
        /// The syntax or value refusal.
        issue: ColorMetadataDefect,
        /// Original text, including the bounded peer-text excerpt.
        diagnostic: Box<str>,
    },
    /// The encoder's diagnostic, whose content is supplied by that library.
    #[error("{codec} encoding failed: {diagnostic}")]
    EncodingFailed {
        /// The encoder.
        codec: ImageCodecKind,
        /// The library diagnostic.
        diagnostic: Box<str>,
    },
}
/// Typed causes of [`crate::Error::InvalidInput`].
///
/// [`InputDefect::Layout`] reports standalone aspect ratios, frame rates,
/// compressed video formats and allocation strides through [`LayoutDefect`].
///
/// Ordinary causes retain their evidence inline without allocating. Boxed
/// diagnostic fields retain caller or SDK text only for the documented exceptions.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum InputDefect {
    /// A refusal of the shared media layout or value checks.
    #[error(transparent)]
    Layout(LayoutDefect),

    /// Source name must be nonempty and contain no NUL.
    #[error("Source name must be nonempty and contain no NUL")]
    InvalidSourceName,
    /// A list element is empty, contains a comma, or contains a NUL.
    #[error("{setting} element {index} must be nonempty and contain neither a comma nor a NUL")]
    ListElement {
        /// The setting that contains the invalid element.
        setting: &'static str,
        /// The zero-based position in the list.
        index: usize,
    },
    /// Host must be a bare hostname, IP address or NDI machine name.
    #[error("Host must be a bare hostname, IP address or NDI machine name")]
    InvalidHostQuery,
    /// PTZ preset must be within 0..=99.
    #[error("PTZ preset must be within 0..=99")]
    PtzPreset,
    /// an explicit NDI library path must be absolute.
    #[error("an explicit NDI library path must be absolute")]
    RelativeLibraryPath,
    /// a listener accepts one Discovery Server endpoint.
    #[error("a listener accepts one Discovery Server endpoint")]
    MultipleDiscoveryServers,
    /// the registration was issued by another advertiser.
    #[error("the registration was issued by another advertiser")]
    ForeignRegistration,
    /// a recorder must record video, audio or metadata; an empty track selection would record all three.
    #[error("a recorder must record video, audio or metadata; an empty track selection would record all three")]
    EmptyRecordingTracks,
    /// a recorder with an exact file name cannot roll over at a format change, because the next file would have the same name; use a filename pattern with `{index}`, or leave format-change rollover unset.
    #[error("a recorder with an exact file name cannot roll over at a format change, because the next file would have the same name; use a filename pattern with {{index}}, or leave format-change rollover unset")]
    ExactNameFormatRollover,
    /// a recorder with an exact file name cannot roll over at a size limit, because the next file would have the same name; use a filename pattern with `{index}`.
    #[error("a recorder with an exact file name cannot roll over at a size limit, because the next file would have the same name; use a filename pattern with {{index}}")]
    ExactNameSizeRollover,
    /// unmatched filename-pattern brace.
    #[error("unmatched filename-pattern brace")]
    FilenamePatternBrace,
    /// A filename index would change the output directory.
    #[error(
        "filename-pattern indexes must be in the file name; move {{index}} out of the directory"
    )]
    FilenamePatternIndexInDirectory,
    /// file position exceeds 64-bit ticks.
    #[error("file position exceeds 64-bit ticks")]
    FilePositionOverflow,
    /// a recording start cannot use Timecode::SYNTHESIZE; choose an explicit start such as zero.
    #[error(
        "a recording start cannot use Timecode::SYNTHESIZE; choose an explicit start such as zero"
    )]
    SynthesizeRecordingStart,
    /// the session counter is exhausted.
    #[error("the session counter is exhausted")]
    SessionCounterExhausted,
    /// the recorder generation is exhausted.
    #[error("the recorder generation is exhausted")]
    RecorderGenerationExhausted,
    /// Allocator limits must be positive.
    #[error("Allocator limits must be positive")]
    AllocatorPositiveLimits,
    /// max_allocation_bytes exceeds 256 MiB.
    #[error("max_allocation_bytes exceeds 256 MiB")]
    AllocatorAllocationLimit,
    /// max_concurrent_calls exceeds max_allocations, which counts every call in progress.
    #[error("max_concurrent_calls exceeds max_allocations, which counts every call in progress")]
    AllocatorConcurrentLimit,
    /// max_bytes is less than max_concurrent_calls allocations of max_allocation_bytes.
    #[error("max_bytes is less than max_concurrent_calls allocations of max_allocation_bytes")]
    AllocatorByteLimit,
    /// Compressed allocations have a byte size, not a configurable stride.
    #[error("Compressed allocations have a byte size, not a configurable stride")]
    CompressedAllocationStride,
    /// External allocation is smaller than the request's minimum.
    #[error("External allocation is smaller than the request's minimum")]
    ExternalAllocationTooSmall,
    /// External allocation is larger than the request's capacity limit.
    #[error("External allocation is larger than the request's capacity limit")]
    ExternalAllocationTooLarge,
    /// External allocation is not aligned to 64 bytes.
    #[error("External allocation is not aligned to 64 bytes")]
    ExternalAllocationAlignment,
    /// Process allocator generation budget exhausted.
    #[error("Process allocator generation budget exhausted")]
    AllocatorGenerationBudget,
    /// Allocator token space exhausted.
    #[error("Allocator token space exhausted")]
    AllocatorTokensExhausted,
    /// Allocator limits differ from the existing process allocator service.
    #[error("Allocator limits differ from the existing process allocator service")]
    AllocatorLimitsDiffer,
    /// Advanced configuration exceeds its byte limit.
    #[error("Advanced configuration exceeds its byte limit")]
    ConfigurationTooLarge,
    /// Invalid Advanced configuration JSON.
    #[error("Invalid Advanced configuration JSON")]
    ConfigurationJson,
    /// Advanced configuration must be a JSON object.
    #[error("Advanced configuration must be a JSON object")]
    ConfigurationNotObject,
    /// queue requires 1..=65536 frames and a positive addressable byte limit.
    #[error("queue requires 1..=65536 frames and a positive addressable byte limit")]
    QueueLimits,
    /// queue submission identifiers exhausted.
    #[error("queue submission identifiers exhausted")]
    QueueIdentifiersExhausted,
    /// queued conversion requires a sender without advertiser registrations.
    #[error("queued conversion requires a sender without advertiser registrations")]
    QueuedSenderRegistered,
    /// AVSync request exceeds the supported audio storage limit.
    #[error("AVSync request exceeds the supported audio storage limit")]
    AvSyncStorageLimit,
    /// timeout {requested:?} exceeds MAX_TIMEOUT (4294967294 ms, about 49.7 days).
    #[error("timeout {requested:?} exceeds MAX_TIMEOUT (4294967294 ms, about 49.7 days)")]
    TimeoutTooLong {
        /// The requested.
        requested: DiagnosticDuration,
    },
    /// timeout {requested:?} ends at an instant this platform cannot represent.
    #[error("timeout {requested:?} ends at an instant this platform cannot represent")]
    TimeoutInstantOverflow {
        /// The requested.
        requested: DiagnosticDuration,
    },
    /// receive delay {requested:?} exceeds 2000 ms.
    #[error("receive delay {requested:?} exceeds 2000 ms")]
    ReceiveDelayTooLong {
        /// The requested.
        requested: DiagnosticDuration,
    },
    /// {name} must be finite and within {minimum}..={maximum}.
    #[error("{detail}")]
    PtzValue {
        /// Exact values, boxed only on 32-bit targets.
        detail: CompactDiagnostic<PtzDefect>,
    },
    /// {member} is already registered.
    #[error("{member} is already registered")]
    AlreadyRegistered {
        /// The member.
        member: &'static str,
    },
    /// {what} must not be empty.
    #[error("{what} must not be empty")]
    Empty {
        /// The what.
        what: &'static str,
    },
    /// {what} is not lossless UTF-8.
    #[error("{what} is not lossless UTF-8")]
    PathNotUtf8 {
        /// The what.
        what: &'static str,
    },
    /// unsupported filename-pattern token {{{token}}}.
    #[error("unsupported filename-pattern token {{{token}}}")]
    FilenamePatternToken {
        /// The token.
        token: Box<str>,
    },
    /// async {queue} queue capacity must be between 1 and 4096, not {capacity}.
    #[error("async {queue} queue capacity must be between 1 and 4096, not {capacity}")]
    FileQueueCapacity {
        /// The queue.
        queue: &'static str,
        /// The capacity.
        capacity: usize,
    },
    /// Allocator stride {stride} is smaller than the requested stride {minimum}.
    #[error("Allocator stride {stride} is smaller than the requested stride {minimum}")]
    AllocatorShortStride {
        /// The stride.
        stride: usize,
        /// The minimum.
        minimum: usize,
    },
    /// An allocation of {bytes} bytes exceeds the capacity limit of {limit} bytes.
    #[error("An allocation of {bytes} bytes exceeds the capacity limit of {limit} bytes")]
    AllocationTooLarge {
        /// The bytes.
        bytes: usize,
        /// The limit.
        limit: usize,
    },
    /// No allocation can hold {bytes} bytes.
    #[error("No allocation can hold {bytes} bytes")]
    AllocationLayout {
        /// The bytes.
        bytes: usize,
    },
    /// KVM key symbol {keysym:#x} exceeds the SDK's range, 0..=0x7fffffff.
    #[error("KVM key symbol {keysym:#x} exceeds the SDK's range, 0..=0x7fffffff")]
    KvmKeySymbol {
        /// The keysym.
        keysym: u32,
    },
    /// KVM position ({x}, {y}) is not within 0..=1 on each axis.
    #[error("KVM position ({x}, {y}) is not within 0..=1 on each axis")]
    KvmPosition {
        /// The x.
        x: f32,
        /// The y.
        y: f32,
    },
    /// KVM wheel units {units} are not finite.
    #[error("KVM wheel units {units} are not finite")]
    KvmWheel {
        /// The units.
        units: f32,
    },
    /// {count} KVM touch points are more than the SDK can count.
    #[error("{count} KVM touch points are more than the SDK can count")]
    KvmTouchCount {
        /// The count.
        count: usize,
    },
    /// frame retains {bytes} bytes of allocation capacity, exceeding the queue's {limit} byte limit.
    #[error("frame retains {bytes} bytes of allocation capacity, exceeding the queue's {limit} byte limit")]
    QueueFrameTooLarge {
        /// The bytes.
        bytes: usize,
        /// The limit.
        limit: usize,
    },
    /// {name} {value} is not in 1..=2147483647.
    #[error("{name} {value} is not in 1..=2147483647")]
    SdkQuantity {
        /// The name.
        name: &'static str,
        /// The value.
        value: u32,
    },
    /// {codec} quality must be in {minimum}..={maximum}, got {quality}.
    #[error("{codec} quality must be in {minimum}..={maximum}, got {quality}")]
    ImageQuality {
        /// The codec.
        codec: ImageCodecKind,
        /// The quality.
        quality: u8,
        /// The minimum.
        minimum: u8,
        /// The maximum.
        maximum: u8,
    },
    /// WebP effort must be in 0..=6, got {effort}.
    #[error("WebP effort must be in 0..=6, got {effort}")]
    WebpEffort {
        /// The effort.
        effort: u8,
    },
    /// {codec} images are 8 bits a channel; SampleDepth::Sixteen needs ImageFormat::Png.
    #[error("{codec} images are 8 bits a channel; SampleDepth::Sixteen needs ImageFormat::Png")]
    SixteenBitNeedsPng {
        /// The codec.
        codec: ImageCodecKind,
    },
    /// {codec} cannot say that its values are {transfer} code values; HdrPolicy::CodeValues needs ImageFormat::Png.
    #[error("{codec} cannot say that its values are {transfer} code values; HdrPolicy::CodeValues needs ImageFormat::Png")]
    HdrNeedsPng {
        /// The codec.
        codec: ImageCodecKind,
        /// The transfer.
        transfer: TransferFunction,
    },
    /// recording file path may reach {longest_bytes} UTF-8 bytes, exceeding the {maximum_bytes}-byte event path limit ({index_digits_reserved} digits reserved for index growth).
    #[error("recording file path may reach {longest_bytes} UTF-8 bytes, exceeding the {maximum_bytes}-byte event path limit ({index_digits_reserved} digits reserved for index growth)")]
    RecordingPathTooLong {
        /// The maximum bytes.
        maximum_bytes: usize,
        /// The longest bytes.
        longest_bytes: usize,
        /// The index digits reserved.
        index_digits_reserved: usize,
    },

    /// The SDK refused the supplied address; arbitrary address text is boxed.
    #[error("{0}")]
    DiscoveryAddressRejected(Box<DiscoveryAddressFailure>),
    /// No address was supplied or configured.
    #[error("the SDK created no {object}: no Discovery Server was named and the NDI configuration supplies none")]
    DiscoveryAddressMissing {
        /// The kind of object the SDK refused to create.
        object: &'static str,
    },
    /// SpeedHQ guidance was applied to another codec.
    #[cfg(feature = "advanced")]
    #[error("SpeedHQ quality guidance does not apply to {codec:?}")]
    SpeedHqCodec {
        /// The requested codec.
        codec: crate::advanced::VideoCodec,
    },
}
/// Typed causes of [`crate::Error::UnexpectedSdkResult`].
///
/// Ordinary causes retain their evidence inline without allocating. Boxed
/// diagnostic fields retain caller or SDK text only for the documented exceptions.
#[derive(Debug, Clone, PartialEq, Error, Copy, Eq, Hash)]
#[non_exhaustive]
pub enum SdkResultDefect {
    /// native completion protocol failed; use QueuedSender::close to recover retained frames.
    #[error(
        "native completion protocol failed; use QueuedSender::close to recover retained frames"
    )]
    UnknownCompletion,
    /// {operation} returned a negative count: {value}.
    #[error("{operation} returned a negative count: {value}")]
    NegativeCount {
        /// The operation.
        operation: crate::NativeFunction,
        /// The value.
        value: DiagnosticInteger,
    },
    /// {operation} returned a string longer than the 4 MiB one string may be.
    #[error("{operation} returned a string longer than the 4 MiB one string may be")]
    UnterminatedText {
        /// The operation.
        operation: crate::NativeFunction,
    },
    /// SDK returned invalid receive delay: {millis} ms.
    #[error("SDK returned invalid receive delay: {millis} ms")]
    InvalidReceiveDelay {
        /// The millis.
        millis: i32,
    },
    /// the source format reports a negative {what}: {value}.
    #[error("the source format reports a negative {what}: {value}")]
    NegativeFormatCount {
        /// The what.
        what: &'static str,
        /// The value.
        value: i32,
    },
    /// source format reports {count} video tracks; the SDK layout holds {capacity}.
    #[error("source format reports {count} video tracks; the SDK layout holds {capacity}")]
    FileTrackCount {
        /// The count.
        count: u32,
        /// The capacity.
        capacity: usize,
    },
    /// the File library wrote past the {bytes} bytes this crate's SDK headers give {name}.
    #[error("the File library wrote past the {bytes} bytes this crate's SDK headers give {name}")]
    FileStructureTooLarge {
        /// The bytes.
        bytes: usize,
        /// The name.
        name: &'static str,
    },
    /// {diagnostic}.
    #[error("{diagnostic}")]
    EventTypeMismatch {
        /// The diagnostic.
        diagnostic: &'static str,
    },
    /// {operation} returned {value}.
    #[error("{operation} returned {value}")]
    InvalidGuidance {
        /// The operation.
        operation: crate::NativeFunction,
        /// The value.
        value: i32,
    },
    /// NDIlib_avsync_synchronize reported a format change to {sample_rate} Hz and {channels} channels.
    #[error("NDIlib_avsync_synchronize reported a format change to {sample_rate} Hz and {channels} channels")]
    AvSyncFormatChange {
        /// The sample rate.
        sample_rate: i32,
        /// The channels.
        channels: i32,
    },
    /// {operation} returned unknown status {code}.
    #[error("{operation} returned unknown status {code}")]
    UnknownStatus {
        /// The operation.
        operation: crate::NativeFunction,
        /// The code.
        code: i32,
    },
    /// {operation} returned {kind}.
    #[error("{operation} returned {kind}")]
    UnexpectedFrameType {
        /// The operation.
        operation: crate::NativeFunction,
        /// The kind.
        kind: u32,
    },

    /// A native discovery descriptor violates its array contract.
    #[error("{0}")]
    MalformedDiscoveryData(DiscoveryDefect),
}

/// The image axis that exceeded an encoder limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum ImageAxisKind {
    /// Width.
    #[error("width")]
    Width,
    /// Height.
    #[error("height")]
    Height,
}

/// Exact diagnostic evidence, inline on 64-bit targets and boxed on 32-bit targets.
///
/// Only the wide cold-path causes use this holder. Boxing on 32-bit targets
/// preserves the existing compact `Error` budget while keeping every diagnostic
/// value. Common layout, timeout, HDR and withheld-metadata causes stay inline.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CompactDiagnostic<T> {
    #[cfg(target_pointer_width = "64")]
    value: T,
    #[cfg(not(target_pointer_width = "64"))]
    value: Box<T>,
}
impl<T> CompactDiagnostic<T> {
    /// Retain exact diagnostic evidence.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self {
            #[cfg(target_pointer_width = "64")]
            value,
            #[cfg(not(target_pointer_width = "64"))]
            value: Box::new(value),
        }
    }
    /// Recover the evidence without another allocation.
    #[must_use]
    pub fn into_inner(self) -> T {
        #[cfg(target_pointer_width = "64")]
        {
            self.value
        }
        #[cfg(not(target_pointer_width = "64"))]
        {
            *self.value
        }
    }
}
impl<T> std::ops::Deref for CompactDiagnostic<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T: std::fmt::Display> std::fmt::Display for CompactDiagnostic<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&**self, f)
    }
}

/// The audio count that failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum AudioCountKind {
    /// Channel count.
    #[error("channels")]
    Channels,
    /// Samples per channel.
    #[error("samples")]
    Samples,
}

/// Exact values reported by `FrameRate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[error("Invalid frame rate {numerator}/{denominator}: both parts must be in 1..=2147483647")]
pub struct FrameRateDefect {
    /// The numerator.
    pub numerator: i64,
    /// The denominator.
    pub denominator: i64,
}

/// Exact values reported by `CompressedVideoDimensions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[error("Compressed video dimensions {width}x{height} must each be in 1..=2147483647")]
pub struct VideoDimensionsDefect {
    /// The width.
    pub width: i64,
    /// The height.
    pub height: i64,
}

/// Exact values reported by `AudioDataLength`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[error("Audio data length {actual}, expected {expected} samples ({channels} channels x {samples} samples) for validated layout")]
pub struct AudioLengthDefect {
    /// The actual.
    pub actual: usize,
    /// The expected.
    pub expected: usize,
    /// The channels.
    pub channels: u32,
    /// The samples.
    pub samples: u32,
}

/// Exact values reported by `CompressedAudioQuantities`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[error("Compressed audio of {sample_rate} Hz, {channels} channels and {samples} samples per channel: each must be in 1..=2147483647")]
pub struct CompressedAudioDefect {
    /// The sample rate.
    pub sample_rate: i64,
    /// The channels.
    pub channels: i64,
    /// The samples.
    pub samples: i64,
}

/// Exact values reported by `PtzValue`.
#[derive(Debug, Clone, Copy, PartialEq, Error)]
#[error("{name} must be finite and within {minimum}..={maximum}")]
pub struct PtzDefect {
    /// The name.
    pub name: &'static str,
    /// The minimum.
    pub minimum: f32,
    /// The maximum.
    pub maximum: f32,
}

/// An exact signed 64-bit diagnostic value with 32-bit alignment.
///
/// Two words keep common layout refusals inline even on 32-bit MSVC, where an
/// `i64` field would enlarge every `Error`. Read the original value with `get`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiagnosticInteger([u32; 2]);
impl DiagnosticInteger {
    /// Retain all bits without allocating or depending on native i64 alignment.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        let bits = value as u64;
        Self([bits as u32, (bits >> 32) as u32])
    }
    /// The original signed value.
    #[must_use]
    pub const fn get(self) -> i64 {
        ((self.0[0] as u64) | ((self.0[1] as u64) << 32)) as i64
    }
}
impl From<i64> for DiagnosticInteger {
    fn from(value: i64) -> Self {
        Self::new(value)
    }
}
impl From<i32> for DiagnosticInteger {
    fn from(value: i32) -> Self {
        Self::new(value.into())
    }
}
impl From<u32> for DiagnosticInteger {
    fn from(value: u32) -> Self {
        Self::new(value.into())
    }
}
impl std::fmt::Display for DiagnosticInteger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.get(), f)
    }
}
impl std::fmt::Debug for DiagnosticInteger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.get(), f)
    }
}

/// Exact timeout evidence, stored in three 32-bit words without allocation.
///
/// `get` returns the original `Duration`; `Debug` uses its usual diagnostic
/// format. The stable alignment preserves the compact Error budget on MSVC.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiagnosticDuration {
    seconds: [u32; 2],
    nanoseconds: u32,
}
impl DiagnosticDuration {
    /// Retain a duration exactly without allocating.
    #[must_use]
    pub const fn new(value: Duration) -> Self {
        let seconds = value.as_secs();
        Self {
            seconds: [seconds as u32, (seconds >> 32) as u32],
            nanoseconds: value.subsec_nanos(),
        }
    }
    /// The original duration, reconstructed in aligned stack storage.
    #[must_use]
    pub const fn get(self) -> Duration {
        Duration::new(
            (self.seconds[0] as u64) | ((self.seconds[1] as u64) << 32),
            self.nanoseconds,
        )
    }
}
impl From<Duration> for DiagnosticDuration {
    fn from(value: Duration) -> Self {
        Self::new(value)
    }
}
impl PartialEq<Duration> for DiagnosticDuration {
    fn eq(&self, other: &Duration) -> bool {
        self.get() == *other
    }
}
impl PartialEq<DiagnosticDuration> for Duration {
    fn eq(&self, other: &DiagnosticDuration) -> bool {
        *self == other.get()
    }
}
impl std::fmt::Debug for DiagnosticDuration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.get(), f)
    }
}
