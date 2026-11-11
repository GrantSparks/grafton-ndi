// Shared File operation registry. The reviewed ABI provenance is in ffi.rs.
file_functions! {
    player_probe => NDIlib_file_player_probe_file(path: *const c_char, out: *mut FileInfo) -> bool;
    player_create => NDIlib_file_player_create(options: *const PlayerOptions) -> Handle;
    player_destroy => NDIlib_file_player_destroy(handle: Handle) -> ();
    player_start => NDIlib_file_player_start(handle: Handle, path: *const c_char) -> bool;
    player_stop => NDIlib_file_player_stop(handle: Handle) -> ();
    player_running => NDIlib_file_player_is_running(handle: Handle) -> bool;
    player_capture => NDIlib_file_player_capture_event(handle: Handle, out: *mut PlayerEvent, timeout: u32) -> i32;
    player_dropped => NDIlib_file_player_get_dropped_event_count(handle: Handle) -> u64;
    player_stats => NDIlib_file_player_get_stats(handle: Handle, out: *mut PlayerStats) -> bool;
    player_seek_time => NDIlib_file_player_seek_to_time(handle: Handle, ticks: u64) -> bool;
    player_seek_frame => NDIlib_file_player_seek_to_frame(handle: Handle, frame: u64) -> bool;
    player_set_paused => NDIlib_file_player_set_paused(handle: Handle, paused: bool) -> ();
    player_get_paused => NDIlib_file_player_get_paused(handle: Handle) -> bool;
    player_set_loop => NDIlib_file_player_set_loop(handle: Handle, looping: bool) -> ();
    player_get_loop => NDIlib_file_player_get_loop(handle: Handle) -> bool;
    player_duration => NDIlib_file_player_get_current_duration(handle: Handle, ticks: *mut u64, final_value: *mut bool) -> bool;
    player_position => NDIlib_file_player_get_current_position(handle: Handle, ticks: *mut u64) -> bool;
    player_timecode => NDIlib_file_player_get_current_source_timecode(handle: Handle, ticks: *mut i64) -> bool;
    player_format => NDIlib_file_player_get_source_format(handle: Handle, out: *mut SourceFormat) -> bool;
    player_source => NDIlib_file_player_get_advertised_source(handle: Handle) -> *const Source;
    recorder_create => NDIlib_file_recorder_create(options: *const RecorderOptions) -> Handle;
    recorder_destroy => NDIlib_file_recorder_destroy(handle: Handle) -> ();
    recorder_start => NDIlib_file_recorder_start(handle: Handle, prefix: *const c_char) -> bool;
    recorder_capture => NDIlib_file_recorder_capture_event(handle: Handle, out: *mut RecorderEvent, timeout: u32) -> i32;
    recorder_dropped => NDIlib_file_recorder_get_dropped_event_count(handle: Handle) -> u64;
    recorder_stop => NDIlib_file_recorder_stop(handle: Handle) -> ();
    recorder_rollover => NDIlib_file_recorder_rollover(handle: Handle, prefix: *const c_char) -> bool;
    recorder_inject => NDIlib_file_recorder_inject_metadata(handle: Handle, metadata: *const Metadata) -> bool;
    recorder_set_source => NDIlib_file_recorder_set_source(handle: Handle, source: *const Source) -> bool;
    recorder_get_source => NDIlib_file_recorder_get_source(handle: Handle, out: *mut Source) -> bool;
    recorder_running => NDIlib_file_recorder_is_recording(handle: Handle) -> bool;
    recorder_current_file => NDIlib_file_recorder_get_current_file(handle: Handle, path: *mut *const c_char) -> bool;
    recorder_stats => NDIlib_file_recorder_get_stats(handle: Handle, out: *mut RecorderStats) -> bool;
    recorder_format => NDIlib_file_recorder_get_source_format(handle: Handle, out: *mut SourceFormat) -> bool;
    recorder_start_timecode => NDIlib_file_recorder_start_with_timecode(handle: Handle, prefix: *const c_char, timecode: i64) -> bool;
}
