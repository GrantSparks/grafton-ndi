//! Owned configuration, event and format values for the File API.

use std::{
    borrow::Cow,
    ffi::{c_char, CString},
    ops::BitOr,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::sys::bindings::*;

/// Errors from File API configuration, loading or native operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FileError {
    /// The File library, or the part of it an operation needs, cannot be used
    /// in this process. The text says why: no library was found (with every
    /// location tried), the selected NDI runtime is not one the File library
    /// can accompany, or the library lacks an entry point.
    #[error("File API unavailable: {0}")]
    Unavailable(String),
    /// A value cannot be represented by the native API.
    #[error("Invalid File API input: {0}")]
    InvalidInput(String),
    /// The operation is not permitted in the current session state.
    #[error("Invalid File API state: {0}")]
    InvalidState(&'static str),
    /// A native boolean operation was rejected; events may carry more detail.
    #[error("The File SDK rejected {0}")]
    Rejected(&'static str),
    /// A native result violates the expected representation/contract.
    #[error("Invalid File SDK result: {0}")]
    Protocol(String),
    /// A core NDI operation failed.
    #[error(transparent)]
    Core(#[from] crate::Error),
    /// Local path resolution or another filesystem operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result type for File API operations.
pub type FileResult<T> = std::result::Result<T, FileError>;

fn cstring(value: &str, what: &str, allow_empty: bool) -> FileResult<CString> {
    if !allow_empty && value.is_empty() {
        return Err(FileError::InvalidInput(format!("{what} must not be empty")));
    }
    CString::new(value)
        .map_err(|_| FileError::InvalidInput(format!("{what} contains an embedded NUL")))
}

/// An owned source name or direct URL used by a recorder.
///
/// Name and URL constructors are separate because the SDK gives a nonempty name
/// precedence over a URL. No discovery occurs while constructing this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpec {
    pub(super) name: Option<CString>,
    pub(super) url: Option<CString>,
}

impl SourceSpec {
    /// Select a source by its NDI name, for example `"MACHINE (Camera 1)"`.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when the name is empty or contains a NUL.
    pub fn new(name: impl AsRef<str>) -> FileResult<Self> {
        Ok(Self {
            name: Some(cstring(name.as_ref(), "source name", false)?),
            url: None,
        })
    }

    /// Select a source by its address, for example `"192.168.1.20:5961"`,
    /// without discovering it by name.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when the address is empty or contains a NUL.
    pub fn from_url(url: impl AsRef<str>) -> FileResult<Self> {
        Ok(Self {
            name: None,
            url: Some(cstring(url.as_ref(), "source URL", false)?),
        })
    }

    /// The requested display name, when selecting by name.
    pub fn name(&self) -> Option<&str> {
        self.name
            .as_ref()
            .map(|s| s.to_str().expect("validated UTF-8"))
    }

    /// The requested URL/address, when selecting directly.
    pub fn url(&self) -> Option<&str> {
        self.url
            .as_ref()
            .map(|s| s.to_str().expect("validated UTF-8"))
    }
}

impl TryFrom<&crate::Source> for SourceSpec {
    type Error = FileError;

    fn try_from(source: &crate::Source) -> FileResult<Self> {
        if !source.name.is_empty() {
            Self::new(&source.name)
        } else {
            match &source.address {
                crate::SourceAddress::Ip(address) | crate::SourceAddress::Url(address) => {
                    Self::from_url(address)
                }
                crate::SourceAddress::None => Err(FileError::InvalidInput(
                    "source has neither a name nor an address".into(),
                )),
            }
        }
    }
}

/// Private-field player configuration. Creating this value advertises nothing.
#[derive(Debug, Clone)]
pub struct PlayerOptions {
    pub(super) name: CString,
    pub(super) groups: Option<CString>,
}

impl PlayerOptions {
    /// Choose the name of the NDI source the player advertises.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when the name is empty or contains a NUL.
    pub fn new(name: impl AsRef<str>) -> FileResult<Self> {
        Ok(Self {
            name: cstring(name.as_ref(), "player name", false)?,
            groups: None,
        })
    }

    /// Set the comma-separated NDI groups the player's source is advertised in.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when the text contains a NUL.
    pub fn with_groups(mut self, groups: impl AsRef<str>) -> FileResult<Self> {
        self.groups = Some(cstring(groups.as_ref(), "player groups", true)?);
        Ok(self)
    }

    /// The configured name.
    pub fn name(&self) -> &str {
        self.name.to_str().expect("validated UTF-8")
    }

    /// The configured group string, if supplied.
    pub fn groups(&self) -> Option<&str> {
        self.groups
            .as_ref()
            .map(|s| s.to_str().expect("validated UTF-8"))
    }
}

/// A nonempty selection of recording tracks.
///
/// Unlike the C API, an empty bitmask is rejected rather than meaning all tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackSelection(u32);

impl TrackSelection {
    /// Record video.
    pub const VIDEO: Self =
        Self(NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_video);
    /// Record audio.
    pub const AUDIO: Self =
        Self(NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_audio);
    /// Record metadata.
    pub const METADATA: Self =
        Self(NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_metadata);
    /// Record all three track types.
    pub const ALL: Self =
        Self(NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_all);

    /// A selection from the SDK's track flags.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when no flag is set or an unknown one is.
    pub fn from_bits(bits: u32) -> FileResult<Self> {
        if bits == 0 || bits & !Self::ALL.0 != 0 {
            Err(FileError::InvalidInput(
                "track selection must contain one or more known track flags".into(),
            ))
        } else {
            Ok(Self(bits))
        }
    }

    /// Native selection bits.
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl Default for TrackSelection {
    fn default() -> Self {
        Self::ALL
    }
}

impl BitOr for TrackSelection {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Recorder configuration fixed when its native instance is created.
#[derive(Debug, Clone)]
pub struct RecorderOptions {
    pub(super) source: SourceSpec,
    pub(super) tracks: TrackSelection,
    pub(super) planar_audio: bool,
    pub(super) disable_auto_rollover: bool,
    pub(super) pattern: Option<CString>,
    pub(super) disable_proxy: bool,
    pub(super) rollover_bytes: u64,
    pub(super) drop_wait: Duration,
}

impl RecorderOptions {
    /// Record all tracks, including a proxy when available, using indexed names.
    pub fn new(source: SourceSpec) -> Self {
        Self {
            source,
            tracks: TrackSelection::ALL,
            planar_audio: false,
            disable_auto_rollover: false,
            pattern: None,
            disable_proxy: false,
            rollover_bytes: 0,
            drop_wait: Self::DEFAULT_DROP_WAIT,
        }
    }

    /// How long dropping a recorder waits for its open file to be finalized,
    /// unless [`with_drop_wait`](Self::with_drop_wait) says otherwise.
    pub const DEFAULT_DROP_WAIT: Duration = Duration::from_secs(5);

    /// Set how long dropping the recorder waits for an unfinished session.
    ///
    /// The SDK finalizes a file between the stop request and its file-closed
    /// event; destroying the recorder sooner may leave a truncated file. A
    /// recorder dropped with a session still open therefore requests the stop
    /// and waits up to `wait` for that event before it is destroyed. The wait
    /// ends as soon as the file is closed, so the limit only matters when the
    /// SDK does not report closure. `Duration::ZERO` destroys at once, like
    /// [`FileRecorder::abandon`](super::FileRecorder::abandon).
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when `wait` is `u32::MAX` milliseconds or more.
    pub fn with_drop_wait(mut self, wait: Duration) -> FileResult<Self> {
        super::timeout_ms(wait)?;
        self.drop_wait = wait;
        Ok(self)
    }

    /// Select which track types to record.
    pub fn with_tracks(mut self, tracks: TrackSelection) -> Self {
        self.tracks = tracks;
        self
    }

    /// Store uncompressed float audio as native planar data when true.
    ///
    /// The default interleaved layout is more widely playable by third parties.
    pub fn with_planar_audio(mut self, planar: bool) -> Self {
        self.planar_audio = planar;
        self
    }

    /// Whether source format changes automatically roll into a new file.
    pub fn with_format_change_rollover(mut self, enabled: bool) -> Self {
        self.disable_auto_rollover = !enabled;
        self
    }

    /// Whether to record the source's proxy alongside its full-quality video.
    pub fn with_proxy(mut self, enabled: bool) -> Self {
        self.disable_proxy = !enabled;
        self
    }

    /// Set a soft size threshold for keyframe-aligned rollover; zero disables it.
    pub fn with_rollover_size(mut self, bytes: u64) -> Self {
        self.rollover_bytes = bytes;
        self
    }

    /// Use the supplied recording target as an exact filename.
    ///
    /// Native creation fails if that file already exists; no overwrite is implied.
    pub fn exact_filename(mut self) -> Self {
        self.pattern = Some(CString::new("{prefix}").expect("literal has no NUL"));
        self
    }

    /// Restore the SDK's indexed `{prefix}_{index:03}.mkv` naming policy.
    pub fn indexed_filenames(mut self) -> Self {
        self.pattern = None;
        self
    }

    /// Set a filename pattern using `{prefix}`, `{index}` and `{index:N}` tokens.
    ///
    /// `{index:N}` pads the index with zeros to `N` digits, from 1 through 1024.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when the pattern is empty, has an unknown
    /// token or unbalanced braces, or contains a NUL.
    pub fn with_pattern(mut self, pattern: impl AsRef<str>) -> FileResult<Self> {
        validate_pattern(pattern.as_ref())?;
        self.pattern = Some(cstring(pattern.as_ref(), "filename pattern", false)?);
        Ok(self)
    }

    /// The configured initial source.
    pub fn source(&self) -> &SourceSpec {
        &self.source
    }

    /// The configured track selection.
    pub fn tracks(&self) -> TrackSelection {
        self.tracks
    }

    /// The configured filename pattern, or None for the SDK indexed default.
    pub fn filename_pattern(&self) -> Option<&str> {
        self.pattern
            .as_ref()
            .map(|s| s.to_str().expect("validated UTF-8"))
    }

    /// How long dropping the recorder waits for an unfinished session.
    pub fn drop_wait(&self) -> Duration {
        self.drop_wait
    }
}

fn validate_pattern(pattern: &str) -> FileResult<()> {
    let mut rest = pattern;
    while let Some(start) = rest.find(['{', '}']) {
        if rest.as_bytes()[start] == b'}' {
            return Err(FileError::InvalidInput(
                "unmatched filename-pattern brace".into(),
            ));
        }
        let after = &rest[start + 1..];
        let end = after
            .find('}')
            .ok_or_else(|| FileError::InvalidInput("unmatched filename-pattern brace".into()))?;
        let token = &after[..end];
        let valid = match token {
            "prefix" | "index" => true,
            _ => token.strip_prefix("index:").is_some_and(|width| {
                !width.is_empty()
                    && width.bytes().all(|b| b.is_ascii_digit())
                    && width.parse::<u16>().is_ok_and(|n| (1..=1024).contains(&n))
            }),
        };
        if !valid {
            return Err(FileError::InvalidInput(format!(
                "unsupported filename-pattern token {{{token}}}"
            )));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

/// An owned absolute output prefix/path interpreted by the recorder's naming policy.
///
/// Construction freezes the current directory for relative paths. It neither
/// creates directories nor creates/removes/overwrites a file. The SDK requires
/// losslessly representable UTF-8 paths.
#[derive(Debug, Clone)]
pub struct RecordingTarget {
    pub(super) prefix: CString,
    path: PathBuf,
}

impl RecordingTarget {
    /// Take an output prefix or exact path, making a relative one absolute.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when the path is empty, is not UTF-8 or
    /// contains a NUL, and [`FileError::Io`] when a relative path cannot be
    /// resolved because the current directory is unavailable.
    pub fn new(path: impl AsRef<Path>) -> FileResult<Self> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(FileError::InvalidInput(
                "recording target must not be empty".into(),
            ));
        }
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        let text = absolute.to_str().ok_or_else(|| {
            FileError::InvalidInput("recording target is not lossless UTF-8".into())
        })?;
        Ok(Self {
            prefix: cstring(text, "recording target", false)?,
            path: absolute,
        })
    }

    /// The frozen absolute path/prefix, without filesystem canonicalization.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub(super) fn path_string(path: &Path) -> FileResult<CString> {
    Ok(RecordingTarget::new(path)?.prefix)
}

/// Original SDK event bytes, retaining invalid UTF-8 and truncation evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventText {
    bytes: Vec<u8>,
    terminated: bool,
    possibly_truncated: bool,
}

impl Default for EventText {
    fn default() -> Self {
        Self {
            bytes: Vec::new(),
            terminated: true,
            possibly_truncated: false,
        }
    }
}

impl EventText {
    pub(super) fn from_buffer(buffer: &[c_char]) -> Self {
        let end = buffer.iter().position(|&b| b == 0);
        let used = end.unwrap_or(buffer.len());
        Self {
            bytes: buffer[..used].iter().map(|&b| b.to_ne_bytes()[0]).collect(),
            terminated: end.is_some(),
            possibly_truncated: !buffer.is_empty() && used >= buffer.len() - 1,
        }
    }

    /// Original bytes up to a bounded NUL search, excluding the terminator.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Strict text decoding; failure does not discard the original event bytes.
    pub fn to_str(&self) -> std::result::Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.bytes)
    }

    /// Text suitable for diagnostics, replacing malformed UTF-8 explicitly.
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }

    /// Whether the native fixed-size buffer contained a NUL terminator.
    pub fn is_terminated(&self) -> bool {
        self.terminated
    }

    /// Whether the string filled the available payload and might be truncated.
    pub fn is_possibly_truncated(&self) -> bool {
        self.possibly_truncated
    }
}

/// A raw codec/pixel identifier in a file's track description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FourCc(pub(super) u32);

impl FourCc {
    /// Preserve a raw SDK FourCC, including unknown future values.
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    /// The unchanged SDK identifier.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Unsigned file-relative time in 100ns units, distinct from editorial timecode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct FilePosition(pub(super) u64);

impl FilePosition {
    /// Preserve native 100ns ticks.
    pub const fn from_ticks(ticks: u64) -> Self {
        Self(ticks)
    }

    /// The position `duration` after the start of the file, rounded down to a
    /// 100 ns tick.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidInput`] when the duration exceeds 64 bits of ticks.
    pub fn from_duration(duration: Duration) -> FileResult<Self> {
        let ticks = duration.as_nanos() / 100;
        u64::try_from(ticks)
            .map(Self)
            .map_err(|_| FileError::InvalidInput("file position exceeds 64-bit ticks".into()))
    }

    /// Native 100ns ticks.
    pub const fn ticks(self) -> u64 {
        self.0
    }

    /// Exact duration represented by these ticks.
    pub fn duration(self) -> Duration {
        Duration::new(self.0 / 10_000_000, ((self.0 % 10_000_000) * 100) as u32)
    }
}

/// Signed editorial source timecode, which need not be monotonic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceTimecode(pub(super) i64);

impl SourceTimecode {
    /// Preserve a native signed timecode in 100ns units.
    pub const fn from_ticks(ticks: i64) -> Self {
        Self(ticks)
    }

    /// Native signed 100ns ticks.
    pub const fn ticks(self) -> i64 {
        self.0
    }
}

/// Timecode policy applied when a recording session starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RecordingTimecode {
    /// Preserve the source's original editorial timecode.
    #[default]
    Preserve,
    /// Generate session timecode starting at this value.
    GenerateFrom(SourceTimecode),
}

/// A current duration that may still be growing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileDuration {
    pub(super) position: FilePosition,
    pub(super) is_final: bool,
}

impl FileDuration {
    /// The duration in 100ns units.
    pub fn value(self) -> FilePosition {
        self.position
    }

    /// Whether the SDK says this duration is finalized.
    pub fn is_final(self) -> bool {
        self.is_final
    }
}

/// Scan information recorded in a track header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileScanType {
    /// Interleaved fields, or the zero default in files lacking scan metadata.
    Interleaved,
    /// Progressive full frames.
    Progressive,
    /// First field.
    Field0,
    /// Second field.
    Field1,
    /// A future raw scan identifier.
    Unknown(u32),
}

