//! NDI receiving functionality for video, audio, and metadata.
//!
//! # Monitoring Connection Status
//!
//! The receiver can monitor connection status changes:
//!
//! ```no_run
//! # use grafton_ndi::{Ndi, Receiver, ReceiverEvent, ReceiverOptions, ReceiverBandwidth, Source};
//! # use std::time::Duration;
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! # let ndi = Ndi::new()?;
//! # let source = Source::new("CAMERA (Program)");
//! // In real usage, obtain the source from Finder::find_sources().
//! let options = ReceiverOptions::new(source).bandwidth(ReceiverBandwidth::MetadataOnly);
//! let receiver = Receiver::new(&ndi, &options)?;
//!
//! // Poll for status changes
//! if let Some(ReceiverEvent::StatusChanged(status)) =
//!     receiver.poll_event(Duration::from_millis(1000))?
//! {
//!     println!("Active connections: {}", status.connections());
//! }
//! # Ok(())
//! # }
//! ```

use std::{
    ffi::CString,
    marker::PhantomData,
    os::raw::c_char,
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use parking_lot::{Mutex, RwLock, RwLockReadGuard};

use crate::{
    capabilities::RuntimeFeature,
    capture::{
        capture_raw, AudioKind, CaptureKind, CaptureResult, MetadataKind, Notification, VideoKind,
    },
    finder::{RawSource, Source},
    sys::{self, bindings::*},
    timeout::Deadline,
    Error, MetadataFrame, Ndi, Result,
};

/// A string the receiver API returned, released when dropped.
struct RecvStringGuard<'a> {
    core: &'a ReceiverCore,
    pointer: *const c_char,
    strings: &'a sys::ReceiverStrings,
}
impl RecvStringGuard<'_> {
    /// The string's bytes before its terminator, or `None` for a null string.
    ///
    /// The scan for the terminator is bounded by
    /// [`MAX_METADATA_BYTES`](crate::frames::MAX_METADATA_BYTES); a string
    /// without one within it is an [`Error::UnexpectedSdkResult`].
    fn bytes(&self) -> Result<Option<&[u8]>> {
        if self.pointer.is_null() {
            return Ok(None);
        }
        // SAFETY: the SDK returned a readable NUL-terminated string, which
        // this guard keeps allocated for as long as it is borrowed.
        let bytes = unsafe { crate::frames::bounded_sdk_text(self.pointer) }.ok_or_else(|| {
            Error::UnexpectedSdkResult(format!(
                "SDK string is missing a NUL terminator within {} bytes",
                crate::frames::MAX_METADATA_BYTES
            ))
        })?;
        Ok(Some(bytes))
    }

    /// Copy the string, failing with [`Error::InvalidUtf8`] when it is not UTF-8.
    fn copy(&self) -> Result<Option<String>> {
        self.bytes()?
            .map(|bytes| {
                std::str::from_utf8(bytes)
                    .map(str::to_owned)
                    .map_err(|error| Error::InvalidUtf8(error.to_string()))
            })
            .transpose()
    }

    /// Copy the string, replacing invalid UTF-8 with U+FFFD.
    ///
    /// For a value the SDK reports only once, where an error would lose it.
    fn copy_lossy(&self) -> Result<Option<String>> {
        Ok(self
            .bytes()?
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned()))
    }
}
impl Drop for RecvStringGuard<'_> {
    fn drop(&mut self) {
        if !self.pointer.is_null() {
            // SAFETY: the SDK returned this string for this instance and it is
            // released exactly once.
            unsafe {
                self.strings
                    .NDIlib_recv_free_string(self.core.instance, self.pointer)
            };
        }
    }
}

/// Convert a delay the SDK reported, rejecting values outside its documented range.
fn receiver_delay(millis: i32) -> Result<Duration> {
    u64::try_from(millis)
        .ok()
        .filter(|millis| *millis <= 2000)
        .map(Duration::from_millis)
        .ok_or_else(|| {
            Error::UnexpectedSdkResult(format!("SDK returned invalid receive delay: {millis} ms"))
        })
}

fn receiver_delay_millis(delay: Duration) -> Result<i32> {
    if delay > Duration::from_secs(2) {
        return Err(Error::InvalidInput(
            "receive delay must not exceed two seconds".into(),
        ));
    }
    // Range validation makes this conversion bounded by 2000, including rounding.
    Ok(delay.as_nanos().div_ceil(1_000_000) as i32)
}

/// The timing of the retry loop behind every receiver capture, for all frame
/// kinds, for both [`Capture`] verbs and the async ones, and for
/// [`Receiver::poll_event`].
struct RetryPolicy {
    /// Timeout per individual capture attempt.
    poll_interval: Duration,
    /// Pause after an attempt that returned before its timeout without a
    /// frame, so that early returns do not busy-wait.
    sleep_between: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(100),
            sleep_between: Duration::from_millis(10),
        }
    }
}

/// Retry `capture_fn` until it returns a frame or `deadline` is spent.
///
/// The SDK returns from a capture early, without a frame, while a new
/// connection synchronizes and whenever it reports a status or source change,
/// so one SDK call does not wait out the caller's timeout. Each attempt is
/// handed at most the policy's poll interval of what is left of the budget,
/// which may have started before this call.
///
/// Returns `Ok(None)` once the budget is spent without a frame. An error from
/// an attempt ends the loop.
fn retry_capture<T, F>(deadline: Deadline, policy: &RetryPolicy, capture_fn: F) -> Result<Option<T>>
where
    F: FnMut(Duration) -> Result<Option<T>>,
{
    retry_capture_with_clock(
        deadline.budget(),
        policy,
        capture_fn,
        || deadline.elapsed(),
        std::thread::sleep,
    )
}

/// Run `attempt` under the default [`RetryPolicy`] until it returns a value or
/// `deadline` is spent, handing each attempt what is left of `deadline`
/// narrowed to the policy's poll interval: the one capture loop behind every
/// receiver capture and [`Receiver::poll_event`], the Advanced packet
/// receivers' included.
pub(crate) fn capture_within<T>(
    deadline: Deadline,
    mut attempt: impl FnMut(Attempt) -> Result<Option<T>>,
) -> Result<Option<T>> {
    retry_capture(deadline, &RetryPolicy::default(), |poll| {
        attempt(Attempt(deadline.within(poll)))
    })
}

/// One attempt of [`capture_within`]: the caller's deadline, narrowed to the
/// poll interval of the default [`RetryPolicy`].
///
/// Only [`capture_within`] makes one, and the shared capture guard is taken
/// only within one ([`ReceiverCore::capture_lock`]), with the SDK waiting only
/// for what is left of it. No capture therefore holds the guard for longer
/// than one poll interval, and a reconnect waits for it about that long at
/// most, however long the timeout of a capture in flight.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Attempt(Deadline);

/// [`retry_capture`] for a budget of `budget`, with `elapsed` reporting the
/// time since the budget started and `sleep` pausing between attempts.
///
/// The first attempt is made even when nothing is left of the budget, so a
/// zero budget makes exactly one attempt, with a zero timeout.
///
/// Only an attempt that returned before its own timeout is followed by the
/// policy's pause: that is the SDK's early return, which would otherwise spin.
/// An attempt that waited out its timeout is followed at once by the next, so
/// that a frame arriving in between is not delayed by a pause.
fn retry_capture_with_clock<T, F, E, S>(
    budget: Duration,
    policy: &RetryPolicy,
    mut capture_fn: F,
    mut elapsed: E,
    mut sleep: S,
) -> Result<Option<T>>
where
    F: FnMut(Duration) -> Result<Option<T>>,
    E: FnMut() -> Duration,
    S: FnMut(Duration),
{
    let time_left =
        |spent: Duration| Some(budget.saturating_sub(spent)).filter(|left| !left.is_zero());
    let mut started = elapsed();
    let mut poll = policy.poll_interval.min(budget.saturating_sub(started));
    loop {
        if let Some(frame) = capture_fn(poll)? {
            return Ok(Some(frame));
        }
        let mut now = elapsed();
        if now.saturating_sub(started) < poll {
            if let Some(left) = time_left(now) {
                sleep(policy.sleep_between.min(left));
                now = elapsed();
            }
        }
        let Some(left) = time_left(now) else {
            return Ok(None);
        };
        started = now;
        poll = policy.poll_interval.min(left);
    }
}

fn validate_ptz_value(name: &str, value: f32, min: f32, max: f32) -> Result<()> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(Error::InvalidInput(format!(
            "{name} must be finite and within {min}..={max}"
        )))
    }
}

fn validate_ptz_preset(preset: u8) -> Result<i32> {
    if preset <= 99 {
        Ok(i32::from(preset))
    } else {
        Err(Error::InvalidInput(
            "PTZ preset must be within 0..=99".into(),
        ))
    }
}

/// Send one PTZ command, the SDK function `$func` with arguments `$arg`, to
/// the source `$ptz` controls.
macro_rules! ptz_command {
    ($ptz:expr, $func:ident $(, $arg:expr)*) => {{
        // SAFETY: the receiver instance is live for the handle's borrow, and
        // every argument is a plain value.
        if unsafe { $ptz.core.sdk().$func($ptz.core.instance $(, $arg)*) } {
            Ok(())
        } else {
            Err(Error::NativeOperationFailed {
                operation: stringify!($func),
            })
        }
    }};
}

/// Operational color formats for received video frames.
///
/// These values are receiver configuration modes that the NDI SDK can use for
/// normal receive operation. SDK `Max` sentinel values are intentionally not
/// exposed in safe Rust because they are placeholders, not valid receiver
/// modes.
///
/// This enum is marked `#[non_exhaustive]` so future SDK receiver modes can be
/// added without another avoidable public enum break. Downstream `match`
/// expressions should include a wildcard arm.
///
/// Choose an explicit operational value such as [`ReceiverColorFormat::Fastest`],
/// [`ReceiverColorFormat::Best`], [`ReceiverColorFormat::RgbxRgba`], or
/// [`ReceiverColorFormat::BgrxBgra`] for receiver configuration.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReceiverColorFormat {
    /// BGRX for opaque video and BGRA when the source has alpha.
    #[default]
    BgrxBgra,
    /// UYVY for opaque video and BGRA when the source has alpha.
    UyvyBgra,
    /// RGBX for opaque video and RGBA when the source has alpha.
    RgbxRgba,
    /// UYVY for opaque video and RGBA when the source has alpha.
    UyvyRgba,
    /// The format the SDK can deliver with the least conversion work.
    /// Individual fields are delivered even if video fields were disabled.
    Fastest,
    /// The format closest to the source's native precision, including
    /// 16-bit formats. Individual fields are delivered even if video fields
    /// were disabled.
    Best,
    /// Preserve native planar 4:4:4 color when available. Requires NDI 6.4.
    /// Individual fields are delivered, like `Best`, even if video fields were disabled.
    Best444,
}

impl ReceiverColorFormat {
    /// The SDK's value for this format.
    const fn to_sdk(self) -> NDIlib_recv_color_format_e {
        match self {
            Self::BgrxBgra => NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA,
            Self::UyvyBgra => NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_BGRA,
            Self::RgbxRgba => NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA,
            Self::UyvyRgba => NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_RGBA,
            Self::Fastest => NDIlib_recv_color_format_e_NDIlib_recv_color_format_fastest,
            Self::Best => NDIlib_recv_color_format_e_NDIlib_recv_color_format_best,
            Self::Best444 => NDIlib_recv_color_format_e_NDIlib_recv_color_format_best_444,
        }
    }
}

/// Operational bandwidth modes for receivers.
///
/// These values are receiver configuration modes that the NDI SDK can use for
/// normal receive operation. SDK `Max` sentinel values are intentionally not
/// exposed in safe Rust because they are placeholders, not valid receiver
/// modes.
///
/// This enum is marked `#[non_exhaustive]` so future SDK receiver modes can be
/// added without another avoidable public enum break. Downstream `match`
/// expressions should include a wildcard arm.
///
/// Choose an explicit operational value such as [`ReceiverBandwidth::Highest`],
/// [`ReceiverBandwidth::Lowest`], [`ReceiverBandwidth::AudioOnly`], or
/// [`ReceiverBandwidth::MetadataOnly`] for receiver configuration.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReceiverBandwidth {
    /// Receive metadata only.
    MetadataOnly,
    /// Receive metadata and audio, without video.
    AudioOnly,
    /// Receive metadata, audio and lower-bandwidth preview video.
    Lowest,
    /// Receive metadata, audio and full-bandwidth video.
    #[default]
    Highest,
    /// Receive metadata and lower-bandwidth video, without audio. Requires NDI 6.4.
    VideoOnlyLowest,
    /// Receive metadata and full-bandwidth video, without audio. Requires NDI 6.4.
    VideoOnlyHighest,
}

impl ReceiverBandwidth {
    /// The SDK's value for this mode.
    const fn to_sdk(self) -> NDIlib_recv_bandwidth_e {
        match self {
            Self::MetadataOnly => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_metadata_only,
            Self::AudioOnly => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_audio_only,
            Self::Lowest => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
            Self::Highest => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
            Self::VideoOnlyLowest => {
                NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_video_only_lowest
            }
            Self::VideoOnlyHighest => {
                NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_video_only_highest
            }
        }
    }
}

