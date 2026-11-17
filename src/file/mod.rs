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
//! runtime. [`FileApi::new`] finds it when it is first needed; nothing else in
//! this crate depends on it, so its absence never affects sending or receiving.
//!
//! * [`FilePlayer::probe`] reads a recording's duration and track layout without
//!   advertising anything on the network.
//! * [`FilePlayer::new`] advertises an NDI source and plays recordings
//!   through it; see [`FilePlayer`].
//! * [`FileRecorder::new`] records a source's compressed essence exactly
//!   as it arrives; see [`FileRecorder`]. The recorder is source-driven: it
//!   subscribes to an NDI source and does not accept frames from Rust.
//!
//! # Where the File library is loaded from
//!
//! Unless [`FileApiOptions::library_path`] names an exact file, the first
//! usable library in this order is selected:
//!
//! 1. the directory of the selected NDI runtime library, where the SDK installs it;
//! 2. the directory containing the running executable;
//! 3. the absolute directory named by `NDI_RUNTIME_DIR_V6`; relative values
//!    are recorded as refused locations and never searched;
//! 4. platform-specific locations: Windows expands absolute `PATH` directories,
//!    then performs a System32-only name lookup; macOS tries `@rpath/<name>`
//!    and absolute `DYLD_LIBRARY_PATH` directories; Linux and Android retain
//!    the native loader search (`rpath`, `LD_LIBRARY_PATH` and system locations).
//!
//! Windows and macOS never add the working directory to this search. Relative
//! or empty `PATH`/`DYLD_LIBRARY_PATH` entries are recorded as refused locations.
//! On macOS, a relative or empty entry in either `DYLD_LIBRARY_PATH` or
//! `DYLD_FALLBACK_LIBRARY_PATH` refuses the entire default search before opening
//! a library. The check reads the process environment at search time; dyld uses
//! its launch-time values, so changing `DYLD_*` after launch is unsupported.
//! Explicit paths retain dyld's own policy.
//! In unrestricted macOS processes dyld can substitute `DYLD_LIBRARY_PATH`
//! even for an absolute path; the crate refuses a substitute with
//! [`crate::LoadRefusal::ResolvedToAnotherFile`]. Native dependencies still
//! follow platform loader rules.
//!
//! When nothing is usable, [`Error::RuntimeLoadFailed`] carries a
//! [`crate::LoadFailure`] for [`crate::LibraryKind::File`], with each attempted
//! location, origin and typed refusal. `loaded_any()` reports whether any
//! native code loaded before being refused. `Display` retains the diagnostic
//! text for each attempt, including the operating system's reason. Like the NDI runtime, a
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
//! is to remove or fix it or to pass [`FileApiOptions::library_path`].
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
//! [`Error::UnsupportedCapability`], naming the family and the first entry
//! point the library does not export.
//!
//! # Finishing a recording
//!
//! The SDK finalizes a file asynchronously after a stop request and reports it
//! with a file-closed event. [`FileRecorder::finish`] requests the stop, waits
//! for that event and returns a [`FinishStatus`]; treat a recording as complete
//! only when it is [`FinishStatus::Closed`], and inspect the [`RecordingReport`]
//! for errors during the session. [`RecordingReport::stopped_by_sdk`] says
//! whether the SDK ended the session before it was asked to stop. Dropping a
//! recorder whose session is still open requests the stop and waits within
//! [`FileRecorderOptions::drop_wait`], but cannot report the outcome. This
//! budget starts at drop (or async close submission) and includes the native
//! stop; after it returns, only the remaining budget is available for events.
//! Synchronous [`FileRecorder::finish`] instead measures its event wait from
//! the stop's return, giving finalization the full requested wait.
//! Native event loss, unreadable lifecycle evidence or exhaustion of bounded
//! file bookkeeping makes tracking incomplete. Cleanup then consumes its
//! remaining original budget even if every known file closes. Neither an empty
//! known file set nor a stopped recorder proves that unknown files finalized.
//! The indeterminate report stays unchanged, and a new session requires
//! [`FileRecorder::reset`]. Async application-notification eviction does not
//! affect the separately maintained native file tracking.
//!
//! After a finalization timeout, [`FileRecorder::reset`] explicitly abandons
//! the uncertain session and replaces its native instance. The old report is
//! returned without confirmation of closure; a new event queue keeps late
//! events from being attributed to the replacement's next session.
//!
//! # Limits of the NDI 6.4 File library
//!
//! These limits apply to the versions and platforms named below. They lie
//! inside the library and this crate cannot correct them.
//!
//! * **On Linux with the NDI 6.4 File library, a file reported closed is not
//!   proof that its data was written.** A
//!   session's report repeats what the library said: that it finalized each
//!   file. When the disk fills while a session is being stopped, a write comes
//!   up short, the library extends the file all the same and reports it closed
//!   without an error, and [`FilePlayer::probe`] then reports a final duration
//!   for a file whose data is not there. [`FinishStatus::Closed`] with no
//!   errors therefore means that the container was finalized, not that every
//!   frame reached the disk. A recording that must be complete has to be
//!   checked by reading it back.
//! * **On Linux with the NDI 6.4 File library, the process does not survive a
//!   full disk during recording.** When a
//!   write fails while a session is recording, the library queues a
//!   write-failed and an error event and then crashes in its own writer
//!   thread, which ends the process. Nothing this crate does can prevent it.
//!   Record only where enough space is certain.
//! * **On Linux with the NDI 6.4 File library, a file can be closed with
//!   nothing in it.** The library opens a file as
//!   soon as a session or a rollover starts, and reports it closed even if no
//!   frame arrived: after a stop before the source delivered anything, a
//!   switch to a source that is not there, or a rollover while the source is
//!   lost. [`RecordingReport::empty_files`] counts these files.
//! * **On Linux with the NDI 6.4 File library, a rollover the library cannot
//!   carry out ends the session.** It accepts
//!   the request, fails to open the new file, closes the old one and stops.
//!   [`FileRecorder::rollover`] checks the file its pattern names first, so a
//!   missing directory or an exact file name that already exists is refused
//!   and the session goes on; a file that cannot be opened for another reason
//!   still ends it. An exact file name cannot roll over by itself at all; see
//!   [`FileNaming::Exact`].
//! * **On every platform with the NDI 6.4 File library, recording paths must
//!   be reportable exactly.** Start and rollover
//!   preflight refuse absolute output paths above 4094 UTF-8 bytes with the
//!   current SDK, reserving at least 20 digits for each index token's growth.
//!   This keeps closure events unambiguous, so a finalized session can close
//!   and another session can start.
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
//! explicitly submits immediately. Submitted stop, finish, close and abandon operations
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
//!         FileApi, FileApiOptions, FileRecorder, FinishStatus, FileRecorderOptions, RecordingTarget,
//!         RecordingTimecode, SourceSpec,
//!     },
//!     Ndi,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let ndi = Ndi::new()?;
//! let file = FileApi::new(&ndi, &FileApiOptions::default())?;
//!
//! let options = FileRecorderOptions::new(SourceSpec::new("STUDIO (Camera 1)")?);
//! let mut recorder = FileRecorder::new(&file, &options)?;
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
#[cfg(windows)]
mod windows_permissions;

