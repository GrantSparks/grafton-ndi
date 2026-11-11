//! Recording NDI sources to disk and playing recordings back, through the NDI
//! File library.
//!
//! The File library is a separate native library that accompanies the NDI
//! runtime. [`FileApi::open`] finds it when it is first needed; nothing else in
//! this crate depends on it, so its absence never affects sending or receiving.
//!
//! * [`FileApi::probe`] reads a recording's duration and track layout without
//!   advertising anything on the network.
//! * [`FileApi::create_player`] advertises an NDI source and plays recordings
//!   through it; see [`FilePlayer`].
//! * [`FileApi::create_recorder`] records a source's compressed essence exactly
//!   as it arrives; see [`FileRecorder`]. The recorder is source-driven: it
//!   subscribes to an NDI source and does not accept frames from Rust.
//!
//! # Where the File library is loaded from
//!
//! Unless [`FileRuntimeOptions::library_path`] names an exact file, the first
//! usable library in this order is selected:
//!
//! 1. the directory of the selected NDI runtime library, where the SDK installs it;
//! 2. the directory containing the running executable;
//! 3. the directory named by the `NDI_RUNTIME_DIR_V6` environment variable;
//! 4. the platform loader's own search for the library name.
//!
//! When nothing is usable, [`FileError::Unavailable`] names every location tried
//! and the operating system's reason for each. Like the NDI runtime, a loaded
//! File library stays selected and loaded for the life of the process.
//!
//! On Linux, macOS and Android the File library imports the NDI runtime's entry
//! points from the **Advanced** SDK runtime, so it can only be used when that is
//! the selected runtime (see [`RuntimeOptions::edition`](crate::RuntimeOptions::edition)).
//! Beside a Standard runtime it is refused before anything is loaded. The
//! Windows File library imports nothing from the NDI runtime, so it is not tied
//! to an edition; the SDK ships it for 64-bit Windows only. Apple mobile targets
//! link the File archive at build time.
//!
//! A library that loads but cannot be used stays loaded. If the platform
//! loader's own search then matches the library name to that module, it returns
//! it again instead of looking further; the error names the file, and the remedy
//! is to remove or fix it or to pass [`FileRuntimeOptions::library_path`].
//!
//! # Finishing a recording
//!
//! The SDK finalizes a file asynchronously after a stop request and reports it
//! with a file-closed event. [`FileRecorder::finish`] requests the stop, waits
//! for that event and returns a [`FinishStatus`]; treat a recording as complete
//! only when it is [`FinishStatus::Closed`], and inspect the [`RecordingReport`]
//! for errors during the session. Dropping a recorder whose session is still
//! open performs the same stop and wait, bounded by
//! [`RecorderOptions::with_drop_wait`], but cannot report the outcome.
//!
//! # Limits of the NDI 6.4 File library
//!
//! These were observed with the NDI 6.4 File library. They lie inside the
//! library and this crate cannot correct them.
//!
//! * **A file reported closed is not proof that its data was written.** A
//!   session's report repeats what the library said: that it finalized each
//!   file. When the disk fills while a session is being stopped, a write comes
//!   up short, the library extends the file all the same and reports it closed
//!   without an error, and [`FileApi::probe`] then reports a final duration
//!   for a file whose data is not there. [`FinishStatus::Closed`] with no
//!   errors therefore means that the container was finalized, not that every
//!   frame reached the disk. A recording that must be complete has to be
//!   checked by reading it back.
//! * **The process does not survive a full disk during recording.** When a
//!   write fails while a session is recording, the library queues a
//!   write-failed and an error event and then crashes in its own writer
//!   thread, which ends the process. Nothing this crate does can prevent it.
//!   Record only where enough space is certain.
//! * **A file can be closed with nothing in it.** The library opens a file as
//!   soon as a session or a rollover starts, and reports it closed even if no
//!   frame arrived: after a stop before the source delivered anything, a
//!   switch to a source that is not there, or a rollover while the source is
//!   lost. [`RecordingReport::empty_files`] counts these files.
//! * **A rollover the library cannot carry out ends the session.** It accepts
//!   the request, fails to open the new file, closes the old one and stops.
//!   [`FileRecorder::rollover`] checks the target's directory first, so the
//!   usual mistake is refused and the session goes on; other causes, such as
//!   an exact file name that already exists, still end it.
//!
//! # Threads
//!
//! [`FilePlayer`] and [`FileRecorder`] are neither `Send` nor `Sync`: the SDK
//! documents no threading contract for them, so each stays on the thread that
//! created it. Every native call can block, including destruction; timeouts
//! bound event waits only. The values they return are owned and can be sent
//! anywhere. [`AsyncFilePlayer`] and [`AsyncFileRecorder`] keep each native
//! object on a dedicated thread and expose futures that work with any executor.
//!
//! # Examples
//!
//! ```no_run
//! use std::time::Duration;
//!
//! use grafton_ndi::{
//!     file::{
//!         FileApi, FileRuntimeOptions, FinishStatus, RecorderOptions, RecordingTarget,
//!         RecordingTimecode, SourceSpec,
//!     },
//!     NDI,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let ndi = NDI::new()?;
//! let file = FileApi::open(&ndi, &FileRuntimeOptions::default())?;
//!
//! let mut recorder = file.create_recorder(RecorderOptions::new(SourceSpec::new("STUDIO (Camera 1)")?))?;
//! recorder.start(RecordingTarget::new("take")?, RecordingTimecode::Preserve)?;
//! std::thread::sleep(Duration::from_secs(10));
//!
//! match recorder.finish(Duration::from_secs(5))? {
//!     FinishStatus::Closed(report) => println!("closed {} file(s)", report.closed_files()),
//!     other => println!("not confirmed complete: {other:?}"),
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod asynchronous;
mod out_param;
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
    ffi::CStr,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    sys::{self, bindings::NDIlib_file_player_file_info_t},
    NDI,
};
use out_param::OutParam;

