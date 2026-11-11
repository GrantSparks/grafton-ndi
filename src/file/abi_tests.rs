//! Reviewed ABI fixtures for the curated File transport records.
//!
//! The x86_64 Linux fixtures were independently measured with C11 sizeof,
//! _Alignof and offsetof against genuine SDK 6.4.0.2 headers (2026-11-06).
//! All 168 C/Rust size/alignment/field-offset/field-size values agreed. These
//! tests do not require the optional File headers on a consuming build machine.
//! They certify these fixtures, not an unmeasured target's native ABI. Release
//! validation also runs the native C comparison on each supported target.

use super::ffi;
use std::mem::{align_of, offset_of, size_of};

#[test]
fn pointer_descriptors_keep_both_native_slots() {
    let ptr = size_of::<*const std::ffi::c_char>();
    assert_eq!(size_of::<ffi::Source>(), 2 * ptr);
    assert_eq!(
        align_of::<ffi::Source>(),
        align_of::<*const std::ffi::c_char>()
    );
    assert_eq!(offset_of!(ffi::Source, p_ndi_name), 0);
    assert_eq!(offset_of!(ffi::Source, p_url_address), ptr);
    assert_eq!(size_of::<ffi::PlayerOptions>(), 2 * ptr);
    assert_eq!(offset_of!(ffi::PlayerOptions, p_ndi_send_name), 0);
    assert_eq!(offset_of!(ffi::PlayerOptions, p_groups), ptr);
}

#[test]
fn video_layout_contains_two_complete_fixed_width_tracks() {
    assert_eq!(size_of::<ffi::VideoTrack>(), 32);
    assert_eq!(align_of::<ffi::VideoTrack>(), 4);
    assert_eq!(size_of::<ffi::SourceFormat>(), 88);
    assert_eq!(align_of::<ffi::SourceFormat>(), 4);
    assert_eq!(offset_of!(ffi::SourceFormat, video_tracks), 4);
    assert_eq!(offset_of!(ffi::SourceFormat, audio_sample_rate), 68);
    assert_eq!(offset_of!(ffi::SourceFormat, audio_fourcc), 84);
}

#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_pointer_width = "64"
))]
mod measured_native64 {
    use super::*;

    fn pointed_size<T>(_: *const T) -> usize {
        size_of::<T>()
    }

    macro_rules! layout {
        ($test:ident, $ty:ident, $size:expr, $align:expr, { $($field:ident: ($offset:expr, $field_size:expr)),* $(,)? }) => {
            #[test]
            fn $test() {
                assert_eq!(size_of::<ffi::$ty>(), $size, concat!(stringify!($ty), " size"));
                assert_eq!(align_of::<ffi::$ty>(), $align, concat!(stringify!($ty), " alignment"));
                $(
                    assert_eq!(offset_of!(ffi::$ty, $field), $offset, concat!(stringify!($ty), ".", stringify!($field), " offset"));
                    let storage = std::mem::MaybeUninit::<ffi::$ty>::uninit();
                    // Only compute a field address in allocated, correctly aligned
                    // storage. No field value or reference is read/constructed.
                    let field = unsafe { std::ptr::addr_of!((*storage.as_ptr()).$field) };
                    assert_eq!(pointed_size(field), $field_size, concat!(stringify!($ty), ".", stringify!($field), " size"));
                )*
            }
        };
    }

    layout!(sdk_source, Source, 16, 8, {
        p_ndi_name: (0, 8),
        p_url_address: (8, 8),
    });

    layout!(sdk_video_track, VideoTrack, 32, 4, {
        is_high_quality: (0, 1),
        xres: (4, 4),
        yres: (8, 4),
        frame_rate_n: (12, 4),
        frame_rate_d: (16, 4),
        fourcc: (20, 4),
        frame_format_type: (24, 4),
        picture_aspect_ratio: (28, 4),
    });

    layout!(sdk_source_format, SourceFormat, 88, 4, {
        video_track_count: (0, 4),
        video_tracks: (4, 64),
        audio_sample_rate: (68, 4),
        audio_no_channels: (72, 4),
        audio_no_samples: (76, 4),
        audio_bit_depth: (80, 4),
        audio_fourcc: (84, 4),
    });

    layout!(sdk_player_options, PlayerOptions, 16, 8, {
        p_ndi_send_name: (0, 8),
        p_groups: (8, 8),
    });

    layout!(sdk_recorder_options, RecorderOptions, 48, 8, {
        source_to_connect_to: (0, 16),
        track_flags: (16, 4),
        record_float_audio_planar_32f: (20, 1),
        disable_auto_rollover_on_format_change: (21, 1),
        p_output_filename_pattern: (24, 8),
        disable_proxy_track: (32, 1),
        rollover_max_file_size_bytes: (40, 8),
    });

    layout!(sdk_player_event, PlayerEvent, 5248, 8, {
        kind: (0, 4),
        path: (4, 4096),
        message: (4100, 1024),
        first_pts_100ns: (5128, 8),
        last_pts_100ns: (5136, 8),
        has_source_timecode: (5144, 1),
        source_timecode_100ns: (5152, 8),
        source_format: (5160, 88),
    });

    layout!(sdk_recorder_event, RecorderEvent, 6264, 8, {
        kind: (0, 4),
        path: (4, 4096),
        message: (4100, 1024),
        track_flags: (5124, 4),
        dropped_packet_count: (5128, 8),
        first_pts_100ns: (5136, 8),
        last_pts_100ns: (5144, 8),
        source_format: (5152, 1024),
        source_format_payload: (6176, 88),
    });

    layout!(sdk_file_info, FileInfo, 120, 8, {
        duration_100ns: (0, 8),
        duration_is_final: (8, 1),
        track_count: (12, 4),
        video_track_count: (16, 4),
        audio_track_count: (20, 4),
        metadata_track_count: (24, 4),
        source_format: (28, 88),
    });

    layout!(sdk_player_stats, PlayerStats, 40, 8, {
        video_frames_sent: (0, 16),
        audio_frames_sent: (16, 8),
        metadata_frames_sent: (24, 8),
        playback_wallclock_100ns: (32, 8),
    });

    layout!(sdk_recorder_stats, RecorderStats, 128, 8, {
        bytes_written: (0, 8),
        video_frames: (8, 8),
        audio_frames: (16, 8),
        metadata_frames: (24, 8),
        receiver_dropped_video_frames: (32, 8),
        receiver_dropped_audio_frames: (40, 8),
        receiver_dropped_metadata_frames: (48, 8),
        pipeline_dropped_video_frames: (56, 8),
        pipeline_dropped_audio_frames: (64, 8),
        pipeline_dropped_metadata_frames: (72, 8),
        writer_dropped_frames: (80, 8),
        rollover_count: (88, 8),
        recording_time_100ns: (96, 8),
        last_video_pts_100ns: (104, 8),
        last_audio_pts_100ns: (112, 8),
        last_metadata_pts_100ns: (120, 8),
    });

    layout!(sdk_metadata, Metadata, 24, 8, {
        length: (0, 4),
        timecode: (8, 8),
        p_data: (16, 8),
    });
}
