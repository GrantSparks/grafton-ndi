//! Diagnostic goldens transcribed from the pre-migration constructors and
//! layout Display implementation at 99d423b, with intentional #219/#228
//! behavior changes covered explicitly. Inputs deliberately differ so
//! reordered fields or lost context cannot silently preserve a generic message.

use super::*;
use crate::{ColorInfo, FrameMetadataStatus, PixelFormat, TransferFunction};
use std::time::Duration;

fn color(metadata: &str) -> ColorInfo {
    crate::color_info::parse(Some(metadata)).unwrap().unwrap()
}

fn reserve_capacity_error() -> std::collections::TryReserveError {
    Vec::<u8>::new().try_reserve(usize::MAX).unwrap_err()
}

fn check_cases<E: std::fmt::Display>(name: &str, cases: &[(&str, E, &str)]) {
    for (variant, cause, expected) in cases {
        assert_eq!(cause.to_string(), *expected, "{name}::{variant}");
    }
    // Keep the hand-written goldens exhaustive as the public taxonomy grows.
    let source = if name == "InitializationFailure" {
        include_str!("../error.rs")
    } else {
        include_str!("defects.rs")
    };
    let declarations = syn::parse_file(source).unwrap();
    let declaration = declarations
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == name => Some(item),
            _ => None,
        })
        .unwrap();
    let mut expected: Vec<_> = declaration
        .variants
        .iter()
        .filter(|variant| variant.ident != "SpeedHqCodec" || cfg!(feature = "advanced"))
        .map(|variant| variant.ident.to_string())
        .collect();
    let mut actual: Vec<_> = cases
        .iter()
        .map(|(variant, _, _)| (*variant).to_owned())
        .collect();
    expected.sort();
    actual.sort();
    assert_eq!(
        actual, expected,
        "every {name} variant needs a diagnostic golden"
    );
}

