//! Structured causes for native library selection failures.

use std::{
    fmt,
    path::{Path, PathBuf},
};

/// Why an irreversible runtime or File selection cannot meet a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SelectionConflict {
    /// The request names a different library from the one selected.
    DifferentLibrary,
    /// The selected runtime does not meet the requested SDK edition.
    EditionNotMet,
    /// A library path cannot replace the implementation linked into the program.
    LinkedImplementation,
    /// Native loading re-entered selection on its own loading thread.
    ReentrantSelection,
    /// The selected runtime is Standard, but the File library needs Advanced.
    FileNeedsAdvancedRuntime,
    /// The process's global symbol scope contains another NDI runtime.
    ForeignRuntimeInScope,
}

/// Which native library a failed selection sought.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LibraryKind {
    /// The NDI runtime.
    Runtime,
    /// The separately loaded NDI File library.
    File,
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
    /// A bare name or Apple `@rpath` searched by the platform loader.
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
            Self::LoaderSearch => "system library search",
            Self::LinkedArchive => "linked SDK archive",
        })
    }
}

/// Why a candidate could not provide a usable implementation.
///
/// This is a structured diagnostic, not a promise that the platform loader
/// can distinguish an absent bare name from every other loading failure.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        /// The native symbol's name.
        symbol: String,
        /// Whether it is an API-required entry rather than the identification anchor.
        required: bool,
        /// The platform's symbol lookup diagnostic, when one is available.
        detail: Option<String>,
    },
    /// A lookup found the entry point in a dependency instead of the selected module.
    EntryPointOutsideModule {
        /// The native symbol's name.
        symbol: String,
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
        /// The first missing entry point in declaration order.
        first_missing: String,
    },
    /// The File library's imports bind to a runtime other than the selected one.
    BoundToAnotherRuntime,
    /// `NDI_RUNTIME_DIR_V6` names a relative directory, which is never searched.
    RelativeRuntimeDirectory,
}

impl fmt::Display for LoadRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => f.write_str("not found"),
            Self::PlatformLoader { detail } | Self::NotIdentifiable { detail }
                | Self::UnreadableVersion { detail } => f.write_str(detail),
            Self::ResolvedToAnotherFile { resolved } => write!(f, "the loader resolved it to {}", resolved.display()),
            Self::MissingEntryPoint { symbol, required, detail } => {
                if *required {
                    write!(f, "it does not export the required entry point {symbol}")?;
                } else {
                    write!(f, "it does not export {symbol}")?;
                }
                if let Some(detail) = detail { write!(f, ": {detail}")?; }
                Ok(())
            }
            Self::EntryPointOutsideModule { symbol, module } => write!(f, "{symbol} resolves outside {}", module.display()),
            Self::EditionNotMet => f.write_str("it does not export the Advanced SDK entry points"),
            Self::AlreadyRefused { path, refusal } => write!(f,
                "the platform loader returned {}, which is already loaded and was rejected ({refusal}); no other library of that name can be found by name in this process, so remove or fix that file or pass an explicit library path", path.display()),
            Self::NoCompleteFileFamily { first_missing } => write!(f, "it provides no family of NDI File entry points (it does not export {first_missing})"),
            Self::BoundToAnotherRuntime => f.write_str("it is bound to an NDI runtime other than the selected one"),
            Self::RelativeRuntimeDirectory => f.write_str("the runtime directory must be absolute"),
        }
    }
}

/// One refused native library location, in the order it was tried.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadFailure(Box<FailureData>);

#[derive(Debug, Clone, PartialEq, Eq)]
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

impl fmt::Display for LoadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(description) = &self.0.description {
            write!(f, "no usable {description}; tried ")?;
        }
        for (index, attempt) in self.0.attempts.iter().enumerate() {
            if index != 0 {
                f.write_str("; ")?;
            }
            fmt::Display::fmt(attempt, f)?;
        }
        Ok(())
    }
}

impl std::error::Error for LoadFailure {}

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
            (LoadRefusal::MissingEntryPoint { symbol: "NDIlib_version".into(), required: false, detail: Some("undefined symbol".into()) }, "it does not export NDIlib_version: undefined symbol"),
            (LoadRefusal::MissingEntryPoint { symbol: "NDIlib_initialize".into(), required: true, detail: None }, "it does not export the required entry point NDIlib_initialize"),
            (LoadRefusal::EntryPointOutsideModule { symbol: "NDIlib_initialize".into(), module: "selected.so".into() }, "NDIlib_initialize resolves outside selected.so"),
            (LoadRefusal::EditionNotMet, "it does not export the Advanced SDK entry points"),
            (LoadRefusal::UnreadableVersion { detail: "NDIlib_version returned no version text".into() }, "NDIlib_version returned no version text"),
            (LoadRefusal::AlreadyRefused { path: "rejected.so".into(), refusal: Box::new(LoadRefusal::EditionNotMet) }, "the platform loader returned rejected.so, which is already loaded and was rejected (it does not export the Advanced SDK entry points); no other library of that name can be found by name in this process, so remove or fix that file or pass an explicit library path"),
            (LoadRefusal::NoCompleteFileFamily { first_missing: "NDIlib_file_player_create".into() }, "it provides no family of NDI File entry points (it does not export NDIlib_file_player_create)"),
            (LoadRefusal::BoundToAnotherRuntime, "it is bound to an NDI runtime other than the selected one"),
            (LoadRefusal::RelativeRuntimeDirectory, "the runtime directory must be absolute"),
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
    fn every_conflict_kind_retains_its_diagnostic() {
        for kind in [
            SelectionConflict::DifferentLibrary,
            SelectionConflict::EditionNotMet,
            SelectionConflict::LinkedImplementation,
            SelectionConflict::ReentrantSelection,
            SelectionConflict::FileNeedsAdvancedRuntime,
            SelectionConflict::ForeignRuntimeInScope,
        ] {
            let error = Error::RuntimeConflict {
                kind,
                detail: "selection context".into(),
            };
            assert_eq!(
                error.to_string(),
                "Conflicting NDI runtime selection: selection context"
            );
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
