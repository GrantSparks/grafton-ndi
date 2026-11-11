//! Optional recording and playback through the NDI File API.
//!
//! [`FileApi::probe`] inspects a recording without advertising a source.
//! [`FileApi::create_player`] immediately advertises a source; stopping playback
//! keeps that source alive until its player is dropped. Recorders subscribe to
//! a source and preserve the SDK's encoded essence rather than accepting Rust
//! video buffers.
//!
//! Call [`FileRecorder::finish`] and inspect its result before treating a recording
//! as finalized. Dropping a recorder releases native resources but does not prove
//! that its output was finalized. Native operations, including destruction, may
//! block; timeout arguments bound event waits, not all native processing.
//!
//! Native owners are intentionally thread-affine until the SDK documents a wider
//! concurrency contract. Owned snapshots can be transferred between threads.
//! [`AsyncFilePlayer`] and [`AsyncFileRecorder`] keep each native owner on a
//! dedicated thread and expose futures usable with any Rust executor.

mod asynchronous;
mod ffi;
#[cfg(test)]
pub(crate) use ffi::FUNCTION_NAMES;
mod player;
mod recorder;
mod types;

pub use asynchronous::{
    AsyncFileOptions, AsyncFilePlayer, AsyncFileRecorder, FileEventFuture, FileOperation,
};
pub use player::FilePlayer;
pub use recorder::{FileRecorder, FinishStatus, RecorderState, RecordingReport, StopRequest};
pub use types::*;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::NDI;

/// Selection of the optional File runtime. No SDK object is created by this value.
#[derive(Clone, Debug, Default)]
pub struct FileRuntimeOptions {
    library_path: Option<PathBuf>,
}
impl FileRuntimeOptions {
    /// Select an absolute File module path. Its dependencies must match the core runtime.
    pub fn with_library_path(mut self, path: impl Into<PathBuf>) -> FileResult<Self> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(FileError::InvalidInput(
                "File library path must be absolute".into(),
            ));
        }
        self.library_path = Some(path);
        Ok(self)
    }
}

struct Backend {
    functions: ffi::Functions,
    // Native owners are destroyed before their Backend Arc. The module therefore
    // outlives every function pointer and resource, including failed conversions.
    #[cfg(not(ndi_link_static))]
    _library: Option<libloading::Library>,
    _ndi: Option<NDI>,
    path: Option<PathBuf>,
}

/// An optional, validated File operation group retaining its runtime dependencies.
#[derive(Clone)]
pub struct FileApi {
    backend: Arc<Backend>,
}
impl std::fmt::Debug for FileApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileApi")
            .field("library_path", &self.backend.path)
            .finish_non_exhaustive()
    }
}
impl FileApi {
    /// Resolve the complete File operation group without creating a player or recorder.
    ///
    /// The current POSIX File distribution requires the matching Advanced core.
    /// It is rejected before loading when the selected core is Standard. An absent
    /// optional File library never prevents existing core operations from running.
    pub fn open(ndi: &NDI, options: &FileRuntimeOptions) -> FileResult<Self> {
        #[cfg(ndi_link_static)]
        {
            if options.library_path.is_some() {
                return Err(FileError::InvalidInput(
                    "a shared File library path cannot be selected for static linkage".into(),
                ));
            }
            #[cfg(ndi_file_static)]
            return Ok(Self {
                backend: Arc::new(Backend {
                    functions: ffi::Functions::linked(),
                    _ndi: Some(ndi.clone()),
                    path: None,
                }),
            });
            #[cfg(not(ndi_file_static))]
            {
                let _ = ndi;
                Err(unavailable(
                    "file",
                    "BuildArtifactUnavailable: this static SDK profile has no linked File artifact",
                ))
            }
        }
        #[cfg(not(ndi_link_static))]
        {
            #[cfg(not(target_os = "windows"))]
            if ndi.info().edition() != crate::capabilities::SdkEdition::Advanced {
                return Err(unavailable("file", "RuntimeEditionMismatch: the POSIX File artifact requires the selected Advanced core; loading it alongside Standard is unsupported"));
            }
            if cfg!(all(target_os = "windows", target_arch = "x86")) {
                return Err(unavailable(
                    "file",
                    "TargetArtifactUnavailable: the SDK supplies no Windows x86 File library",
                ));
            }

            let path = if let Some(path) = &options.library_path {
                path.clone()
            } else {
                let info = ndi.info();
                let core_path = info.library_path().ok_or_else(|| {
                    FileError::Unavailable(
                        "the selected core module's absolute path is unavailable".into(),
                    )
                })?;
                let parent = core_path.parent().ok_or_else(|| {
                    FileError::Unavailable(
                        "the selected core module has no parent directory".into(),
                    )
                })?;
                parent.join(file_library_name())
            };
            // Explicit path, no current-directory search or fallback to a second
            // edition. libloading retains the exact handle used for all lookups.
            let library = load_module(&path).map_err(|error| {
                unavailable(
                    "file",
                    format!("ModuleLoadFailed: {}: {error}", path.display()),
                )
            })?;
            #[cfg(not(target_os = "windows"))]
            {
                type Initialize = unsafe extern "C" fn() -> bool;
                let from_file = unsafe { library.get::<Initialize>(b"NDIlib_initialize\0") }
                    .map_err(|error| {
                        unavailable("file", format!("DependencySymbolUnavailable: {error}"))
                    })?;
                let from_core = ndi.api().core.NDIlib_initialize;
                if *from_file as usize != from_core as usize {
                    return Err(unavailable("file", "RuntimeModuleConflict: File dependency resolves to a different NDI core module"));
                }
            }
            let functions = unsafe { ffi::Functions::load(&library) }?;
            Ok(Self {
                backend: Arc::new(Backend {
                    functions,
                    _library: Some(library),
                    _ndi: Some(ndi.clone()),
                    path: Some(path),
                }),
            })
        }
    }

