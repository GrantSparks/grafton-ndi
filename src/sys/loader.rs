//! Locating, opening and pinning the native NDI implementation.
//!
//! Shared targets search for the runtime library in a fixed order and pin the
//! first usable module for the life of the process. Statically linked targets
//! resolve the same tables from the archive selected at build time.

use std::path::Path;

use super::{
    api::{Gates, Resolved},
    selection::Selection,
    table::Resolver,
};
use crate::{
    capabilities::{RuntimeInfo, SdkEdition, SdkVersion},
    runtime::RuntimeOptions,
    sdk_copy::{bounded_sdk_text, MAX_SDK_STRING_BYTES},
    Error, LoadRefusal, Result, SelectionFailure, VersionTextDefect,
};

#[cfg(any(not(ndi_link_static), test))]
mod file_policy;
#[cfg(any(not(ndi_link_static), test))]
pub(crate) use file_policy::FILE_LIBRARY_NEEDS_ADVANCED_RUNTIME;

/// Which dynamically loaded library's native file name is needed.
#[cfg(not(ndi_link_static))]
pub(in crate::sys) enum LibraryFile {
    Runtime(SdkEdition),
    #[cfg(feature = "file-io")]
    File,
}

/// The vendor's native file names, with one row for each platform naming rule.
#[cfg(not(ndi_link_static))]
pub(in crate::sys) const fn library_file(kind: LibraryFile) -> &'static str {
    let names = if cfg!(all(windows, target_pointer_width = "64")) {
        [
            "Processing.NDI.Lib.x64.dll",
            "Processing.NDI.Lib.Advanced.x64.dll",
            "Processing.NDI.Lib.File.x64.dll",
        ]
    } else if cfg!(windows) {
        [
            "Processing.NDI.Lib.x86.dll",
            "Processing.NDI.Lib.Advanced.x86.dll",
            "Processing.NDI.Lib.File.x86.dll",
        ]
    } else if cfg!(target_vendor = "apple") {
        ["libndi.dylib", "libndi_advanced.dylib", "libndifile.dylib"]
    } else if cfg!(target_os = "android") {
        ["libndi.so", "libndi_advanced.so", "libndifile.so"]
    } else {
        ["libndi.so.6", "libndi_advanced.so.6", "libndifile.so.6"]
    };
    match kind {
        LibraryFile::Runtime(SdkEdition::Standard) => names[0],
        LibraryFile::Runtime(SdkEdition::Advanced) => names[1],
        #[cfg(feature = "file-io")]
        LibraryFile::File => names[2],
    }
}

/// An absolute caller-selected library path, validated before selection.
#[derive(Clone, Copy)]
pub(in crate::sys) struct ExplicitLibrary<'a>(&'a Path);

impl<'a> ExplicitLibrary<'a> {
    pub(in crate::sys) fn new(path: &'a Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(Error::InvalidInput(crate::InputDefect::RelativeLibraryPath));
        }
        Ok(Self(path))
    }

    pub(in crate::sys) fn path(self) -> &'a Path {
        self.0
    }
}

enum RuntimeProvider {
    Native(Provider),
    #[cfg(test)]
    Synthetic,
}

static SELECTED: Selection<Runtime> = Selection::new(crate::LibraryKind::Runtime);

/// The implementation selected for this process.
pub(crate) struct Runtime {
    pub(crate) lifecycle: crate::runtime::RuntimeManager,
    pub(crate) gates: Gates,
    pub(crate) info: RuntimeInfo,
    provider: RuntimeProvider,
}

/// Observe the selected runtime without selecting or loading one.
pub(crate) fn is_running() -> bool {
    SELECTED
        .get()
        .is_some_and(|runtime| runtime.lifecycle.is_running())
}

/// Select the implementation `options` describe, or confirm that the one
/// already pinned satisfies them.
///
/// # Errors
///
/// [`Error::RuntimeLoadFailed`] when nothing usable can be loaded (retryable),
/// [`Error::RuntimeConflict`] when a different implementation is already pinned,
/// [`Error::InvalidInput`] when an explicit library path is relative or empty.
pub(crate) fn select(options: &RuntimeOptions) -> Result<&'static Runtime> {
    let explicit = options
        .library_path
        .as_deref()
        .map(ExplicitLibrary::new)
        .transpose()?;
    let runtime = SELECTED.get_or_try_load(|| Runtime::load(options, explicit))?;
    runtime.accepts(options, explicit)?;
    Ok(runtime)
}

/// The editions whose libraries are searched for when [`RuntimeOptions`]
/// names none, in order of preference at each location.
///
/// Every runtime meets a request that names no edition, so both are looked
/// for. Where both are installed in one place, a build that needs the Advanced
/// runtime for some of its API prefers it: one with the `advanced` feature, or
/// with `file-io` where the File library works only beside the Advanced
/// runtime. Other builds prefer the Standard one. Nothing on the build machine
/// affects the order.
#[cfg(not(ndi_link_static))]
const DEFAULT_SEARCH: [SdkEdition; 2] = if cfg!(feature = "advanced")
    || (cfg!(feature = "file-io") && FILE_LIBRARY_NEEDS_ADVANCED_RUNTIME)
{
    [SdkEdition::Advanced, SdkEdition::Standard]
} else {
    [SdkEdition::Standard, SdkEdition::Advanced]
};

/// Whether an implementation of edition `provided` meets a request for
/// `requested`.
///
/// The Advanced SDK exports everything the Standard SDK does, so only a
/// request for the Advanced SDK can go unmet. Every provider decides by this
/// rule, both when it first describes an implementation and when a later
/// request is checked against the one already selected.
fn meets(requested: SdkEdition, provided: SdkEdition) -> bool {
    requested != SdkEdition::Advanced || provided == SdkEdition::Advanced
}

impl Runtime {
    /// Resolve the tables of an opened implementation and describe it.
    ///
    /// The error is the reason this implementation cannot be used.
    fn describe(
        provider: Provider,
        requested: SdkEdition,
    ) -> std::result::Result<Self, LoadRefusal> {
        let resolved = Resolved::load(&provider)?;
        let edition = provider.edition()?;
        if !meets(requested, edition) {
            return Err(LoadRefusal::EditionNotMet);
        }
        // SAFETY: NDIlib_version takes no arguments and may be called before
        // initialization; it returns a static NUL-terminated string or null.
        let version = unsafe { resolved.core().NDIlib_version() };
        if version.is_null() {
            return Err(LoadRefusal::UnreadableVersion(VersionTextDefect::Missing));
        }
        // Read as far as the first NUL, but no further than one SDK string may
        // reach: a text without a terminator that close is not a version.
        // SAFETY: non-null was checked, and the SDK keeps the string alive for
        // as long as the implementation is loaded, which is the whole process.
        // The scan stops at its terminator or at the limit.
        let raw_version = unsafe { bounded_sdk_text(version, MAX_SDK_STRING_BYTES) }.ok_or(
            LoadRefusal::UnreadableVersion(VersionTextDefect::Unterminated),
        )?;
        let raw_version = std::str::from_utf8(raw_version)
            .map_err(|error| {
                LoadRefusal::UnreadableVersion(VersionTextDefect::NotUtf8 {
                    valid_up_to: error.valid_up_to(),
                })
            })?
            .to_owned();
        let version = SdkVersion::parse(&raw_version);
        let info = RuntimeInfo {
            edition,
            linkage: Provider::LINKAGE,
            library_path: provider.library_path(),
            raw_version,
            version,
        };
        let supported_cpu = resolved.core().NDIlib_is_supported_CPU;
        let (lifecycle, gates) = resolved.seal(version, edition);
        Ok(Self {
            lifecycle: crate::runtime::RuntimeManager::new(lifecycle, supported_cpu),
            gates,
            info,
            provider: RuntimeProvider::Native(provider),
        })
    }

