//! Owned configuration, event and format values for the File API.

use std::{
    borrow::Cow,
    ffi::{c_char, CString},
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    sys::{self, bindings::*},
    Error, FourCc, FrameRate, MediaTypes, Result, ScanType, StreamTier, Timecode,
};

/// Why an operation is not permitted in the current state, carried by
/// [`Error::InvalidState`].
///
/// Its `Display` text describes the state for diagnostics; match on the
/// variant rather than the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StateError {
    /// A session is recording or stopping, so another cannot start. Finish
    /// it, or keep taking events until it has ended.
    PreviousSessionOpen,
    /// Events from an earlier session kept arriving, so a new session was not
    /// started.
    EarlierSessionEvents,
    /// Waiting for a session to end needs a session that has been asked to
    /// stop, or that the SDK is ending.
    StopNotRequested,
    /// The session is stopping.
    Stopping,
    /// The operation needs a session that is recording.
    NotRecording,
    /// The recorder names its files exactly, so it cannot switch to a new file
    /// during a session: the next file would have the same name. Use a
    /// filename pattern with `{index}`.
    ExactFileName,
    /// Another event future is already waiting on the async handle, which
    /// delivers each event to one waiter. A close never reports this: closes
    /// made while one is awaited complete with it.
    WaiterPending,
    /// The async handle's object has been closed, or its thread stopped
    /// normally before it carried out the operation. A thread that failed
    /// reports [`OwnerFailed`](Self::OwnerFailed) instead.
    /// Also returned by an in-flight async finish or wait when a concurrent
    /// close's remaining finalization budget ends without a settled outcome.
    Closed,
    /// The async handle's thread failed, for example by panicking. The
    /// operation it was running, every command queued behind it, every later
    /// command, every event wait and the close report this, not
    /// [`Closed`](Self::Closed).
    OwnerFailed,
}

impl StateError {
    /// Whether the same call can succeed later without anything else changing
    /// on this handle: true only for [`WaiterPending`](Self::WaiterPending),
    /// once the other waiter has finished. A full command queue, which can
    /// also succeed later, is [`Error::QueueFull`](crate::Error::QueueFull).
    ///
    /// The other states change only through another call, such as finishing a
    /// session, or never, for a handle that is closed.
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::WaiterPending)
    }
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::PreviousSessionOpen => "finish the previous session before starting another",
            Self::EarlierSessionEvents => {
                "the recorder keeps producing events from an earlier session"
            }
            Self::StopNotRequested => "request a stop before waiting for the session to end",
            Self::Stopping => "the session is stopping",
            Self::NotRecording => "the operation needs a session that is recording",
            Self::ExactFileName => {
                "a recorder with an exact file name cannot switch to a new file during a session"
            }
            Self::WaiterPending => "another File event waiter is already pending",
            Self::Closed => "File owner has closed",
            Self::OwnerFailed => "File owner thread failed",
        })
    }
}

/// `value` as a C string for the SDK.
///
/// # Errors
///
/// [`Error::InvalidInput`] naming `what` when `value` is empty and
/// `allow_empty` is false, and [`Error::InvalidCString`] when it contains a
/// NUL.
fn cstring(value: &str, what: &str, allow_empty: bool) -> Result<CString> {
    if !allow_empty && value.is_empty() {
        return Err(Error::InvalidInput(format!("{what} must not be empty")));
    }
    Ok(CString::new(value)?)
}

/// An owned source name or direct URL used by a recorder.
///
/// Name and URL constructors are separate because the SDK gives a nonempty name
/// precedence over a URL. Converting a discovered [`crate::Source`] pins its
/// supplied endpoint, falling back to its name only when no address is present.
/// Endpoint pinning can become stale; [`Self::new`] deliberately selects by name
/// so the SDK can rediscover an endpoint after it changes. Neither policy silently
/// falls back to the other after a connection failure. No discovery or SDK call
/// occurs while constructing this value; constructors allocate one C string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceSpec {
    pub(super) name: Option<CString>,
    pub(super) url: Option<CString>,
}

impl SourceSpec {
    /// Select a source by its NDI name, for example `"MACHINE (Camera 1)"`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the name is empty, and
    /// [`Error::InvalidCString`] when it contains a NUL.
    pub fn new(name: impl AsRef<str>) -> Result<Self> {
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
    /// [`Error::InvalidInput`] when the address is empty, and
    /// [`Error::InvalidCString`] when it contains a NUL.
    pub fn from_url(url: impl AsRef<str>) -> Result<Self> {
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
    type Error = Error;

    fn try_from(source: &crate::Source) -> Result<Self> {
        match source.address() {
            Some(crate::SourceAddress::Ip(address) | crate::SourceAddress::Url(address)) => {
                Self::from_url(address)
            }
            None => Self::new(source.name()),
        }
    }
}

/// How a player advertises its NDI source. Creating this value advertises
/// nothing.
///
/// The options are checked when a player is created from them, by
/// [`FileApi::create_player`](super::FileApi::create_player) or
/// [`AsyncFilePlayer::create`](super::AsyncFilePlayer::create): the name must
/// not be empty, and neither the name nor the groups may contain a NUL.
#[derive(Debug, Clone)]
pub struct PlayerOptions {
    pub(super) name: String,
    groups: Option<String>,
}

impl PlayerOptions {
    /// Replace the advertised player name, preserving every other setting.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Inherit discovery groups from the NDI configuration.
    #[must_use]
    pub fn clear_groups(mut self) -> Self {
        self.groups = None;
        self
    }

    /// Requested advertised player name.
    #[must_use]
    pub fn configured_name(&self) -> &str {
        &self.name
    }

    /// Requested discovery groups, or `None` to inherit the NDI configuration.
    #[must_use]
    pub fn configured_groups(&self) -> Option<&str> {
        self.groups.as_deref()
    }

    /// A player that advertises its source as `name`, in the groups of the
    /// NDI configuration.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            groups: None,
        }
    }

    /// Set the comma-separated NDI groups the player's source is advertised
    /// in, instead of those of the NDI configuration.
    #[must_use]
    pub fn groups(mut self, groups: impl Into<String>) -> Self {
        self.groups = Some(groups.into());
        self
    }

    /// Check the options and convert their strings for the SDK.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the name is empty, and
    /// [`Error::InvalidCString`] when the name or the groups contain a NUL.
    pub(super) fn resolve(&self) -> Result<ResolvedPlayerOptions> {
        Ok(ResolvedPlayerOptions {
            name: cstring(&self.name, "player name", false)?,
            groups: self
                .groups
                .as_deref()
                .map(|groups| cstring(groups, "player groups", true))
                .transpose()?,
        })
    }
}

/// A player's options, checked and converted for the SDK.
#[derive(Debug)]
pub(super) struct ResolvedPlayerOptions {
    pub(super) name: CString,
    pub(super) groups: Option<CString>,
}

/// How a recorder names the files of a session from its [`RecordingTarget`].
///
/// The directory the name leads to must exist. The wrapper rejects ordinary
/// collisions at preflight; native creation after that check depends on the
/// platform and SDK. See [`Self::Exact`] for the tested exclusive-open
/// behavior and the limits of this check.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum FileNaming {
    /// Name each file exactly as its [`RecordingTarget`] says, with no index.
    ///
    /// [`FileRecorder::start`] and [`FileRecorder::rollover`] refuse a target
    /// that already exists with an [`Error::Io`] of kind
    /// [`AlreadyExists`](std::io::ErrorKind::AlreadyExists), before the SDK is
    /// called.
    /// This preflight does not reserve the path. A collision created after
    /// the check is handled by the native library; a rollover may return
    /// success before its later open fails and stops the session. Linux File
    /// SDK 6.4.0.2 uses exclusive creation and preserves existing files,
    /// including symlinks, in this race. Other platforms and SDK releases
    /// have not been verified. Use a recorder-controlled output directory
    /// when uninterrupted recording or destination containment matters;
    /// mutable parent directories are not pinned by this API.
    ///
    /// One target names one file, so a session cannot move to a new file by
    /// itself:
    ///
    /// * a format change of the source closes the file and ends the session,
    ///   which [`RecordingReport::stopped_by_sdk`] then reports; see
    ///   [`RecorderOptions::format_change_rollover`];
    /// * format-change rollover switched on, or a nonzero
    ///   [`RecorderOptions::rollover_size`], is refused with
    ///   [`Error::InvalidInput`] when the recorder is created;
    /// * [`FileRecorder::set_source`] is refused during a session with
    ///   [`StateError::ExactFileName`].
    ///
    /// [`FileRecorder::rollover`] to a new target still works. Use
    /// [`Indexed`](Self::Indexed), or a [`Pattern`](Self::Pattern) with
    /// `{index}`, for recordings that must survive these changes.
    ///
    /// [`FileRecorder::start`]: super::FileRecorder::start
    /// [`FileRecorder::rollover`]: super::FileRecorder::rollover
    /// [`FileRecorder::set_source`]: super::FileRecorder::set_source
    /// [`RecordingReport::stopped_by_sdk`]: super::RecordingReport::stopped_by_sdk
    Exact,
    /// The SDK's indexed `{prefix}_{index:03}.mkv`: the target `take` becomes
    /// `take_001.mkv`, and each new file of the session takes the lowest
    /// unused index.
    #[default]
    Indexed,
    /// A filename pattern using `{prefix}`, `{index}` and `{index:N}` tokens.
    ///
    /// `{prefix}` is replaced by the [`RecordingTarget`], and `{index:N}` pads
    /// the index with zeros to `N` digits, from 1 through 1024. The pattern
    /// may name a directory, for example `{prefix}/{index:03}.mkv`; that
    /// directory must exist.
    ///
    /// A pattern without `{index}` or `{index:N}` names its file exactly,
    /// with every rule of [`Exact`](Self::Exact).
    ///
    /// A pattern that is empty, or has an unknown token or unbalanced braces,
    /// is refused with [`Error::InvalidInput`] when the recorder is created,
    /// and one that contains a NUL with [`Error::InvalidCString`].
    Pattern(String),
}