impl FileScanType {
    pub(super) fn from_raw(value: u32) -> Self {
        match value {
            NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_interleaved => {
                Self::Interleaved
            }
            NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_progressive => {
                Self::Progressive
            }
            NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_field_0 => Self::Field0,
            NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_field_1 => Self::Field1,
            value => Self::Unknown(value),
        }
    }
}

/// An owned description of one video track, not a decoded pixel layout.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoTrack {
    pub(super) high_quality: bool,
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) frame_rate_n: i32,
    pub(super) frame_rate_d: i32,
    pub(super) fourcc: FourCc,
    pub(super) scan_type: FileScanType,
    pub(super) aspect_ratio: f32,
}

impl VideoTrack {
    /// Whether this is the primary full-bandwidth representation.
    pub fn is_high_quality(&self) -> bool {
        self.high_quality
    }
    /// Raw resolution. Zero values mean not yet known.
    pub fn resolution(&self) -> (i32, i32) {
        (self.width, self.height)
    }
    /// Raw rational frame rate; a zero denominator remains unknown.
    pub fn frame_rate(&self) -> (i32, i32) {
        (self.frame_rate_n, self.frame_rate_d)
    }
    /// Codec/packing identifier, including unknown future values.
    pub fn fourcc(&self) -> FourCc {
        self.fourcc
    }
    /// Recorded scan information; older files can retain the default value.
    pub fn scan_type(&self) -> FileScanType {
        self.scan_type
    }
    /// Display aspect ratio when positive and finite; zero/invalid means unknown.
    pub fn aspect_ratio(&self) -> Option<f32> {
        (self.aspect_ratio.is_finite() && self.aspect_ratio > 0.0).then_some(self.aspect_ratio)
    }
}

