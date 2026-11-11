//! Receiving compressed packets instead of decoded frames.

use std::{marker::PhantomData, rc::Rc, time::Duration};

use super::{
    packets, AdvancedConfig, AudioCodec, CompressedAudioFrame, CompressedVideoFrame, VideoCodec,
};
use crate::{
    sys::{
        self,
        advanced::{native, ReceiverConfiguration},
        bindings::*,
    },
    AudioFrame, Error, MetadataFrame, Receiver, ReceiverOptions, Result, VideoFrame, NDI,
};

/// Which compressed formats a [`PacketReceiver`] passes through.
///
/// A frame in a format the preset names is delivered as the source sent it;
/// any other frame is delivered decoded, so every capture can yield either
/// kind. Decoded video is 8-bit unless the preset is one of the "best" ones,
/// which keep the source's bit depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PassthroughPreset {
    /// SpeedHQ only.
    SpeedHqOnly,
    /// SpeedHQ; other sources are decoded.
    SpeedHq,
    /// As [`SpeedHq`](Self::SpeedHq), decoding to the source's bit depth.
    SpeedHqBest,
    /// SpeedHQ and H.264.
    H264,
    /// As [`H264`](Self::H264), decoding to the source's bit depth.
    H264Best,
    /// As [`H264`](Self::H264), with compressed audio.
    H264WithAudio,
    /// SpeedHQ, H.264 and HEVC.
    Hevc,
    /// As [`Hevc`](Self::Hevc), decoding to the source's bit depth.
    HevcBest,
    /// As [`Hevc`](Self::Hevc), with compressed audio.
    HevcWithAudio,
    /// SpeedHQ, H.264 and HEVC, including H.264 and HEVC with alpha.
    Alpha,
    /// As [`Alpha`](Self::Alpha), decoding to the source's bit depth.
    AlphaBest,
    /// As [`Alpha`](Self::Alpha), with compressed audio including Opus.
    AlphaWithAudio,
    /// As [`Alpha`](Self::Alpha), for protected streams as well.
    Protected,
    /// As [`Protected`](Self::Protected), decoding to the source's bit depth.
    ProtectedBest,
    /// As [`Protected`](Self::Protected), with compressed audio including Opus.
    ProtectedWithAudio,
}

impl PassthroughPreset {
    /// The receiver color format that selects this preset.
    fn color_format(self, table: &'static ReceiverConfiguration) -> NDIlib_recv_color_format_e {
        native!(table => {
            use sys::advanced_bindings::*;
            let format = match self {
                Self::SpeedHqOnly => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v1
                }
                Self::SpeedHq => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v2
                }
                Self::SpeedHqBest => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v2_best
                }
                Self::H264 => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v3
                }
                Self::H264Best => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v3_best
                }
                Self::H264WithAudio => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v3_with_audio
                }
                Self::Hevc => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v4
                }
                Self::HevcBest => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v4_best
                }
                Self::HevcWithAudio => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v4_with_audio
                }
                Self::Alpha => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v5
                }
                Self::AlphaBest => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v5_best
                }
                Self::AlphaWithAudio => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v5_with_audio
                }
                Self::Protected => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v6
                }
                Self::ProtectedBest => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v6_best
                }
                Self::ProtectedWithAudio => {
                    NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v6_with_audio
                }
            };
            // The extended color formats take the target's C enum type; the
            // creation settings carry the same bits.
            NDIlib_recv_color_format_e::from_ne_bytes(format.to_ne_bytes())
        })
    }
}

/// Video from a [`PacketReceiver`]: a codec packet when the source's format is
/// one the preset passes through, a decoded frame otherwise.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReceivedVideo {
    /// A decoded frame.
    Decoded(VideoFrame),
    /// A codec packet, exactly as the source sent it.
    Compressed(CompressedVideoFrame),
}

/// Audio from a [`PacketReceiver`]: a codec packet when the source's format is
/// one the preset passes through, decoded samples otherwise.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReceivedAudio {
    /// Decoded planar floating-point audio.
    Decoded(AudioFrame),
    /// A codec packet, exactly as the source sent it.
    Compressed(CompressedAudioFrame),
}

