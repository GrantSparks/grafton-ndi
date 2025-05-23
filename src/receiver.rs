//! NDI receiving functionality for video, audio, and metadata.

use crate::{
    error::Error,
    finder::{RawSource, Source},
    frames::{AudioFrame, MetadataFrame, VideoFrame},
    ndi_lib::*,
    NDI,
};
use std::{ffi::CString, ptr};

#[derive(Debug, Default, Clone, Copy)]
pub enum RecvColorFormat {
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

impl From<RecvColorFormat> for NDIlib_recv_color_format_e {
    fn from(format: RecvColorFormat) -> Self {
        match format {
            RecvColorFormat::BGRX_BGRA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA
            }
            RecvColorFormat::UYVY_BGRA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_BGRA
            }
            RecvColorFormat::RGBX_RGBA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA
            }
            RecvColorFormat::UYVY_RGBA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_RGBA
            }
            RecvColorFormat::Fastest => NDIlib_recv_color_format_e_NDIlib_recv_color_format_fastest,
            RecvColorFormat::Best => NDIlib_recv_color_format_e_NDIlib_recv_color_format_best,
            //            RecvColorFormat::BGRX_BGRA_Flipped => {
            //                NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA_flipped
            //            }
            RecvColorFormat::Max => NDIlib_recv_color_format_e_NDIlib_recv_color_format_max,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub enum RecvBandwidth {
    MetadataOnly,
    AudioOnly,
    Lowest,
    #[default]
    Highest,
    Max,
}

impl From<RecvBandwidth> for NDIlib_recv_bandwidth_e {
    fn from(bandwidth: RecvBandwidth) -> Self {
        match bandwidth {
            RecvBandwidth::MetadataOnly => {
                NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_metadata_only
            }
            RecvBandwidth::AudioOnly => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_audio_only,
            RecvBandwidth::Lowest => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
            RecvBandwidth::Highest => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
            RecvBandwidth::Max => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_max,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Receiver {
    pub source_to_connect_to: Source,
    pub color_format: RecvColorFormat,
    pub bandwidth: RecvBandwidth,
    pub allow_video_fields: bool,
    pub ndi_recv_name: Option<String>,
}

#[repr(C)]
pub(crate) struct RawRecvCreateV3 {
    _source: RawSource,
    _name: Option<CString>,
    pub raw: NDIlib_recv_create_v3_t,
}

impl Receiver {
    /// Convert to raw format for FFI use
    ///
    /// # Safety
    ///
    /// The returned RawRecvCreateV3 struct uses #[repr(C)] to guarantee C-compatible layout
    /// for safe FFI interop with the NDI SDK.
    pub(crate) fn to_raw(&self) -> Result<RawRecvCreateV3, Error> {
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
    pub fn builder(source: Source) -> ReceiverBuilder {
        ReceiverBuilder::new(source)
    }
}

/// Builder for configuring a Receiver with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct ReceiverBuilder {
    source_to_connect_to: Source,
    color_format: Option<RecvColorFormat>,
    bandwidth: Option<RecvBandwidth>,
    allow_video_fields: Option<bool>,
    ndi_recv_name: Option<String>,
}

impl ReceiverBuilder {
    /// Create a new builder with the specified source
    pub fn new(source: Source) -> Self {
        ReceiverBuilder {
            source_to_connect_to: source,
            color_format: None,
            bandwidth: None,
            allow_video_fields: None,
            ndi_recv_name: None,
        }
    }

    /// Set the color format for received video
    pub fn color(mut self, fmt: RecvColorFormat) -> Self {
        self.color_format = Some(fmt);
        self
    }

    /// Set the bandwidth mode for the receiver
    pub fn bandwidth(mut self, bw: RecvBandwidth) -> Self {
        self.bandwidth = Some(bw);
        self
    }

    /// Configure whether to allow video fields
    pub fn allow_video_fields(mut self, allow: bool) -> Self {
        self.allow_video_fields = Some(allow);
        self
    }

    /// Set the name for this receiver
    pub fn name<S: Into<String>>(mut self, name: S) -> Self {
        self.ndi_recv_name = Some(name.into());
        self
    }

    /// Build the receiver and create a Recv instance
    pub fn build(self, ndi: &NDI) -> Result<Recv<'_>, Error> {
        let receiver = Receiver {
            source_to_connect_to: self.source_to_connect_to,
            color_format: self.color_format.unwrap_or(RecvColorFormat::BGRX_BGRA),
            bandwidth: self.bandwidth.unwrap_or(RecvBandwidth::Highest),
            allow_video_fields: self.allow_video_fields.unwrap_or(true),
            ndi_recv_name: self.ndi_recv_name,
        };
        Recv::new(ndi, &receiver)
    }
}

pub struct Recv<'a> {
    pub(crate) instance: NDIlib_recv_instance_t,
    ndi: std::marker::PhantomData<&'a NDI>,
}

impl<'a> Recv<'a> {
    pub fn new(_ndi: &'a NDI, create: &Receiver) -> Result<Self, Error> {
        let create_raw = create.to_raw()?;
        // NDIlib_recv_create_v3 already connects to the source specified in source_to_connect_to
        let instance = unsafe { NDIlib_recv_create_v3(&create_raw.raw) };
        if instance.is_null() {
            Err(Error::InitializationFailed(
                "Failed to create NDI recv instance".into(),
            ))
        } else {
            Ok(Recv {
                instance,
                ndi: std::marker::PhantomData,
            })
        }
    }