/// An owned layout snapshot containing no borrowed native memory.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SourceFormat {
    pub(super) video_tracks: Vec<VideoTrack>,
    pub(super) audio_sample_rate: i32,
    pub(super) audio_channels: i32,
    pub(super) audio_samples: i32,
    pub(super) audio_bit_depth: u32,
    pub(super) audio_fourcc: FourCc,
}

impl SourceFormat {
    /// Populated video tracks, at most HQ plus proxy.
    pub fn video_tracks(&self) -> &[VideoTrack] {
        &self.video_tracks
    }
    /// Raw sample rate, possibly zero until observed.
    pub fn audio_sample_rate(&self) -> i32 {
        self.audio_sample_rate
    }
    /// Raw channel count, possibly zero until observed.
    pub fn audio_channels(&self) -> i32 {
        self.audio_channels
    }
    /// Raw samples per packet/frame when known.
    pub fn audio_samples(&self) -> i32 {
        self.audio_samples
    }
    /// Audio bit depth as reported by the SDK.
    pub fn audio_bit_depth(&self) -> u32 {
        self.audio_bit_depth
    }
    /// Audio codec/packing identifier.
    pub fn audio_fourcc(&self) -> FourCc {
        self.audio_fourcc
    }
}

/// A non-advertising file probe result.
#[derive(Debug, Clone, PartialEq)]
pub struct FileInfo {
    pub(super) duration: FileDuration,
    pub(super) track_count: u32,
    pub(super) video_track_count: u32,
    pub(super) audio_track_count: u32,
    pub(super) metadata_track_count: u32,
    pub(super) source_format: SourceFormat,
}

