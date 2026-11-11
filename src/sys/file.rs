//! The NDI File library: its typed entry points and the module that provides them.
//!
//! The File library accompanies the core runtime. Shared targets find it with the
//! core runtime's own search, starting beside the selected core module, and pin
//! it for the life of the process. Statically linked targets resolve the same
//! tables from the archive linked at build time.

use std::{ffi::CStr, path::Path};

#[cfg(ndi_link_static)]
use super::loader::Linked;
#[cfg(not(ndi_link_static))]
use super::loader::{confirm_selected, Module, Search};
use super::{
    bindings::*,
    loader::reason,
    selection::Selection,
    table::{native_table, Resolver},
    Api,
};
use crate::{capabilities::RuntimeInfo, Error, Result};

native_table! {
    /// Probing a recording, and the player that re-publishes one as an NDI source.
    pub(crate) struct FilePlayback from crate::sys::declarations {
        fn NDIlib_file_player_probe_file(p_file_path: *const ::std::os::raw::c_char, p_out_info: *mut NDIlib_file_player_file_info_t) -> bool;
        fn NDIlib_file_player_create(p_create_settings: *const NDIlib_file_player_create_t) -> NDIlib_file_player_instance_t;
        fn NDIlib_file_player_destroy(p_instance: NDIlib_file_player_instance_t);
        fn NDIlib_file_player_start(p_instance: NDIlib_file_player_instance_t, p_file_path: *const ::std::os::raw::c_char) -> bool;
        fn NDIlib_file_player_stop(p_instance: NDIlib_file_player_instance_t);
        fn NDIlib_file_player_is_running(p_instance: NDIlib_file_player_instance_t) -> bool;
        fn NDIlib_file_player_capture_event(p_instance: NDIlib_file_player_instance_t, p_out_event: *mut NDIlib_file_player_event_t, timeout_in_ms: u32) -> NDIlib_file_player_event_type_e;
        fn NDIlib_file_player_get_dropped_event_count(p_instance: NDIlib_file_player_instance_t) -> u64;
        fn NDIlib_file_player_get_stats(p_instance: NDIlib_file_player_instance_t, p_out_stats: *mut NDIlib_file_player_stats_t) -> bool;
        fn NDIlib_file_player_seek_to_time(p_instance: NDIlib_file_player_instance_t, time_100ns: u64) -> bool;
        fn NDIlib_file_player_seek_to_frame(p_instance: NDIlib_file_player_instance_t, frame_index: u64) -> bool;
        fn NDIlib_file_player_set_paused(p_instance: NDIlib_file_player_instance_t, paused: bool);
        fn NDIlib_file_player_get_paused(p_instance: NDIlib_file_player_instance_t) -> bool;
        fn NDIlib_file_player_set_loop(p_instance: NDIlib_file_player_instance_t, loop_: bool);
        fn NDIlib_file_player_get_loop(p_instance: NDIlib_file_player_instance_t) -> bool;
        fn NDIlib_file_player_get_current_duration(p_instance: NDIlib_file_player_instance_t, p_out_duration: *mut u64, p_out_is_final: *mut bool) -> bool;
        fn NDIlib_file_player_get_current_position(p_instance: NDIlib_file_player_instance_t, p_out_position: *mut u64) -> bool;
        fn NDIlib_file_player_get_current_source_timecode(p_instance: NDIlib_file_player_instance_t, p_out_timecode_100ns: *mut i64) -> bool;
        fn NDIlib_file_player_get_source_format(p_instance: NDIlib_file_player_instance_t, p_out_format: *mut NDIlib_file_source_format_t) -> bool;
        fn NDIlib_file_player_get_advertised_source(p_instance: NDIlib_file_player_instance_t) -> *const NDIlib_source_t;
    }
}

