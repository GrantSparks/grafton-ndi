//! NDI receiving functionality for video, audio, and metadata.
//!
//! # Connection state and SDK status reports
//!
//! Poll [`Receiver::is_connected`] or [`Receiver::connection_stats`] to track
//! connections. Losing a connection can produce no [`ReceiverEvent::StatusChanged`]
//! event; that event reports SDK status information, such as source capabilities:
//!
//! ```no_run
//! # use grafton_ndi::{Ndi, Receiver, ReceiverEvent, ReceiverOptions, ReceiverBandwidth, Source};
//! # use std::time::Duration;
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! # let ndi = Ndi::new()?;
//! # let source = Source::new("CAMERA (Program)").unwrap();
//! // In real usage, obtain the source from Finder::find_sources().
//! let options = ReceiverOptions::new(source).bandwidth(ReceiverBandwidth::MetadataOnly);
//! let receiver = Receiver::new(&ndi, &options)?;
//!
//! if let Some(ReceiverEvent::StatusChanged(_)) =
//!     receiver.next_event(Duration::from_millis(1000))?
//! {
//!     println!("The SDK reported a status change");
//! }
//! // Query connection state even when no status event arrived.
//! println!("Connected: {}", receiver.is_connected());
//! println!("Active connections: {}", receiver.connection_stats()?.connections());
//! # Ok(())
//! # }
//! ```

#[cfg(test)]
use crate::RuntimeFeature;
#[cfg(test)]
use std::sync::atomic::AtomicBool;

#[cfg(feature = "advanced")]
use crate::advanced::{AvSyncAttached, PacketAttached};

use std::{
    ffi::CString,
    marker::PhantomData,
    os::raw::c_char,
    ptr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use parking_lot::{Mutex, MutexGuard, RwLock};

use crate::{
    capture::{
        capture_any, capture_raw, AllSlots, AudioKind, CaptureKind, CaptureResult, CaptureStrategy,
        Delivered, Media, MetadataKind, NoSlots, Notification, SlotSet, VideoKind,
    },
    connection_metadata::ConnectionMetadata,
    finder::{RawSource, Source},
    frames::MetadataPartsExt,
    source_retention::RetainedSource,
    sys::{self, bindings::*},
    timeout::{self, Deadline, NeverStop, Stop, POLL_INTERVAL},
    AudioFrame, AudioFrameRef, Error, MetadataFrame, MetadataFrameRef, MetadataSource, Ndi, Result,
    VideoFrame, VideoFrameRef,
};

pub(crate) mod attached;
mod pending;

pub(crate) use pending::PendingEvents;

/// A string the receiver API returned, released when dropped.
struct RecvStringGuard<'a> {
    core: &'a ReceiverCore,
    string: sys::table::Called<*const c_char>,
    strings: &'a sys::ReceiverStrings,
}
impl RecvStringGuard<'_> {
    /// The string's bytes before its terminator, or `None` for a null string.
    ///
    /// The scan for the terminator is bounded by
    /// [`MAX_SDK_STRING_BYTES`](crate::sdk_copy::MAX_SDK_STRING_BYTES); a string
    /// without one within it is an [`Error::UnexpectedSdkResult`].
    fn bytes(&self) -> Result<Option<&[u8]>> {
        if self.string.value().is_null() {
            return Ok(None);
        }
        // SAFETY: the SDK returned a readable NUL-terminated string, which
        // this guard keeps allocated for as long as it is borrowed.
        let bytes = unsafe { crate::sdk_copy::sdk_text(self.string) }?;
        Ok(Some(bytes))
    }

    /// Copy the string, failing with [`Error::InvalidUtf8`] when it is not UTF-8.
    fn copy(&self) -> Result<Option<String>> {
        self.bytes()?
            .map(|bytes| {
                std::str::from_utf8(bytes)
                    .map(str::to_owned)
                    .map_err(Error::InvalidUtf8)
            })
            .transpose()
    }

    /// Copy the string, replacing invalid UTF-8 with U+FFFD.
    ///
    /// For a value the SDK reports only once, where an error would lose it.
    fn copy_lossy(&self) -> Result<Option<String>> {
        Ok(self.bytes()?.map(crate::sdk_copy::copy_lossy))
    }
}
impl Drop for RecvStringGuard<'_> {
    fn drop(&mut self) {
        if !self.string.value().is_null() {
            // SAFETY: the SDK returned this string for this instance and it is
            // released exactly once.
            unsafe {
                self.strings
                    .NDIlib_recv_free_string(self.core.instance.0, self.string.into_value())
            };
        }
    }
}

/// The longest receive delay, in milliseconds, that a receiver sets or
/// accepts from the SDK, for video and audio alike.
///
/// The NDI SDK documentation of receiver video and audio delay clamps both
/// to 0 through 2000 ms; a longer delay is refused here rather than clamped.
const MAX_RECEIVE_DELAY_MILLIS: u32 = 2000;

/// Convert a delay the SDK reported, rejecting values outside
/// 0..=[`MAX_RECEIVE_DELAY_MILLIS`].
fn receiver_delay(millis: i32) -> Result<Duration> {
    u32::try_from(millis)
        .ok()
        .filter(|millis| *millis <= MAX_RECEIVE_DELAY_MILLIS)
        .map(|millis| Duration::from_millis(millis.into()))
        .ok_or({
            Error::UnexpectedSdkResult(crate::SdkResultDefect::InvalidReceiveDelay { millis })
        })
}

/// A consumable report carries its publication order across an async worker.
/// The sequence is private: only the state that published it can admit a replay.
#[derive(Debug)]
pub(crate) struct ReceiverReport<T> {
    sequence: u64,
    pub(crate) value: T,
}

impl<T> ReceiverReport<T> {
    pub(crate) fn unsequenced(value: T) -> Self {
        Self { sequence: 0, value }
    }

    pub(crate) fn map<U>(self, convert: impl FnOnce(T) -> U) -> ReceiverReport<U> {
        ReceiverReport {
            sequence: self.sequence,
            value: convert(self.value),
        }
    }

    pub(crate) fn into_value(self) -> T {
        self.value
    }
}

/// Latest-value reports coalesce like the SDK's one-shot queries. This lock
/// protects only bookkeeping; no SDK call or native-query permit is taken in it.
struct LatestReport<T> {
    sequence: u64,
    pending: Option<ReceiverReport<T>>,
}

impl<T> Default for LatestReport<T> {
    fn default() -> Self {
        Self {
            sequence: 0,
            pending: None,
        }
    }
}

impl<T> LatestReport<T> {
    fn advance(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.pending = None;
        self.sequence
    }

    fn report(&mut self, value: T) -> ReceiverReport<T> {
        ReceiverReport {
            sequence: self.advance(),
            value,
        }
    }

    fn publish(&mut self, value: T) {
        let report = self.report(value);
        self.pending = Some(report);
    }

    #[cfg(any(test, feature = "tokio", feature = "smol"))]
    fn replay(&mut self, report: ReceiverReport<T>) {
        if report.sequence == self.sequence {
            self.pending = Some(report);
        }
    }
}

#[cfg(test)]
mod latest_report_tests {
    use super::LatestReport;

    #[test]
    fn publishing_coalesces_and_taking_consumes_the_latest_value() {
        let mut latest = LatestReport::default();
        latest.publish("first");
        latest.publish("second");
        assert_eq!(latest.pending.take().unwrap().value, "second");
        assert!(latest.pending.take().is_none());
    }

    #[test]
    fn the_current_report_can_be_replayed_after_it_is_taken() {
        let mut latest = LatestReport::default();
        latest.publish("current");
        let abandoned = latest.pending.take().unwrap();
        latest.replay(abandoned);
        assert_eq!(latest.pending.take().unwrap().value, "current");
        assert!(latest.pending.take().is_none());
    }

    #[test]
    fn an_older_report_cannot_follow_a_newer_report() {
        let mut latest = LatestReport::default();
        let older = latest.report("first");
        let current = latest.report("second");
        latest.replay(older);
        assert!(latest.pending.is_none());
        latest.replay(current);
        assert_eq!(latest.pending.take().unwrap().value, "second");
    }

    #[test]
    fn only_the_newest_abandoned_value_is_replayed() {
        let mut latest = LatestReport::default();
        let older = latest.report("first");
        let current = latest.report("second");
        latest.replay(current);
        latest.replay(older);
        assert_eq!(latest.pending.take().unwrap().value, "second");
        assert!(latest.pending.is_none());
    }

    #[test]
    fn taking_a_newer_publication_does_not_readmit_an_older_report() {
        let mut latest = LatestReport::default();
        latest.publish(1);
        let old = latest.pending.take().unwrap();
        latest.publish(2);
        latest.replay(old);
        let current = latest.pending.take().unwrap();
        assert_eq!(current.value, 2);
        latest.publish(3);
        assert_eq!(latest.pending.take().unwrap().value, 3);
        latest.replay(current);
        assert!(latest.pending.is_none());
    }

    #[test]
    fn replay_order_survives_sequence_wrap() {
        let mut latest = LatestReport {
            sequence: u64::MAX - 1,
            pending: None,
        };
        let before_wrap = latest.report("before wrap");
        assert_eq!(before_wrap.sequence, u64::MAX);
        latest.replay(before_wrap);
        let abandoned = latest.pending.take().unwrap();
        let wrapped = latest.report("wrapped");
        assert_eq!(wrapped.sequence, 0);
        latest.replay(abandoned);
        assert!(latest.pending.is_none());
        latest.replay(wrapped);
        let abandoned = latest.pending.take().unwrap();
        assert_eq!(abandoned.value, "wrapped");
        latest.publish("after wrap");
        latest.replay(abandoned);
        assert_eq!(latest.pending.take().unwrap().value, "after wrap");
    }
}

/// A coalesced status invalidation, its failure-report bit and publication
/// generation. Validation completes before a compare/exchange consumes that
/// generation; a failed snapshot stays pending and reports at most once.
#[derive(Default)]
struct PendingStatus(AtomicUsize);

impl PendingStatus {
    const PENDING: usize = 1;
    const REPORTED: usize = 2;

    fn publish(&self) {
        let mut observed = self.0.load(Ordering::Acquire);
        loop {
            match self.0.compare_exchange_weak(
                observed,
                (observed.wrapping_add(4) & !(Self::PENDING | Self::REPORTED)) | Self::PENDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => observed = current,
            }
        }
    }

    #[cfg(test)]
    fn is_pending(&self) -> bool {
        self.0.load(Ordering::Acquire) & Self::PENDING != 0
    }

    #[cfg(test)]
    fn take_with<T>(&self, snapshot: impl FnOnce() -> Result<T>) -> Result<Option<T>> {
        self.take_or_else_with(snapshot, || None)
    }

    fn take_or_else_with<T>(
        &self,
        snapshot: impl FnOnce() -> Result<T>,
        other: impl FnOnce() -> Option<T>,
    ) -> Result<Option<T>> {
        let observed = self.0.load(Ordering::Acquire);
        if observed & Self::PENDING == 0 {
            return Ok(None);
        }
        let status = match snapshot() {
            Ok(status) => status,
            Err(error) => {
                // A bad status must not hide an independent notification. Do
                // not mark its breach reported until a caller receives it.
                if let Some(other) = other() {
                    return Ok(Some(other));
                }
                if observed & Self::REPORTED != 0 {
                    return Ok(None);
                }
                return match self.0.compare_exchange(
                    observed,
                    observed | Self::REPORTED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => Err(error),
                    Err(_) => Ok(None),
                };
            }
        };
        // A failed CAS means another consumer took it or a new generation was
        // published while validating. Neither case may consume that newer event.
        Ok(self
            .0
            .compare_exchange(
                observed,
                observed & !(Self::PENDING | Self::REPORTED),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| status))
    }
}

/// The SDK's millisecond count for a receive delay, rounded up by the one
/// timeout rule, [`timeout::sdk_millis`], and refused above
/// [`MAX_RECEIVE_DELAY_MILLIS`].
fn receiver_delay_millis(delay: Duration) -> Result<i32> {
    timeout::sdk_millis(delay)
        .filter(|millis| *millis <= MAX_RECEIVE_DELAY_MILLIS)
        .and_then(|millis| i32::try_from(millis).ok())
        .ok_or({
            Error::InvalidInput(crate::InputDefect::ReceiveDelayTooLong {
                requested: delay.into(),
            })
        })
}

/// The pause after an attempt that returned before its own end without a
/// value: the SDK's early return, which would otherwise busy-wait.
const EARLY_RETURN_PAUSE: Duration = Duration::from_millis(10);

/// Maximum consecutive notifications retried without yielding. An idle
/// response always backs off; an endless notification stream must do so too.
const IMMEDIATE_NOTIFICATION_RETRIES: usize = 8;

enum Retry<T> {
    Item(T),
    Progress,
    Idle,
}

impl<T> From<Option<T>> for Retry<T> {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Idle, Self::Item)
    }
}

/// Run `attempt` until it returns a value or `deadline` is spent: the one
/// capture loop behind every receiver capture, for all frame kinds, for both
/// typed capture methods and the async ones, and behind [`Receiver::next_event`]
/// and the Advanced packet receivers' capture.
///
/// The SDK returns from a capture early, without a frame, while a new
/// connection synchronizes and whenever it reports a status or source change,
/// so one SDK call does not wait out the caller's timeout. Each attempt is
/// handed what is left of `deadline`, which may have started before this
/// call, narrowed to [`POLL_INTERVAL`].
///
/// Returns `Ok(None)` once the budget is spent without a value. An error from
/// an attempt ends the loop.
fn capture_within<T, R: Into<Retry<T>>>(
    deadline: Deadline,
    stop: impl Stop,
    attempt: impl FnMut(Attempt) -> Result<R>,
) -> Result<Option<T>> {
    capture_within_clock_with_stop(deadline, stop, attempt, Instant::now, std::thread::sleep)
}

/// [`capture_within`] with `now` reading the clock and `sleep` pausing, so
/// that a test can drive the loop with a clock of its own.
///
/// The first attempt is made even when nothing is left of `deadline`, so a
/// spent or zero budget makes exactly one attempt, with a zero timeout.
///
/// Idle early returns and exhausted notification bursts are followed by
/// [`EARLY_RETURN_PAUSE`], never past the end of `deadline`. Notifications
/// otherwise retry immediately. An attempt that waited
/// until its end is followed at once by the next, so that a frame arriving in
/// between is not delayed by a pause.
#[cfg(test)]
fn capture_within_clock<T, R: Into<Retry<T>>>(
    deadline: Deadline,
    attempt: impl FnMut(Attempt) -> Result<R>,
    now: impl FnMut() -> Instant,
    sleep: impl FnMut(Duration),
) -> Result<Option<T>> {
    capture_within_clock_with_stop(deadline, NeverStop, attempt, now, sleep)
}

fn capture_within_clock_with_stop<T, R: Into<Retry<T>>>(
    deadline: Deadline,
    stop: impl Stop,
    mut attempt: impl FnMut(Attempt) -> Result<R>,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
) -> Result<Option<T>> {
    let mut notifications = 0;
    loop {
        let this = Attempt(deadline.within_at(now(), POLL_INTERVAL));
        let progress = match attempt(this)?.into() {
            Retry::Item(value) => return Ok(Some(value)),
            Retry::Progress => true,
            Retry::Idle => false,
        };
        let mut returned = now();
        if deadline.remaining_at(returned).is_zero() {
            return Ok(None);
        }
        if returned < this.0.end() && (!progress || notifications >= IMMEDIATE_NOTIFICATION_RETRIES)
        {
            // Before the attempt's end, and so before the deadline's.
            sleep(EARLY_RETURN_PAUSE.min(deadline.remaining_at(returned)));
            returned = now();
            notifications = 0;
        } else if progress && returned < this.0.end() {
            notifications += 1;
        } else {
            notifications = 0;
        }
        if deadline.remaining_at(returned).is_zero() || stop.is_stopped() {
            return Ok(None);
        }
    }
}

/// One attempt of [`capture_within`]: the caller's deadline, narrowed to
/// [`POLL_INTERVAL`] from when the attempt began.
///
/// Only [`capture_within`] makes one. [`ReceiverCore::capture_lock`] fixes the
/// instance and runtime for its native capture, which waits only for what is
/// left of the attempt. Cancellation and pending notifications are checked
/// again between attempts, whatever the caller's timeout.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Attempt(Deadline);