    /// An isolated SDK-free runtime. Only the supplied core calls are usable.
    #[cfg(test)]
    pub(crate) fn test_double(core: super::Core) -> &'static Self {
        unsafe extern "C" fn initialize() -> bool {
            true
        }
        unsafe extern "C" fn destroy() {}
        unsafe extern "C" fn cpu() -> bool {
            true
        }
        let version = SdkVersion::parse("6.4.0.2");
        let resolved = Resolved::with_test_core(core);
        let runtime = Box::leak(Box::new(Self {
            lifecycle: crate::runtime::RuntimeManager::new(
                super::Lifecycle {
                    NDIlib_initialize: initialize,
                    NDIlib_destroy: destroy,
                },
                cpu,
            ),
            gates: resolved.seal(version, SdkEdition::Standard).1,
            info: RuntimeInfo {
                edition: SdkEdition::Standard,
                linkage: crate::capabilities::SdkLinkage::Static,
                library_path: None,
                raw_version: "6.4.0.2".into(),
                version,
            },
            provider: RuntimeProvider::Synthetic,
        }));
        // Like the selected vendor runtime, isolated doubles are process-lived.
        // Keep them reachable so interpreter leak checks observe the same policy.
        static RUNTIMES: std::sync::Mutex<Vec<&'static Runtime>> =
            std::sync::Mutex::new(Vec::new());
        RUNTIMES.lock().unwrap().push(runtime);
        runtime
    }

    /// The core module whose identity a companion library must share.
    #[cfg(all(feature = "file-io", not(ndi_link_static)))]
    pub(in crate::sys) fn module(&self) -> Option<&Module> {
        match &self.provider {
            RuntimeProvider::Native(module) => Some(module),
            #[cfg(test)]
            RuntimeProvider::Synthetic => None,
        }
    }

    /// Confirm that this implementation, which is selected for the process
    /// and cannot be replaced, meets `options`.
    ///
    /// # Errors
    ///
    /// [`Error::RuntimeConflict`] when `options` ask for an edition it is not
    /// or name a library it was not loaded from.
    fn accepts(
        &self,
        options: &RuntimeOptions,
        explicit: Option<ExplicitLibrary<'_>>,
    ) -> Result<()> {
        if options
            .edition
            .is_some_and(|requested| !meets(requested, self.info.edition))
        {
            return Err(Error::RuntimeConflict(SelectionFailure::edition_not_met(
                self.info.library_path(),
                explicit.map(ExplicitLibrary::path),
            )));
        }
        match &self.provider {
            RuntimeProvider::Native(provider) => provider.confirm_location(explicit),
            #[cfg(test)]
            RuntimeProvider::Synthetic => Ok(()),
        }
    }
}

#[cfg(not(ndi_link_static))]
use shared::Module as Provider;
#[cfg(all(not(ndi_link_static), feature = "file-io"))]
pub(in crate::sys) use shared::{confirm_selected, Module, Search};

#[cfg(ndi_link_static)]
use linked::Linked as Provider;
#[cfg(all(ndi_link_static, feature = "file-io"))]
pub(in crate::sys) use linked::Linked;

#[cfg(not(ndi_link_static))]
mod shared {
    use std::{
        ffi::{c_void, CStr, OsStr},
        fmt,
        mem::ManuallyDrop,
        path::{Path, PathBuf},
        ptr::NonNull,
        sync::{Mutex, PoisonError},
    };

    use super::{ExplicitLibrary, Resolver, Runtime};
    use crate::{
        capabilities::{SdkEdition, SdkLinkage},
        runtime::RuntimeOptions,
        Error, LibraryKind, LoadAttempt, LoadFailure, LoadOrigin as Origin, LoadRefusal, Result,
        SelectionFailure,
    };

    /// The environment variable naming the directory of an installed NDI runtime.
    const RUNTIME_DIRECTORY: &str = "NDI_RUNTIME_DIR_V6";

    /// A symbol every NDI core library defines, used to identify the opened module.
    const CORE_ANCHOR: &CStr = c"NDIlib_version";

    /// An opened native library that stays loaded for the life of the process.
    ///
    /// The handle is never closed: native code may have started threads or
    /// registered callbacks and thread-local destructors that outlive any one
    /// caller, so unloading can never be proven safe.
    pub(crate) struct Module {
        library: ManuallyDrop<libloading::Library>,
        /// The platform loader's handle, which it returns again whenever the
        /// same module is opened, under any name.
        handle: usize,
        base: usize,
        path: PathBuf,
    }

    /// Identity of one pinned module, independent of its symbols and path aliases.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(in crate::sys) struct ModuleId(usize);

    /// Why a location cannot be used, and the module it left loaded when it
    /// got as far as loading one.
    pub(super) struct Refusal {
        reason: LoadRefusal,
        loaded: Option<Loaded>,
    }

    /// A module that stays loaded although it was refused.
    struct Loaded {
        handle: usize,
        /// The file it was loaded from, when that could be established.
        path: Option<PathBuf>,
    }

    impl Refusal {
        /// A location that loaded nothing.
        fn unloaded(reason: LoadRefusal) -> Self {
            Self {
                reason,
                loaded: None,
            }
        }
    }

    impl Module {
        /// Open `location` and identify the module through `anchor`, a symbol
        /// it must define.
        ///
        /// The error carries the reason, including the operating system's own
        /// text, and the identity of the module when the location was loaded
        /// before it was refused: it stays loaded.
        pub(super) fn open(
            location: &OsStr,
            anchor: &'static CStr,
        ) -> std::result::Result<Self, Refusal> {
            let (library, handle) = open_library(location).map_err(|error| {
                Refusal::unloaded(LoadRefusal::PlatformLoader {
                    detail: error_chain(&error),
                })
            })?;
            let library = ManuallyDrop::new(library);
            let refused = |reason: LoadRefusal, path: Option<PathBuf>| Refusal {
                reason,
                loaded: Some(Loaded { handle, path }),
            };
            // SAFETY: the symbol is read only as an address.
            let address = unsafe { library.get::<*const c_void>(anchor.to_bytes_with_nul()) }
                .map(|symbol| *symbol)
                .map_err(|error| {
                    let reason = LoadRefusal::MissingAnchor {
                        symbol: anchor,
                        detail: error_chain(&error),
                    };
                    refused(reason, None)
                })?;
            let (base, path) = containing_module(address)
                .map_err(|detail| refused(LoadRefusal::NotIdentifiable { detail }, None))?;
            let path = path.canonicalize().unwrap_or(path);
            #[cfg(windows)]
            let path = ordinary_module_path(path);
            if Path::new(location).is_absolute() && !same_path(Path::new(location), &path) {
                let reason = LoadRefusal::ResolvedToAnotherFile {
                    resolved: path.clone(),
                };
                return Err(refused(reason, Some(path)));
            }
            Ok(Self {
                library,
                handle,
                base,
                path,
            })
        }

        /// The file the module was loaded from.
        pub(crate) fn path(&self) -> &Path {
            &self.path
        }