/// Configuration for creating a [`Receiver`]: the source it connects to and
/// how it delivers that source's media.
///
/// [`new`](Self::new) starts from the defaults; the presets start from
/// settings suited to one use. Every setter replaces one setting.
///
/// # Examples
///
/// ```no_run
/// # use grafton_ndi::{Ndi, Receiver, ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions, Source};
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = Ndi::new()?;
/// # let source = Source::new("CAMERA (Program)");
/// // In real usage, obtain the source from Finder::find_sources().
/// let options = ReceiverOptions::new(source)
///     .color_format(ReceiverColorFormat::UyvyBgra)
///     .bandwidth(ReceiverBandwidth::Lowest)
///     .name("Multiviewer");
/// let receiver = Receiver::new(&ndi, &options)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ReceiverOptions {
    source: Source,
    color_format: ReceiverColorFormat,
    bandwidth: ReceiverBandwidth,
    allow_video_fields: bool,
    name: Option<String>,
    #[cfg(feature = "advanced")]
    pub(crate) config: Option<crate::advanced::AdvancedConfig>,
}

#[repr(C)]
pub(crate) struct RawRecvCreateV3 {
    _source: RawSource,
    _name: Option<CString>,
    pub raw: NDIlib_recv_create_v3_t,
}

impl ReceiverOptions {
    /// Receive from `source` with the default settings:
    /// [`ReceiverColorFormat::BgrxBgra`], [`ReceiverBandwidth::Highest`],
    /// video fields allowed, and a receiver name the SDK chooses.
    pub fn new(source: Source) -> Self {
        Self {
            source,
            color_format: ReceiverColorFormat::BgrxBgra,
            bandwidth: ReceiverBandwidth::Highest,
            allow_video_fields: true,
            name: None,
            #[cfg(feature = "advanced")]
            config: None,
        }
    }

    /// Settings for capturing snapshots: RGB color, low bandwidth, whole
    /// frames.
    ///
    /// Suited to image export, thumbnails and analysis pipelines, and to
    /// networks with little bandwidth to spare:
    /// - color format [`ReceiverColorFormat::RgbxRgba`], which the image
    ///   encoders take;
    /// - bandwidth [`ReceiverBandwidth::Lowest`], the source's preview stream;
    /// - video fields not allowed, so interlaced video arrives as whole frames.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Finder, FinderOptions, ReceiverOptions, Receiver};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_secs(1))?;
    /// # let sources = finder.current_sources()?;
    /// let options = ReceiverOptions::snapshot_preset(sources[0].clone()).name("Snapshot Receiver");
    /// let receiver = Receiver::new(&ndi, &options)?;
    ///
    /// // Capture and encode in one line (requires image-encoding feature)
    /// #[cfg(feature = "image-encoding")]
    /// if let Some(frame) = receiver.video().capture(Duration::from_secs(5))? {
    ///     let png_bytes = frame.encode_png()?;
    ///     std::fs::write("snapshot.png", &png_bytes)?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn snapshot_preset(source: Source) -> Self {
        Self::new(source)
            .color_format(ReceiverColorFormat::RgbxRgba)
            .bandwidth(ReceiverBandwidth::Lowest)
            .allow_video_fields(false)
    }

    /// Settings for full-quality video processing: RGB color, full
    /// bandwidth, interlaced video as fields.
    ///
    /// Suited to recording, broadcast and analysis that needs every pixel:
    /// - color format [`ReceiverColorFormat::RgbxRgba`];
    /// - bandwidth [`ReceiverBandwidth::Highest`], the source's program stream;
    /// - video fields allowed, so interlaced video keeps its fields.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Finder, FinderOptions, ReceiverOptions, Receiver};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_secs(1))?;
    /// # let sources = finder.current_sources()?;
    /// let options =
    ///     ReceiverOptions::high_quality_preset(sources[0].clone()).name("High Quality Receiver");
    /// let receiver = Receiver::new(&ndi, &options)?;
    ///
    /// // Capture full quality frames
    /// if let Some(frame) = receiver.video().capture(Duration::from_secs(5))? {
    ///     println!("Captured {width}x{height} frame", width = frame.width(), height = frame.height());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn high_quality_preset(source: Source) -> Self {
        Self::new(source)
            .color_format(ReceiverColorFormat::RgbxRgba)
            .bandwidth(ReceiverBandwidth::Highest)
            .allow_video_fields(true)
    }

    /// Settings for monitoring a connection and controlling the source,
    /// without its video or audio.
    ///
    /// Suited to connection tracking, tally and PTZ control:
    /// - bandwidth [`ReceiverBandwidth::MetadataOnly`];
    /// - the default color format, which no frame uses;
    /// - video fields not allowed.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Finder, FinderOptions, ReceiverEvent, ReceiverOptions, Receiver};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_secs(1))?;
    /// # let sources = finder.current_sources()?;
    /// let options = ReceiverOptions::monitoring_preset(sources[0].clone()).name("Status Monitor");
    /// let receiver = Receiver::new(&ndi, &options)?;
    ///
    /// // Poll for status changes
    /// if let Some(ReceiverEvent::StatusChanged(status)) =
    ///     receiver.poll_event(Duration::from_millis(1000))?
    /// {
    ///     println!("Active connections: {}", status.connections());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn monitoring_preset(source: Source) -> Self {
        Self::new(source)
            .bandwidth(ReceiverBandwidth::MetadataOnly)
            .allow_video_fields(false)
    }

    /// Set the pixel formats the receiver delivers video in.
    #[must_use]
    pub fn color_format(mut self, color_format: ReceiverColorFormat) -> Self {
        self.color_format = color_format;
        self
    }

    /// Set which of the source's streams the receiver takes, and at what
    /// quality.
    #[must_use]
    pub fn bandwidth(mut self, bandwidth: ReceiverBandwidth) -> Self {
        self.bandwidth = bandwidth;
        self
    }

    /// Set whether interlaced video is delivered as individual fields rather
    /// than whole frames.
    #[must_use]
    pub fn allow_video_fields(mut self, allow: bool) -> Self {
        self.allow_video_fields = allow;
        self
    }

    /// Set the receiver's own name, which the SDK otherwise chooses.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Create the receiver with this per-instance Advanced SDK configuration,
    /// which it keeps for as long as it lives.
    ///
    /// [`Receiver::new`] then needs an Advanced runtime, and a build with the
    /// Advanced SDK.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn config(mut self, config: crate::advanced::AdvancedConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub(crate) fn validate_runtime(&self, ndi: &Ndi) -> Result<()> {
        if matches!(self.color_format, ReceiverColorFormat::Best444) {
            ndi.require_feature(RuntimeFeature::ReceiveBest444)?;
        }
        if matches!(
            self.bandwidth,
            ReceiverBandwidth::VideoOnlyLowest | ReceiverBandwidth::VideoOnlyHighest
        ) {
            ndi.require_feature(RuntimeFeature::ReceiveVideoOnly)?;
        }
        Ok(())
    }

    /// The native creation settings, with the strings they point to.
    pub(crate) fn to_raw(&self) -> Result<RawRecvCreateV3> {
        let source = self.source.to_raw()?;
        let name = self.name.as_deref().map(CString::new).transpose()?;
        let p_ndi_recv_name = name.as_ref().map_or(ptr::null(), |n| n.as_ptr());
        let source_raw = source.raw;

        Ok(RawRecvCreateV3 {
            raw: NDIlib_recv_create_v3_t {
                source_to_connect_to: source_raw,
                color_format: self.color_format.to_sdk(),
                bandwidth: self.bandwidth.to_sdk(),
                allow_video_fields: self.allow_video_fields,
                p_ndi_recv_name,
            },
            _source: source,
            _name: name,
        })
    }
}

mod mode_sealed {
    pub trait Sealed {}
    pub trait CaptureSealed {}
}

/// Sealed receiver capability mode: what a [`Receiver`] may do besides its
/// common controls.
///
/// The common controls work in every mode: tally, PTZ, upstream metadata,
/// connection statistics, delays, source-name changes, connecting and
/// reconnecting. A [`CaptureMode`] adds the receiver's own audio, metadata and
/// event capture, and only [`RawCapture`] adds video capture.
pub trait ReceiverMode: mode_sealed::Sealed + Send + Sync {}

/// Sealed receiver mode in which the receiver's own audio, metadata and event
/// capture are available: [`RawCapture`] and [`FrameSyncAttached`].
///
/// A mode without it hands those frames and events to the object that holds
/// the receiver, which captures them itself.
pub trait CaptureMode: ReceiverMode + mode_sealed::CaptureSealed {}

/// Ordinary receiver mode, permitting raw video capture.
#[derive(Debug)]
pub enum RawCapture {}
/// Receiver mode held exclusively by [`crate::FrameSync`], without raw video access.
#[derive(Debug)]
pub enum FrameSyncAttached {}
/// Receiver mode held by
/// [`PacketReceiver`](crate::advanced::PacketReceiver), with the common
/// controls only.
///
/// Packet capture takes every frame and event the receiver delivers, so the
/// receiver's own video, audio, metadata and event capture are unavailable in
/// this mode.
#[cfg(feature = "advanced")]
#[derive(Debug)]
pub enum PacketAttached {}
impl mode_sealed::Sealed for RawCapture {}
impl mode_sealed::Sealed for FrameSyncAttached {}
impl mode_sealed::CaptureSealed for RawCapture {}
impl mode_sealed::CaptureSealed for FrameSyncAttached {}
impl ReceiverMode for RawCapture {}
impl ReceiverMode for FrameSyncAttached {}
impl CaptureMode for RawCapture {}
impl CaptureMode for FrameSyncAttached {}
#[cfg(feature = "advanced")]
impl mode_sealed::Sealed for PacketAttached {}
#[cfg(feature = "advanced")]
impl ReceiverMode for PacketAttached {}

/// An owned NDI receiver. The default mode permits all raw capture kinds.
/// [`FrameSync`](crate::FrameSync), and the Advanced `PacketReceiver`, change
/// the mode of the receiver they hold while retaining the same resource core;
/// see [`ReceiverMode`].
///
/// Dropping the receiver disconnects it from its source, unless a
/// [`RecvAdvertiser`](crate::discovery::RecvAdvertiser) registration still
/// holds it: a registered receiver stays connected, and advertised, until that
/// registration is removed or its advertiser is dropped.
pub struct Receiver<M: ReceiverMode = RawCapture> {
    pub(crate) core: Arc<ReceiverCore>,
    _mode: PhantomData<M>,
}

// Preserve the owned receiver's pre-Arc unwind contract. Its private core keeps
// a valid native handle throughout every operation; Rust locks are released on
// unwind, pending notifications are individually committed, and no public method
// runs caller code while mutating protected receiver state. Sharing ownership for
// advertisements therefore introduces no new partially updated public invariant.
// This deliberately does not assert RefUnwindSafe for a shared receiver borrow.
impl<M: ReceiverMode + std::panic::UnwindSafe> std::panic::UnwindSafe for Receiver<M> {}

pub(crate) struct ReceiverCore {
    pub(crate) instance: NDIlib_recv_instance_t,
    pub(crate) _ndi: Ndi,
    source: Source,
    /// Serializes connection changes against in-flight captures.
    ///
    /// Capture calls take this lock *shared* (so video, audio, and metadata
    /// captures still run concurrently), while [`Receiver::reconnect`] takes it
    /// *exclusively*. This guarantees `NDIlib_recv_connect` never overlaps a
    /// `NDIlib_recv_capture_v3` on the same instance — the one combination the
    /// SDK does not make safe — without otherwise serializing capture.
    ///
    /// A shared hold is a [`CaptureLock`], taken for one [`Attempt`] only, and
    /// the native capture cannot be called without one.
    capture_guard: RwLock<()>,
    pending_status: AtomicBool,
    pending_source: AtomicBool,
    pending_unknown: Mutex<Option<u32>>,
    source_query_guard: Mutex<()>,
    #[cfg(feature = "advanced")]
    _config: Option<crate::advanced::AdvancedConfig>,
}

impl Receiver {
    /// Create a receiver and connect it to the configured source.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] when the options need a mode the
    /// selected runtime lacks, or carry an Advanced configuration that the
    /// runtime or this build cannot take; [`Error::InvalidCString`] for a name
    /// or source that contains a NUL byte; and [`Error::NativeOperationFailed`]
    /// when the SDK creates no receiver.
    pub fn new(ndi: &Ndi, options: &ReceiverOptions) -> Result<Self> {
        #[cfg(feature = "advanced")]
        if let Some(config) = &options.config {
            return Self::new_configured(ndi, options, config);
        }
        // SAFETY: the SDK's receiver creation returns null or a fresh receiver
        // that nothing else owns, and the settings outlive the call.
        unsafe {
            Self::create(
                ndi,
                options,
                "NDIlib_recv_create_v3",
                #[cfg(feature = "advanced")]
                None,
                |core, raw| core.NDIlib_recv_create_v3(raw),
            )
        }
    }