fn validate_ptz_value(name: &'static str, value: f32, min: f32, max: f32) -> Result<()> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(Error::InvalidInput(crate::InputDefect::PtzValue {
            detail: crate::CompactDiagnostic::new(crate::PtzDefect {
                name,
                minimum: min,
                maximum: max,
            }),
        }))
    }
}

fn validate_ptz_preset(preset: u8) -> Result<i32> {
    if preset <= 99 {
        Ok(i32::from(preset))
    } else {
        Err(Error::InvalidInput(crate::InputDefect::PtzPreset))
    }
}

/// Send one PTZ command, the SDK function `$func` with arguments `$arg`, to
/// the source `$ptz` controls.
macro_rules! ptz_command {
    ($ptz:expr, $func:ident $(, $arg:expr)*) => {{
        // SAFETY: the receiver instance is live for the handle's borrow, and
        // every argument is a plain value.
        unsafe { $ptz.core.sdk().named().$func($ptz.core.instance.0 $(, $arg)*) }.performed()
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
/// # let source = Source::new("CAMERA (Program)").unwrap();
/// // In real usage, obtain the source from Finder::find_sources().
/// let options = ReceiverOptions::new(source)
///     .color_format(ReceiverColorFormat::UyvyBgra)
///     .bandwidth(ReceiverBandwidth::Lowest)
///     .name("Multiviewer");
/// let receiver = Receiver::new(&ndi, &options)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReceiverOptions {
    source: Source,
    color_format: ReceiverColorFormat,
    bandwidth: ReceiverBandwidth,
    allow_video_fields: bool,
    name: Option<String>,
    #[cfg(feature = "advanced")]
    pub(crate) config: Option<crate::advanced::AdvancedConfig>,
    /// A native color format that the creation settings carry in place of
    /// `color_format`: a passthrough preset's, which no
    /// [`ReceiverColorFormat`] names.
    #[cfg(feature = "advanced")]
    passthrough: Option<NDIlib_recv_color_format_e>,
}

#[repr(C)]
pub(crate) struct RawRecvCreateV3 {
    _source: RawSource,
    _name: Option<CString>,
    pub raw: NDIlib_recv_create_v3_t,
}

impl ReceiverOptions {
    /// Replace the source, preserving every other receiver setting.
    #[must_use]
    pub fn source(mut self, source: Source) -> Self {
        self.source = source;
        self
    }

    /// Let the SDK choose the receiver name again.
    #[must_use]
    pub fn clear_name(mut self) -> Self {
        self.name = None;
        self
    }

    /// Remove the per-instance Advanced configuration override.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn clear_config(mut self) -> Self {
        self.config = None;
        self
    }

    /// The requested source, independent of later live source changes.
    #[must_use]
    pub fn configured_source(&self) -> &Source {
        &self.source
    }

    /// The stored color choice. Packet passthrough presets may override this
    /// when creating a native receiver.
    #[must_use]
    pub fn configured_color_format(&self) -> ReceiverColorFormat {
        self.color_format
    }

    /// The requested bandwidth, before native creation.
    #[must_use]
    pub fn configured_bandwidth(&self) -> ReceiverBandwidth {
        self.bandwidth
    }

    /// Whether the requested configuration permits video fields.
    #[must_use]
    pub fn configured_allow_video_fields(&self) -> bool {
        self.allow_video_fields
    }

    /// The requested receiver name, or `None` to let the SDK choose one.
    #[must_use]
    pub fn configured_name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The stored Advanced configuration, when explicitly set. Reading it
    /// does not expose its JSON or test native support.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn configured_config(&self) -> Option<&crate::advanced::AdvancedConfig> {
        self.config.as_ref()
    }

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
            #[cfg(feature = "advanced")]
            passthrough: None,
        }
    }

    /// Settings for capturing snapshots: RGB color, low bandwidth, whole
    /// frames.
    ///
    /// Suited to image export, thumbnails and analysis pipelines, and to
    /// networks with little bandwidth to spare:
    /// - color format [`ReceiverColorFormat::RgbxRgba`], convenient for
    ///   encoding untagged sources directly;
    /// - bandwidth [`ReceiverBandwidth::Lowest`], the source's preview stream;
    /// - video fields not allowed, so interlaced video arrives as whole frames.
    ///
    /// The SDK converts YUV to RGB with BT.709 limited range even when
    /// `ndi_color_info` names another matrix. This preset's RGB frames already
    /// carry that conversion. See the
    /// [image encoding color guidance](https://grantsparks.github.io/grafton-ndi/grafton_ndi/image_encoding/struct.ImageOptions.html#color)
    /// for receiving YUV when the tag's matrix matters, including alpha and
    /// high bit depth sources. Tags still control HDR refusal and PNG tagging.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Finder, FinderOptions, ReceiverOptions, Receiver};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.watch().wait_for_change(Duration::from_secs(1))?;
    /// # let sources = finder.current_sources()?;
    /// let options = ReceiverOptions::snapshot_preset(sources[0].clone()).name("Snapshot Receiver");
    /// let receiver = Receiver::new(&ndi, &options)?;
    ///
    /// // Capture and encode in one line (requires image-encoding feature)
    /// #[cfg(feature = "image-encoding")]
    /// if let Some(frame) = receiver.capture_video(Duration::from_secs(5))? {
    ///     let png_bytes = frame.encode_image(&grafton_ndi::image_encoding::ImageOptions::new(
    ///         grafton_ndi::image_encoding::ImageFormat::Png,
    ///     ))?;
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
    /// RGB is convenient for untagged sources. For tagged YUV, the SDK has
    /// already converted to BT.709 limited range, ignoring `ndi_color_info`;
    /// image encoding cannot apply the tagged matrix to these RGB frames.
    /// See the
    /// [image encoding color guidance](https://grantsparks.github.io/grafton-ndi/grafton_ndi/image_encoding/struct.ImageOptions.html#color)
    /// for receiving YUV when the tag's matrix matters, including alpha and
    /// high bit depth sources. Tags still control HDR refusal and PNG tagging.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Finder, FinderOptions, ReceiverOptions, Receiver};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.watch().wait_for_change(Duration::from_secs(1))?;
    /// # let sources = finder.current_sources()?;
    /// let options =
    ///     ReceiverOptions::high_quality_preset(sources[0].clone()).name("High Quality Receiver");
    /// let receiver = Receiver::new(&ndi, &options)?;
    ///
    /// // Capture full quality frames
    /// if let Some(frame) = receiver.capture_video(Duration::from_secs(5))? {
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
    /// Suited to polling connection state with [`Receiver::is_connected`] or
    /// [`Receiver::connection_stats`], tally and PTZ control. Connection loss
    /// need not produce a [`ReceiverEvent::StatusChanged`] event.
    ///
    /// Uses these settings:
    /// - bandwidth [`ReceiverBandwidth::MetadataOnly`];
    /// - the default color format, which no frame uses;
    /// - video fields not allowed.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Finder, FinderOptions, ReceiverOptions, Receiver};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.watch().wait_for_change(Duration::from_secs(1))?;
    /// # let sources = finder.current_sources()?;
    /// let options = ReceiverOptions::monitoring_preset(sources[0].clone()).name("Status Monitor");
    /// let receiver = Receiver::new(&ndi, &options)?;
    ///
    /// println!("Connected: {}", receiver.is_connected());
    /// println!("Active connections: {}", receiver.connection_stats()?.connections());
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

    /// These options with the native color format `format`, which no
    /// [`ReceiverColorFormat`] names, in place of their own: the one a
    /// passthrough preset selects. Validation and the creation settings both
    /// take it, so what is checked is what the SDK is given.
    #[cfg(feature = "advanced")]
    pub(crate) fn with_passthrough(mut self, format: NDIlib_recv_color_format_e) -> Self {
        self.passthrough = Some(format);
        self
    }

    /// The color format the creation settings carry.
    fn sdk_color_format(&self) -> NDIlib_recv_color_format_e {
        #[cfg(feature = "advanced")]
        if let Some(format) = self.passthrough {
            return format;
        }
        self.color_format.to_sdk()
    }

    /// Validate exactly the options passed to the SDK, retaining each
    /// capability proof the native descriptor relies on.
    pub(crate) fn validate_runtime<'a>(
        &'a self,
        evidence: sys::Evidence<'a>,
    ) -> Result<ValidatedReceiveOptions<'a>> {
        let best444 = (self.sdk_color_format() == ReceiverColorFormat::Best444.to_sdk())
            .then(|| <sys::ReceiveBest444 as sys::Table>::within(evidence))
            .transpose()?;
        let video_only = matches!(
            self.bandwidth,
            ReceiverBandwidth::VideoOnlyLowest | ReceiverBandwidth::VideoOnlyHighest
        )
        .then(|| <sys::ReceiveVideoOnly as sys::Table>::within(evidence))
        .transpose()?;
        Ok(ValidatedReceiveOptions {
            options: self,
            _best444: best444,
            _video_only: video_only,
        })
    }

    /// The native creation settings, with the strings they point to.
    pub(crate) fn to_raw(&self, ndi: &Ndi) -> Result<RawRecvCreateV3> {
        self.validate_runtime(ndi.evidence())?.to_raw()
    }
}

pub(crate) struct ValidatedReceiveOptions<'a> {
    options: &'a ReceiverOptions,
    _best444: Option<&'a sys::ReceiveBest444>,
    _video_only: Option<&'a sys::ReceiveVideoOnly>,
}

impl ValidatedReceiveOptions<'_> {
    pub(crate) fn to_raw(&self) -> Result<RawRecvCreateV3> {
        let source = self.options.source.to_raw();
        let name = self.options.name.as_deref().map(CString::new).transpose()?;
        let p_ndi_recv_name = name.as_ref().map_or(ptr::null(), |n| n.as_ptr());
        let source_raw = *source.as_raw();
        Ok(RawRecvCreateV3 {
            raw: NDIlib_recv_create_v3_t {
                source_to_connect_to: source_raw,
                color_format: self.options.sdk_color_format(),
                bandwidth: self.options.bandwidth.to_sdk(),
                allow_video_fields: self.options.allow_video_fields,
                p_ndi_recv_name,
            },
            _source: source,
            _name: name,
        })
    }
}

mod mode_sealed {
    pub trait Sealed {}
    #[allow(private_bounds)]
    pub trait MetadataSealed: super::Permits<crate::capture::MetadataKind> {}
    #[allow(private_bounds)]
    pub trait EventSealed: super::Permits<crate::capture::NoSlots> {}
    #[allow(private_bounds)]
    pub trait AudioSealed: super::Permits<crate::capture::AudioKind> {}
    #[allow(private_bounds)]
    pub trait VideoSealed: super::Permits<crate::capture::VideoKind> {}
}

/// Sealed receiver capability mode: what a [`Receiver`] may do besides its
/// common controls.
///
/// The common controls work in every mode: tally, PTZ, upstream metadata,
/// connection statistics, delays, source-name changes, connecting and
/// reconnecting. [`MetadataMode`] adds typed metadata capture, [`EventMode`]
/// adds event polling, [`AudioMode`] adds audio capture, and [`VideoMode`]
/// adds video capture. Only [`RawCapture`] permits every capture kind and
/// event polling.
pub trait ReceiverMode: mode_sealed::Sealed + Send + Sync {}

/// Sealed receiver mode permitting typed metadata capture: [`RawCapture`],
/// [`FrameSyncAttached`] and, with `advanced`, `AvSyncAttached`.
///
/// Metadata capture retains any notifications it encounters for a later raw
/// receiver, but does not grant event polling; see [`EventMode`].
pub trait MetadataMode: ReceiverMode + mode_sealed::MetadataSealed {}

/// Sealed receiver mode permitting event polling: [`RawCapture`].
///
/// Native FrameSync and AV sync can consume status notifications before an
/// attached receiver observes them. Attached receivers therefore use capability
/// getters, [`Receiver::wait_for_source_name_change`], [`Receiver::is_connected`]
/// and [`Receiver::connection_stats`] rather than event polling.
pub trait EventMode: ReceiverMode + mode_sealed::EventSealed {}

/// Sealed receiver mode in which the receiver's own audio and metadata
/// capture are available: [`RawCapture`].
///
/// A [`MetadataMode`] without it hands the receiver's audio to the object
/// that holds the receiver: [`FrameSyncAttached`] to a
/// [`FrameSync`](crate::FrameSync) and, with the `advanced` feature,
/// `AvSyncAttached` to an Advanced `AvSync`.
pub trait AudioMode: MetadataMode + mode_sealed::AudioSealed {}

/// Sealed receiver mode in which the receiver's own video capture is
/// available: [`RawCapture`] and, with the `advanced` feature, the
/// `AvSyncAttached` mode of an Advanced `AvSync`'s receiver.
///
/// A mode without it hands the receiver's video to the object that holds the
/// receiver.
pub trait VideoMode: ReceiverMode + mode_sealed::VideoSealed {}

/// The one crate-internal permission table for both typed and slot-set capture.
/// ReceiverMode seals the implementors. Public modes inherit their permissions,
/// so a generic adapter's public mode bound carries this same internal gate.
pub(crate) trait Permits<K>: ReceiverMode {}

// Raw capture permits every sealed public kind, including generic CaptureKind
// callers. Attached modes retain only the slots their synchronizer leaves free.
impl<K: CaptureKind> Permits<K> for RawCapture {}
impl Permits<AllSlots> for RawCapture {}
impl Permits<NoSlots> for RawCapture {}
impl Permits<MetadataKind> for FrameSyncAttached {}
#[cfg(feature = "advanced")]
impl Permits<AllSlots> for PacketAttached {}
#[cfg(feature = "advanced")]
impl Permits<VideoKind> for AvSyncAttached {}
#[cfg(feature = "advanced")]
impl Permits<MetadataKind> for AvSyncAttached {}

/// Modes native receiver creation can produce directly.
pub(crate) trait CreatableMode: ReceiverMode {}
impl CreatableMode for RawCapture {}
#[cfg(feature = "advanced")]
impl CreatableMode for PacketAttached {}

/// Modes whose native synchronizer must be destroyed before raw capture returns.
pub(crate) trait SynchronizerMode: ReceiverMode {}
impl SynchronizerMode for FrameSyncAttached {}
#[cfg(feature = "advanced")]
impl SynchronizerMode for AvSyncAttached {}

/// Ordinary receiver mode, permitting raw video capture.
#[derive(Debug)]
pub enum RawCapture {}
/// Receiver mode held exclusively by [`crate::FrameSync`], whose synchronizer
/// takes the receiver's video and audio.
///
/// The receiver's own metadata capture and common controls are available.
/// Event polling is unavailable because native synchronization can consume
/// status notifications. Query capabilities with [`Ptz::is_supported`] and
/// [`Receiver::web_control_url`] (or Advanced KVM's `is_supported`), source
/// names with [`Receiver::wait_for_source_name_change`], and connection state with
/// [`Receiver::is_connected`] and [`Receiver::connection_stats`].
/// Its video and audio capture are unavailable: FrameSync returns them through
/// [`FrameSync::capture_video`](crate::FrameSync::capture_video) and
/// [`FrameSync::capture_audio`](crate::FrameSync::capture_audio).
///
/// Raw audio capture on the attached receiver would compete with the
/// synchronizer for the receiver's audio: each audio frame reaches only one
/// of them. Measured with NDI 6.3.2 and 6.4 over loopback, raw capture
/// interleaved with FrameSync pulls takes most of a 30 fps stream's audio
/// frames, none of which then reach the FrameSync, which returns silence in
/// their place. Metadata is not taken that way: every metadata frame still
/// reaches the receiver's own capture while the FrameSync pulls video and
/// audio. `framesync_pulls_leave_sequenced_metadata_for_the_attached_receiver`
/// in `tests/framesync_capture_boundary.rs` repeats that metadata check on
/// every runtime exercised by CI.
#[derive(Debug)]
pub enum FrameSyncAttached {}
impl mode_sealed::Sealed for RawCapture {}
impl mode_sealed::Sealed for FrameSyncAttached {}
impl mode_sealed::MetadataSealed for RawCapture {}
impl mode_sealed::EventSealed for RawCapture {}
impl mode_sealed::MetadataSealed for FrameSyncAttached {}
impl mode_sealed::AudioSealed for RawCapture {}
impl mode_sealed::VideoSealed for RawCapture {}
impl ReceiverMode for RawCapture {}
impl ReceiverMode for FrameSyncAttached {}
impl MetadataMode for RawCapture {}
impl EventMode for RawCapture {}
impl MetadataMode for FrameSyncAttached {}
impl AudioMode for RawCapture {}
impl VideoMode for RawCapture {}
#[cfg(feature = "advanced")]
impl mode_sealed::Sealed for PacketAttached {}
#[cfg(feature = "advanced")]
impl ReceiverMode for PacketAttached {}
#[cfg(feature = "advanced")]
impl mode_sealed::Sealed for AvSyncAttached {}
#[cfg(feature = "advanced")]
impl mode_sealed::MetadataSealed for AvSyncAttached {}
#[cfg(feature = "advanced")]
impl mode_sealed::VideoSealed for AvSyncAttached {}
#[cfg(feature = "advanced")]
impl ReceiverMode for AvSyncAttached {}
#[cfg(feature = "advanced")]
impl MetadataMode for AvSyncAttached {}
#[cfg(feature = "advanced")]
impl VideoMode for AvSyncAttached {}

