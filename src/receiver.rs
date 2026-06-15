//! NDI receiving functionality for video, audio, and metadata.
//!
//! # Monitoring Tally & Connection Count
//!
//! The receiver can monitor status changes including tally state and connection count:
//!
//! ```ignore
//! # use grafton_ndi::{NDI, ReceiverOptions, ReceiverBandwidth, Source};
//! # use std::time::Duration;
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! # let ndi = NDI::new()?;
//! // In real usage, you'd get the source from Finder::find_sources()
//! // let source = /* obtained from Finder */;
//! let options = ReceiverOptions::builder(source)
//!     .bandwidth(ReceiverBandwidth::MetadataOnly)
//!     .build();
//! let receiver = Receiver::new(&ndi, &options)?;
//!
//! // Poll for status changes
//! if let Some(status) = receiver.poll_status_change(Duration::from_millis(1000))? {
//!     if let Some(tally) = status.tally {
//!         println!("Tally: program={program}, preview={preview}",
//!                  program = tally.on_program, preview = tally.on_preview);
//!     }
//!     if let Some(connections) = status.connections {
//!         println!("Active connections: {connections}");
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::{
    ffi::CString,
    marker::PhantomData,
    ptr,
    sync::{PoisonError, RwLock, RwLockReadGuard},
    time::{Duration, Instant},
};

use crate::{
    capture::{capture_raw, AudioKind, CaptureKind, CaptureResult, MetadataKind, VideoKind},
    finder::{RawSource, Source},
    ndi_lib::*,
    to_ms_checked, Error, Result, NDI,
};

/// Retry policy configuration for frame capture operations.
///
/// This struct encapsulates the timing parameters for the retry loop used by
/// the reliable [`Capture::capture`] verb across all frame kinds.
struct RetryPolicy {
    /// Timeout per individual capture attempt.
    poll_interval: Duration,
    /// Sleep duration between retry attempts to avoid busy-waiting.
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

/// Generic retry helper for frame capture operations.
///
/// This function encapsulates the retry logic that handles NDI SDK synchronization
/// behavior during initial connection. The first few capture calls may return
/// immediately while the stream synchronizes.
///
/// # Parameters
///
/// - `timeout`: Total time allowed for the operation to succeed.
/// - `policy`: Retry timing configuration.
/// - `capture_fn`: A closure that attempts to capture a frame with a given timeout.
///
/// # Returns
///
/// - `Ok(T)`: The captured frame on success.
/// - `Err(Error::FrameTimeout)`: If no frame is captured within the total timeout.
fn retry_capture<T, F>(timeout: Duration, policy: &RetryPolicy, capture_fn: F) -> Result<T>
where
    F: FnMut(Duration) -> Result<Option<T>>,
{
    let start_time = Instant::now();
    retry_capture_with_clock(
        timeout,
        policy,
        capture_fn,
        || start_time.elapsed(),
        std::thread::sleep,
    )
}

fn retry_capture_with_clock<T, F, E, S>(
    timeout: Duration,
    policy: &RetryPolicy,
    mut capture_fn: F,
    mut elapsed: E,
    mut sleep: S,
) -> Result<T>
where
    F: FnMut(Duration) -> Result<Option<T>>,
    E: FnMut() -> Duration,
    S: FnMut(Duration),
{
    to_ms_checked(timeout)?;

    let mut attempts = 0;

    if timeout.is_zero() {
        attempts += 1;
        return match capture_fn(Duration::ZERO)? {
            Some(frame) => Ok(frame),
            None => Err(Error::FrameTimeout {
                attempts,
                elapsed: elapsed(),
            }),
        };
    }

    loop {
        let elapsed_before_attempt = elapsed();
        let Some(remaining) = timeout.checked_sub(elapsed_before_attempt) else {
            return Err(Error::FrameTimeout {
                attempts,
                elapsed: elapsed_before_attempt,
            });
        };

        if remaining.is_zero() {
            return Err(Error::FrameTimeout {
                attempts,
                elapsed: elapsed_before_attempt,
            });
        }

        let poll_timeout = policy.poll_interval.min(remaining);
        attempts += 1;

        match capture_fn(poll_timeout)? {
            Some(frame) => return Ok(frame),
            None => {
                let elapsed_after_attempt = elapsed();
                let Some(remaining) = timeout.checked_sub(elapsed_after_attempt) else {
                    return Err(Error::FrameTimeout {
                        attempts,
                        elapsed: elapsed_after_attempt,
                    });
                };

                if remaining.is_zero() {
                    return Err(Error::FrameTimeout {
                        attempts,
                        elapsed: elapsed_after_attempt,
                    });
                }

                let sleep_duration = policy.sleep_between.min(remaining);
                if !sleep_duration.is_zero() {
                    sleep(sleep_duration);
                }
            }
        }
    }
}

macro_rules! ptz_command {
    ($self:expr, $func:ident, $err_msg:expr) => {
        if unsafe { $func($self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed($err_msg.into()))
        }
    };
    ($self:expr, $func:ident, $param:expr, $err_msg:expr) => {
        if unsafe { $func($self.instance, $param) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed($err_msg))
        }
    };
    ($self:expr, $func:ident, $param1:expr, $param2:expr, $err_msg:expr) => {
        if unsafe { $func($self.instance, $param1, $param2) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed($err_msg))
        }
    };
    ($self:expr, $func:ident, $param1:expr, $param2:expr, $param3:expr, $err_msg:expr) => {
        if unsafe { $func($self.instance, $param1, $param2, $param3) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed($err_msg))
        }
    };
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
/// [`ReceiverColorFormat::Best`], [`ReceiverColorFormat::RGBX_RGBA`], or
/// [`ReceiverColorFormat::BGRX_BGRA`] for receiver configuration.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReceiverColorFormat {
    #[default]
    BGRX_BGRA,
    UYVY_BGRA,
    RGBX_RGBA,
    UYVY_RGBA,
    Fastest,
    Best,
}