/// Recorder configuration fixed when its native instance is created.
///
/// Nothing is checked until a recorder is created from the options, by
/// [`FileApi::create_recorder`](super::FileApi::create_recorder),
/// [`FileRecorder::reset`](super::FileRecorder::reset) or
/// [`AsyncFileRecorder::create`](super::AsyncFileRecorder::create). The
/// options are then checked against each other, so the order of the setter
/// calls does not matter.
#[derive(Debug, Clone)]
pub struct RecorderOptions {
    pub(super) source: SourceSpec,
    pub(super) tracks: MediaTypes,
    pub(super) planar_audio: bool,
    /// `None` until set: on for indexed file names, off for exact ones.
    format_change_rollover: Option<bool>,
    naming: FileNaming,
    pub(super) proxy: bool,
    pub(super) rollover_size: u64,
    pub(super) drop_wait: Duration,
}

impl RecorderOptions {
    /// Replace the recording source, preserving every other setting.
    #[must_use]
    pub fn source(mut self, source: SourceSpec) -> Self {
        self.source = source;
        self
    }

    /// Derive format-change rollover from the final naming policy again.
    #[must_use]
    pub fn clear_format_change_rollover(mut self) -> Self {
        self.format_change_rollover = None;
        self
    }

    /// Requested source descriptor, before any live source change.
    #[must_use]
    pub fn configured_source(&self) -> &SourceSpec {
        &self.source
    }

    /// Requested track types.
    #[must_use]
    pub fn configured_tracks(&self) -> MediaTypes {
        self.tracks
    }

    /// Whether planar audio storage was requested.
    #[must_use]
    pub fn configured_planar_audio(&self) -> bool {
        self.planar_audio
    }

    /// Explicit rollover choice, or `None` to derive it from the final naming policy at creation.
    #[must_use]
    pub fn configured_format_change_rollover(&self) -> Option<bool> {
        self.format_change_rollover
    }

    /// Requested file naming policy.
    #[must_use]
    pub fn configured_naming(&self) -> &FileNaming {
        &self.naming
    }

    /// Whether recording the proxy was requested.
    #[must_use]
    pub fn configured_proxy(&self) -> bool {
        self.proxy
    }

    /// Requested soft rollover threshold in bytes; zero disables it.
    #[must_use]
    pub fn configured_rollover_size(&self) -> u64 {
        self.rollover_size
    }

    /// Requested maximum wait on drop.
    #[must_use]
    pub fn configured_drop_wait(&self) -> Duration {
        self.drop_wait
    }

    /// Record every track of `source`, including its proxy when it has one,
    /// in [`FileNaming::Indexed`] files.
    pub fn new(source: SourceSpec) -> Self {
        Self {
            source,
            tracks: MediaTypes::ALL,
            planar_audio: false,
            format_change_rollover: None,
            naming: FileNaming::Indexed,
            proxy: true,
            rollover_size: 0,
            drop_wait: Self::DEFAULT_DROP_WAIT,
        }
    }

    /// How long dropping a recorder waits for its open file to be finalized,
    /// unless [`drop_wait`](Self::drop_wait) says otherwise.
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
    /// A wait longer than [`crate::MAX_TIMEOUT`] is refused with
    /// [`Error::InvalidInput`] when the recorder is created.
    #[must_use]
    pub fn drop_wait(mut self, wait: Duration) -> Self {
        self.drop_wait = wait;
        self
    }

    /// Select which track types to record: all three unless this says
    /// otherwise.
    ///
    /// An empty selection is refused with [`Error::InvalidInput`] when the
    /// recorder is created, rather than meaning all tracks as it does to the
    /// SDK.
    #[must_use]
    pub fn tracks(mut self, tracks: MediaTypes) -> Self {
        self.tracks = tracks;
        self
    }

    /// Store uncompressed float audio as native planar data when true.
    ///
    /// The default interleaved layout is more widely playable by third parties.
    #[must_use]
    pub fn planar_audio(mut self, planar: bool) -> Self {
        self.planar_audio = planar;
        self
    }

    /// Whether a change of the source's format closes the current file and
    /// continues the session in a new one.
    ///
    /// Left unset, a format change rolls over to a new file when files are
    /// indexed, and closes the file and ends the session when the file name
    /// is exact, because a second file cannot take the same name. With
    /// `false` a format change always ends the session. A session that ends
    /// this way is reported by [`RecordingReport::stopped_by_sdk`].
    ///
    /// `true` cannot be combined with an exact file name
    /// ([`FileNaming::Exact`], or a [`FileNaming::Pattern`] without
    /// `{index}`): creating the recorder then fails with
    /// [`Error::InvalidInput`].
    ///
    /// [`RecordingReport::stopped_by_sdk`]: super::RecordingReport::stopped_by_sdk
    #[must_use]
    pub fn format_change_rollover(mut self, enabled: bool) -> Self {
        self.format_change_rollover = Some(enabled);
        self
    }

    /// Whether to record the source's proxy alongside its full-quality video.
    #[must_use]
    pub fn proxy(mut self, enabled: bool) -> Self {
        self.proxy = enabled;
        self
    }

    /// Set a soft size threshold, in bytes, for keyframe-aligned rollover;
    /// zero disables it.
    ///
    /// A nonzero size cannot be combined with an exact file name
    /// ([`FileNaming::Exact`], or a [`FileNaming::Pattern`] without
    /// `{index}`), because the next file would have the same name: creating
    /// the recorder then fails with [`Error::InvalidInput`].
    #[must_use]
    pub fn rollover_size(mut self, bytes: u64) -> Self {
        self.rollover_size = bytes;
        self
    }

    /// Set how the recorder names its files: [`FileNaming::Indexed`] unless
    /// this says otherwise.
    #[must_use]
    pub fn naming(mut self, naming: FileNaming) -> Self {
        self.naming = naming;
        self
    }

    /// Check the options and settle what they leave open.
    ///
    /// Done when a recorder is created rather than in the setters, so that
    /// the order of those calls does not matter.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the drop wait exceeds
    /// [`crate::MAX_TIMEOUT`], the track selection is empty, the filename
    /// pattern is malformed, or an exact file name is combined with a
    /// rollover, and [`Error::InvalidCString`] when the filename pattern
    /// contains a NUL.
    pub(super) fn resolve(&self) -> Result<ResolvedRecorderOptions> {
        crate::to_ms_checked(self.drop_wait)?;
        if self.tracks.is_empty() {
            return Err(Error::InvalidInput(
                "a recorder must record video, audio or metadata; an empty track selection \
                 would record all three"
                    .into(),
            ));
        }
        // The SDK is given no pattern for its own indexed default.
        let (pattern, given) = match &self.naming {
            FileNaming::Exact => (EXACT_PATTERN, true),
            FileNaming::Indexed => (DEFAULT_PATTERN, false),
            FileNaming::Pattern(pattern) => (pattern.as_str(), true),
        };
        let naming = NamePattern::new(pattern)?;
        let pattern = given
            .then(|| cstring(pattern, "filename pattern", false))
            .transpose()?;
        if naming.is_exact() {
            if self.format_change_rollover == Some(true) {
                return Err(Error::InvalidInput(
                    "a recorder with an exact file name cannot roll over at a format change, \
                     because the next file would have the same name; use a filename pattern \
                     with {index}, or leave format-change rollover unset"
                        .into(),
                ));
            }
            if self.rollover_size > 0 {
                return Err(Error::InvalidInput(
                    "a recorder with an exact file name cannot roll over at a size limit, \
                     because the next file would have the same name; use a filename pattern \
                     with {index}"
                        .into(),
                ));
            }
        }
        Ok(ResolvedRecorderOptions {
            format_change_rollover: self.format_change_rollover.unwrap_or(!naming.is_exact()),
            naming,
            pattern,
        })
    }
}

