//! Playing recordings back as an NDI source.

use std::{marker::PhantomData, path::Path, ptr::NonNull, rc::Rc, time::Duration};

use super::{
    out_param::OutParam, timeout_ms, types, FileApi, FileDuration, FileError, FilePosition,
    FileResult, PlayerEvent, PlayerOptions, PlayerStats, SourceFormat, SourceTimecode,
};
use crate::{
    sys::{self, bindings::*},
    Source,
};

/// An NDI source that plays recordings.
///
/// The source is advertised from creation until the player is dropped, whether
/// or not a file is playing; [`stop`](Self::stop) ends the current file only,
/// so receivers stay connected between files. Dropping the player stops
/// playback and removes the source.
///
/// A player belongs to the thread that created it. Every method calls into the
/// SDK and may block. [`AsyncFilePlayer`](super::AsyncFilePlayer) offers the
/// same operations as futures.
pub struct FilePlayer {
    playback: &'static sys::FilePlayback,
    handle: NonNull<NDIlib_file_player_instance_type>,
    // Strings passed to the SDK are needed only for the call that receives
    // them; see the note in `recorder.rs`. The name is kept for `Debug`.
    name: String,
    // Keeps the NDI runtime initialized.
    _api: FileApi,
    _thread: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for FilePlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePlayer")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl FilePlayer {
    pub(super) fn create(api: FileApi, options: &PlayerOptions) -> FileResult<Self> {
        let playback = api.playback()?;
        let settings = NDIlib_file_player_create_t {
            p_ndi_send_name: options.name.as_ptr(),
            p_groups: options
                .groups
                .as_ref()
                .map_or(std::ptr::null(), |groups| groups.as_ptr()),
        };
        // SAFETY: `settings` and the strings it points to are valid for the call.
        let handle = NonNull::new(unsafe { playback.NDIlib_file_player_create(&settings) })
            .ok_or(FileError::Rejected("player creation"))?;
        Ok(Self {
            playback,
            handle,
            name: options.name().to_owned(),
            _api: api,
            _thread: PhantomData,
        })
    }

    /// Play a file through the advertised source.
    ///
    /// A file already playing is closed first. Playback always begins unpaused
    /// and uses the player's looping setting. The first start of a
    /// GOP-compressed file may wait up to about half a second for a receiver
    /// that has just connected, so that it gets the opening keyframe.
    ///
    /// # Errors
    ///
    /// [`FileError::Rejected`] when the SDK cannot open the file, and
    /// [`FileError::InvalidInput`] when the path is empty, is not UTF-8 or
    /// contains a NUL.
    pub fn start(&mut self, path: impl AsRef<Path>) -> FileResult<()> {
        let path = types::path_string(path.as_ref())?;
        // SAFETY: the player is live and `path` is a NUL-terminated string
        // that outlives the call.
        let started = unsafe {
            self.playback
                .NDIlib_file_player_start(self.handle.as_ptr(), path.as_ptr())
        };
        if started {
            Ok(())
        } else {
            Err(FileError::Rejected("player start"))
        }
    }

    /// Stop the current file. The source stays advertised.
    pub fn stop(&mut self) {
        // SAFETY: the player is live for this borrow.
        unsafe { self.playback.NDIlib_file_player_stop(self.handle.as_ptr()) };
    }

    /// Whether a file is playing. False after [`stop`](Self::stop) and once
    /// playback reaches the end of a file that does not loop.
    pub fn is_running(&self) -> bool {
        // SAFETY: the player is live for this borrow.
        unsafe {
            self.playback
                .NDIlib_file_player_is_running(self.handle.as_ptr())
        }
    }

    /// Take the next queued event, waiting up to `wait` for one.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when `wait` is `u32::MAX` milliseconds or
    /// more, and [`FileError::Protocol`] when the SDK's event is malformed.
    pub fn poll_event(&mut self, wait: Duration) -> FileResult<Option<PlayerEvent>> {
        let wait = timeout_ms(wait)?;
        let mut event = OutParam::<NDIlib_file_player_event_t>::new();
        // SAFETY: the player is live and `event` is writable storage for the
        // structure the SDK fills in.
        let kind = unsafe {
            self.playback.NDIlib_file_player_capture_event(
                self.handle.as_ptr(),
                event.as_mut_ptr(),
                wait,
            )
        };
        let event = event.read()?;
        if kind == NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_none {
            return Ok(None);
        }
        if kind != event.type_ {
            return Err(FileError::Protocol(
                "the player returned one event type and stored another".into(),
            ));
        }
        Ok(Some(PlayerEvent::try_from(&event)?))
    }