    /// The one creation path: check `options` against the runtime, create a
    /// receiver through `create`, the entry point `operation`, which is
    /// handed the core table and the native settings for `options`, and keep
    /// `config` for as long as the native receiver lives.
    ///
    /// # Safety
    ///
    /// `create` returns null or a live receiver of `ndi`'s runtime, made from
    /// the settings it was handed, that nothing else owns.
    pub(crate) unsafe fn create(
        ndi: &Ndi,
        options: &ReceiverOptions,
        operation: &'static str,
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(&'static sys::Core, &NDIlib_recv_create_v3_t) -> NDIlib_recv_instance_t,
    ) -> Result<Self> {
        options.validate_runtime(ndi)?;
        let raw = options.to_raw()?;
        let instance = create(&ndi.api().core, &raw.raw);
        if instance.is_null() {
            return Err(Error::NativeOperationFailed { operation });
        }
        Ok(Self {
            core: Arc::new(ReceiverCore {
                instance,
                _ndi: ndi.clone(),
                source: options.source.clone(),
                capture_guard: RwLock::new(()),
                pending_status: AtomicBool::new(false),
                pending_source: AtomicBool::new(false),
                pending_unknown: Mutex::new(None),
                source_query_guard: Mutex::new(()),
                #[cfg(feature = "advanced")]
                _config: config,
            }),
            _mode: PhantomData,
        })
    }

    /// Capture **video** frames from this receiver.
    ///
    /// Returns a [`Capture`] view with two verbs, which both wait up to their
    /// timeout for a frame:
    ///
    /// - [`capture`](Capture::capture) copies the frame into an owned
    ///   [`VideoFrame`](crate::VideoFrame);
    /// - [`capture_ref`](Capture::capture_ref) borrows the SDK's buffer in
    ///   place.
    ///
    /// Video frames may carry per-row padding (e.g. RGBX/BGRX); see
    /// [`VideoFrame`](crate::VideoFrame) for how to read them correctly.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("CAMERA (Program)")))?;
    /// // An owned copy
    /// if let Some(frame) = receiver.video().capture(Duration::from_secs(5))? {
    ///     println!("{}x{}", frame.width(), frame.height());
    /// }
    ///
    /// // Zero-copy borrow
    /// if let Some(frame) = receiver.video().capture_ref(Duration::from_secs(1))? {
    ///     for plane in frame.planes() {
    ///         for row in plane.rows() {
    ///             println!("{} active bytes in place", row.len());
    ///         }
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "the Capture view does nothing until a capture verb is called"]
    pub fn video(&self) -> Capture<'_, VideoKind> {
        Capture::new(&self.core)
    }
}

impl<M: ReceiverMode> Receiver<M> {
    pub(crate) fn into_mode<T: ReceiverMode>(self) -> Receiver<T> {
        Receiver {
            core: self.core,
            _mode: PhantomData,
        }
    }

    /// Set native video buffering, in the range zero through two seconds.
    ///
    /// Positive fractional milliseconds round up. Values above two seconds are
    /// rejected instead of silently clamped. [`SettingChange::Unchanged`]
    /// means the delay already had this value. Delay is independent of audio
    /// buffering and stacks with FrameSync correction.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a delay above two seconds, and
    /// [`Error::UnsupportedCapability`] without
    /// [`RuntimeFeature::ReceiveDelay`].
    pub fn set_video_delay(&self, delay: Duration) -> Result<SettingChange> {
        let millis = receiver_delay_millis(delay)?;
        let table = self.core.delay()?;
        // SAFETY: the receiver instance is live for this borrow.
        let changed = unsafe { table.NDIlib_recv_set_video_delay(self.core.instance, millis) };
        Ok(SettingChange::from_sdk(changed))
    }

    /// Return the effective native video delay. Requires receiver-delay support.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] without
    /// [`RuntimeFeature::ReceiveDelay`], and [`Error::UnexpectedSdkResult`]
    /// when the SDK reports a delay outside 0 to 2000 ms.
    pub fn video_delay(&self) -> Result<Duration> {
        let table = self.core.delay()?;
        // SAFETY: the receiver instance is live for this borrow.
        receiver_delay(unsafe { table.NDIlib_recv_get_video_delay(self.core.instance) })
    }

    /// Set native audio buffering with the same rounding/range as [`Self::set_video_delay`].
    ///
    /// Equal audio and video delays preserve their relative timing; different
    /// values deliberately offset the streams.
    ///
    /// # Errors
    ///
    /// As for [`Self::set_video_delay`].
    pub fn set_audio_delay(&self, delay: Duration) -> Result<SettingChange> {
        let millis = receiver_delay_millis(delay)?;
        let table = self.core.delay()?;
        // SAFETY: the receiver instance is live for this borrow.
        let changed = unsafe { table.NDIlib_recv_set_audio_delay(self.core.instance, millis) };
        Ok(SettingChange::from_sdk(changed))
    }

    /// Return the effective native audio delay. Requires receiver-delay support.
    ///
    /// # Errors
    ///
    /// As for [`Self::video_delay`].
    pub fn audio_delay(&self) -> Result<Duration> {
        let table = self.core.delay()?;
        // SAFETY: the receiver instance is live for this borrow.
        receiver_delay(unsafe { table.NDIlib_recv_get_audio_delay(self.core.instance) })
    }

    /// Add validated metadata sent immediately and again on each connection.
    pub fn add_connection_metadata(&self, metadata: &MetadataFrame) -> Result<()> {
        let table = self.core.connection_metadata()?;
        let raw = metadata.to_raw();
        // SAFETY: The metadata C string remains live for the synchronous SDK call.
        unsafe { table.NDIlib_recv_add_connection_metadata(self.core.instance, &raw) };
        Ok(())
    }

    /// Clear metadata registered for subsequent receiver connections.
    pub fn clear_connection_metadata(&self) -> Result<()> {
        let table = self.core.connection_metadata()?;
        // SAFETY: the receiver instance is live for this borrow.
        unsafe { table.NDIlib_recv_clear_connection_metadata(self.core.instance) };
        Ok(())
    }

    /// Copy the optional device web-control URL. This does not open the URL.
    pub fn web_control_url(&self) -> Result<Option<String>> {
        // The query comes with the function that releases what it returns.
        let (web_control, strings) = self
            .core
            ._ndi
            .table::<(sys::ReceiverWebControl, sys::ReceiverStrings)>()?;
        let guard = RecvStringGuard {
            core: &self.core,
            // SAFETY: the receiver instance is live for this borrow.
            pointer: unsafe { web_control.NDIlib_recv_get_web_control(self.core.instance) },
            strings,
        };
        guard.copy()
    }

    /// Observe a change to the actual connected source name.
    ///
    /// `None` means no change within the wait budget; `Disconnected` is an
    /// observed change to no source. Concurrent pollers compete for notifications.
    /// This differs from [`Self::source`], the immutable original connection target.
    ///
    /// The SDK reports each change once, so a name that is not valid UTF-8 is
    /// not an error: invalid sequences are replaced with U+FFFD, as
    /// [`Finder`](crate::Finder) does for discovered source names.
    pub fn poll_source_name_change(&self, timeout: Duration) -> Result<Option<SourceNameChange>> {
        let deadline = Deadline::after(timeout)?;
        // The query comes with the function that releases what it returns.
        let (source_name, strings) = self
            .core
            ._ndi
            .table::<(sys::ReceiverSourceName, sys::ReceiverStrings)>()?;
        self.poll_source_name_change_until(source_name, strings, deadline)
    }

    fn poll_source_name_change_until(
        &self,
        source_name: &sys::ReceiverSourceName,
        strings: &sys::ReceiverStrings,
        deadline: Deadline,
    ) -> Result<Option<SourceNameChange>> {
        let Some(_query) = self.core.source_query_guard.try_lock_until(deadline.end()) else {
            return Ok(None);
        };
        let mut pointer = ptr::null();
        let remaining = deadline.remaining_ms();
        // SAFETY: the receiver instance is live for this borrow and `pointer` is
        // writable storage for the returned string.
        let changed = unsafe {
            source_name.NDIlib_recv_get_source_name(self.core.instance, &mut pointer, remaining)
        };
        let guard = RecvStringGuard {
            core: &self.core,
            pointer,
            strings,
        };
        if !changed {
            return Ok(None);
        }
        // The SDK reports a change only once, since the previous call, so a
        // strict decoding error here would lose the name for good.
        Ok(Some(match guard.copy_lossy()? {
            Some(name) => SourceNameChange::Connected(name),
            None => SourceNameChange::Disconnected,
        }))
    }

    /// Request a different native source without changing the original target.
    ///
    /// [`Self::reconnect`] continues to reconnect to [`Self::source`]. This call
    /// waits for the capture attempts in flight, as [`Self::reconnect`] does,
    /// then issues the request; success does not imply the source is already
    /// available. Existing FrameSync ownership remains.
    pub fn connect_to(&self, source: &Source) -> Result<()> {
        let raw = source.to_raw()?;
        self.core.connect(Some(&raw));
        Ok(())
    }

    /// Disconnect the native receiver while retaining its original reconnect
    /// target, once the capture attempts in flight have returned, as for
    /// [`Self::reconnect`].
    pub fn disconnect(&self) {
        self.core.connect(None);
    }

    /// Publish this receiver's program and preview tally to its source.
    ///
    /// The SDK keeps the state and sends it to every source the receiver
    /// connects to, so it can be set before a connection and survives a
    /// reconnect.
    pub fn set_tally(&self, tally: Tally) {
        // `Processing.NDI.Recv.h`: the flag is false when nothing is
        // connected, and the state is sent the moment something connects, so
        // it reports no failure.
        // SAFETY: the receiver instance is live for this borrow, and the
        // tally outlives the synchronous call.
        unsafe {
            self.core
                .sdk()
                .NDIlib_recv_set_tally(self.core.instance, &tally.to_sdk())
        };
    }

    /// Send metadata upstream, to the source this receiver is connected to.
    ///
    /// # Errors
    ///
    /// [`Error::NotConnected`] when the receiver is connected to no source,
    /// so nothing was sent.
    pub fn send_metadata(&self, metadata: &MetadataFrame) -> Result<()> {
        let raw = metadata.to_raw();
        // SAFETY: the receiver instance is live for this borrow, and the
        // borrowed C string outlives the synchronous call.
        let sent = unsafe {
            self.core
                .sdk()
                .NDIlib_recv_send_metadata(self.core.instance, &raw)
        };
        // `Processing.NDI.Recv.h`: false means nothing is connected.
        if sent {
            Ok(())
        } else {
            Err(Error::NotConnected)
        }
    }

    /// Control the pan, tilt, zoom, focus, white balance and exposure of the
    /// connected source.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::monitoring_preset(Source::new("CAMERA (Program)")))?;
    /// let ptz = receiver.ptz();
    /// if ptz.is_supported() {
    ///     ptz.zoom(0.5)?;
    ///     ptz.pan_tilt(-0.25, 0.1)?;
    ///     ptz.auto_focus()?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "the Ptz handle does nothing until a command is called"]
    pub fn ptz(&self) -> Ptz<'_> {
        Ptz { core: &self.core }
    }

    /// Check if the receiver is still connected to its source.
    ///
    /// Returns `true` if there is at least one active connection to the source,
    /// `false` otherwise. This can be used to detect when a source goes offline
    /// or becomes unavailable.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("CAMERA (Program)")))?;
    /// if receiver.is_connected() {
    ///     println!("Still connected to source");
    /// } else {
    ///     println!("Lost connection to source");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn is_connected(&self) -> bool {
        // SAFETY: the receiver instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.core
                .sdk()
                .NDIlib_recv_get_no_connections(self.core.instance)
                > 0
        }
    }

    /// Re-establish this receiver's connection to its original source,
    /// in place.
    ///
    /// Re-points the existing NDI receiver instance at the [`Source`] it
    /// was created for **without** destroying and recreating the
    /// receiver. This is intended for mid-session recovery: when a
    /// source's feed has gone silent (for example an encoder-bound
    /// NDI|HX camera that dropped its proxy stream under load), a
    /// reconnect forces a fresh connection attempt while avoiding the
    /// source-rediscovery round trip — and the discovery race — that
    /// tearing down and rebuilding the receiver would incur.
    ///
    /// Liveness can be observed via [`Self::is_connected`] and
    /// [`Self::connection_stats`] (`video_frames_received()` resuming its
    /// climb indicates frames are flowing again).
    ///
    /// # Thread safety
    ///
    /// `NDIlib_recv_connect` is the one receive call the SDK does *not* make
    /// safe to run concurrently with `NDIlib_recv_capture_v3` on the same
    /// instance. This method therefore takes the receiver's capture guard
    /// exclusively: it **blocks until every capture attempt in flight on this
    /// receiver returns**, and holds off any attempt that starts while it waits
    /// or runs. Every capture on the receiver — the [`Capture`] verbs,
    /// [`poll_event`](Self::poll_event), and the Advanced packet capture —
    /// holds the guard for one attempt of at most 100 ms, whatever its own
    /// timeout, so a reconnect waits about that long at most. A capture held
    /// off spends its own timeout waiting and captures again once the
    /// reconnect is done. Captures on the same receiver still run concurrently
    /// with each other; only a reconnect is exclusive. It is still a blocking
    /// call, so call it off the async runtime — `AsyncReceiver::reconnect`
    /// does this for you.
    ///
    /// # Errors
    ///
    /// Returns an error only if the stored [`Source`] cannot be
    /// re-marshalled to its FFI representation; `NDIlib_recv_connect`
    /// itself reports no status, so a returned `Ok` means the reconnect was
    /// *issued*, not that the feed has recovered — confirm recovery via
    /// [`Self::connection_stats`].
    pub fn reconnect(&self) -> Result<()> {
        // Marshal before locking; this touches only `self.core.source`, not the
        // instance, so it need not hold off captures.
        let raw = self.core.source.to_raw()?;
        self.core.connect(Some(&raw));
        Ok(())
    }

    /// Get the original source target specified when this receiver was created.
    ///
    /// Returns a reference to the [`Source`] that was specified when creating
    /// this receiver. [`Self::reconnect`] returns to this target even after
    /// [`Self::connect_to`] or [`Self::disconnect`]. Actual native source changes
    /// can be observed with [`Self::poll_source_name_change`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("CAMERA (Program)")))?;
    /// let source = receiver.source();
    /// println!("Original target: {name}", name = source.name());
    /// # Ok(())
    /// # }
    /// ```
    pub fn source(&self) -> &Source {
        &self.core.source
    }

    /// Get connection and performance statistics for this receiver.
    ///
    /// Provides detailed statistics including:
    /// - Number of active connections
    /// - Total frames received (video, audio, metadata)
    /// - Dropped frames (video, audio, metadata)
    /// - Queued frames waiting to be processed
    ///
    /// This is useful for monitoring receiver health and diagnosing
    /// performance issues in production applications.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("CAMERA (Program)")))?;
    /// let stats = receiver.connection_stats();
    /// println!("Connections: {connections}", connections = stats.connections());
    /// println!("Video frames: {received} (dropped: {dropped})",
    ///          received = stats.video_frames_received(),
    ///          dropped = stats.video_frames_dropped());
    /// println!("Frame drop rate: {rate:.2}%",
    ///          rate = stats.video_drop_percentage());
    /// # Ok(())
    /// # }
    /// ```
    pub fn connection_stats(&self) -> ConnectionStats {
        // SAFETY: the receiver instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        let connections = unsafe {
            self.core
                .sdk()
                .NDIlib_recv_get_no_connections(self.core.instance)
        };

        let mut total = NDIlib_recv_performance_t::default();
        let mut dropped = NDIlib_recv_performance_t::default();
        // SAFETY: the receiver instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.core.sdk().NDIlib_recv_get_performance(
                self.core.instance,
                &mut total,
                &mut dropped,
            );
        }

        let mut queue = NDIlib_recv_queue_t::default();
        // SAFETY: the receiver instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.core
                .sdk()
                .NDIlib_recv_get_queue(self.core.instance, &mut queue);
        }

        ConnectionStats {
            connections: connections.max(0) as u32,
            video_frames_received: total.video_frames.max(0) as u64,
            audio_frames_received: total.audio_frames.max(0) as u64,
            metadata_frames_received: total.metadata_frames.max(0) as u64,
            video_frames_dropped: dropped.video_frames.max(0) as u64,
            audio_frames_dropped: dropped.audio_frames.max(0) as u64,
            metadata_frames_dropped: dropped.metadata_frames.max(0) as u64,
            video_frames_queued: queue.video_frames.max(0) as u32,
            audio_frames_queued: queue.audio_frames.max(0) as u32,
            metadata_frames_queued: queue.metadata_frames.max(0) as u32,
        }
    }
}

