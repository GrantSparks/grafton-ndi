//! Locating, opening and pinning the native NDI implementation.
//!
//! Shared targets search for the runtime library in a fixed order and pin the
//! first usable module for the life of the process. Statically linked targets
//! resolve the same tables from the archive selected at build time.

use std::ffi::CStr;

use super::{api::Api, selection::Selection, table::Resolver};
use crate::{
    capabilities::{Capabilities, RuntimeInfo, SdkEdition, SdkVersion},
    runtime::RuntimeOptions,
    Error, Result,
};

static SELECTED: Selection<Runtime> = Selection::new();

/// Entry points only the Advanced SDK exports; together they identify its edition.
const ADVANCED_SENDER: &CStr = c"NDIlib_send_create_v2";
const ADVANCED_RECEIVER: &CStr = c"NDIlib_recv_create_v4";

/// The implementation selected for this process.
pub(crate) struct Runtime {
    pub(crate) api: Api,
    /// The Advanced tables, handed out by their `Table::of`.
    #[cfg(ndi_has_advanced_header)]
    pub(crate) advanced: super::advanced::AdvancedApi,
    pub(crate) info: RuntimeInfo,
    pub(crate) capabilities: Capabilities,
    provider: Provider,
}

/// Which Advanced SDK entry points an implementation exports.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AdvancedEntryPoints {
    pub(crate) sender_configuration: bool,
    pub(crate) receiver_configuration: bool,
}

impl AdvancedEntryPoints {
    fn detect(resolver: &dyn Resolver) -> Result<Self> {
        Ok(Self {
            sender_configuration: resolver.resolve(ADVANCED_SENDER)?.is_some(),
            receiver_configuration: resolver.resolve(ADVANCED_RECEIVER)?.is_some(),
        })
    }
}

/// Select the implementation `options` describe, or confirm that the one
/// already pinned satisfies them.
///
/// # Errors
///
/// [`Error::RuntimeLoadFailed`] when nothing usable can be loaded (retryable),
/// [`Error::RuntimeConflict`] when a different implementation is already pinned.
pub(crate) fn select(options: &RuntimeOptions) -> Result<&'static Runtime> {
    let runtime = SELECTED.get_or_try_load(|| Runtime::load(options))?;
    runtime.accepts(options)?;
    Ok(runtime)
}

/// The edition used when [`RuntimeOptions`] names none, fixed by the build.
pub(crate) fn default_edition() -> SdkEdition {
    if env!("GRAFTON_NDI_EDITION") == "advanced" {
        SdkEdition::Advanced
    } else {
        SdkEdition::Standard
    }
}

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
    fn describe(provider: Provider, requested: SdkEdition) -> std::result::Result<Self, String> {
        let api = Api::load(&provider).map_err(reason)?;
        #[cfg(ndi_has_advanced_header)]
        let advanced_tables = super::advanced::AdvancedApi::load(&provider).map_err(reason)?;
        let advanced = AdvancedEntryPoints::detect(&provider).map_err(reason)?;
        let edition = provider.edition(advanced);
        if !meets(requested, edition) {
            return Err("it does not export the Advanced SDK entry points".into());
        }
        // SAFETY: NDIlib_version takes no arguments and may be called before
        // initialization; it returns a static NUL-terminated string or null.
        let version = unsafe { api.core.NDIlib_version() };
        if version.is_null() {
            return Err("NDIlib_version returned no version text".into());
        }
        // SAFETY: non-null was checked and the SDK keeps the string alive for
        // as long as the implementation is loaded, which is the whole process.
        let raw_version = unsafe { CStr::from_ptr(version) }
            .to_str()
            .map_err(|error| format!("its version text is not UTF-8: {error}"))?
            .to_owned();
        let version = SdkVersion::parse(&raw_version);
        let info = RuntimeInfo {
            edition,
            linkage: Provider::LINKAGE,
            library_path: provider.path(),
            raw_version,
            version,
        };
        Ok(Self {
            capabilities: Capabilities::detect(&api, version, edition, advanced),
            api,
            #[cfg(ndi_has_advanced_header)]
            advanced: advanced_tables,
            info,
            provider,
        })
    }

    /// Confirm that this implementation, which is selected for the process
    /// and cannot be replaced, meets `options`.
    ///
    /// # Errors
    ///
    /// [`Error::RuntimeConflict`] when `options` ask for an edition it is not
    /// or name a library it was not loaded from;
    /// [`Error::InvalidConfiguration`] when they name a relative path.
    fn accepts(&self, options: &RuntimeOptions) -> Result<()> {
        if options
            .edition
            .is_some_and(|requested| !meets(requested, self.info.edition))
        {
            return Err(Error::RuntimeConflict(
                "the selected NDI implementation is not an Advanced SDK".into(),
            ));
        }
        self.provider
            .confirm_location(options.library_path.as_deref())
    }
}