        pub(in crate::sys) fn id(&self) -> ModuleId {
            ModuleId(self.base)
        }

        pub(super) fn library_path(&self) -> Option<PathBuf> {
            Some(self.path().to_owned())
        }

        /// Where a lookup through this module's handle finds `name`, and the
        /// load address of the module that defines it there.
        ///
        /// A handle lookup searches the module and then the libraries it
        /// depends on, so the definition may lie outside this module.
        fn lookup(
            &self,
            name: &CStr,
        ) -> std::result::Result<Option<(NonNull<c_void>, usize)>, LoadRefusal> {
            // SAFETY: the symbol is read only as an address; a caller that
            // goes on to call it applies the function type.
            let symbol = unsafe { self.library.get::<*mut c_void>(name.to_bytes_with_nul()) };
            let Some(address) = symbol.ok().and_then(|symbol| NonNull::new(*symbol)) else {
                return Ok(None);
            };
            let (base, _) = containing_module(address.as_ptr())
                .map_err(|detail| LoadRefusal::NotIdentifiable { detail })?;
            Ok(Some((address, base)))
        }

        /// The module this handle finds for `name`: its own definition or,
        /// where the platform's handle lookup reaches them, the one a library it
        /// depends on supplies. `None` when the lookup finds neither.
        #[cfg(feature = "file-io")]
        pub(in crate::sys) fn bound_module(
            &self,
            name: &CStr,
        ) -> std::result::Result<Option<ModuleId>, LoadRefusal> {
            Ok(self.lookup(name)?.map(|(_, base)| ModuleId(base)))
        }

        pub(super) const LINKAGE: SdkLinkage = SdkLinkage::Shared;

        /// Confirm that a request naming the library at `requested` is met by
        /// this module.
        pub(super) fn confirm_location(
            &self,
            requested: Option<ExplicitLibrary<'_>>,
        ) -> Result<()> {
            confirm_selected(LibraryKind::Runtime, &self.path, requested)
        }

        /// A module is an Advanced runtime when it exports both Advanced entry
        /// points; its file name and the caller's request are not evidence.
        pub(super) fn edition(&self) -> std::result::Result<SdkEdition, LoadRefusal> {
            // Only shared libraries need symbol-based edition detection. A
            // linked provider already knows the archive the build selected.
            let sender_configuration = self.resolve(c"NDIlib_send_create_v2")?.is_some();
            let receiver_configuration = self.resolve(c"NDIlib_recv_create_v4")?.is_some();
            Ok(if sender_configuration && receiver_configuration {
                SdkEdition::Advanced
            } else {
                SdkEdition::Standard
            })
        }
    }

    impl Resolver for Module {
        fn resolve(
            &self,
            name: &'static CStr,
        ) -> std::result::Result<Option<NonNull<c_void>>, LoadRefusal> {
            let Some((address, base)) = self.lookup(name)? else {
                return Ok(None);
            };
            // Accept a symbol only when this module itself defines it.
            if ModuleId(base) != self.id() {
                return Err(LoadRefusal::EntryPointOutsideModule {
                    symbol: name,
                    module: self.path.clone(),
                });
            }
            Ok(Some(address))
        }
    }

    impl Runtime {
        /// Search for the runtime library `options` describe: the requested
        /// edition's file name, or, without a request, the file names of both
        /// editions at each location.
        pub(in crate::sys) fn load(
            options: &RuntimeOptions,
            explicit: Option<ExplicitLibrary<'_>>,
        ) -> Result<Self> {
            let editions = match options.edition {
                Some(requested) => vec![requested],
                None => super::DEFAULT_SEARCH.to_vec(),
            };
            let names: Vec<&str> = editions
                .iter()
                .map(|&edition| library_name(edition))
                .collect();
            let description = match options.edition {
                Some(requested) => format!("{requested:?} runtime"),
                None => "NDI runtime".to_owned(),
            };
            // Without a request any edition is acceptable, as it is for Standard.
            let requested = options.edition.unwrap_or(SdkEdition::Standard);
            Search {
                kind: LibraryKind::Runtime,
                explicit,
                companion: None,
                names: &names,
                anchor: CORE_ANCHOR,
                description: &description,
            }
            .open(|module| Runtime::describe(module, requested))
        }
    }

    /// Confirm that a request naming `requested` is met by the library at
    /// `selected`, which is pinned for the process and cannot be replaced.
    pub(in crate::sys) fn confirm_selected(
        library: LibraryKind,
        selected: &Path,
        requested: Option<ExplicitLibrary<'_>>,
    ) -> Result<()> {
        let Some(requested) = requested else {
            return Ok(());
        };
        let requested = requested.path();
        if same_path(requested, selected) {
            Ok(())
        } else {
            Err(Error::RuntimeConflict(SelectionFailure::different_library(
                library, selected, requested,
            )))
        }
    }

