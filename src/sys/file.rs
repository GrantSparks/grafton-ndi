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
    loader::ExplicitLibrary,
    selection::Selection,
    table::{native_table, Resolver},
    Runtime,
};
#[cfg(ndi_link_static)]
use crate::LoadFailure;
use crate::{
    capabilities::{Capability, CapabilityRefusal, CapabilityStatus},
    file::FileFamily,
    Error, LibraryKind, LoadRefusal, Result, SelectionFailure,
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
                            first_missing: first,
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

static SELECTED: Selection<FileRuntime> = Selection::new(LibraryKind::File);

/// Select the File library that accompanies the selected core runtime, or
/// confirm that the one already pinned satisfies a request for `library_path`.
///
/// `runtime` owns the selected core implementation and its lifecycle.
///
/// # Errors
///
/// [`Error::RuntimeLoadFailed`] when no usable File library can be provided
/// (retryable), [`Error::RuntimeConflict`] when a different one is already
/// pinned or the selected core runtime is one no File library can accompany,
/// [`Error::InvalidInput`] for a relative `library_path`.
pub(crate) fn select_file(
    runtime: &Runtime,
    library_path: Option<&Path>,
) -> Result<&'static FileRuntime> {
    let library_path = library_path.map(ExplicitLibrary::new).transpose()?;
    let file = SELECTED.get_or_try_load(|| FileRuntime::load(runtime, library_path))?;
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
    /// [`Error::UnsupportedCapability`] with the family and the first
    /// entry point the library does not export, exactly when
    /// [`Self::status`] reports the family unavailable.
    pub(crate) fn table<T: FileTable>(&self) -> Result<&T> {
        T::resolved(self)
            .as_ref()
            .map_err(|symbol| Error::UnsupportedCapability {
                capability: Capability::File(T::FAMILY),
                refusal: CapabilityRefusal::SymbolUnavailable(symbol),
            })
    }

    /// Whether the library provides `family`: unavailable exactly when
    /// [`Self::table`] refuses the family's table.
    pub(crate) fn status(&self, family: FileFamily) -> CapabilityStatus {
        match self.families.missing(family) {
            None => CapabilityStatus::Available,
            Some(symbol) => {
                CapabilityStatus::Unavailable(CapabilityRefusal::SymbolUnavailable(symbol))
            }
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
    const LIBRARY_NAME: &'static str =
        super::loader::library_file(super::loader::LibraryFile::File);

    /// The file the library was loaded from.
    pub(crate) fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn load(runtime: &Runtime, explicit: Option<ExplicitLibrary<'_>>) -> Result<Self> {
        let core = &runtime.info;
        if super::loader::FILE_LIBRARY_NEEDS_ADVANCED_RUNTIME
            && core.edition() != crate::capabilities::SdkEdition::Advanced
        {
            return Err(Error::RuntimeConflict(
                SelectionFailure::file_needs_advanced(
                    core.library_path(),
                    explicit.map(ExplicitLibrary::path),
                ),
            ));
        }
        Search {
            kind: LibraryKind::File,
            explicit,
            beside: core.library_path(),
            names: &[Self::LIBRARY_NAME],
            anchor: Self::ANCHOR,
            #[cfg(all(unix, not(target_vendor = "apple")))]
            selected_core: Some(runtime.gates.api().core.NDIlib_version as *const std::ffi::c_void),
            description: "NDI File library",
        }
        .open(|module| Self::describe(module, runtime))
    }

    /// Resolve the tables of an opened File library.
    ///
    /// The error is the reason this library cannot be used.
    fn describe(module: Module, runtime: &Runtime) -> std::result::Result<Self, LoadRefusal> {
        // A lookup through the library's handle shows which module the platform
        // loader bound its core imports to. That must be the selected core: a
        // second copy of the runtime would not share its initialization. The
        // Windows File library imports and exports no core entry point, so
        // there the lookup finds nothing and there is no binding to check.
        if let Some(bound) = module.binding(Self::CORE_ENTRY)? {
            if bound.as_ptr() != runtime.lifecycle.initialize_address().cast_mut() {
                return Err(LoadRefusal::BoundToAnotherRuntime);
            }
        }
        let families = Families::load(&module)?;
        let path = module.path().to_owned();
        Ok(Self { families, path })
    }

    fn accepts(&self, requested: Option<ExplicitLibrary<'_>>) -> Result<()> {
        confirm_selected(LibraryKind::File, &self.path, requested)
    }
}

#[cfg(ndi_link_static)]
impl FileRuntime {
    /// A linked implementation has no file of its own.
    pub(crate) fn path(&self) -> Option<&Path> {
        None
    }

    fn load(_runtime: &Runtime, explicit: Option<ExplicitLibrary<'_>>) -> Result<Self> {
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

    fn accepts(&self, requested: Option<ExplicitLibrary<'_>>) -> Result<()> {
        Self::reject_path(requested)
    }

    fn reject_path(requested: Option<ExplicitLibrary<'_>>) -> Result<()> {
        if let Some(requested) = requested {
            return Err(Error::RuntimeConflict(SelectionFailure::linked(
                LibraryKind::File,
                requested.path(),
            )));
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
        let runtime = Box::leak(Box::new(Self {
            families: Families {
                playback,
                recording,
            },
            #[cfg(not(ndi_link_static))]
            path: "test double".into(),
        }));
        // Selected native modules have process lifetime. Keep the test doubles
        // reachable too, so Miri distinguishes this ownership from a leak.
        #[cfg(miri)]
        {
            static SELECTED: std::sync::Mutex<Vec<&'static FileRuntime>> =
                std::sync::Mutex::new(Vec::new());
            SELECTED.lock().unwrap().push(runtime);
        }
        runtime
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::table::ExportsWhere;
    use crate::CapabilityUnavailable;

    /// A library that exports the names `exported` accepts.
    fn library(exported: impl Fn(&str) -> bool) -> std::result::Result<FileRuntime, LoadRefusal> {
        Ok(FileRuntime {
            families: Families::load(&ExportsWhere(exported))?,
            #[cfg(not(ndi_link_static))]
            path: "test".into(),
        })
    }

    /// A refusal's family and missing entry point.
    fn refusal<T>(result: Result<T>) -> Option<(FileFamily, &'static CStr)> {
        match result {
            Ok(_) => None,
            Err(Error::UnsupportedCapability {
                capability: Capability::File(family),
                refusal: CapabilityRefusal::SymbolUnavailable(missing),
            }) => Some((family, missing)),
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
            (library.status(FileFamily::Recording)).reason(),
            Some(CapabilityUnavailable::SymbolUnavailable)
        );
        assert_eq!(
            refusal(library.table::<FileRecording>()),
            Some((FileFamily::Recording, c"NDIlib_file_recorder_rollover"))
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
                let ((refused, status), allocations) = crate::test_alloc::allocation_count(|| {
                    let refused = match family {
                        FileFamily::Playback => refusal(library.table::<FilePlayback>()),
                        FileFamily::Recording => refusal(library.table::<FileRecording>()),
                    };
                    (refused, library.status(family))
                });
                let context = format!("{family:?} lacking {lacking:?}");
                assert_eq!(allocations, 0, "{context}");
                match status {
                    CapabilityStatus::Available => assert_eq!(refused, None, "{context}"),
                    CapabilityStatus::Unavailable(reason) => {
                        let (refused_family, detail) = refused.expect(&context);
                        assert_eq!(refused_family, family, "{context}");
                        assert_eq!(
                            reason,
                            CapabilityRefusal::SymbolUnavailable(detail),
                            "{context}"
                        );
                        assert_eq!(Some(detail.to_str().unwrap()), lacking, "{context}");
                    }
                }
            }
        }
        assert_eq!(FilePlayback::FAMILY, FileFamily::Playback);
        assert_eq!(FileRecording::FAMILY, FileFamily::Recording);
    }

    #[test]
    fn refusing_every_file_family_borrows_the_missing_export_name() {
        let (refusal, allocations) = crate::test_alloc::allocation_count(|| {
            Families::load(&ExportsWhere(|_| false))
                .err()
                .expect("no File family is complete")
        });
        assert_eq!(allocations, 0);
        assert_eq!(
            refusal,
            LoadRefusal::NoCompleteFileFamily {
                first_missing: c"NDIlib_file_player_probe_file",
            }
        );
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
                first_missing: c"NDIlib_file_player_create"
            }
        );
    }
}