pub use asynchronous::{
    AsyncFileOptions, AsyncFilePlayer, AsyncFileRecorder, FileEventFuture, FileOperation,
};
pub use player::FilePlayer;
pub use recorder::{FileRecorder, FinishStatus, RecorderState, RecordingReport, StopRequest};
pub use types::*;

use std::path::{Path, PathBuf};

#[cfg(any(test, doc))]
use crate::Error;
use crate::{capabilities::CapabilityStatus, sys, Ndi, Result};
#[cfg(test)]
use out_param::OutParam;

/// A family of the NDI File library's entry points, which a library provides
/// completely or not at all.
///
/// [`FileApi::status`] reports whether the selected library provides one, and
/// an operation that needs a family the library lacks fails with
/// [`Error::UnsupportedCapability`], naming that family and its first missing
/// entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FileFamily {
    /// Probing recordings ([`FilePlayer::probe`]) and playing them back as an
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

impl std::fmt::Display for FileFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// How the File library is located. Creating this value loads nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct FileApiOptions {
    library_path: Option<PathBuf>,
}

impl FileApiOptions {
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
    /// [`FileApi::new`] with [`Error::InvalidInput`] on every target, before
    /// selection. A target that links the SDK statically refuses an absolute
    /// path with [`Error::RuntimeConflict`].
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
    runtime: FileBound<sys::FileRuntime>,
}

