//! Structured causes for native library selection failures.

use thiserror::Error;

use std::{
    ffi::CStr,
    fmt,
    path::{Path, PathBuf},
};

/// Why an irreversible runtime or File selection cannot meet a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum SelectionConflict {
    /// The request names a different library from the one selected.
    #[error("a different library is already selected")]
    DifferentLibrary,
    /// The selected runtime does not meet the requested SDK edition.
    #[error("the selected runtime does not meet the requested SDK edition")]
    EditionNotMet,
    /// A library path cannot replace the implementation linked into the program.
    #[error("a linked implementation cannot be replaced")]
    LinkedImplementation,
    /// Native loading re-entered selection on its own loading thread.
    #[error("native loading re-entered selection")]
    ReentrantSelection,
    /// The selected runtime is Standard, but the File library needs Advanced.
    #[error("the File library needs the Advanced SDK runtime")]
    FileNeedsAdvancedRuntime,
    /// The process's global symbol scope contains another NDI runtime.
    ///
    /// On dynamically loaded, non-Apple ELF targets, an explicit runtime path
    /// must identify that same module. The File library also refuses a global
    /// runtime other than the selected core. Both checks run before opening
    /// the candidate. Select the runtime named in the diagnostic, or stop
    /// linking or preloading the other runtime.
    #[error("another NDI runtime is in the global symbol scope")]
    ForeignRuntimeInScope,
}

/// Which native library a failed selection sought.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum LibraryKind {
    /// The NDI runtime.
    #[error("NDI runtime")]
    Runtime,
    /// The separately loaded NDI File library.
    #[error("NDI File library")]
    File,
}

/// A selection conflict, with the library identities needed to act on it.
///
/// The payload is boxed so adding diagnostic context does not enlarge
/// [`crate::Error`] on capture and send paths. Use the accessors for decisions
/// and [`Display`](fmt::Display) for diagnostic text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
#[error("{0}")]
pub struct SelectionFailure(Box<SelectionData>);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SelectionData {
    #[cfg_attr(ndi_link_static, allow(dead_code))]
    DifferentLibrary {
        library: LibraryKind,
        selected: PathBuf,
        requested: PathBuf,
    },
    EditionNotMet {
        selected: Option<PathBuf>,
        requested: Option<PathBuf>,
    },
    #[cfg_attr(not(any(ndi_link_static, test)), allow(dead_code))]
    LinkedImplementation {
        library: LibraryKind,
        requested: PathBuf,
    },
    ReentrantSelection {
        library: LibraryKind,
    },
    #[cfg_attr(any(not(feature = "file-io"), ndi_link_static), allow(dead_code))]
    FileNeedsAdvancedRuntime {
        selected: Option<PathBuf>,
        requested: Option<PathBuf>,
    },
    #[cfg_attr(ndi_link_static, allow(dead_code))]
    ForeignRuntimeInScope {
        library: LibraryKind,
        selected: Option<PathBuf>,
        requested: Option<PathBuf>,
        foreign_module: PathBuf,
        candidate: PathBuf,
        origin: LoadOrigin,
    },
}

impl SelectionFailure {
    /// The typed cause of this conflict.
    pub fn kind(&self) -> SelectionConflict {
        match &*self.0 {
            SelectionData::DifferentLibrary { .. } => SelectionConflict::DifferentLibrary,
            SelectionData::EditionNotMet { .. } => SelectionConflict::EditionNotMet,
            SelectionData::LinkedImplementation { .. } => SelectionConflict::LinkedImplementation,
            SelectionData::ReentrantSelection { .. } => SelectionConflict::ReentrantSelection,
            SelectionData::FileNeedsAdvancedRuntime { .. } => {
                SelectionConflict::FileNeedsAdvancedRuntime
            }
            SelectionData::ForeignRuntimeInScope { .. } => SelectionConflict::ForeignRuntimeInScope,
        }
    }

