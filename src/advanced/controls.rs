use super::{ffi, AdvancedConfig, VideoPacketFormat};
use crate::{
    ndi_lib::*, Receiver, ReceiverMode, ReceiverOptions, Result, Sender, SenderOptions, Tally, NDI,
};
use std::{
    ffi::{c_char, CString},
    time::Duration,
};

/// Owned discovery snapshot including optional per-source Advanced metadata.
#[derive(Clone, Debug)]
pub struct ExtendedSource {
    pub(crate) source: crate::Source,
    pub(crate) metadata: Option<String>,
}
impl ExtendedSource {
    pub fn source(&self) -> &crate::Source {
        &self.source
    }
    pub fn metadata(&self) -> Option<&str> {
        self.metadata.as_deref()
    }
}

impl Sender {
    /// Create a sender with owned, redacted Advanced SDK JSON configuration.
    pub fn new_with_config(
        ndi: &NDI,
        options: &SenderOptions,
        config: &AdvancedConfig,
    ) -> Result<Self> {
        type Create = unsafe extern "C" fn(
            *const NDIlib_send_create_t,
            *const c_char,
        ) -> NDIlib_send_instance_t;
        #[cfg(ndi_has_advanced_header)]
        let _: Create = crate::ndi_lib::raw::NDIlib_send_create_v2;
        let create: Create = unsafe { ffi::required(ndi, b"NDIlib_send_create_v2\0")? };
        let name = CString::new(options.name.as_str())?;
        let groups = options.groups.as_deref().map(CString::new).transpose()?;
        // Retain the exact allocation passed to native creation. A copy made
        // afterwards would not keep a potentially borrowed SDK pointer alive.
        let configuration = CString::new(config.expose_json()).expect("validated JSON");
        let raw = NDIlib_send_create_t {
            p_ndi_name: name.as_ptr(),
            p_groups: groups.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            clock_video: options.clock_video,
            clock_audio: options.clock_audio,
        };
        let instance = unsafe { create(&raw, configuration.as_ptr()) };
        if instance.is_null() {
            return Err(crate::Error::InitializationFailed(
                "Advanced sender creation failed".into(),
            ));
        }
        Ok(unsafe { Self::from_native(ndi, instance, name, groups, Some(configuration)) })
    }

    /// Override the sender's local tally contribution. Native rejection is preserved.
    pub fn set_external_tally(&self, tally: Tally) -> Result<bool> {
        type Set = unsafe extern "C" fn(NDIlib_send_instance_t, *const NDIlib_tally_t) -> bool;
        #[cfg(ndi_has_advanced_header)]
        let _: Set = crate::ndi_lib::raw::NDIlib_send_set_tally;
        let set: Set = unsafe { ffi::required(self.runtime(), b"NDIlib_send_set_tally\0")? };
        let raw = NDIlib_tally_t {
            on_program: tally.on_program,
            on_preview: tally.on_preview,
        };
        Ok(unsafe { set(self.native_instance(), &raw) })
    }