#[test]
fn frame_defect_diagnostics_preserve_their_text() {
    let cases = [
        ("ImageScratchAllocation", FrameDefect::ImageScratchAllocation(reserve_capacity_error()), "image scratch allocation failed: memory allocation failed because the computed capacity exceeded the collection's maximum"),
        ("ImageScratchOverflow", FrameDefect::ImageScratchOverflow, "image scratch size overflow"),
        ("ImageDimensionOverflow", FrameDefect::ImageDimensionOverflow, "image dimension overflow"),
        ("ImageDimension", FrameDefect::ImageDimension { codec: ImageCodec::Jpeg, dimension: ImageDimension::Width, length: 6, maximum: 16 }, "JPEG width 6 exceeds maximum supported value 16"),
        ("FrameRate", FrameDefect::FrameRate { detail: CompactDiagnostic::new(FrameRateDefect { numerator: 30, denominator: 0 }) }, "Invalid frame rate 30/0: both parts must be in 1..=2147483647"),
        ("CompressedVideoDimensions", FrameDefect::CompressedVideoDimensions { detail: CompactDiagnostic::new(VideoDimensionsDefect { width: 8, height: 4 }) }, "Compressed video dimensions 8x4 must each be in 1..=2147483647"),
        ("VideoZeroStride", FrameDefect::VideoZeroStride, "Video line stride must be positive, got 0"),
        ("VideoZeroHeight", FrameDefect::VideoZeroHeight, "Video frame height must be positive, got 0"),
        ("VideoAllocationOverflow", FrameDefect::VideoAllocationOverflow, "Video send allocation size overflow"),
        ("VideoZeroSize", FrameDefect::VideoZeroSize, "Video frame has zero calculated size"),
        ("VideoBufferOverflow", FrameDefect::VideoBufferOverflow, "Video buffer size overflow"),
        ("VideoPayloadOverflow", FrameDefect::VideoPayloadOverflow, "Video payload size overflow"),
        ("AudioStrideRange", FrameDefect::AudioStrideRange, "Audio channel stride exceeds i32 range"),
        ("MetadataMissingTerminator", FrameDefect::MetadataMissingTerminator, "Metadata frame length does not include a trailing NUL terminator"),
        ("MetadataInteriorNul", FrameDefect::MetadataInteriorNul, "Metadata frame contains an interior NUL byte"),
        ("MetadataNullData", FrameDefect::MetadataNullData, "Metadata frame has non-zero length with null data pointer"),
        ("MetadataEmpty", FrameDefect::MetadataEmpty, "Metadata length must include a trailing NUL terminator"),
        ("MetadataLengthOverflow", FrameDefect::MetadataLengthOverflow, "Frame metadata length overflow while adding terminator"),
        ("NullVideoData", FrameDefect::NullVideoData, "Video frame has null data pointer"),
        ("NullAudioData", FrameDefect::NullAudioData, "Audio frame has null data pointer"),
        ("AudioMisaligned", FrameDefect::AudioMisaligned, "Audio data pointer is not aligned for f32"),
        ("EmptyAudioHasData", FrameDefect::EmptyAudioHasData, "Zero-length audio frame has non-null data pointer"),
        ("V210RowOverflow", FrameDefect::V210RowOverflow, "V210 active row size overflow"),
        ("V210Stride", FrameDefect::V210Stride, "V210 stride must be a multiple of four and cover every complete pixel group"),
        ("V210Storage", FrameDefect::V210Storage, "V210 storage exceeds the video limit"),
        ("V210Width", FrameDefect::V210Width, "V210 conversion requires a positive width divisible by six"),
        ("P216LayoutMismatch", FrameDefect::P216LayoutMismatch, "P216 frame format and dimensions must match the V210 layout"),
        ("PlanarAudioMismatch", FrameDefect::PlanarAudioMismatch, "planar destination rate, channels and samples must match the interleaved source"),
        ("PacketHeaderTruncated", FrameDefect::PacketHeaderTruncated, "Truncated compressed packet header"),
        ("PacketHeaderSize", FrameDefect::PacketHeaderSize, "Unsupported compressed packet header size"),
        ("PacketHeaderUnsupported", FrameDefect::PacketHeaderUnsupported, "Cannot send a packet with an unsupported header version or unknown flags"),
        ("PacketCodecMismatch", FrameDefect::PacketCodecMismatch, "Packet codec does not match the frame's codec"),
        ("EmptyCodecPayload", FrameDefect::EmptyCodecPayload, "Empty codec payload"),
        ("PacketLengthsMismatch", FrameDefect::PacketLengthsMismatch, "Packet header lengths do not match the size of the frame"),
        ("KeyframeConfigurationMissing", FrameDefect::KeyframeConfigurationMissing, "Keyframes require codec configuration data"),
        ("AacKeyframeMissing", FrameDefect::AacKeyframeMissing, "AAC packets must carry the keyframe flag"),
        ("AacConfigurationMissing", FrameDefect::AacConfigurationMissing, "AAC packets require their AudioSpecificConfig as codec configuration data"),
        ("AacRateMismatch", FrameDefect::AacRateMismatch, "The AAC configuration names a different sample rate than the frame"),
        ("AacChannelsMismatch", FrameDefect::AacChannelsMismatch, "The AAC configuration names a different channel layout than the frame"),
        ("PacketLength", FrameDefect::PacketLength, "Invalid compressed packet length"),
        ("PacketLengthOverflow", FrameDefect::PacketLengthOverflow, "Compressed packet length overflow"),
        ("PacketTruncated", FrameDefect::PacketTruncated, "Truncated compressed packet"),
        ("PacketSizeOverflow", FrameDefect::PacketSizeOverflow, "Packet size overflow"),
        ("PacketPayloadSize", FrameDefect::PacketPayloadSize, "Invalid compressed packet payload size"),
        ("UnknownCompressedVideo", FrameDefect::UnknownCompressedVideo, "Unknown compressed video FourCC"),
        ("SpeedHqQuality", FrameDefect::SpeedHqQuality, "A SpeedHQ frame begins with its quality, a byte below 100"),
        ("PreviewVideoDimensions", FrameDefect::PreviewVideoDimensions, "An H.264 or HEVC picture on the preview stream is at most 640 pixels wide and 640 high"),
        ("CompressedVideoSize", FrameDefect::CompressedVideoSize, "Invalid compressed video size"),
        ("VideoCodecMismatch", FrameDefect::VideoCodecMismatch, "Packet codec does not match video descriptor"),
        ("SpeedHqTruncated", FrameDefect::SpeedHqTruncated, "Truncated SpeedHQ frame"),
        ("OpusSampleRate", FrameDefect::OpusSampleRate, "Opus is sent at 8000, 12000, 16000, 24000 or 48000 Hz"),
        ("OpusDuration", FrameDefect::OpusDuration, "An Opus frame holds 2.5, 5, 10, 20, 40 or 60 ms of audio"),
        ("AacSampleCount", FrameDefect::AacSampleCount, "An AAC frame holds 1024 samples per channel"),
        ("AacSampleRate", FrameDefect::AacSampleRate, "AAC is sent at a sample rate its configuration can name, from 7350 to 96000 Hz"),
        ("AacChannels", FrameDefect::AacChannels, "AAC is sent with one to six or eight channels"),
        ("CompressedAudioSize", FrameDefect::CompressedAudioSize, "Invalid compressed audio size"),
        ("AacEnvelope", FrameDefect::AacEnvelope, "Invalid AAC packet envelope"),
        ("CompressedBuffer", FrameDefect::CompressedBuffer, "Invalid compressed frame buffer"),
        ("ScatterBlockCount", FrameDefect::ScatterBlockCount, "Invalid scatter block count"),
        ("EmptyScatterBlock", FrameDefect::EmptyScatterBlock, "Scatter blocks must not be empty"),
        ("ScatterSize", FrameDefect::ScatterSize, "Scatter frame exceeds the packet size limit"),
        ("ScatterHeader", FrameDefect::ScatterHeader, "The first scatter block must hold the frame's complete header"),
        ("UnknownCompressedAudio", FrameDefect::UnknownCompressedAudio, "Unknown compressed audio FourCC"),
        ("VideoWidth", FrameDefect::VideoWidth((-3).into()), "Video frame has invalid width: -3"),
        ("VideoOddWidth", FrameDefect::VideoOddWidth((-3).into()), "NDI video frames require even width, got -3"),
        ("VideoHeight", FrameDefect::VideoHeight((-3).into()), "Video frame has invalid height: -3"),
        ("VideoSdkStride", FrameDefect::VideoSdkStride(-3), "Uncompressed video frame has invalid line_stride_in_bytes: -3"),
        ("VideoShortStride", FrameDefect::VideoShortStride { detail: CompactDiagnostic::new(VideoStrideDefect { stride: 7, minimum: 12, format: PixelFormat::Bgra, width: 8 }) }, "Video line_stride_in_bytes 7 is smaller than minimum row size 12 for Bgra width 8"),
        ("VideoStrideOverflow", FrameDefect::VideoStrideOverflow { format: PixelFormat::Bgra, width: 8 }, "Video line stride overflow for Bgra width 8"),
        ("VideoBytes", FrameDefect::VideoBytes(7), "Video frame exceeds maximum size: 7 bytes > 268435456 bytes"),
        ("VideoPlanar420", FrameDefect::VideoPlanar420 { height: 4, stride: 7 }, "Planar 4:2:0 (YV12/I420) video frames require even height and line stride, got height 4 and stride 7"),
        ("VideoSemiPlanar420", FrameDefect::VideoSemiPlanar420(7), "Semi-planar 4:2:0 (NV12) video frames require even height, got 7"),
        ("VideoUyvaStride", FrameDefect::VideoUyvaStride(7), "UYVA video frames require even line stride, got 7"),
        ("PictureAspectRatio", FrameDefect::PictureAspectRatio(-1.5), "Video frame has invalid picture aspect ratio: -1.5"),
        ("AudioRate", FrameDefect::AudioRate((-3).into()), "Invalid sample rate: -3"),
        ("AudioCount", FrameDefect::AudioCount { value: (-3).into(), kind: AudioCountKind::Channels }, "Invalid number of channels: -3"),
        ("AudioStride", FrameDefect::AudioStride((-3).into()), "Invalid channel_stride_in_bytes: -3"),
        ("AudioStrideAlignment", FrameDefect::AudioStrideAlignment(7), "channel_stride_in_bytes must be a multiple of 4, got 7"),
        ("AudioShortStride", FrameDefect::AudioShortStride { stride: 7, minimum: 12 }, "channel_stride_in_bytes 7 is smaller than one channel of audio samples 12"),
        ("AudioStrideOverflow", FrameDefect::AudioStrideOverflow(7), "Audio channel stride overflow: 7 samples × 4 bytes"),
        ("AudioChannelOffset", FrameDefect::AudioChannelOffset { channels: 2, stride: 7 }, "Audio channel offset overflow: 2 channels × 7 stride samples"),
        ("AudioSampleCount", FrameDefect::AudioSampleCount { offset: 17, samples: 16 }, "Audio backing sample count overflow: channel offset 17 + 16 samples"),
        ("AudioByteSize", FrameDefect::AudioByteSize(7), "Audio byte size overflow: 7 samples × 4 bytes"),
        ("AudioBytes", FrameDefect::AudioBytes(7), "Audio frame exceeds maximum size: 7 bytes > 67108864 bytes"),
        ("VideoStrideRange", FrameDefect::VideoStrideRange { stride: 7 }, "Video line stride 7 exceeds i32 range"),
        ("UnknownScanType", FrameDefect::UnknownScanType { code: 1094861636 }, "Unknown scan type: 0x41424344"),
        ("AudioBufferTooSmall", FrameDefect::AudioBufferTooSmall { actual: 5, minimum: 12 }, "Audio buffer has 5 samples, requires at least 12"),
        ("MetadataNegativeLength", FrameDefect::MetadataNegativeLength { length: 6 }, "Metadata frame has negative length: 6"),
        ("MetadataInvalidLength", FrameDefect::MetadataInvalidLength { length: 6 }, "Invalid metadata length value: 6"),
        ("MetadataTooLarge", FrameDefect::MetadataTooLarge { length: 6 }, "Metadata exceeds maximum size: 6 bytes > 4194304 bytes"),
        ("MetadataSdkLength", FrameDefect::MetadataSdkLength { length: 6 }, "Metadata length 6 exceeds SDK i32 range"),
        ("MetadataUnterminated", FrameDefect::MetadataUnterminated, "Frame metadata is missing a NUL terminator within 4194304 bytes"),
        ("VideoDataLength", FrameDefect::VideoDataLength { actual: 5, expected: 12 }, "Video data length 5, expected 12 bytes for validated layout"),
        ("AudioDataLength", FrameDefect::AudioDataLength { detail: CompactDiagnostic::new(AudioLengthDefect { actual: 5, expected: 12, channels: 2, samples: 16 }) }, "Audio data length 5, expected 12 samples (2 channels x 16 samples) for validated layout"),
        ("EmptyAudioStride", FrameDefect::EmptyAudioStride { stride: 7 }, "Zero-length audio frame has non-zero channel_stride_in_bytes: 7"),
        ("EmptyAudioFormat", FrameDefect::EmptyAudioFormat { sample_rate: 48000, channels: 2 }, "Invalid zero-length audio state: sample_rate=48000, no_channels=2"),
        ("UnknownAudioFormat", FrameDefect::UnknownAudioFormat { fourcc: 1094861636 }, "Unknown audio format FourCC: 0x41424344"),
        ("UnknownPixelFormat", FrameDefect::UnknownPixelFormat { fourcc: 1094861636 }, "Unknown pixel format FourCC: 0x41424344"),
        ("FrameSyncAudioInput", FrameDefect::FrameSyncAudioInput { channels: 2, samples: 16 }, "FrameSync audio input-format query returned 2 channels of 16 samples"),
        ("VideoBufferTooSmall", FrameDefect::VideoBufferTooSmall { format: PixelFormat::Bgra, actual: 5, minimum: 12 }, "Buffer too small for Bgra: got 5 bytes, expected at least 12"),
        ("V210BufferTooSmall", FrameDefect::V210BufferTooSmall { actual: 5, minimum: 12 }, "V210 buffer has 5 words, requires 12"),
        ("P216BufferTooSmall", FrameDefect::P216BufferTooSmall { actual: 5, minimum: 12 }, "P216 buffer has 5 bytes, requires 12"),
        ("InterleavedAudioLength", FrameDefect::InterleavedAudioLength { actual: 5, channels: 2 }, "interleaved audio has 5 samples, which is not a non-zero multiple of 2 channels"),
        ("AudioDestinationLength", FrameDefect::AudioDestinationLength { actual: 5, minimum: 12 }, "audio destination has 5 samples, requires 12"),
        ("CompressedAudioQuantities", FrameDefect::CompressedAudioQuantities { detail: CompactDiagnostic::new(CompressedAudioDefect { sample_rate: 48000, channels: 2, samples: 16 }) }, "Compressed audio of 48000 Hz, 2 channels and 16 samples per channel: each must be in 1..=2147483647"),
        ("ColorMetadataWithheld", FrameDefect::ColorMetadataWithheld { status: FrameMetadataStatus::NotUtf8 }, "The frame's color description cannot be read because its metadata was withheld (NotUtf8); ColorPolicy::Override explicitly supplies its color conversion"),
        ("ConflictingColorMetadata", FrameDefect::ConflictingColorMetadata { first: color("<ndi_color_info transfer='bt_709'/>"), second: color("<ndi_color_info transfer='bt_2100_hlg'/>") }, "The frame's metadata holds ndi_color_info elements that differ: ColorInfo { primaries: None, transfer: Some(Bt709), matrix: None } and ColorInfo { primaries: None, transfer: Some(Hlg), matrix: None }"),
        ("HdrRefused", FrameDefect::HdrRefused { transfer: TransferFunction::Pq }, "The frame's ndi_color_info gives the PQ transfer function; HdrPolicy::CodeValues encodes its code values to a tagged PNG"),
        ("ColorMetadata", FrameDefect::ColorMetadata { issue: ColorMetadataIssue::UndefinedValue, diagnostic: "peer diagnostic".into() }, "peer diagnostic"),
        ("EncodingFailed", FrameDefect::EncodingFailed { codec: ImageCodec::Jpeg, diagnostic: "peer diagnostic".into() }, "JPEG encoding failed: peer diagnostic"),
    ];
    check_cases("FrameDefect", &cases);
}