    /// The native library the failed selection sought.
    pub fn library(&self) -> LibraryKind {
        match &*self.0 {
            SelectionData::DifferentLibrary { library, .. }
            | SelectionData::LinkedImplementation { library, .. }
            | SelectionData::ReentrantSelection { library }
            | SelectionData::ForeignRuntimeInScope { library, .. } => *library,
            SelectionData::EditionNotMet { .. } => LibraryKind::Runtime,
            SelectionData::FileNeedsAdvancedRuntime { .. } => LibraryKind::File,
        }
    }

    /// The already selected library, when its path is known.
    ///
    /// For a File conflict caused by the selected core runtime, this is that
    /// core runtime's path, rather than a File library not yet selected.
    pub fn selected(&self) -> Option<&Path> {
        match &*self.0 {
            SelectionData::DifferentLibrary { selected, .. } => Some(selected),
            SelectionData::EditionNotMet { selected, .. }
            | SelectionData::FileNeedsAdvancedRuntime { selected, .. }
            | SelectionData::ForeignRuntimeInScope { selected, .. } => selected.as_deref(),
            SelectionData::LinkedImplementation { .. }
            | SelectionData::ReentrantSelection { .. } => None,
        }
    }

    /// The explicit library path that conflicted, when the caller supplied one.
    pub fn requested(&self) -> Option<&Path> {
        match &*self.0 {
            SelectionData::DifferentLibrary { requested, .. }
            | SelectionData::LinkedImplementation { requested, .. } => Some(requested),
            SelectionData::FileNeedsAdvancedRuntime { requested, .. }
            | SelectionData::ForeignRuntimeInScope { requested, .. }
            | SelectionData::EditionNotMet { requested, .. } => requested.as_deref(),
            SelectionData::ReentrantSelection { .. } => None,
        }
    }

    /// The foreign runtime already in the process's global symbol scope.
    pub fn foreign_module(&self) -> Option<&Path> {
        match &*self.0 {
            SelectionData::ForeignRuntimeInScope { foreign_module, .. } => Some(foreign_module),
            _ => None,
        }
    }

    #[cfg(any(not(ndi_link_static), test))]
    pub(crate) fn different_library(
        library: LibraryKind,
        selected: &Path,
        requested: &Path,
    ) -> Self {
        Self(Box::new(SelectionData::DifferentLibrary {
            library,
            selected: selected.into(),
            requested: requested.into(),
        }))
    }

    pub(crate) fn edition_not_met(selected: Option<&Path>, requested: Option<&Path>) -> Self {
        Self(Box::new(SelectionData::EditionNotMet {
            selected: selected.map(Path::to_owned),
            requested: requested.map(Path::to_owned),
        }))
    }

    #[cfg(any(ndi_link_static, test))]
    pub(crate) fn linked(library: LibraryKind, requested: &Path) -> Self {
        Self(Box::new(SelectionData::LinkedImplementation {
            library,
            requested: requested.into(),
        }))
    }

    pub(crate) fn reentrant(library: LibraryKind) -> Self {
        Self(Box::new(SelectionData::ReentrantSelection { library }))
    }

    #[cfg(all(feature = "file-io", any(not(ndi_link_static), test)))]
    pub(crate) fn file_needs_advanced(selected: Option<&Path>, requested: Option<&Path>) -> Self {
        Self(Box::new(SelectionData::FileNeedsAdvancedRuntime {
            selected: selected.map(Path::to_owned),
            requested: requested.map(Path::to_owned),
        }))
    }

    #[cfg(any(not(ndi_link_static), test))]
    pub(crate) fn foreign(
        library: LibraryKind,
        selected: Option<&Path>,
        requested: Option<&Path>,
        foreign_module: &Path,
        candidate: &Path,
        origin: LoadOrigin,
    ) -> Self {
        Self(Box::new(SelectionData::ForeignRuntimeInScope {
            library,
            selected: selected.map(Path::to_owned),
            requested: requested.map(Path::to_owned),
            foreign_module: foreign_module.into(),
            candidate: candidate.into(),
            origin,
        }))
    }
}

