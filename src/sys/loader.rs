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
    /// The Advanced tables, reached through `sys::advanced::AdvancedTable::of`.
    #[cfg(ndi_has_advanced_header)]
    pub(crate) advanced: super::advanced::AdvancedApi,
    pub(crate) info: RuntimeInfo,
    pub(crate) capabilities: Capabilities,
    // The linked provider carries no state; only the Advanced bridge reads it.
    #[cfg_attr(all(ndi_link_static, not(feature = "advanced")), allow(dead_code))]
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
        if requested == SdkEdition::Advanced && edition != SdkEdition::Advanced {
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

    /// The provider's raw symbol lookup, for Advanced entry points whose
    /// declarations are not vendored.
    #[cfg(feature = "advanced")]
    pub(crate) fn resolver(&self) -> &dyn Resolver {
        &self.provider
    }
}

/// The text of a loader error without its variant prefix.
fn reason(error: Error) -> String {
    match error {
        Error::RuntimeLoadFailed(reason) => reason,
        other => other.to_string(),
    }
}

#[cfg(not(ndi_link_static))]
use shared::Module as Provider;

#[cfg(ndi_link_static)]
use linked::Linked as Provider;

#[cfg(not(ndi_link_static))]
mod shared {
    use std::{
        ffi::{c_void, CStr, OsStr},
        fmt,
        mem::ManuallyDrop,
        path::{Path, PathBuf},
        ptr::NonNull,
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

        pub(super) const LINKAGE: SdkLinkage = SdkLinkage::Shared;

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
            // SAFETY: the symbol is read only as an address; the table that asked
            // for it applies the function type.
            let symbol = unsafe { self.library.get::<*mut c_void>(name.to_bytes_with_nul()) };
            let Some(address) = symbol.ok().and_then(|symbol| NonNull::new(*symbol)) else {
                return Ok(None);
            };
            // A handle lookup also searches the module's dependencies. Accept a
            // symbol only when this module defines it.
            let (base, _) =
                containing_module(address.as_ptr()).map_err(Error::RuntimeLoadFailed)?;
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
            if options
                .library_path
                .as_ref()
                .is_some_and(|path| !path.is_absolute())
            {
                return Err(Error::InvalidConfiguration(
                    "an explicit NDI library path must be absolute".into(),
                ));
            }
            let requested = options.edition.unwrap_or_else(super::default_edition);
            let executable = std::env::current_exe().ok();
            let runtime_directory = std::env::var_os(RUNTIME_DIRECTORY)
                .filter(|directory| !directory.is_empty())
                .map(PathBuf::from);
            let plan = search_plan(
                options.library_path.as_deref(),
                executable.as_deref().and_then(Path::parent),
                runtime_directory.as_deref(),
                library_name(requested),
            );
            let mut failures = Vec::with_capacity(plan.len());
            for candidate in plan {
                match candidate.load(requested) {
                    Ok(runtime) => return Ok(runtime),
                    Err(reason) => failures.push(format!("{candidate}: {reason}")),
                }
            }
            Err(Error::RuntimeLoadFailed(format!(
                "no usable {requested:?} runtime; tried {}",
                failures.join("; ")
            )))
        }

        pub(in crate::sys) fn accepts(&self, options: &RuntimeOptions) -> Result<()> {
            if options.edition == Some(SdkEdition::Advanced)
                && self.info.edition != SdkEdition::Advanced
            {
                return Err(Error::RuntimeConflict(
                    "the selected NDI runtime is not an Advanced SDK runtime".into(),
                ));
            }
            let Some(requested) = &options.library_path else {
                return Ok(());
            };
            if !requested.is_absolute() {
                return Err(Error::InvalidConfiguration(
                    "an explicit NDI library path must be absolute".into(),
                ));
            }
            if same_path(requested, &self.provider.path) {
                Ok(())
            } else {
                Err(Error::RuntimeConflict(format!(
                    "{} is already selected; the runtime cannot be switched to {}",
                    self.provider.path.display(),
                    requested.display()
                )))
            }
        }
    }

    /// Where a candidate location came from.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Origin {
        /// `RuntimeOptions::library_path`.
        Explicit,
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
                Origin::Executable => "next to the executable",
                Origin::RuntimeDirectory => RUNTIME_DIRECTORY,
                Origin::LoaderSearch => "system library search",
            };
            write!(f, "{} ({origin})", self.location.display())
        }
    }

    impl Candidate {
        fn load(&self, requested: SdkEdition) -> std::result::Result<Runtime, String> {
            // A directory candidate names one exact file. Skipping an absent
            // one keeps the loader from searching elsewhere for that path.
            if matches!(self.origin, Origin::Executable | Origin::RuntimeDirectory)
                && !self.location.is_file()
            {
                return Err("not found".into());
            }
            let module = Module::open(self.location.as_os_str(), CORE_ANCHOR)?;
            Runtime::describe(module, requested)
        }
    }

    /// The ordered places to look for `name`.
    ///
    /// An explicit path is exclusive. Otherwise a library shipped beside the
    /// executable wins over an installed runtime, which wins over whatever the
    /// platform loader finds on its own.
    pub(super) fn search_plan(
        explicit: Option<&Path>,
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
            let plan = search_plan(None, None, None, "libndi.so.6");
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
        path::PathBuf,
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
    pub(super) struct Linked;

    impl Linked {
        pub(super) const LINKAGE: SdkLinkage = SdkLinkage::Static;

        pub(super) fn path(&self) -> Option<PathBuf> {
            None
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
        pub(in crate::sys) fn load(options: &RuntimeOptions) -> Result<Self> {
            conflict_with_linked(options)?;
            Self::describe(Linked, super::default_edition())
                .map_err(|reason| Error::RuntimeLoadFailed(format!("the linked SDK: {reason}")))
        }

        pub(in crate::sys) fn accepts(&self, options: &RuntimeOptions) -> Result<()> {
            conflict_with_linked(options)
        }
    }

    fn conflict_with_linked(options: &RuntimeOptions) -> Result<()> {
        let other_edition = options
            .edition
            .is_some_and(|edition| edition != super::default_edition());
        if options.library_path.is_some() || other_edition {
            return Err(Error::RuntimeConflict(
                "a statically linked NDI implementation cannot select a different module or edition"
                    .into(),
            ));
        }
        Ok(())
    }
}