impl From<ReceiverColorFormat> for NDIlib_recv_color_format_e {
    fn from(format: ReceiverColorFormat) -> Self {
        match format {
            ReceiverColorFormat::BGRX_BGRA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA
            }
            ReceiverColorFormat::UYVY_BGRA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_BGRA
            }
            ReceiverColorFormat::RGBX_RGBA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA
            }
            ReceiverColorFormat::UYVY_RGBA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_RGBA
            }
            ReceiverColorFormat::Fastest => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_fastest
            }
            ReceiverColorFormat::Best => NDIlib_recv_color_format_e_NDIlib_recv_color_format_best,
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReceiverBandwidth {
    MetadataOnly,
    AudioOnly,
    Lowest,
    #[default]
    Highest,
}

impl From<ReceiverBandwidth> for NDIlib_recv_bandwidth_e {
    fn from(bandwidth: ReceiverBandwidth) -> Self {
        match bandwidth {
            ReceiverBandwidth::MetadataOnly => {
                NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_metadata_only
            }
            ReceiverBandwidth::AudioOnly => {
                NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_audio_only
            }
            ReceiverBandwidth::Lowest => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
            ReceiverBandwidth::Highest => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ReceiverOptions {
    pub source_to_connect_to: Source,
    pub color_format: ReceiverColorFormat,
    pub bandwidth: ReceiverBandwidth,
    pub allow_video_fields: bool,
    pub ndi_recv_name: Option<String>,
}

#[repr(C)]
pub(crate) struct RawRecvCreateV3 {
    _source: RawSource,
    _name: Option<CString>,
    pub raw: NDIlib_recv_create_v3_t,
}

impl ReceiverOptions {
    /// Convert to raw format for FFI use
    ///
    /// # Safety
    ///
    /// The returned RawRecvCreateV3 struct uses #[repr(C)] to guarantee C-compatible layout
    /// for safe FFI interop with the NDI SDK.
    pub(crate) fn to_raw(&self) -> Result<RawRecvCreateV3> {
        let source = self.source_to_connect_to.to_raw()?;
        let name = self
            .ndi_recv_name
            .as_ref()
            .map(|n| CString::new(n.clone()))
            .transpose()
            .map_err(Error::InvalidCString)?;

        let p_ndi_recv_name = name.as_ref().map_or(ptr::null(), |n| n.as_ptr());
        let source_raw = source.raw;

        Ok(RawRecvCreateV3 {
            raw: NDIlib_recv_create_v3_t {
                source_to_connect_to: source_raw,
                color_format: self.color_format.into(),
                bandwidth: self.bandwidth.into(),
                allow_video_fields: self.allow_video_fields,
                p_ndi_recv_name,
            },
            _source: source,
            _name: name,
        })
    }

    /// Create a builder for configuring a receiver
    pub fn builder(source: Source) -> ReceiverOptionsBuilder {
        ReceiverOptionsBuilder::new(source)
    }
}

/// Builder for configuring a ReceiverOptions with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct ReceiverOptionsBuilder {
    source_to_connect_to: Source,
    color_format: Option<ReceiverColorFormat>,
    bandwidth: Option<ReceiverBandwidth>,
    allow_video_fields: Option<bool>,
    ndi_recv_name: Option<String>,
}

impl ReceiverOptionsBuilder {
    /// Create a new builder with the specified source
    pub fn new(source: Source) -> Self {
        Self {
            source_to_connect_to: source,
            color_format: None,
            bandwidth: None,
            allow_video_fields: None,
            ndi_recv_name: None,
        }
    }

    /// Preset for capturing snapshots (low resolution, RGBA, lowest bandwidth).
    ///
    /// This preset is optimized for:
    /// - Image export and snapshot capture
    /// - AI/ML processing pipelines
    /// - Thumbnail generation
    /// - Low bandwidth environments
    ///
    /// Configuration:
    /// - Color format: `RGBX_RGBA` (compatible with image encoding)
    /// - Bandwidth: `Lowest` (reduces resolution and bitrate)
    /// - Video fields: Disabled (progressive frames only)
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, Finder, FinderOptions, ReceiverOptionsBuilder, Receiver};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_secs(1))?;
    /// # let sources = finder.current_sources()?;
    /// let options = ReceiverOptionsBuilder::snapshot_preset(sources[0].clone())
    ///     .name("Snapshot Receiver")
    ///     .build();
    /// let receiver = Receiver::new(&ndi, &options)?;
    ///
    /// // Capture and encode in one line (requires image-encoding feature)
    /// #[cfg(feature = "image-encoding")]
    /// {
    ///     let frame = receiver.video().capture(Duration::from_secs(5))?;
    ///     let png_bytes = frame.encode_png()?;
    ///     std::fs::write("snapshot.png", &png_bytes)?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn snapshot_preset(source: Source) -> Self {
        Self::new(source)
            .color(ReceiverColorFormat::RGBX_RGBA)
            .bandwidth(ReceiverBandwidth::Lowest)
            .allow_video_fields(false)
    }

    /// Preset for high-quality video processing (full resolution, highest bandwidth).
    ///
    /// This preset is optimized for:
    /// - Professional video processing workflows
    /// - Recording and archival
    /// - Real-time video analysis requiring full quality
    /// - Broadcasting and production
    ///
    /// Configuration:
    /// - Color format: `RGBX_RGBA` (uncompressed, compatible with most tools)
    /// - Bandwidth: `Highest` (full resolution and bitrate)
    /// - Video fields: Enabled (supports interlaced sources)
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, Finder, FinderOptions, ReceiverOptionsBuilder, Receiver};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_secs(1))?;
    /// # let sources = finder.current_sources()?;
    /// let options = ReceiverOptionsBuilder::high_quality_preset(sources[0].clone())
    ///     .name("High Quality Receiver")
    ///     .build();
    /// let receiver = Receiver::new(&ndi, &options)?;
    ///
    /// // Capture full quality frames
    /// let frame = receiver.video().capture(Duration::from_secs(5))?;
    /// println!("Captured {width}x{height} frame", width = frame.width(), height = frame.height());
    /// # Ok(())
    /// # }
    /// ```
    pub fn high_quality_preset(source: Source) -> Self {
        Self::new(source)
            .color(ReceiverColorFormat::RGBX_RGBA)
            .bandwidth(ReceiverBandwidth::Highest)
            .allow_video_fields(true)
    }

    /// Preset for metadata and tally monitoring only (no video/audio).
    ///
    /// This preset is optimized for:
    /// - Tally light monitoring
    /// - Connection status tracking
    /// - PTZ control applications
    /// - Minimal bandwidth overhead
    ///
    /// Configuration:
    /// - Bandwidth: `MetadataOnly` (no video or audio data)
    /// - Color format: Default (not used for metadata-only)
    /// - Video fields: Disabled (not applicable)
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, Finder, FinderOptions, ReceiverOptionsBuilder, Receiver};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(Duration::from_secs(1))?;
    /// # let sources = finder.current_sources()?;
    /// let options = ReceiverOptionsBuilder::monitoring_preset(sources[0].clone())
    ///     .name("Tally Monitor")
    ///     .build();
    /// let receiver = Receiver::new(&ndi, &options)?;
    ///
    /// // Poll for status changes
    /// if let Some(status) = receiver.poll_status_change(Duration::from_millis(1000))? {
    ///     if let Some(tally) = status.tally {
    ///         println!("Tally: program={program}, preview={preview}",
    ///                  program = tally.on_program, preview = tally.on_preview);
    ///     }
    ///     if let Some(connections) = status.connections {
    ///         println!("Active connections: {connections}");
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn monitoring_preset(source: Source) -> Self {
        Self::new(source)
            .bandwidth(ReceiverBandwidth::MetadataOnly)
            .allow_video_fields(false)
    }

    /// Set the color format for received video
    #[must_use]
    pub fn color(mut self, fmt: ReceiverColorFormat) -> Self {
        self.color_format = Some(fmt);
        self
    }

    /// Set the bandwidth mode for the receiver
    #[must_use]
    pub fn bandwidth(mut self, bw: ReceiverBandwidth) -> Self {
        self.bandwidth = Some(bw);
        self
    }

    /// Configure whether to allow video fields
    #[must_use]
    pub fn allow_video_fields(mut self, allow: bool) -> Self {
        self.allow_video_fields = Some(allow);
        self
    }

    /// Set the name for this receiver
    #[must_use]
    pub fn name<S: Into<String>>(mut self, name: S) -> Self {
        self.ndi_recv_name = Some(name.into());
        self
    }

    /// Build the receiver options
    ///
    /// This method is infallible and simply applies defaults for any unset options.
    /// To create a `Receiver`, pass the resulting `ReceiverOptions` to `Receiver::new()`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, Source};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source::default();
    /// let options = ReceiverOptions::builder(source).build();
    /// let receiver = Receiver::new(&ndi, &options)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn build(self) -> ReceiverOptions {
        ReceiverOptions {
            source_to_connect_to: self.source_to_connect_to,
            color_format: self.color_format.unwrap_or(ReceiverColorFormat::BGRX_BGRA),
            bandwidth: self.bandwidth.unwrap_or(ReceiverBandwidth::Highest),
            allow_video_fields: self.allow_video_fields.unwrap_or(true),
            ndi_recv_name: self.ndi_recv_name,
        }
    }
}