impl FileInfo {
    /// Best-known duration and its finalized status.
    pub fn duration(&self) -> FileDuration {
        self.duration
    }
    /// Total reported track count.
    pub fn track_count(&self) -> u32 {
        self.track_count
    }
    /// Reported video track count.
    pub fn video_track_count(&self) -> u32 {
        self.video_track_count
    }
    /// Reported audio track count.
    pub fn audio_track_count(&self) -> u32 {
        self.audio_track_count
    }
    /// Reported metadata track count.
    pub fn metadata_track_count(&self) -> u32 {
        self.metadata_track_count
    }
    /// Owned codec/layout information.
    pub fn source_format(&self) -> &SourceFormat {
        &self.source_format
    }
}

/// Playback event kind; unknown native values remain observable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlayerEventKind {
    /// Natural end of file.
    Ended,
    /// Unrecoverable playback error.
    Error,
    /// File was opened.
    FileOpened,
    /// First displayed packet after start, seek or loop.
    FirstFrame,
    /// A playback session closed.
    FileClosed,
    /// Playback looped to its beginning.
    Looped,
    /// An unrecognized native event identifier.
    Unknown(u32),
}

impl PlayerEventKind {
    pub(super) fn from_raw(value: u32) -> Self {
        match value {
            NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_ended => Self::Ended,
            NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_error => Self::Error,
            NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_file_opened => {
                Self::FileOpened
            }
            NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_first_frame => {
                Self::FirstFrame
            }
            NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_file_closed => {
                Self::FileClosed
            }
            NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_looped => Self::Looped,
            value => Self::Unknown(value),
        }
    }
}