    /// A native library to locate and open.
    pub(in crate::sys) struct Search<'a> {
        /// Which library is being sought.
        pub(in crate::sys) kind: LibraryKind,
        /// The only acceptable location, when the caller named one.
        pub(in crate::sys) explicit: Option<ExplicitLibrary<'a>>,
        /// The selected module this library accompanies; its directory is
        /// searched first, and core imports must resolve to its identity.
        pub(in crate::sys) companion: Option<&'a Module>,
        /// The library's file names on this target, tried in this order at
        /// each location.
        pub(in crate::sys) names: &'a [&'a str],
        /// A symbol the library must define; it identifies the opened module.
        pub(in crate::sys) anchor: &'static CStr,
        /// What is being looked for, as the error names it.
        pub(in crate::sys) description: &'a str,
    }

    impl Search<'_> {
        /// Open the first location that loads and that `accept` can use.
        ///
        /// # Errors
        ///
        /// [`Error::RuntimeConflict`] for an explicit runtime or a File search
        /// that would bind to another globally visible runtime;
        /// otherwise [`Error::RuntimeLoadFailed`] naming every location tried
        /// and the reason each was rejected.
        pub(in crate::sys) fn open<T>(
            &self,
            accept: impl Fn(Module) -> std::result::Result<T, LoadRefusal>,
        ) -> Result<T> {
            let executable = std::env::current_exe().ok();
            let runtime_directory = std::env::var_os(RUNTIME_DIRECTORY)
                .filter(|directory| !directory.is_empty())
                .map(PathBuf::from);
            let loader_directories = if cfg!(windows) {
                std::env::var_os("PATH")
            } else if cfg!(target_vendor = "apple") {
                std::env::var_os("DYLD_LIBRARY_PATH")
            } else {
                None
            };
            let fallback_directories = if cfg!(target_vendor = "apple") {
                std::env::var_os("DYLD_FALLBACK_LIBRARY_PATH")
            } else {
                None
            };
            let plan = search_plan(
                self.explicit,
                self.companion.map(Module::path).and_then(Path::parent),
                executable.as_deref().and_then(Path::parent),
                runtime_directory.as_deref(),
                loader_directories.as_deref(),
                fallback_directories.as_deref(),
                self.names,
            );
            let mut failures = Vec::with_capacity(plan.len());
            for candidate in plan {
                let refusal = match candidate.open(self) {
                    Ok(module) => {
                        // `accept` consumes the module, so its identity is taken first.
                        let loaded = Loaded {
                            handle: module.handle,
                            path: Some(module.path.clone()),
                        };
                        match accept(module) {
                            Ok(opened) => return Ok(opened),
                            Err(reason) => Refusal {
                                reason,
                                loaded: Some(loaded),
                            },
                        }
                    }
                    Err(refusal) => refusal,
                };
                if let LoadRefusal::ForeignRuntimeInScope { module } = &refusal.reason {
                    if self.explicit.is_some() || self.kind == LibraryKind::File {
                        return Err(Error::RuntimeConflict(SelectionFailure::foreign(
                            self.kind,
                            self.companion.map(Module::path),
                            self.explicit.map(ExplicitLibrary::path),
                            module,
                            &candidate.location,
                            candidate.origin,
                        )));
                    }
                }
                failures.push(candidate.describe(refusal));
            }
            Err(Error::RuntimeLoadFailed(LoadFailure::searched(
                self.kind,
                self.description,
                failures,
            )))
        }

        /// ELF global definitions take precedence over a module's own imports,
        /// even when that module is opened locally. Check before opening it:
        /// constructors and PLT self-calls could otherwise reach another core.
        #[cfg(all(unix, not(target_vendor = "apple")))]
        fn check_global_scope(&self, location: &OsStr) -> std::result::Result<(), LoadRefusal> {
            let Some(global) = global_definition(CORE_ANCHOR)? else {
                return Ok(());
            };
            let same_module = match self.companion {
                Some(selected) => global.module == selected.id(),
                None => same_loaded_module(location, global.path.as_os_str()),
            };
            if same_module {
                Ok(())
            } else {
                Err(LoadRefusal::ForeignRuntimeInScope {
                    module: global.path,
                })
            }
        }
    }

    /// The modules that were loaded and then refused, by the platform loader's
    /// handle, each with the file it was first loaded from.
    ///
    /// A module is never closed, so a refused one stays loaded, whether it was
    /// refused before or after it could be identified. The platform loader
    /// then answers a later request for a library by name with that module
    /// whenever the name matches the one the module carries, instead of
    /// searching for another file.
    static REJECTED: Mutex<Vec<(usize, PathBuf)>> = Mutex::new(Vec::new());

    /// One place the runtime library may be loaded from.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct Candidate {
        pub(super) location: PathBuf,
        pub(super) origin: Origin,
    }

    impl fmt::Display for Candidate {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{} ({})", self.location.display(), self.origin)
        }
    }

    impl Candidate {
        fn open(&self, search: &Search<'_>) -> std::result::Result<Module, Refusal> {
            // Environment directories name exact absolute files. Refuse
            // relative/empty entries before opening anything: they never ask
            // for code relative to the working directory. Only Unix's native
            // search and Apple's @rpath, or Windows' System32-only search,
            // may resolve a non-absolute candidate through the platform loader.
            if !self.location.is_absolute()
                && matches!(
                    self.origin,
                    Origin::RuntimeDirectory
                        | Origin::PathDirectory
                        | Origin::DyldLibraryDirectory
                        | Origin::DyldFallbackLibraryDirectory
                )
            {
                return Err(Refusal::unloaded(LoadRefusal::RelativeSearchDirectory));
            }
            // A directory candidate names one exact file. Skipping an absent
            // one keeps the loader from searching elsewhere for that path.
            if matches!(
                self.origin,
                Origin::Companion
                    | Origin::Executable
                    | Origin::RuntimeDirectory
                    | Origin::PathDirectory
                    | Origin::DyldLibraryDirectory
            ) && !self.location.is_file()
            {
                return Err(Refusal::unloaded(LoadRefusal::Absent));
            }
            #[cfg(all(unix, not(target_vendor = "apple")))]
            search
                .check_global_scope(self.location.as_os_str())
                .map_err(Refusal::unloaded)?;
            Module::open(self.location.as_os_str(), search.anchor)
        }

        /// Why this candidate cannot be used.
        ///
        /// A candidate that names a file reports that file's own defect. One
        /// the platform loader resolved by name reports which file that was,
        /// and whether it is a module refused before: the loader keeps
        /// returning such a module for its name, so the search cannot reach
        /// any other library of that name.
        fn describe(&self, refusal: Refusal) -> LoadAttempt {
            let Refusal { reason, loaded } = refusal;
            let Some(loaded) = loaded else {
                return LoadAttempt::new(self.location.clone(), self.origin, reason, false, None);
            };
            let refused_before = {
                let mut rejected = REJECTED.lock().unwrap_or_else(PoisonError::into_inner);
                let earlier = rejected
                    .iter()
                    .find(|(handle, _)| *handle == loaded.handle)
                    .map(|(_, path)| path.clone());
                if earlier.is_none() {
                    let path = loaded.path.clone().unwrap_or_else(|| self.location.clone());
                    rejected.push((loaded.handle, path));
                }
                earlier
            };
            let reason = match (self.origin, refused_before) {
                (Origin::LoaderSearch, Some(path)) => LoadRefusal::AlreadyRefused {
                    path,
                    refusal: Box::new(reason),
                },
                _ => reason,
            };
            LoadAttempt::new(
                self.location.clone(),
                self.origin,
                reason,
                true,
                loaded.path,
            )
        }
    }

    /// The ordered places to look for the library under any of `names`.
    ///
    /// An explicit path is exclusive. Otherwise a library installed beside the
    /// one it accompanies wins over one shipped beside the executable, which
    /// wins over an installed runtime, which wins over whatever the platform
    /// loader finds on its own. Each location is tried for every name, in
    /// order, before the next location: where a library is installed matters
    /// more than which of the names it has.
    /// Apple default searches refuse all locations before opening any when
    /// either dyld library directory list contains a relative or empty entry.
    /// The current environment is checked; callers must configure dyld before launch.
    pub(super) fn search_plan(
        explicit: Option<ExplicitLibrary<'_>>,
        companion_directory: Option<&Path>,
        executable_directory: Option<&Path>,
        runtime_directory: Option<&Path>,
        loader_directories: Option<&OsStr>,
        fallback_directories: Option<&OsStr>,
        names: &[&str],
    ) -> Vec<Candidate> {
        if let Some(path) = explicit {
            return vec![Candidate {
                location: path.path().to_owned(),
                origin: Origin::Explicit,
            }];
        }
        // Both dyld lists can substitute a leaf name, including for @rpath.
        // Refuse unsafe entries before any open can run an initializer. The
        // fallback list is preflighted only: its absolute entries are not
        // ordinary exact candidates.
        #[cfg(target_vendor = "apple")]
        {
            let refused: Vec<_> = [
                (loader_directories, Origin::DyldLibraryDirectory),
                (fallback_directories, Origin::DyldFallbackLibraryDirectory),
            ]
            .into_iter()
            .flat_map(|(directories, origin)| {
                directories.into_iter().flat_map(move |directories| {
                    std::env::split_paths(directories)
                        .filter(|directory| !directory.is_absolute())
                        .map(move |location| Candidate { location, origin })
                })
            })
            .collect();
            if !refused.is_empty() {
                return refused;
            }
        }
        #[cfg(not(target_vendor = "apple"))]
        let _ = fallback_directories;
        let mut plan: Vec<Candidate> = Vec::new();
        let mut add = |location: PathBuf, origin| {
            if !plan.iter().any(|earlier| earlier.location == location) {
                plan.push(Candidate { location, origin });
            }
        };
        let directories = [
            (companion_directory, Origin::Companion),
            (executable_directory, Origin::Executable),
            (runtime_directory, Origin::RuntimeDirectory),
        ];
        for (directory, origin) in directories {
            let Some(directory) = directory else { continue };
            if origin == Origin::RuntimeDirectory && !directory.is_absolute() {
                // Retain the refused input for diagnostics, without creating
                // any candidate underneath the relative directory.
                add(directory.to_owned(), origin);
                continue;
            }
            for name in names {
                add(directory.join(name), origin);
            }
        }
        // Application bundles locate their libraries through the run path.
        if cfg!(target_vendor = "apple") {
            for name in names {
                add(Path::new("@rpath").join(name), Origin::LoaderSearch);
            }
        }
        // Expand only absolute environment directories on Windows and Apple
        // targets; their bare-name searches would also trust the working
        // directory. Preserve refused inputs in the plan for diagnostics.
        if cfg!(any(windows, target_vendor = "apple")) {
            let origin = if cfg!(windows) {
                Origin::PathDirectory
            } else {
                Origin::DyldLibraryDirectory
            };
            if let Some(directories) = loader_directories {
                for directory in std::env::split_paths(directories) {
                    if !directory.is_absolute() {
                        add(directory, origin);
                        continue;
                    }
                    for name in names {
                        add(directory.join(name), origin);
                    }
                }
            }
        }
        if !cfg!(target_vendor = "apple") {
            // Windows limits this final leaf to System32; non-Apple Unix
            // retains its native rpath/environment/cache search.
            for name in names {
                add(PathBuf::from(name), Origin::LoaderSearch);
            }
        }
        plan
    }

    /// The runtime library's file name for this target.
    pub(super) const fn library_name(edition: SdkEdition) -> &'static str {
        super::library_file(super::LibraryFile::Runtime(edition))
    }

    /// Present a canonical Windows file in ordinary form when Win32 parsing
    /// preserves its name. Keep extended syntax for names that need it.
    #[cfg(windows)]
    fn ordinary_module_path(path: PathBuf) -> PathBuf {
        use std::{
            ffi::{OsStr, OsString},
            os::windows::ffi::{OsStrExt, OsStringExt},
            path::{Component, Prefix},
        };

        fn needs_verbatim(component: &OsStr) -> bool {
            let units: Vec<_> = component.encode_wide().collect();
            if units.last().is_some_and(|&unit| unit == 32 || unit == 46)
                || units
                    .iter()
                    .any(|&unit| unit < 32 || [34, 42, 47, 58, 60, 62, 63, 92, 124].contains(&unit))
            {
                return true;
            }
            // DOS device names remain reserved with an extension, including
            // COM/LPT names using superscript 1, 2 or 3. Spaces before an
            // extension do not make a device name ordinary.
            let end = units
                .iter()
                .position(|&unit| unit == 46)
                .unwrap_or(units.len());
            let mut stem = &units[..end];
            while stem.last() == Some(&32) {
                stem = &stem[..stem.len() - 1];
            }
            let upper: Vec<_> = stem
                .iter()
                .map(|&unit| {
                    if (97..=122).contains(&unit) {
                        unit - 32
                    } else {
                        unit
                    }
                })
                .collect();
            let named_device = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
                .iter()
                .any(|name| upper.iter().copied().eq(name.encode_utf16()));
            named_device
                || (upper.len() == 4
                    && (upper[..3] == [67, 79, 77] || upper[..3] == [76, 80, 84])
                    && ((49..=57).contains(&upper[3]) || [0xb9, 0xb2, 0xb3].contains(&upper[3])))
        }

        let Some(Component::Prefix(prefix)) = path.components().next() else {
            return path;
        };
        let units: Vec<_> = path.as_os_str().encode_wide().collect();
        let plain = match prefix.kind() {
            Prefix::VerbatimDisk(_) => OsString::from_wide(&units[4..]),
            Prefix::VerbatimUNC(server, share) => {
                if needs_verbatim(server) || needs_verbatim(share) {
                    return path;
                }
                let mut plain = OsString::from(r"\\");
                plain.push(OsString::from_wide(&units[8..]));
                plain
            }
            _ => return path,
        };
        if path
            .components()
            .any(|component| matches!(component, Component::Normal(name) if needs_verbatim(name)))
        {
            return path;
        }
        // MAX_PATH includes the terminator. Longer ordinary paths depend on
        // process/OS opt-ins; ask Win32 directly rather than assume them.
        let mut plain_units: Vec<_> = plain.encode_wide().collect();
        plain_units.push(0);
        if plain_units.len() > 260 {
            use windows_sys::Win32::Storage::FileSystem::{
                GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
            };
            // SAFETY: the buffer is a live, NUL-terminated UTF-16 path.
            if unsafe { GetFileAttributesW(plain_units.as_ptr()) } == INVALID_FILE_ATTRIBUTES {
                return path;
            }
        }
        PathBuf::from(plain)
    }

    fn same_path(a: &Path, b: &Path) -> bool {
        #[cfg(all(unix, not(target_vendor = "apple")))]
        {
            use std::os::unix::fs::MetadataExt;
            if let (Ok(a), Ok(b)) = (a.metadata(), b.metadata()) {
                return a.dev() == b.dev() && a.ino() == b.ino();
            }
        }
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        }
    }

    #[cfg(all(unix, not(target_vendor = "apple")))]
    struct GlobalDefinition {
        module: ModuleId,
        path: PathBuf,
    }

    /// Identify the global definition through the same module lookup used by
    /// opened libraries, keeping all `dladdr` access in `containing_module`.
    #[cfg(all(unix, not(target_vendor = "apple")))]
    fn global_definition(
        name: &CStr,
    ) -> std::result::Result<Option<GlobalDefinition>, LoadRefusal> {
        // SAFETY: a lookup of a NUL-terminated name in the default scope. The
        // address is inspected, never called through this pointer.
        let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        if address.is_null() {
            return Ok(None);
        }
        let (base, path) =
            containing_module(address).map_err(|detail| LoadRefusal::NotIdentifiable { detail })?;
        Ok(Some(GlobalDefinition {
            module: ModuleId(base),
            path: path.canonicalize().unwrap_or(path),
        }))
    }

    /// Compare loader handles without loading either name. In particular, a
    /// bare name may resolve to a preloaded module through its SONAME, and
    /// aliases may name that module without sharing its original path spelling.
    #[cfg(all(unix, not(target_vendor = "apple")))]
    fn same_loaded_module(a: &OsStr, b: &OsStr) -> bool {
        use libloading::os::unix::Library;
        let probe = |location: &OsStr| {
            // SAFETY: RTLD_NOLOAD only acquires a reference to an existing
            // module; it cannot load code or run constructors. RTLD_LAZY does
            // not promote existing lazy relocations to eager binding.
            let library =
                unsafe { Library::open(Some(location), libc::RTLD_NOLOAD | libc::RTLD_LAZY) }
                    .ok()?;
            let handle = library.into_raw();
            // SAFETY: this is the reference `into_raw` just released. Its drop
            // balances only the no-load probe, leaving the original load intact.
            Some((unsafe { Library::from_raw(handle) }, handle))
        };
        match (probe(a), probe(b)) {
            (Some((_, a)), Some((_, b))) => a == b,
            _ => false,
        }
    }

    /// An error with every cause the operating system reported.
    fn error_chain(error: &dyn std::error::Error) -> String {
        let mut text = error.to_string();
        let mut cause = error.source();
        while let Some(source) = cause {
            text.push_str(": ");
            text.push_str(&source.to_string());
            cause = source.source();
        }
        text
    }

    /// Load `location`, returning the library with the platform loader's
    /// handle for it.
    #[cfg(unix)]
    fn open_library(
        location: &OsStr,
    ) -> std::result::Result<(libloading::Library, usize), libloading::Error> {
        use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
        // SAFETY: loading runs the library's constructors. Callers select a
        // trusted NDI runtime; eager binding reports missing dependencies here
        // rather than at the first call.
        let handle = unsafe { Library::open(Some(location), RTLD_NOW | RTLD_LOCAL) }?.into_raw();
        // SAFETY: `handle` is the open handle `into_raw` has just released.
        let library = unsafe { Library::from_raw(handle) };
        Ok((library.into(), handle as usize))
    }

    /// Load `location`, returning the library with the platform loader's
    /// handle for it.
    #[cfg(windows)]
    fn open_library(
        location: &OsStr,
    ) -> std::result::Result<(libloading::Library, usize), libloading::Error> {
        use libloading::os::windows::{
            Library, LOAD_LIBRARY_SEARCH_SYSTEM32, LOAD_WITH_ALTERED_SEARCH_PATH,
        };
        // Exact paths resolve dependencies beside the library. A final leaf
        // may match a loaded module or System32, never the working directory
        // or PATH (whose absolute entries were already expanded).
        let flags = if Path::new(location).is_absolute() {
            LOAD_WITH_ALTERED_SEARCH_PATH
        } else {
            LOAD_LIBRARY_SEARCH_SYSTEM32
        };
        // SAFETY: loading runs the library's initialization routine. Callers
        // select a trusted NDI runtime.
        let handle = unsafe { Library::load_with_flags(location, flags) }?.into_raw();
        // SAFETY: `handle` is the open handle `into_raw` has just released.
        let library = unsafe { Library::from_raw(handle) };
        Ok((library.into(), handle as usize))
    }

    /// The load address and file of the module that defines `address`.
    #[cfg(unix)]
    fn containing_module(address: *const c_void) -> std::result::Result<(usize, PathBuf), String> {
        use std::os::unix::ffi::OsStrExt;
        let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
        // SAFETY: `info` is writable storage for one Dl_info; dladdr only reads
        // the loader's module list for `address`.
        if unsafe { libc::dladdr(address, info.as_mut_ptr()) } == 0 {
            return Err("the loader cannot identify the module defining a symbol".into());
        }
        // SAFETY: dladdr reported success, so it initialized `info`.
        let info = unsafe { info.assume_init() };
        if info.dli_fname.is_null() || info.dli_fbase.is_null() {
            return Err("the loader reported an incomplete module identity".into());
        }
        // SAFETY: a non-null dli_fname is a NUL-terminated string owned by the
        // loader for as long as the module stays loaded.
        let file = unsafe { CStr::from_ptr(info.dli_fname) };
        Ok((
            info.dli_fbase as usize,
            PathBuf::from(OsStr::from_bytes(file.to_bytes())),
        ))
    }

    /// The load address and file of the module that defines `address`.
    #[cfg(windows)]
    fn containing_module(address: *const c_void) -> std::result::Result<(usize, PathBuf), String> {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        use windows_sys::Win32::System::LibraryLoader::{
            GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        };
        const LONGEST_PATH: usize = 1 << 15;
        let mut module = std::ptr::null_mut();
        // SAFETY: `module` is writable storage for one handle. The reference
        // count is left unchanged, so the handle needs no release.
        let found = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                address.cast(),
                &mut module,
            )
        };
        if found == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut buffer = vec![0u16; 260];
        loop {
            // SAFETY: `buffer` is writable for the length passed.
            let written =
                unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) }
                    as usize;
            if written == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if written < buffer.len() {
                return Ok((
                    module as usize,
                    PathBuf::from(OsString::from_wide(&buffer[..written])),
                ));
            }
            if buffer.len() >= LONGEST_PATH {
                return Err("the module path exceeds the longest Windows path".into());
            }
            buffer.resize(buffer.len() * 2, 0);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn runtime_directory() -> &'static Path {
            Path::new(if cfg!(windows) {
                "C:/runtime"
            } else {
                "/runtime"
            })
        }

        fn locations(plan: &[Candidate]) -> Vec<(PathBuf, Origin)> {
            plan.iter()
                .map(|candidate| (candidate.location.clone(), candidate.origin))
                .collect()
        }

        #[test]
        fn an_explicit_path_is_the_only_candidate() {
            let path = runtime_directory().join("libndi.so.6");
            let plan = search_plan(
                Some(ExplicitLibrary::new(&path).unwrap()),
                Some(Path::new("/core")),
                Some(Path::new("/app")),
                Some(runtime_directory()),
                None,
                None,
                &["libndi.so.6"],
            );
            assert_eq!(locations(&plan), [(path, Origin::Explicit)]);
        }

        #[test]
        fn a_bundled_library_precedes_the_installed_runtime_and_the_loader_search() {
            let plan = search_plan(
                None,
                None,
                Some(Path::new("/app")),
                Some(runtime_directory()),
                None,
                None,
                &["libndi.so.6"],
            );
            let mut expected = vec![
                (PathBuf::from("/app/libndi.so.6"), Origin::Executable),
                (
                    runtime_directory().join("libndi.so.6"),
                    Origin::RuntimeDirectory,
                ),
            ];
            if cfg!(target_vendor = "apple") {
                expected.push((PathBuf::from("@rpath/libndi.so.6"), Origin::LoaderSearch));
            }
            if !cfg!(target_vendor = "apple") {
                expected.push((PathBuf::from("libndi.so.6"), Origin::LoaderSearch));
            }
            assert_eq!(locations(&plan), expected);
        }

        #[test]
        fn the_loader_search_remains_when_no_directory_is_known() {
            let plan = search_plan(None, None, None, None, None, None, &["libndi.so.6"]);
            assert_eq!(
                plan.last().map(|candidate| candidate.origin),
                Some(Origin::LoaderSearch)
            );
            assert!(plan
                .iter()
                .all(|candidate| candidate.origin == Origin::LoaderSearch));
        }

        #[cfg(target_vendor = "apple")]
        #[test]
        fn apple_search_contains_only_absolute_files_and_rpaths() {
            let directories = OsStr::new("/opt/ndi:/Library/NDI");
            let plan = search_plan(
                None,
                None,
                None,
                None,
                Some(directories),
                None,
                &["libndi.dylib", "libndi_advanced.dylib"],
            );
            assert!(plan.iter().all(|candidate| {
                candidate.location.is_absolute() || candidate.location.starts_with("@rpath")
            }));
            assert_eq!(plan[2].location, Path::new("/opt/ndi/libndi.dylib"));
            assert_eq!(plan[2].origin, Origin::DyldLibraryDirectory);
        }

        #[cfg(any(windows, target_vendor = "apple"))]
        #[test]
        fn loader_environment_directories_are_expanded_or_refused_before_opening() {
            let absolute = runtime_directory();
            let directories =
                std::env::join_paths([absolute, Path::new("relative"), Path::new("")]).unwrap();
            let plan = search_plan(
                None,
                None,
                None,
                None,
                Some(&directories),
                None,
                &["first", "second"],
            );
            let origin = if cfg!(windows) {
                Origin::PathDirectory
            } else {
                Origin::DyldLibraryDirectory
            };
            let entries: Vec<_> = plan
                .iter()
                .filter(|candidate| candidate.origin == origin)
                .collect();
            let refused = if cfg!(windows) {
                assert_eq!(entries.len(), 4);
                assert_eq!(entries[0].location, absolute.join("first"));
                assert_eq!(entries[1].location, absolute.join("second"));
                &entries[2..]
            } else {
                // No absolute or @rpath attempt may precede the refusal:
                // dyld could apply the relative override to that attempt.
                assert_eq!(plan.len(), 2);
                &entries[..]
            };
            assert_eq!(refused[0].location, Path::new("relative"));
            assert!(refused[1].location.as_os_str().is_empty());
            let search = Search {
                kind: LibraryKind::Runtime,
                explicit: None,
                companion: None,
                names: &["first", "second"],
                anchor: CORE_ANCHOR,
                description: "NDI runtime",
            };
            for candidate in refused {
                let refusal = match candidate.open(&search) {
                    Ok(_) => panic!("relative search directory was opened"),
                    Err(refusal) => refusal,
                };
                assert!(refusal.loaded.is_none());
                assert_eq!(refusal.reason, LoadRefusal::RelativeSearchDirectory);
                let diagnostic = candidate.describe(refusal).to_string();
                assert!(diagnostic.contains(&origin.to_string()), "{diagnostic}");
                assert!(diagnostic.contains("absolute and nonempty"), "{diagnostic}");
            }
        }

        #[cfg(target_vendor = "apple")]
        #[test]
        fn unsafe_dyld_entries_block_every_default_location_before_opening() {
            for directories in ["relative", "", "/trusted:relative", "/trusted:"] {
                let plan = search_plan(
                    None,
                    Some(Path::new("/core")),
                    Some(Path::new("/app")),
                    Some(runtime_directory()),
                    Some(OsStr::new(directories)),
                    None,
                    &["libndi.dylib", "libndi_advanced.dylib"],
                );
                assert_eq!(plan.len(), 1, "{directories:?}: {plan:?}");
                assert_eq!(plan[0].origin, Origin::DyldLibraryDirectory);
                assert!(!plan[0].location.is_absolute());
            }
        }

        #[cfg(target_vendor = "apple")]
        #[test]
        fn unsafe_dyld_fallback_entries_block_every_default_location_before_opening() {
            for primary in [None, Some(OsStr::new("/trusted"))] {
                for directories in ["relative", "", "/trusted:relative", "/trusted:"] {
                    let plan = search_plan(
                        None,
                        Some(Path::new("/core")),
                        Some(Path::new("/app")),
                        Some(runtime_directory()),
                        primary,
                        Some(OsStr::new(directories)),
                        &["libndi.dylib", "libndi_advanced.dylib"],
                    );
                    assert_eq!(plan.len(), 1, "{directories:?}: {plan:?}");
                    assert_eq!(plan[0].origin, Origin::DyldFallbackLibraryDirectory);
                    assert!(!plan[0].location.is_absolute());
                }
            }
            let baseline = search_plan(None, None, None, None, None, None, &["libndi.dylib"]);
            let safe_fallback = search_plan(
                None,
                None,
                None,
                None,
                None,
                Some(OsStr::new("/trusted")),
                &["libndi.dylib"],
            );
            assert_eq!(locations(&baseline), locations(&safe_fallback));
        }

        #[test]
        fn a_relative_runtime_directory_is_recorded_but_never_opened() {
            let plan = search_plan(
                None,
                None,
                None,
                Some(Path::new("relative")),
                None,
                None,
                &["libndi.so.6", "libndi_advanced.so.6"],
            );
            let refused = &plan[0];
            assert_eq!(refused.location, Path::new("relative"));
            assert_eq!(refused.origin, Origin::RuntimeDirectory);
            assert!(!plan
                .iter()
                .any(|candidate| candidate.location.parent() == Some(Path::new("relative"))));
            let search = Search {
                kind: LibraryKind::Runtime,
                explicit: None,
                companion: None,
                names: &["libndi.so.6"],
                anchor: CORE_ANCHOR,
                description: "NDI runtime",
            };
            let refusal = match refused.open(&search) {
                Ok(_) => panic!("relative runtime directory was opened"),
                Err(refusal) => refusal,
            };
            assert!(refusal.loaded.is_none());
            assert_eq!(refusal.reason, LoadRefusal::RelativeSearchDirectory);
            let attempt = refused.describe(refusal);
            assert_eq!(attempt.origin(), Origin::RuntimeDirectory);
            let diagnostic = attempt.to_string();
            assert!(diagnostic.contains("NDI_RUNTIME_DIR_V6"), "{diagnostic}");
            assert!(diagnostic.contains("must be absolute"), "{diagnostic}");
            assert_eq!(plan.last().unwrap().origin, Origin::LoaderSearch);
        }

        #[test]
        fn one_directory_named_twice_is_tried_once() {
            let directory = std::env::current_dir().unwrap().join("app");
            let plan = search_plan(
                None,
                None,
                Some(directory.as_path()),
                Some(directory.as_path()),
                None,
                None,
                &["libndi.so.6"],
            );
            let bundled = plan
                .iter()
                .filter(|candidate| candidate.location == directory.join("libndi.so.6"))
                .count();
            assert_eq!(bundled, 1);
        }

        #[test]
        fn a_companion_library_is_sought_beside_its_runtime_first() {
            let plan = search_plan(
                None,
                Some(Path::new("/core")),
                Some(Path::new("/app")),
                Some(runtime_directory()),
                None,
                None,
                &["libndifile.so.6"],
            );
            assert_eq!(
                locations(&plan)[..3],
                [
                    (PathBuf::from("/core/libndifile.so.6"), Origin::Companion),
                    (PathBuf::from("/app/libndifile.so.6"), Origin::Executable),
                    (
                        runtime_directory().join("libndifile.so.6"),
                        Origin::RuntimeDirectory
                    ),
                ]
            );
            assert_eq!(
                plan.last().map(|candidate| candidate.origin),
                Some(Origin::LoaderSearch)
            );
        }

        #[test]
        fn every_name_is_tried_at_a_location_before_the_next_location() {
            let plan = search_plan(
                None,
                None,
                Some(Path::new("/app")),
                Some(runtime_directory()),
                None,
                None,
                &["libndi.so.6", "libndi_advanced.so.6"],
            );
            let mut expected = vec![
                (PathBuf::from("/app/libndi.so.6"), Origin::Executable),
                (
                    PathBuf::from("/app/libndi_advanced.so.6"),
                    Origin::Executable,
                ),
                (
                    runtime_directory().join("libndi.so.6"),
                    Origin::RuntimeDirectory,
                ),
                (
                    runtime_directory().join("libndi_advanced.so.6"),
                    Origin::RuntimeDirectory,
                ),
            ];
            if cfg!(target_vendor = "apple") {
                expected.push((PathBuf::from("@rpath/libndi.so.6"), Origin::LoaderSearch));
                expected.push((
                    PathBuf::from("@rpath/libndi_advanced.so.6"),
                    Origin::LoaderSearch,
                ));
            }
            if !cfg!(target_vendor = "apple") {
                expected.push((PathBuf::from("libndi.so.6"), Origin::LoaderSearch));
                expected.push((PathBuf::from("libndi_advanced.so.6"), Origin::LoaderSearch));
            }
            assert_eq!(locations(&plan), expected);
        }

        #[test]
        fn without_a_request_both_editions_are_sought_with_the_features_preference_first() {
            use crate::capabilities::SdkEdition::{Advanced, Standard};
            let expected = if cfg!(feature = "advanced")
                || (cfg!(feature = "file-io") && super::super::FILE_LIBRARY_NEEDS_ADVANCED_RUNTIME)
            {
                [Advanced, Standard]
            } else {
                [Standard, Advanced]
            };
            assert_eq!(super::super::DEFAULT_SEARCH, expected);
        }

        #[test]
        fn load_errors_keep_the_operating_system_reason() {
            let error = open_library(OsStr::new("/nonexistent/libndi-missing.so")).unwrap_err();
            let text = error_chain(&error);
            let summary = error.to_string();
            assert!(text.starts_with(&summary), "{text}");
            assert!(text.len() > summary.len(), "{text}");
            #[cfg(unix)]
            assert!(text.contains("/nonexistent/libndi-missing.so"), "{text}");
        }

        #[cfg(windows)]
        #[test]
        fn reported_windows_paths_use_ordinary_disk_and_unc_forms() {
            for (verbatim, ordinary) in [
                (r"\\?\C:\a\b.dll", r"C:\a\b.dll"),
                (r"\\?\UNC\s\x\b.dll", r"\\s\x\b.dll"),
            ] {
                assert_eq!(ordinary_module_path(verbatim.into()), Path::new(ordinary));
            }
        }

        #[cfg(windows)]
        #[test]
        fn reported_windows_paths_keep_syntax_needed_to_preserve_the_file_name() {
            for path in [
                r"\\?\C:\a.\b.dll",
                r"\\?\C:\a \b.dll",
                r"\\?\C:\nul\b.dll",
                r"\\?\C:\NuL.dll",
                r"\\?\C:\NUL .dll",
                r"\\?\C:\COM1\b.dll",
                r"\\?\C:\LPT³\b.dll",
                r"\\?\UNC\s\share.\b.dll",
                r"\\?\Volume{1234}\b.dll",
            ] {
                assert_eq!(ordinary_module_path(path.into()), Path::new(path));
            }
            // A component longer than NTFS permits also proves that the
            // unprefixed long path cannot be opened, regardless of opt-ins.
            let long = PathBuf::from(format!(r"\\?\C:\{}\b.dll", "a".repeat(300)));
            assert_eq!(ordinary_module_path(long.clone()), long);
        }
    }
}