/// What one [`PacketReceiver::capture`] call produced.
#[derive(Debug)]
#[non_exhaustive]
pub enum PacketEvent {
    /// A video frame.
    Video(ReceivedVideo),
    /// An audio frame.
    Audio(ReceivedAudio),
    /// A metadata frame.
    Metadata(MetadataFrame),
    /// Nothing arrived within the timeout.
    Timeout,
    /// The connection or the source's properties changed.
    StatusChanged,
    /// The receiver is now connected to a different source.
    SourceChanged,
    /// A frame type this crate does not know, with its native value.
    Unknown(u32),
}

/// A receiver that delivers compressed packets where it can.
///
/// One capture call returns whichever frame arrives first, as a
/// [`PacketEvent`]; every result owns its data. The receiver is used from the
/// thread that owns it, one call at a time. It cannot feed a
/// [`FrameSync`](crate::framesync::FrameSync), which needs decoded frames.
///
/// # Examples
///
/// ```no_run
/// use std::time::Duration;
/// use grafton_ndi::{
///     advanced::{AdvancedConfig, PacketEvent, PacketReceiver, PassthroughPreset, ReceivedVideo},
///     ReceiverOptions, Source, NDI,
/// };
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = NDI::new()?;
/// # let source = Source::default();
/// let mut receiver = PacketReceiver::new(
///     &ndi,
///     &ReceiverOptions::builder(source).build(),
///     PassthroughPreset::Hevc,
///     &AdvancedConfig::parse("{}")?,
/// )?;
/// if let PacketEvent::Video(ReceivedVideo::Compressed(packet)) =
///     receiver.capture(Duration::from_secs(1))?
/// {
///     println!("{:?}: {} bytes", packet.format().codec(), packet.data().len());
/// }
/// # Ok(())
/// # }
/// ```
pub struct PacketReceiver {
    pub(crate) receiver: Receiver,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl PacketReceiver {
    /// Create a receiver that passes through the formats `preset` names.
    ///
    /// The options' color format is replaced by the preset; fielded video is
    /// always allowed, because a compressed stream cannot be deinterlaced.
    ///
    /// # Errors
    ///
    /// As for [`Receiver::new_with_config`].
    pub fn new(
        ndi: &NDI,
        options: &ReceiverOptions,
        preset: PassthroughPreset,
        config: &AdvancedConfig,
    ) -> Result<Self> {
        let table = ndi.advanced::<ReceiverConfiguration>()?;
        let color_format = preset.color_format(table);
        let receiver = Receiver::configured(ndi, table, options, config, |create| {
            create.color_format = color_format;
            create.allow_video_fields = true;
        })?;
        Ok(Self::from_receiver(receiver))
    }

    pub(crate) fn from_receiver(receiver: Receiver) -> Self {
        Self {
            receiver,
            _thread_affinity: PhantomData,
        }
    }

    /// Wait up to `timeout` for the next frame or event.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfiguration`] when `timeout` exceeds
    /// [`crate::MAX_TIMEOUT`], [`Error::CaptureFailed`] when the SDK reports a
    /// lost connection, and [`Error::InvalidFrame`] when a delivered frame
    /// fails validation.
    pub fn capture(&mut self, timeout: Duration) -> Result<PacketEvent> {
        Self::capture_from(&self.receiver, timeout)
    }

    pub(crate) fn capture_from(receiver: &Receiver, timeout: Duration) -> Result<PacketEvent> {
        let timeout = crate::to_ms_checked(timeout)?;
        let mut video = NDIlib_video_frame_v2_t::default();
        let mut audio = NDIlib_audio_frame_v3_t::default();
        let mut metadata = NDIlib_metadata_frame_t::default();
        let instance = receiver.core.instance;
        let sdk = receiver.core.sdk();
        // SAFETY: the receiver is live, the three descriptors are writable,
        // and this type's exclusive borrow keeps captures from overlapping.
        let kind = unsafe {
            sdk.NDIlib_recv_capture_v3(instance, &mut video, &mut audio, &mut metadata, timeout)
        };
        match kind {
            NDIlib_frame_type_e_NDIlib_frame_type_video => {
                let frame = Delivered {
                    receiver: instance,
                    frame: video,
                    // SAFETY: the receiver delivered this frame and it is
                    // returned exactly once.
                    release: |receiver, frame| unsafe {
                        sdk.NDIlib_recv_free_video_v2(receiver, frame)
                    },
                };
                let video = if VideoCodec::from_outer(frame.frame.FourCC).is_some() {
                    // SAFETY: the FourCC marks a compressed frame, which the
                    // SDK keeps valid until it is released.
                    ReceivedVideo::Compressed(unsafe { packets::copy_video_packet(&frame.frame)? })
                } else {
                    // SAFETY: the SDK keeps the frame valid until it is released.
                    ReceivedVideo::Decoded(unsafe {
                        VideoFrame::from_raw_for(&frame.frame, &receiver.core._ndi)?
                    })
                };
                Ok(PacketEvent::Video(video))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_audio => {
                let frame = Delivered {
                    receiver: instance,
                    frame: audio,
                    // SAFETY: the receiver delivered this frame and it is
                    // returned exactly once.
                    release: |receiver, frame| unsafe {
                        sdk.NDIlib_recv_free_audio_v3(receiver, frame)
                    },
                };
                let audio = if AudioCodec::from_outer(frame.frame.FourCC).is_some() {
                    // SAFETY: the FourCC marks a compressed frame, which the
                    // SDK keeps valid until it is released.
                    ReceivedAudio::Compressed(unsafe { packets::copy_audio_packet(&frame.frame)? })
                } else {
                    let layout = crate::frames::validate_audio_layout(&frame.frame)?;
                    // SAFETY: the SDK keeps the frame and its metadata valid
                    // until the frame is released.
                    ReceivedAudio::Decoded(unsafe {
                        let metadata =
                            crate::frames::validate_frame_metadata(frame.frame.p_metadata)?;
                        AudioFrame::from_raw_validated(frame.frame, layout, metadata)?
                    })
                };
                Ok(PacketEvent::Audio(audio))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                let frame = Delivered {
                    receiver: instance,
                    frame: metadata,
                    // SAFETY: the receiver delivered this frame and it is
                    // returned exactly once.
                    release: |receiver, frame| unsafe {
                        sdk.NDIlib_recv_free_metadata(receiver, frame)
                    },
                };
                // SAFETY: the SDK keeps the frame valid until it is released.
                Ok(PacketEvent::Metadata(unsafe {
                    let layout = crate::frames::validate_metadata_layout(&frame.frame)?;
                    MetadataFrame::from_raw_validated(&frame.frame, layout)
                }))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(PacketEvent::Timeout),
            NDIlib_frame_type_e_NDIlib_frame_type_status_change => Ok(PacketEvent::StatusChanged),
            NDIlib_frame_type_e_NDIlib_frame_type_source_change => Ok(PacketEvent::SourceChanged),
            NDIlib_frame_type_e_NDIlib_frame_type_error => Err(Error::CaptureFailed(
                "Packet receiver reported an SDK error".into(),
            )),
            other => Ok(PacketEvent::Unknown(other)),
        }
    }

    /// Switch between the source's program and preview streams; see
    /// [`Receiver::set_video_quality`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::set_video_quality`].
    pub fn set_video_quality(&mut self, quality: super::VideoQuality) -> Result<bool> {
        self.receiver.set_video_quality(quality)
    }

    /// Choose which streams the source sends; see [`Receiver::set_streams`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::set_streams`].
    pub fn set_streams(&mut self, streams: super::StreamSelection) -> Result<super::StreamChange> {
        self.receiver.set_streams(streams)
    }

    /// Ask the source for a keyframe, which a decoder needs before it can
    /// start on a compressed stream; see [`Receiver::request_keyframe`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::request_keyframe`].
    pub fn request_keyframe(&self) -> Result<()> {
        self.receiver.request_keyframe()
    }

    /// Send metadata upstream to the source. Returns whether the receiver is
    /// connected and the SDK accepted the frame.
    pub fn send_metadata(&self, metadata: &MetadataFrame) -> bool {
        self.receiver.send_metadata(metadata)
    }

    /// Whether the receiver is connected to its source.
    pub fn is_connected(&self) -> bool {
        self.receiver.is_connected()
    }
}

/// A frame the SDK delivered, returned to it when dropped: after the copy, or
/// when validation fails first.
struct Delivered<T, F: Fn(NDIlib_recv_instance_t, &T)> {
    receiver: NDIlib_recv_instance_t,
    frame: T,
    release: F,
}

impl<T, F: Fn(NDIlib_recv_instance_t, &T)> Drop for Delivered<T, F> {
    fn drop(&mut self) {
        (self.release)(self.receiver, &self.frame);
    }
}