#[test]
fn input_defect_diagnostics_preserve_their_text() {
    let cases = [
        ("VideoZeroSize", InputDefect::VideoZeroSize, "Video frame has zero calculated size"),
        ("VideoBufferOverflow", InputDefect::VideoBufferOverflow, "Video buffer size overflow"),
        ("VideoPayloadOverflow", InputDefect::VideoPayloadOverflow, "Video payload size overflow"),
        ("AudioStrideRange", InputDefect::AudioStrideRange, "Audio channel stride exceeds i32 range"),
        ("FrameRate", InputDefect::FrameRate { detail: CompactDiagnostic::new(FrameRateDefect { numerator: 30, denominator: 0 }) }, "Invalid frame rate 30/0: both parts must be in 1..=2147483647"),
        ("CompressedVideoDimensions", InputDefect::CompressedVideoDimensions { detail: CompactDiagnostic::new(VideoDimensionsDefect { width: 8, height: 4 }) }, "Compressed video dimensions 8x4 must each be in 1..=2147483647"),
        ("InvalidSourceName", InputDefect::InvalidSourceName, "Source name must be nonempty and contain no NUL"),
        ("InvalidHostQuery", InputDefect::InvalidHostQuery, "Host must be a bare hostname, IP address or NDI machine name"),
        ("PtzPreset", InputDefect::PtzPreset, "PTZ preset must be within 0..=99"),
        ("RelativeLibraryPath", InputDefect::RelativeLibraryPath, "an explicit NDI library path must be absolute"),
        ("MultipleDiscoveryServers", InputDefect::MultipleDiscoveryServers, "a listener accepts one Discovery Server endpoint"),
        ("ForeignRegistration", InputDefect::ForeignRegistration, "the registration was issued by another advertiser"),
        ("EmptyRecordingTracks", InputDefect::EmptyRecordingTracks, "a recorder must record video, audio or metadata; an empty track selection would record all three"),
        ("ExactNameFormatRollover", InputDefect::ExactNameFormatRollover, "a recorder with an exact file name cannot roll over at a format change, because the next file would have the same name; use a filename pattern with {index}, or leave format-change rollover unset"),
        ("ExactNameSizeRollover", InputDefect::ExactNameSizeRollover, "a recorder with an exact file name cannot roll over at a size limit, because the next file would have the same name; use a filename pattern with {index}"),
        ("FilenamePatternBrace", InputDefect::FilenamePatternBrace, "unmatched filename-pattern brace"),
        ("FilePositionOverflow", InputDefect::FilePositionOverflow, "file position exceeds 64-bit ticks"),
        ("SynthesizeRecordingStart", InputDefect::SynthesizeRecordingStart, "a recording start cannot use Timecode::SYNTHESIZE; choose an explicit start such as zero"),
        ("SessionCounterExhausted", InputDefect::SessionCounterExhausted, "the session counter is exhausted"),
        ("RecorderGenerationExhausted", InputDefect::RecorderGenerationExhausted, "the recorder generation is exhausted"),
        ("AllocatorPositiveLimits", InputDefect::AllocatorPositiveLimits, "Allocator limits must be positive"),
        ("AllocatorAllocationLimit", InputDefect::AllocatorAllocationLimit, "max_allocation_bytes exceeds 256 MiB"),
        ("AllocatorConcurrentLimit", InputDefect::AllocatorConcurrentLimit, "max_concurrent_calls exceeds max_allocations, which counts every call in progress"),
        ("AllocatorByteLimit", InputDefect::AllocatorByteLimit, "max_bytes is less than max_concurrent_calls allocations of max_allocation_bytes"),
        ("AllocatorAudioExtentOverflow", InputDefect::AllocatorAudioExtentOverflow, "Audio allocator extent overflow"),
        ("CompressedAllocationStride", InputDefect::CompressedAllocationStride, "Compressed allocations have a byte size, not a configurable stride"),
        ("ExternalAllocationTooSmall", InputDefect::ExternalAllocationTooSmall, "External allocation is smaller than the request's minimum"),
        ("ExternalAllocationTooLarge", InputDefect::ExternalAllocationTooLarge, "External allocation is larger than the request's capacity limit"),
        ("ExternalAllocationAlignment", InputDefect::ExternalAllocationAlignment, "External allocation is not aligned to 64 bytes"),
        ("AllocatorLeaseStorage", InputDefect::AllocatorLeaseStorage, "Unable to reserve bounded allocator lease storage"),
        ("AllocatorGenerationBudget", InputDefect::AllocatorGenerationBudget, "Process allocator generation budget exhausted"),
        ("AllocatorTokensExhausted", InputDefect::AllocatorTokensExhausted, "Allocator token space exhausted"),
        ("AllocatorLimitsDiffer", InputDefect::AllocatorLimitsDiffer, "Allocator limits differ from the existing process allocator service"),
        ("ConfigurationTooLarge", InputDefect::ConfigurationTooLarge, "Advanced configuration exceeds its byte limit"),
        ("ConfigurationJson", InputDefect::ConfigurationJson, "Invalid Advanced configuration JSON"),
        ("ConfigurationNotObject", InputDefect::ConfigurationNotObject, "Advanced configuration must be a JSON object"),
        ("ConfigurationNul", InputDefect::ConfigurationNul, "Advanced configuration contains NUL"),
        ("QueueLimits", InputDefect::QueueLimits, "queue requires 1..=65536 frames and a positive addressable byte limit"),
        ("QueueIdentifiersExhausted", InputDefect::QueueIdentifiersExhausted, "queue submission identifiers exhausted"),
        ("QueuedSenderRegistered", InputDefect::QueuedSenderRegistered, "queued conversion requires a sender without advertiser registrations"),
        ("CompletionStorage", InputDefect::CompletionStorage, "unable to allocate bounded completion storage"),
        ("AvSyncStorageLimit", InputDefect::AvSyncStorageLimit, "AVSync request exceeds the supported audio storage limit"),
        ("VideoWidth", InputDefect::VideoWidth((-3).into()), "Video frame has invalid width: -3"),
        ("VideoOddWidth", InputDefect::VideoOddWidth((-3).into()), "NDI video frames require even width, got -3"),
        ("VideoHeight", InputDefect::VideoHeight((-3).into()), "Video frame has invalid height: -3"),
        ("VideoSdkStride", InputDefect::VideoSdkStride(-3), "Uncompressed video frame has invalid line_stride_in_bytes: -3"),
        ("VideoShortStride", InputDefect::VideoShortStride { detail: CompactDiagnostic::new(VideoStrideDefect { stride: 7, minimum: 12, format: PixelFormat::Bgra, width: 8 }) }, "Video line_stride_in_bytes 7 is smaller than minimum row size 12 for Bgra width 8"),
        ("VideoStrideOverflow", InputDefect::VideoStrideOverflow { format: PixelFormat::Bgra, width: 8 }, "Video line stride overflow for Bgra width 8"),
        ("VideoBytes", InputDefect::VideoBytes(7), "Video frame exceeds maximum size: 7 bytes > 268435456 bytes"),
        ("VideoPlanar420", InputDefect::VideoPlanar420 { height: 4, stride: 7 }, "Planar 4:2:0 (YV12/I420) video frames require even height and line stride, got height 4 and stride 7"),
        ("VideoSemiPlanar420", InputDefect::VideoSemiPlanar420(7), "Semi-planar 4:2:0 (NV12) video frames require even height, got 7"),
        ("VideoUyvaStride", InputDefect::VideoUyvaStride(7), "UYVA video frames require even line stride, got 7"),
        ("PictureAspectRatio", InputDefect::PictureAspectRatio(-1.5), "Video frame has invalid picture aspect ratio: -1.5"),
        ("AudioRate", InputDefect::AudioRate((-3).into()), "Invalid sample rate: -3"),
        ("AudioCount", InputDefect::AudioCount { value: (-3).into(), kind: AudioCountKind::Channels }, "Invalid number of channels: -3"),
        ("AudioStride", InputDefect::AudioStride((-3).into()), "Invalid channel_stride_in_bytes: -3"),
        ("AudioStrideAlignment", InputDefect::AudioStrideAlignment(7), "channel_stride_in_bytes must be a multiple of 4, got 7"),
        ("AudioShortStride", InputDefect::AudioShortStride { stride: 7, minimum: 12 }, "channel_stride_in_bytes 7 is smaller than one channel of audio samples 12"),
        ("AudioStrideOverflow", InputDefect::AudioStrideOverflow(7), "Audio channel stride overflow: 7 samples × 4 bytes"),
        ("AudioChannelOffset", InputDefect::AudioChannelOffset { channels: 2, stride: 7 }, "Audio channel offset overflow: 2 channels × 7 stride samples"),
        ("AudioSampleCount", InputDefect::AudioSampleCount { offset: 17, samples: 16 }, "Audio backing sample count overflow: channel offset 17 + 16 samples"),
        ("AudioByteSize", InputDefect::AudioByteSize(7), "Audio byte size overflow: 7 samples × 4 bytes"),
        ("AudioBytes", InputDefect::AudioBytes(7), "Audio frame exceeds maximum size: 7 bytes > 67108864 bytes"),
        ("VideoStrideRange", InputDefect::VideoStrideRange { stride: 7 }, "Video line stride 7 exceeds i32 range"),
        ("TimeoutTooLong", InputDefect::TimeoutTooLong { requested: Duration::from_millis(4_294_967_295).into() }, "timeout 4294967.295s exceeds MAX_TIMEOUT (4294967294 ms, about 49.7 days)"),
        ("TimeoutInstantOverflow", InputDefect::TimeoutInstantOverflow { requested: Duration::from_millis(4_294_967_295).into() }, "timeout 4294967.295s ends at an instant this platform cannot represent"),
        ("ReceiveDelayTooLong", InputDefect::ReceiveDelayTooLong { requested: Duration::from_millis(4_294_967_295).into() }, "receive delay 4294967.295s exceeds 2000 ms"),
        ("PtzValue", InputDefect::PtzValue { detail: CompactDiagnostic::new(PtzDefect { name: "quantity", minimum: 0.0, maximum: 1.0 }) }, "quantity must be finite and within 0..=1"),
        ("AlreadyRegistered", InputDefect::AlreadyRegistered { member: "receiver" }, "receiver is already registered"),
        ("Empty", InputDefect::Empty { what: "file path" }, "file path must not be empty"),
        ("PathNotUtf8", InputDefect::PathNotUtf8 { what: "file path" }, "file path is not lossless UTF-8"),
        ("FilenamePatternToken", InputDefect::FilenamePatternToken { token: "unknown".into() }, "unsupported filename-pattern token {unknown}"),
        ("FileQueueCapacity", InputDefect::FileQueueCapacity { queue: "command", capacity: 2 }, "async command queue capacity must be between 1 and 4096, not 2"),
        ("AllocatorShortStride", InputDefect::AllocatorShortStride { stride: 7, minimum: 12 }, "Allocator stride 7 is smaller than the requested stride 12"),
        ("AllocatorAudioStride", InputDefect::AllocatorAudioStride { stride: 7 }, "Audio allocator stride 7 is not a whole number of samples"),
        ("AllocationTooLarge", InputDefect::AllocationTooLarge { bytes: 268435457, limit: 268435456 }, "An allocation of 268435457 bytes exceeds the capacity limit of 268435456 bytes"),
        ("AllocationLayout", InputDefect::AllocationLayout { bytes: 268435457 }, "No allocation can hold 268435457 bytes"),
        ("AllocationFailed", InputDefect::AllocationFailed { bytes: 268435457 }, "The global allocator did not supply 268435457 bytes"),
        ("KvmKeySymbol", InputDefect::KvmKeySymbol { keysym: 2147483648 }, "KVM key symbol 0x80000000 exceeds the SDK's range, 0..=0x7fffffff"),
        ("KvmPosition", InputDefect::KvmPosition { x: 1.25, y: -0.5 }, "KVM position (1.25, -0.5) is not within 0..=1 on each axis"),
        ("KvmWheel", InputDefect::KvmWheel { units: 2.5 }, "KVM wheel units 2.5 are not finite"),
        ("KvmTouchCount", InputDefect::KvmTouchCount { count: 3 }, "3 KVM touch points are more than the SDK can count"),
        ("QueueFrameTooLarge", InputDefect::QueueFrameTooLarge { bytes: 268435457, limit: 268435456 }, "frame retains 268435457 bytes of allocation capacity, exceeding the queue's 268435456 byte limit"),
        ("SdkQuantity", InputDefect::SdkQuantity { name: "quantity", value: 3 }, "quantity 3 is not in 1..=2147483647"),
        ("ImageQuality", InputDefect::ImageQuality { format: "JPEG", quality: 101, minimum: 1, maximum: 100 }, "JPEG quality must be in 1..=100, got 101"),
        ("WebpEffort", InputDefect::WebpEffort { effort: 7 }, "WebP effort must be in 0..=6, got 7"),
        ("SixteenBitNeedsPng", InputDefect::SixteenBitNeedsPng { format: "JPEG" }, "JPEG images are 8 bits a channel; SampleDepth::Sixteen needs ImageFormat::Png"),
        ("HdrNeedsPng", InputDefect::HdrNeedsPng { format: "JPEG", transfer: TransferFunction::Pq }, "JPEG cannot say that its values are PQ code values; HdrPolicy::CodeValues needs ImageFormat::Png"),
        ("RecordingPathTooLong", InputDefect::RecordingPathTooLong { maximum_bytes: 4094, longest_bytes: 4095, index_digits_reserved: 17 }, "recording file path may reach 4095 UTF-8 bytes, exceeding the 4094-byte event path limit (17 digits reserved for index growth)"),
        ("DiscoveryAddressRejected", InputDefect::DiscoveryAddressRejected(Box::new(DiscoveryAddressFailure { object: "advertiser", address: "server:5960".into() })), "the SDK created no advertiser: it does not accept \"server:5960\" as a Discovery Server address"),
        ("DiscoveryAddressMissing", InputDefect::DiscoveryAddressMissing { object: "advertiser" }, "the SDK created no advertiser: no Discovery Server was named and the NDI configuration supplies none"),
        #[cfg(feature = "advanced")]
        ("SpeedHqCodec", InputDefect::SpeedHqCodec { codec: crate::advanced::VideoCodec::H264 }, "SpeedHQ quality guidance does not apply to H264"),
    ];
    check_cases("InputDefect", &cases);
}