/// An owned NDI receiver. The default mode permits all raw capture kinds.
/// [`FrameSync`](crate::FrameSync) and the Advanced `AvSync` change the mode of
/// the receiver they hold while retaining the same resource core. The Advanced
/// `PacketReceiver` creates its receiver in `PacketAttached` mode; see [`ReceiverMode`].
///
/// Dropping the receiver disconnects it from its source, unless a
/// [`RecvAdvertiser`](crate::discovery_server::RecvAdvertiser) registration still
/// holds it: a registered receiver stays connected, and advertised, until that
/// registration is removed or its advertiser is dropped.
pub struct Receiver<M: ReceiverMode = RawCapture> {
    pub(crate) core: Arc<ReceiverCore>,
    _mode: PhantomData<M>,
}

impl<M: ReceiverMode> std::fmt::Debug for Receiver<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Receiver")
            .field(
                "name",
                &self.core._name.as_ref().map(|name| name.to_string_lossy()),
            )
            .field("mode", &std::any::type_name::<M>())
            .finish_non_exhaustive()
    }
}

/// Only a receiver's connection lock can issue this permit.
struct ConnectionPermit;

/// One local connection request, with this receiver's exclusive permit.
/// It keeps the passed descriptor and its strings until a later request has
/// returned or the native receiver is destroyed. Captures may overlap it.
struct Connection<'a> {
    core: &'a ReceiverCore,
    _exclusive: MutexGuard<'a, ConnectionPermit>,
}

enum ConnectionRequest {
    Connect(RetainedSource),
    Reconnect,
    Disconnect,
}

impl Connection<'_> {
    fn connect(&self, next: RetainedSource) {
        self.request(ConnectionRequest::Connect(next));
    }

    fn reconnect(&self) {
        self.request(ConnectionRequest::Reconnect);
    }

    fn disconnect(&self) {
        self.request(ConnectionRequest::Disconnect);
    }

    fn request(&self, request: ConnectionRequest) {
        // The connection permit serializes writers. Clone only the retained
        // allocation under the read lock; snapshots can continue throughout
        // native work, and the write lock covers only the final replacement.
        let call = |source| {
            // SAFETY: this permit fixes the live receiver and serializes native
            // requests. RetainedSource keeps the descriptors and text alive
            // through this call and until replacement or native destruction.
            unsafe {
                self.core
                    .sdk()
                    .NDIlib_recv_connect(self.core.instance.0, source)
            };
        };
        match request {
            ConnectionRequest::Connect(next) => {
                let mut target = self.core.target.read().clone();
                target.replace(next, call);
                *self.core.target.write() = target;
            }
            ConnectionRequest::Reconnect => self.core.target.read().reissue(call),
            // Disconnect deliberately preserves the last target for reconnect.
            ConnectionRequest::Disconnect => call(ptr::null()),
        }
    }
}

/// A receiver's opaque native identity; ownership stays with `ReceiverCore`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecvInstance(pub(crate) NDIlib_recv_instance_t);

// SAFETY: the SDK gives receivers no thread affinity and permits concurrent
// captures (Processing.NDI.Recv.h). CaptureLock records the native implementation
// observations and tests supporting concurrent connection changes. The core
// serializes local connection changes with one another, and source-name waits
// with one another.
// Every native use retains the core; its other fields determine its auto traits.
unsafe impl Send for RecvInstance {}
// SAFETY: as above; receiver modes reserve the media paths used by synchronizers.
unsafe impl Sync for RecvInstance {}

pub(crate) struct ReceiverCore {
    pub(crate) instance: RecvInstance,
    pub(crate) _ndi: Ndi,
    /// Serializes local connection changes. Captures never take this lock;
    /// their overlap with connection changes is measured beside CaptureLock.
    connection_guard: Mutex<ConnectionPermit>,
    /// The latest requested target. Connection serializes its writers;
    /// snapshots never take it and therefore never wait for a capture or a queued
    /// connection change. Writers hold this lock only to replace the value.
    target: RwLock<RetainedSource>,
    pending: PendingEvents,
    // Only native source-name queries take this permit. Replaying an abandoned
    // result takes the separate short lock, never waiting for the SDK.
    source_query_guard: Mutex<()>,
    source_name: Mutex<LatestReport<SourceNameChange>>,
    connection_metadata: Mutex<ConnectionMetadata>,
    _name: Option<CString>,
    #[cfg(feature = "advanced")]
    _config: Option<crate::advanced::AdvancedConfig>,
}

impl Receiver {
    /// Wait up to `timeout` for video, audio, metadata, or a receiver event,
    /// borrowing delivered media directly from the SDK without a payload
    /// copy or allocation. No Advanced feature or runtime is required.
    ///
    /// A single native wait requests every media kind, so a silent stream
    /// does not delay other kinds behind its own timeout. All capture methods share the same wait-budget and zero-timeout
    /// behavior described by [`Self::capture_video_ref`]. The returned item borrows this receiver; media references
    /// may move to scoped worker threads while that borrow remains live.
    ///
    /// Pending and native events are consumed once, competing with
    /// [`Self::next_event`] and other combined captures. They are coalesced
    /// invalidations, with no ordered history or broadcast guarantee. Typed
    /// media captures retain their events for these consumers.
    /// [`ReceiverEvent::SourceChanged`] requires the separate
    /// [`Self::wait_for_source_name_change`] query for the new name.
    ///
    /// This operation exists only in ordinary raw-capture mode. FrameSync,
    /// AvSync and packet receivers reserve some or all native media slots.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`],
    /// [`Error::ConnectionLost`] when the SDK reports a lost connection, and
    /// the same frame-validation errors as typed borrowed capture. Rejected
    /// frames are freed through their matching native free operation.
    /// [`Error::UnexpectedSdkResult`] reports a negative status connection
    /// count once per publication. That status stays pending for recovery;
    /// other events and subsequent media capture remain available.
    #[doc(alias = "NDIlib_recv_capture_v3")]
    pub fn capture_any_ref(&self, timeout: Duration) -> Result<Option<ReceiverItemRef<'_>>> {
        self.capture_any_ref_until(Deadline::after(timeout)?, NeverStop)
    }

    /// Wait up to `timeout` for any media or receiver event and return an
    /// owned item. See [`Self::capture_any_ref`] for delivery and wait rules.
    ///
    /// Media is copied through the same validated paths as typed capture:
    /// active video rows are packed, audio channel padding is omitted, and
    /// timing and metadata are preserved. Video/audio allocate one payload
    /// buffer plus metadata when present; standalone metadata allocates its
    /// text. Events allocate nothing. Copying may finish after the wait
    /// budget expires. Use [`Self::capture_any_ref`] for in-place processing.
    ///
    /// # Errors
    ///
    /// The errors of [`Self::capture_any_ref`].
    #[doc(alias = "NDIlib_recv_capture_v3")]
    pub fn capture_any(&self, timeout: Duration) -> Result<Option<ReceiverItem>> {
        self.capture_any_until(Deadline::after(timeout)?)
    }

    /// Create a receiver and connect it to the configured source.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] when the options need a mode the
    /// selected runtime lacks, or carry an Advanced configuration that the
    /// runtime or this build cannot take; [`Error::InvalidCString`] for a receiver
    /// name that contains a NUL byte; and [`Error::NativeOperationFailed`]
    /// when the SDK creates no receiver.
    #[doc(alias = "NDIlib_recv_create_v3")]
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
                #[cfg(feature = "advanced")]
                None,
                |core, raw| core.named().NDIlib_recv_create_v3(raw),
            )
        }
    }
}

#[allow(private_bounds)]
impl<M: CreatableMode> Receiver<M> {
    /// The one creation path: check `options` against the runtime, create a
    /// receiver through `create`, which is
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
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(
            &'static sys::Core,
            &NDIlib_recv_create_v3_t,
        ) -> sys::table::Called<NDIlib_recv_instance_t>,
    ) -> Result<Self> {
        let RawRecvCreateV3 {
            _source: _,
            _name,
            raw,
        } = options.to_raw(ndi)?;
        // SAFETY: the core keeps every string these settings point to until
        // after native destruction, including the target and receiver name.
        let instance = create(&ndi.api().core, &raw).created()?.as_ptr();
        Ok(Self {
            core: Arc::new(ReceiverCore {
                instance: RecvInstance(instance),
                _ndi: ndi.clone(),
                connection_guard: Mutex::new(ConnectionPermit),
                target: RwLock::new(RetainedSource::new(&options.source)),
                pending: PendingEvents::default(),
                source_query_guard: Mutex::new(()),
                source_name: Mutex::new(LatestReport::default()),
                connection_metadata: Mutex::new(ConnectionMetadata::default()),
                _name,
                #[cfg(feature = "advanced")]
                _config: config,
            }),
            _mode: PhantomData,
        })
    }
}

impl Receiver<RawCapture> {
    /// Synthetic packet-mode receiver for tests without a passthrough SDK.
    #[cfg(all(test, feature = "advanced"))]
    pub(crate) fn into_packet_for_test(self) -> Receiver<PacketAttached> {
        Receiver {
            core: self.core,
            _mode: PhantomData,
        }
    }
}

macro_rules! receiver_capture_methods {
    ($(($mode:ident, $kind:ident, $owned:ident, $borrowed:ident,
        $capture:ident, $capture_ref:ident, $capture_with:ident, $label:literal)),* $(,)?) => {
        $(
            impl<M: $mode> Receiver<M> {
                #[doc = concat!("Wait up to `timeout` for ", $label, " and copy it into an owned frame.")]
                ///
                /// A zero timeout makes one nonblocking SDK attempt. Early SDK
                /// status and source reports are retained for `next_event`, or, on an
                /// attached receiver, for the raw receiver that detaching returns;
                /// capture continues until a frame arrives or the budget expires.
                /// Each native attempt waits at most 100 ms; validation and copying
                /// can finish after the budget. `None` alone does not establish
                /// disconnection or freshness.
                ///
                /// # Errors
                ///
                /// Invalid timeouts, a lost connection, or invalid SDK frame data.
                #[doc(alias = "NDIlib_recv_capture_v3")]
                pub fn $capture(&self, timeout: Duration) -> Result<Option<$owned>> {
                    Ok(self.$capture_ref(timeout)?.as_ref().map($borrowed::to_owned))
                }

                #[doc = concat!("Wait up to `timeout` for ", $label, " and borrow its native buffer.")]
                ///
                /// The returned frame borrows the receiver and releases its SDK
                /// buffer when dropped. Capture retains status and source reports
                /// for `next_event`, or, on an attached receiver, for the raw
                /// receiver that detaching returns. A zero timeout makes one nonblocking attempt;
                /// each other attempt waits at most 100 ms within the shared budget.
                ///
                /// # Errors
                ///
                /// Invalid timeouts, a lost connection, or invalid SDK frame data.
                #[doc(alias = "NDIlib_recv_capture_v3")]
                pub fn $capture_ref(&self, timeout: Duration) -> Result<Option<$borrowed<'_>>> {
                    self.core.capture_ref_until::<$kind>(Deadline::after(timeout)?, NeverStop)
                }
            }
        )*
    };
}
crate::capture::capture_method_kinds!(receiver_capture_methods);

impl<M: ReceiverMode> Receiver<M> {
    /// Typed capture for an adapter whose receiver mode permits this kind.
    #[cfg(any(feature = "tokio", feature = "smol", ndi_test_capture_permissions))]
    pub(crate) fn capture_ref_until<K: CaptureKind>(
        &self,
        deadline: Deadline,
        stop: impl Stop,
    ) -> Result<Option<K::Ref<'_>>>
    where
        M: Permits<K>,
    {
        self.core.capture_ref_until::<K>(deadline, stop)
    }

