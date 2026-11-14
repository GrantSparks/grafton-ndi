//! The NDI File library: its typed entry points and the module that provides them.
//!
//! The File library accompanies the core runtime. Shared targets find it with the
//! core runtime's own search, starting beside the selected core module, and pin
//! it for the life of the process. Statically linked targets resolve the same
//! tables from the archive linked at build time.
//!
//! Its entry points come in families, one for each [`FileFamily`], and each
//! family resolves all-or-nothing. A family's table is handed out only by
//! [`FileRuntime::table`], which refuses it under that family when the library
//! lacks one of its entry points, so a wrapper holding the table can call
//! everything in it.

use std::{ffi::CStr, path::Path};

#[cfg(ndi_link_static)]
use super::loader::Linked;
#[cfg(not(ndi_link_static))]
use super::loader::{confirm_selected, Module, Search};
use super::{
    bindings::*,
    selection::Selection,
    table::{native_table, Resolver},
    Api,
};
#[cfg(ndi_link_static)]
use crate::LoadFailure;
use crate::{
    capabilities::{CapabilityStatus, CapabilityUnavailable, RuntimeInfo},
    file::FileFamily,
    Error, LibraryKind, LoadRefusal, Result, SelectionConflict,
};

/// A family's table as one library resolved it, or the first of its entry
/// points the library lacks.
type Resolved<T> = std::result::Result<T, &'static CStr>;

/// The table of one family of File entry points.
pub(crate) trait FileTable: Sized + 'static {
    /// The family the table holds, under which it is gated and reported.
    const FAMILY: FileFamily;

    /// The table as `runtime` resolved it. Callers use
    /// [`FileRuntime::table`], which reports a missing entry point as the
    /// family's error.
    fn resolved(runtime: &FileRuntime) -> &Resolved<Self>;
}