native_table! {
    /// The recorder that writes an NDI source to disk.
    pub(crate) struct FileRecording from crate::sys::declarations {
        fn NDIlib_file_recorder_create(p_create_settings: *const NDIlib_file_recorder_create_t) -> NDIlib_file_recorder_instance_t;
        fn NDIlib_file_recorder_destroy(p_instance: NDIlib_file_recorder_instance_t);
        fn NDIlib_file_recorder_start(p_instance: NDIlib_file_recorder_instance_t, p_output_prefix: *const ::std::os::raw::c_char) -> bool;
        fn NDIlib_file_recorder_start_with_timecode(p_instance: NDIlib_file_recorder_instance_t, p_output_prefix: *const ::std::os::raw::c_char, start_timecode_100ns: i64) -> bool;
        fn NDIlib_file_recorder_capture_event(p_instance: NDIlib_file_recorder_instance_t, p_out_event: *mut NDIlib_file_recorder_event_t, timeout_in_ms: u32) -> NDIlib_file_recorder_event_type_e;
        fn NDIlib_file_recorder_get_dropped_event_count(p_instance: NDIlib_file_recorder_instance_t) -> u64;
        fn NDIlib_file_recorder_stop(p_instance: NDIlib_file_recorder_instance_t);
        fn NDIlib_file_recorder_rollover(p_instance: NDIlib_file_recorder_instance_t, p_new_output_prefix: *const ::std::os::raw::c_char) -> bool;
        fn NDIlib_file_recorder_inject_metadata(p_instance: NDIlib_file_recorder_instance_t, p_metadata: *const NDIlib_metadata_frame_t) -> bool;
        fn NDIlib_file_recorder_set_source(p_instance: NDIlib_file_recorder_instance_t, p_source: *const NDIlib_file_recorder_source_t) -> bool;
        fn NDIlib_file_recorder_is_recording(p_instance: NDIlib_file_recorder_instance_t) -> bool;
        fn NDIlib_file_recorder_get_stats(p_instance: NDIlib_file_recorder_instance_t, p_out_stats: *mut NDIlib_file_recorder_stats_t) -> bool;
        fn NDIlib_file_recorder_get_source_format(p_instance: NDIlib_file_recorder_instance_t, p_out_format: *mut NDIlib_file_source_format_t) -> bool;
    }
}

/// File functions the headers declare that this crate deliberately does not
/// call, with the reason. The vendor's function-table loader is listed with the
/// core ones.
#[cfg(test)]
pub(crate) const UNWRAPPED: &[(&str, &str)] = &[
    (
        "NDIlib_file_recorder_get_source",
        "returns strings the recorder frees when it stops, which it can do on its own thread \
         while they are being copied; the wrapper reports the source it last accepted instead",
    ),
    (
        "NDIlib_file_recorder_get_current_file",
        "returns a string the recorder frees at its next rollover, which it performs on its own \
         thread; the wrapper reports the path from the recorder's owned events instead",
    ),
];

/// One family of entry points, or the first of them an implementation lacks.
///
/// A family resolves all-or-nothing: an implementation without one of its
/// entry points provides none of it.
type Family<T> = std::result::Result<T, &'static CStr>;

/// The File library selected for this process.
pub(crate) struct FileRuntime {
    playback: Family<FilePlayback>,
    recording: Family<FileRecording>,
    #[cfg(not(ndi_link_static))]
    path: std::path::PathBuf,
}

static SELECTED: Selection<FileRuntime> = Selection::new();

/// Select the File library that accompanies the selected core runtime, or
/// confirm that the one already pinned satisfies a request for `library_path`.
///
/// `core` and `api` describe the selected core runtime.
///
/// # Errors
///
/// [`Error::RuntimeLoadFailed`] when no usable File library can be provided
/// (retryable), [`Error::RuntimeConflict`] when a different one is already
/// pinned, [`Error::InvalidConfiguration`] for a relative `library_path`.
pub(crate) fn select_file(
    core: &RuntimeInfo,
    api: &Api,
    library_path: Option<&Path>,
) -> Result<&'static FileRuntime> {
    let file = SELECTED.get_or_try_load(|| FileRuntime::load(core, api, library_path))?;
    file.accepts(library_path)?;
    Ok(file)
}

impl FileRuntime {
    /// The playback family, or the first of its entry points the library lacks.
    pub(crate) fn playback(&self) -> Family<&FilePlayback> {
        self.playback.as_ref().map_err(|symbol| *symbol)
    }

    /// The recording family, or the first of its entry points the library lacks.
    pub(crate) fn recording(&self) -> Family<&FileRecording> {
        self.recording.as_ref().map_err(|symbol| *symbol)
    }

    /// Resolve both families from `resolver`.
    ///
    /// The error is the reason this implementation cannot be used at all.
    fn resolve(
        resolver: &dyn Resolver,
    ) -> std::result::Result<(Family<FilePlayback>, Family<FileRecording>), String> {
        let playback = FilePlayback::load(resolver).map_err(reason)?;
        let recording = FileRecording::load(resolver).map_err(reason)?;
        if let (Err(symbol), Err(_)) = (&playback, &recording) {
            return Err(format!(
                "it provides neither file playback nor file recording (no {})",
                symbol.to_string_lossy()
            ));
        }
        Ok((playback, recording))
    }
}

