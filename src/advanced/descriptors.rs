//! Frame descriptors as the SDK's own constructors leave them.
//!
//! The SDK's structures have default member values that only its C++
//! declarations state, and some of its functions read them: a request left
//! zeroed is not a default request. Every descriptor this module hands to the
//! SDK as input starts from one of these and then sets what it means to say.
//! Descriptors the SDK only fills in (the capture results) need no starting
//! value.

use crate::sys::bindings::*;

/// A video frame descriptor as the SDK constructs it: no picture, UYVY,
/// progressive, 30000/1001 frames per second and a timecode for the SDK to
/// synthesize.
pub(crate) fn video_frame() -> NDIlib_video_frame_v2_t {
    NDIlib_video_frame_v2_t {
        FourCC: NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVY,
        frame_rate_N: 30_000,
        frame_rate_D: 1001,
        frame_format_type: NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive,
        timecode: NDIlib_send_timecode_synthesize,
        ..Default::default()
    }
}

/// An audio frame descriptor as the SDK constructs it: no samples, planar
/// 32-bit floating point, 48 kHz stereo and a timecode for the SDK to
/// synthesize.
pub(crate) fn audio_frame() -> NDIlib_audio_frame_v3_t {
    NDIlib_audio_frame_v3_t {
        sample_rate: 48_000,
        no_channels: 2,
        timecode: NDIlib_send_timecode_synthesize,
        FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
        ..Default::default()
    }
}