    /// Mixed capture selects its slot set through the same mode permission.
    pub(crate) fn capture_any_until_with<'rx, S: SlotSet, T>(
        &'rx self,
        deadline: Deadline,
        stop: impl Stop,
        mut event: impl FnMut(ReceiverEvent) -> T,
        convert: impl FnMut(S::Media<'rx>) -> Result<T>,
    ) -> Result<Option<T>>
    where
        M: Permits<S>,
    {
        self.core.capture_any_until_with::<S, T>(
            deadline,
            stop,
            |report| event(report.value),
            convert,
        )
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    pub(crate) fn capture_any_report_until<S: SlotSet, T>(
        &self,
        deadline: Deadline,
        stop: impl Stop,
        event: impl FnMut(ReceiverReport<ReceiverEvent>) -> T,
        convert: impl FnMut(S::Media<'_>) -> Result<T>,
    ) -> Result<Option<T>>
    where
        M: Permits<S>,
    {
        self.core
            .capture_any_until_with::<S, T>(deadline, stop, event, convert)
    }

    pub(crate) fn capture_any_ref_until(
        &self,
        deadline: Deadline,
        stop: impl Stop,
    ) -> Result<Option<ReceiverItemRef<'_>>>
    where
        M: Permits<AllSlots>,
    {
        self.capture_any_until_with::<AllSlots, _>(
            deadline,
            stop,
            ReceiverItemRef::Event,
            ReceiverCore::combined_result,
        )
    }

    fn capture_any_until(&self, deadline: Deadline) -> Result<Option<ReceiverItem>>
    where
        M: Permits<AllSlots>,
    {
        Ok(self
            .capture_any_ref_until(deadline, NeverStop)?
            .as_ref()
            .map(ReceiverItemRef::to_owned))
    }

    pub(crate) fn next_event_until(
        &self,
        deadline: Deadline,
        stop: impl Stop,
    ) -> Result<Option<ReceiverEvent>>
    where
        M: Permits<NoSlots>,
    {
        self.capture_any_until_with::<NoSlots, _>(
            deadline,
            stop,
            |event| event,
            |media| match media {},
        )
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
    /// [`RuntimeFeature::ReceiveDelay`](crate::RuntimeFeature::ReceiveDelay).
    #[doc(alias = "NDIlib_recv_set_video_delay")]
    pub fn set_video_delay(&self, delay: Duration) -> Result<SettingChange> {
        let millis = receiver_delay_millis(delay)?;
        let table = self.core.delay()?;
        // SAFETY: the receiver instance is live for this borrow.
        let changed = unsafe { table.NDIlib_recv_set_video_delay(self.core.instance.0, millis) };
        Ok(SettingChange::from_sdk(changed))
    }

    /// Return the effective native video delay. Requires receiver-delay support.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] without
    /// [`RuntimeFeature::ReceiveDelay`](crate::RuntimeFeature::ReceiveDelay), and [`Error::UnexpectedSdkResult`]
    /// when the SDK reports a delay outside 0 to 2000 ms.
    #[doc(alias = "NDIlib_recv_get_video_delay")]
    pub fn video_delay(&self) -> Result<Duration> {
        let table = self.core.delay()?;
        // SAFETY: the receiver instance is live for this borrow.
        receiver_delay(unsafe { table.NDIlib_recv_get_video_delay(self.core.instance.0) })
    }

    /// Set native audio buffering with the same rounding/range as [`Self::set_video_delay`].
    ///
    /// Equal audio and video delays preserve their relative timing; different
    /// values deliberately offset the streams.
    ///
    /// # Errors
    ///
    /// As for [`Self::set_video_delay`].
    #[doc(alias = "NDIlib_recv_set_audio_delay")]
    pub fn set_audio_delay(&self, delay: Duration) -> Result<SettingChange> {
        let millis = receiver_delay_millis(delay)?;
        let table = self.core.delay()?;
        // SAFETY: the receiver instance is live for this borrow.
        let changed = unsafe { table.NDIlib_recv_set_audio_delay(self.core.instance.0, millis) };
        Ok(SettingChange::from_sdk(changed))
    }

    /// Return the effective native audio delay. Requires receiver-delay support.
    ///
    /// # Errors
    ///
    /// As for [`Self::video_delay`].
    #[doc(alias = "NDIlib_recv_get_audio_delay")]
    pub fn audio_delay(&self) -> Result<Duration> {
        let table = self.core.delay()?;
        // SAFETY: the receiver instance is live for this borrow.
        receiver_delay(unsafe { table.NDIlib_recv_get_audio_delay(self.core.instance.0) })
    }

    /// Add validated metadata sent immediately and again on each connection.
    ///
    /// Accepts owned or captured metadata. The receiver keeps its own copy,
    /// so the input need not outlive the registration.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] without
    /// [`RuntimeFeature::ReceiverConnectionMetadata`](crate::RuntimeFeature::ReceiverConnectionMetadata).
    #[doc(alias = "NDIlib_recv_add_connection_metadata")]
    pub fn add_connection_metadata<S: MetadataSource + ?Sized>(&self, metadata: &S) -> Result<()> {
        let table = self.core.connection_metadata()?;
        let mut registrations = self.core.connection_metadata.lock();
        registrations.add(metadata, |raw| {
            // SAFETY: the locked collection owns this stable descriptor and its
            // text until clear returns or after the native receiver is destroyed.
            unsafe { table.NDIlib_recv_add_connection_metadata(self.core.instance.0, raw) };
        });
        Ok(())
    }

    /// Clear metadata registered for subsequent receiver connections.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] without
    /// [`RuntimeFeature::ReceiverConnectionMetadata`](crate::RuntimeFeature::ReceiverConnectionMetadata).
    #[doc(alias = "NDIlib_recv_clear_connection_metadata")]
    pub fn clear_connection_metadata(&self) -> Result<()> {
        let table = self.core.connection_metadata()?;
        let mut registrations = self.core.connection_metadata.lock();
        registrations.clear(|| {
            // SAFETY: the receiver and every registered descriptor remain live
            // until the native clear returns under the same registration lock.
            unsafe { table.NDIlib_recv_clear_connection_metadata(self.core.instance.0) };
        });
        Ok(())
    }

    /// Copy the optional device web-control URL. This does not open the URL.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] without
    /// [`RuntimeFeature::ReceiverWebControl`](crate::RuntimeFeature::ReceiverWebControl),
    /// [`Error::InvalidUtf8`] for an invalid URL encoding, and
    /// [`Error::UnexpectedSdkResult`] when the SDK does not terminate the URL
    /// within the bounded string scan.
    #[doc(alias = "NDIlib_recv_get_web_control")]
    pub fn web_control_url(&self) -> Result<Option<String>> {
        // The query comes with the function that releases what it returns.
        let (web_control, strings) = self
            .core
            ._ndi
            .table::<(sys::ReceiverWebControl, sys::ReceiverStrings)>()?;
        let guard = RecvStringGuard {
            core: &self.core,
            // SAFETY: the receiver instance is live for this borrow.
            string: unsafe {
                web_control
                    .named()
                    .NDIlib_recv_get_web_control(self.core.instance.0)
            },
            strings,
        };
        guard.copy()
    }

    /// Observe a change to the actual connected source name.
    ///
    /// `None` means no change within the wait budget; `Disconnected` is an
    /// observed change to no source. Concurrent pollers compete for notifications.
    /// This differs from [`Self::connection_target`], the latest requested target.
    ///
    /// The SDK reports each change once, so a name that is not valid UTF-8 is
    /// not an error: invalid sequences are replaced with U+FFFD, as
    /// [`Finder`](crate::Finder) does for discovered source names.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`],
    /// [`Error::UnsupportedCapability`] without
    /// [`RuntimeFeature::ConnectedSourceName`](crate::RuntimeFeature::ConnectedSourceName),
    /// and [`Error::UnexpectedSdkResult`] when the SDK does not terminate the
    /// name within the bounded string scan.
    #[doc(alias = "NDIlib_recv_get_source_name")]
    pub fn wait_for_source_name_change(
        &self,
        timeout: Duration,
    ) -> Result<Option<SourceNameChange>> {
        self.core
            .wait_for_source_name_change_until(Deadline::after(timeout)?, NeverStop)
    }

    /// Request a different source and remember it as the connection target.
    ///
    /// Serializes with other local connection changes, then issues the request
    /// and updates [`Self::connection_target`] together. Captures may continue
    /// while the SDK applies the change.
    /// Success means the request was issued, not that the source is available.
    /// The source was validated at construction. Existing FrameSync ownership
    /// remains.
    #[doc(alias = "NDIlib_recv_connect")]
    pub fn connect_to(&self, source: &Source) {
        self.connect_target(RetainedSource::new(source));
    }

    /// Apply an input already validated on the synchronous or async caller.
    pub(crate) fn connect_target(&self, target: RetainedSource) {
        self.core.lock_connection().connect(target);
    }

    /// Disconnect while retaining the target for [`Self::reconnect`].
    ///
    /// Serializes with other local connection changes, as [`Self::reconnect`]
    /// does, while captures may continue.
    #[doc(alias = "NDIlib_recv_connect")]
    pub fn disconnect(&self) {
        self.core.lock_connection().disconnect();
    }

    /// Publish this receiver's program and preview tally to its source.
    ///
    /// The SDK keeps the state and sends it to every source the receiver
    /// connects to, so it can be set before a connection and survives a
    /// reconnect.
    #[doc(alias = "NDIlib_recv_set_tally")]
    pub fn set_tally(&self, tally: Tally) {
        // `Processing.NDI.Recv.h`: the flag is false when nothing is
        // connected, and the state is sent the moment something connects, so
        // it reports no failure.
        // SAFETY: the receiver instance is live for this borrow, and the
        // tally outlives the synchronous call.
        unsafe {
            self.core
                .sdk()
                .NDIlib_recv_set_tally(self.core.instance.0, &tally.to_sdk())
        };
    }

    /// Send metadata upstream, to the source this receiver is connected to.
    ///
    /// Accepts owned or captured metadata without copying its text in the
    /// wrapper. The SDK copies the text before returning.
    ///
    /// # Errors
    ///
    /// [`Error::NotConnected`] when the receiver is connected to no source,
    /// so nothing was sent.
    #[doc(alias = "NDIlib_recv_send_metadata")]
    pub fn send_metadata<S: MetadataSource + ?Sized>(&self, metadata: &S) -> Result<()> {
        let raw = metadata.metadata_raw();
        // SAFETY: the receiver instance is live for this borrow, and the
        // borrowed C string outlives the synchronous call.
        let sent = unsafe {
            self.core
                .sdk()
                .NDIlib_recv_send_metadata(self.core.instance.0, &raw)
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
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::monitoring_preset(Source::new("CAMERA (Program)").unwrap()))?;
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
    /// Returns `true` if there is at least one native connection to the source,
    /// `false` otherwise. A connection does not establish progress of any
    /// particular media stream; see the
    /// [reception monitoring guide](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/usage.md#monitor-reception-of-the-expected-media).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("CAMERA (Program)").unwrap()))?;
    /// if receiver.is_connected() {
    ///     println!("Still connected to source");
    /// } else {
    ///     println!("Lost connection to source");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    /// A negative native count reports no established connection; use
    /// [`Self::connection_stats`] to inspect that SDK contract error.
    #[doc(alias = "NDIlib_recv_get_no_connections")]
    pub fn is_connected(&self) -> bool {
        self.core.connections().is_ok_and(|count| count > 0)
    }

    /// Re-establish this receiver's connection to its latest requested target,
    /// in place.
    ///
    /// Re-points the existing NDI receiver instance at the [`Source`] it
    /// most recently requested through [`Self::connect_to`] (or its initial
    /// source) without destroying and recreating the
    /// receiver. This is intended for mid-session recovery: when a
    /// source's feed has gone silent (for example an encoder-bound
    /// NDI|HX camera that dropped its proxy stream under load), a
    /// reconnect forces a fresh connection attempt while avoiding the
    /// source-rediscovery round trip — and the discovery race — that
    /// tearing down and rebuilding the receiver would incur.
    ///
    /// This requests the last local target again. If a Discovery Server client
    /// remotely changed this receiver's source, reconnecting replaces that
    /// remote routing with the local target. Observe remote changes through
    /// [`Self::wait_for_source_name_change`].
    ///
    /// Observe native connection state with [`Self::is_connected`] and
    /// progress of the expected media with [`Self::connection_stats`]. A
    /// reconnect alone does not establish that new media has arrived.
    ///
    /// # Thread safety
    ///
    /// Local connection changes serialize with one another. They may overlap
    /// captures, including FrameSync and AV sync, so this method does not wait
    /// for a Rust capture gate. Native synchronization and the request itself
    /// still run on the calling thread; async code should use
    /// `AsyncReceiver::reconnect` to run that work on its blocking pool.
    ///
    /// Uses the retained, validated target without allocation or conversion.
    /// `NDIlib_recv_connect` reports no status: returning means the request was
    /// issued, not that the feed recovered. Confirm recovery through
    /// [`Self::connection_stats`].
    #[doc(alias = "NDIlib_recv_connect")]
    pub fn reconnect(&self) {
        self.core.lock_connection().reconnect();
    }

    /// An owned snapshot of the latest locally requested connection target.
    ///
    /// Initially the source specified in [`ReceiverOptions`], updated by each
    /// successful [`Self::connect_to`]. [`Self::disconnect`] retains it, and
    /// [`Self::reconnect`] requests it again. A Discovery Server client's remote
    /// connection change does not update this local target. Observe actual
    /// connection changes with [`Self::wait_for_source_name_change`].
    /// The snapshot clones the source's name and address. It never waits for
    /// captures or queued connection changes; a concurrent `connect_to` holds
    /// the target lock only while replacing the value after its request returns.
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};
    /// # fn main() -> grafton_ndi::Result<()> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("CAMERA (Program)").unwrap()))?;
    /// println!("Requested target: {}", receiver.connection_target());
    /// # Ok(())
    /// # }
    /// ```
    pub fn connection_target(&self) -> Source {
        self.core.target.read().current().clone()
    }

    /// Get connection and performance statistics for this receiver.
    ///
    /// Provides detailed statistics including:
    /// - Number of active connections
    /// - Total frames received (video, audio, metadata)
    /// - Dropped frames (video, audio, metadata)
    /// - Queued frames waiting to be processed
    ///
    /// The native received totals include frames later dropped from the
    /// receiver queue or processing. These three native calls do not form an
    /// atomic connection/performance/queue snapshot. Counters may reset on
    /// connection changes; see [`ConnectionStats`] and the
    /// [reception monitoring guide](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/usage.md#monitor-reception-of-the-expected-media).
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] when any native count is negative.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("CAMERA (Program)").unwrap()))?;
    /// let stats = receiver.connection_stats()?;
    /// println!("Connections: {connections}", connections = stats.connections());
    /// println!("Video frames: {received} (dropped: {dropped})",
    ///          received = stats.video_frames_received(),
    ///          dropped = stats.video_frames_dropped());
    /// println!("Frame drop rate: {rate:.2}%",
    ///          rate = stats.video_drop_percentage());
    /// # Ok(())
    /// # }
    /// ```
    #[doc(alias = "NDIlib_recv_get_no_connections")]
    #[doc(alias = "NDIlib_recv_get_performance")]
    #[doc(alias = "NDIlib_recv_get_queue")]
    pub fn connection_stats(&self) -> Result<ConnectionStats> {
        let connections = self.core.connections()?;

        let mut total = NDIlib_recv_performance_t::default();
        let mut dropped = NDIlib_recv_performance_t::default();
        // SAFETY: the receiver instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        let performance = unsafe {
            self.core.sdk().named().NDIlib_recv_get_performance(
                self.core.instance.0,
                &mut total,
                &mut dropped,
            )
        };

        let mut queue = NDIlib_recv_queue_t::default();
        // SAFETY: the receiver instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        let queued = unsafe {
            self.core
                .sdk()
                .named()
                .NDIlib_recv_get_queue(self.core.instance.0, &mut queue)
        };

        Ok(ConnectionStats {
            connections,
            video_frames_received: performance.map(|()| total.video_frames).count()?,
            audio_frames_received: performance.map(|()| total.audio_frames).count()?,
            metadata_frames_received: performance.map(|()| total.metadata_frames).count()?,
            video_frames_dropped: performance.map(|()| dropped.video_frames).count()?,
            audio_frames_dropped: performance.map(|()| dropped.audio_frames).count()?,
            metadata_frames_dropped: performance.map(|()| dropped.metadata_frames).count()?,
            video_frames_queued: queued.map(|()| queue.video_frames).count()?,
            audio_frames_queued: queued.map(|()| queue.audio_frames).count()?,
            metadata_frames_queued: queued.map(|()| queue.metadata_frames).count()?,
        })
    }
}

/// Event polling is available only before a synchronizer takes the receiver.
impl<M: EventMode> Receiver<M> {
    /// Wait up to `timeout` for a status or source change, or return `None`
    /// when none came.
    ///
    /// Events are coalesced invalidations, including those that media capture
    /// passed over and kept, not an ordered history or broadcast stream.
    /// Concurrent pollers and combined captures compete to consume a pending one. The new source
    /// name is read with the SDK's separate [`Self::wait_for_source_name_change`]
    /// API.
    ///
    /// The timeout is spent as the typed capture methods spend it: in attempts of
    /// at most 100 ms, each of which first takes a pending event and otherwise
    /// waits in the SDK for the rest of the attempt. The SDK's early returns
    /// are waited over, so `None`
    /// comes only once the whole timeout is spent, and [`Duration::ZERO`]
    /// makes one attempt without waiting. An event that a concurrent capture
    /// keeps is returned by the next attempt. Connection changes may run
    /// concurrently with these attempts.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`crate::MAX_TIMEOUT`]
    /// (~49.7 days), and [`Error::ConnectionLost`] when the SDK reports a lost
    /// connection, or [`Error::UnexpectedSdkResult`] when a status snapshot
    /// contains a negative native connection count, once per publication.
    /// Its invalidation stays pending for recovery, without hiding other events
    /// or preventing subsequent native captures.
    #[doc(alias = "NDIlib_recv_capture_v3")]
    pub fn next_event(&self, timeout: Duration) -> Result<Option<ReceiverEvent>> {
        self.next_event_until(Deadline::after(timeout)?, NeverStop)
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

    /// Issue a permit tied to this receiver and its local connection lock.
    fn lock_connection(&self) -> Connection<'_> {
        Connection {
            core: self,
            _exclusive: self.connection_guard.lock(),
        }
    }