impl<M: CaptureMode> Receiver<M> {
    /// Wait up to `timeout` for a status or source change, or return `None`
    /// when none came.
    ///
    /// Events are coalesced invalidations, including those that media capture
    /// passed over and kept, not an ordered history or broadcast stream.
    /// Concurrent pollers compete to consume a pending one. The new source
    /// name is read with the SDK's separate [`Self::poll_source_name_change`]
    /// API.
    ///
    /// The timeout is spent as the [`Capture`] verbs spend it: in attempts of
    /// at most 100 ms, each of which first takes a pending event and otherwise
    /// waits in the SDK, under the receiver's shared capture guard, for the
    /// rest of the attempt. The SDK's early returns are waited over, so `None`
    /// comes only once the whole timeout is spent, and [`Duration::ZERO`]
    /// makes one attempt without waiting. An event that a concurrent capture
    /// keeps is returned by the next attempt, and a
    /// [`reconnect`](Self::reconnect) waits for one attempt at most.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`]
    /// (~49.7 days), and [`Error::CaptureFailed`] when the SDK reports a lost
    /// connection.
    pub fn poll_event(&self, timeout: Duration) -> Result<Option<ReceiverEvent>> {
        self.core.poll_event(timeout)
    }

    /// Capture **audio** frames from this receiver.
    ///
    /// Returns a [`Capture`] view; see [`Receiver::video`] for its two verbs,
    /// [`capture`](Capture::capture) and [`capture_ref`](Capture::capture_ref).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("CAMERA (Program)")))?;
    /// if let Some(audio) = receiver.audio().capture_ref(Duration::from_secs(1))? {
    ///     println!("{} channels, {} samples", audio.num_channels(), audio.num_samples());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "the Capture view does nothing until a capture verb is called"]
    pub fn audio(&self) -> Capture<'_, AudioKind> {
        Capture::new(&self.core)
    }

    /// Capture **metadata** frames from this receiver.
    ///
    /// Returns a [`Capture`] view; see [`Receiver::video`] for its two verbs,
    /// [`capture`](Capture::capture) and [`capture_ref`](Capture::capture_ref).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("CAMERA (Program)")))?;
    /// if let Some(meta) = receiver.metadata().capture(Duration::from_millis(100))? {
    ///     println!("metadata: {}", meta.data());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "the Capture view does nothing until a capture verb is called"]
    pub fn metadata(&self) -> Capture<'_, MetadataKind> {
        Capture::new(&self.core)
    }
}

impl ReceiverCore {
    /// The entry points of the runtime this receiver was created through.
    pub(crate) fn sdk(&self) -> &'static sys::Core {
        &self._ndi.api().core
    }

    fn delay(&self) -> Result<&'static sys::ReceiveDelay> {
        self._ndi.table()
    }

    fn connection_metadata(&self) -> Result<&'static sys::ReceiverConnectionMetadata> {
        self._ndi.table()
    }

    /// Point the native receiver at `source`, or at no source, once no capture
    /// is in flight.
    ///
    /// The capture guard is held exclusively for the call: it waits for the
    /// capture attempts in flight to return and holds off new ones, so
    /// `NDIlib_recv_connect` never overlaps `NDIlib_recv_capture_v3` on this
    /// instance, the one combination `Processing.NDI.Recv.h` does not make safe.
    /// Each attempt holds the guard for one [`Attempt`] at most, so this waits
    /// about one poll interval at most.
    fn connect(&self, source: Option<&RawSource>) {
        let _exclusive = self.capture_guard.write();
        let source = source.map_or(ptr::null(), |raw| ptr::from_ref(&raw.raw));
        // SAFETY: the receiver instance is live for this borrow. `source` is
        // null or points into a `RawSource`, which owns the strings its
        // descriptor points to and outlives this synchronous call, during
        // which the SDK copies the descriptor.
        unsafe { self.sdk().NDIlib_recv_connect(self.instance, source) };
    }

    /// Take the shared capture guard for `attempt`, waiting for it no longer
    /// than the attempt lasts: `None` when a connect or reconnect held it, or
    /// waited for it, until then.
    ///
    /// This is the only way to make a [`CaptureLock`], and so to call
    /// `NDIlib_recv_capture_v3` on this instance.
    pub(crate) fn capture_lock(&self, attempt: Attempt) -> Option<CaptureLock<'_>> {
        let shared = self.capture_guard.try_read_until(attempt.0.end())?;
        Some(CaptureLock {
            core: self,
            attempt,
            _shared: shared,
        })
    }

    /// Hold the lock a reconnect holds, so that a test can contend with capture.
    #[cfg(all(test, feature = "advanced"))]
    pub(crate) fn reconnect_lock(&self) -> parking_lot::RwLockWriteGuard<'_, ()> {
        self.capture_guard.write()
    }

    /// One SDK capture of kind `K` within `attempt`.
    ///
    /// Holds the shared capture guard for one `NDIlib_recv_capture_v3` call,
    /// so it runs concurrently with other captures but never overlaps a
    /// [`reconnect`](Receiver::reconnect). `None` when the guard was not free
    /// in time, or the SDK returned without a frame of this kind; a
    /// notification it returned instead is kept for [`Receiver::poll_event`].
    fn poll_ref_until<K: CaptureKind>(&self, attempt: Attempt) -> Result<Option<K::Ref<'_>>> {
        let Some(lock) = self.capture_lock(attempt) else {
            return Ok(None);
        };
        let result = capture_raw::<K>(&lock)?;
        drop(lock);
        self.capture_result::<K>(result)
    }

    fn capture_result<'rx, K: CaptureKind>(
        &'rx self,
        result: CaptureResult<'rx, K>,
    ) -> Result<Option<K::Ref<'rx>>> {
        match result {
            CaptureResult::Frame(guard) => {
                // Validation (e.g. FourCC) happens during ref construction.
                // SAFETY: the guard owns a frame this capture reported as `K`'s kind.
                let frame_ref = unsafe { K::make_ref(guard)? };
                Ok(Some(frame_ref))
            }
            CaptureResult::Notification(notification) => {
                self.retain(notification);
                Ok(None)
            }
        }
    }

    /// The one capture loop, behind [`Capture::capture_ref`] and every owned
    /// capture: SDK captures under the default [`RetryPolicy`] until one
    /// delivers a frame of kind `K` or `deadline` is spent.
    pub(crate) fn capture_ref_until<K: CaptureKind>(
        &self,
        deadline: Deadline,
    ) -> Result<Option<K::Ref<'_>>> {
        capture_within(deadline, |attempt| self.poll_ref_until::<K>(attempt))
    }

    /// [`Self::capture_ref_until`], copied into an owned frame: behind
    /// [`Capture::capture`] and the async receivers' capture.
    pub(crate) fn capture_until<K: CaptureKind>(
        &self,
        deadline: Deadline,
    ) -> Result<Option<K::Owned>> {
        self.capture_ref_until::<K>(deadline)?
            .as_ref()
            .map(K::ref_to_owned)
            .transpose()
    }

    fn take_pending_status(&self) -> Option<ReceiverStatus> {
        // Atomic swap linearizes competing pollers with capture publication.
        // A store after this take remains pending; no load/store pair can erase it.
        self.pending_status
            .swap(false, Ordering::AcqRel)
            .then(|| self.current_status())
    }

    fn current_status(&self) -> ReceiverStatus {
        ReceiverStatus {
            // SAFETY: the receiver instance is live for this borrow, and every pointer
            // passed outlives the synchronous call.
            connections: unsafe {
                self.sdk()
                    .NDIlib_recv_get_no_connections(self.instance)
                    .max(0) as u32
            },
        }
    }

    /// Keep a notification a capture reported for [`Self::poll_event`].
    fn retain(&self, notification: Notification) {
        match notification {
            Notification::None => {}
            Notification::StatusChanged => self.pending_status.store(true, Ordering::Release),
            Notification::SourceChanged => self.pending_source.store(true, Ordering::Release),
            Notification::Unknown(value) => *self.pending_unknown.lock() = Some(value),
        }
    }

    fn take_pending_event(&self) -> Option<ReceiverEvent> {
        if let Some(status) = self.take_pending_status() {
            return Some(ReceiverEvent::StatusChanged(status));
        }
        if self.pending_source.swap(false, Ordering::AcqRel) {
            return Some(ReceiverEvent::SourceChanged);
        }
        self.pending_unknown
            .lock()
            .take()
            .map(ReceiverEvent::Unknown)
    }

    /// [`Receiver::poll_event`]: event attempts in the one capture loop.
    fn poll_event(&self, timeout: Duration) -> Result<Option<ReceiverEvent>> {
        capture_within(Deadline::after(timeout)?, |attempt| {
            self.poll_event_once(attempt)
        })
    }

    /// One attempt of [`Self::poll_event`]: a pending event, or what one SDK
    /// capture without media slots reports within `attempt`.
    ///
    /// A pending event is taken before waiting for the guard, so that it is
    /// returned even while a reconnect holds the guard, and again once the
    /// guard is held, so that one a capture kept meanwhile spares the wait.
    fn poll_event_once(&self, attempt: Attempt) -> Result<Option<ReceiverEvent>> {
        if let Some(event) = self.take_pending_event() {
            return Ok(Some(event));
        }
        let Some(lock) = self.capture_lock(attempt) else {
            return Ok(self.take_pending_event());
        };
        if let Some(event) = self.take_pending_event() {
            return Ok(Some(event));
        }
        // Null media slots request notifications only; no frame allocation escapes.
        let frame_type = lock.capture(None, None, None);
        drop(lock);
        self.retain(Notification::classify(frame_type)?);
        Ok(self.take_pending_event())
    }
}

/// Proof that the shared capture guard of one receiver is held, for one
/// [`Attempt`]: the only way to call `NDIlib_recv_capture_v3`.
///
/// Only [`ReceiverCore::capture_lock`] makes one, and every function that
/// reaches the native capture takes one by reference, so no capture can
/// overlap `NDIlib_recv_connect` on the same instance, which takes the guard
/// exclusively and which `Processing.NDI.Recv.h` does not make safe to run
/// beside a capture. The lock also fixes the receiver the capture is made on
/// and how long it may wait: what is left of its attempt.
pub(crate) struct CaptureLock<'rx> {
    core: &'rx ReceiverCore,
    attempt: Attempt,
    _shared: RwLockReadGuard<'rx, ()>,
}

