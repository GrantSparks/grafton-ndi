//! Playing recordings back as an NDI source.

use std::{marker::PhantomData, path::Path, ptr::NonNull, rc::Rc, time::Duration};

#[cfg(doc)]
use super::FileFamily;
use super::{
    out_param, types, FileApi, FileDuration, FileInfo, FilePlayerOptions, FilePosition,
    PlayerEvent, PlayerStats, SourceFormat,
};
use crate::{
    sys::{self, bindings::*},
    Error, Result, Source, Timecode,
};

/// An NDI source that plays recordings.
///
/// The source is advertised from creation until the player is dropped, whether
/// or not a file is playing; [`stop`](Self::stop) ends the current file only,
/// so receivers stay connected between files. Dropping the player stops
/// playback and removes the source.
///
/// A player belongs to the thread that created it, so it is neither [`Send`]
/// nor [`Sync`]: the File API documents no thread on which a player may be
/// used or destroyed, nor whether two calls on one player may overlap. Every
/// method calls into the SDK and may block.
/// [`AsyncFilePlayer`](super::AsyncFilePlayer) offers the same operations as
/// futures.
pub struct FilePlayer {
    playback: &'static sys::FilePlayback,
    handle: NonNull<NDIlib_file_player_instance_type>,
    // Strings passed to the SDK are needed only for the call that receives
    // them; see the note in `recorder.rs`. The name is kept for `Debug`.
    name: String,
    // Keeps the NDI runtime initialized.
    _api: FileApi,
    // Neither the NDI File Format API documentation nor
    // Processing.NDI.Lib.File.Player.h says on which threads a player may be
    // created, used and destroyed, or whether calls on one player may
    // overlap; their only mention of threads is the player's own playback
    // thread. Without that contract a player stays on its creating thread,
    // and AsyncFilePlayer creates, uses and destroys one on a thread of its
    // own.
    _thread: PhantomData<Rc<()>>,
}

/// Checked Rust data and its native family; no native player exists yet.
pub(super) struct PreparedPlayer {
    api: FileApi,
    playback: &'static sys::FilePlayback,
    resolved: types::ResolvedPlayerOptions,
    name: String,
}

impl PreparedPlayer {
    pub(super) fn create(self) -> Result<FilePlayer> {
        let Self {
            api,
            playback,
            resolved,
            name,
        } = self;
        let settings = NDIlib_file_player_create_t {
            p_ndi_send_name: resolved.name.as_ptr(),
            p_groups: resolved
                .groups
                .as_ref()
                .map_or(std::ptr::null(), |groups| groups.as_ptr()),
        };
        // SAFETY: `settings` and the strings it points to are valid for the call.
        let handle = unsafe { playback.named().NDIlib_file_player_create(&settings) }.created()?;
        Ok(FilePlayer {
            playback,
            handle,
            name,
            _api: api,
            _thread: PhantomData,
        })
    }
}