pub struct Receiver {
    pub(crate) instance: NDIlib_recv_instance_t,
    _ndi: NDI,
    source: Source,
    /// Serializes connection changes against in-flight captures.
    ///
    /// Capture calls take this lock *shared* (so video, audio, and metadata
    /// captures still run concurrently), while [`Receiver::reconnect`] takes it
    /// *exclusively*. This guarantees `NDIlib_recv_connect` never overlaps a
    /// `NDIlib_recv_capture_v3` on the same instance — the one combination the
    /// SDK does not make safe — without otherwise serializing capture.
    capture_guard: RwLock<()>,
}

impl Receiver {
    pub fn new(ndi: &NDI, create: &ReceiverOptions) -> Result<Self> {
        let create_raw = create.to_raw()?;
        let instance = unsafe { NDIlib_recv_create_v3(&create_raw.raw) };
        if instance.is_null() {
            Err(Error::InitializationFailed(
                "Failed to create NDI recv instance".into(),
            ))
        } else {
            Ok(Self {
                instance,
                _ndi: ndi.clone(),
                source: create.source_to_connect_to.clone(),
                capture_guard: RwLock::new(()),
            })
        }
    }

    /// Acquire the shared capture guard, held for the duration of a single
    /// `NDIlib_recv_capture_v3` call. Multiple captures proceed concurrently;
    /// only [`Self::reconnect`] takes the guard exclusively. The guarded data
    /// is `()`, so a poisoned lock carries no invalid state and is recovered.
    fn capture_lock(&self) -> RwLockReadGuard<'_, ()> {
        self.capture_guard
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn ptz_is_supported(&self) -> bool {
        unsafe { NDIlib_recv_ptz_is_supported(self.instance) }
    }