/// How the File library is located. Creating this value loads nothing.
#[derive(Clone, Debug, Default)]
pub struct FileRuntimeOptions {
    library_path: Option<PathBuf>,
}

impl FileRuntimeOptions {
    /// Load exactly this absolute File library path.
    ///
    /// No other location is tried, so a failure to load it is reported rather
    /// than hidden by a fallback. A relative path is rejected by
    /// [`FileApi::open`], as is any path on a target that links the SDK
    /// statically.
    pub fn library_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.library_path = Some(path.into());
        self
    }
}

/// The File library selected for this process.
///
/// Cloning is cheap. Every clone, and every player and recorder created from
/// one, keeps the NDI runtime initialized.
#[derive(Clone)]
pub struct FileApi {
    playback: Result<&'static sys::FilePlayback, &'static CStr>,
    recording: Result<&'static sys::FileRecording, &'static CStr>,
    library_path: Option<&'static Path>,
    // Absent only when the entry points are test doubles, which need no runtime.
    _ndi: Option<NDI>,
}

impl std::fmt::Debug for FileApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileApi")
            .field("library_path", &self.library_path)
            .finish_non_exhaustive()
    }
}

impl FileApi {
    /// Find and load the File library that accompanies `ndi`'s runtime.
    ///
    /// The first successful call selects the library for the process; later
    /// calls return the same one. No player or recorder is created and nothing
    /// is advertised.
    ///
    /// # Errors
    ///
    /// * [`FileError::Unavailable`] when no usable File library can be loaded,
    ///   or the selected NDI runtime is not one it can accompany. This is
    ///   retryable, and the rest of the crate keeps working.
    /// * [`FileError::Core`] with [`Error::RuntimeConflict`](crate::Error::RuntimeConflict)
    ///   when `options` names a library other than the one already selected,
    ///   or with [`Error::InvalidConfiguration`](crate::Error::InvalidConfiguration)
    ///   when it names a relative path.
    pub fn open(ndi: &NDI, options: &FileRuntimeOptions) -> FileResult<Self> {
        let runtime = sys::select_file(ndi.info(), ndi.api(), options.library_path.as_deref())
            .map_err(|error| match error {
                crate::Error::RuntimeLoadFailed(reason) => FileError::Unavailable(reason),
                other => FileError::Core(other),
            })?;
        Ok(Self {
            playback: runtime.playback(),
            recording: runtime.recording(),
            library_path: runtime.path(),
            _ndi: Some(ndi.clone()),
        })
    }