/// A native File capability retains its initialized core runtime. Synthetic
/// test capabilities explicitly reach no runtime and need no initialization.
/// File owners keep the same field type in production and unit-test builds.
enum FileBound<T: crate::runtime::BoundTable> {
    Native(crate::runtime::Bound<T>),
    #[cfg(test)]
    Synthetic(&'static T),
}

impl<T: crate::runtime::BoundTable> Clone for FileBound<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Native(bound) => Self::Native(bound.clone()),
            #[cfg(test)]
            Self::Synthetic(table) => Self::Synthetic(table),
        }
    }
}

impl<T: crate::runtime::BoundTable> std::ops::Deref for FileBound<T> {
    type Target = T;

    fn deref(&self) -> &T {
        match self {
            Self::Native(bound) => bound,
            #[cfg(test)]
            Self::Synthetic(table) => table,
        }
    }
}

impl FileBound<sys::FileRuntime> {
    /// Transfer the existing lease to the gated family before native creation.
    fn into_table<T: sys::FileTable>(self) -> Result<FileBound<T>> {
        match self {
            Self::Native(bound) => bound.into_file_table().map(FileBound::Native),
            #[cfg(test)]
            Self::Synthetic(runtime) => runtime.table().map(FileBound::Synthetic),
        }
    }

    fn bind_table<T: sys::FileTable>(&self) -> Result<FileBound<T>> {
        match self {
            Self::Native(bound) => bound.bind_file_table().map(FileBound::Native),
            #[cfg(test)]
            Self::Synthetic(runtime) => runtime.table().map(FileBound::Synthetic),
        }
    }
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
    ///   loaded. Its [`crate::LoadFailure`] records typed attempts and whether
    ///   any native code loaded before being refused. This is retryable, and
    ///   the rest of the crate keeps working.
    /// * [`Error::RuntimeConflict`] when the selected NDI runtime is not one
    ///   the File library can accompany, or `options` names a library other
    ///   than the one already selected. [`crate::SelectionFailure::kind`] names
    ///   the [`crate::SelectionConflict`]; its `Display` gives the diagnostic text.
    /// * [`Error::InvalidInput`] when `options` names a relative path.
    pub fn new(ndi: &Ndi, options: &FileApiOptions) -> Result<Self> {
        let runtime = sys::select_file(
            ndi.runtime().selected_runtime(),
            options.library_path.as_deref(),
        )?;
        Ok(Self {
            runtime: FileBound::Native(ndi.bind_file_runtime(runtime)),
        })
    }

    /// Canonical loaded file, with links resolved, or `None` for a linked SDK.
    /// Uses the platform's ordinary path form; Windows extended syntax is kept
    /// only when needed to express the same file.
    pub fn library_path(&self) -> Option<&Path> {
        self.runtime.path()
    }

    /// Whether the selected library provides `family`.
    ///
    /// [`CapabilityStatus::Unavailable`] with
    /// [`SymbolUnavailable`](crate::capabilities::CapabilityRefusal::SymbolUnavailable)
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

    /// Require the complete native entry-point family.
    ///
    /// Returns [`Error::UnsupportedCapability`] with the missing symbol when
    /// the selected File library does not provide `family`.
    pub fn require_family(&self, family: FileFamily) -> Result<()> {
        match self.status(family) {
            CapabilityStatus::Available => Ok(()),
            CapabilityStatus::Unavailable(refusal) => Err(crate::Error::UnsupportedCapability {
                capability: crate::capabilities::Capability::File(family),
                refusal,
            }),
        }
    }

    /// The table of a family, or the error for the first of its entry points
    /// the library does not export. A player, recorder or probe takes its
    /// table here, before it calls anything, so none runs without its family.
    fn table<T: sys::FileTable>(&self) -> Result<&T> {
        self.runtime.table()
    }

    fn into_table<T: sys::FileTable>(self) -> Result<FileBound<T>> {
        self.runtime.into_table()
    }

    fn bind_table<T: sys::FileTable>(&self) -> Result<FileBound<T>> {
        self.runtime.bind_table()
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
            runtime: FileBound::Synthetic(runtime),
        }
    }
}

#[cfg(test)]
mod mock;

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod sdk_tests;