    /// Capture a frame with owned data (copies the frame data)
    #[deprecated(
        note = "Use capture_video, capture_audio, or capture_metadata for concurrent access"
    )]
    pub fn capture(&mut self, timeout_ms: u32) -> Result<FrameType<'_>, Error> {
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
                let frame = unsafe { VideoFrame::from_raw(&video_frame, Some(self.instance)) }?;
                // Note: Drop impl will call NDIlib_recv_free_video_v2 when frame is dropped
                Ok(FrameType::Video(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_audio => {
                let frame = AudioFrame::from_raw(audio_frame, Some(self.instance))?;
                // Note: Drop impl will call NDIlib_recv_free_audio_v3 when frame is dropped
                Ok(FrameType::Audio(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                let frame = MetadataFrame::from_raw(&metadata_frame);
                unsafe { NDIlib_recv_free_metadata(self.instance, &metadata_frame) };
                Ok(FrameType::Metadata(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(FrameType::None),
            NDIlib_frame_type_e_NDIlib_frame_type_status_change => Ok(FrameType::StatusChange),
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

    pub fn ptz_recall_preset(&self, preset: u32, speed: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_recall_preset(self.instance, preset as i32, speed) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to recall PTZ preset {} with speed {}",
                preset, speed
            )))
        }
    }

    pub fn ptz_zoom(&self, zoom_value: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_zoom(self.instance, zoom_value) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to set PTZ zoom to {}",
                zoom_value
            )))
        }
    }

    pub fn ptz_zoom_speed(&self, zoom_speed: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_zoom_speed(self.instance, zoom_speed) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to set PTZ zoom speed to {}",
                zoom_speed
            )))
        }
    }

    pub fn ptz_pan_tilt(&self, pan_value: f32, tilt_value: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_pan_tilt(self.instance, pan_value, tilt_value) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to set PTZ pan/tilt to ({}, {})",
                pan_value, tilt_value
            )))
        }
    }

    pub fn ptz_pan_tilt_speed(&self, pan_speed: f32, tilt_speed: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_pan_tilt_speed(self.instance, pan_speed, tilt_speed) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to set PTZ pan/tilt speed to ({}, {})",
                pan_speed, tilt_speed
            )))
        }
    }

    pub fn ptz_store_preset(&self, preset_no: i32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_store_preset(self.instance, preset_no) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to store PTZ preset {}",
                preset_no
            )))
        }
    }

    pub fn ptz_auto_focus(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_auto_focus(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(
                "Failed to enable PTZ auto focus".into(),
            ))
        }
    }

    pub fn ptz_focus(&self, focus_value: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_focus(self.instance, focus_value) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to set PTZ focus".into()))
        }
    }

    pub fn ptz_focus_speed(&self, focus_speed: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_focus_speed(self.instance, focus_speed) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(
                "Failed to set PTZ focus speed".into(),
            ))
        }
    }

    pub fn ptz_white_balance_auto(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_white_balance_auto(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(
                "Failed to set PTZ auto white balance".into(),
            ))
        }
    }

    pub fn ptz_white_balance_indoor(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_white_balance_indoor(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(
                "Failed to set PTZ indoor white balance".into(),
            ))
        }
    }

    pub fn ptz_white_balance_outdoor(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_white_balance_outdoor(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(
                "Failed to set PTZ outdoor white balance".into(),
            ))
        }
    }

    pub fn ptz_white_balance_oneshot(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_white_balance_oneshot(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(
                "Failed to set PTZ oneshot white balance".into(),
            ))
        }
    }

    pub fn ptz_white_balance_manual(&self, red: f32, blue: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_white_balance_manual(self.instance, red, blue) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(
                "Failed to set PTZ manual white balance".into(),
            ))
        }
    }

    pub fn ptz_exposure_auto(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_exposure_auto(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(
                "Failed to set PTZ auto exposure".into(),
            ))
        }
    }

    pub fn ptz_exposure_manual(&self, exposure_level: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_exposure_manual(self.instance, exposure_level) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(
                "Failed to set PTZ manual exposure".into(),
            ))
        }
    }

    pub fn ptz_exposure_manual_v2(
        &self,
        iris: f32,
        gain: f32,
        shutter_speed: f32,
    ) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_exposure_manual_v2(self.instance, iris, gain, shutter_speed) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(
                "Failed to set PTZ manual exposure v2".into(),
            ))
        }
    }

    /// Capture only video frames - safe to call from multiple threads concurrently
    pub fn capture_video(&self, timeout_ms: u32) -> Result<Option<VideoFrame<'_>>, Error> {
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
                let frame = unsafe { VideoFrame::from_raw(&video_frame, Some(self.instance)) }?;
                Ok(Some(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(None),
            NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::CaptureFailed("Received an error frame".into()))
            }
            _ => Ok(None), // Other frame types are ignored when capturing video only
        }
    }

    /// Capture only audio frames - safe to call from multiple threads concurrently
    pub fn capture_audio(&self, timeout_ms: u32) -> Result<Option<AudioFrame<'_>>, Error> {
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
                let frame = AudioFrame::from_raw(audio_frame, Some(self.instance))?;
                Ok(Some(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(None),
            NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::CaptureFailed("Received an error frame".into()))
            }
            _ => Ok(None), // Other frame types are ignored when capturing audio only
        }
    }

    /// Capture only metadata frames - safe to call from multiple threads concurrently
    pub fn capture_metadata(&self, timeout_ms: u32) -> Result<Option<MetadataFrame>, Error> {
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
}

impl Drop for Recv<'_> {
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
/// The Recv struct only holds an opaque pointer returned by the SDK, and the SDK
/// guarantees that this pointer can be safely moved between threads.
unsafe impl std::marker::Send for Recv<'_> {}

/// # Safety
///
/// The NDI 6 SDK documentation guarantees that `NDIlib_recv_capture_v3` is internally
/// synchronized and can be called concurrently from multiple threads. This is explicitly
/// mentioned in the SDK manual's thread safety section. The capture_video, capture_audio,
/// and capture_metadata methods can be safely called from multiple threads simultaneously.
unsafe impl std::marker::Sync for Recv<'_> {}

#[derive(Debug)]
pub enum FrameType<'rx> {
    Video(VideoFrame<'rx>),
    Audio(AudioFrame<'rx>),
    Metadata(MetadataFrame),
    None,
    StatusChange,
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