    /// The exact requested File module path, or `None` for a static provider.
    pub fn library_path(&self) -> Option<&Path> {
        self.backend.path.as_deref()
    }

    /// Read a file's track layout and duration without advertising an NDI sender.
    pub fn probe(&self, path: impl AsRef<Path>) -> FileResult<FileInfo> {
        let path = types::path_string(path.as_ref())?;
        let mut raw = ffi::FileInfo::default();
        if !unsafe { (self.backend.functions.player_probe)(path.as_ptr(), &mut raw) } {
            return Err(FileError::Rejected("file probe"));
        }
        FileInfo::try_from(&raw)
    }

    /// Create and immediately advertise a player source with a fixed name/groups.
    pub fn create_player(&self, options: PlayerOptions) -> FileResult<FilePlayer> {
        FilePlayer::create(self.clone(), options)
    }

    /// Prepare a source recorder. Writing starts only on an explicit `start` call.
    pub fn create_recorder(&self, options: RecorderOptions) -> FileResult<FileRecorder> {
        FileRecorder::create(self.clone(), options)
    }
}

#[cfg(not(ndi_link_static))]
fn file_library_name() -> &'static str {
    if let Some(name) = option_env!("GRAFTON_NDI_FILE_LIBRARY_NAME") {
        if !name.is_empty() {
            return name;
        }
    }
    if cfg!(target_os = "windows") {
        "Processing.NDI.Lib.File.x64.dll"
    } else if cfg!(target_os = "macos") {
        "libndifile.dylib"
    } else if cfg!(target_os = "android") {
        "libndifile.so"
    } else {
        "libndifile.so.6"
    }
}

pub(super) fn timeout_ms(wait: Duration) -> FileResult<u32> {
    // Reserve the maximum integer rather than accidentally passing a possible
    // native infinity sentinel. Fractional milliseconds are rounded up.
    let millis = wait.as_millis() + u128::from(!wait.subsec_nanos().is_multiple_of(1_000_000));
    if millis >= u128::from(u32::MAX) {
        return Err(FileError::InvalidInput(
            "event wait must be less than u32::MAX milliseconds".into(),
        ));
    }
    Ok(millis as u32)
}

#[cfg(test)]
mod abi_tests;

#[cfg(all(not(ndi_link_static), unix))]
fn load_module(path: &Path) -> Result<libloading::Library, libloading::Error> {
    // Eager binding detects missing dependent exports before any File resource.
    unsafe {
        libloading::os::unix::Library::open(
            Some(path),
            libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_LOCAL,
        )
    }
    .map(Into::into)
}
#[cfg(all(not(ndi_link_static), not(unix)))]
fn load_module(path: &Path) -> Result<libloading::Library, libloading::Error> {
    unsafe { libloading::Library::new(path) }
}

pub(super) fn unavailable(capability: &str, reason: impl Into<String>) -> FileError {
    FileError::Core(crate::Error::UnsupportedCapability {
        capability: capability.into(),
        reason: reason.into(),
    })
}

#[cfg(test)]
impl FileApi {
    fn mocked(functions: ffi::Functions) -> Self {
        Self {
            backend: Arc::new(Backend {
                functions,
                #[cfg(not(ndi_link_static))]
                _library: None,
                _ndi: None,
                path: None,
            }),
        }
    }
}
#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod sdk_tests;