/// An owned player event. Inline SDK strings are copied within their bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerEvent {
    pub(super) kind: PlayerEventKind,
    pub(super) path: EventText,
    pub(super) message: EventText,
    pub(super) first_pts: FilePosition,
    pub(super) last_pts: FilePosition,
    pub(super) source_timecode: Option<SourceTimecode>,
    pub(super) source_format: SourceFormat,
}

impl PlayerEvent {
    /// Event kind.
    pub fn kind(&self) -> PlayerEventKind {
        self.kind
    }
    /// Reported path, potentially truncated.
    pub fn path(&self) -> &EventText {
        &self.path
    }
    /// Diagnostic/causality text.
    pub fn message(&self) -> &EventText {
        &self.message
    }
    /// First packet timestamp where applicable; otherwise native zero.
    pub fn first_pts(&self) -> FilePosition {
        self.first_pts
    }
    /// Last packet timestamp where applicable; otherwise native zero.
    pub fn last_pts(&self) -> FilePosition {
        self.last_pts
    }
    /// Editorial source timecode when the SDK marked it known.
    pub fn source_timecode(&self) -> Option<SourceTimecode> {
        self.source_timecode
    }
    /// Source format snapshot; zero/empty fields may mean inapplicable.
    pub fn source_format(&self) -> &SourceFormat {
        &self.source_format
    }
}

/// Recorder event kind; a write failure is not necessarily fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecorderEventKind {
    /// First output file opened.
    Started,
    /// A new rollover file opened.
    Rollover,
    /// Output open failed.
    OpenFailed,
    /// Packet write failure, either fatal I/O or nonfatal missing track.
    WriteFailed,
    /// Unrecoverable writer error.
    Error,
    /// Source format changed.
    FormatChanged,
    /// First audio/video packet was written; absent for metadata-only recording.
    FirstFrame,
    /// Source disconnected while recording remains active.
    SourceLost,
    /// Source resumed delivering packets.
    SourceConnected,
    /// Current file finalized and closed.
    FileClosed,
    /// Expected proxy was unavailable.
    ProxyMissing,
    /// An unrecognized native event identifier.
    Unknown(u32),
}

impl RecorderEventKind {
    pub(super) fn from_raw(value: u32) -> Self {
        match value {
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_started => {
                Self::Started
            }
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_rollover => {
                Self::Rollover
            }
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_open_failed => {
                Self::OpenFailed
            }
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_write_failed => {
                Self::WriteFailed
            }
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_error => Self::Error,
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_format_changed => {
                Self::FormatChanged
            }
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_first_frame => {
                Self::FirstFrame
            }
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_source_lost => {
                Self::SourceLost
            }
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_source_connected => {
                Self::SourceConnected
            }
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_file_closed => {
                Self::FileClosed
            }
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_proxy_missing => {
                Self::ProxyMissing
            }
            value => Self::Unknown(value),
        }
    }
}