#[cfg(ndi_link_static)]
mod linked {
    use std::{
        ffi::{c_void, CStr},
        path::PathBuf,
        ptr::NonNull,
    };

    use super::{ExplicitLibrary, Resolver, Runtime};
    use crate::{
        capabilities::{SdkEdition, SdkLinkage},
        runtime::RuntimeOptions,
        Error, LibraryKind, LoadFailure, LoadRefusal, Result, SelectionFailure,
    };

    // `LINKED_EDITION` and `fn linked_address(name: &[u8]) -> Option<*const c_void>`,
    // generated by the build script from the selected archive and its exports.
    include!(concat!(env!("OUT_DIR"), "/ndi_static_symbols.rs"));

    /// The SDK archive linked into this program.
    pub(in crate::sys) struct Linked;

    impl Linked {
        pub(super) const LINKAGE: SdkLinkage = SdkLinkage::Static;

        pub(super) fn library_path(&self) -> Option<PathBuf> {
            None
        }

        /// A linked archive is the only implementation there is, so no request
        /// can name a library to load in its place.
        pub(super) fn confirm_location(
            &self,
            requested: Option<ExplicitLibrary<'_>>,
        ) -> Result<()> {
            match requested {
                None => Ok(()),
                Some(requested) => Err(Error::RuntimeConflict(SelectionFailure::linked(
                    LibraryKind::Runtime,
                    requested.path(),
                ))),
            }
        }