impl fmt::Display for SelectionData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectionData::DifferentLibrary { selected, requested, .. } => write!(f, "{} is already selected; it cannot be switched to {}", selected.display(), requested.display()),
            SelectionData::EditionNotMet { .. } => f.write_str("the selected NDI implementation is not an Advanced SDK"),
            SelectionData::LinkedImplementation { library: LibraryKind::Runtime, requested } => write!(f, "the NDI SDK is linked into this program, so {} cannot be loaded instead", requested.display()),
            SelectionData::LinkedImplementation { library: LibraryKind::File, .. } => f.write_str("a statically linked NDI File implementation cannot be replaced by a library"),
            SelectionData::ReentrantSelection { .. } => f.write_str("native module loading re-entered NDI runtime selection"),
            SelectionData::FileNeedsAdvancedRuntime { .. } => f.write_str("the NDI File library needs the Advanced SDK runtime, and the selected runtime is the Standard one"),
            SelectionData::ForeignRuntimeInScope { candidate, origin, foreign_module, .. } => write!(f,
                "{} ({origin}): the process's global symbol scope already holds another NDI runtime ({}); select that runtime or stop linking or preloading it", candidate.display(), foreign_module.display()),
        }
    }
}

/// Why a native library location was considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LoadOrigin {
    /// An exact path the caller supplied.
    Explicit,
    /// Beside the selected NDI runtime.
    Companion,
    /// Beside the running executable.
    Executable,
    /// The directory supplied through `NDI_RUNTIME_DIR_V6`.
    RuntimeDirectory,
    /// An absolute `PATH` directory on Windows (or a refused relative entry).
    PathDirectory,
    /// An absolute `DYLD_LIBRARY_PATH` directory on Apple targets (or a refused relative entry).
    DyldLibraryDirectory,
    /// Unix's native search, Apple `@rpath`, or Windows' System32-only search.
    LoaderSearch,
    /// The SDK archive linked into the program at build time.
    LinkedArchive,
}

impl fmt::Display for LoadOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Explicit => "explicit library path",
            Self::Companion => "next to the NDI runtime",
            Self::Executable => "next to the executable",
            Self::RuntimeDirectory => "NDI_RUNTIME_DIR_V6",
            Self::PathDirectory => "PATH",
            Self::DyldLibraryDirectory => "DYLD_LIBRARY_PATH",
            Self::LoaderSearch => "system library search",
            Self::LinkedArchive => "linked SDK archive",
        })
    }
}

/// Why a candidate could not provide a usable implementation.
///
/// This is a structured diagnostic, not a promise that the platform loader
/// can distinguish an absent bare name from every other loading failure.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
#[error("{}", LoadRefusalDiagnostic(self))]
#[non_exhaustive]
pub enum LoadRefusal {
    /// No file exists at an exact directory candidate.
    Absent,
    /// The platform loader could not open a location; its diagnostic is retained.
    PlatformLoader {
        /// The operating system's diagnostic, including its cause chain.
        detail: String,
    },
    /// A loaded module or resolved address could not be identified.
    NotIdentifiable {
        /// The platform's diagnostic.
        detail: String,
    },
    /// The platform opened another file instead of the requested absolute path.
    ResolvedToAnotherFile {
        /// The path the loader actually opened.
        resolved: PathBuf,
    },
    /// A required entry point, including an identification anchor, is absent.
    MissingEntryPoint {
        /// The statically declared native symbol's name.
        symbol: &'static CStr,
        /// Whether it is an API-required entry rather than the identification anchor.
        required: bool,
        /// The platform's symbol lookup diagnostic, when one is available.
        detail: Option<String>,
    },
    /// A lookup found the entry point in a dependency instead of the selected module.
    EntryPointOutsideModule {
        /// The statically declared native symbol's name.
        symbol: &'static CStr,
        /// The selected module that did not define it.
        module: PathBuf,
    },
    /// The library lacks the entry points identifying the requested Advanced edition.
    EditionNotMet,
    /// The runtime did not return bounded, valid UTF-8 version text.
    UnreadableVersion {
        /// The invalid version condition.
        detail: String,
    },
    /// A loader search returned a module already loaded and refused earlier.
    AlreadyRefused {
        /// The file first associated with that module.
        path: PathBuf,
        /// Why this attempt could not use it.
        refusal: Box<LoadRefusal>,
    },
    /// The File library has no complete family of entry points.
    NoCompleteFileFamily {
        /// The first missing, statically declared entry point in declaration order.
        first_missing: &'static CStr,
    },
    /// The File library's imports bind to a runtime other than the selected one.
    BoundToAnotherRuntime,
    /// Another NDI runtime in the global symbol scope would capture this
    /// candidate's native calls. The candidate was not opened.
    ///
    /// Applies to dynamically loaded, non-Apple ELF targets. Select the module
    /// named here, or stop linking or preloading it. An explicit runtime path
    /// instead reports [`SelectionConflict::ForeignRuntimeInScope`].
    ForeignRuntimeInScope {
        /// The module defining `NDIlib_version` in the global symbol scope.
        module: PathBuf,
    },
    /// `NDI_RUNTIME_DIR_V6` names a relative directory, which is never searched.
    RelativeRuntimeDirectory,
    /// A `PATH` or `DYLD_LIBRARY_PATH` entry is relative or empty and was not opened.
    RelativeSearchDirectory,
}