/// An owned recorder event, including original bounded diagnostic text.
#[derive(Debug, Clone, PartialEq)]
pub struct RecorderEvent {
    pub(super) kind: RecorderEventKind,
    pub(super) path: EventText,
    pub(super) message: EventText,
    pub(super) track_flags: u32,
    pub(super) dropped_packet_count: u64,
    pub(super) first_pts: FilePosition,
    pub(super) last_pts: FilePosition,
    pub(super) format_text: EventText,
    pub(super) source_format: SourceFormat,
}

impl RecorderEvent {
    /// Event kind.
    pub fn kind(&self) -> RecorderEventKind {
        self.kind
    }
    /// Reported file path, potentially truncated.
    pub fn path(&self) -> &EventText {
        &self.path
    }
    /// Diagnostic/causality text.
    pub fn message(&self) -> &EventText {
        &self.message
    }
    /// Raw affected-track bits, retaining future values and the zero sentinel.
    pub fn track_flags(&self) -> u32 {
        self.track_flags
    }
    /// Packet count attached to this particular event, not all session drops.
    pub fn dropped_packet_count(&self) -> u64 {
        self.dropped_packet_count
    }
    /// First packet timestamp where applicable.
    pub fn first_pts(&self) -> FilePosition {
        self.first_pts
    }
    /// Last packet timestamp where applicable.
    pub fn last_pts(&self) -> FilePosition {
        self.last_pts
    }
    /// Legacy textual format description.
    pub fn format_text(&self) -> &EventText {
        &self.format_text
    }
    /// Structured format description.
    pub fn source_format(&self) -> &SourceFormat {
        &self.source_format
    }
}

/// Owned playback counters for the current or most recent file session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlayerStats {
    pub(super) video_frames: [u64; 2],
    pub(super) audio_frames: u64,
    pub(super) metadata_frames: u64,
    pub(super) wallclock: FilePosition,
}

impl PlayerStats {
    /// Sent frames for HQ and proxy respectively.
    pub fn video_frames(&self) -> [u64; 2] {
        self.video_frames
    }
    /// Sent audio packet count.
    pub fn audio_frames(&self) -> u64 {
        self.audio_frames
    }
    /// Sent metadata packet count.
    pub fn metadata_frames(&self) -> u64 {
        self.metadata_frames
    }
    /// Playback wall-clock duration reported by the SDK.
    pub fn wallclock(&self) -> FilePosition {
        self.wallclock
    }
}

/// Owned recorder telemetry. Counts refer to packets rather than decoded samples.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecorderStats {
    pub(super) bytes_written: u64,
    pub(super) video_frames: u64,
    pub(super) audio_frames: u64,
    pub(super) metadata_frames: u64,
    pub(super) receiver_dropped: [u64; 3],
    pub(super) pipeline_dropped: [u64; 3],
    pub(super) writer_dropped: u64,
    pub(super) rollover_count: u64,
    pub(super) recording_time: FilePosition,
    pub(super) last_pts: [FilePosition; 3],
}

impl RecorderStats {
    /// Bytes written.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }
    /// Muxed video packet count.
    pub fn video_frames(&self) -> u64 {
        self.video_frames
    }
    /// Muxed audio packet count.
    pub fn audio_frames(&self) -> u64 {
        self.audio_frames
    }
    /// Muxed metadata packet count.
    pub fn metadata_frames(&self) -> u64 {
        self.metadata_frames
    }
    /// Receiver drops in video/audio/metadata order.
    pub fn receiver_dropped(&self) -> [u64; 3] {
        self.receiver_dropped
    }
    /// Pipeline drops in video/audio/metadata order.
    pub fn pipeline_dropped(&self) -> [u64; 3] {
        self.pipeline_dropped
    }
    /// Writer-stage drops across all tracks.
    pub fn writer_dropped(&self) -> u64 {
        self.writer_dropped
    }
    /// Completed rollover count.
    pub fn rollover_count(&self) -> u64 {
        self.rollover_count
    }
    /// Recorded duration reported by the SDK.
    pub fn recording_time(&self) -> FilePosition {
        self.recording_time
    }
    /// Last muxed timestamps in video/audio/metadata order.
    pub fn last_pts(&self) -> [FilePosition; 3] {
        self.last_pts
    }
}

