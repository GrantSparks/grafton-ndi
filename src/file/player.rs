use super::{
    ffi, timeout_ms, types, FileApi, FileDuration, FileError, FilePosition, FileResult,
    PlayerEvent, PlayerOptions, PlayerStats, SourceFormat, SourceSpec, SourceTimecode,
};
use std::{
    ffi::{c_void, CStr, CString},
    marker::PhantomData,
    path::Path,
    ptr::NonNull,
    rc::Rc,
    time::Duration,
};

/// An advertised playback source. Native calls and destruction stay on this thread.
///
/// Dropping this value stops playback and removes its advertised source. Calling
/// [`stop`](Self::stop) alone keeps the advertised source alive between files.
pub struct FilePlayer {
    api: FileApi,
    handle: NonNull<c_void>,
    _options: PlayerOptions,
    _raw_options: Box<ffi::PlayerOptions>,
    // The input-copy contract is not specified. Keep all submitted paths until
    // native destruction, including rejected starts, rather than guess a lifetime.
    paths: Vec<CString>,
    _thread: PhantomData<Rc<()>>,
}
impl std::fmt::Debug for FilePlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePlayer")
            .field("name", &self._options.name())
            .finish_non_exhaustive()
    }
}
impl FilePlayer {
    pub(super) fn create(api: FileApi, options: PlayerOptions) -> FileResult<Self> {
        let raw = Box::new(ffi::PlayerOptions {
            p_ndi_send_name: options.name.as_ptr(),
            p_groups: options
                .groups
                .as_ref()
                .map_or(std::ptr::null(), |s| s.as_ptr()),
        });
        let handle = NonNull::new(unsafe { (api.backend.functions.player_create)(&*raw) })
            .ok_or(FileError::Rejected("player creation"))?;
        Ok(Self {
            api,
            handle,
            _options: options,
            _raw_options: raw,
            paths: Vec::new(),
            _thread: PhantomData,
        })
    }

    /// Start a file, replacing any prior file and clearing pause. Loop setting persists.
    ///
    /// The first GOP-compressed start may wait briefly for receiver registration.
    pub fn start(&mut self, path: impl AsRef<Path>) -> FileResult<()> {
        let path = types::path_string(path.as_ref())?;
        let pointer = path.as_ptr();
        self.paths.push(path);
        if unsafe { (self.api.backend.functions.player_start)(self.handle.as_ptr(), pointer) } {
            Ok(())
        } else {
            Err(FileError::Rejected("player start"))
        }
    }

    /// Stop the current file while retaining the advertised sender.
    pub fn stop(&mut self) {
        unsafe { (self.api.backend.functions.player_stop)(self.handle.as_ptr()) };
    }

    /// Whether the native playback session is active.
    pub fn is_running(&self) -> bool {
        unsafe { (self.api.backend.functions.player_running)(self.handle.as_ptr()) }
    }

    /// Capture an owned event; timeout is a finite native wait budget.
    pub fn poll_event(&mut self, wait: Duration) -> FileResult<Option<PlayerEvent>> {
        let wait = timeout_ms(wait)?;
        let mut raw = ffi::PlayerEvent::default();
        let kind = unsafe {
            (self.api.backend.functions.player_capture)(self.handle.as_ptr(), &mut raw, wait)
        };
        if kind == 0 {
            return Ok(None);
        }
        if kind != raw.kind {
            return Err(FileError::Protocol(
                "player event discriminants disagree".into(),
            ));
        }
        Ok(Some(PlayerEvent::try_from(&raw)?))
    }

    /// Number of events lost by the native queue over this instance's lifetime.
    pub fn dropped_event_count(&self) -> u64 {
        unsafe { (self.api.backend.functions.player_dropped)(self.handle.as_ptr()) }
    }

    /// Current or most recent file telemetry, if available.
    pub fn stats(&self) -> Option<PlayerStats> {
        let mut raw = ffi::PlayerStats::default();
        unsafe { (self.api.backend.functions.player_stats)(self.handle.as_ptr(), &mut raw) }
            .then(|| PlayerStats::from(&raw))
    }