struct LoadRefusalDiagnostic<'a>(&'a LoadRefusal);

impl fmt::Display for LoadRefusalDiagnostic<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            LoadRefusal::Absent => f.write_str("not found"),
            LoadRefusal::PlatformLoader { detail } | LoadRefusal::NotIdentifiable { detail }
                | LoadRefusal::UnreadableVersion { detail } => f.write_str(detail),
            LoadRefusal::ResolvedToAnotherFile { resolved } => write!(f, "the loader resolved it to {}", resolved.display()),
            LoadRefusal::MissingEntryPoint { symbol, required, detail } => {
                let symbol = symbol.to_string_lossy();
                if *required {
                    write!(f, "it does not export the required entry point {symbol}")?;
                } else {
                    write!(f, "it does not export {symbol}")?;
                }
                if let Some(detail) = detail { write!(f, ": {detail}")?; }
                Ok(())
            }
            LoadRefusal::EntryPointOutsideModule { symbol, module } => write!(f, "{} resolves outside {}", symbol.to_string_lossy(), module.display()),
            LoadRefusal::EditionNotMet => f.write_str("it does not export the Advanced SDK entry points"),
            LoadRefusal::AlreadyRefused { path, refusal } => write!(f,
                "the platform loader returned {}, which is already loaded and was rejected ({refusal}); no other library of that name can be found by name in this process, so remove or fix that file or pass an explicit library path", path.display()),
            LoadRefusal::NoCompleteFileFamily { first_missing } => write!(f, "it provides no family of NDI File entry points (it does not export {})", first_missing.to_string_lossy()),
            LoadRefusal::BoundToAnotherRuntime => f.write_str("it is bound to an NDI runtime other than the selected one"),
            LoadRefusal::ForeignRuntimeInScope { module } => write!(f,
                "the process's global symbol scope already holds another NDI runtime ({}); select that runtime or stop linking or preloading it", module.display()),
            LoadRefusal::RelativeRuntimeDirectory => f.write_str("the runtime directory must be absolute"),
            LoadRefusal::RelativeSearchDirectory => f.write_str("the library search directory must be absolute and nonempty"),
        }
    }
}

/// One refused native library location, in the order it was tried.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LoadAttempt {
    location: PathBuf,
    origin: LoadOrigin,
    refusal: LoadRefusal,
    loaded: bool,
    resolved: Option<PathBuf>,
}

impl LoadAttempt {
    pub(crate) fn new(
        location: PathBuf,
        origin: LoadOrigin,
        refusal: LoadRefusal,
        loaded: bool,
        resolved: Option<PathBuf>,
    ) -> Self {
        Self {
            location,
            origin,
            refusal,
            loaded,
            resolved,
        }
    }

    /// The requested path or name; for a linked archive, its diagnostic label.
    pub fn location(&self) -> &Path {
        &self.location
    }

    /// Why this location was included in the search.
    pub fn origin(&self) -> LoadOrigin {
        self.origin
    }