/// What a recorder's options settle once they are checked against each other.
#[derive(Debug)]
pub(super) struct ResolvedRecorderOptions {
    pub(super) naming: NamePattern,
    /// The pattern the SDK is given, or `None` for its own indexed default.
    pub(super) pattern: Option<CString>,
    /// Whether a format change rolls over to a new file.
    pub(super) format_change_rollover: bool,
}

/// The SDK's filename pattern when none is set.
const DEFAULT_PATTERN: &str = "{prefix}_{index:03}.mkv";

/// The pattern that names each file exactly as its target says.
const EXACT_PATTERN: &str = "{prefix}";

/// One part of a filename pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatternPart<'a> {
    Text(&'a str),
    Prefix,
    /// `{index}`, with no padding, or `{index:N}`, padded to `N` digits.
    Index(usize),
}

/// The literal text and tokens of a filename pattern, in order.
fn pattern_parts(pattern: &str) -> Result<Vec<PatternPart<'_>>> {
    let unmatched = || Error::InvalidInput("unmatched filename-pattern brace".into());
    let mut parts = Vec::new();
    let mut rest = pattern;
    while let Some(start) = rest.find(['{', '}']) {
        if rest.as_bytes()[start] == b'}' {
            return Err(unmatched());
        }
        if start > 0 {
            parts.push(PatternPart::Text(&rest[..start]));
        }
        let after = &rest[start + 1..];
        let end = after.find('}').ok_or_else(unmatched)?;
        let token = &after[..end];
        let width = |width: &str| {
            (!width.is_empty() && width.bytes().all(|b| b.is_ascii_digit()))
                .then(|| width.parse::<u16>().ok())
                .flatten()
                .filter(|n| (1..=1024).contains(n))
        };
        parts.push(match token {
            "prefix" => PatternPart::Prefix,
            "index" => PatternPart::Index(0),
            _ => match token.strip_prefix("index:").and_then(width) {
                Some(width) => PatternPart::Index(usize::from(width)),
                None => {
                    return Err(Error::InvalidInput(format!(
                        "unsupported filename-pattern token {{{token}}}"
                    )))
                }
            },
        });
        rest = &after[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(PatternPart::Text(rest));
    }
    Ok(parts)
}

/// The filename pattern a recorder names its files with.
#[derive(Debug, Clone)]
pub(super) struct NamePattern {
    pattern: String,
    /// The pattern has no index, so each target names exactly one file.
    exact: bool,
}

impl NamePattern {
    /// Parse `pattern`, failing with [`Error::InvalidInput`] when it is
    /// malformed.
    fn new(pattern: &str) -> Result<Self> {
        let exact = !pattern_parts(pattern)?
            .iter()
            .any(|part| matches!(part, PatternPart::Index(_)));
        Ok(Self {
            pattern: pattern.to_owned(),
            exact,
        })
    }

    /// Whether each target names exactly one file.
    pub(super) fn is_exact(&self) -> bool {
        self.exact
    }

    /// The file the pattern names for `target`: the exact file, or for an
    /// indexed pattern the one with index 1, which locates its directory.
    ///
    /// A pattern that leaves the path relative is resolved, as the SDK
    /// resolves it, against the current directory.
    pub(super) fn first_file(&self, target: &RecordingTarget) -> Result<PathBuf> {
        let prefix = target.prefix.to_str().expect("validated UTF-8");
        let mut name = String::new();
        for part in pattern_parts(&self.pattern)? {
            match part {
                PatternPart::Text(text) => name.push_str(text),
                PatternPart::Prefix => name.push_str(prefix),
                PatternPart::Index(width) => name.push_str(&format!("{:0width$}", 1)),
            }
        }
        let file = PathBuf::from(name);
        Ok(if file.is_absolute() {
            file
        } else {
            std::path::absolute(file)?
        })
    }

    /// Confirm that the SDK can create the file this pattern gives `target`:
    /// that its directory exists and can be written to, and, for an exact
    /// name, that the file does not exist yet.
    ///
    /// The SDK accepts a rollover to a file it cannot create and then ends
    /// the whole session, so the mistake is caught here, while the session
    /// can still go on. Nothing is created.
    pub(super) fn check(&self, target: &RecordingTarget) -> Result<()> {
        let file = self.first_file(target)?;
        check_directory(file.parent().unwrap_or(&file))?;
        if self.exact && std::fs::symlink_metadata(&file).is_ok() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "the recording file {} already exists at preflight",
                    file.display()
                ),
            )));
        }
        Ok(())
    }
}

/// Confirm that `directory` exists and can be written to.
fn check_directory(directory: &Path) -> Result<()> {
    let refuse = |kind, problem: &str| {
        Error::Io(std::io::Error::new(
            kind,
            format!("the recording directory {} {problem}", directory.display()),
        ))
    };
    match std::fs::metadata(directory) {
        Err(error) => return Err(refuse(error.kind(), "cannot be used")),
        Ok(found) if !found.is_dir() => {
            return Err(refuse(
                std::io::ErrorKind::NotADirectory,
                "is not a directory",
            ))
        }
        Ok(_) => {}
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(directory.as_os_str().as_bytes())?;
        // SAFETY: `path` is a NUL-terminated string that outlives the call.
        if unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) } != 0 {
            return Err(refuse(
                std::io::ErrorKind::PermissionDenied,
                "cannot be written to",
            ));
        }
    }
    Ok(())
}

/// An owned absolute output prefix/path interpreted by the recorder's naming policy.
///
/// Construction freezes the current directory for relative paths. It neither
/// creates directories nor creates/removes/overwrites a file. The SDK requires
/// losslessly representable UTF-8 paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecordingTarget {
    pub(super) prefix: CString,
    path: PathBuf,
}

impl RecordingTarget {
    /// Take an output prefix or exact path, making a relative one absolute.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the path is empty or is not UTF-8,
    /// [`Error::InvalidCString`] when it contains a NUL, and [`Error::Io`]
    /// when the operating system cannot resolve a relative path.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let (path, prefix) = resolve_path(path.as_ref(), "recording target")?;
        Ok(Self { prefix, path })
    }

    /// The frozen absolute path/prefix, without filesystem canonicalization.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Make `path` absolute against the current directory and convert it for the
/// SDK. `what` names the path in the errors: a recording target and a file to
/// play are different things to the caller.
///
/// # Errors
///
/// [`Error::InvalidInput`] naming `what` when the path is empty or is not
/// UTF-8, [`Error::InvalidCString`] when it contains a NUL, and
/// [`Error::Io`] when the operating system cannot resolve a relative path.
pub(super) fn resolve_path(path: &Path, what: &str) -> Result<(PathBuf, CString)> {
    if path.as_os_str().is_empty() {
        return Err(Error::InvalidInput(format!("{what} must not be empty")));
    }
    let text = path
        .to_str()
        .ok_or_else(|| Error::InvalidInput(format!("{what} is not lossless UTF-8")))?;
    // Windows path resolution rejects NULs as an I/O error. Keep the SDK
    // string error consistent with absolute paths and the other platforms.
    if text.contains('\0') {
        return Err(CString::new(text).unwrap_err().into());
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        // Joining a drive-relative Windows path (C:take.mkv) discards the
        // current directory and leaves it relative. Resolve its drive too.
        std::path::absolute(path)?
    };
    let text = absolute
        .to_str()
        .ok_or_else(|| Error::InvalidInput(format!("{what} is not lossless UTF-8")))?;
    let text = cstring(text, what, false)?;
    Ok((absolute, text))
}

/// A path to play or probe, as the SDK takes it.
pub(super) fn path_string(path: &Path) -> Result<CString> {
    Ok(resolve_path(path, "file path")?.1)
}

/// Original SDK event bytes, retaining invalid UTF-8 and truncation evidence.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