impl From<&NDIlib_file_video_track_format_t> for VideoTrack {
    fn from(raw: &NDIlib_file_video_track_format_t) -> Self {
        Self {
            high_quality: raw.is_high_quality,
            width: raw.xres,
            height: raw.yres,
            frame_rate_n: raw.frame_rate_n,
            frame_rate_d: raw.frame_rate_d,
            fourcc: FourCc::from_raw(raw.fourcc),
            scan_type: FileScanType::from_raw(raw.frame_format_type),
            aspect_ratio: raw.picture_aspect_ratio,
        }
    }
}

impl TryFrom<&NDIlib_file_source_format_t> for SourceFormat {
    type Error = FileError;

    fn try_from(raw: &NDIlib_file_source_format_t) -> FileResult<Self> {
        let count = usize::try_from(raw.video_track_count)
            .map_err(|_| FileError::Protocol("video track count does not fit usize".into()))?;
        let tracks = raw.video_tracks.get(..count).ok_or_else(|| {
            FileError::Protocol(format!(
                "source format reports {} video tracks; the SDK layout holds {}",
                raw.video_track_count,
                raw.video_tracks.len()
            ))
        })?;
        Ok(Self {
            video_tracks: tracks.iter().map(VideoTrack::from).collect(),
            audio_sample_rate: raw.audio_sample_rate,
            audio_channels: raw.audio_no_channels,
            audio_samples: raw.audio_no_samples,
            audio_bit_depth: raw.audio_bit_depth,
            audio_fourcc: FourCc::from_raw(raw.audio_fourcc),
        })
    }
}

impl TryFrom<&NDIlib_file_player_file_info_t> for FileInfo {
    type Error = FileError;

    fn try_from(raw: &NDIlib_file_player_file_info_t) -> FileResult<Self> {
        Ok(Self {
            duration: FileDuration {
                position: FilePosition::from_ticks(raw.duration_100ns),
                is_final: raw.duration_is_final,
            },
            track_count: raw.track_count,
            video_track_count: raw.video_track_count,
            audio_track_count: raw.audio_track_count,
            metadata_track_count: raw.metadata_track_count,
            source_format: SourceFormat::try_from(&raw.source_format)?,
        })
    }
}

impl TryFrom<&NDIlib_file_player_event_t> for PlayerEvent {
    type Error = FileError;

    fn try_from(raw: &NDIlib_file_player_event_t) -> FileResult<Self> {
        Ok(Self {
            kind: PlayerEventKind::from_raw(raw.type_),
            path: EventText::from_buffer(&raw.p_path),
            message: EventText::from_buffer(&raw.p_message),
            first_pts: FilePosition::from_ticks(raw.first_pts_100ns),
            last_pts: FilePosition::from_ticks(raw.last_pts_100ns),
            source_timecode: raw
                .has_source_timecode
                .then(|| SourceTimecode::from_ticks(raw.source_timecode_100ns)),
            source_format: SourceFormat::try_from(&raw.source_format)?,
        })
    }
}

impl TryFrom<&NDIlib_file_recorder_event_t> for RecorderEvent {
    type Error = FileError;

    fn try_from(raw: &NDIlib_file_recorder_event_t) -> FileResult<Self> {
        Ok(Self {
            kind: RecorderEventKind::from_raw(raw.type_),
            path: EventText::from_buffer(&raw.p_path),
            message: EventText::from_buffer(&raw.p_message),
            track_flags: raw.track_flags,
            dropped_packet_count: raw.dropped_packet_count,
            first_pts: FilePosition::from_ticks(raw.first_pts_100ns),
            last_pts: FilePosition::from_ticks(raw.last_pts_100ns),
            format_text: EventText::from_buffer(&raw.source_format),
            source_format: SourceFormat::try_from(&raw.source_format_payload)?,
        })
    }
}

impl From<&NDIlib_file_player_stats_t> for PlayerStats {
    fn from(raw: &NDIlib_file_player_stats_t) -> Self {
        Self {
            video_frames: raw.video_frames_sent,
            audio_frames: raw.audio_frames_sent,
            metadata_frames: raw.metadata_frames_sent,
            wallclock: FilePosition::from_ticks(raw.playback_wallclock_100ns),
        }
    }
}