#[test]
fn sdk_result_defect_diagnostics_preserve_their_text() {
    let cases = [
        ("DiscoveryAllocation", SdkResultDefect::DiscoveryAllocation, "could not reserve memory for discovery results"),
        ("UnknownCompletion", SdkResultDefect::UnknownCompletion, "native completion protocol failed; use QueuedSender::close to recover retained frames"),
        ("FileTrackCountOverflow", SdkResultDefect::FileTrackCountOverflow, "video track count does not fit usize"),
        ("NegativeCount", SdkResultDefect::NegativeCount { operation: "native operation", value: -3 }, "native operation returned a negative count: -3"),
        ("UnterminatedText", SdkResultDefect::UnterminatedText { operation: "native operation" }, "native operation returned a string longer than the 4 MiB one string may be"),
        ("UnterminatedReceiverText", SdkResultDefect::UnterminatedReceiverText, "SDK string is missing a NUL terminator within 4194304 bytes"),
        ("InvalidReceiveDelay", SdkResultDefect::InvalidReceiveDelay { millis: -3 }, "SDK returned invalid receive delay: -3 ms"),
        ("NegativeFormatCount", SdkResultDefect::NegativeFormatCount { what: "file path", value: -3 }, "the source format reports a negative file path: -3"),
        ("FileTrackCount", SdkResultDefect::FileTrackCount { count: 3, capacity: 2 }, "source format reports 3 video tracks; the SDK layout holds 2"),
        ("FileStructureTooLarge", SdkResultDefect::FileStructureTooLarge { bytes: 268435457, name: "quantity" }, "the File library wrote past the 268435457 bytes this crate's SDK headers give quantity"),
        ("EventTypeMismatch", SdkResultDefect::EventTypeMismatch { diagnostic: "event types disagree" }, "event types disagree"),
        ("InvalidGuidance", SdkResultDefect::InvalidGuidance { operation: "native operation", value: -3 }, "native operation returned -3"),
        ("AvSyncFormatChange", SdkResultDefect::AvSyncFormatChange { sample_rate: 48000, channels: 2 }, "NDIlib_avsync_synchronize reported a format change to 48000 Hz and 2 channels"),
        ("UnknownStatus", SdkResultDefect::UnknownStatus { operation: "native operation", code: -3 }, "native operation returned unknown status -3"),
        ("UnexpectedFrameType", SdkResultDefect::UnexpectedFrameType { operation: "native operation", kind: u32::MAX - 2 }, "native operation returned 4294967293"),
        ("MalformedDiscoveryData", SdkResultDefect::MalformedDiscoveryData(DiscoveryDefect::NullArray), "SDK discovery array is null with a nonzero count"),
    ];
    check_cases("SdkResultDefect", &cases);
}