    /// The file the library was loaded from, or `None` when the SDK is linked
    /// statically.
    pub fn library_path(&self) -> Option<&Path> {
        self.library_path
    }

    /// Read a recording's duration and track layout.
    ///
    /// Nothing is played and no NDI source is advertised.
    ///
    /// # Errors
    ///
    /// [`FileError::Rejected`] when the SDK cannot read the file,
    /// [`FileError::InvalidInput`] when the path is not UTF-8, and
    /// [`FileError::Unavailable`] when the library provides no playback.
    pub fn probe(&self, path: impl AsRef<Path>) -> FileResult<FileInfo> {
        let playback = self.playback()?;
        let path = types::path_string(path.as_ref())?;
        let mut info = OutParam::<NDIlib_file_player_file_info_t>::new();
        // SAFETY: `path` is a NUL-terminated string that outlives the call and
        // `info` is writable storage for the structure the SDK fills in.
        let probed =
            unsafe { playback.NDIlib_file_player_probe_file(path.as_ptr(), info.as_mut_ptr()) };
        let info = info.read()?;
        if !probed {
            return Err(FileError::Rejected("file probe"));
        }
        FileInfo::try_from(&info)
    }

    /// Create a player and advertise its NDI source.
    ///
    /// The source is advertised from this call until the player is dropped,
    /// whether or not a file is playing.
    ///
    /// # Errors
    ///
    /// [`FileError::Rejected`] when the SDK cannot create the player and
    /// [`FileError::Unavailable`] when the library provides no playback.
    pub fn create_player(&self, options: PlayerOptions) -> FileResult<FilePlayer> {
        FilePlayer::create(self.clone(), options)
    }

    /// Create a recorder for the source named in `options`.
    ///
    /// Nothing is written until [`FileRecorder::start`].
    ///
    /// # Errors
    ///
    /// [`FileError::Rejected`] when the SDK cannot create the recorder and
    /// [`FileError::Unavailable`] when the library provides no recording.
    pub fn create_recorder(&self, options: RecorderOptions) -> FileResult<FileRecorder> {
        FileRecorder::create(self.clone(), options)
    }

    /// The playback entry points, or why the library provides none.
    fn playback(&self) -> FileResult<&'static sys::FilePlayback> {
        self.playback
            .map_err(|symbol| missing("file playback", symbol))
    }

    /// The recording entry points, or why the library provides none.
    fn recording(&self) -> FileResult<&'static sys::FileRecording> {
        self.recording
            .map_err(|symbol| missing("file recording", symbol))
    }
}

/// The error for a family of entry points the selected library lacks.
fn missing(family: &str, symbol: &CStr) -> FileError {
    FileError::Unavailable(format!(
        "the selected NDI File library provides no {family}: it does not export {}",
        symbol.to_string_lossy()
    ))
}

/// A wait as the SDK's millisecond timeout, rounded up.
///
/// The largest value is refused rather than passed on, because the SDK may
/// treat it as "wait forever".
pub(super) fn timeout_ms(wait: Duration) -> FileResult<u32> {
    let millis = wait.as_millis() + u128::from(!wait.subsec_nanos().is_multiple_of(1_000_000));
    u32::try_from(millis)
        .ok()
        .filter(|&millis| millis != u32::MAX)
        .ok_or_else(|| {
            FileError::InvalidInput("a wait must be less than u32::MAX milliseconds".into())
        })
}

#[cfg(test)]
impl FileApi {
    /// An API whose entry points are the given test doubles.
    fn with_tables(playback: sys::FilePlayback, recording: sys::FileRecording) -> Self {
        Self {
            playback: Ok(Box::leak(Box::new(playback))),
            recording: Ok(Box::leak(Box::new(recording))),
            library_path: None,
            _ndi: None,
        }
    }
}

#[cfg(test)]
mod mock;

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod sdk_tests;
