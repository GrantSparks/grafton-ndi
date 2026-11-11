use super::{ffi, packets, AdvancedConfig, CompressedAudioFrame, CompressedVideoFrame};
use crate::{
    ndi_lib::*, AudioFrame, MetadataFrame, Receiver, ReceiverOptions, Result, VideoFrame, NDI,
};
use std::{ffi::c_char, marker::PhantomData, rc::Rc, time::Duration};

/// Documented codec passthrough levels, including decoded fallback where supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
#[non_exhaustive]
pub enum PassthroughPreset {
    SpeedHqOnly = 300,
    SpeedHq = 301,
    H264 = 302,
    Hevc = 303,
    H264WithAudio = 304,
    HevcWithAudio = 305,
    Alpha = 307,
    AlphaWithAudio = 308,
    SpeedHqBest = 309,
    H264Best = 310,
    HevcBest = 311,
    AlphaBest = 312,
    Protected = 313,
    ProtectedBest = 314,
    ProtectedWithAudio = 315,
}

/// A captured video can be a decoded frame or a codec packet under the same preset.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReceivedVideo {
    Decoded(VideoFrame),
    Compressed(CompressedVideoFrame),
}
#[derive(Debug)]
#[non_exhaustive]
pub enum ReceivedAudio {
    Decoded(AudioFrame),
    Compressed(CompressedAudioFrame),
}

/// All events from one receive call, including status/source changes.
#[derive(Debug)]
#[non_exhaustive]
pub enum PacketEvent {
    Video(ReceivedVideo),
    Audio(ReceivedAudio),
    Metadata(MetadataFrame),
    Timeout,
    StatusChanged,
    SourceChanged,
    Unknown(u32),
}

/// Receiver with an explicit packet result model. It cannot be attached to FrameSync.
/// Calls require exclusive access; captured results own their data.
pub struct PacketReceiver {
    pub(crate) receiver: Receiver,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl PacketReceiver {
    pub fn new(
        ndi: &NDI,
        options: &ReceiverOptions,
        preset: PassthroughPreset,
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
        let mut raw = options.to_raw()?;
        raw.raw.color_format = preset as i32 as _;
        raw.raw.allow_video_fields = true;
        let instance = unsafe { create(&raw.raw, config.as_ptr()) };
        let mut receiver = unsafe { Receiver::from_native(ndi, options, instance)? };
        receiver.retain_config(config.clone());
        Ok(Self {
            receiver,
            _thread_affinity: PhantomData,
        })
    }

    pub fn capture(&mut self, timeout: Duration) -> Result<PacketEvent> {
        Self::capture_from(&self.receiver, timeout)
    }

    pub(crate) fn capture_from(receiver: &Receiver, timeout: Duration) -> Result<PacketEvent> {
        let mut video = NDIlib_video_frame_v2_t::default();
        let mut audio = NDIlib_audio_frame_v3_t::default();
        let mut metadata = NDIlib_metadata_frame_t::default();
        let instance = receiver.core.instance;
        let kind = unsafe {
            NDIlib_recv_capture_v3(
                instance,
                &mut video,
                &mut audio,
                &mut metadata,
                crate::to_ms_checked(timeout)?,
            )
        };
        match kind {
            NDIlib_frame_type_e_NDIlib_frame_type_video => {
                struct Free(NDIlib_recv_instance_t, NDIlib_video_frame_v2_t);
                impl Drop for Free {
                    fn drop(&mut self) {
                        unsafe { NDIlib_recv_free_video_v2(self.0, &self.1) };
                    }
                }
                let guard = Free(instance, video);
                Ok(PacketEvent::Video(
                    if super::VideoCodec::from_outer(ffi::video_fourcc_bits(guard.1.FourCC))
                        .is_some()
                    {
                        ReceivedVideo::Compressed(unsafe { packets::copy_video_packet(&guard.1)? })
                    } else {
                        ReceivedVideo::Decoded(unsafe {
                            VideoFrame::from_raw_for(&guard.1, &receiver.core._ndi)?
                        })
                    },
                ))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_audio => {
                struct Free(NDIlib_recv_instance_t, NDIlib_audio_frame_v3_t);
                impl Drop for Free {
                    fn drop(&mut self) {
                        unsafe { NDIlib_recv_free_audio_v3(self.0, &self.1) };
                    }
                }
                let guard = Free(instance, audio);
                Ok(PacketEvent::Audio(
                    if guard.1.FourCC == NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP {
                        ReceivedAudio::Decoded(unsafe {
                            AudioFrame::from_raw_validated(
                                guard.1,
                                crate::frames::validate_audio_layout(&guard.1)?,
                                crate::frames::validate_frame_metadata(guard.1.p_metadata)?,
                            )?
                        })
                    } else {
                        ReceivedAudio::Compressed(unsafe { packets::copy_audio_packet(&guard.1)? })
                    },
                ))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                struct Free(NDIlib_recv_instance_t, NDIlib_metadata_frame_t);
                impl Drop for Free {
                    fn drop(&mut self) {
                        unsafe { NDIlib_recv_free_metadata(self.0, &self.1) };
                    }
                }
                let guard = Free(instance, metadata);
                Ok(PacketEvent::Metadata(unsafe {
                    MetadataFrame::from_raw_validated(
                        &guard.1,
                        crate::frames::validate_metadata_layout(&guard.1)?,
                    )
                }))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(PacketEvent::Timeout),
            NDIlib_frame_type_e_NDIlib_frame_type_status_change => Ok(PacketEvent::StatusChanged),
            NDIlib_frame_type_e_NDIlib_frame_type_source_change => Ok(PacketEvent::SourceChanged),
            NDIlib_frame_type_e_NDIlib_frame_type_error => Err(crate::Error::CaptureFailed(
                "Packet receiver reported an SDK error".into(),
            )),
            // Bindgen chooses the native enum integer type for each target.
            #[allow(clippy::unnecessary_cast)]
            other => Ok(PacketEvent::Unknown(other as u32)),
        }
    }
    pub fn set_video_quality(&mut self, quality: super::VideoQuality) -> Result<bool> {
        self.receiver.set_video_quality(quality)
    }
    pub fn set_streams(&mut self, streams: super::StreamSelection) -> Result<super::StreamChange> {
        self.receiver.set_streams(streams)
    }
    pub fn request_keyframe(&self) -> Result<()> {
        self.receiver.request_keyframe()
    }
    pub fn send_metadata(&self, metadata: &MetadataFrame) -> bool {
        self.receiver.send_metadata(metadata)
    }
    pub fn is_connected(&self) -> bool {
        self.receiver.is_connected()
    }
}
