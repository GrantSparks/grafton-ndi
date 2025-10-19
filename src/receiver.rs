//! NDI receiving functionality for video, audio, and metadata.
//!
//! # Monitoring Tally & Connection Count
//!
//! The receiver can monitor status changes including tally state and connection count:
//!
//! ```ignore
//! # use grafton_ndi::{NDI, ReceiverOptions, ReceiverBandwidth, Source};
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! # let ndi = NDI::new()?;
//! // In real usage, you'd get the source from Finder::get_sources()
//! // let source = /* obtained from Finder */;
//! let receiver = ReceiverOptions::builder(source)
//!     .bandwidth(ReceiverBandwidth::MetadataOnly)
//!     .build(&ndi)?;
//!
//! // Poll for status changes
//! if let Some(status) = receiver.poll_status_change(1000) {
//!     if let Some(tally) = status.tally {
//!         println!("Tally: program={}, preview={}",
//!                  tally.on_program, tally.on_preview);
//!     }
//!     if let Some(connections) = status.connections {
//!         println!("Active connections: {}", connections);
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::{ffi::CString, ptr};

use crate::{
    finder::{RawSource, Source},
    frames::{AudioFrame, MetadataFrame, VideoFrame},
    ndi_lib::*,
    Error, Result, NDI,
};

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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverColorFormat {
    #[default]
    BGRX_BGRA,
    UYVY_BGRA,
    RGBX_RGBA,
    UYVY_RGBA,
    Fastest,
    Best,
    //    BGRX_BGRA_Flipped,
    Max,
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
            //            ReceiverColorFormat::BGRX_BGRA_Flipped => {
            //                NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA_flipped
            //            }
            ReceiverColorFormat::Max => NDIlib_recv_color_format_e_NDIlib_recv_color_format_max,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverBandwidth {
    MetadataOnly,
    AudioOnly,
    Lowest,
    #[default]
    Highest,
    Max,
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
            ReceiverBandwidth::Max => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_max,
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
    /// # use grafton_ndi::{NDI, Finder, FinderOptions, ReceiverOptionsBuilder};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(1000);
    /// # let sources = finder.get_sources(0)?;
    /// let receiver = ReceiverOptionsBuilder::snapshot_preset(sources[0].clone())
    ///     .name("Snapshot Receiver")
    ///     .build(&ndi)?;
    ///
    /// // Capture and encode in one line (requires image-encoding feature)
    /// #[cfg(feature = "image-encoding")]
    /// {
    ///     let frame = receiver.capture_video_blocking(5000)?;
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
    /// # use grafton_ndi::{NDI, Finder, FinderOptions, ReceiverOptionsBuilder};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(1000);
    /// # let sources = finder.get_sources(0)?;
    /// let receiver = ReceiverOptionsBuilder::high_quality_preset(sources[0].clone())
    ///     .name("High Quality Receiver")
    ///     .build(&ndi)?;
    ///
    /// // Capture full quality frames
    /// let frame = receiver.capture_video_blocking(5000)?;
    /// println!("Captured {}x{} frame", frame.width, frame.height);
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
    /// # use grafton_ndi::{NDI, Finder, FinderOptions, ReceiverOptionsBuilder};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// # finder.wait_for_sources(1000);
    /// # let sources = finder.get_sources(0)?;
    /// let receiver = ReceiverOptionsBuilder::monitoring_preset(sources[0].clone())
    ///     .name("Tally Monitor")
    ///     .build(&ndi)?;
    ///
    /// // Poll for status changes
    /// if let Some(status) = receiver.poll_status_change(1000) {
    ///     if let Some(tally) = status.tally {
    ///         println!("Tally: program={}, preview={}",
    ///                  tally.on_program, tally.on_preview);
    ///     }
    ///     if let Some(connections) = status.connections {
    ///         println!("Active connections: {}", connections);
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

    /// Build the receiver and create a Receiver instance
    pub fn build(self, ndi: &NDI) -> Result<Receiver> {
        let receiver = ReceiverOptions {
            source_to_connect_to: self.source_to_connect_to,
            color_format: self.color_format.unwrap_or(ReceiverColorFormat::BGRX_BGRA),
            bandwidth: self.bandwidth.unwrap_or(ReceiverBandwidth::Highest),
            allow_video_fields: self.allow_video_fields.unwrap_or(true),
            ndi_recv_name: self.ndi_recv_name,
        };
        Receiver::new(ndi, &receiver)
    }
}

pub struct Receiver {
    pub(crate) instance: NDIlib_recv_instance_t,
    _ndi: NDI,
    source: Source,
}

impl Receiver {
    pub fn new(ndi: &NDI, create: &ReceiverOptions) -> Result<Self> {
        let create_raw = create.to_raw()?;
        // NDIlib_recv_create_v3 already connects to the source specified in source_to_connect_to
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
            })
        }
    }

    /// Capture a frame with owned data (copies the frame data)
    #[deprecated(
        note = "Use capture_video, capture_audio, or capture_metadata for concurrent access"
    )]
    pub fn capture(&mut self, timeout_ms: u32) -> Result<FrameType> {
        let mut video_frame = NDIlib_video_frame_v2_t::default();
        let mut audio_frame = NDIlib_audio_frame_v3_t::default();
        let mut metadata_frame = NDIlib_metadata_frame_t::default();

        let frame_type = unsafe {
            NDIlib_recv_capture_v3(
                self.instance,
                &mut video_frame,
                &mut audio_frame,
                &mut metadata_frame,
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_video => {
                let frame = unsafe { VideoFrame::from_raw(&video_frame) }?;
                // Note: Drop impl will call NDIlib_recv_free_video_v2 when frame is dropped
                Ok(FrameType::Video(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_audio => {
                let frame = AudioFrame::from_raw(audio_frame)?;
                // Note: Drop impl will call NDIlib_recv_free_audio_v3 when frame is dropped
                Ok(FrameType::Audio(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                let frame = MetadataFrame::from_raw(&metadata_frame);
                unsafe { NDIlib_recv_free_metadata(self.instance, &metadata_frame) };
                Ok(FrameType::Metadata(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(FrameType::None),
            NDIlib_frame_type_e_NDIlib_frame_type_status_change => {
                // For the deprecated capture() method, we'll return a simple status with minimal info
                let status = ReceiverStatus {
                    tally: None,
                    connections: None,
                    other: true,
                };
                Ok(FrameType::StatusChange(status))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::CaptureFailed("Received an error frame".into()))
            }
            _ => Err(Error::CaptureFailed(format!(
                "Unknown frame type: {}",
                frame_type
            ))),
        }
    }

    #[allow(dead_code)]
    pub fn free_string(&self, string: &str) {
        let c_string = CString::new(string).expect("Failed to create CString");
        unsafe {
            NDIlib_recv_free_string(self.instance, c_string.into_raw());
        }
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
            format!(
                "Failed to recall PTZ preset {} with speed {}",
                preset, speed
            )
        )
    }

    pub fn ptz_zoom(&self, zoom_value: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_zoom,
            zoom_value,
            format!("Failed to set PTZ zoom to {}", zoom_value)
        )
    }

    pub fn ptz_zoom_speed(&self, zoom_speed: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_zoom_speed,
            zoom_speed,
            format!("Failed to set PTZ zoom speed to {}", zoom_speed)
        )
    }

    pub fn ptz_pan_tilt(&self, pan_value: f32, tilt_value: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_pan_tilt,
            pan_value,
            tilt_value,
            format!(
                "Failed to set PTZ pan/tilt to ({}, {})",
                pan_value, tilt_value
            )
        )
    }

    pub fn ptz_pan_tilt_speed(&self, pan_speed: f32, tilt_speed: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_pan_tilt_speed,
            pan_speed,
            tilt_speed,
            format!(
                "Failed to set PTZ pan/tilt speed to ({}, {})",
                pan_speed, tilt_speed
            )
        )
    }

    pub fn ptz_store_preset(&self, preset_no: i32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_store_preset,
            preset_no,
            format!("Failed to store PTZ preset {}", preset_no)
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
            format!("Failed to set PTZ focus to {}", focus_value)
        )
    }

    pub fn ptz_focus_speed(&self, focus_speed: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_focus_speed,
            focus_speed,
            format!("Failed to set PTZ focus speed to {}", focus_speed)
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
            format!(
                "Failed to set PTZ manual white balance (red: {}, blue: {})",
                red, blue
            )
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
            format!("Failed to set PTZ manual exposure to {}", exposure_level)
        )
    }

    pub fn ptz_exposure_manual_v2(&self, iris: f32, gain: f32, shutter_speed: f32) -> Result<()> {
        ptz_command!(
            self,
            NDIlib_recv_ptz_exposure_manual_v2,
            iris,
            gain,
            shutter_speed,
            format!(
                "Failed to set PTZ manual exposure v2 (iris: {}, gain: {}, shutter: {})",
                iris, gain, shutter_speed
            )
        )
    }

    /// Capture only video frames - safe to call from multiple threads concurrently
    pub fn capture_video(&self, timeout_ms: u32) -> Result<Option<VideoFrame>> {
        let mut video_frame = NDIlib_video_frame_v2_t::default();

        // SAFETY: NDI SDK documentation states that recv_capture_v3 is thread-safe
        let frame_type = unsafe {
            NDIlib_recv_capture_v3(
                self.instance,
                &mut video_frame,
                ptr::null_mut(), // no audio
                ptr::null_mut(), // no metadata
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_video => {
                let frame = unsafe { VideoFrame::from_raw(&video_frame) }?;
                Ok(Some(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(None),
            NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::CaptureFailed("Received an error frame".into()))
            }
            _ => Ok(None), // Other frame types are ignored when capturing video only
        }
    }

    /// Capture video with automatic retry logic.
    ///
    /// The NDI SDK's `capture_video` may return `None` immediately even when frames
    /// are available, rather than blocking for the full timeout duration. This method
    /// handles that SDK behavior by retrying up to `max_attempts` times.
    ///
    /// # Arguments
    ///
    /// * `timeout_ms` - Timeout for each individual capture attempt in milliseconds
    /// * `max_attempts` - Maximum number of retry attempts before giving up
    ///
    /// # Returns
    ///
    /// * `Ok(Some(frame))` - Successfully captured a video frame
    /// * `Ok(None)` - No frame available after all retry attempts
    /// * `Err(_)` - An error occurred during capture
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, ReceiverOptions, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let receiver = ReceiverOptions::builder(source).build(&ndi)?;
    /// // Try up to 10 times with 100ms timeout per attempt
    /// if let Some(frame) = receiver.capture_video_with_retry(100, 10)? {
    ///     println!("Captured {}x{} frame", frame.width, frame.height);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn capture_video_with_retry(
        &self,
        timeout_ms: u32,
        max_attempts: usize,
    ) -> Result<Option<VideoFrame>> {
        for attempt in 1..=max_attempts {
            match self.capture_video(timeout_ms)? {
                Some(frame) => return Ok(Some(frame)),
                None => {
                    // Don't sleep on the last attempt
                    if attempt < max_attempts {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Capture video, blocking until a frame is received or timeout expires.
    ///
    /// This is the recommended method for reliable video frame capture. It handles
    /// the NDI SDK's quirk where `capture_video` may return immediately rather than
    /// blocking for the full timeout duration.
    ///
    /// # Arguments
    ///
    /// * `total_timeout_ms` - Total time to wait for a frame in milliseconds
    ///
    /// # Returns
    ///
    /// * `Ok(frame)` - Successfully captured a video frame
    /// * `Err(Error::FrameTimeout)` - No frame received within the timeout period (includes retry details)
    /// * `Err(_)` - Another error occurred during capture
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, ReceiverOptions, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let receiver = ReceiverOptions::builder(source).build(&ndi)?;
    /// // Wait up to 5 seconds for a frame
    /// let frame = receiver.capture_video_blocking(5000)?;
    /// println!("Captured {}x{} frame", frame.width, frame.height);
    /// # Ok(())
    /// # }
    /// ```
    pub fn capture_video_blocking(&self, total_timeout_ms: u32) -> Result<VideoFrame> {
        let start_time = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(total_timeout_ms.into());
        let mut attempts = 0;

        loop {
            attempts += 1;

            // Check if we've exceeded our total timeout
            let elapsed = start_time.elapsed();
            if elapsed > timeout {
                return Err(Error::FrameTimeout { attempts, elapsed });
            }

            // Try to capture with a short per-attempt timeout (100ms)
            // The NDI SDK may return immediately even with a longer timeout
            match self.capture_video(100)? {
                Some(frame) => return Ok(frame),
                None => {
                    // Brief sleep before retry to avoid busy-waiting
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }

    /// Capture only audio frames - safe to call from multiple threads concurrently
    pub fn capture_audio(&self, timeout_ms: u32) -> Result<Option<AudioFrame>> {
        let mut audio_frame = NDIlib_audio_frame_v3_t::default();

        // SAFETY: NDI SDK documentation states that recv_capture_v3 is thread-safe
        let frame_type = unsafe {
            NDIlib_recv_capture_v3(
                self.instance,
                ptr::null_mut(), // no video
                &mut audio_frame,
                ptr::null_mut(), // no metadata
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_audio => {
                let frame = AudioFrame::from_raw(audio_frame)?;
                Ok(Some(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(None),
            NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::CaptureFailed("Received an error frame".into()))
            }
            _ => Ok(None), // Other frame types are ignored when capturing audio only
        }
    }

    /// Capture audio with automatic retry logic.
    ///
    /// Similar to `capture_video_with_retry`, this handles the NDI SDK's behavior
    /// where capture may return `None` immediately rather than blocking.
    ///
    /// # Arguments
    ///
    /// * `timeout_ms` - Timeout for each individual capture attempt in milliseconds
    /// * `max_attempts` - Maximum number of retry attempts before giving up
    ///
    /// # Returns
    ///
    /// * `Ok(Some(frame))` - Successfully captured an audio frame
    /// * `Ok(None)` - No frame available after all retry attempts
    /// * `Err(_)` - An error occurred during capture
    pub fn capture_audio_with_retry(
        &self,
        timeout_ms: u32,
        max_attempts: usize,
    ) -> Result<Option<AudioFrame>> {
        for attempt in 1..=max_attempts {
            match self.capture_audio(timeout_ms)? {
                Some(frame) => return Ok(Some(frame)),
                None => {
                    // Don't sleep on the last attempt
                    if attempt < max_attempts {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Capture audio, blocking until a frame is received or timeout expires.
    ///
    /// This is the recommended method for reliable audio frame capture.
    ///
    /// # Arguments
    ///
    /// * `total_timeout_ms` - Total time to wait for a frame in milliseconds
    ///
    /// # Returns
    ///
    /// * `Ok(frame)` - Successfully captured an audio frame
    /// * `Err(Error::FrameTimeout)` - No frame received within the timeout period (includes retry details)
    /// * `Err(_)` - Another error occurred during capture
    pub fn capture_audio_blocking(&self, total_timeout_ms: u32) -> Result<AudioFrame> {
        let start_time = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(total_timeout_ms.into());
        let mut attempts = 0;

        loop {
            attempts += 1;

            // Check if we've exceeded our total timeout
            let elapsed = start_time.elapsed();
            if elapsed > timeout {
                return Err(Error::FrameTimeout { attempts, elapsed });
            }

            // Try to capture with a short per-attempt timeout (100ms)
            match self.capture_audio(100)? {
                Some(frame) => return Ok(frame),
                None => {
                    // Brief sleep before retry to avoid busy-waiting
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }

    /// Capture only metadata frames - safe to call from multiple threads concurrently
    pub fn capture_metadata(&self, timeout_ms: u32) -> Result<Option<MetadataFrame>> {
        let mut metadata_frame = NDIlib_metadata_frame_t::default();

        // SAFETY: NDI SDK documentation states that recv_capture_v3 is thread-safe
        let frame_type = unsafe {
            NDIlib_recv_capture_v3(
                self.instance,
                ptr::null_mut(), // no video
                ptr::null_mut(), // no audio
                &mut metadata_frame,
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                let frame = MetadataFrame::from_raw(&metadata_frame);
                unsafe { NDIlib_recv_free_metadata(self.instance, &metadata_frame) };
                Ok(Some(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(None),
            NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::CaptureFailed("Received an error frame".into()))
            }
            _ => Ok(None), // Other frame types are ignored when capturing metadata only
        }
    }

    /// Capture metadata with automatic retry logic.
    ///
    /// Similar to `capture_video_with_retry`, this handles the NDI SDK's behavior
    /// where capture may return `None` immediately rather than blocking.
    ///
    /// # Arguments
    ///
    /// * `timeout_ms` - Timeout for each individual capture attempt in milliseconds
    /// * `max_attempts` - Maximum number of retry attempts before giving up
    ///
    /// # Returns
    ///
    /// * `Ok(Some(frame))` - Successfully captured a metadata frame
    /// * `Ok(None)` - No frame available after all retry attempts
    /// * `Err(_)` - An error occurred during capture
    pub fn capture_metadata_with_retry(
        &self,
        timeout_ms: u32,
        max_attempts: usize,
    ) -> Result<Option<MetadataFrame>> {
        for attempt in 1..=max_attempts {
            match self.capture_metadata(timeout_ms)? {
                Some(frame) => return Ok(Some(frame)),
                None => {
                    // Don't sleep on the last attempt
                    if attempt < max_attempts {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Capture metadata, blocking until a frame is received or timeout expires.
    ///
    /// This is the recommended method for reliable metadata frame capture.
    ///
    /// # Arguments
    ///
    /// * `total_timeout_ms` - Total time to wait for a frame in milliseconds
    ///
    /// # Returns
    ///
    /// * `Ok(frame)` - Successfully captured a metadata frame
    /// * `Err(Error::FrameTimeout)` - No frame received within the timeout period (includes retry details)
    /// * `Err(_)` - Another error occurred during capture
    pub fn capture_metadata_blocking(&self, total_timeout_ms: u32) -> Result<MetadataFrame> {
        let start_time = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(total_timeout_ms.into());
        let mut attempts = 0;

        loop {
            attempts += 1;

            // Check if we've exceeded our total timeout
            let elapsed = start_time.elapsed();
            if elapsed > timeout {
                return Err(Error::FrameTimeout { attempts, elapsed });
            }

            // Try to capture with a short per-attempt timeout (100ms)
            match self.capture_metadata(100)? {
                Some(frame) => return Ok(frame),
                None => {
                    // Brief sleep before retry to avoid busy-waiting
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
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
    /// # use grafton_ndi::{NDI, ReceiverOptions, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let receiver = ReceiverOptions::builder(source).build(&ndi)?;
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

    /// Get the source this receiver is connected to.
    ///
    /// Returns a reference to the [`Source`] that was specified when creating
    /// this receiver. This is useful for identifying which source a receiver
    /// is associated with when managing multiple receivers.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, ReceiverOptions, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let receiver = ReceiverOptions::builder(source).build(&ndi)?;
    /// let source = receiver.source();
    /// println!("Connected to: {}", source.name);
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
    /// # use grafton_ndi::{NDI, ReceiverOptions, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let receiver = ReceiverOptions::builder(source).build(&ndi)?;
    /// let stats = receiver.connection_stats();
    /// println!("Connections: {}", stats.connections);
    /// println!("Video frames: {} (dropped: {})",
    ///          stats.video_frames_received,
    ///          stats.video_frames_dropped);
    /// println!("Frame drop rate: {:.2}%",
    ///          stats.video_drop_percentage());
    /// # Ok(())
    /// # }
    /// ```
    pub fn connection_stats(&self) -> ConnectionStats {
        // Get number of active connections
        let connections = unsafe { NDIlib_recv_get_no_connections(self.instance) };

        // Get performance statistics (total and dropped frames)
        let mut total = NDIlib_recv_performance_t::default();
        let mut dropped = NDIlib_recv_performance_t::default();
        unsafe {
            NDIlib_recv_get_performance(self.instance, &mut total, &mut dropped);
        }

        // Get queue depth
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
    /// Returns None on timeout, Some(RecvStatus) when status has changed
    pub fn poll_status_change(&self, timeout_ms: u32) -> Option<ReceiverStatus> {
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

                Some(ReceiverStatus {
                    tally,
                    connections,
                    other: !has_tally && !has_connections,
                })
            }
            _ => None,
        }
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
/// mentioned in the SDK manual's thread safety section. The capture_video, capture_audio,
/// and capture_metadata methods can be safely called from multiple threads simultaneously.
unsafe impl Sync for Receiver {}

#[derive(Debug)]
pub enum FrameType {
    Video(VideoFrame),
    Audio(AudioFrame),
    Metadata(MetadataFrame),
    None,
    StatusChange(ReceiverStatus),
}

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