    /// Why this location was refused.
    pub fn refusal(&self) -> &LoadRefusal {
        &self.refusal
    }

    /// Whether this attempt loaded native code before refusing it.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// The identified module path, when the platform could report it.
    pub fn resolved_path(&self) -> Option<&Path> {
        self.resolved.as_deref()
    }
}

impl fmt::Display for LoadAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.origin == LoadOrigin::LinkedArchive {
            return write!(f, "{}: {}", self.location.display(), self.refusal);
        }
        write!(f, "{} ({})", self.location.display(), self.origin)?;
        if self.origin == LoadOrigin::LoaderSearch
            && !matches!(self.refusal, LoadRefusal::AlreadyRefused { .. })
        {
            if let Some(path) = &self.resolved {
                write!(f, " resolved to {}", path.display())?;
            }
        }
        write!(f, ": {}", self.refusal)
    }
}

/// A failed search, with a typed cause for every attempted location.
///
/// The aggregate is boxed so enriching selection diagnostics does not enlarge
/// [`crate::Error`] on capture and send paths. Match [`LoadRefusal`] rather
/// than parsing the text produced by `Display`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
#[error("{0}")]
pub struct LoadFailure(Box<FailureData>);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FailureData {
    library: LibraryKind,
    description: Option<String>,
    attempts: Vec<LoadAttempt>,
}

impl LoadFailure {
    #[cfg(any(not(ndi_link_static), test))]
    pub(crate) fn searched(
        library: LibraryKind,
        description: &str,
        attempts: Vec<LoadAttempt>,
    ) -> Self {
        Self(Box::new(FailureData {
            library,
            description: Some(description.to_owned()),
            attempts,
        }))
    }

    #[cfg(any(ndi_link_static, test))]
    pub(crate) fn linked(library: LibraryKind, label: &str, refusal: LoadRefusal) -> Self {
        Self(Box::new(FailureData {
            library,
            description: None,
            attempts: vec![LoadAttempt::new(
                PathBuf::from(label),
                LoadOrigin::LinkedArchive,
                refusal,
                true,
                None,
            )],
        }))
    }

    /// Which native library the selection sought.
    pub fn library(&self) -> LibraryKind {
        self.0.library
    }

    /// Every refused location, in search order.
    pub fn attempts(&self) -> &[LoadAttempt] {
        &self.0.attempts
    }

    /// Whether any candidate loaded native code before it was refused.
    ///
    /// `false` means none could be opened. It does not establish that every
    /// bare name was absent: platform loader failures retain the OS diagnostic.
    pub fn loaded_any(&self) -> bool {
        self.0.attempts.iter().any(LoadAttempt::loaded)
    }
}