#[test]
fn discovery_defect_diagnostics_preserve_their_text() {
    let cases = [
        (
            "ArraySpanOverflow",
            DiscoveryDefect::ArraySpanOverflow,
            "SDK discovery array span overflow",
        ),
        (
            "UnterminatedString",
            DiscoveryDefect::UnterminatedString,
            "SDK discovery string exceeds its 4 MiB bound",
        ),
        (
            "NullArray",
            DiscoveryDefect::NullArray,
            "SDK discovery array is null with a nonzero count",
        ),
        (
            "ArrayCountOverflow",
            DiscoveryDefect::ArrayCountOverflow,
            "SDK discovery array count overflow",
        ),
    ];
    check_cases("DiscoveryDefect", &cases);
}

#[test]
fn image_codec_diagnostics_preserve_their_text() {
    let cases = [
        ("Png", ImageCodec::Png, "PNG"),
        ("Jpeg", ImageCodec::Jpeg, "JPEG"),
        ("Webp", ImageCodec::Webp, "WebP"),
    ];
    check_cases("ImageCodec", &cases);
}

#[test]
fn image_dimension_diagnostics_preserve_their_text() {
    let cases = [
        ("Width", ImageDimension::Width, "width"),
        ("Height", ImageDimension::Height, "height"),
    ];
    check_cases("ImageDimension", &cases);
}

