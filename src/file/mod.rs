//! Recording NDI sources to disk and playing recordings back, through the NDI
//! File library.
//!
//! Enabled by `file-io`, independently of the `advanced` Cargo feature. The
//! File ABI is included, so desktop and Android builds need no Advanced
//! headers or `libclang`. Linux, macOS and Android still require the native
//! Advanced runtime with the shipped 6.4.0.2 File binaries; 64-bit Windows
//! supports either runtime edition. Both SDK editions distribute the File API.
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
//! When nothing is usable, [`Error::RuntimeLoadFailed`] names every location
//! tried and the operating system's reason for each. Like the NDI runtime, a
//! loaded File library stays selected and loaded for the life of the process.
//!
//! On Linux, macOS and Android the 6.4.0.2 File library imports the NDI runtime's entry
//! points from the **Advanced** SDK runtime, so it can only be used when that is
//! the selected runtime (see [`RuntimeOptions::edition`](crate::RuntimeOptions::edition)).
//! Beside a Standard runtime it is refused with [`Error::RuntimeConflict`]
//! before anything is loaded. The
//! Windows File library imports nothing from the NDI runtime, so it is not tied
//! to an edition; the SDK ships it for 64-bit Windows only. Apple mobile targets
//! link the File archive at build time.
//!
//! A library that loads but cannot be used stays loaded. If the platform
//! loader's own search then matches the library name to that module, it returns
//! it again instead of looking further; the error names the file, and the remedy
//! is to remove or fix it or to pass [`FileRuntimeOptions::library_path`].
//!
//! The native borrowed source/path getters are deliberately not called: their
//! strings can be invalidated by the SDK's recording thread. Use
//! [`FileRecorder::last_accepted_source`] for the last accepted source request
//! and [`FileRecorder::reported_current_file`] for the owned event view of the
//! file. These can lag or differ from the native state as their docs describe.
//!
//! # What the File library provides
//!
//! Its entry points come in [`FileFamily`] groups: playback, which also
//! probes recordings, and recording. A library is selected when it provides
//! at least one family completely. [`FileApi::status`] reports each family,
//! and an operation that needs a family the selected library lacks fails with
//! [`Error::UnsupportedCapability`] for
//! [`RuntimeFeature::File`](crate::capabilities::RuntimeFeature::File), naming
//! the entry point the library does not export. The runtime's own
//! [`Capabilities`](crate::capabilities::Capabilities) do not know what the
//! File library provides.
//!
//! # Finishing a recording
//!
//! The SDK finalizes a file asynchronously after a stop request and reports it
//! with a file-closed event. [`FileRecorder::finish`] requests the stop, waits
//! for that event and returns a [`FinishStatus`]; treat a recording as complete
//! only when it is [`FinishStatus::Closed`], and inspect the [`RecordingReport`]
//! for errors during the session. [`RecordingReport::stopped_by_sdk`] says
//! whether the SDK ended the session before it was asked to stop. Dropping a
//! recorder whose session is still open performs the same stop and wait,
//! bounded by [`RecorderOptions::drop_wait`], but cannot report the outcome.
//!
//! After a finalization timeout, [`FileRecorder::reset`] explicitly abandons
//! the uncertain session and replaces its native instance. The old report is
//! returned without confirmation of closure; a new event queue keeps late
//! events from being attributed to the replacement's next session.
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
//!   [`FileRecorder::rollover`] checks the file its pattern names first, so a
//!   missing directory or an exact file name that already exists is refused
//!   and the session goes on; a file that cannot be opened for another reason
//!   still ends it. An exact file name cannot roll over by itself at all; see
//!   [`FileNaming::Exact`].
//!
//! # Threads
//!
//! [`FilePlayer`] and [`FileRecorder`] are neither `Send` nor `Sync`: the SDK
//! documents no threading contract for them, so each stays on the thread that
//! created it. Every native call can block, including destruction; timeouts
//! bound event waits only. The values they return are owned and can be sent
//! anywhere. [`AsyncFilePlayer`] and [`AsyncFileRecorder`] keep each native
//! object on a dedicated thread and expose futures that work with any executor.
//! Those futures submit work on their first poll; [`FileOperation::submit`]
//! explicitly submits immediately. Submitted stop, finish and close operations
//! survive cancellation of their futures. Dropping the last async handle can
//! block; await its `close()` operation before dropping it on an executor.
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
//!     Ndi,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let ndi = Ndi::new()?;
//! let file = FileApi::open(&ndi, &FileRuntimeOptions::default())?;
//!
//! let options = RecorderOptions::new(SourceSpec::new("STUDIO (Camera 1)")?);
//! let mut recorder = file.create_recorder(&options)?;
//! recorder.start(&RecordingTarget::new("take")?, RecordingTimecode::Preserve)?;
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