impl From<&NDIlib_file_recorder_stats_t> for RecorderStats {
    fn from(raw: &NDIlib_file_recorder_stats_t) -> Self {
        Self {
            bytes_written: raw.bytes_written,
            video_frames: raw.video_frames,
            audio_frames: raw.audio_frames,
            metadata_frames: raw.metadata_frames,
            receiver_dropped: [
                raw.receiver_dropped_video_frames,
                raw.receiver_dropped_audio_frames,
                raw.receiver_dropped_metadata_frames,
            ],
            pipeline_dropped: [
                raw.pipeline_dropped_video_frames,
                raw.pipeline_dropped_audio_frames,
                raw.pipeline_dropped_metadata_frames,
            ],
            writer_dropped: raw.writer_dropped_frames,
            rollover_count: raw.rollover_count,
            recording_time: FilePosition::from_ticks(raw.recording_time_100ns),
            last_pts: [
                FilePosition::from_ticks(raw.last_video_pts_100ns),
                FilePosition::from_ticks(raw.last_audio_pts_100ns),
                FilePosition::from_ticks(raw.last_metadata_pts_100ns),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_selection_is_explicit_and_rejects_nul() {
        assert!(SourceSpec::new("").is_err());
        assert!(SourceSpec::from_url("url\0tail").is_err());
        let source = SourceSpec::from_url("ndi://camera").unwrap();
        assert_eq!(source.name(), None);
        assert_eq!(source.url(), Some("ndi://camera"));
    }

    #[test]
    fn tracks_do_not_silently_map_empty_to_all() {
        assert!(TrackSelection::from_bits(0).is_err());
        assert!(TrackSelection::from_bits(8).is_err());
        assert_eq!((TrackSelection::AUDIO | TrackSelection::METADATA).bits(), 6);
    }

    #[test]
    fn filename_patterns_are_validated_without_filesystem_mutation() {
        let options = RecorderOptions::new(SourceSpec::new("camera").unwrap());
        assert_eq!(
            options.clone().exact_filename().filename_pattern(),
            Some("{prefix}")
        );
        assert!(options
            .clone()
            .with_pattern("{prefix}_{index:03}.mkv")
            .is_ok());
        for pattern in [
            "",
            "{prefix",
            "prefix}",
            "{unknown}",
            "{index:0}",
            "{index:2000}",
            "a\0b",
        ] {
            assert!(
                options.clone().with_pattern(pattern).is_err(),
                "{pattern:?}"
            );
        }
        let target = RecordingTarget::new("nonexistent-parent/take").unwrap();
        assert!(target.path().is_absolute());
        assert!(target.path().ends_with("nonexistent-parent/take"));
    }

    #[test]
    fn bounded_event_text_preserves_bad_utf8_and_missing_terminator() {
        let text = EventText::from_buffer(&[0xff_u8 as c_char, b'x' as c_char]);
        assert_eq!(text.as_bytes(), &[0xff, b'x']);
        assert!(!text.is_terminated());
        assert!(text.is_possibly_truncated());
        assert!(text.to_str().is_err());
        let short = EventText::from_buffer(&[b'a' as c_char, 0, 42, 42]);
        assert_eq!(short.as_bytes(), b"a");
        assert!(short.is_terminated());
        assert!(!short.is_possibly_truncated());
    }

    #[test]
    fn time_conversion_retains_large_values_and_checks_overflow() {
        let max = FilePosition::from_ticks(u64::MAX);
        assert_eq!(FilePosition::from_duration(max.duration()).unwrap(), max);
        assert!(FilePosition::from_duration(Duration::MAX).is_err());
        assert_eq!(
            FilePosition::from_duration(Duration::from_nanos(199))
                .unwrap()
                .ticks(),
            1
        );
        assert_eq!(SourceTimecode::from_ticks(-100).ticks(), -100);
    }

    #[test]
    fn future_event_values_are_preserved() {
        assert_eq!(
            PlayerEventKind::from_raw(1234),
            PlayerEventKind::Unknown(1234)
        );
        assert_eq!(
            RecorderEventKind::from_raw(4321),
            RecorderEventKind::Unknown(4321)
        );
        assert_eq!(FileScanType::from_raw(99), FileScanType::Unknown(99));
    }

    #[test]
    fn raw_track_count_is_bounded_before_slicing() {
        let mut raw = NDIlib_file_source_format_t {
            video_track_count: 3,
            ..Default::default()
        };
        assert!(matches!(
            SourceFormat::try_from(&raw),
            Err(FileError::Protocol(_))
        ));
        raw.video_track_count = 2;
        raw.video_tracks[1].fourcc = u32::MAX;
        let snapshot = SourceFormat::try_from(&raw).unwrap();
        assert_eq!(snapshot.video_tracks().len(), 2);
        assert_eq!(snapshot.video_tracks()[1].fourcc().raw(), u32::MAX);
        assert_eq!(snapshot.video_tracks()[0].aspect_ratio(), None);
    }

    #[test]
    fn malformed_event_text_does_not_hide_file_closed() {
        let mut raw = NDIlib_file_recorder_event_t {
            type_: NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_file_closed,
            ..Default::default()
        };
        raw.p_path.fill(0xff_u8 as c_char);
        let event = RecorderEvent::try_from(&raw).unwrap();
        assert_eq!(event.kind(), RecorderEventKind::FileClosed);
        assert!(!event.path().is_terminated());
        assert!(event.path().to_str().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_native_paths_are_rejected_without_lossy_conversion() {
        use std::os::unix::ffi::OsStrExt;
        assert!(RecordingTarget::new(Path::new(std::ffi::OsStr::from_bytes(b"bad\xff"))).is_err());
    }
}