    /// Encoder target size in bytes; negative native values are rejected.
    pub fn target_frame_bytes(&self, format: VideoPacketFormat) -> Result<u32> {
        self.encoder_quantity(format, b"NDIlib_send_get_target_frame_size\0")
    }
    /// Encoder target bitrate in bits per second.
    pub fn target_bit_rate(&self, format: VideoPacketFormat) -> Result<u32> {
        self.encoder_quantity(format, b"NDIlib_send_get_target_bit_rate\0")
    }
    fn encoder_quantity(&self, format: VideoPacketFormat, symbol: &'static [u8]) -> Result<u32> {
        type Get =
            unsafe extern "C" fn(NDIlib_send_instance_t, *const NDIlib_video_frame_v2_t) -> i32;
        #[cfg(ndi_has_advanced_header)]
        {
            let _: Get = crate::ndi_lib::raw::NDIlib_send_get_target_frame_size;
            let _: Get = crate::ndi_lib::raw::NDIlib_send_get_target_bit_rate;
            let _: Get = crate::ndi_lib::raw::NDIlib_send_get_q_factor;
        }
        let get: Get = unsafe { ffi::required(self.runtime(), symbol)? };
        u32::try_from(unsafe { get(self.native_instance(), &format.raw()) })
            .map_err(|_| ffi::invalid("Encoder guidance is unavailable for this frame format"))
    }
    /// Legacy SpeedHQ quality guidance, without permission to emit a new format.
    pub fn legacy_quality(&self, format: VideoPacketFormat) -> Result<u32> {
        self.encoder_quantity(format, b"NDIlib_send_get_q_factor\0")
    }
    /// Query actual quality and whether negotiated receivers permit the newer format.
    pub fn quality_plan(
        &self,
        format: VideoPacketFormat,
        supports_new_format: bool,
    ) -> Result<QualityPlan> {
        type Get = unsafe extern "C" fn(
            NDIlib_send_instance_t,
            *const NDIlib_video_frame_v2_t,
            bool,
            *mut bool,
        ) -> i32;
        #[cfg(ndi_has_advanced_header)]
        let _: Get = crate::ndi_lib::raw::NDIlib_send_get_q_factor_v2;
        let get: Get = unsafe { ffi::required(self.runtime(), b"NDIlib_send_get_q_factor_v2\0")? };
        let mut short_dc = false;
        let value = unsafe {
            get(
                self.native_instance(),
                &format.raw(),
                supports_new_format,
                &mut short_dc,
            )
        };
        let value = u32::try_from(value)
            .map_err(|_| ffi::invalid("Quality guidance requires a supported SpeedHQ frame"))?;
        Ok(QualityPlan {
            quality: if value >= 100 { value - 100 } else { value },
            new_format: value >= 100,
            short_dc,
        })
    }
    /// Query keyframe demand for one stream, or for any stream when format is None.
    pub fn keyframe_required(&self, format: Option<VideoPacketFormat>) -> Result<bool> {
        type Get =
            unsafe extern "C" fn(NDIlib_send_instance_t, *const NDIlib_video_frame_v2_t) -> bool;
        #[cfg(ndi_has_advanced_header)]
        let _: Get = crate::ndi_lib::raw::NDIlib_send_is_keyframe_required;
        let get: Get =
            unsafe { ffi::required(self.runtime(), b"NDIlib_send_is_keyframe_required\0")? };
        let raw = format.map(VideoPacketFormat::raw);
        Ok(unsafe {
            get(
                self.native_instance(),
                raw.as_ref().map_or(std::ptr::null(), |r| r),
            )
        })
    }
    /// Wait for a demand change. Query keyframe_required afterwards for its state.
    pub fn wait_for_keyframe_change(
        &self,
        timeout: Duration,
        format: Option<VideoPacketFormat>,
    ) -> Result<bool> {
        type Wait = unsafe extern "C" fn(
            NDIlib_send_instance_t,
            u32,
            *const NDIlib_video_frame_v2_t,
        ) -> bool;
        #[cfg(ndi_has_advanced_header)]
        let _: Wait = crate::ndi_lib::raw::NDIlib_send_wait_for_keyframe_request;
        let wait: Wait =
            unsafe { ffi::required(self.runtime(), b"NDIlib_send_wait_for_keyframe_request\0")? };
        let raw = format.map(VideoPacketFormat::raw);
        Ok(unsafe {
            wait(
                self.native_instance(),
                crate::to_ms_checked(timeout)?,
                raw.as_ref().map_or(std::ptr::null(), |r| r),
            )
        })
    }
}

/// Encoder quality and negotiated SpeedHQ format permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QualityPlan {
    quality: u32,
    new_format: bool,
    short_dc: bool,
}
impl QualityPlan {
    pub fn quality(self) -> u32 {
        self.quality
    }
    pub fn permits_new_format(self) -> bool {
        self.new_format
    }
    pub fn enable_short_dc(self) -> bool {
        self.short_dc
    }
}