#[cfg(not(ndi_link_static))]
impl FileRuntime {
    /// A symbol every File library defines, used to identify the opened module.
    const ANCHOR: &'static CStr = c"NDIlib_file_v1_load";

    /// A core entry point the POSIX File library imports.
    const CORE_ENTRY: &'static CStr = c"NDIlib_initialize";

    /// The File library's file name on this target, as the vendor ships it.
    const LIBRARY_NAME: &'static str = if cfg!(all(windows, target_pointer_width = "64")) {
        "Processing.NDI.Lib.File.x64.dll"
    } else if cfg!(windows) {
        "Processing.NDI.Lib.File.x86.dll"
    } else if cfg!(target_vendor = "apple") {
        "libndifile.dylib"
    } else if cfg!(target_os = "android") {
        "libndifile.so"
    } else {
        "libndifile.so.6"
    };

    /// The file the library was loaded from.
    pub(crate) fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn load(core: &RuntimeInfo, api: &Api, explicit: Option<&Path>) -> Result<Self> {
        // The POSIX File library imports the core entry points from the
        // Advanced runtime by name. Opened beside a Standard core it would make
        // the platform loader bring a second NDI implementation into the
        // process, so it is refused before anything is opened. The Windows
        // File library carries its own implementation and imports none.
        if cfg!(unix) && core.edition() != crate::capabilities::SdkEdition::Advanced {
            return Err(Error::RuntimeLoadFailed(
                "the NDI File library needs the Advanced SDK runtime, and the selected runtime \
                 is the Standard one"
                    .into(),
            ));
        }
        Search {
            explicit,
            beside: core.library_path(),
            name: Self::LIBRARY_NAME,
            anchor: Self::ANCHOR,
            description: "NDI File library",
        }
        .open(|module| Self::describe(module, api))
    }

    /// Resolve the tables of an opened File library.
    ///
    /// The error is the reason this library cannot be used.
    fn describe(module: Module, api: &Api) -> std::result::Result<Self, String> {
        // A lookup through the library's handle shows which module the platform
        // loader bound its core imports to. That must be the selected core: a
        // second copy of the runtime would not share its initialization. The
        // Windows File library imports and exports no core entry point, so
        // there the lookup finds nothing and there is no binding to check.
        if let Some(bound) = module.binding(Self::CORE_ENTRY)? {
            if bound.as_ptr() != api.core.NDIlib_initialize as *mut std::ffi::c_void {
                return Err("it is bound to an NDI runtime other than the selected one".into());
            }
        }
        let (playback, recording) = Self::resolve(&module)?;
        let path = module
            .path()
            .ok_or("the loader did not report where it was loaded from")?;
        Ok(Self {
            playback,
            recording,
            path,
        })
    }

    fn accepts(&self, requested: Option<&Path>) -> Result<()> {
        confirm_selected(&self.path, requested)
    }
}

#[cfg(ndi_link_static)]
impl FileRuntime {
    /// A linked implementation has no file of its own.
    pub(crate) fn path(&self) -> Option<&Path> {
        None
    }

    fn load(_core: &RuntimeInfo, _api: &Api, explicit: Option<&Path>) -> Result<Self> {
        Self::reject_path(explicit)?;
        let (playback, recording) = Self::resolve(&Linked).map_err(|reason| {
            Error::RuntimeLoadFailed(format!("the linked NDI SDK archive: {reason}"))
        })?;
        Ok(Self {
            playback,
            recording,
        })
    }

    fn accepts(&self, requested: Option<&Path>) -> Result<()> {
        Self::reject_path(requested)
    }

    fn reject_path(requested: Option<&Path>) -> Result<()> {
        if requested.is_some() {
            return Err(Error::RuntimeConflict(
                "a statically linked NDI File implementation cannot be replaced by a library"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::table::ExportsWhere;

    #[test]
    fn each_family_resolves_independently() {
        let (playback, recording) = FileRuntime::resolve(&ExportsWhere(|name| {
            name != "NDIlib_file_recorder_rollover"
        }))
        .unwrap();
        assert!(playback.is_ok());
        assert_eq!(recording.err(), Some(c"NDIlib_file_recorder_rollover"));
    }

    #[test]
    fn a_library_with_neither_family_is_not_a_file_library() {
        let reason = FileRuntime::resolve(&ExportsWhere(|name| {
            !name.starts_with("NDIlib_file_player_create")
                && !name.starts_with("NDIlib_file_recorder_create")
        }))
        .err()
        .unwrap();
        assert!(reason.contains("NDIlib_file_player_create"), "{reason}");
    }
}