    /// Bind `attempt` to this receiver and its runtime, without taking the
    /// connection guard. Connection changes may overlap native captures.
    ///
    /// This is the only way to make a [`CaptureLock`], and so to call
    /// `NDIlib_recv_capture_v3` on this instance.
    fn capture_lock(&self, attempt: Attempt) -> CaptureLock<'_> {
        CaptureLock {
            core: self,
            attempt,
        }
    }

    /// Hold the local connection lock, so tests can verify captures and target
    /// snapshots proceed while another connection change is queued.
    #[cfg(all(test, any(feature = "advanced", feature = "tokio", feature = "smol")))]
    pub(crate) fn reconnect_lock(&self) -> impl Drop + '_ {
        self.lock_connection()._exclusive
    }

    /// One SDK capture of kind `K` within `attempt`.
    ///
    /// Runs concurrently with other captures and connection changes. `None`
    /// when the SDK returned without a frame of this kind; a
    /// notification it returned instead is kept for [`Receiver::next_event`].
    fn poll_ref_until<K: CaptureKind>(&self, attempt: Attempt) -> Result<Retry<K::Ref<'_>>> {
        let lock = self.capture_lock(attempt);
        let result = capture_raw::<K>(&lock)?;
        // Classify the result itself, rather than inspecting pending events:
        // another consumer may take a retained event concurrently.
        let progress = matches!(result, CaptureResult::Notification(n) if n != Notification::None);
        Ok(match self.capture_result::<K>(result)? {
            Some(frame) => Retry::Item(frame),
            None if progress => Retry::Progress,
            None => Retry::Idle,
        })
    }

    fn capture_result<'rx, K: CaptureKind>(
        &'rx self,
        result: CaptureResult<'rx, K>,
    ) -> Result<Option<K::Ref<'rx>>> {
        match result {
            CaptureResult::Frame(guard) => {
                // Validation (e.g. FourCC) happens during ref construction.
                let frame_ref = K::make_ref(guard)?;
                Ok(Some(frame_ref))
            }
            CaptureResult::Notification(notification) => {
                self.retain(notification);
                Ok(None)
            }
        }
    }

    /// The one capture loop, behind every borrowed and owned
    /// capture: SDK captures in [`capture_within`]'s attempts until one
    /// delivers a frame of kind `K` or `deadline` is spent.
    fn capture_ref_until<K: CaptureKind>(
        &self,
        deadline: Deadline,
        stop: impl Stop,
    ) -> Result<Option<K::Ref<'_>>> {
        capture_within(deadline, stop, |attempt| self.poll_ref_until::<K>(attempt))
    }

    /// The mixed-capture kernel, shared by raw and packet receivers and
    /// notification-only polling. A pending event is taken before the native
    /// attempt; events retained during that attempt merge with its result.
    /// Only media reaches `convert`, retaining
    /// its guards through the caller's validation and copy.
    fn capture_any_until_with<'rx, S: SlotSet, T>(
        &'rx self,
        deadline: Deadline,
        stop: impl Stop,
        mut event: impl FnMut(ReceiverReport<ReceiverEvent>) -> T,
        mut convert: impl FnMut(S::Media<'rx>) -> Result<T>,
    ) -> Result<Option<T>> {
        capture_within(deadline, stop, |attempt| {
            if let Some(pending) = self.take_pending_event_report()? {
                return Ok(Some(event(pending)));
            }
            let lock = self.capture_lock(attempt);
            let delivered = capture_any::<S>(&lock)?;
            self.capture_any_report_result_with(delivered, &mut event, &mut convert)
        })
    }

    /// The kernel's delivery boundary. Notifications always coalesce through
    /// `event`, including a native miss when a typed capture retained an event
    /// during the wait. A caller can convert only media from its slot set.
    #[cfg(all(test, feature = "advanced"))]
    pub(crate) fn capture_any_result_with<'rx, S: SlotSet, T>(
        &'rx self,
        delivered: Delivered<'rx, S>,
        event: impl FnOnce(ReceiverEvent) -> T,
        convert: impl FnOnce(S::Media<'rx>) -> Result<T>,
    ) -> Result<Option<T>> {
        self.capture_any_report_result_with(delivered, |report| event(report.value), convert)
    }

    fn capture_any_report_result_with<'rx, S: SlotSet, T>(
        &'rx self,
        delivered: Delivered<'rx, S>,
        event: impl FnOnce(ReceiverReport<ReceiverEvent>) -> T,
        convert: impl FnOnce(S::Media<'rx>) -> Result<T>,
    ) -> Result<Option<T>> {
        match delivered {
            Delivered::Media(media) => convert(media).map(Some),
            Delivered::Notification(notification) => {
                self.retain(notification);
                self.take_pending_event_report().map(|item| item.map(event))
            }
        }
    }

    pub(crate) fn combined_result(media: Media<'_>) -> Result<ReceiverItemRef<'_>> {
        Ok(match media {
            Media::Video(guard) => ReceiverItemRef::Video(VideoKind::make_ref(guard)?),
            Media::Audio(guard) => ReceiverItemRef::Audio(AudioKind::make_ref(guard)?),
            Media::Metadata(guard) => ReceiverItemRef::Metadata(MetadataKind::make_ref(guard)?),
        })
    }

    #[cfg(test)]
    fn take_pending_status(&self) -> Result<Option<ReceiverStatus>> {
        self.pending.status.take_with(|| self.current_status())
    }

    fn connections(&self) -> Result<u32> {
        // SAFETY: the receiver is live for this nonblocking native query.
        unsafe {
            self.sdk()
                .named()
                .NDIlib_recv_get_no_connections(self.instance.0)
        }
        .count()
    }

    fn current_status(&self) -> Result<ReceiverStatus> {
        Ok(ReceiverStatus {
            connections: self.connections()?,
        })
    }

    /// Keep a notification a capture reported for [`Receiver::next_event`].
    fn retain(&self, notification: Notification) {
        self.pending.publish(notification);
    }

    /// Return an undelivered async event without overwriting a newer report.
    #[cfg(any(feature = "tokio", feature = "smol"))]
    pub(crate) fn retain_event(&self, event: ReceiverReport<ReceiverEvent>) {
        self.pending.replay(event);
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    pub(crate) fn retain_source_name_change(&self, change: ReceiverReport<SourceNameChange>) {
        self.source_name.lock().replay(change);
    }

    fn take_pending_event_report(&self) -> Result<Option<ReceiverReport<ReceiverEvent>>> {
        self.pending.take(|| self.current_status())
    }

    /// The one implementation of [`Receiver::wait_for_source_name_change`], also
    /// behind the async receivers' version: wait until `deadline` for a
    /// change of the connected source's name.
    ///
    /// The tables are resolved after the caller validated the timeout, so an
    /// invalid timeout is reported before a missing capability.
    pub(crate) fn wait_for_source_name_change_until(
        &self,
        deadline: Deadline,
        stop: impl Stop,
    ) -> Result<Option<SourceNameChange>> {
        // The query comes with the function that releases what it returns.
        let (source_name, strings) = self.source_name_tables()?;
        self.wait_for_source_name_change_with(source_name, strings, deadline, stop)
            .map(|change| change.map(ReceiverReport::into_value))
    }

    pub(crate) fn source_name_tables(
        &self,
    ) -> Result<&'static (sys::ReceiverSourceName, sys::ReceiverStrings)> {
        self._ndi.table()
    }

    /// [`Self::wait_for_source_name_change_until`] through the given tables, so
    /// that a test can substitute the SDK functions.
    pub(crate) fn wait_for_source_name_change_with(
        &self,
        source_name: &sys::ReceiverSourceName,
        strings: &sys::ReceiverStrings,
        deadline: Deadline,
        stop: impl Stop,
    ) -> Result<Option<ReceiverReport<SourceNameChange>>> {
        capture_within(deadline, stop, |attempt| {
            if let Some(change) = self.source_name.lock().pending.take() {
                return Ok(Some(change));
            }
            let Some(_query) = self.source_query_guard.try_lock_until(attempt.0.end()) else {
                return Ok(None);
            };
            // A worker may have replayed while this poller waited for the permit.
            if let Some(change) = self.source_name.lock().pending.take() {
                return Ok(Some(change));
            }
            let mut pointer = ptr::null();
            let remaining = attempt.0.remaining_ms();
            // SAFETY: the receiver instance is live for this borrow and `pointer` is
            // writable storage for the returned string.
            let changed = unsafe {
                source_name.named().NDIlib_recv_get_source_name(
                    self.instance.0,
                    &mut pointer,
                    remaining,
                )
            };
            let guard = RecvStringGuard {
                core: self,
                string: changed.map(|_| pointer),
                strings,
            };
            if !changed.into_value() {
                return Ok(self.source_name.lock().pending.take());
            }
            // Still under the query permit: even a malformed newer report
            // supersedes older abandoned reports before its text is decoded.
            let sequence = self.source_name.lock().advance();
            // The SDK reports a change only once, since the previous call, so a
            // strict decoding error here would lose the name for good.
            let change = match guard.copy_lossy()? {
                Some(name) => SourceNameChange::Connected(name),
                None => SourceNameChange::Disconnected,
            };
            Ok(Some(ReceiverReport {
                sequence,
                value: change,
            }))
        })
    }
}

/// Proof of one receiver, runtime and remaining [`Attempt`] for a native
/// capture. Only the receiver's permission-gated entry points make one, through
/// [`ReceiverCore::capture_lock`]; it holds no connection lock.
///
/// `Processing.NDI.Recv.h` documents simultaneous captures, and the SDK manual
/// permits connecting "at any time" without explicitly naming this overlap.
/// The connection/capture decision rests on native observations and tests on
/// Linux x86-64, as the FrameSync threading decision does:
///
/// - In Standard 6.1.1 and Standard/Advanced 6.3.2 and 6.4.0.2, connection
///   changes take the receiver's internal rwlock exclusively and capture takes
///   the same lock shared (receiver offset 0x108). Capture releases it around
///   a native wait and reacquires it before accessing the connection again.
/// - Helgrind 3.19 with four capture threads and 1080p feeds on Standard and
///   Advanced 6.3.2 and 6.4.0.2 reports direct cross connect/capture accesses
///   only to the byte polling flag at offset 0x152: single-byte loads, and stores
///   followed by MFENCE. This is consistent with fenced atomic access that
///   Helgrind does not model; a valid std::atomic byte/fence control compiled
///   to the same instructions reproduces its warning. These are qualified
///   detector results, not an assertion that Helgrind reported no errors.
/// - With the Rust exclusion bypassed, sixteen raw capture threads and local
///   connect/disconnect/reconnect storms ran for 120 seconds on each of those
///   five runtimes with 1920x1080 BGRA/UYVY video, audio and metadata. Frames
///   validated without tearing, calls stayed within the checked budgets,
///   progress resumed, and every delivered loan was released. Separate brief
///   runs on each runtime counted matching native captures and frees directly.
/// - Brief FrameSync and AV sync overlap cases, and Discovery Server remote
///   control on 6.4, also validate delivery and recovery. Remote changes happen
///   inside the SDK and are outside any Rust connection lock.
///
/// `tests/receiver_connect_overlap_sdk.rs` repeats the applicable cases briefly
/// on every runtime/platform the suite runs against. The retained core keeps
/// the native instance and its runtime live for the capture and its frames.
pub(crate) struct CaptureLock<'rx> {
    core: &'rx ReceiverCore,
    attempt: Attempt,
}

impl<'rx> CaptureLock<'rx> {
    /// The runtime the receiver was created through, which frees the frames
    /// a capture under this lock delivers.
    pub(crate) fn runtime(&self) -> &'rx Ndi {
        &self.core._ndi
    }

    /// The receiver instance a capture under this lock is made on.
    pub(crate) fn instance(&self) -> NDIlib_recv_instance_t {
        self.core.instance.0
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
        // Read before the call below, so it is at least the wait the SDK gets.
        #[cfg(test)]
        native_capture_calls::record(self.attempt.0.remaining_ms());
        // SAFETY: the receiver instance is live for 'rx, which this lock
        // borrows it for. Each descriptor is null or exclusively borrowed,
        // writable storage that outlives the synchronous call. The proof fixes
        // this instance and its runtime. Connection changes may overlap this
        // call, as supported by the native observations beside CaptureLock.
        unsafe {
            self.core.sdk().NDIlib_recv_capture_v3(
                self.core.instance.0,
                video.map_or(ptr::null_mut(), ptr::from_mut),
                audio.map_or(ptr::null_mut(), ptr::from_mut),
                metadata.map_or(ptr::null_mut(), ptr::from_mut),
                self.attempt.0.remaining_ms(),
            )
        }
    }
}

/// Counts native capture calls on the calling thread, so that tests can tell
/// a capture that was skipped from one that found nothing, and keeps the
/// longest wait any of them was given, so that they can tell how long the SDK
/// was allowed to block whatever it then took.
#[cfg(test)]
pub(crate) mod native_capture_calls {
    use std::cell::Cell;
    std::thread_local! {
        static CALLS: Cell<usize> = const { Cell::new(0) };
        static LONGEST_WAIT_MS: Cell<u32> = const { Cell::new(0) };
    }
    /// Count a call that the SDK is allowed to block for `wait_ms` at most.
    pub(crate) fn record(wait_ms: u32) {
        CALLS.with(|calls| calls.set(calls.get() + 1));
        LONGEST_WAIT_MS.with(|longest| longest.set(longest.get().max(wait_ms)));
    }
    /// The calls made since the last take.
    pub(crate) fn take() -> usize {
        take_with_longest_wait().0
    }
    /// The calls made since the last take, and the longest wait, in
    /// milliseconds, that any of them was given.
    pub(crate) fn take_with_longest_wait() -> (usize, u32) {
        (
            CALLS.with(|calls| calls.replace(0)),
            LONGEST_WAIT_MS.with(|longest| longest.replace(0)),
        )
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

impl std::fmt::Debug for Ptz<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ptz")
            .field(
                "receiver_name",
                &self.core._name.as_ref().map(|name| name.to_string_lossy()),
            )
            .finish_non_exhaustive()
    }
}

impl Ptz<'_> {
    /// Whether the connected source accepts PTZ commands.
    #[must_use]
    #[doc(alias = "NDIlib_recv_ptz_is_supported")]
    pub fn is_supported(&self) -> bool {
        // SAFETY: the receiver instance is live for the handle's borrow.
        unsafe {
            self.core
                .sdk()
                .NDIlib_recv_ptz_is_supported(self.core.instance.0)
        }
    }

    /// Recall preset `0..=99`, moving at `speed` within `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    #[doc(alias = "NDIlib_recv_ptz_recall_preset")]
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
    #[doc(alias = "NDIlib_recv_ptz_store_preset")]
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
    #[doc(alias = "NDIlib_recv_ptz_zoom")]
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
    #[doc(alias = "NDIlib_recv_ptz_zoom_speed")]
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
    #[doc(alias = "NDIlib_recv_ptz_pan_tilt")]
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
    #[doc(alias = "NDIlib_recv_ptz_pan_tilt_speed")]
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
    #[doc(alias = "NDIlib_recv_ptz_auto_focus")]
    pub fn auto_focus(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_auto_focus)
    }

    /// Set the absolute focus within `0.0..=1.0`: zero focuses at infinity,
    /// one as close as possible.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    #[doc(alias = "NDIlib_recv_ptz_focus")]
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
    #[doc(alias = "NDIlib_recv_ptz_focus_speed")]
    pub fn focus_speed(&self, speed: f32) -> Result<()> {
        validate_ptz_value("focus speed", speed, -1.0, 1.0)?;
        ptz_command!(self, NDIlib_recv_ptz_focus_speed, speed)
    }

    /// Balance white automatically.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    #[doc(alias = "NDIlib_recv_ptz_white_balance_auto")]
    pub fn white_balance_auto(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_white_balance_auto)
    }

    /// Select the indoor white-balance preset.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    #[doc(alias = "NDIlib_recv_ptz_white_balance_indoor")]
    pub fn white_balance_indoor(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_white_balance_indoor)
    }

    /// Select the outdoor white-balance preset.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    #[doc(alias = "NDIlib_recv_ptz_white_balance_outdoor")]
    pub fn white_balance_outdoor(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_white_balance_outdoor)
    }

    /// Balance white once, from the current image.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    #[doc(alias = "NDIlib_recv_ptz_white_balance_oneshot")]
    pub fn white_balance_oneshot(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_white_balance_oneshot)
    }

    /// Set the red and blue white-balance levels within `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    #[doc(alias = "NDIlib_recv_ptz_white_balance_manual")]
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
    #[doc(alias = "NDIlib_recv_ptz_exposure_auto")]
    pub fn exposure_auto(&self) -> Result<()> {
        ptz_command!(self, NDIlib_recv_ptz_exposure_auto)
    }

    /// Set the exposure level within `0.0..=1.0`, from dark to light.
    ///
    /// # Errors
    ///
    /// See [`Ptz`].
    #[doc(alias = "NDIlib_recv_ptz_exposure_manual")]
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
    #[doc(alias = "NDIlib_recv_ptz_exposure_manual_v2")]
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
            self.sdk().NDIlib_recv_destroy(self.instance.0);
        }
        #[cfg(test)]
        lifecycle_test_events::record("receiver");
    }
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ReceiverCore>();
};