/// Unsigned file-relative time in 100ns units, distinct from the editorial
/// [`Timecode`] that frames carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
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
    /// [`Error::InvalidInput`] when the duration exceeds 64 bits of ticks.
    pub fn from_duration(duration: Duration) -> Result<Self> {
        let ticks = duration.as_nanos() / 100;
        u64::try_from(ticks)
            .map(Self)
            .map_err(|_| Error::InvalidInput("file position exceeds 64-bit ticks".into()))
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

/// An explicit editorial timecode from which a recording session can advance.
///
/// The send-only [`Timecode::SYNTHESIZE`] sentinel is not a recording start.
/// Choose a value such as zero when the source has no meaningful timecode.
///
/// ```
/// use grafton_ndi::{file::{RecordingTimecode, SessionTimecode}, Timecode};
///
/// let start = SessionTimecode::new(Timecode::from_ticks(0))?;
/// let policy = RecordingTimecode::GenerateFrom(start);
/// assert_eq!(start.timecode(), Timecode::from_ticks(0));
/// assert!(SessionTimecode::new(Timecode::SYNTHESIZE).is_err());
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionTimecode(Timecode);

impl SessionTimecode {
    /// Validate an explicit recording start without changing its ticks.
    ///
    /// Negative values and every value except the send sentinel are accepted.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for [`Timecode::SYNTHESIZE`], including the
    /// default `Timecode`. Use an explicit start such as zero instead.
    pub fn new(timecode: Timecode) -> Result<Self> {
        if timecode == Timecode::SYNTHESIZE {
            return Err(Error::InvalidInput(
                "a recording start cannot use Timecode::SYNTHESIZE; choose an explicit start such as zero".into(),
            ));
        }
        Ok(Self(timecode))
    }

    /// The validated editorial timecode.
    pub const fn timecode(self) -> Timecode {
        self.0
    }

    /// The explicit start in 100-nanosecond ticks.
    pub const fn ticks(self) -> i64 {
        self.0.ticks()
    }
}

/// Timecode policy applied when a recording session starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum RecordingTimecode {
    /// Preserve the source's original editorial timecode.
    #[default]
    Preserve,
    /// Generate the session's timecode, starting at this value and advancing
    /// with the content, in place of the source's own. Use it when the source
    /// provides no meaningful timecode. Construct the explicit start with
    /// [`SessionTimecode::new`]; the send-only synthesize sentinel is refused.
    GenerateFrom(SessionTimecode),
}

/// A current duration that may still be growing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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

/// An owned description of one video track, not a decoded pixel layout.
///
/// A recorder's events describe the source's declared format, and a probe the
/// track header of the file. A value the SDK does not know yet is zero or
/// `None`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoTrack {
    tier: StreamTier,
    width: u32,
    height: u32,
    frame_rate: Option<FrameRate>,
    fourcc: FourCc,
    scan_type: Option<ScanType>,
    picture_aspect_ratio: Option<f32>,
}

impl VideoTrack {
    /// Which of the source's streams the track recorded: the full-quality
    /// program stream or the low-bandwidth preview (proxy) stream.
    pub fn tier(&self) -> StreamTier {
        self.tier
    }

    /// The width in pixels, or zero while it is not known.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The height in pixels, or zero while it is not known.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The frame rate, or `None` while it is not known.
    pub fn frame_rate(&self) -> Option<FrameRate> {
        self.frame_rate
    }

    /// The codec or packing identifier, including one this crate does not
    /// know.
    pub fn fourcc(&self) -> FourCc {
        self.fourcc
    }

    /// The scan type, or `None` for a value this crate does not know.
    ///
    /// A file recorded before the SDK stored the scan type reports
    /// [`ScanType::Interlaced`], the SDK's zero value.
    pub fn scan_type(&self) -> Option<ScanType> {
        self.scan_type
    }

    /// The display aspect ratio of the full frame, such as 16/9, or `None`
    /// when it is not known, in which case the pixels are square.
    pub fn picture_aspect_ratio(&self) -> Option<f32> {
        self.picture_aspect_ratio
    }
}

/// An owned layout snapshot containing no borrowed native memory.
///
/// A value the SDK does not know, or that does not apply, such as the audio
/// of a source without audio, is zero.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceFormat {
    video_tracks: Vec<VideoTrack>,
    audio_sample_rate: u32,
    audio_num_channels: u32,
    audio_num_samples: u32,
    audio_bit_depth: u32,
    audio_fourcc: FourCc,
}

impl SourceFormat {
    /// The populated video tracks: the program track first, then the
    /// preview (proxy) track when there is one.
    pub fn video_tracks(&self) -> &[VideoTrack] {
        &self.video_tracks
    }

    /// The audio sample rate in Hz.
    pub fn audio_sample_rate(&self) -> u32 {
        self.audio_sample_rate
    }

    /// The number of audio channels.
    pub fn audio_num_channels(&self) -> u32 {
        self.audio_num_channels
    }

    /// The number of audio samples per channel in each packet.
    pub fn audio_num_samples(&self) -> u32 {
        self.audio_num_samples
    }

    /// The audio bit depth.
    pub fn audio_bit_depth(&self) -> u32 {
        self.audio_bit_depth
    }

    /// The audio codec or packing identifier.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
            sys::bindings::NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_ended => Self::Ended,
            sys::bindings::NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_error => Self::Error,
            sys::bindings::NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_file_opened => {
                Self::FileOpened
            }
            sys::bindings::NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_first_frame => {
                Self::FirstFrame
            }
            sys::bindings::NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_file_closed => {
                Self::FileClosed
            }
            sys::bindings::NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_looped => Self::Looped,
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
    pub(super) source_timecode: Option<Timecode>,
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
    /// The source's original editorial timecode, recorded with the frame,
    /// when the SDK knows it. Unlike the file position it need not increase.
    pub fn source_timecode(&self) -> Option<Timecode> {
        self.source_timecode
    }
    /// Source format snapshot; zero/empty fields may mean inapplicable.
    pub fn source_format(&self) -> &SourceFormat {
        &self.source_format
    }
}