impl<'rx> CaptureLock<'rx> {
    /// The runtime the receiver was created through, which frees the frames
    /// a capture under this lock delivers.
    pub(crate) fn runtime(&self) -> &'rx Ndi {
        &self.core._ndi
    }

    /// The receiver instance a capture under this lock is made on.
    pub(crate) fn instance(&self) -> NDIlib_recv_instance_t {
        self.core.instance
    }

    /// The receiver's one `NDIlib_recv_capture_v3` call: wait for what is
    /// left of this lock's attempt for a frame of a kind whose descriptor is
    /// given, or for a notification, and return the frame type the SDK
    /// reported.
    ///
    /// The SDK fills only the descriptor of the kind it returns, and only
    /// when that descriptor was given; the caller frees such a frame through
    /// the matching `NDIlib_recv_free_*` function of [`Self::runtime`].
    pub(crate) fn capture(
        &self,
        video: Option<&mut NDIlib_video_frame_v2_t>,
        audio: Option<&mut NDIlib_audio_frame_v3_t>,
        metadata: Option<&mut NDIlib_metadata_frame_t>,
    ) -> NDIlib_frame_type_e {
        #[cfg(test)]
        native_capture_calls::record();
        // SAFETY: the receiver instance is live for 'rx, which this lock
        // borrows it for. Each descriptor is null or exclusively borrowed,
        // writable storage that outlives the synchronous call. The lock holds
        // the shared capture guard, so NDIlib_recv_connect, which takes it
        // exclusively, cannot run on this instance during the call.
        unsafe {
            self.core.sdk().NDIlib_recv_capture_v3(
                self.core.instance,
                video.map_or(ptr::null_mut(), ptr::from_mut),
                audio.map_or(ptr::null_mut(), ptr::from_mut),
                metadata.map_or(ptr::null_mut(), ptr::from_mut),
                self.attempt.0.remaining_ms(),
            )
        }
    }
}

/// Counts native capture calls on the calling thread, so that tests can tell
/// a capture that was skipped from one that found nothing.
#[cfg(test)]
pub(crate) mod native_capture_calls {
    use std::cell::Cell;
    std::thread_local! {
        static CALLS: Cell<usize> = const { Cell::new(0) };
    }
    pub(crate) fn record() {
        CALLS.with(|calls| calls.set(calls.get() + 1));
    }
    pub(crate) fn take() -> usize {
        CALLS.with(|calls| calls.replace(0))
    }
}

/// A typed view over a [`Receiver`] for capturing frames of one kind.
///
/// Created by [`Receiver::video`], [`Receiver::audio`], and
/// [`Receiver::metadata`]. The view is a cheap borrow of the receiver and does
/// nothing on its own; call one of its verbs to capture:
///
/// - [`capture`](Self::capture) copies the frame into an owned value that
///   can be kept, sent or moved to another thread.
/// - [`capture_ref`](Self::capture_ref) borrows the SDK's buffer in place,
///   with no allocation and no copy. It is the one for in-place processing:
///   for a 1920×1080 BGRA frame it avoids ~8.3 MB of copying per frame
///   (~475 MB/s at 60 fps). Convert it with `try_to_owned` when you need to
///   keep or send it.
///
/// Both wait up to their timeout for a frame. The SDK returns from a capture
/// early, without a frame, while a new connection synchronizes and whenever
/// it reports a status or source change; both verbs capture again until a
/// frame arrives, and return `Ok(None)` only once the whole timeout is spent.
/// A status or source change they pass over is kept for
/// [`Receiver::poll_event`].
///
/// Each attempt waits at most 100 ms, and every SDK call holds a shared
/// capture guard for one attempt, so captures of different kinds run
/// concurrently with each other but never overlap a [`Receiver::reconnect`],
/// which waits for one attempt at most. Each timeout budget starts when the
/// verb is called and includes waiting for that guard. A zero timeout makes
/// one attempt and never waits for the guard; if it is unavailable, no SDK
/// capture is attempted. SDK calls and owned-frame copying may finish after
/// the budget expires, so the timeout is not a strict wall-clock completion
/// guarantee.
///
/// Only the ordinary receiver mode exposes a video view. FrameSync owns video
/// capture until [`crate::FrameSync::into_receiver`] restores the ordinary mode.
/// Audio and metadata views exist in each [`CaptureMode`].
pub struct Capture<'rx, K: CaptureKind> {
    rx: &'rx ReceiverCore,
    _kind: PhantomData<K>,
}

impl<'rx, K: CaptureKind> Capture<'rx, K> {
    fn new(rx: &'rx ReceiverCore) -> Self {
        Self {
            rx,
            _kind: PhantomData,
        }
    }

    /// Wait up to `timeout` for a frame and return an owned copy of it, or
    /// `None` when none arrived in time.
    ///
    /// See [`Capture`] for how the timeout is spent; [`Duration::ZERO`]
    /// makes one attempt without waiting.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`]
    /// (~49.7 days), [`Error::CaptureFailed`] when the SDK reports a lost
    /// connection, and the errors of validating and copying the frame.
    pub fn capture(&self, timeout: Duration) -> Result<Option<K::Owned>> {
        self.rx.capture_until::<K>(Deadline::after(timeout)?)
    }

    /// Wait up to `timeout` for a frame and borrow the SDK's buffer in
    /// place, or return `None` when none arrived in time.
    ///
    /// The returned reference borrows the [`Receiver`] (not this temporary
    /// view), so it may outlive the `Capture` while keeping the receiver
    /// borrowed. See [`Capture`] for how the timeout is spent.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`]
    /// (~49.7 days), [`Error::CaptureFailed`] when the SDK reports a lost
    /// connection, and the errors of validating the frame.
    pub fn capture_ref(&self, timeout: Duration) -> Result<Option<K::Ref<'rx>>> {
        // Bind through `'rx` so the returned borrow is tied to the receiver, not
        // to this short-lived view.
        let rx: &'rx ReceiverCore = self.rx;
        rx.capture_ref_until::<K>(Deadline::after(timeout)?)
    }
}

/// Pan, tilt, zoom, focus, white balance and exposure control of the source a
/// [`Receiver`] is connected to, from [`Receiver::ptz`].
///
/// A value outside its range, or not finite, is refused with
/// [`Error::InvalidInput`] before anything is sent. A command the SDK does
/// not send is reported as [`Error::NativeOperationFailed`].
#[derive(Clone, Copy)]
pub struct Ptz<'rx> {
    core: &'rx ReceiverCore,
}

impl Ptz<'_> {
    /// Whether the connected source accepts PTZ commands.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        // SAFETY: the receiver instance is live for the handle's borrow.
        unsafe {
            self.core
                .sdk()
                .NDIlib_recv_ptz_is_supported(self.core.instance)
        }
    }

    /// Recall preset `0..=99`, moving at `speed` within `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn recall_preset(&self, preset: u8, speed: f32) -> Result<()> {
        let preset = validate_ptz_preset(preset)?;
        validate_ptz_value("speed", speed, 0.0, 1.0)?;
        ptz_command!(self, NDIlib_recv_ptz_recall_preset, preset, speed)
    }

    /// Store the current camera settings as preset `0..=99`.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn store_preset(&self, preset: u8) -> Result<()> {
        let preset = validate_ptz_preset(preset)?;
        ptz_command!(self, NDIlib_recv_ptz_store_preset, preset)
    }

    /// Set the absolute zoom within `0.0..=1.0`: zero is zoomed in, one is
    /// zoomed out.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn zoom(&self, zoom: f32) -> Result<()> {
        validate_ptz_value("zoom", zoom, 0.0, 1.0)?;
        ptz_command!(self, NDIlib_recv_ptz_zoom, zoom)
    }

    /// Set the zoom speed within `-1.0..=1.0`: negative zooms out, positive
    /// zooms in, zero stops.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn zoom_speed(&self, speed: f32) -> Result<()> {
        validate_ptz_value("zoom speed", speed, -1.0, 1.0)?;
        ptz_command!(self, NDIlib_recv_ptz_zoom_speed, speed)
    }

    /// Set the absolute pan and tilt within `-1.0..=1.0`: pan increases from
    /// left to right, tilt from bottom to top.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn pan_tilt(&self, pan: f32, tilt: f32) -> Result<()> {
        validate_ptz_value("pan", pan, -1.0, 1.0)?;
        validate_ptz_value("tilt", tilt, -1.0, 1.0)?;
        ptz_command!(self, NDIlib_recv_ptz_pan_tilt, pan, tilt)
    }

    /// Set the pan and tilt speeds within `-1.0..=1.0`: positive pans left
    /// and tilts up, zero stops.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn pan_tilt_speed(&self, pan_speed: f32, tilt_speed: f32) -> Result<()> {
        validate_ptz_value("pan speed", pan_speed, -1.0, 1.0)?;
        validate_ptz_value("tilt speed", tilt_speed, -1.0, 1.0)?;
        ptz_command!(self, NDIlib_recv_ptz_pan_tilt_speed, pan_speed, tilt_speed)
    }

    /// Focus automatically.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn auto_focus(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_auto_focus)
    }

    /// Set the absolute focus within `0.0..=1.0`: zero focuses at infinity,
    /// one as close as possible.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn focus(&self, focus: f32) -> Result<()> {
        validate_ptz_value("focus", focus, 0.0, 1.0)?;
        ptz_command!(self, NDIlib_recv_ptz_focus, focus)
    }

    /// Set the focus speed within `-1.0..=1.0`: negative focuses farther,
    /// positive nearer, zero stops.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn focus_speed(&self, speed: f32) -> Result<()> {
        validate_ptz_value("focus speed", speed, -1.0, 1.0)?;
        ptz_command!(self, NDIlib_recv_ptz_focus_speed, speed)
    }

    /// Balance white automatically.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn white_balance_auto(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_white_balance_auto)
    }

    /// Select the indoor white-balance preset.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn white_balance_indoor(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_white_balance_indoor)
    }

    /// Select the outdoor white-balance preset.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn white_balance_outdoor(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_white_balance_outdoor)
    }

    /// Balance white once, from the current image.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn white_balance_oneshot(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_white_balance_oneshot)
    }

    /// Set the red and blue white-balance levels within `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn white_balance_manual(&self, red: f32, blue: f32) -> Result<()> {
        validate_ptz_value("red", red, 0.0, 1.0)?;
        validate_ptz_value("blue", blue, 0.0, 1.0)?;
        ptz_command!(self, NDIlib_recv_ptz_white_balance_manual, red, blue)
    }

    /// Set the exposure automatically.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn exposure_auto(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_exposure_auto)
    }

    /// Set the exposure level within `0.0..=1.0`, from dark to light.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn exposure_manual(&self, level: f32) -> Result<()> {
        validate_ptz_value("exposure level", level, 0.0, 1.0)?;
        ptz_command!(self, NDIlib_recv_ptz_exposure_manual, level)
    }

    /// Set the iris, gain and shutter speed within `0.0..=1.0`: iris and gain
    /// from dark to light, shutter speed from slow to fast.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    pub fn exposure_manual_detailed(&self, iris: f32, gain: f32, shutter_speed: f32) -> Result<()> {
        validate_ptz_value("iris", iris, 0.0, 1.0)?;
        validate_ptz_value("gain", gain, 0.0, 1.0)?;
        validate_ptz_value("shutter speed", shutter_speed, 0.0, 1.0)?;
        ptz_command!(
            self,
            NDIlib_recv_ptz_exposure_manual_v2,
            iris,
            gain,
            shutter_speed
        )
    }
}

impl Drop for ReceiverCore {
    fn drop(&mut self) {
        // SAFETY: the receiver instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.sdk().NDIlib_recv_destroy(self.instance);
        }
        #[cfg(test)]
        lifecycle_test_events::record("receiver");
    }
}

/// # Safety
///
/// The NDI 6 SDK documentation explicitly states that recv operations are thread-safe.
/// `NDIlib_recv_capture_v3` and related functions use internal synchronization.
/// The Receiver struct only holds an opaque pointer returned by the SDK, and the SDK
/// guarantees that this pointer can be safely moved between threads.
unsafe impl Send for ReceiverCore {}

/// # Safety
///
/// The NDI 6 SDK documentation guarantees that `NDIlib_recv_capture_v3` is internally
/// synchronized and can be called concurrently from multiple threads. This is explicitly
/// mentioned in the SDK manual's thread safety section. The capture verbs (via
/// [`Receiver::video`], [`Receiver::audio`], and [`Receiver::metadata`]) can be
/// safely called from multiple threads simultaneously. The wrapper's capture
/// guard excludes reconnect calls from receiver capture calls. Receiver modes
/// prevent raw video capture while a FrameSync owns this core.
/// Borrowed frame guards retain the receiver borrow until they free their buffers.
unsafe impl Sync for ReceiverCore {}

/// A source-name change observed by the native receiver.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SourceNameChange {
    /// The receiver is now connected to the named source.
    Connected(String),
    /// The receiver is no longer connected to any source.
    Disconnected,
}

/// A coalesced receiver invalidation, not an ordered event history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReceiverEvent {
    /// The connection or the source's capabilities changed.
    StatusChanged(ReceiverStatus),
    /// The connected source changed; poll its name for the new value.
    SourceChanged,
    /// An unknown non-media event value; no unknown payload is exposed.
    Unknown(u32),
}

/// What a status-change notification reports: the current connection count.
///
/// The notification may also mean that the source's capabilities or web
/// control changed; the SDK does not say which. Tally is published upstream
/// through [`Receiver::set_tally`], rather than received here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ReceiverStatus {
    connections: u32,
}

impl ReceiverStatus {
    /// The number of sources the receiver is connected to: normally zero or
    /// one.
    #[must_use]
    pub const fn connections(self) -> u32 {
        self.connections
    }
}

/// Whether a source is shown on program output, on preview, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Tally {
    on_program: bool,
    on_preview: bool,
}

impl Tally {
    /// A tally state: on program (live) output, on preview, both or neither.
    #[must_use]
    pub const fn new(on_program: bool, on_preview: bool) -> Self {
        Self {
            on_program,
            on_preview,
        }
    }

    /// Whether the source is on program (live) output.
    #[must_use]
    pub const fn on_program(self) -> bool {
        self.on_program
    }

    /// Whether the source is on preview output.
    #[must_use]
    pub const fn on_preview(self) -> bool {
        self.on_preview
    }