impl fmt::Display for FailureData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(description) = &self.description {
            write!(f, "no usable {description}; tried ")?;
        }
        for (index, attempt) in self.attempts.iter().enumerate() {
            if index != 0 {
                f.write_str("; ")?;
            }
            fmt::Display::fmt(attempt, f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    #[test]
    fn refusal_display_keeps_each_existing_diagnostic() {
        let cases = [
            (LoadRefusal::Absent, "not found"),
            (LoadRefusal::PlatformLoader { detail: "dlopen failed: absent".into() }, "dlopen failed: absent"),
            (LoadRefusal::NotIdentifiable { detail: "the module cannot be identified".into() }, "the module cannot be identified"),
            (LoadRefusal::ResolvedToAnotherFile { resolved: "another.so".into() }, "the loader resolved it to another.so"),
            (LoadRefusal::MissingEntryPoint { symbol: c"NDIlib_version", required: false, detail: Some("undefined symbol".into()) }, "it does not export NDIlib_version: undefined symbol"),
            (LoadRefusal::MissingEntryPoint { symbol: c"NDIlib_initialize", required: true, detail: None }, "it does not export the required entry point NDIlib_initialize"),
            (LoadRefusal::EntryPointOutsideModule { symbol: c"NDIlib_initialize", module: "selected.so".into() }, "NDIlib_initialize resolves outside selected.so"),
            (LoadRefusal::EditionNotMet, "it does not export the Advanced SDK entry points"),
            (LoadRefusal::UnreadableVersion { detail: "NDIlib_version returned no version text".into() }, "NDIlib_version returned no version text"),
            (LoadRefusal::AlreadyRefused { path: "rejected.so".into(), refusal: Box::new(LoadRefusal::EditionNotMet) }, "the platform loader returned rejected.so, which is already loaded and was rejected (it does not export the Advanced SDK entry points); no other library of that name can be found by name in this process, so remove or fix that file or pass an explicit library path"),
            (LoadRefusal::NoCompleteFileFamily { first_missing: c"NDIlib_file_player_create" }, "it provides no family of NDI File entry points (it does not export NDIlib_file_player_create)"),
            (LoadRefusal::BoundToAnotherRuntime, "it is bound to an NDI runtime other than the selected one"),
            (LoadRefusal::ForeignRuntimeInScope { module: "preloaded.so".into() }, "the process's global symbol scope already holds another NDI runtime (preloaded.so); select that runtime or stop linking or preloading it"),
            (LoadRefusal::RelativeRuntimeDirectory, "the runtime directory must be absolute"),
            (LoadRefusal::RelativeSearchDirectory, "the library search directory must be absolute and nonempty"),
        ];
        for (refusal, expected) in cases {
            assert_eq!(refusal.to_string(), expected, "{refusal:?}");
        }
    }

    #[test]
    fn structured_search_keeps_order_origins_and_loaded_status() {
        let absent = LoadAttempt::new(
            "bundle/ndi.so".into(),
            LoadOrigin::Executable,
            LoadRefusal::Absent,
            false,
            None,
        );
        let relative = LoadAttempt::new(
            "relative".into(),
            LoadOrigin::RuntimeDirectory,
            LoadRefusal::RelativeRuntimeDirectory,
            false,
            None,
        );
        let loader = LoadAttempt::new(
            "ndi.so".into(),
            LoadOrigin::LoaderSearch,
            LoadRefusal::EditionNotMet,
            true,
            Some("resolved.so".into()),
        );
        let nothing_loaded = LoadFailure::searched(
            LibraryKind::Runtime,
            "NDI runtime",
            vec![absent.clone(), relative.clone()],
        );
        assert!(!nothing_loaded.loaded_any());
        let failure = LoadFailure::searched(
            LibraryKind::Runtime,
            "NDI runtime",
            vec![absent, relative, loader],
        );
        assert_eq!(failure.library(), LibraryKind::Runtime);
        assert_eq!(failure.attempts().len(), 3);
        assert!(failure.loaded_any());
        assert_eq!(
            failure.attempts()[2].resolved_path(),
            Some(Path::new("resolved.so"))
        );
        assert_eq!(failure.to_string(), "no usable NDI runtime; tried bundle/ndi.so (next to the executable): not found; relative (NDI_RUNTIME_DIR_V6): the runtime directory must be absolute; ndi.so (system library search) resolved to resolved.so: it does not export the Advanced SDK entry points");
        assert_eq!(Error::RuntimeLoadFailed(failure).to_string(), "Failed to load an NDI library: no usable NDI runtime; tried bundle/ndi.so (next to the executable): not found; relative (NDI_RUNTIME_DIR_V6): the runtime directory must be absolute; ndi.so (system library search) resolved to resolved.so: it does not export the Advanced SDK entry points");
    }

    #[test]
    fn every_conflict_kind_preserves_its_typed_paths_and_diagnostic() {
        let selected = Path::new("selected/ndi.so");
        let requested = Path::new("requested/ndi.so");
        let foreign = Path::new("foreign/ndi.so");
        for library in [LibraryKind::Runtime, LibraryKind::File] {
            let failure = SelectionFailure::different_library(library, selected, requested);
            assert_eq!(failure.kind(), SelectionConflict::DifferentLibrary);
            assert_eq!(failure.library(), library);
            assert_eq!(failure.selected(), Some(selected));
            assert_eq!(failure.requested(), Some(requested));
            assert_eq!(failure.foreign_module(), None);
            assert_eq!(
                failure.to_string(),
                "selected/ndi.so is already selected; it cannot be switched to requested/ndi.so"
            );

            let failure = SelectionFailure::linked(library, requested);
            assert_eq!(failure.kind(), SelectionConflict::LinkedImplementation);
            assert_eq!(failure.library(), library);
            assert_eq!(failure.selected(), None);
            assert_eq!(failure.requested(), Some(requested));
            assert_eq!(failure.foreign_module(), None);
            assert_eq!(failure.to_string(), match library {
                LibraryKind::Runtime => "the NDI SDK is linked into this program, so requested/ndi.so cannot be loaded instead",
                LibraryKind::File => "a statically linked NDI File implementation cannot be replaced by a library",
            });

            let failure = SelectionFailure::reentrant(library);
            assert_eq!(failure.kind(), SelectionConflict::ReentrantSelection);
            assert_eq!(failure.library(), library);
            assert_eq!(failure.selected(), None);
            assert_eq!(failure.requested(), None);
            assert_eq!(failure.foreign_module(), None);
            assert_eq!(
                failure.to_string(),
                "native module loading re-entered NDI runtime selection"
            );

            for (explicit, origin) in [
                (Some(requested), LoadOrigin::Explicit),
                (None, LoadOrigin::Companion),
            ] {
                let failure = SelectionFailure::foreign(
                    library,
                    Some(selected),
                    explicit,
                    foreign,
                    requested,
                    origin,
                );
                assert_eq!(failure.kind(), SelectionConflict::ForeignRuntimeInScope);
                assert_eq!(failure.library(), library);
                assert_eq!(failure.selected(), Some(selected));
                assert_eq!(failure.requested(), explicit);
                assert_eq!(failure.foreign_module(), Some(foreign));
                assert_eq!(
                    failure.to_string(),
                    format!(
                        "requested/ndi.so ({origin}): {}",
                        LoadRefusal::ForeignRuntimeInScope {
                            module: foreign.into()
                        }
                    )
                );
                assert_eq!(
                    Error::RuntimeConflict(failure.clone()).to_string(),
                    format!("Conflicting NDI runtime selection: {failure}")
                );
            }
        }
        let failure = SelectionFailure::edition_not_met(Some(selected), Some(requested));
        assert_eq!(failure.kind(), SelectionConflict::EditionNotMet);
        assert_eq!(failure.library(), LibraryKind::Runtime);
        assert_eq!(failure.selected(), Some(selected));
        assert_eq!(failure.requested(), Some(requested));
        assert_eq!(failure.foreign_module(), None);
        assert_eq!(
            failure.to_string(),
            "the selected NDI implementation is not an Advanced SDK"
        );

        #[cfg(feature = "file-io")]
        {
            let failure = SelectionFailure::file_needs_advanced(Some(selected), Some(requested));
            assert_eq!(failure.kind(), SelectionConflict::FileNeedsAdvancedRuntime);
            assert_eq!(failure.library(), LibraryKind::File);
            assert_eq!(failure.selected(), Some(selected));
            assert_eq!(failure.requested(), Some(requested));
            assert_eq!(failure.foreign_module(), None);
            assert_eq!(failure.to_string(), "the NDI File library needs the Advanced SDK runtime, and the selected runtime is the Standard one");
        }
    }

    #[test]
    fn linked_failures_report_one_linked_attempt_without_changing_display() {
        for (library, label) in [
            (LibraryKind::Runtime, "the linked SDK"),
            (LibraryKind::File, "the linked NDI SDK archive"),
        ] {
            let failure = LoadFailure::linked(library, label, LoadRefusal::EditionNotMet);
            assert_eq!(failure.library(), library);
            assert!(failure.loaded_any());
            assert_eq!(failure.attempts().len(), 1);
            assert_eq!(failure.attempts()[0].origin(), LoadOrigin::LinkedArchive);
            assert_eq!(
                failure.to_string(),
                format!("{label}: it does not export the Advanced SDK entry points")
            );
        }
    }
}