    pub fn ptz_recall_preset(&self, preset: u32, speed: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_recall_preset,
            preset as i32,
            speed,
            format!("Failed to recall PTZ preset {preset} with speed {speed}")
        )
    }

    pub fn ptz_zoom(&self, zoom_value: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_zoom,
            zoom_value,
            format!("Failed to set PTZ zoom to {zoom_value}")
        )
    }

    pub fn ptz_zoom_speed(&self, zoom_speed: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_zoom_speed,
            zoom_speed,
            format!("Failed to set PTZ zoom speed to {zoom_speed}")
        )
    }

    pub fn ptz_pan_tilt(&self, pan_value: f32, tilt_value: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_pan_tilt,
            pan_value,
            tilt_value,
            format!("Failed to set PTZ pan/tilt to ({pan_value}, {tilt_value})")
        )
    }

    pub fn ptz_pan_tilt_speed(&self, pan_speed: f32, tilt_speed: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_pan_tilt_speed,
            pan_speed,
            tilt_speed,
            format!("Failed to set PTZ pan/tilt speed to ({pan_speed}, {tilt_speed})")
        )
    }

    pub fn ptz_store_preset(&self, preset_no: i32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_store_preset,
            preset_no,
            format!("Failed to store PTZ preset {preset_no}")
        )
    }

    pub fn ptz_auto_focus(&self) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_auto_focus,
            "Failed to enable PTZ auto focus"
        )
    }

    pub fn ptz_focus(&self, focus_value: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_focus,
            focus_value,
            format!("Failed to set PTZ focus to {focus_value}")
        )
    }

    pub fn ptz_focus_speed(&self, focus_speed: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_focus_speed,
            focus_speed,
            format!("Failed to set PTZ focus speed to {focus_speed}")
        )
    }

    pub fn ptz_white_balance_auto(&self) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_white_balance_auto,
            "Failed to set PTZ auto white balance"
        )
    }

    pub fn ptz_white_balance_indoor(&self) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_white_balance_indoor,
            "Failed to set PTZ indoor white balance"
        )
    }

    pub fn ptz_white_balance_outdoor(&self) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_white_balance_outdoor,
            "Failed to set PTZ outdoor white balance"
        )
    }

    pub fn ptz_white_balance_oneshot(&self) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_white_balance_oneshot,
            "Failed to set PTZ oneshot white balance"
        )
    }

    pub fn ptz_white_balance_manual(&self, red: f32, blue: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_white_balance_manual,
            red,
            blue,
            format!("Failed to set PTZ manual white balance (red: {red}, blue: {blue})")
        )
    }

    pub fn ptz_exposure_auto(&self) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_exposure_auto,
            "Failed to set PTZ auto exposure"
        )
    }

    pub fn ptz_exposure_manual(&self, exposure_level: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_exposure_manual,
            exposure_level,
            format!("Failed to set PTZ manual exposure to {exposure_level}")
        )
    }

    pub fn ptz_exposure_manual_v2(&self, iris: f32, gain: f32, shutter_speed: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_exposure_manual_v2,
            iris,
            gain,
            shutter_speed,
            format!("Failed to set PTZ manual exposure v2 (iris: {iris}, gain: {gain}, shutter: {shutter_speed})")
        )
    }

    /// Capture **video** frames from this receiver.
    ///
    /// Returns a [`Capture`] view whose verbs cover the three capture styles:
    ///
    /// - [`capture`](Capture::capture) — reliable owned capture that retries
    ///   across the NDI SDK's initial-sync warm-up.
    /// - [`try_capture`](Capture::try_capture) — a single owned poll, `Ok(None)`
    ///   when no frame is ready.
    /// - [`try_capture_ref`](Capture::try_capture_ref) — a single zero-copy poll
    ///   borrowing the SDK buffer in place.
    ///
    /// Video frames may carry per-row padding (e.g. RGBX/BGRX); see
    /// [`VideoFrame`](crate::VideoFrame) for how to read them correctly.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, Receiver, ReceiverOptions, Source, SourceAddress};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let options = ReceiverOptions::builder(source).build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// // Reliable owned capture
    /// let frame = receiver.video().capture(Duration::from_secs(5))?;
    /// println!("{}x{}", frame.width(), frame.height());
    ///
    /// // Zero-copy borrow
    /// if let Some(frame) = receiver.video().try_capture_ref(Duration::from_secs(1))? {
    ///     let pixels = frame.data();
    ///     println!("{} bytes in place", pixels.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "the Capture view does nothing until a capture verb is called"]
    pub fn video(&self) -> Capture<'_, VideoKind> {
        Capture::new(self)
    }

    /// Capture **audio** frames from this receiver.
    ///
    /// Returns a [`Capture`] view; see [`video`](Self::video) for the three
    /// capture verbs it exposes ([`capture`](Capture::capture),
    /// [`try_capture`](Capture::try_capture),
    /// [`try_capture_ref`](Capture::try_capture_ref)).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, Receiver, ReceiverOptions, Source, SourceAddress};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let options = ReceiverOptions::builder(source).build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// if let Some(audio) = receiver.audio().try_capture_ref(Duration::from_secs(1))? {
    ///     println!("{} channels, {} samples", audio.num_channels(), audio.num_samples());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "the Capture view does nothing until a capture verb is called"]
    pub fn audio(&self) -> Capture<'_, AudioKind> {
        Capture::new(self)
    }

    /// Capture **metadata** frames from this receiver.
    ///
    /// Returns a [`Capture`] view; see [`video`](Self::video) for the three
    /// capture verbs it exposes ([`capture`](Capture::capture),
    /// [`try_capture`](Capture::try_capture),
    /// [`try_capture_ref`](Capture::try_capture_ref)).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, Receiver, ReceiverOptions, Source, SourceAddress};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let options = ReceiverOptions::builder(source).build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// if let Some(meta) = receiver.metadata().try_capture(Duration::from_millis(100))? {
    ///     println!("metadata: {}", meta.data());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "the Capture view does nothing until a capture verb is called"]
    pub fn metadata(&self) -> Capture<'_, MetadataKind> {
        Capture::new(self)
    }

    /// Single zero-copy poll backing every [`Capture::try_capture_ref`].
    ///
    /// Holds the shared capture guard for the duration of one
    /// `NDIlib_recv_capture_v3` call, so it runs concurrently with other
    /// captures but never overlaps a [`reconnect`](Self::reconnect).
    fn capture_ref_kind<K: CaptureKind>(&self, timeout: Duration) -> Result<Option<K::Ref<'_>>> {
        let timeout_ms = to_ms_checked(timeout)?;

        // Shared guard: excludes a concurrent `reconnect`, not other captures.
        let _capture = self.capture_lock();
        // SAFETY: self.instance is a valid NDI receiver instance.
        match unsafe { capture_raw::<K>(self.instance, timeout_ms) } {
            CaptureResult::Frame(guard) => {
                // Validation (e.g. FourCC) happens during ref construction.
                let frame_ref = unsafe { K::make_ref(guard)? };
                Ok(Some(frame_ref))
            }
            CaptureResult::None => Ok(None),
            CaptureResult::Error => Err(Error::CaptureFailed("Received an error frame".into())),
        }
    }

    /// Single owned poll backing every [`Capture::try_capture`].
    pub(crate) fn try_capture_kind<K: CaptureKind>(
        &self,
        timeout: Duration,
    ) -> Result<Option<K::Owned>> {
        match self.capture_ref_kind::<K>(timeout)? {
            Some(frame_ref) => Ok(Some(K::ref_to_owned(&frame_ref)?)),
            None => Ok(None),
        }
    }

    /// Retried owned capture backing every [`Capture::capture`].
    ///
    /// Polls [`try_capture_kind`](Self::try_capture_kind) under the default
    /// [`RetryPolicy`], absorbing the SDK's initial-sync misses where it returns
    /// `none` immediately instead of blocking for the full timeout.
    pub(crate) fn capture_kind<K: CaptureKind>(&self, timeout: Duration) -> Result<K::Owned> {
        retry_capture(timeout, &RetryPolicy::default(), |poll| {
            self.try_capture_kind::<K>(poll)
        })
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
    /// # use grafton_ndi::{NDI, Receiver, ReceiverOptions, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let options = ReceiverOptions::builder(source).build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// if receiver.is_connected() {
    ///     println!("Still connected to source");
    /// } else {
    ///     println!("Lost connection to source");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn is_connected(&self) -> bool {
        unsafe { NDIlib_recv_get_no_connections(self.instance) > 0 }
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
    /// [`Self::connection_stats`] (`video_frames_received` resuming its
    /// climb indicates frames are flowing again).
    ///
    /// # Thread safety
    ///
    /// `NDIlib_recv_connect` is the one receive call the SDK does *not* make
    /// safe to run concurrently with `NDIlib_recv_capture_v3` on the same
    /// instance. This method therefore takes the receiver's capture guard
    /// exclusively: it **blocks until every in-flight capture on this receiver
    /// returns**, and holds off any capture that starts while it runs. Captures
    /// on the same receiver still run concurrently with each other; only a
    /// reconnect is exclusive. Because it can block for as long as a capture's
    /// timeout, prefer short capture timeouts on receivers you intend to
    /// recover, and call it off the async runtime — `AsyncReceiver::reconnect`
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
        // Marshal before locking; this touches only `self.source`, not the
        // instance, so it need not hold off captures.
        let raw = self.source.to_raw()?;
        // Exclusive guard: waits for in-flight captures to drain and blocks new
        // ones, so `NDIlib_recv_connect` never overlaps `NDIlib_recv_capture_v3`.
        let _exclusive = self
            .capture_guard
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        // SAFETY: `self.instance` is a valid receiver instance for the
        // lifetime of `self`, and `raw` (which owns the backing
        // CStrings) outlives the call, so the pointers inside `raw.raw`
        // stay valid while the SDK copies the source descriptor.
        unsafe { NDIlib_recv_connect(self.instance, &raw.raw) };
        Ok(())
    }

    /// Get the source this receiver is connected to.
    ///
    /// Returns a reference to the [`Source`] that was specified when creating
    /// this receiver. This is useful for identifying which source a receiver
    /// is associated with when managing multiple receivers.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, Receiver, ReceiverOptions, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let options = ReceiverOptions::builder(source).build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let source = receiver.source();
    /// println!("Connected to: {name}", name = source.name);
    /// # Ok(())
    /// # }
    /// ```
    pub fn source(&self) -> &Source {
        &self.source
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
    /// # use grafton_ndi::{NDI, Receiver, ReceiverOptions, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let options = ReceiverOptions::builder(source).build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let stats = receiver.connection_stats();
    /// println!("Connections: {connections}", connections = stats.connections);
    /// println!("Video frames: {received} (dropped: {dropped})",
    ///          received = stats.video_frames_received,
    ///          dropped = stats.video_frames_dropped);
    /// println!("Frame drop rate: {rate:.2}%",
    ///          rate = stats.video_drop_percentage());
    /// # Ok(())
    /// # }
    /// ```
    pub fn connection_stats(&self) -> ConnectionStats {
        let connections = unsafe { NDIlib_recv_get_no_connections(self.instance) };

        let mut total = NDIlib_recv_performance_t::default();
        let mut dropped = NDIlib_recv_performance_t::default();
        unsafe {
            NDIlib_recv_get_performance(self.instance, &mut total, &mut dropped);
        }

        let mut queue = NDIlib_recv_queue_t::default();
        unsafe {
            NDIlib_recv_get_queue(self.instance, &mut queue);
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

    /// Poll for status changes (tally, connections, etc.)
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for status change.
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// * `Some(ReceiverStatus)` - Status has changed
    /// * `None` - Timeout occurred with no status change
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] if `timeout` exceeds [`crate::MAX_TIMEOUT`].
    pub fn poll_status_change(&self, timeout: Duration) -> Result<Option<ReceiverStatus>> {
        let timeout_ms = to_ms_checked(timeout)?;
        // Shared guard: excludes a concurrent `reconnect`, not other captures.
        let _capture = self.capture_lock();
        // SAFETY: NDI SDK documentation states that recv_capture_v3 is thread-safe
        let frame_type = unsafe {
            NDIlib_recv_capture_v3(
                self.instance,
                ptr::null_mut(), // no video
                ptr::null_mut(), // no audio
                ptr::null_mut(), // no metadata
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_status_change => {
                // Note: NDI SDK doesn't provide recv_get_tally, so we can't query current tally state
                // We would need to track it from set_tally calls
                let tally = None;

                // Get number of connections
                let connections = {
                    let conn_count = unsafe { NDIlib_recv_get_no_connections(self.instance) };
                    if conn_count >= 0 {
                        Some(conn_count)
                    } else {
                        None
                    }
                };

                let has_tally = tally.is_some();
                let has_connections = connections.is_some();

                Ok(Some(ReceiverStatus {
                    tally,
                    connections,
                    other: !has_tally && !has_connections,
                }))
            }
            _ => Ok(None),
        }
    }
}

/// A typed view over a [`Receiver`] for capturing frames of one kind.
///
/// Created by [`Receiver::video`], [`Receiver::audio`], and
/// [`Receiver::metadata`]. The view is a cheap borrow of the receiver and does
/// nothing on its own — call one of its verbs to capture:
///
/// - [`capture`](Self::capture) — reliable owned capture with built-in retry.
///   **The primary method.** It absorbs the NDI SDK's initial-sync warm-up (the
///   first few polls after connecting return `none` immediately instead of
///   blocking), then runs with zero overhead in steady state, so it is safe in
///   a continuous capture loop.
/// - [`try_capture`](Self::try_capture) — a single owned poll; `Ok(None)` when
///   no frame is ready. For manual polling where you handle timing yourself.
/// - [`try_capture_ref`](Self::try_capture_ref) — a single zero-copy poll that
///   borrows the SDK's buffer in place (no allocation, no memcpy). The
///   recommended API for performance-critical, in-place processing: for a
///   1920×1080 BGRA frame it avoids ~8.3 MB of copying per frame (~475 MB/s at
///   60 fps). Convert to an owned frame with `to_owned` when you need to keep or
///   send it.
///
/// Every verb holds a shared capture guard for the duration of a single SDK
/// call, so captures of different kinds run concurrently with each other but
/// never overlap a [`Receiver::reconnect`].
pub struct Capture<'rx, K: CaptureKind> {
    rx: &'rx Receiver,
    _kind: PhantomData<K>,
}

impl<'rx, K: CaptureKind> Capture<'rx, K> {
    fn new(rx: &'rx Receiver) -> Self {
        Self {
            rx,
            _kind: PhantomData,
        }
    }

    /// Reliable owned capture: blocks up to `timeout`, retrying across the NDI
    /// SDK's initial-sync warm-up.
    ///
    /// `timeout` is a total retry budget; each individual SDK poll is capped to
    /// the remaining budget, and [`Duration::ZERO`] performs exactly one
    /// non-blocking attempt.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Total time to wait for a frame. Must not exceed
    ///   [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Errors
    ///
    /// Returns [`Error::FrameTimeout`] if no frame arrives within `timeout`;
    /// other errors propagate from the capture itself.
    pub fn capture(&self, timeout: Duration) -> Result<K::Owned> {
        self.rx.capture_kind::<K>(timeout)
    }

    /// Single owned poll: returns `Ok(None)` if no frame is available within
    /// `timeout`.
    ///
    /// Prefer [`capture`](Self::capture) for reliable capture; this variant does
    /// not retry the SDK's warm-up misses.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for a frame. Must not exceed
    ///   [`crate::MAX_TIMEOUT`] (~49.7 days).
    pub fn try_capture(&self, timeout: Duration) -> Result<Option<K::Owned>> {
        self.rx.try_capture_kind::<K>(timeout)
    }

    /// Single zero-copy poll: returns a borrowed view of the SDK's buffer, or
    /// `Ok(None)` if no frame is available within `timeout`.
    ///
    /// The returned reference borrows the [`Receiver`] (not this temporary
    /// view), so it may outlive the `Capture` while keeping the receiver
    /// borrowed.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for a frame. Must not exceed
    ///   [`crate::MAX_TIMEOUT`] (~49.7 days).
    pub fn try_capture_ref(&self, timeout: Duration) -> Result<Option<K::Ref<'rx>>> {
        // Bind through `'rx` so the returned borrow is tied to the receiver, not
        // to this short-lived view.
        let rx: &'rx Receiver = self.rx;
        rx.capture_ref_kind::<K>(timeout)
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        unsafe {
            NDIlib_recv_destroy(self.instance);
        }
    }
}