    pub(crate) const fn to_sdk(self) -> NDIlib_tally_t {
        NDIlib_tally_t {
            on_program: self.on_program,
            on_preview: self.on_preview,
        }
    }
}

/// Whether a setting changed.
///
/// Returned by the setters whose SDK call reports whether it changed anything.
/// Callers that only want the setting in effect can ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingChange {
    /// The setting changed.
    Applied,
    /// The setting did not change: it was already in effect, or the SDK did
    /// not apply it. Each setter says when the SDK does not.
    Unchanged,
}

impl SettingChange {
    /// The change an SDK call reported with `changed`.
    pub(crate) const fn from_sdk(changed: bool) -> Self {
        if changed {
            Self::Applied
        } else {
            Self::Unchanged
        }
    }
}

/// Connection and performance statistics for a receiver.
///
/// Connection count, frame counts, drop rates and queue depths, from
/// [`Receiver::connection_stats`]. Useful for monitoring and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ConnectionStats {
    connections: u32,
    video_frames_received: u64,
    audio_frames_received: u64,
    metadata_frames_received: u64,
    video_frames_dropped: u64,
    audio_frames_dropped: u64,
    metadata_frames_dropped: u64,
    video_frames_queued: u32,
    audio_frames_queued: u32,
    metadata_frames_queued: u32,
}

impl ConnectionStats {
    /// The number of active connections.
    #[must_use]
    pub const fn connections(&self) -> u32 {
        self.connections
    }

    /// The total number of video frames received.
    #[must_use]
    pub const fn video_frames_received(&self) -> u64 {
        self.video_frames_received
    }

    /// The total number of audio frames received.
    #[must_use]
    pub const fn audio_frames_received(&self) -> u64 {
        self.audio_frames_received
    }

    /// The total number of metadata frames received.
    #[must_use]
    pub const fn metadata_frames_received(&self) -> u64 {
        self.metadata_frames_received
    }

    /// The number of video frames dropped, because a buffer overflowed or
    /// processing fell behind.
    #[must_use]
    pub const fn video_frames_dropped(&self) -> u64 {
        self.video_frames_dropped
    }

    /// The number of audio frames dropped.
    #[must_use]
    pub const fn audio_frames_dropped(&self) -> u64 {
        self.audio_frames_dropped
    }

    /// The number of metadata frames dropped.
    #[must_use]
    pub const fn metadata_frames_dropped(&self) -> u64 {
        self.metadata_frames_dropped
    }

    /// The number of video frames queued, waiting to be captured.
    #[must_use]
    pub const fn video_frames_queued(&self) -> u32 {
        self.video_frames_queued
    }

    /// The number of audio frames queued.
    #[must_use]
    pub const fn audio_frames_queued(&self) -> u32 {
        self.audio_frames_queued
    }

    /// The number of metadata frames queued.
    #[must_use]
    pub const fn metadata_frames_queued(&self) -> u32 {
        self.metadata_frames_queued
    }

    /// The percentage of video frames dropped, out of those received and
    /// dropped; 0.0 when both counts are zero.
    #[must_use]
    pub fn video_drop_percentage(&self) -> f64 {
        drop_percentage(self.video_frames_received, self.video_frames_dropped)
    }

    /// The percentage of audio frames dropped, out of those received and
    /// dropped; 0.0 when both counts are zero.
    #[must_use]
    pub fn audio_drop_percentage(&self) -> f64 {
        drop_percentage(self.audio_frames_received, self.audio_frames_dropped)
    }

    /// The percentage of metadata frames dropped, out of those received and
    /// dropped; 0.0 when both counts are zero.
    #[must_use]
    pub fn metadata_drop_percentage(&self) -> f64 {
        drop_percentage(self.metadata_frames_received, self.metadata_frames_dropped)
    }

    /// Whether there is at least one active connection.
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connections > 0
    }
}

fn drop_percentage(received: u64, dropped: u64) -> f64 {
    let total = u128::from(received) + u128::from(dropped);
    if total == 0 {
        0.0
    } else {
        (dropped as f64 / total as f64) * 100.0
    }
}

#[cfg(test)]
pub(crate) mod lifecycle_test_events {
    use std::cell::RefCell;
    std::thread_local! {
        static EVENTS: RefCell<Option<Vec<&'static str>>> = const { RefCell::new(None) };
    }
    pub(crate) fn record(event: &'static str) {
        EVENTS.with(|events| {
            if let Some(events) = &mut *events.borrow_mut() {
                events.push(event);
            }
        });
    }
    pub(crate) fn track(action: impl FnOnce()) -> Vec<&'static str> {
        EVENTS.with(|events| *events.borrow_mut() = Some(Vec::new()));
        action();
        EVENTS.with(|events| events.borrow_mut().take().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::Cell;

    #[test]
    fn receive_delay_rounds_up_and_rejects_out_of_range_values() {
        for (duration, millis) in [
            (Duration::ZERO, 0),
            (Duration::from_nanos(1), 1),
            (Duration::from_nanos(999_999), 1),
            (Duration::from_millis(1), 1),
            (Duration::from_nanos(1_000_001), 2),
            (Duration::from_nanos(1_999_999_999), 2000),
            (Duration::from_secs(2), 2000),
        ] {
            assert_eq!(receiver_delay_millis(duration).unwrap(), millis);
        }
        assert!(receiver_delay_millis(Duration::from_secs(2) + Duration::from_nanos(1)).is_err());
        assert!(receiver_delay_millis(Duration::MAX).is_err());
    }

    #[test]
    fn receive_64_modes_require_the_selected_runtime_before_native_creation() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let best = ReceiverOptions::new(test_source()).color_format(ReceiverColorFormat::Best444);
        assert_eq!(best.to_raw().unwrap().raw.color_format, 102);
        assert_eq!(
            best.validate_runtime(&ndi).is_ok(),
            ndi.require_feature(RuntimeFeature::ReceiveBest444).is_ok()
        );
        for (bandwidth, value) in [
            (ReceiverBandwidth::VideoOnlyLowest, 1),
            (ReceiverBandwidth::VideoOnlyHighest, 101),
        ] {
            let options = ReceiverOptions::new(test_source()).bandwidth(bandwidth);
            assert_eq!(options.to_raw().unwrap().raw.bandwidth, value);
            assert_eq!(
                options.validate_runtime(&ndi).is_ok(),
                ndi.require_feature(RuntimeFeature::ReceiveVideoOnly)
                    .is_ok()
            );
        }
        assert!(unconnected().validate_runtime(&ndi).is_ok());
    }