/// Define the families of the File library's entry points.
///
/// Each entry states, once, everything about one family: its table, the
/// field of [`Families`] that holds it and the [`FileFamily`] it is gated and
/// reported under. Every family needs an entry, because
/// [`Families::missing`] matches each one.
macro_rules! file_families {
    ($(
        $(#[$meta:meta])*
        struct $Table:ident in $field:ident: $Family:ident {
            $(fn $name:ident($($argument:ident: $type:ty),* $(,)?) $(-> $return:ty)?;)+
        }
    )+) => {
        $(
            native_table! {
                $(#[$meta])*
                pub(crate) struct $Table from crate::sys::declarations {
                    $(fn $name($($argument: $type),*) $(-> $return)?;)+
                }
            }

            impl FileTable for $Table {
                const FAMILY: FileFamily = FileFamily::$Family;

                fn resolved(runtime: &FileRuntime) -> &Resolved<Self> {
                    &runtime.families.$field
                }
            }
        )+

        /// Every family resolved from one File library.
        struct Families {
            $($field: Resolved<$Table>,)+
        }

        impl Families {
            /// Resolve every family from `resolver`.
            ///
            /// The error is the reason this implementation cannot be used at
            /// all: an entry point resolves outside it, or it provides no
            /// family.
            fn load(resolver: &dyn Resolver) -> std::result::Result<Self, LoadRefusal> {
                let families = Self {
                    $($field: $Table::load(resolver)?,)+
                };
                // The first entry point each family lacks, in declaration order.
                let missing = [$(families.$field.as_ref().err().copied()),+];
                if let [Some(first), ..] = missing {
                    if missing.iter().all(Option::is_some) {
                        return Err(LoadRefusal::NoCompleteFileFamily {
                            first_missing: first.to_string_lossy().into_owned(),
                        });
                    }
                }
                Ok(families)
            }

            /// The first entry point of `family` that the library does not
            /// export, or `None` when it exports them all.
            fn missing(&self, family: FileFamily) -> Option<&'static CStr> {
                match family {
                    $(FileFamily::$Family => self.$field.as_ref().err().copied(),)+
                }
            }
        }
    };
}

file_families! {
    /// Probing a recording, and the player that re-publishes one as an NDI source.
    struct FilePlayback in playback: Playback {
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

    /// The recorder that writes an NDI source to disk.
    struct FileRecording in recording: Recording {
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

/// The File library selected for this process.
pub(crate) struct FileRuntime {
    families: Families,
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
/// pinned or the selected core runtime is one no File library can accompany,
/// [`Error::InvalidInput`] for a relative `library_path`.
pub(crate) fn select_file(
    core: &RuntimeInfo,
    api: &Api,
    library_path: Option<&Path>,
) -> Result<&'static FileRuntime> {
    super::loader::validate_library_path(library_path)?;
    let file = SELECTED.get_or_try_load(|| FileRuntime::load(core, api, library_path))?;
    file.accepts(library_path)?;
    Ok(file)
}

impl FileRuntime {
    /// The table of the family `T` holds, once the library is known to
    /// export every one of its entry points.
    ///
    /// This is the only way to a family's table, so a table is never used
    /// without the gate of its family, or reported as another one.
    ///
    /// # Errors
    ///
    /// [`Error::FileFamilyUnavailable`] with the family and the first
    /// entry point the library does not export, exactly when
    /// [`Self::status`] reports the family unavailable.
    pub(crate) fn table<T: FileTable>(&self) -> Result<&T> {
        T::resolved(self)
            .as_ref()
            .map_err(|symbol| Error::FileFamilyUnavailable {
                family: T::FAMILY,
                detail: symbol.to_string_lossy().into_owned(),
            })
    }

    /// Whether the library provides `family`: unavailable exactly when
    /// [`Self::table`] refuses the family's table.
    pub(crate) fn status(&self, family: FileFamily) -> CapabilityStatus {
        match self.families.missing(family) {
            None => CapabilityStatus::Available,
            Some(_) => CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable),
        }
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
        // The 6.4.0.2 POSIX File library, including Standard SDK copies,
        // imports the core entry points from the Advanced runtime by name.
        // Opened beside a Standard core it would make
        // the platform loader bring a second NDI implementation into the
        // process, so it is refused before anything is opened. The Windows
        // File library carries its own implementation and imports none.
        if cfg!(unix) && core.edition() != crate::capabilities::SdkEdition::Advanced {
            return Err(Error::RuntimeConflict {
                kind: SelectionConflict::FileNeedsAdvancedRuntime,
                detail:
                    "the NDI File library needs the Advanced SDK runtime, and the selected runtime \
                 is the Standard one"
                        .into(),
            });
        }
        // Where libraries are ELF objects, the platform loader binds the File
        // library's imports from the process's global scope before it looks
        // at the library's own dependencies. A runtime already there, such as
        // one the program was linked or preloaded with, would be the one the
        // File library uses, whatever its dependency resolves to.
        #[cfg(all(unix, not(target_vendor = "apple")))]
        if let Some(file) = foreign_global_definition(
            Self::CORE_ENTRY,
            api.core.NDIlib_initialize as *mut std::ffi::c_void,
        ) {
            return Err(Error::RuntimeConflict { kind: SelectionConflict::ForeignRuntimeInScope,
                detail: format!("the process's global symbol scope already holds an NDI runtime that is not the \
                 selected one ({file}); the NDI File library would bind to it"),
            });
        }
        Search {
            kind: LibraryKind::File,
            explicit,
            beside: core.library_path(),
            names: &[Self::LIBRARY_NAME],
            anchor: Self::ANCHOR,
            description: "NDI File library",
        }
        .open(|module| Self::describe(module, api))
    }

    /// Resolve the tables of an opened File library.
    ///
    /// The error is the reason this library cannot be used.
    fn describe(module: Module, api: &Api) -> std::result::Result<Self, LoadRefusal> {
        // A lookup through the library's handle shows which module the platform
        // loader bound its core imports to. That must be the selected core: a
        // second copy of the runtime would not share its initialization. The
        // Windows File library imports and exports no core entry point, so
        // there the lookup finds nothing and there is no binding to check.
        if let Some(bound) = module.binding(Self::CORE_ENTRY)? {
            if bound.as_ptr() != api.core.NDIlib_initialize as *mut std::ffi::c_void {
                return Err(LoadRefusal::BoundToAnotherRuntime);
            }
        }
        let families = Families::load(&module)?;
        let path = module.path().ok_or_else(|| LoadRefusal::NotIdentifiable {
            detail: "the loader did not report where it was loaded from".into(),
        })?;
        Ok(Self { families, path })
    }

    fn accepts(&self, requested: Option<&Path>) -> Result<()> {
        confirm_selected(&self.path, requested)
    }
}

/// The file that defines `name` in the process's global symbol scope, when
/// that definition is not the one at `selected`.
#[cfg(all(unix, not(target_vendor = "apple"), not(ndi_link_static)))]
fn foreign_global_definition(name: &CStr, selected: *mut std::ffi::c_void) -> Option<String> {
    // SAFETY: a lookup of a NUL-terminated name in the default scope.
    let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    if address.is_null() || address == selected {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    // SAFETY: `info` is writable storage for one Dl_info; dladdr only reads
    // the loader's module list for `address`.
    let file = (unsafe { libc::dladdr(address, info.as_mut_ptr()) } != 0)
        // SAFETY: dladdr reported success, so it initialized `info`.
        .then(|| unsafe { info.assume_init() }.dli_fname)
        .filter(|file| !file.is_null())
        // SAFETY: a non-null dli_fname is a NUL-terminated string the loader
        // keeps for as long as the module is loaded.
        .map(|file| {
            unsafe { CStr::from_ptr(file) }
                .to_string_lossy()
                .into_owned()
        });
    Some(file.unwrap_or_else(|| "a module the loader cannot name".into()))
}

#[cfg(ndi_link_static)]
impl FileRuntime {
    /// A linked implementation has no file of its own.
    pub(crate) fn path(&self) -> Option<&Path> {
        None
    }

    fn load(_core: &RuntimeInfo, _api: &Api, explicit: Option<&Path>) -> Result<Self> {
        Self::reject_path(explicit)?;
        let families = Families::load(&Linked).map_err(|reason| {
            Error::RuntimeLoadFailed(LoadFailure::linked(
                LibraryKind::File,
                "the linked NDI SDK archive",
                reason,
            ))
        })?;
        Ok(Self { families })
    }

    fn accepts(&self, requested: Option<&Path>) -> Result<()> {
        Self::reject_path(requested)
    }

    fn reject_path(requested: Option<&Path>) -> Result<()> {
        if requested.is_some() {
            return Err(Error::RuntimeConflict {
                kind: SelectionConflict::LinkedImplementation,
                detail:
                    "a statically linked NDI File implementation cannot be replaced by a library"
                        .into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
impl FileRuntime {
    /// A library whose families are the given test doubles, or lack the
    /// entry point an `Err` names. It is never freed, as a selected library
    /// is never unloaded.
    pub(crate) fn test_double(
        playback: Resolved<FilePlayback>,
        recording: Resolved<FileRecording>,
    ) -> &'static Self {
        Box::leak(Box::new(Self {
            families: Families {
                playback,
                recording,
            },
            #[cfg(not(ndi_link_static))]
            path: "test double".into(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::table::ExportsWhere;

    /// A library that exports the names `exported` accepts.
    fn library(exported: impl Fn(&str) -> bool) -> std::result::Result<FileRuntime, LoadRefusal> {
        Ok(FileRuntime {
            families: Families::load(&ExportsWhere(exported))?,
            #[cfg(not(ndi_link_static))]
            path: "test".into(),
        })
    }

    /// A refusal's family and missing entry point.
    fn refusal<T>(result: Result<T>) -> Option<(FileFamily, String)> {
        match result {
            Ok(_) => None,
            Err(Error::FileFamilyUnavailable { family, detail }) => Some((family, detail)),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn each_family_resolves_independently_and_is_refused_as_itself() {
        let library = library(|name| name != "NDIlib_file_recorder_rollover").unwrap();
        assert_eq!(
            library.status(FileFamily::Playback),
            CapabilityStatus::Available
        );
        assert!(library.table::<FilePlayback>().is_ok());
        assert_eq!(
            library.status(FileFamily::Recording),
            CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable)
        );
        assert_eq!(
            refusal(library.table::<FileRecording>()),
            Some((
                FileFamily::Recording,
                "NDIlib_file_recorder_rollover".into()
            ))
        );
    }

    /// The status of each family and the gate of its table are one decision,
    /// whichever entry point the library lacks.
    #[test]
    fn every_family_has_a_table_whose_gate_agrees_with_its_status() {
        let every_symbol = FilePlayback::SYMBOLS
            .iter()
            .chain(FileRecording::SYMBOLS)
            .map(|symbol| symbol.to_str().unwrap());
        for lacking in std::iter::once(None).chain(every_symbol.map(Some)) {
            let library = library(|name| Some(name) != lacking).unwrap();
            for family in FileFamily::ALL {
                let refused = match family {
                    FileFamily::Playback => refusal(library.table::<FilePlayback>()),
                    FileFamily::Recording => refusal(library.table::<FileRecording>()),
                };
                let context = format!("{family:?} lacking {lacking:?}");
                match library.status(family) {
                    CapabilityStatus::Available => assert_eq!(refused, None, "{context}"),
                    CapabilityStatus::Unavailable(reason) => {
                        let (refused_family, detail) = refused.expect(&context);
                        assert_eq!(refused_family, family, "{context}");
                        assert_eq!(
                            reason,
                            CapabilityUnavailable::SymbolUnavailable,
                            "{context}"
                        );
                        assert_eq!(Some(detail.as_str()), lacking, "{context}");
                    }
                }
            }
        }
        assert_eq!(FilePlayback::FAMILY, FileFamily::Playback);
        assert_eq!(FileRecording::FAMILY, FileFamily::Recording);
    }

    #[test]
    fn a_library_with_neither_family_is_not_a_file_library() {
        let Err(reason) = library(|name| {
            !name.starts_with("NDIlib_file_player_create")
                && !name.starts_with("NDIlib_file_recorder_create")
        }) else {
            panic!("a library without any family was accepted");
        };
        assert_eq!(
            reason,
            LoadRefusal::NoCompleteFileFamily {
                first_missing: "NDIlib_file_player_create".into()
            }
        );
    }
}