        /// The build selected exactly one edition-specific archive.
        pub(super) fn edition(&self) -> std::result::Result<SdkEdition, LoadRefusal> {
            Ok(LINKED_EDITION)
        }
    }

    impl Resolver for Linked {
        fn resolve(
            &self,
            name: &'static CStr,
        ) -> std::result::Result<Option<NonNull<c_void>>, LoadRefusal> {
            Ok(
                linked_address(name.to_bytes())
                    .and_then(|address| NonNull::new(address.cast_mut())),
            )
        }
    }

    impl Runtime {
        /// The archive was chosen when the program was built, so the request
        /// plays no part here: it is checked against the archive exactly as
        /// against any implementation that is already selected.
        pub(in crate::sys) fn load(
            _: &RuntimeOptions,
            _: Option<ExplicitLibrary<'_>>,
        ) -> Result<Self> {
            Self::describe(Linked, LINKED_EDITION).map_err(|reason| {
                Error::RuntimeLoadFailed(LoadFailure::linked(LibraryKind::Runtime, reason))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(ndi_link_static))]
    #[test]
    fn native_library_file_names_match_the_vendor_files() {
        #[cfg(all(windows, target_pointer_width = "64"))]
        let expected = [
            "Processing.NDI.Lib.x64.dll",
            "Processing.NDI.Lib.Advanced.x64.dll",
            "Processing.NDI.Lib.File.x64.dll",
        ];
        #[cfg(all(windows, target_pointer_width = "32"))]
        let expected = [
            "Processing.NDI.Lib.x86.dll",
            "Processing.NDI.Lib.Advanced.x86.dll",
            "Processing.NDI.Lib.File.x86.dll",
        ];
        #[cfg(target_vendor = "apple")]
        let expected = ["libndi.dylib", "libndi_advanced.dylib", "libndifile.dylib"];
        #[cfg(target_os = "android")]
        let expected = ["libndi.so", "libndi_advanced.so", "libndifile.so"];
        #[cfg(not(any(windows, target_vendor = "apple", target_os = "android")))]
        let expected = ["libndi.so.6", "libndi_advanced.so.6", "libndifile.so.6"];
        assert_eq!(
            library_file(LibraryFile::Runtime(SdkEdition::Standard)),
            expected[0]
        );
        assert_eq!(
            library_file(LibraryFile::Runtime(SdkEdition::Advanced)),
            expected[1]
        );
        #[cfg(feature = "file-io")]
        assert_eq!(library_file(LibraryFile::File), expected[2]);
    }

    #[test]
    fn only_a_request_for_the_advanced_sdk_can_go_unmet() {
        use SdkEdition::{Advanced, Standard};
        assert!(meets(Standard, Standard));
        // An Advanced implementation exports all of the Standard SDK.
        assert!(meets(Standard, Advanced));
        assert!(meets(Advanced, Advanced));
        assert!(!meets(Advanced, Standard));
    }

    #[test]
    fn file_library_platform_dependency_matches_the_vendor_libraries() {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "android"))]
        const {
            assert!(FILE_LIBRARY_NEEDS_ADVANCED_RUNTIME)
        };
        #[cfg(windows)]
        const {
            assert!(!FILE_LIBRARY_NEEDS_ADVANCED_RUNTIME)
        };
    }

    #[test]
    fn explicit_paths_are_validated_before_any_provider_is_consulted() {
        let absolute = std::env::current_dir().unwrap().join("library");
        assert!(ExplicitLibrary::new(&absolute).is_ok());
        for path in [Path::new("library"), Path::new(""), Path::new("./library")] {
            assert!(matches!(
                ExplicitLibrary::new(path),
                Err(Error::InvalidInput(_))
            ));
        }
    }
}