use std::path::{Path, PathBuf};

use crate::{
    capabilities::CapabilityStatus,
    sys::{self, bindings::NDIlib_file_player_file_info_t},
    Error, Ndi, Result,
};
use out_param::OutParam;

/// A family of the NDI File library's entry points, which a library provides
/// completely or not at all.
///
/// [`FileApi::status`] reports whether the selected library provides one, and
/// an operation that needs a family the library lacks fails with
/// [`Error::UnsupportedCapability`] for
/// [`RuntimeFeature::File`](crate::capabilities::RuntimeFeature::File) and
/// that family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FileFamily {
    /// Probing recordings ([`FileApi::probe`]) and playing them back as an
    /// NDI source ([`FilePlayer`], [`AsyncFilePlayer`]).
    Playback,
    /// Recording NDI sources to disk ([`FileRecorder`], [`AsyncFileRecorder`]).
    Recording,
}

impl FileFamily {
    /// Every family.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 2] = [Self::Playback, Self::Recording];

    /// The name the family is reported under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Playback => "file playback",
            Self::Recording => "file recording",
        }
    }
}

/// How the File library is located. Creating this value loads nothing.
#[derive(Clone, Debug, Default)]
pub struct FileRuntimeOptions {
    library_path: Option<PathBuf>,
}

impl FileRuntimeOptions {
    /// Restore the normal File library search instead of an explicit path.
    #[must_use]
    pub fn clear_library_path(mut self) -> Self {
        self.library_path = None;
        self
    }

    /// Requested exact File library path, or `None` for the normal search.
    #[must_use]
    pub fn configured_library_path(&self) -> Option<&Path> {
        self.library_path.as_deref()
    }

    /// The default options: the first usable File library from the
    /// [documented search](crate::file#where-the-file-library-is-loaded-from).
    pub fn new() -> Self {
        Self::default()
    }

    /// Load exactly this absolute File library path.
    ///
    /// No other location is tried, so a failure to load it is reported rather
    /// than hidden by a fallback. A relative path is rejected by
    /// [`FileApi::open`], as is any path on a target that links the SDK
    /// statically.
    #[must_use]
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
    runtime: &'static sys::FileRuntime,
    /// Keeps the NDI runtime initialized.
    #[cfg(not(test))]
    _ndi: Ndi,
    /// In a unit-test build, the API of a library of test doubles has none:
    /// it reaches no runtime. Every other API keeps one, as in other builds.
    #[cfg(test)]
    _ndi: Option<Ndi>,
}