/// # Safety
///
/// The NDI 6 SDK documentation explicitly states that recv operations are thread-safe.
/// `NDIlib_recv_capture_v3` and related functions use internal synchronization.
/// The Receiver struct only holds an opaque pointer returned by the SDK, and the SDK
/// guarantees that this pointer can be safely moved between threads.
unsafe impl Send for Receiver {}

/// # Safety
///
/// The NDI 6 SDK documentation guarantees that `NDIlib_recv_capture_v3` is internally
/// synchronized and can be called concurrently from multiple threads. This is explicitly
/// mentioned in the SDK manual's thread safety section. The capture verbs (via
/// [`Receiver::video`], [`Receiver::audio`], and [`Receiver::metadata`]) can be
/// safely called from multiple threads simultaneously.
unsafe impl Sync for Receiver {}

#[derive(Debug, Clone)]
pub struct ReceiverStatus {
    /// Current Tally (program/preview) if known
    pub tally: Option<Tally>,
    /// Number of active connections (None if unknown)
    pub connections: Option<i32>,
    /// True when the receiver reports any other change (latency, PTZ, etc.)
    pub other: bool,
}

#[derive(Debug, Clone)]
pub struct Tally {
    pub on_program: bool,
    pub on_preview: bool,
}

impl Tally {
    pub fn new(on_program: bool, on_preview: bool) -> Self {
        Tally {
            on_program,
            on_preview,
        }
    }