    /// Seek relative to the file timeline.
    ///
    /// GOP codecs resume at an earlier/equal keyframe. Intra-only content resumes
    /// at the first frame at/after the requested position; this is not exact seeking.
    pub fn seek(&mut self, position: FilePosition) -> FileResult<()> {
        if unsafe {
            (self.api.backend.functions.player_seek_time)(self.handle.as_ptr(), position.ticks())
        } {
            Ok(())
        } else {
            Err(FileError::Rejected("time seek"))
        }
    }

    /// Seek through a frame-rate conversion. Fractional rates make this approximate.
    pub fn seek_frame_approximate(&mut self, frame: u64) -> FileResult<()> {
        if unsafe { (self.api.backend.functions.player_seek_frame)(self.handle.as_ptr(), frame) } {
            Ok(())
        } else {
            Err(FileError::Rejected("frame seek"))
        }
    }

    /// Pause/resume the current file; has no effect with no file open.
    pub fn set_paused(&mut self, paused: bool) {
        unsafe { (self.api.backend.functions.player_set_paused)(self.handle.as_ptr(), paused) };
    }
    /// Current-file pause state. A new start resets it to false.
    pub fn is_paused(&self) -> bool {
        unsafe { (self.api.backend.functions.player_get_paused)(self.handle.as_ptr()) }
    }
    /// Change instance looping, retained across start/stop. Looping suppresses ended.
    pub fn set_looping(&mut self, looping: bool) {
        unsafe { (self.api.backend.functions.player_set_loop)(self.handle.as_ptr(), looping) };
    }
    /// The persistent instance looping setting.
    pub fn is_looping(&self) -> bool {
        unsafe { (self.api.backend.functions.player_get_loop)(self.handle.as_ptr()) }
    }
    /// Current duration and whether it is final. Growing files can report provisional values.
    pub fn duration(&self) -> Option<FileDuration> {
        let (mut ticks, mut is_final) = (0, false);
        unsafe {
            (self.api.backend.functions.player_duration)(
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
    /// Position of the main video frame last sent (audio for an audio-only file).
    pub fn position(&self) -> Option<FilePosition> {
        let mut ticks = 0;
        unsafe { (self.api.backend.functions.player_position)(self.handle.as_ptr(), &mut ticks) }
            .then_some(FilePosition::from_ticks(ticks))
    }
    /// Preserved signed source timecode, distinct from file-relative position.
    pub fn source_timecode(&self) -> Option<SourceTimecode> {
        let mut ticks = 0;
        unsafe { (self.api.backend.functions.player_timecode)(self.handle.as_ptr(), &mut ticks) }
            .then_some(SourceTimecode::from_ticks(ticks))
    }
    /// Owned current track format, once a file containing audio/video is open.
    pub fn source_format(&self) -> FileResult<Option<SourceFormat>> {
        let mut raw = ffi::SourceFormat::default();
        if unsafe { (self.api.backend.functions.player_format)(self.handle.as_ptr(), &mut raw) } {
            Ok(Some(SourceFormat::try_from(&raw)?))
        } else {
            Ok(None)
        }
    }
    /// Copy this player's advertised source. Its native strings live until destroy.
    pub fn advertised_source(&self) -> FileResult<SourceSpec> {
        let raw = unsafe { (self.api.backend.functions.player_source)(self.handle.as_ptr()) };
        let raw = unsafe { raw.as_ref() }
            .ok_or_else(|| FileError::Protocol("player returned no advertised source".into()))?;
        // The SDK explicitly keeps these NUL-terminated strings valid until this
        // owner is destroyed; this thread-affine borrow excludes destruction.
        let copy = |pointer: *const std::ffi::c_char| -> FileResult<Option<CString>> {
            if pointer.is_null() {
                return Ok(None);
            }
            let text = unsafe { CStr::from_ptr(pointer) };
            text.to_str()
                .map_err(|error| FileError::Protocol(error.to_string()))?;
            Ok(Some(text.to_owned()))
        };
        Ok(SourceSpec {
            name: copy(raw.p_ndi_name)?,
            url: copy(raw.p_url_address)?,
        })
    }
    /// Stop playback and release the native instance on the current thread.
    pub fn close(self) {
        drop(self);
    }
}
impl Drop for FilePlayer {
    fn drop(&mut self) {
        unsafe {
            (self.api.backend.functions.player_stop)(self.handle.as_ptr());
            (self.api.backend.functions.player_destroy)(self.handle.as_ptr());
        }
    }
}