    #[test]
    fn media_source_and_unknown_events_are_retained_for_poll_event() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        for _ in 0..3 {
            assert!(receiver
                .core
                .capture_result::<VideoKind>(CaptureResult::Notification(
                    Notification::SourceChanged
                ))
                .unwrap()
                .is_none());
        }
        receiver.core.retain(Notification::classify(987).unwrap());
        assert!(matches!(
            receiver.core.take_pending_event(),
            Some(ReceiverEvent::SourceChanged)
        ));
        assert!(matches!(
            receiver.core.take_pending_event(),
            Some(ReceiverEvent::Unknown(987))
        ));
        assert!(receiver.core.take_pending_event().is_none());
        assert!(receiver
            .core
            .capture_result::<VideoKind>(CaptureResult::Notification(Notification::Unknown(988)))
            .unwrap()
            .is_none());
        receiver.core.retain(
            Notification::classify(NDIlib_frame_type_e_NDIlib_frame_type_source_change).unwrap(),
        );
        assert!(matches!(
            receiver.core.take_pending_event(),
            Some(ReceiverEvent::SourceChanged)
        ));
        assert!(matches!(
            receiver.core.take_pending_event(),
            Some(ReceiverEvent::Unknown(988))
        ));
    }

    #[test]
    fn receive_delay_native_changed_flag_and_capability_failure() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        if ndi.require_feature(RuntimeFeature::ReceiveDelay).is_ok() {
            assert_eq!(receiver.video_delay().unwrap(), Duration::ZERO);
            assert_eq!(receiver.audio_delay().unwrap(), Duration::ZERO);
            use SettingChange::{Applied, Unchanged};
            assert_eq!(receiver.set_video_delay(Duration::ZERO).unwrap(), Unchanged);
            assert_eq!(
                receiver.set_video_delay(Duration::from_nanos(1)).unwrap(),
                Applied
            );
            assert_eq!(receiver.video_delay().unwrap(), Duration::from_millis(1));
            assert_eq!(
                receiver.set_video_delay(Duration::from_millis(1)).unwrap(),
                Unchanged
            );
            assert_eq!(receiver.audio_delay().unwrap(), Duration::ZERO);
            assert_eq!(
                receiver.set_audio_delay(Duration::from_secs(2)).unwrap(),
                Applied
            );
            assert_eq!(receiver.audio_delay().unwrap(), Duration::from_secs(2));
            assert_eq!(
                receiver.set_audio_delay(Duration::from_secs(2)).unwrap(),
                Unchanged
            );
            assert!(receiver.set_audio_delay(Duration::from_secs(3)).is_err());
            assert_eq!(receiver.audio_delay().unwrap(), Duration::from_secs(2));
        } else {
            assert!(matches!(
                receiver.video_delay(),
                Err(Error::UnsupportedCapability { .. })
            ));
            assert!(matches!(
                receiver.audio_delay(),
                Err(Error::UnsupportedCapability { .. })
            ));
            assert!(matches!(
                receiver.set_video_delay(Duration::ZERO),
                Err(Error::UnsupportedCapability { .. })
            ));
            assert!(matches!(
                receiver.set_audio_delay(Duration::ZERO),
                Err(Error::UnsupportedCapability { .. })
            ));
        }
    }

    #[test]
    fn receiver_allocated_strings_free_once_even_when_utf8_is_invalid() {
        use std::sync::atomic::AtomicUsize;
        static FREES: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn free(_instance: NDIlib_recv_instance_t, pointer: *const c_char) {
            FREES.fetch_add(1, Ordering::Relaxed);
            drop(unsafe { CString::from_raw(pointer.cast_mut()) });
        }
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        let strings = sys::ReceiverStrings {
            NDIlib_recv_free_string: free,
        };
        let before = FREES.load(Ordering::Relaxed);
        for (bytes, valid) in [
            (b"source".to_vec(), true),
            (Vec::new(), true),
            (vec![255], false),
        ] {
            let guard = RecvStringGuard {
                core: &receiver.core,
                pointer: CString::new(bytes).unwrap().into_raw(),
                strings: &strings,
            };
            assert_eq!(guard.copy().is_ok(), valid);
            drop(guard);
        }
        let empty = RecvStringGuard {
            core: &receiver.core,
            pointer: ptr::null(),
            strings: &strings,
        };
        assert_eq!(empty.copy().unwrap(), None);
        assert_eq!(empty.copy_lossy().unwrap(), None);
        drop(empty);
        assert_eq!(FREES.load(Ordering::Relaxed) - before, 3);
    }

    #[test]
    fn lossy_receiver_strings_keep_the_terminator_bound() {
        unsafe extern "C" fn keep(_instance: NDIlib_recv_instance_t, _pointer: *const c_char) {}
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        let strings = sys::ReceiverStrings {
            NDIlib_recv_free_string: keep,
        };
        let max = crate::frames::MAX_METADATA_BYTES;
        let copy = |bytes: &[u8]| {
            RecvStringGuard {
                core: &receiver.core,
                pointer: bytes.as_ptr().cast(),
                strings: &strings,
            }
            .copy_lossy()
        };
        assert_eq!(copy(b"\0").unwrap().as_deref(), Some(""));
        assert_eq!(copy(b"a\xffb\0").unwrap().as_deref(), Some("a\u{FFFD}b"));
        // Stops at the first terminator.
        assert_eq!(copy(b"a\0\xff\0").unwrap().as_deref(), Some("a"));
        // A terminator in the last byte the scan may read is accepted.
        let mut longest = vec![b'x'; max];
        longest[max - 1] = 0;
        assert_eq!(copy(&longest).unwrap().unwrap().len(), max - 1);
        // Without one, the scan stops at the bound and reports an error.
        let unterminated = vec![b'x'; max];
        assert!(matches!(
            copy(&unterminated),
            Err(Error::UnexpectedSdkResult(_))
        ));
    }

    #[test]
    fn a_changed_source_name_that_is_not_utf8_is_still_reported_and_freed_once() {
        use std::{cell::RefCell, sync::atomic::AtomicUsize};
        static FREES: AtomicUsize = AtomicUsize::new(0);
        std::thread_local! {
            // What the fake SDK reports next: whether the name changed, and
            // the new name, if any.
            static NEXT: RefCell<(bool, Option<Vec<u8>>)> = const { RefCell::new((false, None)) };
        }
        unsafe extern "C" fn free(_instance: NDIlib_recv_instance_t, pointer: *const c_char) {
            FREES.fetch_add(1, Ordering::Relaxed);
            drop(unsafe { CString::from_raw(pointer.cast_mut()) });
        }
        unsafe extern "C" fn get_source_name(
            _instance: NDIlib_recv_instance_t,
            name: *mut *const c_char,
            _timeout_in_ms: u32,
        ) -> bool {
            let (changed, next) = NEXT.with(|next| next.take());
            let pointer = next.map_or(ptr::null(), |bytes| {
                CString::new(bytes).unwrap().into_raw().cast_const()
            });
            // SAFETY: the caller passes writable storage for the pointer.
            unsafe { name.write(pointer) };
            changed
        }
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        let source_name = sys::ReceiverSourceName {
            NDIlib_recv_get_source_name: get_source_name,
        };
        let strings = sys::ReceiverStrings {
            NDIlib_recv_free_string: free,
        };
        let poll = |changed, name: Option<&[u8]>| {
            NEXT.with(|next| *next.borrow_mut() = (changed, name.map(<[u8]>::to_vec)));
            let before = FREES.load(Ordering::Relaxed);
            let change = receiver.poll_source_name_change_until(
                &source_name,
                &strings,
                Deadline::after(Duration::ZERO).unwrap(),
            );
            (change.unwrap(), FREES.load(Ordering::Relaxed) - before)
        };
        assert_eq!(
            poll(true, Some(b"Studio \xff\xfe camera")),
            (
                Some(SourceNameChange::Connected(
                    "Studio \u{FFFD}\u{FFFD} camera".into()
                )),
                1
            )
        );
        assert_eq!(
            poll(true, Some("Studio é".as_bytes())),
            (Some(SourceNameChange::Connected("Studio é".into())), 1)
        );
        assert_eq!(poll(true, None), (Some(SourceNameChange::Disconnected), 0));
        // A string returned without a change is still released.
        assert_eq!(poll(false, Some(b"\xff")), (None, 1));
        assert_eq!(poll(false, None), (None, 0));
    }

    /// A source no receiver can connect to: no name and no address.
    fn test_source() -> Source {
        Source::new("")
    }

    /// The options of a receiver that stays unconnected.
    fn unconnected() -> ReceiverOptions {
        ReceiverOptions::new(test_source())
    }

    #[derive(Default)]
    struct FakeClock {
        elapsed: Cell<Duration>,
    }

    impl FakeClock {
        fn elapsed(&self) -> Duration {
            self.elapsed.get()
        }

        fn advance(&self, duration: Duration) {
            self.elapsed.set(self.elapsed.get() + duration);
        }
    }

    fn assert_raw_receiver_modes(
        raw: &RawRecvCreateV3,
        expected_color_format: NDIlib_recv_color_format_e,
        expected_bandwidth: NDIlib_recv_bandwidth_e,
    ) {
        assert_eq!(raw.raw.color_format, expected_color_format);
        assert_eq!(raw.raw.bandwidth, expected_bandwidth);
        assert_ne!(
            raw.raw.color_format,
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_max
        );
        assert_ne!(
            raw.raw.bandwidth,
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_max
        );
    }

    #[test]
    fn media_status_is_retained_coalesced_and_consumed_once() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        fn check<K: CaptureKind>(receiver: &Receiver) {
            let (_, allocations) = crate::test_alloc::allocation_count(|| {
                for _ in 0..3 {
                    assert!(receiver
                        .core
                        .capture_result::<K>(CaptureResult::Notification(
                            Notification::StatusChanged
                        ))
                        .unwrap()
                        .is_none());
                }
                assert!(receiver
                    .core
                    .capture_result::<K>(CaptureResult::Notification(Notification::None))
                    .unwrap()
                    .is_none());
                assert!(matches!(
                    receiver.core.take_pending_event(),
                    Some(ReceiverEvent::StatusChanged(_))
                ));
                assert!(receiver.core.take_pending_event().is_none());
            });
            assert_eq!(allocations, 0);
        }
        check::<VideoKind>(&receiver);
        check::<AudioKind>(&receiver);
        check::<MetadataKind>(&receiver);
    }

    #[test]
    fn pending_status_survives_an_invalid_poll_event_timeout() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        receiver.core.pending_status.store(true, Ordering::Release);
        assert!(receiver.poll_event(Duration::MAX).is_err());
        assert!(receiver.core.pending_status.load(Ordering::Acquire));
        assert!(matches!(
            receiver.poll_event(Duration::ZERO).unwrap(),
            Some(ReceiverEvent::StatusChanged(_))
        ));
        assert!(!receiver.core.pending_status.load(Ordering::Acquire));
    }

    #[test]
    fn every_receiver_wait_refuses_a_timeout_above_max_timeout() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        let excessive = crate::MAX_TIMEOUT + Duration::from_nanos(1);
        for result in [
            receiver.video().capture(excessive).map(drop),
            receiver.audio().capture_ref(excessive).map(drop),
            receiver.metadata().capture(excessive).map(drop),
            receiver.poll_event(excessive).map(drop),
            receiver.poll_source_name_change(excessive).map(drop),
        ] {
            assert!(matches!(result, Err(Error::InvalidInput(_))), "{result:?}");
        }
    }

    #[test]
    fn concurrent_status_pollers_take_once_and_later_publications_survive() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        receiver.core.pending_status.store(true, Ordering::Release);
        let barrier = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let receiver = &receiver;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        receiver.core.take_pending_status().is_some()
                    })
                })
                .collect();
            barrier.wait();
            assert_eq!(
                handles
                    .into_iter()
                    .map(|h| usize::from(h.join().unwrap()))
                    .sum::<usize>(),
                1
            );
        });
        receiver.core.pending_status.store(true, Ordering::Release);
        assert!(receiver.core.take_pending_status().is_some());
    }

    #[test]
    fn status_publication_is_consumed_once_and_newer_publication_survives() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        receiver
            .core
            .capture_result::<AudioKind>(CaptureResult::Notification(Notification::StatusChanged))
            .unwrap();
        assert!(receiver.core.take_pending_status().is_some());

        // A pending notification avoids even a contended reconnect lock.
        let _exclusive = receiver.core.capture_guard.write();
        receiver
            .core
            .capture_result::<VideoKind>(CaptureResult::Notification(Notification::StatusChanged))
            .unwrap();
        assert!(matches!(
            receiver.poll_event(Duration::ZERO).unwrap(),
            Some(ReceiverEvent::StatusChanged(_))
        ));
    }

    #[test]
    fn receiver_mode_transitions_preserve_pending_status_and_core_identity() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        let original_core = Arc::as_ptr(&receiver.core);
        receiver.core.pending_status.store(true, Ordering::Release);
        let fs = crate::FrameSync::new(receiver).unwrap();
        assert!(matches!(
            fs.receiver().poll_event(Duration::ZERO).unwrap(),
            Some(ReceiverEvent::StatusChanged(_))
        ));
        fs.receiver()
            .core
            .pending_status
            .store(true, Ordering::Release);
        let receiver = fs.into_receiver();
        assert!(matches!(
            receiver.poll_event(Duration::ZERO).unwrap(),
            Some(ReceiverEvent::StatusChanged(_))
        ));
        assert!(!receiver.core.pending_status.load(Ordering::Acquire));
        assert_eq!(Arc::as_ptr(&receiver.core), original_core);
        assert_eq!(
            std::mem::size_of::<Receiver>(),
            std::mem::size_of::<Receiver<FrameSyncAttached>>()
        );
    }

    #[test]
    fn value_types_read_back_what_the_sdk_reported() {
        assert_eq!(SettingChange::from_sdk(true), SettingChange::Applied);
        assert_eq!(SettingChange::from_sdk(false), SettingChange::Unchanged);
        let idle = ConnectionStats::default();
        assert!(!idle.is_connected());
        assert_eq!(idle.connections(), 0);
        assert_eq!(idle.video_drop_percentage(), 0.0);
        let stats = ConnectionStats {
            connections: 1,
            video_frames_received: 900,
            video_frames_dropped: 100,
            audio_frames_received: 3,
            metadata_frames_dropped: 1,
            video_frames_queued: 5,
            ..idle
        };
        assert!(stats.is_connected());
        assert_eq!(stats.video_drop_percentage(), 10.0);
        assert_eq!(stats.audio_drop_percentage(), 0.0);
        assert_eq!(stats.metadata_drop_percentage(), 100.0);
        assert_eq!(
            (
                stats.video_frames_received(),
                stats.audio_frames_received(),
                stats.video_frames_queued()
            ),
            (900, 3, 5)
        );
        assert_eq!(ReceiverStatus { connections: 2 }.connections(), 2);
    }

    #[test]
    fn drop_rates_handle_extreme_public_counters() {
        assert_eq!(drop_percentage(0, 0), 0.0);
        assert_eq!(drop_percentage(900, 100), 10.0);
        assert_eq!(drop_percentage(u64::MAX, u64::MAX), 50.0);
        assert_eq!(drop_percentage(0, u64::MAX), 100.0);
    }

    #[test]
    fn ptz_rejects_invalid_commands_before_calling_sdk() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        let ptz = receiver.ptz();
        for result in [
            ptz.recall_preset(u8::MAX, 0.5),
            ptz.recall_preset(100, 0.5),
            ptz.recall_preset(0, f32::NAN),
            ptz.store_preset(100),
            ptz.zoom(f32::INFINITY),
            ptz.zoom_speed(-1.1),
            ptz.pan_tilt(0.0, 1.1),
            ptz.pan_tilt_speed(f32::NAN, 0.0),
            ptz.focus(-0.1),
            ptz.focus_speed(1.1),
            ptz.white_balance_manual(0.0, 1.1),
            ptz.exposure_manual(f32::NEG_INFINITY),
            ptz.exposure_manual_detailed(0.5, f32::NAN, 0.5),
        ] {
            assert!(matches!(result, Err(Error::InvalidInput(_))));
        }
        assert_eq!(validate_ptz_preset(0).unwrap(), 0);
        assert_eq!(validate_ptz_preset(99).unwrap(), 99);
        assert!(validate_ptz_value("speed", -1.0, -1.0, 1.0).is_ok());
        assert!(validate_ptz_value("speed", 1.0, -1.0, 1.0).is_ok());
    }

    #[test]
    fn outbound_controls_accept_invocation_without_a_source() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        // Tally is kept for a later connection. Metadata has nowhere to go,
        // which runtimes may or may not report; either way the call returns.
        receiver.set_tally(Tally::new(true, false));
        let frame = MetadataFrame::new("<ndi_text>é</ndi_text>")
            .unwrap()
            .with_timecode(crate::Timecode::from_ticks(42));
        let pointer = frame.to_raw().p_data;
        for _ in 0..3 {
            let sent = receiver.send_metadata(&frame);
            assert!(
                matches!(sent, Ok(()) | Err(Error::NotConnected)),
                "{sent:?}"
            );
            assert_eq!(frame.to_raw().p_data, pointer);
        }
    }

    #[test]
    fn capture_contention_obeys_zero_and_bounded_budgets() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        let exclusive = receiver.core.capture_guard.write();
        std::thread::scope(|scope| {
            let (done, completed) = std::sync::mpsc::channel();
            let receiver = &receiver;
            let worker = scope.spawn(move || {
                assert!(receiver.video().capture(Duration::ZERO).unwrap().is_none());
                assert!(receiver.poll_event(Duration::ZERO).unwrap().is_none());
                assert!(receiver
                    .audio()
                    .capture(Duration::from_millis(5))
                    .unwrap()
                    .is_none());
                assert!(receiver
                    .metadata()
                    .capture_ref(Duration::from_millis(5))
                    .unwrap()
                    .is_none());
                done.send(()).unwrap();
            });
            // The writer remains held until the worker has returned: a stale
            // unbounded read would deadlock instead of finishing within budget.
            let result = completed.recv_timeout(Duration::from_secs(2));
            drop(exclusive);
            worker.join().unwrap();
            result.unwrap();
        });
    }

    /// A long `poll_event` holds the capture guard for one attempt at a time:
    /// a reconnect gets it within about one poll interval, a capture that
    /// starts while the reconnect waits still reaches the SDK within its own
    /// budget, and an event a capture keeps ends the poll at its next attempt.
    /// One SDK call holding the guard for the whole timeout made the
    /// reconnect wait almost five seconds.
    #[test]
    fn a_long_poll_event_holds_off_reconnect_for_one_attempt_at_most() {
        use std::{sync::mpsc, time::Instant};
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = &Receiver::new(&ndi, &unconnected()).unwrap();
        let stop = &AtomicBool::new(false);
        let budget = Duration::from_secs(1);
        std::thread::scope(|scope| {
            let (polling, started_polling) = mpsc::channel();
            let poller = scope.spawn(move || {
                polling.send(()).unwrap();
                loop {
                    // Nothing is connected, so only the event published
                    // below ends a poll before its timeout.
                    let event = receiver.poll_event(Duration::from_secs(5)).unwrap();
                    if stop.load(Ordering::Acquire) {
                        return event;
                    }
                }
            });
            started_polling.recv().unwrap();
            // Let the poller settle into an SDK wait under the guard.
            std::thread::sleep(Duration::from_millis(250));

            let (waiting, writer_waits) = mpsc::channel();
            let capture = scope.spawn(move || {
                writer_waits.recv().unwrap();
                // Start once the reconnect is queued for the guard: a fair
                // lock holds this capture off until the reconnect is done.
                std::thread::sleep(Duration::from_millis(20));
                native_capture_calls::take();
                let began = Instant::now();
                let frame = receiver.metadata().capture(budget).unwrap();
                (
                    frame.is_none(),
                    began.elapsed(),
                    native_capture_calls::take(),
                )
            });
            waiting.send(()).unwrap();
            let began = Instant::now();
            receiver.reconnect().unwrap();
            let reconnected = began.elapsed();
            let (nothing_arrived, captured_for, sdk_calls) = capture.join().unwrap();

            stop.store(true, Ordering::Release);
            let published = Instant::now();
            receiver.core.retain(Notification::StatusChanged);
            let ended = poller.join().unwrap();
            let poll_ended_after = published.elapsed();

            assert!(reconnected < Duration::from_secs(1), "{reconnected:?}");
            assert!(nothing_arrived);
            // The capture waited for the guard within its budget, then
            // reached the SDK, and spent the rest of its budget there.
            assert!(sdk_calls >= 1, "{sdk_calls} SDK calls");
            assert!(captured_for >= budget, "{captured_for:?}");
            assert!(
                captured_for < budget + Duration::from_millis(500),
                "{captured_for:?}"
            );
            assert!(ended.is_some());
            assert!(
                poll_ended_after < Duration::from_secs(1),
                "{poll_ended_after:?}"
            );
        });
    }

    #[test]
    fn poll_event_spends_its_timeout_in_bounded_attempts() {
        use std::time::Instant;
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        // A zero timeout makes exactly one attempt, with a zero SDK wait.
        native_capture_calls::take();
        let began = Instant::now();
        assert!(receiver.poll_event(Duration::ZERO).unwrap().is_none());
        assert!(began.elapsed() < Duration::from_millis(500));
        assert_eq!(native_capture_calls::take(), 1);
        // Nothing is connected: the whole budget is spent, in attempts of at
        // most one poll interval each, where one SDK call used to take it all.
        let budget = Duration::from_millis(450);
        let began = Instant::now();
        assert!(receiver.poll_event(budget).unwrap().is_none());
        assert!(began.elapsed() >= budget, "{:?}", began.elapsed());
        let calls = native_capture_calls::take();
        assert!(calls >= 3, "{calls} SDK calls");
    }

    #[test]
    fn receiver_color_format_maps_to_exact_sdk_values() {
        assert_eq!(
            ReceiverColorFormat::BgrxBgra.to_sdk(),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA
        );
        assert_eq!(
            ReceiverColorFormat::UyvyBgra.to_sdk(),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_BGRA
        );
        assert_eq!(
            ReceiverColorFormat::RgbxRgba.to_sdk(),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA
        );
        assert_eq!(
            ReceiverColorFormat::UyvyRgba.to_sdk(),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_RGBA
        );
        assert_eq!(
            ReceiverColorFormat::Fastest.to_sdk(),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_fastest
        );
        assert_eq!(
            ReceiverColorFormat::Best.to_sdk(),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_best
        );
    }

    #[test]
    fn receiver_bandwidth_maps_to_exact_sdk_values() {
        assert_eq!(
            ReceiverBandwidth::MetadataOnly.to_sdk(),
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_metadata_only
        );
        assert_eq!(
            ReceiverBandwidth::AudioOnly.to_sdk(),
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_audio_only
        );
        assert_eq!(
            ReceiverBandwidth::Lowest.to_sdk(),
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest
        );
        assert_eq!(
            ReceiverBandwidth::Highest.to_sdk(),
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest
        );
    }

    #[test]
    fn receiver_options_defaults_emit_operational_raw_modes() {
        let raw = unconnected().to_raw().unwrap();

        assert_raw_receiver_modes(
            &raw,
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA,
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
        );
        assert!(raw.raw.allow_video_fields);
    }

    #[test]
    fn snapshot_preset_emits_operational_raw_modes() {
        let raw = ReceiverOptions::snapshot_preset(test_source())
            .to_raw()
            .unwrap();

        assert_raw_receiver_modes(
            &raw,
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA,
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
        );
        assert!(!raw.raw.allow_video_fields);
    }

    #[test]
    fn high_quality_preset_emits_operational_raw_modes() {
        let raw = ReceiverOptions::high_quality_preset(test_source())
            .to_raw()
            .unwrap();

        assert_raw_receiver_modes(
            &raw,
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA,
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
        );
        assert!(raw.raw.allow_video_fields);
    }

    #[test]
    fn monitoring_preset_emits_operational_raw_modes() {
        let raw = ReceiverOptions::monitoring_preset(test_source())
            .to_raw()
            .unwrap();

        assert_raw_receiver_modes(
            &raw,
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA,
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_metadata_only,
        );
        assert!(!raw.raw.allow_video_fields);
    }

    #[test]
    fn retry_caps_sub_poll_timeout_to_total_budget() {
        let clock = FakeClock::default();
        let policy = RetryPolicy {
            poll_interval: Duration::from_millis(100),
            sleep_between: Duration::from_millis(10),
        };
        let mut polls = Vec::new();

        let result = retry_capture_with_clock(
            Duration::from_millis(25),
            &policy,
            |poll| {
                polls.push(poll);
                Ok(Some(42))
            },
            || clock.elapsed(),
            |_| {},
        );

        assert_eq!(result.unwrap(), Some(42));
        assert_eq!(polls, [Duration::from_millis(25)]);
    }

    #[test]
    fn retry_shrinks_later_poll_to_remaining_budget() {
        let clock = FakeClock::default();
        let policy = RetryPolicy {
            poll_interval: Duration::from_millis(40),
            sleep_between: Duration::from_millis(10),
        };
        let mut polls = Vec::new();
        let mut sleeps = Vec::new();

        let result: Result<Option<i32>> = retry_capture_with_clock(
            Duration::from_millis(50),
            &policy,
            |poll| {
                polls.push(poll);
                if polls.len() == 1 {
                    clock.advance(Duration::from_millis(5));
                } else {
                    clock.advance(poll);
                }
                Ok(None)
            },
            || clock.elapsed(),
            |sleep| {
                sleeps.push(sleep);
                clock.advance(sleep);
            },
        );

        assert!(result.unwrap().is_none());
        assert_eq!(
            polls,
            [Duration::from_millis(40), Duration::from_millis(35)]
        );
        assert_eq!(sleeps, [Duration::from_millis(10)]);
    }

    #[test]
    fn retry_zero_timeout_success_makes_one_nonblocking_attempt() {
        let clock = FakeClock::default();
        let mut polls = Vec::new();

        let result = retry_capture_with_clock(
            Duration::ZERO,
            &RetryPolicy::default(),
            |poll| {
                polls.push(poll);
                Ok(Some(42))
            },
            || clock.elapsed(),
            |_| panic!("zero-timeout success should not sleep"),
        );

        assert_eq!(result.unwrap(), Some(42));
        assert_eq!(polls, [Duration::ZERO]);
    }

    #[test]
    fn retry_zero_timeout_miss_gives_up_after_one_attempt() {
        let clock = FakeClock::default();
        let mut polls = Vec::new();

        let result: Result<Option<i32>> = retry_capture_with_clock(
            Duration::ZERO,
            &RetryPolicy::default(),
            |poll| {
                polls.push(poll);
                Ok(None)
            },
            || clock.elapsed(),
            |_| panic!("zero-timeout miss should not sleep"),
        );

        assert!(result.unwrap().is_none());
        assert_eq!(polls, [Duration::ZERO]);
    }

    #[test]
    fn retry_stops_when_an_attempt_spends_the_budget() {
        let clock = FakeClock::default();
        let policy = RetryPolicy {
            poll_interval: Duration::from_millis(8),
            sleep_between: Duration::from_millis(5),
        };
        let mut polls = Vec::new();

        let result: Result<Option<i32>> = retry_capture_with_clock(
            Duration::from_millis(10),
            &policy,
            |poll| {
                polls.push(poll);
                clock.advance(Duration::from_millis(10));
                Ok(None)
            },
            || clock.elapsed(),
            |_| panic!("expired timeout should not sleep"),
        );

        assert!(result.unwrap().is_none());
        assert_eq!(polls, [Duration::from_millis(8)]);
    }

    #[test]
    fn retry_caps_sleep_to_remaining_budget() {
        let clock = FakeClock::default();
        let policy = RetryPolicy {
            poll_interval: Duration::from_millis(8),
            sleep_between: Duration::from_millis(5),
        };
        let mut sleeps = Vec::new();

        let mut attempts = 0;

        let result: Result<Option<i32>> = retry_capture_with_clock(
            Duration::from_millis(10),
            &policy,
            |poll| {
                attempts += 1;
                assert_eq!(poll, Duration::from_millis(8));
                // An early return, 1 ms short of the attempt's timeout.
                clock.advance(Duration::from_millis(7));
                Ok(None)
            },
            || clock.elapsed(),
            |sleep| {
                sleeps.push(sleep);
                clock.advance(sleep);
            },
        );

        assert!(result.unwrap().is_none());
        assert_eq!(attempts, 1);
        assert_eq!(sleeps, [Duration::from_millis(3)]);
    }

    #[test]
    fn retry_follows_a_full_wait_at_once() {
        // Every attempt waits out its timeout, as when no frame is sent: the
        // next attempt starts at once, so a frame arriving in between is taken
        // without a pause.
        let clock = FakeClock::default();
        let mut polls = Vec::new();

        let result: Result<Option<i32>> = retry_capture_with_clock(
            Duration::from_millis(250),
            &RetryPolicy::default(),
            |poll| {
                polls.push(poll);
                clock.advance(poll);
                Ok(None)
            },
            || clock.elapsed(),
            |_| panic!("an attempt that waited out its timeout must not be followed by a pause"),
        );

        assert!(result.unwrap().is_none());
        assert_eq!(clock.elapsed(), Duration::from_millis(250));
        assert_eq!(
            polls,
            [
                Duration::from_millis(100),
                Duration::from_millis(100),
                Duration::from_millis(50),
            ]
        );
    }

    #[test]
    fn retry_pauses_only_after_an_early_return() {
        // Warm-up: the SDK returns 3 ms into the first attempt and at once on
        // the second, then waits out the third, and the fourth delivers.
        let clock = FakeClock::default();
        let mut polls = Vec::new();
        let mut sleeps = Vec::new();
        let waits = [3, 0, 100].map(Duration::from_millis);

        let result = retry_capture_with_clock(
            Duration::from_secs(1),
            &RetryPolicy::default(),
            |poll| {
                let Some(wait) = waits.get(polls.len()).copied() else {
                    return Ok(Some(42));
                };
                polls.push(poll);
                clock.advance(wait);
                Ok(None)
            },
            || clock.elapsed(),
            |sleep| {
                sleeps.push(sleep);
                clock.advance(sleep);
            },
        );

        assert_eq!(result.unwrap(), Some(42));
        assert_eq!(polls, [Duration::from_millis(100); 3]);
        assert_eq!(sleeps, [Duration::from_millis(10); 2]);
        // One deadline for the whole call: 3 + 10 + 0 + 10 + 100 ms.
        assert_eq!(clock.elapsed(), Duration::from_millis(123));
    }

    #[test]
    fn retry_timeout_counts_time_spent_before_the_first_attempt() {
        // A caller's 50 ms budget of which 30 ms went before the retry began,
        // as when an async capture waited for a blocking-pool thread.
        let clock = FakeClock::default();
        clock.advance(Duration::from_millis(30));
        let mut polls = Vec::new();

        let result: Result<Option<i32>> = retry_capture_with_clock(
            Duration::from_millis(50),
            &RetryPolicy::default(),
            |poll| {
                polls.push(poll);
                clock.advance(poll);
                Ok(None)
            },
            || clock.elapsed(),
            |_| panic!("an expired budget should not sleep"),
        );

        assert!(result.unwrap().is_none());
        assert_eq!(clock.elapsed(), Duration::from_millis(50));
        assert_eq!(polls, [Duration::from_millis(20)]);
    }

    #[test]
    fn retry_with_a_spent_budget_makes_one_nonblocking_attempt() {
        let clock = FakeClock::default();
        clock.advance(Duration::from_millis(70));
        let mut polls = Vec::new();

        let result: Result<Option<i32>> = retry_capture_with_clock(
            Duration::from_millis(50),
            &RetryPolicy::default(),
            |poll| {
                polls.push(poll);
                Ok(None)
            },
            || clock.elapsed(),
            |_| panic!("a spent budget should not sleep"),
        );

        assert!(result.unwrap().is_none());
        assert_eq!(polls, [Duration::ZERO]);
    }

    #[test]
    fn retry_capture_measures_the_callers_budget() {
        let deadline = Deadline::after(Duration::from_millis(20)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let mut polls = Vec::new();

        let result: Result<Option<i32>> =
            retry_capture(deadline, &RetryPolicy::default(), |poll| {
                polls.push(poll);
                Ok(None)
            });

        assert!(result.unwrap().is_none());
        assert!(!polls.is_empty());
        // `None` comes only once the whole budget is spent.
        assert!(deadline.is_expired());
        // The first poll is taken from what was left after the caller's 5 ms.
        assert!(polls[0] <= Duration::from_millis(15), "{polls:?}");
    }

    #[test]
    fn retry_succeeds_after_retry_before_timeout() {
        let clock = FakeClock::default();
        let attempts = Cell::new(0);

        let result = retry_capture_with_clock(
            Duration::from_millis(50),
            &RetryPolicy::default(),
            |_| {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 1 {
                    Ok(None)
                } else {
                    Ok(Some(42))
                }
            },
            || clock.elapsed(),
            |sleep| clock.advance(sleep),
        );

        assert_eq!(result.unwrap(), Some(42));
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn retry_propagates_error() {
        let clock = FakeClock::default();
        let attempts = Cell::new(0);
        let mut sleeps = Vec::new();

        let result: Result<Option<i32>> = retry_capture_with_clock(
            Duration::from_secs(1),
            &RetryPolicy::default(),
            |_| {
                attempts.set(attempts.get() + 1);
                Err(Error::CaptureFailed("test error".into()))
            },
            || clock.elapsed(),
            |sleep| sleeps.push(sleep),
        );

        assert!(
            matches!(result, Err(Error::CaptureFailed(_))),
            "Should propagate CaptureFailed error"
        );
        assert_eq!(attempts.get(), 1);
        assert!(sleeps.is_empty());
    }
}