/// Only these quality choices are legal for dynamic bandwidth changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoQuality {
    Program,
    Preview,
}

/// Requested streams, constrained by the receiver's creation configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamSelection {
    video: bool,
    audio: bool,
    metadata: bool,
}
impl StreamSelection {
    pub fn new(video: bool, audio: bool, metadata: bool) -> Self {
        Self {
            video,
            audio,
            metadata,
        }
    }
    pub fn video(self) -> bool {
        self.video
    }
    pub fn audio(self) -> bool {
        self.audio
    }
    pub fn metadata(self) -> bool {
        self.metadata
    }
}

/// The SDK does not distinguish unchanged settings from a rejected stream request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamChange {
    Applied,
    UnchangedOrRejected,
}

impl Receiver {
    pub fn new_with_config(
        ndi: &NDI,
        options: &ReceiverOptions,
        config: &AdvancedConfig,
    ) -> Result<Self> {
        options.validate_runtime(ndi)?;
        type Create = unsafe extern "C" fn(
            *const NDIlib_recv_create_v3_t,
            *const c_char,
        ) -> NDIlib_recv_instance_t;
        #[cfg(ndi_has_advanced_header)]
        let _: Create = crate::ndi_lib::raw::NDIlib_recv_create_v4;
        let create: Create = unsafe { ffi::required(ndi, b"NDIlib_recv_create_v4\0")? };
        let raw = options.to_raw()?;
        let instance = unsafe { create(&raw.raw, config.as_ptr()) };
        let mut receiver = unsafe { Self::from_native(ndi, options, instance)? };
        receiver.retain_config(config.clone());
        Ok(receiver)
    }
}

impl<M: ReceiverMode> Receiver<M> {
    /// Ask the source for a keyframe; delivery is not immediate or guaranteed.
    pub fn request_keyframe(&self) -> Result<()> {
        type Request = unsafe extern "C" fn(NDIlib_recv_instance_t);
        #[cfg(ndi_has_advanced_header)]
        let _: Request = crate::ndi_lib::raw::NDIlib_recv_request_keyframe;
        let request: Request =
            unsafe { ffi::required(&self.core._ndi, b"NDIlib_recv_request_keyframe\0")? };
        unsafe { request(self.core.instance) };
        Ok(())
    }
    /// Change program/preview quality, retaining video-only creation semantics.
    pub fn set_video_quality(&mut self, quality: VideoQuality) -> Result<bool> {
        type Set = unsafe extern "C" fn(NDIlib_recv_instance_t, NDIlib_recv_bandwidth_e) -> bool;
        #[cfg(ndi_has_advanced_header)]
        let _: Set = crate::ndi_lib::raw::NDIlib_recv_set_bandwidth;
        let set: Set = unsafe { ffi::required(&self.core._ndi, b"NDIlib_recv_set_bandwidth\0")? };
        let value = match quality {
            VideoQuality::Program => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
            VideoQuality::Preview => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
        };
        Ok(unsafe { set(self.core.instance, value) })
    }
    pub fn set_streams(&mut self, streams: StreamSelection) -> Result<StreamChange> {
        type Set = unsafe extern "C" fn(NDIlib_recv_instance_t, bool, bool, bool) -> bool;
        #[cfg(ndi_has_advanced_header)]
        let _: Set = crate::ndi_lib::raw::NDIlib_recv_set_enabled_streams;
        let set: Set =
            unsafe { ffi::required(&self.core._ndi, b"NDIlib_recv_set_enabled_streams\0")? };
        Ok(
            if unsafe {
                set(
                    self.core.instance,
                    streams.video,
                    streams.audio,
                    streams.metadata,
                )
            } {
                StreamChange::Applied
            } else {
                StreamChange::UnchangedOrRejected
            },
        )
    }
}