#[test]
fn initialization_diagnostics_preserve_their_text() {
    let cases = [
        (
            "NotStarted",
            InitializationFailure::NotStarted,
            "NDIlib_initialize returned false on a CPU the runtime reports as supported",
        ),
        (
            "HandleCountExhausted",
            InitializationFailure::HandleCountExhausted,
            "NDI runtime reference count exhausted",
        ),
    ];
    check_cases("InitializationFailure", &cases);
}

#[test]
fn color_metadata_issue_diagnostics_preserve_supplied_peer_text() {
    for issue in [
        ColorMetadataIssue::UnclosedElement,
        ColorMetadataIssue::AttributeStart,
        ColorMetadataIssue::MissingValue,
        ColorMetadataIssue::UnquotedValue,
        ColorMetadataIssue::UnclosedValue,
        ColorMetadataIssue::DuplicateAttribute,
        ColorMetadataIssue::UndefinedValue,
    ] {
        let cause = FrameDefect::ColorMetadata {
            issue,
            diagnostic: "peer's original diagnostic".into(),
        };
        assert_eq!(cause.to_string(), "peer's original diagnostic");
    }
}

#[test]
fn audio_count_kind_diagnostics_preserve_their_text() {
    check_cases(
        "AudioCountKind",
        &[
            ("Channels", AudioCountKind::Channels, "channels"),
            ("Samples", AudioCountKind::Samples, "samples"),
        ],
    );
}