// A panic cannot leave a receiver's shared state half-changed: of its locks,
// the target and pending unknown event are replaced whole; the string-query
// lock protects complete retained changes. The rest of its mutable state is atomic, and no
// application code runs while a
// lock is held. The `parking_lot` locks do not poison and so do not declare
// this themselves, which would otherwise leave `Receiver` short of
// `RefUnwindSafe` and a `RecvAdvertiser`, which keeps registered receivers
// alive, short of the unwind safety a `SendAdvertiser` has.
impl std::panic::RefUnwindSafe for ReceiverCore {}

/// A source-name change observed by the native receiver.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SourceNameChange {
    /// The receiver is now connected to the named source.
    Connected(String),
    /// The receiver is no longer connected to any source.
    Disconnected,
}

/// An owned item delivered by [`Receiver::capture_any`] or an async
/// receiver's combined capture. Media buffers and metadata can outlive the
/// receiver and move between threads.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ReceiverItem {
    /// Validated uncompressed video with packed active rows.
    Video(VideoFrame),
    /// Validated planar floating-point audio with packed channels.
    Audio(AudioFrame),
    /// Standalone frame metadata.
    Metadata(MetadataFrame),
    /// A consumed, coalesced receiver invalidation.
    Event(ReceiverEvent),
}

/// An item delivered by [`Receiver::capture_any_ref`]. Media borrows native
/// storage and keeps the receiver borrowed until its matching guard is
/// dropped. Borrowed capture allocates and copies no payload; video row and
/// audio channel padding are never exposed as initialized slices.
///
/// These items implement `Send + Sync` under the same SDK contract as the
/// typed receiver references. They may move to scoped worker threads while
/// retaining their receiver borrow. Use [`Self::to_owned`] to keep an item
/// after releasing the receiver.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReceiverItemRef<'rx> {
    /// Validated uncompressed video, possibly with padded rows or planes.
    Video(VideoFrameRef<'rx>),
    /// Validated planar audio, possibly with padded channels.
    Audio(AudioFrameRef<'rx>),
    /// Standalone frame metadata.
    Metadata(MetadataFrameRef<'rx>),
    /// A consumed, coalesced receiver invalidation; no native buffer.
    Event(ReceiverEvent),
}

impl ReceiverItemRef<'_> {
    /// Copy media into an owned item using the typed capture copy paths.
    /// Video rows and audio channels become packed; timing and metadata are
    /// preserved. Video/audio allocate one payload buffer and metadata when
    /// present; metadata allocates its text; events simply copy their value.
    /// This makes no SDK call and retains no native frame or receiver borrow.
    pub fn to_owned(&self) -> ReceiverItem {
        match self {
            Self::Video(frame) => ReceiverItem::Video(frame.to_owned()),
            Self::Audio(frame) => ReceiverItem::Audio(frame.to_owned()),
            Self::Metadata(frame) => ReceiverItem::Metadata(frame.to_owned()),
            Self::Event(event) => ReceiverItem::Event(*event),
        }
    }
}

/// A coalesced receiver invalidation, not an ordered event history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReceiverEvent {
    /// The SDK reported changed status information, such as source capabilities.
    ///
    /// Connection loss can produce no such event. Poll [`Receiver::is_connected`]
    /// or [`Receiver::connection_stats`] to track the connection itself.
    StatusChanged(ReceiverStatus),
    /// The connected source changed; poll its name for the new value.
    SourceChanged,
    /// An unknown non-media event value; no unknown payload is exposed.
    Unknown(u32),
}

/// What a status-change notification reports: the current connection count.
///
/// The notification may also mean that the source's capabilities or web
/// control changed; the SDK does not say which. Its connection count is a
/// snapshot taken when the event is consumed, and connection loss need not
/// produce an event. Poll [`Receiver::is_connected`] or
/// [`Receiver::connection_stats`] for connection tracking. Tally is published
/// upstream through [`Receiver::set_tally`], rather than received here.
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
/// [`Receiver::connection_stats`]. These are native counter snapshots, not an
/// atomic view of connection, performance and queue state. Counters can reset
/// when a connection changes. Received totals include frames later dropped by
/// the receiver queue or processing; drops are not a count of lost network
/// packets or a measure of downstream playback quality. For interval rates,
/// reset the baseline when either counter decreases or the source/connection
/// is explicitly reset.
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

    /// The native total number of video frames received, including later drops.
    #[must_use]
    pub const fn video_frames_received(&self) -> u64 {
        self.video_frames_received
    }

    /// The native total number of audio frames received, including later drops.
    #[must_use]
    pub const fn audio_frames_received(&self) -> u64 {
        self.audio_frames_received
    }

    /// The native total number of metadata frames received, including later drops.
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

    /// Cumulative receiver video queue/processing drops as a percentage of
    /// the native received total. Returns 0.0 for zero received frames and is
    /// bounded to 0..=100 even when native snapshots disagree.
    #[must_use]
    pub fn video_drop_percentage(&self) -> f64 {
        drop_percentage(self.video_frames_received, self.video_frames_dropped)
    }

    /// Cumulative receiver audio queue/processing drops as a percentage of
    /// the native received total. Returns 0.0 for zero received frames and is
    /// bounded to 0..=100 even when native snapshots disagree.
    #[must_use]
    pub fn audio_drop_percentage(&self) -> f64 {
        drop_percentage(self.audio_frames_received, self.audio_frames_dropped)
    }

    /// Cumulative receiver metadata queue/processing drops as a percentage of
    /// the native received total. Returns 0.0 for zero received frames and is
    /// bounded to 0..=100 even when native snapshots disagree.
    #[must_use]
    pub fn metadata_drop_percentage(&self) -> f64 {
        drop_percentage(self.metadata_frames_received, self.metadata_frames_dropped)
    }

    /// Whether there is at least one native connection; this does not
    /// establish progress of a particular media stream.
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connections > 0
    }
}

fn drop_percentage(received: u64, dropped: u64) -> f64 {
    if received == 0 {
        0.0
    } else {
        100.0 * (dropped.min(received) as f64 / received as f64)
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
    pub(crate) fn snapshot() -> Vec<&'static str> {
        EVENTS.with(|events| events.borrow().as_ref().unwrap().clone())
    }
    pub(crate) fn track(action: impl FnOnce()) -> Vec<&'static str> {
        EVENTS.with(|events| *events.borrow_mut() = Some(Vec::new()));
        action();
        EVENTS.with(|events| events.borrow_mut().take().unwrap())
    }
}