    /// How many events the SDK has discarded since the player was created
    /// because they were not taken in time.
    pub fn dropped_event_count(&self) -> u64 {
        // SAFETY: the player is live for this borrow.
        unsafe {
            self.playback
                .NDIlib_file_player_get_dropped_event_count(self.handle.as_ptr())
        }
    }

    /// Counters for the current or most recent file, when the SDK has any.
    ///
    /// # Errors
    ///
    /// [`FileError::Protocol`] when the SDK's structure does not match this
    /// crate's layout.
    pub fn stats(&self) -> FileResult<Option<PlayerStats>> {
        let mut stats = OutParam::<NDIlib_file_player_stats_t>::new();
        // SAFETY: the player is live and `stats` is writable storage for the
        // structure the SDK fills in.
        let available = unsafe {
            self.playback
                .NDIlib_file_player_get_stats(self.handle.as_ptr(), stats.as_mut_ptr())
        };
        let stats = stats.read()?;
        Ok(available.then(|| PlayerStats::from(&stats)))
    }

    /// Seek to a position on the file's timeline, where zero is its first frame.
    ///
    /// GOP-compressed video resumes at the keyframe at or before the position;
    /// intra-only video resumes at the first frame at or after it.
    ///
    /// # Errors
    ///
    /// [`FileError::Rejected`] when the SDK refuses the seek.
    pub fn seek(&mut self, position: FilePosition) -> FileResult<()> {
        // SAFETY: the player is live for this borrow.
        let sought = unsafe {
            self.playback
                .NDIlib_file_player_seek_to_time(self.handle.as_ptr(), position.ticks())
        };
        if sought {
            Ok(())
        } else {
            Err(FileError::Rejected("time seek"))
        }
    }

    /// Seek to a frame index.
    ///
    /// The SDK converts the index to a time through the video frame rate, so
    /// the result is approximate for fractional rates.
    ///
    /// # Errors
    ///
    /// [`FileError::Rejected`] when the SDK refuses the seek.
    pub fn seek_frame_approximate(&mut self, frame: u64) -> FileResult<()> {
        // SAFETY: the player is live for this borrow.
        let sought = unsafe {
            self.playback
                .NDIlib_file_player_seek_to_frame(self.handle.as_ptr(), frame)
        };
        if sought {
            Ok(())
        } else {
            Err(FileError::Rejected("frame seek"))
        }
    }

    /// Pause or resume the current file. Has no effect when no file is open.
    pub fn set_paused(&mut self, paused: bool) {
        // SAFETY: the player is live for this borrow.
        unsafe {
            self.playback
                .NDIlib_file_player_set_paused(self.handle.as_ptr(), paused)
        };
    }

    /// Whether the current file is paused. Every [`start`](Self::start) resets
    /// this to false.
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
    pub fn set_looping(&mut self, looping: bool) {
        // SAFETY: the player is live for this borrow.
        unsafe {
            self.playback
                .NDIlib_file_player_set_loop(self.handle.as_ptr(), looping)
        };
    }

    /// Whether playback restarts at the end of a file.
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
    pub fn source_timecode(&self) -> Option<SourceTimecode> {
        let mut ticks = 0;
        // SAFETY: the player is live and the pointer is writable.
        unsafe {
            self.playback
                .NDIlib_file_player_get_current_source_timecode(self.handle.as_ptr(), &mut ticks)
        }
        .then_some(SourceTimecode::from_ticks(ticks))
    }

    /// The track layout of the current file, once one with audio or video is open.
    ///
    /// # Errors
    ///
    /// [`FileError::Protocol`] when the SDK's structure is malformed.
    pub fn source_format(&self) -> FileResult<Option<SourceFormat>> {
        let mut format = OutParam::<NDIlib_file_source_format_t>::new();
        // SAFETY: the player is live and `format` is writable storage for the
        // structure the SDK fills in.
        let known = unsafe {
            self.playback
                .NDIlib_file_player_get_source_format(self.handle.as_ptr(), format.as_mut_ptr())
        };
        let format = format.read()?;
        known.then(|| SourceFormat::try_from(&format)).transpose()
    }

    /// The NDI source this player advertises, for example to connect a
    /// [`Receiver`](crate::Receiver) to it.
    ///
    /// # Errors
    ///
    /// [`FileError::Core`] when the SDK returns no source or one whose name is
    /// not UTF-8.
    pub fn advertised_source(&self) -> FileResult<Source> {
        // SAFETY: the player is live. The descriptor the SDK returns, and its
        // strings, stay valid until the player is destroyed, which this borrow
        // rules out while they are copied.
        Ok(unsafe {
            Source::try_from_raw(
                self.playback
                    .NDIlib_file_player_get_advertised_source(self.handle.as_ptr()),
            )
        }?)
    }

    /// Stop playback and remove the advertised source.
    ///
    /// Equivalent to dropping the player; provided so the intent is visible at
    /// the call site.
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