    pub(crate) fn to_raw(&self) -> NDIlib_tally_t {
        NDIlib_tally_t {
            on_program: self.on_program,
            on_preview: self.on_preview,
        }
    }
}

/// Connection and performance statistics for a receiver.
///
/// Provides detailed metrics about receiver health including connection count,
/// frame counts, and drop rates. Useful for monitoring and diagnostics.
#[derive(Debug, Clone)]
pub struct ConnectionStats {
    /// Number of active connections to this receiver
    pub connections: u32,

    /// Total number of video frames received
    pub video_frames_received: u64,

    /// Total number of audio frames received
    pub audio_frames_received: u64,

    /// Total number of metadata frames received
    pub metadata_frames_received: u64,

    /// Number of video frames dropped due to buffer overflow or processing delays
    pub video_frames_dropped: u64,

    /// Number of audio frames dropped
    pub audio_frames_dropped: u64,

    /// Number of metadata frames dropped
    pub metadata_frames_dropped: u64,

    /// Number of video frames currently queued for processing
    pub video_frames_queued: u32,

    /// Number of audio frames currently queued
    pub audio_frames_queued: u32,

    /// Number of metadata frames currently queued
    pub metadata_frames_queued: u32,
}

impl ConnectionStats {
    /// Calculate video frame drop percentage.
    ///
    /// Returns the percentage of video frames that were dropped out of the total
    /// received + dropped. Returns 0.0 if no frames have been received.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grafton_ndi::ConnectionStats;
    /// let stats = ConnectionStats {
    ///     connections: 1,
    ///     video_frames_received: 900,
    ///     video_frames_dropped: 100,
    ///     audio_frames_received: 0,
    ///     audio_frames_dropped: 0,
    ///     metadata_frames_received: 0,
    ///     metadata_frames_dropped: 0,
    ///     video_frames_queued: 5,
    ///     audio_frames_queued: 0,
    ///     metadata_frames_queued: 0,
    /// };
    /// assert_eq!(stats.video_drop_percentage(), 10.0);
    /// ```
    pub fn video_drop_percentage(&self) -> f64 {
        let total = self.video_frames_received + self.video_frames_dropped;
        if total == 0 {
            0.0
        } else {
            (self.video_frames_dropped as f64 / total as f64) * 100.0
        }
    }