#[cfg(test)]
#[path = "receiver_mixed_tests.rs"]
mod mixed_tests;

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::Cell;

    #[cfg(feature = "advanced")]
    #[test]
    fn packet_capture_consumes_the_same_pending_events_as_raw_capture() {
        use crate::advanced::{PacketItem, PacketReceiver};

        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected())
            .unwrap()
            .into_packet_for_test();
        // Model notifications retained before a mode transition. Packet capture
        // must consume them once without spending its timeout in native capture.
        receiver.core.retain(Notification::StatusChanged);
        receiver.core.retain(Notification::SourceChanged);
        receiver.core.retain(Notification::Unknown(987));
        native_capture_calls::take();
        for expected in [
            ReceiverEvent::StatusChanged(ReceiverStatus { connections: 0 }),
            ReceiverEvent::SourceChanged,
            ReceiverEvent::Unknown(987),
        ] {
            let Some(PacketItem::Event(actual)) =
                PacketReceiver::capture_from(&receiver, Duration::from_secs(1)).unwrap()
            else {
                panic!("expected receiver event {expected:?}");
            };
            assert_eq!(actual, expected);
        }
        assert_eq!(native_capture_calls::take(), 0);
        assert!(receiver.core.take_pending_event_report().unwrap().is_none());
    }

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
        // Both directions share one bound: what may be set may be reported.
        for millis in [0, 1, 1999, 2000] {
            let delay = receiver_delay(millis).unwrap();
            assert_eq!(receiver_delay_millis(delay).unwrap(), millis);
        }
        for millis in [-1, 2001, i32::MIN, i32::MAX] {
            assert!(matches!(
                receiver_delay(millis),
                Err(Error::UnexpectedSdkResult(_))
            ));
        }
    }

    #[test]
    fn receive_64_modes_require_the_selected_runtime_before_native_creation() {
        for version in ["6.1.1.0", "6.4.0.2"] {
            sys::with_test_evidence(version, |evidence| {
                let supported = version == "6.4.0.2";
                let best =
                    ReceiverOptions::new(test_source()).color_format(ReceiverColorFormat::Best444);
                let validated = best.validate_runtime(evidence);
                assert_eq!(validated.is_ok(), supported);
                if let Ok(validated) = validated {
                    assert!(validated._best444.is_some());
                    assert_eq!(validated.to_raw().unwrap().raw.color_format, 102);
                }
                for (bandwidth, value) in [
                    (ReceiverBandwidth::VideoOnlyLowest, 1),
                    (ReceiverBandwidth::VideoOnlyHighest, 101),
                ] {
                    let options = ReceiverOptions::new(test_source()).bandwidth(bandwidth);
                    let validated = options.validate_runtime(evidence);
                    assert_eq!(validated.is_ok(), supported);
                    if let Ok(validated) = validated {
                        assert!(validated._video_only.is_some());
                        assert_eq!(validated.to_raw().unwrap().raw.bandwidth, value);
                    }
                }
                assert!(unconnected().validate_runtime(evidence).is_ok());
            });
        }
    }

    #[test]
    fn typed_capture_paths_publish_notifications() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        fn check<K: CaptureKind>(receiver: &Receiver) {
            assert!(receiver
                .core
                .capture_result::<K>(CaptureResult::Notification(Notification::SourceChanged))
                .unwrap()
                .is_none());
            assert!(matches!(
                receiver.next_event(Duration::ZERO).unwrap(),
                Some(ReceiverEvent::SourceChanged)
            ));
        }
        check::<VideoKind>(&receiver);
        check::<AudioKind>(&receiver);
        check::<MetadataKind>(&receiver);
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
    fn reconnect_reuses_validated_name_url_and_ip_targets_without_allocating() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        for source in [
            Source::new("retained name").unwrap(),
            Source::new("retained URL")
                .unwrap()
                .with_address(crate::SourceAddress::new("ndi://127.0.0.1:5961").unwrap()),
            Source::new("retained IP")
                .unwrap()
                .with_address(crate::SourceAddress::new("127.0.0.1:5961").unwrap()),
        ] {
            let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source.clone())).unwrap();
            let ((), allocations) = crate::test_alloc::allocation_count(|| receiver.reconnect());
            assert_eq!(allocations, 0);
            assert_eq!(receiver.connection_target(), source);
            receiver.disconnect();
            let ((), allocations) = crate::test_alloc::allocation_count(|| receiver.reconnect());
            assert_eq!(allocations, 0);
            assert_eq!(receiver.connection_target(), source);
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
                string: sys::table::for_test(
                    CString::new(bytes).unwrap().into_raw().cast_const(),
                    c"NDIlib_recv_get_web_control",
                ),
                strings: &strings,
            };
            assert_eq!(guard.copy().is_ok(), valid);
            drop(guard);
        }
        let empty = RecvStringGuard {
            core: &receiver.core,
            string: sys::table::for_test(ptr::null(), c"NDIlib_recv_get_web_control"),
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
        let max = crate::sdk_copy::MAX_SDK_STRING_BYTES;
        let copy = |bytes: &[u8]| {
            RecvStringGuard {
                core: &receiver.core,
                string: sys::table::for_test(bytes.as_ptr().cast(), c"NDIlib_recv_get_source_name"),
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
        let (result, allocations) = crate::test_alloc::allocation_count(|| copy(&unterminated));
        assert!(
            matches!(result, Err(Error::UnexpectedSdkResult(crate::SdkResultDefect::UnterminatedText { operation }))
            if operation == c"NDIlib_recv_get_source_name")
        );
        assert_eq!(allocations, 0);
        let guard = RecvStringGuard {
            core: &receiver.core,
            string: sys::table::for_test(
                unterminated.as_ptr().cast(),
                c"NDIlib_recv_get_web_control",
            ),
            strings: &strings,
        };
        assert!(
            matches!(guard.copy(), Err(Error::UnexpectedSdkResult(crate::SdkResultDefect::UnterminatedText { operation }))
            if operation == c"NDIlib_recv_get_web_control")
        );
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
            let change = receiver.core.wait_for_source_name_change_with(
                &source_name,
                &strings,
                Deadline::after(Duration::ZERO).unwrap(),
                NeverStop,
            );
            (
                change.unwrap().map(ReceiverReport::into_value),
                FREES.load(Ordering::Relaxed) - before,
            )
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

    #[test]
    fn source_name_wait_retries_bounded_queries_and_honors_cancellation() {
        use std::cell::RefCell;
        std::thread_local! {
            static CALLS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
        }
        unsafe extern "C" fn get_source_name(
            _instance: NDIlib_recv_instance_t,
            name: *mut *const c_char,
            timeout_ms: u32,
        ) -> bool {
            let changed = CALLS.with(|calls| {
                let mut calls = calls.borrow_mut();
                calls.push(timeout_ms);
                calls.len() == 3
            });
            unsafe {
                name.write(if changed {
                    c"later source".as_ptr()
                } else {
                    ptr::null()
                })
            };
            changed
        }
        unsafe extern "C" fn free(_instance: NDIlib_recv_instance_t, _name: *const c_char) {}
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
        let result = receiver
            .core
            .wait_for_source_name_change_with(
                &source_name,
                &strings,
                Deadline::after(Duration::from_secs(2)).unwrap(),
                NeverStop,
            )
            .unwrap();
        assert_eq!(
            result.map(ReceiverReport::into_value),
            Some(SourceNameChange::Connected("later source".into()))
        );
        CALLS.with(|calls| {
            assert_eq!(calls.borrow().len(), 3);
            assert!(calls.borrow().iter().all(|&wait| wait <= 100));
            calls.borrow_mut().clear();
        });
        // The entry gate already authorized the first attempt; stopping affects
        // later attempts even when a query returns immediately with no change.
        let stopped = AtomicBool::new(true);
        assert!(receiver
            .core
            .wait_for_source_name_change_with(
                &source_name,
                &strings,
                Deadline::after(Duration::from_secs(2)).unwrap(),
                &stopped,
            )
            .unwrap()
            .is_none());
        CALLS.with(|calls| assert_eq!(calls.borrow().len(), 1));
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    mod replay_tests {
        use super::*;
        use std::{cell::Cell, ffi::CStr};

        thread_local! {
            static NEXT: Cell<Option<&'static CStr>> = const { Cell::new(None) };
            static CALLS: Cell<usize> = const { Cell::new(0) };
        }

        unsafe extern "C" fn get_source_name(
            _instance: NDIlib_recv_instance_t,
            name: *mut *const c_char,
            _timeout_ms: u32,
        ) -> bool {
            CALLS.with(|calls| calls.set(calls.get() + 1));
            let next = NEXT.take();
            // SAFETY: the caller provides writable storage and the string is static.
            unsafe { name.write(next.map_or(ptr::null(), CStr::as_ptr)) };
            next.is_some()
        }
        unsafe extern "C" fn free(_instance: NDIlib_recv_instance_t, _name: *const c_char) {}

        fn poll(
            core: &ReceiverCore,
            name: Option<&'static CStr>,
        ) -> Option<ReceiverReport<SourceNameChange>> {
            NEXT.set(name);
            core.wait_for_source_name_change_with(
                &sys::ReceiverSourceName {
                    NDIlib_recv_get_source_name: get_source_name,
                },
                &sys::ReceiverStrings {
                    NDIlib_recv_free_string: free,
                },
                Deadline::after(Duration::ZERO).unwrap(),
                NeverStop,
            )
            .unwrap()
        }

        #[test]
        fn only_the_newest_abandoned_name_precedes_a_native_query() {
            let Some(ndi) = crate::test_support::ndi() else {
                return;
            };
            let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
            let first = poll(&receiver.core, Some(c"first")).unwrap();
            let second = poll(&receiver.core, Some(c"second")).unwrap();
            receiver.core.retain_source_name_change(second);
            receiver.core.retain_source_name_change(first);
            let before = CALLS.get();
            assert_eq!(
                poll(&receiver.core, None).unwrap().value,
                SourceNameChange::Connected("second".into())
            );
            assert_eq!(CALLS.get(), before);
            assert!(poll(&receiver.core, None).is_none());
            assert_eq!(CALLS.get(), before + 1);
        }

        #[test]
        fn replay_does_not_wait_for_another_pollers_native_query() {
            static ENTERED: AtomicBool = AtomicBool::new(false);
            unsafe extern "C" fn slow_query(
                _instance: NDIlib_recv_instance_t,
                name: *mut *const c_char,
                _timeout_ms: u32,
            ) -> bool {
                ENTERED.store(true, Ordering::Release);
                std::thread::sleep(Duration::from_millis(100));
                // SAFETY: the caller provides writable storage for the pointer.
                unsafe { name.write(c"second".as_ptr()) };
                true
            }
            let Some(ndi) = crate::test_support::ndi() else {
                return;
            };
            let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
            let abandoned = poll(&receiver.core, Some(c"first")).unwrap();
            ENTERED.store(false, Ordering::Release);
            std::thread::scope(|scope| {
                let worker = scope.spawn(|| {
                    receiver
                        .core
                        .wait_for_source_name_change_with(
                            &sys::ReceiverSourceName {
                                NDIlib_recv_get_source_name: slow_query,
                            },
                            &sys::ReceiverStrings {
                                NDIlib_recv_free_string: free,
                            },
                            Deadline::after(Duration::from_millis(100)).unwrap(),
                            NeverStop,
                        )
                        .unwrap()
                });
                let waiting = Instant::now();
                while !ENTERED.load(Ordering::Acquire) {
                    assert!(waiting.elapsed() < Duration::from_secs(2));
                    std::thread::yield_now();
                }
                let started = Instant::now();
                receiver.core.retain_source_name_change(abandoned);
                assert!(started.elapsed() < Duration::from_millis(10));
                assert_eq!(
                    worker.join().unwrap().unwrap().value,
                    SourceNameChange::Connected("second".into())
                );
                assert!(poll(&receiver.core, None).is_none());
            });
        }
    }

    /// A source no receiver can connect to: no name and no address.
    fn test_source() -> Source {
        Source::new("").unwrap()
    }

    /// The options of a receiver that stays unconnected.
    fn unconnected() -> ReceiverOptions {
        ReceiverOptions::new(test_source())
    }

    /// A clock that moves only when a test advances it, to drive
    /// [`capture_within_clock`].
    struct FakeClock {
        start: Instant,
        elapsed: Cell<Duration>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                start: Instant::now(),
                elapsed: Cell::new(Duration::ZERO),
            }
        }

        fn now(&self) -> Instant {
            self.start + self.elapsed.get()
        }

        fn elapsed(&self) -> Duration {
            self.elapsed.get()
        }

        fn advance(&self, duration: Duration) {
            self.elapsed.set(self.elapsed.get() + duration);
        }

        /// A caller's budget that started when this clock did.
        fn deadline(&self, budget: Duration) -> Deadline {
            Deadline::starting(self.start, crate::timeout::Timeout::new(budget).unwrap()).unwrap()
        }

        /// What is left of `attempt` now: the timeout its SDK call would get,
        /// for the current attempt.
        fn timeout(&self, attempt: Attempt) -> Duration {
            attempt.0.remaining_at(self.now())
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
    fn pending_status_keeps_failed_and_newer_publications() {
        let pending = PendingStatus::default();
        pending.publish();
        assert!(matches!(
            pending.take_with::<()>(|| Err(Error::ConnectionLost)),
            Err(Error::ConnectionLost)
        ));
        assert!(pending.is_pending());
        assert_eq!(
            pending
                .take_with(|| {
                    pending.publish();
                    Ok(1)
                })
                .unwrap(),
            None
        );
        assert!(
            pending.is_pending(),
            "a publication during validation must survive"
        );
        assert_eq!(pending.take_with(|| Ok(2)).unwrap(), Some(2));
        assert!(!pending.is_pending());
        pending.publish();
        assert!(pending
            .take_with::<()>(|| {
                pending.publish();
                Err(Error::ConnectionLost)
            })
            .unwrap()
            .is_none());
        assert!(pending.is_pending());
        assert_eq!(pending.take_with(|| Ok(3)).unwrap(), Some(3));
    }

    #[test]
    fn pending_status_reports_each_publication_once_and_retries_validation() {
        let pending = PendingStatus::default();
        for _ in 0..2 {
            pending.publish();
            assert!(matches!(
                pending.take_with::<()>(|| Err(Error::ConnectionLost)),
                Err(Error::ConnectionLost)
            ));
            assert!(pending
                .take_with::<()>(|| Err(Error::ConnectionLost))
                .unwrap()
                .is_none());
            assert!(pending.is_pending());
        }
        assert_eq!(pending.take_with(|| Ok(7)).unwrap(), Some(7));
        assert!(!pending.is_pending());
    }

    #[test]
    fn pending_status_defers_failure_reporting_when_another_event_is_ready() {
        let pending = PendingStatus::default();
        pending.publish();
        assert_eq!(
            pending
                .take_or_else_with(|| Err(Error::ConnectionLost), || Some(7))
                .unwrap(),
            Some(7)
        );
        assert!(matches!(
            pending.take_with::<()>(|| Err(Error::ConnectionLost)),
            Err(Error::ConnectionLost)
        ));
    }

    #[test]
    fn pending_status_failure_has_one_competing_reporter() {
        let pending = PendingStatus::default();
        pending.publish();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        pending
                            .take_with::<()>(|| {
                                barrier.wait();
                                Err(Error::ConnectionLost)
                            })
                            .is_err()
                    })
                })
                .collect();
            assert_eq!(
                workers
                    .into_iter()
                    .map(|worker| worker.join().unwrap())
                    .filter(|reported| *reported)
                    .count(),
                1
            );
        });
        assert!(pending.is_pending());
    }

    #[test]
    fn pending_status_validation_has_one_competing_consumer() {
        let pending = PendingStatus::default();
        pending.publish();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        pending
                            .take_with(|| {
                                barrier.wait();
                                Ok(())
                            })
                            .unwrap()
                            .is_some()
                    })
                })
                .collect();
            assert_eq!(
                workers
                    .into_iter()
                    .map(|worker| worker.join().unwrap())
                    .filter(|taken| *taken)
                    .count(),
                1
            );
        });
        assert!(!pending.is_pending());
    }

    #[test]
    fn pending_status_survives_an_invalid_next_event_timeout() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        receiver.core.pending.status.publish();
        assert!(receiver.next_event(Duration::MAX).is_err());
        assert!(receiver.core.pending.status.is_pending());
        assert!(matches!(
            receiver.next_event(Duration::ZERO).unwrap(),
            Some(ReceiverEvent::StatusChanged(_))
        ));
        assert!(!receiver.core.pending.status.is_pending());
    }

    #[test]
    fn every_receiver_wait_refuses_a_timeout_above_max_timeout() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        let excessive = crate::MAX_TIMEOUT + Duration::from_nanos(1);
        for result in [
            receiver.capture_video(excessive).map(drop),
            receiver.capture_audio_ref(excessive).map(drop),
            receiver.capture_metadata(excessive).map(drop),
            receiver.capture_any(excessive).map(drop),
            receiver.capture_any_ref(excessive).map(drop),
            receiver.next_event(excessive).map(drop),
            receiver.wait_for_source_name_change(excessive).map(drop),
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
        receiver.core.pending.status.publish();
        let barrier = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let receiver = &receiver;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        receiver.core.take_pending_status().unwrap().is_some()
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
        receiver.core.pending.status.publish();
        assert!(receiver.core.take_pending_status().unwrap().is_some());
    }

    #[test]
    fn connection_target_waits_for_neither_captures_nor_queued_connection_changes() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        let expected = receiver.connection_target();
        let connection = receiver.core.lock_connection();
        std::thread::scope(|scope| {
            let (started, waiting) = std::sync::mpsc::channel();
            let receiver = &receiver;
            let reconnect = scope.spawn(move || {
                started.send(()).unwrap();
                receiver.reconnect();
            });
            waiting.recv_timeout(Duration::from_secs(2)).unwrap();
            let (done, completed) = std::sync::mpsc::channel();
            let getter = scope.spawn(move || {
                done.send(receiver.connection_target()).unwrap();
            });
            let snapshot = completed.recv_timeout(Duration::from_millis(20));
            // Release the connection lock before inspecting the outcome, so a
            // regression fails instead of leaving scoped threads deadlocked.
            drop(connection);
            getter.join().unwrap();
            reconnect.join().unwrap();
            assert_eq!(snapshot.expect("the getter waited for capture"), expected);
        });
        assert_eq!(receiver.connection_target(), expected);
    }

    #[test]
    fn reconnect_returns_while_a_capture_attempt_is_in_flight() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        std::thread::scope(|scope| {
            let (done, completed) = std::sync::mpsc::channel();
            let result = {
                let _attempt = receiver
                    .core
                    .capture_lock(Attempt(Deadline::after(Duration::from_secs(1)).unwrap()));
                let receiver = &receiver;
                scope.spawn(move || {
                    receiver.reconnect();
                    done.send(()).unwrap();
                });
                // Keep the attempt live across this wait. An accidental Rust
                // exclusion would hold the reconnect off until this block ends.
                completed.recv_timeout(Duration::from_millis(20))
            };
            result.expect("reconnect waited for the capture attempt");
        });
    }

    #[test]
    fn combined_capture_consumes_pending_events_once_even_during_reconnect() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        receiver.core.retain(Notification::SourceChanged);
        let _exclusive = receiver.core.lock_connection();
        native_capture_calls::take();
        assert!(matches!(
            receiver.capture_any_ref(Duration::ZERO).unwrap(),
            Some(ReceiverItemRef::Event(ReceiverEvent::SourceChanged))
        ));
        assert_eq!(native_capture_calls::take(), 0);
        assert!(receiver.capture_any_ref(Duration::ZERO).unwrap().is_none());
        assert_eq!(native_capture_calls::take(), 1);
        let began = Instant::now();
        assert!(receiver
            .capture_any(Duration::from_millis(20))
            .unwrap()
            .is_none());
        assert!(began.elapsed() >= Duration::from_millis(20));
        assert!(native_capture_calls::take() >= 1);
    }

    #[test]
    fn combined_and_event_consumers_compete_for_one_retained_invalidation() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        receiver
            .core
            .capture_result::<VideoKind>(CaptureResult::Notification(Notification::Unknown(987)))
            .unwrap();
        let barrier = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            let combined = scope.spawn(|| {
                barrier.wait();
                matches!(
                    receiver.capture_any(Duration::ZERO).unwrap(),
                    Some(ReceiverItem::Event(ReceiverEvent::Unknown(987)))
                )
            });
            let events = scope.spawn(|| {
                barrier.wait();
                matches!(
                    receiver.next_event(Duration::ZERO).unwrap(),
                    Some(ReceiverEvent::Unknown(987))
                )
            });
            barrier.wait();
            assert_eq!(
                usize::from(combined.join().unwrap()) + usize::from(events.join().unwrap()),
                1
            );
        });
        receiver.core.retain(Notification::Unknown(988));
        assert!(matches!(
            receiver.capture_any(Duration::ZERO).unwrap(),
            Some(ReceiverItem::Event(ReceiverEvent::Unknown(988)))
        ));
    }

    #[test]
    fn receiver_mode_transitions_preserve_pending_status_and_core_identity() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        let original_core = Arc::as_ptr(&receiver.core);
        receiver.core.pending.status.publish();
        let fs = crate::FrameSync::new(receiver).unwrap();
        assert!(fs.receiver().core.pending.status.is_pending());
        assert_eq!(Arc::as_ptr(&fs.receiver().core), original_core);
        let receiver = fs.into_receiver();
        assert!(matches!(
            receiver.next_event(Duration::ZERO).unwrap(),
            Some(ReceiverEvent::StatusChanged(_))
        ));
        assert!(!receiver.core.pending.status.is_pending());
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
        assert!((stats.video_drop_percentage() - 100.0 / 9.0).abs() < 1e-12);
        assert_eq!(stats.audio_drop_percentage(), 0.0);
        assert_eq!(stats.metadata_drop_percentage(), 0.0);
        let per_media = ConnectionStats {
            audio_frames_received: 1000,
            audio_frames_dropped: 100,
            metadata_frames_received: 350,
            metadata_frames_dropped: 285,
            ..stats
        };
        assert_eq!(per_media.audio_drop_percentage(), 10.0);
        assert!((per_media.metadata_drop_percentage() - 81.42857142857143).abs() < 1e-12);
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
        assert_eq!(drop_percentage(1000, 0), 0.0);
        assert_eq!(drop_percentage(1000, 1000), 100.0);
        assert_eq!(drop_percentage(1000, 100), 10.0);
        assert!((drop_percentage(900, 100) - 100.0 / 9.0).abs() < 1e-12);
        assert!((drop_percentage(350, 285) - 81.42857142857143).abs() < 1e-12);
        assert_eq!(drop_percentage(u64::MAX, u64::MAX), 100.0);
        assert_eq!(drop_percentage(0, u64::MAX), 0.0);
        assert_eq!(drop_percentage(1, u64::MAX), 100.0);
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
        let pointer = frame.metadata_raw().p_data;
        for _ in 0..3 {
            let sent = receiver.send_metadata(&frame);
            assert!(
                matches!(sent, Ok(()) | Err(Error::NotConnected)),
                "{sent:?}"
            );
            assert_eq!(frame.metadata_raw().p_data, pointer);
        }
    }

    #[test]
    fn captures_keep_their_budgets_while_local_connection_changes_are_locked() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        let exclusive = receiver.core.lock_connection();
        std::thread::scope(|scope| {
            let (done, completed) = std::sync::mpsc::channel();
            let receiver = &receiver;
            let worker = scope.spawn(move || {
                native_capture_calls::take();
                assert!(receiver.capture_video(Duration::ZERO).unwrap().is_none());
                assert!(receiver.next_event(Duration::ZERO).unwrap().is_none());
                assert!(receiver
                    .capture_audio(Duration::from_millis(5))
                    .unwrap()
                    .is_none());
                assert!(receiver
                    .capture_metadata_ref(Duration::from_millis(5))
                    .unwrap()
                    .is_none());
                done.send(native_capture_calls::take()).unwrap();
            });
            // The connection lock remains held until capture returns. Taking
            // it in a capture would stall instead of reaching the SDK.
            let result = completed.recv_timeout(Duration::from_secs(2));
            drop(exclusive);
            worker.join().unwrap();
            assert!(result.unwrap() >= 4);
        });
    }

    /// A long `next_event` uses bounded attempts alongside connection changes
    /// and other captures. An event retained by another capture ends that poll
    /// at its next attempt.
    #[test]
    fn a_long_next_event_allows_reconnect_and_observes_a_retained_event() {
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
                    let event = receiver.next_event(Duration::from_secs(5)).unwrap();
                    if stop.load(Ordering::Acquire) {
                        return event;
                    }
                }
            });
            started_polling.recv().unwrap();
            // Let the poller settle into a native wait.
            std::thread::sleep(Duration::from_millis(250));

            let capture = scope.spawn(move || {
                native_capture_calls::take();
                let began = Instant::now();
                let frame = receiver.capture_metadata(budget).unwrap();
                (
                    frame.is_none(),
                    began.elapsed(),
                    native_capture_calls::take(),
                )
            });
            let began = Instant::now();
            receiver.reconnect();
            let reconnected = began.elapsed();
            let (nothing_arrived, captured_for, sdk_calls) = capture.join().unwrap();

            stop.store(true, Ordering::Release);
            let published = Instant::now();
            receiver.core.retain(Notification::StatusChanged);
            let ended = poller.join().unwrap();
            let poll_ended_after = published.elapsed();

            assert!(reconnected < Duration::from_secs(1), "{reconnected:?}");
            assert!(nothing_arrived);
            // The concurrent capture reached the SDK and spent its budget
            // there, regardless of the local connection change.
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

    /// `next_event` gives each SDK call one poll interval of its timeout at
    /// most.
    ///
    /// What each call is given, and that the poll ends only once the budget
    /// is spent, hold however long the SDK then takes. How many calls fit in
    /// the budget does not: on a loaded runner the SDK can overrun its waits,
    /// so that only two fit. The count is the capture loop's, which the
    /// clock-driven tests below check, such as
    /// `retry_follows_a_full_wait_at_once`.
    #[test]
    fn next_event_spends_its_timeout_in_bounded_attempts() {
        use std::time::Instant;
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &unconnected()).unwrap();
        // A zero timeout makes exactly one attempt, with a zero SDK wait.
        native_capture_calls::take();
        assert!(receiver.next_event(Duration::ZERO).unwrap().is_none());
        assert_eq!(native_capture_calls::take_with_longest_wait(), (1, 0));
        // Nothing is connected: the whole budget is spent, in attempts of at
        // most one poll interval each.
        let budget = Duration::from_millis(450);
        let began = Instant::now();
        assert!(receiver.next_event(budget).unwrap().is_none());
        assert!(began.elapsed() >= budget, "{:?}", began.elapsed());
        let (calls, longest_wait) = native_capture_calls::take_with_longest_wait();
        assert!(calls >= 1, "{calls} SDK calls");
        assert!(
            u128::from(longest_wait) <= POLL_INTERVAL.as_millis(),
            "an SDK call was given {longest_wait} ms"
        );
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

    fn raw_settings(options: &ReceiverOptions) -> RawRecvCreateV3 {
        sys::with_test_evidence("6.4.0.2", |evidence| {
            options
                .validate_runtime(evidence)
                .unwrap()
                .to_raw()
                .unwrap()
        })
    }

    #[test]
    fn receiver_options_defaults_emit_operational_raw_modes() {
        let raw = raw_settings(&unconnected());

        assert_raw_receiver_modes(
            &raw,
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA,
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
        );
        assert!(raw.raw.allow_video_fields);
    }

    #[test]
    fn snapshot_preset_emits_operational_raw_modes() {
        let raw = raw_settings(&ReceiverOptions::snapshot_preset(test_source()));

        assert_raw_receiver_modes(
            &raw,
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA,
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
        );
        assert!(!raw.raw.allow_video_fields);
    }

    #[test]
    fn high_quality_preset_emits_operational_raw_modes() {
        let raw = raw_settings(&ReceiverOptions::high_quality_preset(test_source()));

        assert_raw_receiver_modes(
            &raw,
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA,
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
        );
        assert!(raw.raw.allow_video_fields);
    }

    #[test]
    fn monitoring_preset_emits_operational_raw_modes() {
        let raw = raw_settings(&ReceiverOptions::monitoring_preset(test_source()));

        assert_raw_receiver_modes(
            &raw,
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA,
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_metadata_only,
        );
        assert!(!raw.raw.allow_video_fields);
    }

    #[test]
    fn retry_notifications_reach_a_queued_frame_without_sleeping() {
        for count in 1..=IMMEDIATE_NOTIFICATION_RETRIES {
            let clock = FakeClock::new();
            let mut calls = 0;
            let result = capture_within_clock(
                clock.deadline(Duration::from_secs(1)),
                |_| {
                    calls += 1;
                    Ok(if calls <= count {
                        Retry::Progress
                    } else {
                        Retry::Item(42)
                    })
                },
                || clock.now(),
                |_| panic!("a queued frame after a notification must not be delayed"),
            );
            assert_eq!(result.unwrap(), Some(42));
            assert_eq!(calls, count + 1);
        }
    }

    #[test]
    fn cancellation_stops_before_another_attempt_or_pending_event() {
        assert_eq!(std::mem::size_of::<NeverStop>(), 0);
        for progress in [false, true] {
            let clock = FakeClock::new();
            let cancelled = AtomicBool::new(false);
            let mut calls = 0;
            let mut pending = false;
            let result: Result<Option<()>> = capture_within_clock_with_stop(
                clock.deadline(Duration::from_secs(30)),
                &cancelled,
                |_| {
                    calls += 1;
                    assert!(!pending, "a stopped retry consumed the pending event");
                    pending = true;
                    cancelled.store(true, Ordering::Release);
                    Ok(if progress {
                        Retry::Progress
                    } else {
                        Retry::Idle
                    })
                },
                || clock.now(),
                |pause| clock.advance(pause),
            );
            assert!(result.unwrap().is_none());
            assert_eq!(calls, 1);
            assert!(pending);
        }
    }

    #[test]
    fn cancellation_preserves_the_item_from_the_attempt_in_flight() {
        let clock = FakeClock::new();
        let cancelled = AtomicBool::new(false);
        let result = capture_within_clock_with_stop(
            clock.deadline(Duration::from_secs(30)),
            &cancelled,
            |_| {
                cancelled.store(true, Ordering::Release);
                Ok(Some(42))
            },
            || clock.now(),
            |_| panic!("delivered items must not wait again"),
        );
        assert_eq!(result.unwrap(), Some(42));
    }

    #[test]
    fn retry_notification_storm_is_bounded_and_idle_still_backs_off() {
        for idle in [false, true] {
            let clock = FakeClock::new();
            let mut calls = 0;
            let mut sleeps = 0;
            let result: Result<Option<()>> = capture_within_clock(
                clock.deadline(Duration::from_millis(25)),
                |_| {
                    calls += 1;
                    Ok(if idle && calls % 2 == 0 {
                        Retry::Idle
                    } else {
                        Retry::Progress
                    })
                },
                || clock.now(),
                |pause| {
                    sleeps += 1;
                    clock.advance(pause);
                },
            );
            assert!(result.unwrap().is_none());
            assert_eq!(sleeps, 3);
            assert!(calls <= 3 * (IMMEDIATE_NOTIFICATION_RETRIES + 1));
            assert_eq!(clock.elapsed(), Duration::from_millis(25));
        }
    }

    #[test]
    fn retry_zero_or_spent_budget_observes_only_one_notification() {
        for spent in [Duration::ZERO, Duration::from_millis(30)] {
            let clock = FakeClock::new();
            let deadline = clock.deadline(Duration::ZERO);
            clock.advance(spent);
            let mut calls = 0;
            let result: Result<Option<()>> = capture_within_clock(
                deadline,
                |attempt| {
                    calls += 1;
                    assert!(clock.timeout(attempt).is_zero());
                    Ok(Retry::Progress)
                },
                || clock.now(),
                |_| panic!("spent budgets must not sleep"),
            );
            assert!(result.unwrap().is_none());
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn retry_caps_sub_poll_timeout_to_total_budget() {
        let clock = FakeClock::new();
        let mut polls = Vec::new();

        let result = capture_within_clock(
            clock.deadline(Duration::from_millis(25)),
            |attempt| {
                polls.push(clock.timeout(attempt));
                Ok(Some(42))
            },
            || clock.now(),
            |_| {},
        );

        assert_eq!(result.unwrap(), Some(42));
        assert_eq!(polls, [Duration::from_millis(25)]);
    }

    #[test]
    fn retry_shrinks_later_poll_to_remaining_budget() {
        let clock = FakeClock::new();
        let mut polls = Vec::new();
        let mut sleeps = Vec::new();

        let result: Result<Option<i32>> = capture_within_clock(
            clock.deadline(Duration::from_millis(110)),
            |attempt| {
                let poll = clock.timeout(attempt);
                polls.push(poll);
                if polls.len() == 1 {
                    clock.advance(Duration::from_millis(5));
                } else {
                    clock.advance(poll);
                }
                Ok(None)
            },
            || clock.now(),
            |sleep| {
                sleeps.push(sleep);
                clock.advance(sleep);
            },
        );

        assert!(result.unwrap().is_none());
        assert_eq!(
            polls,
            [Duration::from_millis(100), Duration::from_millis(95)]
        );
        assert_eq!(sleeps, [Duration::from_millis(10)]);
    }

    #[test]
    fn retry_zero_timeout_success_makes_one_nonblocking_attempt() {
        let clock = FakeClock::new();
        let mut polls = Vec::new();

        let result = capture_within_clock(
            clock.deadline(Duration::ZERO),
            |attempt| {
                polls.push(clock.timeout(attempt));
                Ok(Some(42))
            },
            || clock.now(),
            |_| panic!("zero-timeout success should not sleep"),
        );

        assert_eq!(result.unwrap(), Some(42));
        assert_eq!(polls, [Duration::ZERO]);
    }

    #[test]
    fn retry_zero_timeout_miss_gives_up_after_one_attempt() {
        let clock = FakeClock::new();
        let mut polls = Vec::new();

        let result: Result<Option<i32>> = capture_within_clock(
            clock.deadline(Duration::ZERO),
            |attempt| {
                polls.push(clock.timeout(attempt));
                Ok(None)
            },
            || clock.now(),
            |_| panic!("zero-timeout miss should not sleep"),
        );

        assert!(result.unwrap().is_none());
        assert_eq!(polls, [Duration::ZERO]);
    }

    #[test]
    fn retry_stops_when_an_attempt_spends_the_budget() {
        let clock = FakeClock::new();
        let mut polls = Vec::new();

        let result: Result<Option<i32>> = capture_within_clock(
            clock.deadline(Duration::from_millis(120)),
            |attempt| {
                polls.push(clock.timeout(attempt));
                // The SDK overran the attempt's timeout and the budget.
                clock.advance(Duration::from_millis(120));
                Ok(None)
            },
            || clock.now(),
            |_| panic!("expired timeout should not sleep"),
        );

        assert!(result.unwrap().is_none());
        assert_eq!(polls, [POLL_INTERVAL]);
    }

    #[test]
    fn retry_caps_sleep_to_remaining_budget() {
        let clock = FakeClock::new();
        let mut sleeps = Vec::new();

        let mut attempts = 0;

        let result: Result<Option<i32>> = capture_within_clock(
            clock.deadline(Duration::from_millis(105)),
            |attempt| {
                attempts += 1;
                assert_eq!(clock.timeout(attempt), POLL_INTERVAL);
                // An early return, 1 ms short of the attempt's timeout.
                clock.advance(Duration::from_millis(99));
                Ok(None)
            },
            || clock.now(),
            |sleep| {
                sleeps.push(sleep);
                clock.advance(sleep);
            },
        );

        assert!(result.unwrap().is_none());
        assert_eq!(attempts, 1);
        assert_eq!(sleeps, [Duration::from_millis(6)]);
    }

    #[test]
    fn retry_follows_a_full_wait_at_once() {
        // Every attempt waits out its timeout, as when no frame is sent: the
        // next attempt starts at once, so a frame arriving in between is taken
        // without a pause.
        let clock = FakeClock::new();
        let mut polls = Vec::new();

        let result: Result<Option<i32>> = capture_within_clock(
            clock.deadline(Duration::from_millis(250)),
            |attempt| {
                let poll = clock.timeout(attempt);
                polls.push(poll);
                clock.advance(poll);
                Ok(None)
            },
            || clock.now(),
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
        let clock = FakeClock::new();
        let mut polls = Vec::new();
        let mut sleeps = Vec::new();
        let waits = [3, 0, 100].map(Duration::from_millis);

        let result = capture_within_clock(
            clock.deadline(Duration::from_secs(1)),
            |attempt| {
                let Some(wait) = waits.get(polls.len()).copied() else {
                    return Ok(Some(42));
                };
                polls.push(clock.timeout(attempt));
                clock.advance(wait);
                Ok(None)
            },
            || clock.now(),
            |sleep| {
                sleeps.push(sleep);
                clock.advance(sleep);
            },
        );

        assert_eq!(result.unwrap(), Some(42));
        assert_eq!(polls, [Duration::from_millis(100); 3]);
        assert_eq!(sleeps, [EARLY_RETURN_PAUSE; 2]);
        // One deadline for the whole call: 3 + 10 + 0 + 10 + 100 ms.
        assert_eq!(clock.elapsed(), Duration::from_millis(123));
    }

    #[test]
    fn every_attempt_ends_within_one_poll_interval_and_the_callers_deadline() {
        // Early returns, full waits and overruns alike: every attempt's
        // requested wait ends within one poll interval and the caller's budget.
        let clock = FakeClock::new();
        let deadline = clock.deadline(Duration::from_millis(1000));
        let waits = [3, 100, 0, 130, 100, 40].map(Duration::from_millis);
        let mut attempts = 0;

        let result: Result<Option<i32>> = capture_within_clock(
            deadline,
            |attempt| {
                assert!(attempt.0.end() <= deadline.end());
                assert!(attempt.0.end() <= clock.now() + POLL_INTERVAL);
                clock.advance(waits[attempts % waits.len()]);
                attempts += 1;
                Ok(None)
            },
            || clock.now(),
            |sleep| clock.advance(sleep),
        );

        assert!(result.unwrap().is_none());
        assert!(attempts > waits.len(), "{attempts} attempts");
        assert!(clock.elapsed() >= Duration::from_millis(1000));
    }

    #[test]
    fn retry_timeout_counts_time_spent_before_the_first_attempt() {
        // A caller's 50 ms budget of which 30 ms went before the retry began,
        // as when an async capture waited for a blocking-pool thread.
        let clock = FakeClock::new();
        let deadline = clock.deadline(Duration::from_millis(50));
        clock.advance(Duration::from_millis(30));
        let mut polls = Vec::new();

        let result: Result<Option<i32>> = capture_within_clock(
            deadline,
            |attempt| {
                let poll = clock.timeout(attempt);
                polls.push(poll);
                clock.advance(poll);
                Ok(None)
            },
            || clock.now(),
            |_| panic!("an expired budget should not sleep"),
        );

        assert!(result.unwrap().is_none());
        assert_eq!(clock.elapsed(), Duration::from_millis(50));
        assert_eq!(polls, [Duration::from_millis(20)]);
    }

    #[test]
    fn retry_with_a_spent_budget_makes_one_nonblocking_attempt() {
        let clock = FakeClock::new();
        let deadline = clock.deadline(Duration::from_millis(50));
        clock.advance(Duration::from_millis(70));
        let mut polls = Vec::new();

        let result: Result<Option<i32>> = capture_within_clock(
            deadline,
            |attempt| {
                polls.push(clock.timeout(attempt));
                Ok(None)
            },
            || clock.now(),
            |_| panic!("a spent budget should not sleep"),
        );

        assert!(result.unwrap().is_none());
        assert_eq!(polls, [Duration::ZERO]);
    }

    #[test]
    fn capture_within_measures_the_callers_budget() {
        let deadline = Deadline::after(Duration::from_millis(20)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let mut polls = Vec::new();

        let result: Result<Option<i32>> = capture_within(deadline, NeverStop, |attempt| {
            polls.push(attempt.0.remaining());
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
        let clock = FakeClock::new();
        let attempts = Cell::new(0);

        let result = capture_within_clock(
            clock.deadline(Duration::from_millis(50)),
            |_| {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 1 {
                    Ok(None)
                } else {
                    Ok(Some(42))
                }
            },
            || clock.now(),
            |sleep| clock.advance(sleep),
        );

        assert_eq!(result.unwrap(), Some(42));
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn retry_propagates_error() {
        let clock = FakeClock::new();
        let attempts = Cell::new(0);
        let mut sleeps = Vec::new();

        let result: Result<Option<i32>> = capture_within_clock(
            clock.deadline(Duration::from_secs(1)),
            |_| {
                attempts.set(attempts.get() + 1);
                Err::<Option<i32>, _>(Error::ConnectionLost)
            },
            || clock.now(),
            |sleep| sleeps.push(sleep),
        );

        assert!(
            matches!(result, Err(Error::ConnectionLost)),
            "Should propagate ConnectionLost error"
        );
        assert_eq!(attempts.get(), 1);
        assert!(sleeps.is_empty());
    }
}

// This fixture is compiled in the crate, so it exercises internal gates rather
// than failing because an external doctest cannot access private methods.
#[cfg(ndi_test_capture_permissions)]
#[path = "../tests/ui/internal/capture_permissions.rs"]
mod capture_permission_fixture;
