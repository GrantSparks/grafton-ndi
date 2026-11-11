//! Private, reviewed C ABI for the NDI File API, independent of installed headers.
//!
//! Derived from Processing.NDI.Lib.File.{Player,Recorder,Types}.h, SDK 6.4.0.2.
//! The original per-header notice is retained below. This does not license SDK
//! binaries or the SDK as a whole. Layouts are checked against genuine headers.
//
// Copyright (C) 2023-2026 Vizrt NDI AB. All rights reserved.
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
// of the Software, and to permit persons to whom the Software is furnished to do
// so, subject to the following conditions:
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use std::ffi::{c_char, c_void};
pub(super) type Handle = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct Source {
    pub p_ndi_name: *const c_char,
    pub p_url_address: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct VideoTrack {
    pub is_high_quality: bool,
    pub xres: i32,
    pub yres: i32,
    pub frame_rate_n: i32,
    pub frame_rate_d: i32,
    pub fourcc: u32,
    pub frame_format_type: i32,
    pub picture_aspect_ratio: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SourceFormat {
    pub video_track_count: u32,
    pub video_tracks: [VideoTrack; 2],
    pub audio_sample_rate: i32,
    pub audio_no_channels: i32,
    pub audio_no_samples: i32,
    pub audio_bit_depth: u32,
    pub audio_fourcc: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct PlayerOptions {
    pub p_ndi_send_name: *const c_char,
    pub p_groups: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct RecorderOptions {
    pub source_to_connect_to: Source,
    pub track_flags: i32,
    pub record_float_audio_planar_32f: bool,
    pub disable_auto_rollover_on_format_change: bool,
    pub p_output_filename_pattern: *const c_char,
    pub disable_proxy_track: bool,
    pub rollover_max_file_size_bytes: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct PlayerEvent {
    pub kind: i32,
    pub path: [c_char; 4096],
    pub message: [c_char; 1024],
    pub first_pts_100ns: u64,
    pub last_pts_100ns: u64,
    pub has_source_timecode: bool,
    pub source_timecode_100ns: i64,
    pub source_format: SourceFormat,
}
impl Default for PlayerEvent {
    fn default() -> Self {
        // All fields are integers, bools, float values or arrays of those; zero
        // is valid for each. SDK enum discriminants remain integers at the ABI.
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct RecorderEvent {
    pub kind: i32,
    pub path: [c_char; 4096],
    pub message: [c_char; 1024],
    pub track_flags: i32,
    pub dropped_packet_count: u64,
    pub first_pts_100ns: u64,
    pub last_pts_100ns: u64,
    pub source_format: [c_char; 1024],
    pub source_format_payload: SourceFormat,
}
impl Default for RecorderEvent {
    fn default() -> Self {
        // See PlayerEvent::default; no pointers/references or Rust enums here.
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FileInfo {
    pub duration_100ns: u64,
    pub duration_is_final: bool,
    pub track_count: u32,
    pub video_track_count: u32,
    pub audio_track_count: u32,
    pub metadata_track_count: u32,
    pub source_format: SourceFormat,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PlayerStats {
    pub video_frames_sent: [u64; 2],
    pub audio_frames_sent: u64,
    pub metadata_frames_sent: u64,
    pub playback_wallclock_100ns: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct RecorderStats {
    pub bytes_written: u64,
    pub video_frames: u64,
    pub audio_frames: u64,
    pub metadata_frames: u64,
    pub receiver_dropped_video_frames: u64,
    pub receiver_dropped_audio_frames: u64,
    pub receiver_dropped_metadata_frames: u64,
    pub pipeline_dropped_video_frames: u64,
    pub pipeline_dropped_audio_frames: u64,
    pub pipeline_dropped_metadata_frames: u64,
    pub writer_dropped_frames: u64,
    pub rollover_count: u64,
    pub recording_time_100ns: u64,
    pub last_video_pts_100ns: u64,
    pub last_audio_pts_100ns: u64,
    pub last_metadata_pts_100ns: u64,
}

#[repr(C)]
pub(super) struct Metadata {
    pub length: i32,
    pub timecode: i64,
    pub p_data: *mut c_char,
}

// One registry defines the shared/static signatures. Symbols are resolved as a
// complete lifecycle group before an object may be constructed.
macro_rules! file_functions {
    ($( $field:ident => $symbol:ident ($($arg:ident: $ty:ty),*) -> $ret:ty; )*) => {
        /// The File library's entry points, for the declaration coverage test.
        #[cfg(test)]
        pub(crate) const FUNCTION_NAMES: &[&str] = &[$(stringify!($symbol),)*];

        #[derive(Clone, Copy)]
        pub(super) struct Functions { $(pub $field: unsafe extern "C" fn($($ty),*) -> $ret,)* }
        impl Functions {
            #[cfg(not(ndi_link_static))]
            pub unsafe fn load(library: &libloading::Library) -> super::FileResult<Self> {
                Ok(Self { $($field: *unsafe { library.get::<unsafe extern "C" fn($($ty),*) -> $ret>(concat!(stringify!($symbol), "\0").as_bytes()) }
                    .map_err(|error| super::unavailable("file", format!("SymbolUnavailable: {}: {error}", stringify!($symbol))))?,)* })
            }
            #[cfg(test)]
            pub fn mocked() -> Self {
                #[allow(non_snake_case)]
                mod stubs {
                    use super::*;
                    $(pub unsafe extern "C" fn $symbol($($arg: $ty),*) -> $ret {
                        let _ = ($($arg,)*);
                        Default::default()
                    })*
                }
                Self { $($field: stubs::$symbol,)* }
            }
            #[cfg(ndi_file_static)]
            pub fn linked() -> Self { Self { $($field: linked::$symbol,)* } }
        }
        #[cfg(ndi_file_static)]
        mod linked {
            use super::*;
            unsafe extern "C" { $(pub fn $symbol($($arg: $ty),*) -> $ret;)* }
        }
    }
}

include!("functions.rs");