/// The text of a loader error without its variant prefix.
pub(in crate::sys) fn reason(error: Error) -> String {
    match error {
        Error::RuntimeLoadFailed(reason) => reason,
        other => other.to_string(),
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

    use super::{AdvancedEntryPoints, Resolver, Runtime};
    use crate::{
        capabilities::{SdkEdition, SdkLinkage},
        runtime::RuntimeOptions,
        Error, Result,
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
        base: usize,
        path: PathBuf,
    }

    impl Module {
        /// Open `location` and identify the module through `anchor`, a symbol
        /// it must define.
        ///
        /// The error is the reason, including the operating system's own text.
        pub(crate) fn open(location: &OsStr, anchor: &CStr) -> std::result::Result<Self, String> {
            let library = ManuallyDrop::new(open_library(location).map_err(|e| error_chain(&e))?);
            // SAFETY: the symbol is read only as an address.
            let address = unsafe { library.get::<*const c_void>(anchor.to_bytes_with_nul()) }
                .map(|symbol| *symbol)
                .map_err(|error| {
                    format!(
                        "it does not export {}: {}",
                        anchor.to_string_lossy(),
                        error_chain(&error)
                    )
                })?;
            let (base, path) = containing_module(address)?;
            let path = path.canonicalize().unwrap_or(path);
            if Path::new(location).is_absolute() && !same_path(Path::new(location), &path) {
                return Err(format!("the loader resolved it to {}", path.display()));
            }
            Ok(Self {
                library,
                base,
                path,
            })
        }

        /// The file the module was loaded from.
        pub(crate) fn path(&self) -> Option<PathBuf> {
            Some(self.path.clone())
        }

        /// Where a lookup through this module's handle finds `name`, and the
        /// load address of the module that defines it there.
        ///
        /// A handle lookup searches the module and then the libraries it
        /// depends on, so the definition may lie outside this module.
        fn lookup(
            &self,
            name: &CStr,
        ) -> std::result::Result<Option<(NonNull<c_void>, usize)>, String> {
            // SAFETY: the symbol is read only as an address; a caller that
            // goes on to call it applies the function type.
            let symbol = unsafe { self.library.get::<*mut c_void>(name.to_bytes_with_nul()) };
            let Some(address) = symbol.ok().and_then(|symbol| NonNull::new(*symbol)) else {
                return Ok(None);
            };
            let (base, _) = containing_module(address.as_ptr())?;
            Ok(Some((address, base)))
        }

        /// The address this module is bound to for `name`: its own definition or,
        /// where the platform's handle lookup reaches them, the one a library it
        /// depends on supplies. `None` when the lookup finds neither.
        #[cfg(feature = "file-io")]
        pub(in crate::sys) fn binding(
            &self,
            name: &CStr,
        ) -> std::result::Result<Option<NonNull<c_void>>, String> {
            Ok(self.lookup(name)?.map(|(address, _)| address))
        }

        pub(super) const LINKAGE: SdkLinkage = SdkLinkage::Shared;

        /// Confirm that a request naming the library at `requested` is met by
        /// this module.
        pub(super) fn confirm_location(&self, requested: Option<&Path>) -> Result<()> {
            confirm_selected(&self.path, requested)
        }

        /// A module is an Advanced runtime when it exports both Advanced entry
        /// points; its file name and the caller's request are not evidence.
        pub(super) fn edition(&self, advanced: AdvancedEntryPoints) -> SdkEdition {
            if advanced.sender_configuration && advanced.receiver_configuration {
                SdkEdition::Advanced
            } else {
                SdkEdition::Standard
            }
        }
    }

    impl Resolver for Module {
        fn resolve(&self, name: &CStr) -> Result<Option<NonNull<c_void>>> {
            let Some((address, base)) = self.lookup(name).map_err(Error::RuntimeLoadFailed)? else {
                return Ok(None);
            };
            // Accept a symbol only when this module itself defines it.
            if base != self.base {
                return Err(Error::RuntimeLoadFailed(format!(
                    "{} resolves outside {}",
                    name.to_string_lossy(),
                    self.path.display()
                )));
            }
            Ok(Some(address))
        }
    }

    impl Runtime {
        pub(in crate::sys) fn load(options: &RuntimeOptions) -> Result<Self> {
            let requested = options.edition.unwrap_or_else(super::default_edition);
            Search {
                explicit: options.library_path.as_deref(),
                beside: None,
                name: library_name(requested),
                anchor: CORE_ANCHOR,
                description: &format!("{requested:?} runtime"),
            }
            .open(|module| Runtime::describe(module, requested))
        }
    }

    /// Confirm that a request naming `requested` is met by the library at
    /// `selected`, which is pinned for the process and cannot be replaced.
    pub(in crate::sys) fn confirm_selected(
        selected: &Path,
        requested: Option<&Path>,
    ) -> Result<()> {
        let Some(requested) = requested else {
            return Ok(());
        };
        require_absolute(requested)?;
        if same_path(requested, selected) {
            Ok(())
        } else {
            Err(Error::RuntimeConflict(format!(
                "{} is already selected; it cannot be switched to {}",
                selected.display(),
                requested.display()
            )))
        }
    }

    /// A native library to locate and open.
    pub(in crate::sys) struct Search<'a> {
        /// The only acceptable location, when the caller named one.
        pub(in crate::sys) explicit: Option<&'a Path>,
        /// The file of an already selected library this one is installed
        /// beside; its directory is searched first.
        pub(in crate::sys) beside: Option<&'a Path>,
        /// The library's file name on this target.
        pub(in crate::sys) name: &'a str,
        /// A symbol the library must define; it identifies the opened module.
        pub(in crate::sys) anchor: &'a CStr,
        /// What is being looked for, as the error names it.
        pub(in crate::sys) description: &'a str,
    }

    impl Search<'_> {
        /// Open the first location that loads and that `accept` can use.
        ///
        /// # Errors
        ///
        /// [`Error::InvalidConfiguration`] for a relative explicit path;
        /// otherwise [`Error::RuntimeLoadFailed`] naming every location tried
        /// and the reason each was rejected.
        pub(in crate::sys) fn open<T>(
            &self,
            accept: impl Fn(Module) -> std::result::Result<T, String>,
        ) -> Result<T> {
            if let Some(explicit) = self.explicit {
                require_absolute(explicit)?;
            }
            let executable = std::env::current_exe().ok();
            let runtime_directory = std::env::var_os(RUNTIME_DIRECTORY)
                .filter(|directory| !directory.is_empty())
                .map(PathBuf::from);
            let plan = search_plan(
                self.explicit,
                self.beside.and_then(Path::parent),
                executable.as_deref().and_then(Path::parent),
                runtime_directory.as_deref(),
                self.name,
            );
            let mut failures = Vec::with_capacity(plan.len());
            for candidate in plan {
                let module = match candidate.open(self.anchor) {
                    Ok(module) => module,
                    Err(reason) => {
                        failures.push(format!("{candidate}: {reason}"));
                        continue;
                    }
                };
                // `accept` consumes the module, so its identity is taken first.
                let (base, path) = (module.base, module.path.clone());
                match accept(module) {
                    Ok(opened) => return Ok(opened),
                    Err(reason) => failures.push(candidate.rejection(base, &path, &reason)),
                }
            }
            Err(Error::RuntimeLoadFailed(format!(
                "no usable {}; tried {}",
                self.description,
                failures.join("; ")
            )))
        }
    }

    /// Load addresses of the modules that were opened and then rejected.
    ///
    /// A module is never closed, so a rejected one stays loaded. The platform
    /// loader then answers a later request for a library by name with that
    /// module whenever the name matches the one the module carries, instead of
    /// searching for another file.
    static REJECTED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    /// Reject an explicit library path the platform loader would search for
    /// instead of opening as the named file.
    fn require_absolute(path: &Path) -> Result<()> {
        if path.is_absolute() {
            Ok(())
        } else {
            Err(Error::InvalidConfiguration(
                "an explicit NDI library path must be absolute".into(),
            ))
        }
    }

    /// Where a candidate location came from.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Origin {
        /// A path the caller named.
        Explicit,
        /// The directory of the library this one is installed beside.
        Companion,
        /// The directory containing the running executable.
        Executable,
        /// The directory named by `NDI_RUNTIME_DIR_V6`.
        RuntimeDirectory,
        /// The platform loader's own search for a library name.
        LoaderSearch,
    }

    /// One place the runtime library may be loaded from.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct Candidate {
        pub(super) location: PathBuf,
        pub(super) origin: Origin,
    }

    impl fmt::Display for Candidate {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let origin = match self.origin {
                Origin::Explicit => "explicit library path",
                Origin::Companion => "next to the NDI runtime",
                Origin::Executable => "next to the executable",
                Origin::RuntimeDirectory => RUNTIME_DIRECTORY,
                Origin::LoaderSearch => "system library search",
            };
            write!(f, "{} ({origin})", self.location.display())
        }
    }

    impl Candidate {
        fn open(&self, anchor: &CStr) -> std::result::Result<Module, String> {
            // A directory candidate names one exact file. Skipping an absent
            // one keeps the loader from searching elsewhere for that path.
            if matches!(
                self.origin,
                Origin::Companion | Origin::Executable | Origin::RuntimeDirectory
            ) && !self.location.is_file()
            {
                return Err("not found".into());
            }
            Module::open(self.location.as_os_str(), anchor)
        }

        /// Why the module at `base`, which this candidate opened from `path`,
        /// cannot be used, given the `reason` it was just rejected for.
        ///
        /// A candidate that names a file reports that file's own defect. One
        /// the platform loader resolved by name reports which file that was,
        /// and whether it is a module rejected before: the loader keeps
        /// returning such a module for its name, so the search cannot reach
        /// any other library of that name.
        fn rejection(&self, base: usize, path: &Path, reason: &str) -> String {
            let mut rejected = REJECTED.lock().unwrap_or_else(PoisonError::into_inner);
            let rejected_before = rejected.contains(&base);
            if !rejected_before {
                rejected.push(base);
            }
            if self.origin != Origin::LoaderSearch {
                format!("{self}: {reason}")
            } else if rejected_before {
                format!(
                    "{self}: the platform loader returned {}, which is already loaded and was \
                     rejected ({reason}); no other library of that name can be found by name in \
                     this process, so remove or fix that file or pass an explicit library path",
                    path.display()
                )
            } else {
                format!("{self} resolved to {}: {reason}", path.display())
            }
        }
    }

    /// The ordered places to look for `name`.
    ///
    /// An explicit path is exclusive. Otherwise a library installed beside the
    /// one it accompanies wins over one shipped beside the executable, which
    /// wins over an installed runtime, which wins over whatever the platform
    /// loader finds on its own.
    pub(super) fn search_plan(
        explicit: Option<&Path>,
        companion_directory: Option<&Path>,
        executable_directory: Option<&Path>,
        runtime_directory: Option<&Path>,
        name: &str,
    ) -> Vec<Candidate> {
        if let Some(path) = explicit {
            return vec![Candidate {
                location: path.to_owned(),
                origin: Origin::Explicit,
            }];
        }
        let mut plan: Vec<Candidate> = Vec::new();
        let directories = [
            (companion_directory, Origin::Companion),
            (executable_directory, Origin::Executable),
            (runtime_directory, Origin::RuntimeDirectory),
        ];
        for (directory, origin) in directories {
            let Some(directory) = directory else { continue };
            let location = directory.join(name);
            if !plan.iter().any(|earlier| earlier.location == location) {
                plan.push(Candidate { location, origin });
            }
        }
        // Application bundles locate their libraries through the run path.
        if cfg!(target_vendor = "apple") {
            plan.push(Candidate {
                location: Path::new("@rpath").join(name),
                origin: Origin::LoaderSearch,
            });
        }
        plan.push(Candidate {
            location: PathBuf::from(name),
            origin: Origin::LoaderSearch,
        });
        plan
    }

    /// The runtime library's file name for this target.
    pub(super) const fn library_name(edition: SdkEdition) -> &'static str {
        match edition {
            SdkEdition::Standard => {
                if cfg!(all(windows, target_pointer_width = "64")) {
                    "Processing.NDI.Lib.x64.dll"
                } else if cfg!(windows) {
                    "Processing.NDI.Lib.x86.dll"
                } else if cfg!(target_vendor = "apple") {
                    "libndi.dylib"
                } else if cfg!(target_os = "android") {
                    "libndi.so"
                } else {
                    "libndi.so.6"
                }
            }
            SdkEdition::Advanced => {
                if cfg!(all(windows, target_pointer_width = "64")) {
                    "Processing.NDI.Lib.Advanced.x64.dll"
                } else if cfg!(windows) {
                    "Processing.NDI.Lib.Advanced.x86.dll"
                } else if cfg!(target_vendor = "apple") {
                    "libndi_advanced.dylib"
                } else if cfg!(target_os = "android") {
                    "libndi_advanced.so"
                } else {
                    "libndi_advanced.so.6"
                }
            }
        }
    }

    fn same_path(a: &Path, b: &Path) -> bool {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
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

    #[cfg(unix)]
    fn open_library(
        location: &OsStr,
    ) -> std::result::Result<libloading::Library, libloading::Error> {
        use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
        // SAFETY: loading runs the library's constructors. Callers select a
        // trusted NDI runtime; eager binding reports missing dependencies here
        // rather than at the first call.
        unsafe { Library::open(Some(location), RTLD_NOW | RTLD_LOCAL) }.map(Into::into)
    }

    #[cfg(windows)]
    fn open_library(
        location: &OsStr,
    ) -> std::result::Result<libloading::Library, libloading::Error> {
        use libloading::os::windows::{Library, LOAD_WITH_ALTERED_SEARCH_PATH};
        // A full path resolves the library's own dependencies beside it; a bare
        // name uses the standard search order.
        let flags = if Path::new(location).is_absolute() {
            LOAD_WITH_ALTERED_SEARCH_PATH
        } else {
            0
        };
        // SAFETY: loading runs the library's initialization routine. Callers
        // select a trusted NDI runtime.
        unsafe { Library::load_with_flags(location, flags) }.map(Into::into)
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

        fn locations(plan: &[Candidate]) -> Vec<(PathBuf, Origin)> {
            plan.iter()
                .map(|candidate| (candidate.location.clone(), candidate.origin))
                .collect()
        }

        #[test]
        fn an_explicit_path_is_the_only_candidate() {
            let plan = search_plan(
                Some(Path::new("/opt/ndi/libndi.so.6")),
                Some(Path::new("/core")),
                Some(Path::new("/app")),
                Some(Path::new("/runtime")),
                "libndi.so.6",
            );
            assert_eq!(
                locations(&plan),
                [(PathBuf::from("/opt/ndi/libndi.so.6"), Origin::Explicit)]
            );
        }

        #[test]
        fn a_bundled_library_precedes_the_installed_runtime_and_the_loader_search() {
            let plan = search_plan(
                None,
                None,
                Some(Path::new("/app")),
                Some(Path::new("/runtime")),
                "libndi.so.6",
            );
            let mut expected = vec![
                (PathBuf::from("/app/libndi.so.6"), Origin::Executable),
                (
                    PathBuf::from("/runtime/libndi.so.6"),
                    Origin::RuntimeDirectory,
                ),
            ];
            if cfg!(target_vendor = "apple") {
                expected.push((PathBuf::from("@rpath/libndi.so.6"), Origin::LoaderSearch));
            }
            expected.push((PathBuf::from("libndi.so.6"), Origin::LoaderSearch));
            assert_eq!(locations(&plan), expected);
        }

        #[test]
        fn the_loader_search_remains_when_no_directory_is_known() {
            let plan = search_plan(None, None, None, None, "libndi.so.6");
            assert_eq!(
                plan.last().map(|candidate| candidate.origin),
                Some(Origin::LoaderSearch)
            );
            assert!(plan
                .iter()
                .all(|candidate| candidate.origin == Origin::LoaderSearch));
        }

        #[test]
        fn one_directory_named_twice_is_tried_once() {
            let plan = search_plan(
                None,
                None,
                Some(Path::new("/app")),
                Some(Path::new("/app")),
                "libndi.so.6",
            );
            let bundled = plan
                .iter()
                .filter(|candidate| candidate.location == Path::new("/app/libndi.so.6"))
                .count();
            assert_eq!(bundled, 1);
        }

        #[test]
        fn a_companion_library_is_sought_beside_its_runtime_first() {
            let plan = search_plan(
                None,
                Some(Path::new("/core")),
                Some(Path::new("/app")),
                Some(Path::new("/runtime")),
                "libndifile.so.6",
            );
            assert_eq!(
                locations(&plan)[..3],
                [
                    (PathBuf::from("/core/libndifile.so.6"), Origin::Companion),
                    (PathBuf::from("/app/libndifile.so.6"), Origin::Executable),
                    (
                        PathBuf::from("/runtime/libndifile.so.6"),
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
        fn load_errors_keep_the_operating_system_reason() {
            let error = open_library(OsStr::new("/nonexistent/libndi-missing.so")).unwrap_err();
            let text = error_chain(&error);
            let summary = error.to_string();
            assert!(text.starts_with(&summary), "{text}");
            assert!(text.len() > summary.len(), "{text}");
            #[cfg(unix)]
            assert!(text.contains("/nonexistent/libndi-missing.so"), "{text}");
        }
    }
}

#[cfg(ndi_link_static)]
mod linked {
    use std::{
        ffi::{c_void, CStr},
        path::{Path, PathBuf},
        ptr::NonNull,
    };

    use super::{AdvancedEntryPoints, Resolver, Runtime};
    use crate::{
        capabilities::{SdkEdition, SdkLinkage},
        runtime::RuntimeOptions,
        Error, Result,
    };

    // `fn linked_address(name: &[u8]) -> Option<*const c_void>`, generated by the
    // build script from the functions the linked archive defines.
    include!(concat!(env!("OUT_DIR"), "/ndi_static_symbols.rs"));

    /// The SDK archive linked into this program.
    pub(in crate::sys) struct Linked;

    impl Linked {
        pub(super) const LINKAGE: SdkLinkage = SdkLinkage::Static;

        pub(super) fn path(&self) -> Option<PathBuf> {
            None
        }

        /// A linked archive is the only implementation there is, so no request
        /// can name a library to load in its place.
        pub(super) fn confirm_location(&self, requested: Option<&Path>) -> Result<()> {
            match requested {
                None => Ok(()),
                Some(requested) => Err(Error::RuntimeConflict(format!(
                    "the NDI SDK is linked into this program, so {} cannot be loaded instead",
                    requested.display()
                ))),
            }
        }

        /// The build selected exactly one edition-specific archive.
        pub(super) fn edition(&self, _advanced: AdvancedEntryPoints) -> SdkEdition {
            super::default_edition()
        }
    }

    impl Resolver for Linked {
        fn resolve(&self, name: &CStr) -> Result<Option<NonNull<c_void>>> {
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
        pub(in crate::sys) fn load(_: &RuntimeOptions) -> Result<Self> {
            Self::describe(Linked, super::default_edition())
                .map_err(|reason| Error::RuntimeLoadFailed(format!("the linked SDK: {reason}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_request_for_the_advanced_sdk_can_go_unmet() {
        use SdkEdition::{Advanced, Standard};
        assert!(meets(Standard, Standard));
        // An Advanced implementation exports all of the Standard SDK.
        assert!(meets(Standard, Advanced));
        assert!(meets(Advanced, Advanced));
        assert!(!meets(Advanced, Standard));
    }
}