impl std::fmt::Debug for FilePlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePlayer")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl FilePlayer {
    /// Create a player and advertise its NDI source.
    ///
    /// The source is advertised from this call until the player is dropped,
    /// whether or not a file is playing. The options are only read, so they
    /// stay with the caller whatever the outcome.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the player's name is empty or a group
    /// is empty or contains a comma or NUL. [`Error::InvalidCString`] when
    /// the player's name contains a NUL.
    /// [`Error::NativeOperationFailed`] when the SDK cannot create the player
    /// and [`Error::UnsupportedCapability`] when the library does not provide
    /// [`FileFamily::Playback`].
    #[doc(alias = "NDIlib_file_player_create")]
    pub fn new(api: &FileApi, options: &FilePlayerOptions) -> Result<Self> {
        Self::prepare(api.clone(), options)?.create()
    }

    pub(super) fn prepare(api: FileApi, options: &FilePlayerOptions) -> Result<PreparedPlayer> {
        let playback = api.table::<sys::FilePlayback>()?;
        let resolved = options.resolve()?;
        Ok(PreparedPlayer {
            api,
            playback,
            resolved,
            name: options.name.clone(),
        })
    }

    /// Read a recording's duration and track layout.
    ///
    /// Nothing is played and no NDI source is advertised.
    /// An unreadable format payload does not discard the other information;
    /// [`FileInfo::source_format`] reports the typed cause instead.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK cannot read the file,
    /// [`Error::InvalidInput`] when the path is empty or is not UTF-8,
    /// [`Error::InvalidCString`] when it contains a NUL, [`Error::Io`] when a
    /// relative path cannot be resolved because the current directory is
    /// unavailable, and [`Error::UnsupportedCapability`] when the library
    /// does not provide [`FileFamily::Playback`].
    /// [`Error::UnexpectedSdkResult`] when the library's file information
    /// does not match this crate's structure layout.
    #[doc(alias = "NDIlib_file_player_probe_file")]
    pub fn probe(api: &FileApi, path: impl AsRef<Path>) -> Result<FileInfo> {
        let playback = api.table::<sys::FilePlayback>()?;
        let path = types::path_string(path.as_ref())?;
        let info = out_param::required_fill(|info| {
            // SAFETY: `path` is a NUL-terminated string that outlives the
            // call; the helper keeps guarded info storage writable through it.
            unsafe {
                playback
                    .named()
                    .NDIlib_file_player_probe_file(path.as_ptr(), info)
            }
        })?;
        Ok(FileInfo::from_sdk(&info))
    }

    /// Play a file through the advertised source.
    ///
    /// A file already playing is closed first. Playback always begins unpaused
    /// and uses the player's looping setting. As specified by
    /// `Processing.NDI.Lib.File.Player.h`, only the player's first start may
    /// wait up to about half a second, and only for a GOP-compressed file with
    /// a receiver that has just connected, so it gets the opening keyframe.
    /// There is no wait without a connected receiver, and later starts on the
    /// same player never wait.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK cannot open the file,
    /// [`Error::InvalidInput`] when the path is empty or is not UTF-8, and
    /// [`Error::InvalidCString`] when it contains a NUL.
    #[doc(alias = "NDIlib_file_player_start")]
    pub fn start(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = types::path_string(path.as_ref())?;
        // SAFETY: the player is live and `path` is a NUL-terminated string
        // that outlives the call.
        let started = unsafe {
            self.playback
                .named()
                .NDIlib_file_player_start(self.handle.as_ptr(), path.as_ptr())
        };
        started.performed()
    }

    /// Stop the current file. The source stays advertised.
    #[doc(alias = "NDIlib_file_player_stop")]
    pub fn stop(&mut self) {
        // SAFETY: the player is live for this borrow.
        unsafe { self.playback.NDIlib_file_player_stop(self.handle.as_ptr()) };
    }

    /// Whether a file is playing. False after [`stop`](Self::stop) and once
    /// playback reaches the end of a file that does not loop.
    #[doc(alias = "NDIlib_file_player_is_running")]
    pub fn is_running(&self) -> bool {
        // SAFETY: the player is live for this borrow.
        unsafe {
            self.playback
                .NDIlib_file_player_is_running(self.handle.as_ptr())
        }
    }

    /// Take the next queued event, waiting up to `timeout` for one, or
    /// `None` when none arrives in time.
    /// An unreadable format is withheld through [`PlayerEvent::source_format`]
    /// while the rest of the event is delivered.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`],
    /// and [`Error::UnexpectedSdkResult`] when the SDK's event does not match
    /// this crate's structure layout or its returned event type.
    #[doc(alias = "NDIlib_file_player_capture_event")]
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<PlayerEvent>> {
        let timeout = crate::to_ms_checked(timeout)?;
        out_param::capture_event(
            |event| {
                // SAFETY: the player is live and the helper keeps guarded
                // event storage writable through the call.
                unsafe {
                    self.playback.NDIlib_file_player_capture_event(
                        self.handle.as_ptr(),
                        event,
                        timeout,
                    )
                }
            },
            |event| event.type_,
            NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_none,
            "the player returned one event type and stored another",
        )
        .map(|event| event.map(|event| PlayerEvent::from_sdk(&event)))
    }

    /// How many events the SDK has discarded since the player was created
    /// because they were not taken in time.
    #[doc(alias = "NDIlib_file_player_get_dropped_event_count")]
    pub fn dropped_event_count(&self) -> u64 {
        // SAFETY: the player is live for this borrow.
        unsafe {
            self.playback
                .NDIlib_file_player_get_dropped_event_count(self.handle.as_ptr())
        }
    }

    /// Counters for the current or most recent file, when the SDK has any.
    /// Interpret them alongside playback session state and events, rather
    /// than as receiver-media progress.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] when the SDK's structure does not match this
    /// crate's layout.
    #[doc(alias = "NDIlib_file_player_get_stats")]
    pub fn stats(&self) -> Result<Option<PlayerStats>> {
        out_param::checked_fill(|stats| {
            // SAFETY: the player is live and the helper keeps guarded stats
            // storage writable through the call.
            unsafe {
                self.playback
                    .NDIlib_file_player_get_stats(self.handle.as_ptr(), stats)
            }
        })
        .map(|stats| stats.map(|stats| PlayerStats::from_sdk(&stats)))
    }

    /// Seek to a position on the file's timeline, where zero is its first frame.
    ///
    /// GOP-compressed video resumes at the keyframe at or before the position;
    /// intra-only video resumes at the first frame at or after it.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK refuses the seek.
    #[doc(alias = "NDIlib_file_player_seek_to_time")]
    pub fn seek(&mut self, position: FilePosition) -> Result<()> {
        // SAFETY: the player is live for this borrow.
        let sought = unsafe {
            self.playback
                .named()
                .NDIlib_file_player_seek_to_time(self.handle.as_ptr(), position.ticks())
        };
        sought.performed()
    }

    /// Seek to a frame index.
    ///
    /// The SDK converts the index to a time through the video frame rate, so
    /// the result is approximate for fractional rates.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK refuses the seek.
    #[doc(alias = "NDIlib_file_player_seek_to_frame")]
    pub fn seek_frame_approximate(&mut self, frame: u64) -> Result<()> {
        // SAFETY: the player is live for this borrow.
        let sought = unsafe {
            self.playback
                .named()
                .NDIlib_file_player_seek_to_frame(self.handle.as_ptr(), frame)
        };
        sought.performed()
    }

    /// Pause or resume the current file. Has no effect when no file is open.
    #[doc(alias = "NDIlib_file_player_set_paused")]
    pub fn set_paused(&mut self, paused: bool) {
        // SAFETY: the player is live for this borrow.
        unsafe {
            self.playback
                .NDIlib_file_player_set_paused(self.handle.as_ptr(), paused)
        };
    }

    /// Whether the current file is paused. Every [`start`](Self::start) resets
    /// this to false.
    #[doc(alias = "NDIlib_file_player_get_paused")]
    pub fn is_paused(&self) -> bool {
        // SAFETY: the player is live for this borrow.
        unsafe {
            self.playback
                .NDIlib_file_player_get_paused(self.handle.as_ptr())
        }
    }

    /// Set whether playback restarts at the end of a file.
    ///
    /// Looping belongs to the player, not the file: it is kept across
    /// [`start`](Self::start) and [`stop`](Self::stop). A looping file never
    /// raises [`PlayerEventKind::Ended`](super::PlayerEventKind::Ended).
    #[doc(alias = "NDIlib_file_player_set_loop")]
    pub fn set_looping(&mut self, looping: bool) {
        // SAFETY: the player is live for this borrow.
        unsafe {
            self.playback
                .NDIlib_file_player_set_loop(self.handle.as_ptr(), looping)
        };
    }

    /// Whether playback restarts at the end of a file.
    #[doc(alias = "NDIlib_file_player_get_loop")]
    pub fn is_looping(&self) -> bool {
        // SAFETY: the player is live for this borrow.
        unsafe {
            self.playback
                .NDIlib_file_player_get_loop(self.handle.as_ptr())
        }
    }

    /// The current file's best-known duration, and whether it is final.
    ///
    /// A file that is still being recorded reports a provisional duration.
    #[doc(alias = "NDIlib_file_player_get_current_duration")]
    pub fn duration(&self) -> Option<FileDuration> {
        let (mut ticks, mut is_final) = (0, false);
        // SAFETY: the player is live and both pointers are writable.
        unsafe {
            self.playback.NDIlib_file_player_get_current_duration(
                self.handle.as_ptr(),
                &mut ticks,
                &mut is_final,
            )
        }
        .then_some(FileDuration {
            position: FilePosition::from_ticks(ticks),
            is_final,
        })
    }

    /// The position of the frame sent last: the main video track's, or the
    /// audio track's in a file without video.
    #[doc(alias = "NDIlib_file_player_get_current_position")]
    pub fn position(&self) -> Option<FilePosition> {
        let mut ticks = 0;
        // SAFETY: the player is live and the pointer is writable.
        unsafe {
            self.playback
                .NDIlib_file_player_get_current_position(self.handle.as_ptr(), &mut ticks)
        }
        .then_some(FilePosition::from_ticks(ticks))
    }

    /// The recorded source timecode of the frame sent last. Unlike the
    /// position it is editorial and need not increase.
    #[doc(alias = "NDIlib_file_player_get_current_source_timecode")]
    pub fn source_timecode(&self) -> Option<Timecode> {
        let mut ticks = 0;
        // SAFETY: the player is live and the pointer is writable.
        unsafe {
            self.playback
                .NDIlib_file_player_get_current_source_timecode(self.handle.as_ptr(), &mut ticks)
        }
        .then_some(Timecode::from_ticks(ticks))
    }

    /// The track layout of the current file, once one with audio or video is open.
    /// A known layout alone does not establish current playback progress;
    /// inspect the player state and events as well.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] when the SDK's structure is malformed.
    #[doc(alias = "NDIlib_file_player_get_source_format")]
    pub fn source_format(&self) -> Result<Option<SourceFormat>> {
        out_param::checked_fill(|format| {
            // SAFETY: the player is live and the helper keeps guarded format
            // storage writable through the call.
            unsafe {
                self.playback
                    .NDIlib_file_player_get_source_format(self.handle.as_ptr(), format)
            }
        })?
        .map(|format| SourceFormat::from_sdk(&format).map_err(Error::UnexpectedSdkResult))
        .transpose()
    }

    /// The NDI source this player advertises, for example to connect a
    /// [`Receiver`](crate::Receiver) to it.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK returns no source, or one
    /// that names neither a source nor an address, and
    /// [`Error::UnexpectedSdkResult`] when its name or address is longer than
    /// the 4 MiB one SDK string may be.
    #[doc(alias = "NDIlib_file_player_get_advertised_source")]
    pub fn advertised_source(&self) -> Result<Source> {
        // SAFETY: the player is live. The descriptor the SDK returns, and its
        // strings, stay valid until the player is destroyed, which this borrow
        // rules out while they are copied.
        unsafe {
            Source::try_from_raw(
                self.playback
                    .named()
                    .NDIlib_file_player_get_advertised_source(self.handle.as_ptr()),
            )
        }
    }

    /// Stop playback and remove the advertised source.
    ///
    /// Equivalent to dropping the player; provided so the intent is visible at
    /// the call site.
    #[doc(alias = "NDIlib_file_player_destroy")]
    pub fn close(self) {
        drop(self);
    }
}

impl Drop for FilePlayer {
    fn drop(&mut self) {
        // SAFETY: the player is live and is destroyed exactly once, on the
        // thread that created it.
        unsafe {
            self.playback.NDIlib_file_player_stop(self.handle.as_ptr());
            self.playback
                .NDIlib_file_player_destroy(self.handle.as_ptr());
        }
    }
}