impl std::fmt::Debug for FileApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileApi")
            .field("library_path", &self.library_path())
            .field("playback", &self.status(FileFamily::Playback))
            .field("recording", &self.status(FileFamily::Recording))
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
    /// * [`Error::RuntimeLoadFailed`] when no usable File library can be
    ///   loaded. This is retryable, and the rest of the crate keeps working.
    /// * [`Error::RuntimeConflict`] when the selected NDI runtime is not one
    ///   the File library can accompany, or `options` names a library other
    ///   than the one already selected.
    /// * [`Error::InvalidInput`] when `options` names a relative path.
    pub fn open(ndi: &Ndi, options: &FileRuntimeOptions) -> Result<Self> {
        let runtime = sys::select_file(ndi.info(), ndi.api(), options.library_path.as_deref())?;
        Ok(Self {
            runtime,
            #[cfg(not(test))]
            _ndi: ndi.clone(),
            #[cfg(test)]
            _ndi: Some(ndi.clone()),
        })
    }

    /// The file the library was loaded from, or `None` when the SDK is linked
    /// statically.
    pub fn library_path(&self) -> Option<&Path> {
        self.runtime.path()
    }

    /// Whether the selected library provides `family`.
    ///
    /// [`CapabilityStatus::Unavailable`] with
    /// [`SymbolUnavailable`](crate::capabilities::CapabilityUnavailable::SymbolUnavailable)
    /// when it does not export one of the family's entry points. Every
    /// operation that needs the family then fails with
    /// [`Error::UnsupportedCapability`], and keeps failing: the library stays
    /// selected for the life of the process.
    pub fn status(&self, family: FileFamily) -> CapabilityStatus {
        self.runtime.status(family)
    }

    /// Whether the selected library provides `family`; see
    /// [`status`](Self::status).
    pub fn supports(&self, family: FileFamily) -> bool {
        self.status(family) == CapabilityStatus::Available
    }

    /// Read a recording's duration and track layout.
    ///
    /// Nothing is played and no NDI source is advertised.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK cannot read the file,
    /// [`Error::InvalidInput`] when the path is empty or is not UTF-8,
    /// [`Error::InvalidCString`] when it contains a NUL, [`Error::Io`] when a
    /// relative path cannot be resolved because the current directory is
    /// unavailable, and [`Error::UnsupportedCapability`] when the library
    /// does not provide [`FileFamily::Playback`].
    pub fn probe(&self, path: impl AsRef<Path>) -> Result<FileInfo> {
        let playback = self.table::<sys::FilePlayback>()?;
        let path = types::path_string(path.as_ref())?;
        let mut info = OutParam::<NDIlib_file_player_file_info_t>::new();
        // SAFETY: `path` is a NUL-terminated string that outlives the call and
        // `info` is writable storage for the structure the SDK fills in.
        let probed =
            unsafe { playback.NDIlib_file_player_probe_file(path.as_ptr(), info.as_mut_ptr()) };
        let info = info.read()?;
        if !probed {
            return Err(Error::NativeOperationFailed {
                operation: "file probe",
            });
        }
        FileInfo::from_sdk(&info)
    }

    /// Create a player and advertise its NDI source.
    ///
    /// The source is advertised from this call until the player is dropped,
    /// whether or not a file is playing. The options are only read, so they
    /// stay with the caller whatever the outcome.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the player's name is empty, and
    /// [`Error::InvalidCString`] when its name or groups contain a NUL.
    /// [`Error::NativeOperationFailed`] when the SDK cannot create the player
    /// and [`Error::UnsupportedCapability`] when the library does not provide
    /// [`FileFamily::Playback`].
    pub fn create_player(&self, options: &PlayerOptions) -> Result<FilePlayer> {
        FilePlayer::create(self.clone(), options)
    }

    /// Create a recorder for the source named in `options`.
    ///
    /// Nothing is written until [`FileRecorder::start`]. The options are only
    /// read, so they stay with the caller whatever the outcome; the recorder
    /// keeps its own copy of the source they name.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the options are malformed or conflict:
    /// an empty track selection, a drop wait beyond [`crate::MAX_TIMEOUT`], a
    /// malformed [`FileNaming::Pattern`], or an exact file name
    /// ([`FileNaming::Exact`], or a pattern without `{index}`) with
    /// format-change rollover switched on or a nonzero rollover size.
    /// [`Error::InvalidCString`] when the filename pattern contains a NUL.
    /// [`Error::NativeOperationFailed`] when the SDK cannot create the
    /// recorder and [`Error::UnsupportedCapability`] when the library does not
    /// provide [`FileFamily::Recording`].
    pub fn create_recorder(&self, options: &RecorderOptions) -> Result<FileRecorder> {
        FileRecorder::create(self.clone(), options)
    }

    /// The table of a family, or the error for the first of its entry points
    /// the library does not export. A player, recorder or probe takes its
    /// table here, before it calls anything, so none runs without its family.
    fn table<T: sys::FileTable>(&self) -> Result<&'static T> {
        self.runtime.table()
    }
}

#[cfg(test)]
impl FileApi {
    /// An API whose entry points are the given test doubles.
    fn with_tables(playback: sys::FilePlayback, recording: sys::FileRecording) -> Self {
        Self::with_runtime(sys::FileRuntime::test_double(Ok(playback), Ok(recording)))
    }

    /// An API for a library of test doubles, which needs no runtime.
    fn with_runtime(runtime: &'static sys::FileRuntime) -> Self {
        Self {
            runtime,
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