#[test]
fn compact_scalar_evidence_roundtrips_without_changing_its_diagnostic() {
    for (value, text) in [
        (i64::MIN, "-9223372036854775808"),
        (-1, "-1"),
        (0, "0"),
        (u32::MAX as i64, "4294967295"),
        (i64::MAX, "9223372036854775807"),
    ] {
        for evidence in [DiagnosticInteger::new(value), value.into()] {
            assert_eq!(evidence.get(), value);
            assert_eq!(evidence.to_string(), text);
            assert_eq!(format!("{evidence:?}"), text);
        }
    }
    assert_eq!(DiagnosticInteger::from(i32::MIN).get(), i64::from(i32::MIN));
    assert_eq!(DiagnosticInteger::from(u32::MAX).get(), i64::from(u32::MAX));
    for (value, text) in [
        (Duration::ZERO, "0ns"),
        (Duration::from_nanos(1), "1ns"),
        (Duration::from_millis(1500), "1.5s"),
        (
            Duration::new(u64::MAX, 999_999_999),
            "18446744073709551615.999999999s",
        ),
    ] {
        for evidence in [DiagnosticDuration::new(value), value.into()] {
            assert_eq!(evidence.get(), value);
            assert_eq!(format!("{evidence:?}"), text);
        }
    }
}