/// Recorder event kind; a write failure is not necessarily fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
            sys::bindings::NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_started => {
                Self::Started
            }
            sys::bindings::NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_rollover => {
                Self::Rollover
            }
            sys::bindings::NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_open_failed => {
                Self::OpenFailed
            }
            sys::bindings::NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_write_failed => {
                Self::WriteFailed
            }
            sys::bindings::NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_error => Self::Error,
            sys::bindings::NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_format_changed => {
                Self::FormatChanged
            }
            sys::bindings::NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_first_frame => {
                Self::FirstFrame
            }
            sys::bindings::NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_source_lost => {
                Self::SourceLost
            }
            sys::bindings::NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_source_connected => {
                Self::SourceConnected
            }
            sys::bindings::NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_file_closed => {
                Self::FileClosed
            }
            sys::bindings::NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_proxy_missing => {
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
    pub(super) tracks: MediaTypes,
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
    /// The track a [`WriteFailed`](RecorderEventKind::WriteFailed) event
    /// concerns; empty for every other event.
    pub fn tracks(&self) -> MediaTypes {
        self.tracks
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PlayerStats {
    pub(super) program_video_frames: u64,
    pub(super) preview_video_frames: u64,
    pub(super) audio_frames: u64,
    pub(super) metadata_frames: u64,
    pub(super) wallclock: FilePosition,
}

impl PlayerStats {
    /// Sent video frames for the program or preview stream.
    pub fn video_frames(&self, tier: StreamTier) -> u64 {
        match tier {
            StreamTier::Program => self.program_video_frames,
            StreamTier::Preview => self.preview_video_frames,
        }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RecorderStats {
    pub(super) bytes_written: u64,
    pub(super) video_frames: u64,
    pub(super) audio_frames: u64,
    pub(super) metadata_frames: u64,
    pub(super) video_frames_dropped_by_receiver: u64,
    pub(super) audio_frames_dropped_by_receiver: u64,
    pub(super) metadata_frames_dropped_by_receiver: u64,
    pub(super) video_frames_dropped_by_pipeline: u64,
    pub(super) audio_frames_dropped_by_pipeline: u64,
    pub(super) metadata_frames_dropped_by_pipeline: u64,
    pub(super) writer_dropped: u64,
    pub(super) rollover_count: u64,
    pub(super) recording_time: FilePosition,
    pub(super) last_video_pts: FilePosition,
    pub(super) last_audio_pts: FilePosition,
    pub(super) last_metadata_pts: FilePosition,
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
    /// Video packets dropped by the receiver.
    pub fn video_frames_dropped_by_receiver(&self) -> u64 {
        self.video_frames_dropped_by_receiver
    }
    /// Audio packets dropped by the receiver.
    pub fn audio_frames_dropped_by_receiver(&self) -> u64 {
        self.audio_frames_dropped_by_receiver
    }
    /// Metadata packets dropped by the receiver.
    pub fn metadata_frames_dropped_by_receiver(&self) -> u64 {
        self.metadata_frames_dropped_by_receiver
    }
    /// Video packets dropped by the pipeline.
    pub fn video_frames_dropped_by_pipeline(&self) -> u64 {
        self.video_frames_dropped_by_pipeline
    }
    /// Audio packets dropped by the pipeline.
    pub fn audio_frames_dropped_by_pipeline(&self) -> u64 {
        self.audio_frames_dropped_by_pipeline
    }
    /// Metadata packets dropped by the pipeline.
    pub fn metadata_frames_dropped_by_pipeline(&self) -> u64 {
        self.metadata_frames_dropped_by_pipeline
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
    /// Last muxed video timestamp.
    pub fn last_video_pts(&self) -> FilePosition {
        self.last_video_pts
    }
    /// Last muxed audio timestamp.
    pub fn last_audio_pts(&self) -> FilePosition {
        self.last_audio_pts
    }
    /// Last muxed metadata timestamp.
    pub fn last_metadata_pts(&self) -> FilePosition {
        self.last_metadata_pts
    }
}

/// A quantity the SDK reports as a signed integer, which must not be negative.
fn non_negative(value: i32, what: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| {
        Error::UnexpectedSdkResult(format!(
            "the source format reports a negative {what}: {value}"
        ))
    })
}

/// The scan type a File SDK frame format names, if it names one.
fn scan_type(format: NDIlib_file_video_frame_format_e) -> Option<ScanType> {
    match format {
        sys::bindings::NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_interleaved => {
            Some(ScanType::Interlaced)
        }
        sys::bindings::NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_progressive => {
            Some(ScanType::Progressive)
        }
        sys::bindings::NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_field_0 => {
            Some(ScanType::Field0)
        }
        sys::bindings::NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_field_1 => {
            Some(ScanType::Field1)
        }
        _ => None,
    }
}

/// Each media type and the SDK's recorder track flag for it.
const TRACK_FLAGS: [(MediaTypes, NDIlib_file_recorder_track_flags_e); 3] = [
    (
        MediaTypes::VIDEO,
        NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_video,
    ),
    (
        MediaTypes::AUDIO,
        NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_audio,
    ),
    (
        MediaTypes::METADATA,
        NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_metadata,
    ),
];

/// The SDK's recorder track flags for `tracks`.
pub(super) fn track_flags(tracks: MediaTypes) -> NDIlib_file_recorder_track_flags_e {
    TRACK_FLAGS
        .into_iter()
        .filter(|&(kind, _)| tracks.contains(kind))
        .fold(0, |flags, (_, flag)| flags | flag)
}

/// The media types the SDK's recorder track flags name. A flag this crate
/// does not know is left out.
fn tracks(flags: NDIlib_file_recorder_track_flags_e) -> MediaTypes {
    TRACK_FLAGS
        .into_iter()
        .filter(|&(_, flag)| flags & flag == flag)
        .fold(MediaTypes::NONE, |tracks, (kind, _)| tracks | kind)
}

impl VideoTrack {
    /// The track the SDK's `raw` describes.
    pub(super) fn from_sdk(raw: &NDIlib_file_video_track_format_t) -> Result<Self> {
        let ratio = raw.picture_aspect_ratio;
        Ok(Self {
            tier: if raw.is_high_quality {
                StreamTier::Program
            } else {
                StreamTier::Preview
            },
            width: non_negative(raw.xres, "width")?,
            height: non_negative(raw.yres, "height")?,
            // Both parts are at most i32::MAX, so only a zero part, which
            // the SDK reports while the rate is unknown, has no frame rate.
            frame_rate: FrameRate::new(
                non_negative(raw.frame_rate_n, "frame-rate numerator")?,
                non_negative(raw.frame_rate_d, "frame-rate denominator")?,
            )
            .ok(),
            fourcc: FourCc::from_raw(raw.fourcc),
            scan_type: scan_type(raw.frame_format_type),
            picture_aspect_ratio: (ratio.is_finite() && ratio > 0.0).then_some(ratio),
        })
    }
}

impl SourceFormat {
    /// The format the SDK's `raw` describes.
    pub(super) fn from_sdk(raw: &NDIlib_file_source_format_t) -> Result<Self> {
        let count = usize::try_from(raw.video_track_count).map_err(|_| {
            Error::UnexpectedSdkResult("video track count does not fit usize".into())
        })?;
        let tracks = raw.video_tracks.get(..count).ok_or_else(|| {
            Error::UnexpectedSdkResult(format!(
                "source format reports {} video tracks; the SDK layout holds {}",
                raw.video_track_count,
                raw.video_tracks.len()
            ))
        })?;
        Ok(Self {
            video_tracks: tracks
                .iter()
                .map(VideoTrack::from_sdk)
                .collect::<Result<_>>()?,
            audio_sample_rate: non_negative(raw.audio_sample_rate, "audio sample rate")?,
            audio_num_channels: non_negative(raw.audio_no_channels, "audio channel count")?,
            audio_num_samples: non_negative(raw.audio_no_samples, "audio sample count")?,
            audio_bit_depth: raw.audio_bit_depth,
            audio_fourcc: FourCc::from_raw(raw.audio_fourcc),
        })
    }
}

impl FileInfo {
    /// The information the SDK's `raw` reports.
    pub(super) fn from_sdk(raw: &NDIlib_file_player_file_info_t) -> Result<Self> {
        Ok(Self {
            duration: FileDuration {
                position: FilePosition::from_ticks(raw.duration_100ns),
                is_final: raw.duration_is_final,
            },
            track_count: raw.track_count,
            video_track_count: raw.video_track_count,
            audio_track_count: raw.audio_track_count,
            metadata_track_count: raw.metadata_track_count,
            source_format: SourceFormat::from_sdk(&raw.source_format)?,
        })
    }
}

impl PlayerEvent {
    /// The event the SDK's `raw` reports.
    pub(super) fn from_sdk(raw: &NDIlib_file_player_event_t) -> Result<Self> {
        Ok(Self {
            kind: PlayerEventKind::from_raw(raw.type_),
            path: EventText::from_buffer(&raw.p_path),
            message: EventText::from_buffer(&raw.p_message),
            first_pts: FilePosition::from_ticks(raw.first_pts_100ns),
            last_pts: FilePosition::from_ticks(raw.last_pts_100ns),
            source_timecode: raw
                .has_source_timecode
                .then(|| Timecode::from_ticks(raw.source_timecode_100ns)),
            source_format: SourceFormat::from_sdk(&raw.source_format)?,
        })
    }
}

impl RecorderEvent {
    /// The event the SDK's `raw` reports.
    pub(super) fn from_sdk(raw: &NDIlib_file_recorder_event_t) -> Result<Self> {
        Ok(Self {
            kind: RecorderEventKind::from_raw(raw.type_),
            path: EventText::from_buffer(&raw.p_path),
            message: EventText::from_buffer(&raw.p_message),
            tracks: tracks(raw.track_flags),
            dropped_packet_count: raw.dropped_packet_count,
            first_pts: FilePosition::from_ticks(raw.first_pts_100ns),
            last_pts: FilePosition::from_ticks(raw.last_pts_100ns),
            format_text: EventText::from_buffer(&raw.source_format),
            source_format: SourceFormat::from_sdk(&raw.source_format_payload)?,
        })
    }
}

impl PlayerStats {
    /// The statistics the SDK's `raw` reports.
    pub(super) fn from_sdk(raw: &NDIlib_file_player_stats_t) -> Self {
        Self {
            program_video_frames: raw.video_frames_sent[0],
            preview_video_frames: raw.video_frames_sent[1],
            audio_frames: raw.audio_frames_sent,
            metadata_frames: raw.metadata_frames_sent,
            wallclock: FilePosition::from_ticks(raw.playback_wallclock_100ns),
        }
    }
}

impl RecorderStats {
    /// The statistics the SDK's `raw` reports.
    pub(super) fn from_sdk(raw: &NDIlib_file_recorder_stats_t) -> Self {
        Self {
            bytes_written: raw.bytes_written,
            video_frames: raw.video_frames,
            audio_frames: raw.audio_frames,
            metadata_frames: raw.metadata_frames,
            video_frames_dropped_by_receiver: raw.receiver_dropped_video_frames,
            audio_frames_dropped_by_receiver: raw.receiver_dropped_audio_frames,
            metadata_frames_dropped_by_receiver: raw.receiver_dropped_metadata_frames,
            video_frames_dropped_by_pipeline: raw.pipeline_dropped_video_frames,
            audio_frames_dropped_by_pipeline: raw.pipeline_dropped_audio_frames,
            metadata_frames_dropped_by_pipeline: raw.pipeline_dropped_metadata_frames,
            writer_dropped: raw.writer_dropped_frames,
            rollover_count: raw.rollover_count,
            recording_time: FilePosition::from_ticks(raw.recording_time_100ns),
            last_video_pts: FilePosition::from_ticks(raw.last_video_pts_100ns),
            last_audio_pts: FilePosition::from_ticks(raw.last_audio_pts_100ns),
            last_metadata_pts: FilePosition::from_ticks(raw.last_metadata_pts_100ns),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_timecodes_refuse_only_the_send_sentinel() {
        for timecode in [Timecode::SYNTHESIZE, Timecode::default()] {
            let error = SessionTimecode::new(timecode).unwrap_err();
            assert!(matches!(error, Error::InvalidInput(ref message) if message.contains("zero")));
        }
        for ticks in [i64::MIN, -1, 0, 360_000_000_000, i64::MAX - 1] {
            let timecode = Timecode::from_ticks(ticks);
            let start = SessionTimecode::new(timecode).unwrap();
            assert_eq!(start.timecode(), timecode);
            assert_eq!(start.ticks(), ticks);
        }
    }

    #[test]
    fn file_statistics_preserve_each_native_media_and_stream_counter() {
        let playback = PlayerStats::from_sdk(&NDIlib_file_player_stats_t {
            video_frames_sent: [11, 22],
            audio_frames_sent: 33,
            metadata_frames_sent: u64::MAX,
            playback_wallclock_100ns: 55,
        });
        assert_eq!(playback.video_frames(StreamTier::Program), 11);
        assert_eq!(playback.video_frames(StreamTier::Preview), 22);
        assert_eq!(playback.audio_frames(), 33);
        assert_eq!(playback.metadata_frames(), u64::MAX);
        assert_eq!(playback.wallclock().ticks(), 55);

        let recording = RecorderStats::from_sdk(&NDIlib_file_recorder_stats_t {
            bytes_written: u64::MAX,
            video_frames: 1,
            audio_frames: 2,
            metadata_frames: 3,
            receiver_dropped_video_frames: 4,
            receiver_dropped_audio_frames: 5,
            receiver_dropped_metadata_frames: 6,
            pipeline_dropped_video_frames: 7,
            pipeline_dropped_audio_frames: 8,
            pipeline_dropped_metadata_frames: 9,
            writer_dropped_frames: 10,
            rollover_count: 11,
            recording_time_100ns: 12,
            last_video_pts_100ns: 13,
            last_audio_pts_100ns: 14,
            last_metadata_pts_100ns: 15,
        });
        assert_eq!(recording.bytes_written(), u64::MAX);
        assert_eq!(recording.video_frames(), 1);
        assert_eq!(recording.audio_frames(), 2);
        assert_eq!(recording.metadata_frames(), 3);
        assert_eq!(recording.video_frames_dropped_by_receiver(), 4);
        assert_eq!(recording.audio_frames_dropped_by_receiver(), 5);
        assert_eq!(recording.metadata_frames_dropped_by_receiver(), 6);
        assert_eq!(recording.video_frames_dropped_by_pipeline(), 7);
        assert_eq!(recording.audio_frames_dropped_by_pipeline(), 8);
        assert_eq!(recording.metadata_frames_dropped_by_pipeline(), 9);
        assert_eq!(recording.writer_dropped(), 10);
        assert_eq!(recording.rollover_count(), 11);
        assert_eq!(recording.recording_time().ticks(), 12);
        assert_eq!(recording.last_video_pts().ticks(), 13);
        assert_eq!(recording.last_audio_pts().ticks(), 14);
        assert_eq!(recording.last_metadata_pts().ticks(), 15);
    }

    fn invalid_text<T: std::fmt::Debug>(result: Result<T>) -> String {
        match result {
            Err(Error::InvalidInput(text)) => text,
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    /// Where the NUL that `result` was refused for is, in bytes.
    fn nul_position<T: std::fmt::Debug>(result: Result<T>) -> usize {
        match result {
            Err(Error::InvalidCString(error)) => error.nul_position(),
            other => panic!("expected InvalidCString, got {other:?}"),
        }
    }

    #[test]
    fn path_errors_name_what_the_path_is() {
        let empty = Path::new("");
        assert_eq!(
            invalid_text(path_string(empty)),
            "file path must not be empty"
        );
        assert_eq!(
            invalid_text(RecordingTarget::new(empty)),
            "recording target must not be empty"
        );
    }

    #[test]
    fn a_nul_in_any_string_for_the_sdk_is_the_crates_c_string_error() {
        // An absolute path on every platform: `/take` is not one on Windows, where
        // it would be resolved against the current drive and the NUL would sit
        // after that prefix. The path is used as it is, so the NUL stays where
        // `str::find` puts it in the text.
        let nul = std::env::current_dir().unwrap().join("take\0.mkv");
        assert!(nul.is_absolute());
        let at = nul.to_str().unwrap().find('\0').unwrap();
        assert_eq!(nul_position(path_string(&nul)), at);
        assert_eq!(nul_position(RecordingTarget::new(&nul)), at);
        assert_eq!(nul_position(SourceSpec::new("cam\0era")), 3);
        assert_eq!(nul_position(SourceSpec::from_url("url\0tail")), 3);
        assert_eq!(nul_position(PlayerOptions::new("Re\0play").resolve()), 2);
        assert_eq!(
            nul_position(PlayerOptions::new("Replay").groups("a\0b").resolve()),
            1
        );
        let pattern = FileNaming::Pattern("{prefix}\0_{index}.mkv".into());
        assert_eq!(
            nul_position(
                RecorderOptions::new(SourceSpec::new("camera").unwrap())
                    .naming(pattern)
                    .resolve()
            ),
            8
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_named_by_their_use() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let path = Path::new(OsStr::from_bytes(b"/take-\xff.mkv"));
        assert_eq!(
            invalid_text(path_string(path)),
            "file path is not lossless UTF-8"
        );
        assert_eq!(
            invalid_text(RecordingTarget::new(path)),
            "recording target is not lossless UTF-8"
        );
    }

    #[test]
    fn relative_paths_are_made_absolute_once() {
        let (absolute, text) = resolve_path(Path::new("take.mkv"), "file path").unwrap();
        assert!(absolute.is_absolute());
        assert_eq!(text.to_str().unwrap(), absolute.to_str().unwrap());
        assert_eq!(
            RecordingTarget::new("take.mkv").unwrap().path(),
            absolute.as_path()
        );
    }

    #[test]
    fn relative_paths_reject_nul_as_a_c_string_error() {
        let path = Path::new("take\0.mkv");
        assert_eq!(nul_position(path_string(path)), 4);
        assert_eq!(nul_position(RecordingTarget::new(path)), 4);
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_relative_paths_are_made_absolute() {
        for path in [r"C:take.mkv", r"C:folder\take.mkv", r"\take.mkv"] {
            let (absolute, text) = resolve_path(Path::new(path), "file path").unwrap();
            assert!(absolute.is_absolute(), "{absolute:?}");
            assert!(absolute.ends_with("take.mkv"));
            assert_eq!(text.to_str().unwrap(), absolute.to_str().unwrap());
            assert_eq!(path_string(Path::new(path)).unwrap(), text);
            assert_eq!(RecordingTarget::new(path).unwrap().path(), absolute);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_relative_patterns_name_absolute_files() {
        let target = RecordingTarget::new("take").unwrap();
        for pattern in [r"C:take_{index:03}.mkv", r"\take_{index:03}.mkv"] {
            let file = NamePattern::new(pattern)
                .unwrap()
                .first_file(&target)
                .unwrap();
            assert!(file.is_absolute(), "{file:?}");
            assert!(file.ends_with("take_001.mkv"));
        }
    }

    #[test]
    fn source_selection_is_explicit_and_rejects_nul() {
        assert!(SourceSpec::new("").is_err());
        assert!(SourceSpec::from_url("url\0tail").is_err());
        let source = SourceSpec::from_url("ndi://camera").unwrap();
        assert_eq!(source.name(), None);
        assert_eq!(source.url(), Some("ndi://camera"));
    }

    #[test]
    fn discovered_sources_pin_the_complete_endpoint_without_name_precedence() {
        use crate::{Source, SourceAddress};
        for address in [
            SourceAddress::Ip("192.0.2.10:5962".into()),
            SourceAddress::Url("ndi://user@camera.local:5962/path?opaque=1".into()),
        ] {
            let text = match &address {
                SourceAddress::Ip(text) | SourceAddress::Url(text) => text.as_str(),
            };
            for name in ["STUDIO (Camera 2)", "", "diagnostic\0name"] {
                let selected = Source::new(name).with_address(address.clone());
                let spec = SourceSpec::try_from(&selected).unwrap();
                assert_eq!(spec.name(), None, "name must not override an endpoint");
                assert_eq!(spec.url(), Some(text));
                assert_eq!(spec.clone(), spec);
            }
        }
        let named = Source::new("STUDIO (Camera 2)");
        let spec = SourceSpec::try_from(&named).unwrap();
        assert_eq!(spec.name(), Some(named.name()));
        assert_eq!(spec.url(), None);
        let addressed = named
            .clone()
            .with_address(SourceAddress::Ip("192.0.2.10:5962".into()));
        let logical = SourceSpec::new(addressed.name()).unwrap();
        assert_eq!(logical.name(), Some(addressed.name()));
        assert_eq!(logical.url(), None);
        for bad in ["", "bad\0endpoint"] {
            for address in [
                SourceAddress::Ip(bad.into()),
                SourceAddress::Url(bad.into()),
            ] {
                let selected = named.clone().with_address(address);
                assert!(
                    SourceSpec::try_from(&selected).is_err(),
                    "invalid endpoint must not fall back to name"
                );
            }
        }
        assert!(SourceSpec::try_from(&Source::new("")).is_err());
        assert!(matches!(
            SourceSpec::try_from(&Source::new("bad\0name")),
            Err(Error::InvalidCString(_))
        ));
    }

    #[test]
    fn player_options_are_checked_when_they_are_resolved() {
        let resolved = PlayerOptions::new("Replay").groups("").resolve().unwrap();
        assert_eq!(resolved.name.as_bytes(), b"Replay");
        assert_eq!(resolved.groups.unwrap().as_bytes(), b"");
        assert!(PlayerOptions::new("Replay")
            .resolve()
            .unwrap()
            .groups
            .is_none());
        assert_eq!(
            invalid_text(PlayerOptions::new("").resolve()),
            "player name must not be empty"
        );
    }

    #[test]
    fn a_drop_wait_beyond_the_largest_timeout_is_refused_when_resolved() {
        let options = RecorderOptions::new(SourceSpec::new("camera").unwrap());
        assert_eq!(options.drop_wait, RecorderOptions::DEFAULT_DROP_WAIT);
        options
            .clone()
            .drop_wait(crate::MAX_TIMEOUT)
            .resolve()
            .unwrap();
        assert!(invalid_text(
            options
                .drop_wait(crate::MAX_TIMEOUT + Duration::from_millis(1))
                .resolve()
        )
        .contains("MAX_TIMEOUT"));
    }

    #[test]
    fn tracks_do_not_silently_map_empty_to_all() {
        let options = RecorderOptions::new(SourceSpec::new("camera").unwrap());
        assert_eq!(options.tracks, MediaTypes::ALL);
        assert!(options.resolve().is_ok());
        let empty = options.clone().tracks(MediaTypes::NONE);
        assert!(invalid_text(empty.resolve()).contains("empty track selection"));
        let some = options.tracks(MediaTypes::AUDIO | MediaTypes::METADATA);
        assert_eq!(some.tracks, MediaTypes::AUDIO | MediaTypes::METADATA);
        assert!(some.resolve().is_ok());
    }

    /// The pattern the SDK is given for `naming`.
    fn sdk_pattern(naming: FileNaming) -> Result<Option<String>> {
        let options = RecorderOptions::new(SourceSpec::new("camera").unwrap()).naming(naming);
        Ok(options
            .resolve()?
            .pattern
            .map(|pattern| pattern.into_string().unwrap()))
    }

    #[test]
    fn filename_patterns_are_validated_without_filesystem_mutation() {
        assert_eq!(sdk_pattern(FileNaming::default()).unwrap(), None);
        assert_eq!(sdk_pattern(FileNaming::Indexed).unwrap(), None);
        assert_eq!(
            sdk_pattern(FileNaming::Exact).unwrap().as_deref(),
            Some("{prefix}")
        );
        assert_eq!(
            sdk_pattern(FileNaming::Pattern("{prefix}_{index:03}.mkv".into()))
                .unwrap()
                .as_deref(),
            Some("{prefix}_{index:03}.mkv")
        );
        for pattern in [
            "",
            "{prefix",
            "prefix}",
            "{unknown}",
            "{index:0}",
            "{index:2000}",
        ] {
            assert!(
                matches!(
                    sdk_pattern(FileNaming::Pattern(pattern.into())),
                    Err(Error::InvalidInput(_))
                ),
                "{pattern:?}"
            );
        }
        let target = RecordingTarget::new("nonexistent-parent/take").unwrap();
        assert!(target.path().is_absolute());
        assert!(target.path().ends_with("nonexistent-parent/take"));
    }

    #[test]
    fn only_a_pending_waiter_is_retryable() {
        use StateError::*;
        for (state, retryable) in [
            (PreviousSessionOpen, false),
            (EarlierSessionEvents, false),
            (StopNotRequested, false),
            (Stopping, false),
            (NotRecording, false),
            (ExactFileName, false),
            (WaiterPending, true),
            (Closed, false),
            (OwnerFailed, false),
        ] {
            assert_eq!(state.is_retryable(), retryable, "{state:?}");
        }
    }

    #[test]
    fn state_errors_keep_their_messages() {
        assert_eq!(
            StateError::PreviousSessionOpen.to_string(),
            "finish the previous session before starting another"
        );
        assert_eq!(
            Error::InvalidState(StateError::Stopping).to_string(),
            "Invalid File API state: the session is stopping"
        );
    }

    fn options() -> RecorderOptions {
        RecorderOptions::new(SourceSpec::new("camera").unwrap())
    }

    fn refused(options: RecorderOptions) -> String {
        match options.resolve() {
            Err(Error::InvalidInput(reason)) => reason,
            Err(other) => panic!("expected the options to be refused as input: {other}"),
            Ok(_) => panic!("expected the options to be refused"),
        }
    }

    #[test]
    fn a_pattern_is_exact_when_it_has_no_index() {
        for (pattern, exact) in [
            (DEFAULT_PATTERN, false),
            (EXACT_PATTERN, true),
            ("{prefix}.mkv", true),
            ("{prefix}_{index}.mkv", false),
            ("{prefix}/{index:03}.mkv", false),
        ] {
            assert_eq!(
                NamePattern::new(pattern).unwrap().is_exact(),
                exact,
                "{pattern:?}"
            );
        }
        let exact = |options: RecorderOptions| options.resolve().unwrap().naming.is_exact();
        assert!(!exact(options()));
        assert!(exact(options().naming(FileNaming::Exact)));
        assert!(exact(
            options().naming(FileNaming::Pattern("{prefix}.mkv".into()))
        ));
        assert!(!exact(
            options()
                .naming(FileNaming::Exact)
                .naming(FileNaming::Indexed)
        ));
    }

    #[test]
    fn format_change_rollover_follows_the_naming_unless_set() {
        let rolls = |options: RecorderOptions| options.resolve().unwrap().format_change_rollover;
        assert!(rolls(options()));
        assert!(!rolls(options().naming(FileNaming::Exact)));
        assert!(!rolls(options().format_change_rollover(false)));
        assert!(!rolls(
            options()
                .naming(FileNaming::Exact)
                .format_change_rollover(false)
        ));
        assert!(rolls(
            options()
                .naming(FileNaming::Pattern("{prefix}-{index:2}.mkv".into()))
                .format_change_rollover(true)
        ));
    }

    #[test]
    fn clearing_rollover_override_recomputes_the_policy_for_the_final_template() {
        let template = options().format_change_rollover(true);
        assert!(template
            .clone()
            .naming(FileNaming::Exact)
            .resolve()
            .is_err());
        let exact = template
            .clone()
            .naming(FileNaming::Exact)
            .clear_format_change_rollover()
            .resolve()
            .unwrap();
        assert!(!exact.format_change_rollover);
        let indexed = template
            .format_change_rollover(false)
            .clear_format_change_rollover()
            .resolve()
            .unwrap();
        assert!(indexed.format_change_rollover);
    }

    #[test]
    fn player_template_clear_groups_restores_the_native_null_default() {
        let template = PlayerOptions::new("Replay A").groups("");
        let override_groups = template.resolve().unwrap();
        assert!(override_groups.groups.is_some());
        let inherited = template.name("Replay B").clear_groups().resolve().unwrap();
        assert_eq!(inherited.name.as_bytes(), b"Replay B");
        assert!(inherited.groups.is_none());
    }

    #[test]
    fn an_exact_name_that_would_roll_over_is_refused_whatever_the_call_order() {
        for options in [
            options()
                .naming(FileNaming::Exact)
                .format_change_rollover(true),
            options()
                .format_change_rollover(true)
                .naming(FileNaming::Exact),
            options()
                .format_change_rollover(true)
                .naming(FileNaming::Pattern("{prefix}.mkv".into())),
        ] {
            let reason = refused(options);
            assert!(reason.contains("format change"), "{reason}");
            assert!(reason.contains("{index}"), "{reason}");
        }
        for options in [
            options().naming(FileNaming::Exact).rollover_size(1 << 20),
            options().rollover_size(1 << 20).naming(FileNaming::Exact),
        ] {
            let reason = refused(options);
            assert!(reason.contains("size limit"), "{reason}");
            assert!(reason.contains("{index}"), "{reason}");
        }
        // Indexed names may roll over either way.
        options()
            .rollover_size(1 << 20)
            .format_change_rollover(true)
            .resolve()
            .unwrap();
    }

    /// A fresh directory under the system temporary directory.
    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "grafton-ndi-naming-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        std::fs::create_dir(&directory).unwrap();
        directory
    }

    fn io_kind(result: Result<()>) -> std::io::ErrorKind {
        match result {
            Err(Error::Io(error)) => error.kind(),
            other => panic!("expected a filesystem error: {other:?}"),
        }
    }

    #[test]
    fn the_file_a_pattern_names_is_where_it_is_checked() {
        let directory = scratch("check");
        let target = RecordingTarget::new(directory.join("take")).unwrap();
        let first = |pattern: &str| {
            NamePattern::new(pattern)
                .unwrap()
                .first_file(&target)
                .unwrap()
        };
        assert_eq!(first(DEFAULT_PATTERN), directory.join("take_001.mkv"));
        assert_eq!(first(EXACT_PATTERN), directory.join("take"));
        assert_eq!(
            first("{prefix}/{index:03}.mkv"),
            directory.join("take/001.mkv")
        );

        // The default pattern checks the target's own directory.
        let indexed = NamePattern::new(DEFAULT_PATTERN).unwrap();
        indexed.check(&target).unwrap();
        let missing = RecordingTarget::new(directory.join("absent/take")).unwrap();
        assert_eq!(
            io_kind(indexed.check(&missing)),
            std::io::ErrorKind::NotFound
        );
        // A pattern that names a directory of its own checks that one.
        let nested = NamePattern::new("{prefix}/{index:03}.mkv").unwrap();
        assert_eq!(io_kind(nested.check(&target)), std::io::ErrorKind::NotFound);
        std::fs::create_dir(directory.join("take")).unwrap();
        nested.check(&target).unwrap();

        // An exact name refuses a file that exists, and an indexed one does not.
        let exact = NamePattern::new("{prefix}.mkv").unwrap();
        exact.check(&target).unwrap();
        std::fs::write(directory.join("take.mkv"), b"kept").unwrap();
        assert_eq!(
            io_kind(exact.check(&target)),
            std::io::ErrorKind::AlreadyExists
        );
        std::fs::write(directory.join("take_001.mkv"), b"kept").unwrap();
        indexed.check(&target).unwrap();
        assert_eq!(std::fs::read(directory.join("take.mkv")).unwrap(), b"kept");
        let _ = std::fs::remove_dir_all(&directory);
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
        assert_eq!(scan_type(99), None);
    }

    #[test]
    fn every_file_frame_format_names_its_scan_type() {
        for (format, scan) in [
            (
                NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_interleaved,
                ScanType::Interlaced,
            ),
            (
                NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_progressive,
                ScanType::Progressive,
            ),
            (
                NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_field_0,
                ScanType::Field0,
            ),
            (
                NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_field_1,
                ScanType::Field1,
            ),
        ] {
            assert_eq!(scan_type(format), Some(scan));
        }
    }

    #[test]
    fn track_selections_map_to_exactly_the_sdk_flags_and_back() {
        assert_eq!(
            track_flags(MediaTypes::ALL),
            NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_all
        );
        assert_eq!(
            track_flags(MediaTypes::VIDEO),
            NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_video
        );
        assert_eq!(
            track_flags(MediaTypes::AUDIO | MediaTypes::METADATA),
            NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_audio
                | NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_metadata
        );
        // The SDK reads no flags as all of them; creation refuses that first.
        assert_eq!(track_flags(MediaTypes::NONE), 0);
        for selection in [
            MediaTypes::NONE,
            MediaTypes::VIDEO,
            MediaTypes::AUDIO,
            MediaTypes::METADATA,
            MediaTypes::VIDEO | MediaTypes::METADATA,
            MediaTypes::ALL,
        ] {
            assert_eq!(tracks(track_flags(selection)), selection);
        }
        // A flag from a later SDK is left out rather than misread.
        assert_eq!(
            tracks(
                1 << 8 | NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_audio
            ),
            MediaTypes::AUDIO
        );
    }

    #[test]
    fn a_write_failure_names_its_track() {
        let raw = NDIlib_file_recorder_event_t {
            type_: NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_write_failed,
            track_flags: NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_audio,
            ..Default::default()
        };
        assert_eq!(
            RecorderEvent::from_sdk(&raw).unwrap().tracks(),
            MediaTypes::AUDIO
        );
        let other = NDIlib_file_recorder_event_t {
            type_: NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_started,
            ..Default::default()
        };
        assert!(RecorderEvent::from_sdk(&other).unwrap().tracks().is_empty());
    }

    #[test]
    fn a_player_event_keeps_the_source_timecode_only_when_the_sdk_has_one() {
        let mut raw = NDIlib_file_player_event_t {
            type_: NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_first_frame,
            source_timecode_100ns: -100,
            ..Default::default()
        };
        assert_eq!(PlayerEvent::from_sdk(&raw).unwrap().source_timecode(), None);
        raw.has_source_timecode = true;
        assert_eq!(
            PlayerEvent::from_sdk(&raw).unwrap().source_timecode(),
            Some(Timecode::from_ticks(-100))
        );
    }

    #[test]
    fn raw_track_count_is_bounded_before_slicing() {
        let mut raw = NDIlib_file_source_format_t {
            video_track_count: 3,
            ..Default::default()
        };
        assert!(matches!(
            SourceFormat::from_sdk(&raw),
            Err(Error::UnexpectedSdkResult(_))
        ));
        raw.video_track_count = 2;
        raw.video_tracks[0].is_high_quality = true;
        raw.video_tracks[1].fourcc = u32::MAX;
        let snapshot = SourceFormat::from_sdk(&raw).unwrap();
        assert_eq!(snapshot.video_tracks().len(), 2);
        assert_eq!(snapshot.video_tracks()[0].tier(), StreamTier::Program);
        assert_eq!(snapshot.video_tracks()[1].tier(), StreamTier::Preview);
        assert_eq!(snapshot.video_tracks()[1].fourcc().raw(), u32::MAX);
        assert_eq!(snapshot.video_tracks()[0].picture_aspect_ratio(), None);
    }

    #[test]
    fn a_track_reports_what_the_sdk_knows_and_nothing_it_does_not() {
        let mut raw = NDIlib_file_video_track_format_t {
            is_high_quality: true,
            xres: 1920,
            yres: 1080,
            frame_rate_n: 30_000,
            frame_rate_d: 1_001,
            fourcc: u32::from_le_bytes(*b"H264"),
            frame_format_type:
                NDIlib_file_video_frame_format_e_NDIlib_file_video_frame_format_progressive,
            picture_aspect_ratio: 16.0 / 9.0,
        };
        let track = VideoTrack::from_sdk(&raw).unwrap();
        assert_eq!(track.tier(), StreamTier::Program);
        assert_eq!((track.width(), track.height()), (1920, 1080));
        assert_eq!(
            track.frame_rate(),
            Some(FrameRate::new(30_000, 1_001).unwrap())
        );
        assert_eq!(track.fourcc().raw().to_le_bytes(), *b"H264");
        assert_eq!(track.scan_type(), Some(ScanType::Progressive));
        assert_eq!(track.picture_aspect_ratio(), Some(16.0 / 9.0));

        // Zero is what the SDK reports for what it does not know yet.
        let unknown = VideoTrack::from_sdk(&NDIlib_file_video_track_format_t {
            frame_rate_n: 30,
            ..Default::default()
        })
        .unwrap();
        assert_eq!((unknown.width(), unknown.height()), (0, 0));
        assert_eq!(unknown.frame_rate(), None);
        assert_eq!(unknown.picture_aspect_ratio(), None);
        raw.frame_format_type = 99;
        raw.picture_aspect_ratio = f32::NAN;
        let future = VideoTrack::from_sdk(&raw).unwrap();
        assert_eq!(future.scan_type(), None);
        assert_eq!(future.picture_aspect_ratio(), None);
    }

    #[test]
    fn a_negative_quantity_breaks_the_sdk_contract() {
        let track = NDIlib_file_video_track_format_t {
            xres: 1920,
            yres: 1080,
            frame_rate_n: 30,
            frame_rate_d: 1,
            ..Default::default()
        };
        for broken in [
            NDIlib_file_video_track_format_t { xres: -1, ..track },
            NDIlib_file_video_track_format_t { yres: -1, ..track },
            NDIlib_file_video_track_format_t {
                frame_rate_n: -30,
                ..track
            },
            NDIlib_file_video_track_format_t {
                frame_rate_d: -1,
                ..track
            },
        ] {
            assert!(matches!(
                VideoTrack::from_sdk(&broken),
                Err(Error::UnexpectedSdkResult(_))
            ));
        }
        let format = NDIlib_file_source_format_t {
            audio_sample_rate: 48_000,
            audio_no_channels: 2,
            audio_no_samples: 1_600,
            audio_bit_depth: 32,
            ..Default::default()
        };
        let audio = SourceFormat::from_sdk(&format).unwrap();
        assert_eq!(
            (
                audio.audio_sample_rate(),
                audio.audio_num_channels(),
                audio.audio_num_samples(),
                audio.audio_bit_depth()
            ),
            (48_000, 2, 1_600, 32)
        );
        for broken in [
            NDIlib_file_source_format_t {
                audio_sample_rate: -1,
                ..format
            },
            NDIlib_file_source_format_t {
                audio_no_channels: -1,
                ..format
            },
            NDIlib_file_source_format_t {
                audio_no_samples: -1,
                ..format
            },
        ] {
            assert!(matches!(
                SourceFormat::from_sdk(&broken),
                Err(Error::UnexpectedSdkResult(_))
            ));
        }
    }

    #[test]
    fn malformed_event_text_does_not_hide_file_closed() {
        let mut raw = NDIlib_file_recorder_event_t {
            type_: NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_file_closed,
            ..Default::default()
        };
        raw.p_path.fill(0xff_u8 as c_char);
        let event = RecorderEvent::from_sdk(&raw).unwrap();
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