    /// Calculate audio frame drop percentage.
    ///
    /// Returns the percentage of audio frames that were dropped out of the total
    /// received + dropped. Returns 0.0 if no frames have been received.
    pub fn audio_drop_percentage(&self) -> f64 {
        let total = self.audio_frames_received + self.audio_frames_dropped;
        if total == 0 {
            0.0
        } else {
            (self.audio_frames_dropped as f64 / total as f64) * 100.0
        }
    }

    /// Calculate metadata frame drop percentage.
    ///
    /// Returns the percentage of metadata frames that were dropped out of the total
    /// received + dropped. Returns 0.0 if no frames have been received.
    pub fn metadata_drop_percentage(&self) -> f64 {
        let total = self.metadata_frames_received + self.metadata_frames_dropped;
        if total == 0 {
            0.0
        } else {
            (self.metadata_frames_dropped as f64 / total as f64) * 100.0
        }
    }

    /// Check if the receiver is currently connected.
    ///
    /// Returns `true` if there is at least one active connection.
    pub fn is_connected(&self) -> bool {
        self.connections > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::Cell;

    fn test_source() -> Source {
        Source::default()
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
    fn receiver_color_format_maps_to_exact_sdk_values() {
        assert_eq!(
            NDIlib_recv_color_format_e::from(ReceiverColorFormat::BGRX_BGRA),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA
        );
        assert_eq!(
            NDIlib_recv_color_format_e::from(ReceiverColorFormat::UYVY_BGRA),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_BGRA
        );
        assert_eq!(
            NDIlib_recv_color_format_e::from(ReceiverColorFormat::RGBX_RGBA),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA
        );
        assert_eq!(
            NDIlib_recv_color_format_e::from(ReceiverColorFormat::UYVY_RGBA),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_RGBA
        );
        assert_eq!(
            NDIlib_recv_color_format_e::from(ReceiverColorFormat::Fastest),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_fastest
        );
        assert_eq!(
            NDIlib_recv_color_format_e::from(ReceiverColorFormat::Best),
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_best
        );
    }

    #[test]
    fn receiver_bandwidth_maps_to_exact_sdk_values() {
        assert_eq!(
            NDIlib_recv_bandwidth_e::from(ReceiverBandwidth::MetadataOnly),
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_metadata_only
        );
        assert_eq!(
            NDIlib_recv_bandwidth_e::from(ReceiverBandwidth::AudioOnly),
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_audio_only
        );
        assert_eq!(
            NDIlib_recv_bandwidth_e::from(ReceiverBandwidth::Lowest),
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest
        );
        assert_eq!(
            NDIlib_recv_bandwidth_e::from(ReceiverBandwidth::Highest),
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest
        );
    }

    #[test]
    fn receiver_options_defaults_emit_operational_raw_modes() {
        let raw = ReceiverOptions::builder(test_source())
            .build()
            .to_raw()
            .unwrap();

        assert_raw_receiver_modes(
            &raw,
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA,
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
        );
        assert!(raw.raw.allow_video_fields);
    }

    #[test]
    fn snapshot_preset_emits_operational_raw_modes() {
        let raw = ReceiverOptionsBuilder::snapshot_preset(test_source())
            .build()
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
        let raw = ReceiverOptionsBuilder::high_quality_preset(test_source())
            .build()
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
        let raw = ReceiverOptionsBuilder::monitoring_preset(test_source())
            .build()
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

        assert_eq!(result.unwrap(), 42);
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

        let result: Result<i32> = retry_capture_with_clock(
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

        assert!(matches!(
            result,
            Err(Error::FrameTimeout { attempts: 2, .. })
        ));
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

        assert_eq!(result.unwrap(), 42);
        assert_eq!(polls, [Duration::ZERO]);
    }

    #[test]
    fn retry_zero_timeout_miss_times_out_after_one_attempt() {
        let clock = FakeClock::default();
        let mut polls = Vec::new();

        let result: Result<i32> = retry_capture_with_clock(
            Duration::ZERO,
            &RetryPolicy::default(),
            |poll| {
                polls.push(poll);
                Ok(None)
            },
            || clock.elapsed(),
            |_| panic!("zero-timeout miss should not sleep"),
        );

        match result {
            Err(Error::FrameTimeout { attempts, elapsed }) => {
                assert_eq!(attempts, 1);
                assert_eq!(elapsed, Duration::ZERO);
            }
            _ => panic!("Expected FrameTimeout error"),
        }
        assert_eq!(polls, [Duration::ZERO]);
    }

    #[test]
    fn retry_timeout_counts_only_actual_capture_attempts() {
        let clock = FakeClock::default();
        let policy = RetryPolicy {
            poll_interval: Duration::from_millis(8),
            sleep_between: Duration::from_millis(5),
        };
        let mut polls = Vec::new();

        let result: Result<i32> = retry_capture_with_clock(
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

        match result {
            Err(Error::FrameTimeout { attempts, elapsed }) => {
                assert_eq!(attempts, 1);
                assert_eq!(elapsed, Duration::from_millis(10));
            }
            _ => panic!("Expected FrameTimeout error"),
        }
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

        let result: Result<i32> = retry_capture_with_clock(
            Duration::from_millis(10),
            &policy,
            |poll| {
                assert_eq!(poll, Duration::from_millis(8));
                clock.advance(Duration::from_millis(8));
                Ok(None)
            },
            || clock.elapsed(),
            |sleep| {
                sleeps.push(sleep);
                clock.advance(sleep);
            },
        );

        assert!(matches!(
            result,
            Err(Error::FrameTimeout { attempts: 1, .. })
        ));
        assert_eq!(sleeps, [Duration::from_millis(2)]);
    }

    #[test]
    fn retry_validates_total_timeout_before_attempts() {
        let attempts = Cell::new(0);

        let result: Result<i32> = retry_capture(
            crate::MAX_TIMEOUT + Duration::from_nanos(1),
            &RetryPolicy::default(),
            |_| {
                attempts.set(attempts.get() + 1);
                Ok(Some(42))
            },
        );

        assert!(matches!(result, Err(Error::InvalidConfiguration(_))));
        assert_eq!(attempts.get(), 0);
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

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn retry_propagates_error() {
        let clock = FakeClock::default();
        let attempts = Cell::new(0);
        let mut sleeps = Vec::new();

        let result: Result<i32> = retry_capture_with_clock(
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
