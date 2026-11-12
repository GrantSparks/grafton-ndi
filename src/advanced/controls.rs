//! Advanced SDK controls on the ordinary sender and receiver.

use std::time::Duration;

use super::{invalid, VideoPacketFormat};
use crate::{
    sys::{
        advanced::{
            native, EncoderGuidance, KeyframeDemand, KeyframeRequest, QualityPlanning,
            ReceiverBandwidthControl, ReceiverStreamControl, SenderTally,
        },
        bindings::*,
    },
    Receiver, ReceiverMode, Result, Sender, Tally,
};

/// Encoder guidance is a non-negative quantity; the SDK reports a frame format
/// it has no guidance for as a negative value.
fn encoder_quantity(value: i32) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| invalid("Encoder guidance is unavailable for this frame format"))
}

/// A null descriptor asks about any stream; otherwise the one `format` names.
fn stream(format: Option<&NDIlib_video_frame_v2_t>) -> *const NDIlib_video_frame_v2_t {
    format.map_or(std::ptr::null(), |format| format)
}

impl Sender {
    /// Set this sender's own contribution to its tally state, which is
    /// otherwise the combination of what its receivers report.
    ///
    /// Returns whether the SDK accepted the change.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime.
    pub fn set_external_tally(&self, tally: Tally) -> Result<bool> {
        let table = self.runtime().table::<SenderTally>()?;
        let tally = tally.to_raw();
        // SAFETY: the sender instance is live for this borrow.
        Ok(native!(SenderTally, table => unsafe {
            table.NDIlib_send_set_tally(self.native_instance(), &tally)
        }))
    }

    /// The compressed frame size, in bytes, an encoder should aim for when
    /// producing `format` for this sender's connections.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime, and
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when the SDK has no
    /// guidance for the format.
    pub fn target_frame_bytes(&self, format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        let format = format.raw();
        // SAFETY: the sender instance is live for this borrow.
        encoder_quantity(native!(EncoderGuidance, table => unsafe {
            table.NDIlib_send_get_target_frame_size(self.native_instance(), &format)
        }))
    }

    /// The bit rate, in bits per second, an encoder should aim for when
    /// producing `format` for this sender's connections.
    ///
    /// # Errors
    ///
    /// As for [`target_frame_bytes`](Self::target_frame_bytes).
    pub fn target_bit_rate(&self, format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        let format = format.raw();
        // SAFETY: the sender instance is live for this borrow.
        encoder_quantity(native!(EncoderGuidance, table => unsafe {
            table.NDIlib_send_get_target_bit_rate(self.native_instance(), &format)
        }))
    }

    /// The SpeedHQ quality factor for `format`, as the original quality query
    /// reports it. It says nothing about whether receivers accept the newer
    /// SpeedHQ format; [`quality_plan`](Self::quality_plan) does.
    ///
    /// # Errors
    ///
    /// As for [`target_frame_bytes`](Self::target_frame_bytes).
    pub fn legacy_quality(&self, format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        let format = format.raw();
        // SAFETY: the sender instance is live for this borrow.
        encoder_quantity(native!(EncoderGuidance, table => unsafe {
            table.NDIlib_send_get_q_factor(self.native_instance(), &format)
        }))
    }

    /// The SpeedHQ quality for `format` and whether the connected receivers
    /// accept the newer SpeedHQ format. Pass `supports_new_format` as `true`
    /// when the encoder can produce it.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime, and
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when `format` is not
    /// a SpeedHQ format the SDK has guidance for.
    pub fn quality_plan(
        &self,
        format: VideoPacketFormat,
        supports_new_format: bool,
    ) -> Result<QualityPlan> {
        let table = self.runtime().table::<QualityPlanning>()?;
        let format = format.raw();
        let mut short_dc = false;
        // SAFETY: the sender instance is live for this borrow and `short_dc`
        // is writable storage for the flag.
        let value: i32 = native!(QualityPlanning, table => unsafe {
            table.NDIlib_send_get_q_factor_v2(
                self.native_instance(),
                &format,
                supports_new_format,
                &mut short_dc,
            )
        });
        let value = u32::try_from(value)
            .map_err(|_| invalid("Quality guidance requires a supported SpeedHQ frame"))?;
        // A value of 100 or more permits the newer format; the quality is then
        // the value less 100.
        let (quality, new_format) = match value.checked_sub(100) {
            Some(quality) => (quality, true),
            None => (value, false),
        };
        Ok(QualityPlan {
            quality,
            new_format,
            short_dc,
        })
    }

    /// Whether a connected receiver needs a keyframe: on the stream `format`
    /// names, or on any stream when `format` is `None`.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime.
    pub fn keyframe_required(&self, format: Option<VideoPacketFormat>) -> Result<bool> {
        let table = self.runtime().table::<KeyframeDemand>()?;
        let format = format.map(VideoPacketFormat::raw);
        // SAFETY: the sender instance is live for this borrow.
        Ok(native!(KeyframeDemand, table => unsafe {
            table.NDIlib_send_is_keyframe_required(self.native_instance(), stream(format.as_ref()))
        }))
    }

    /// Wait up to `timeout` for keyframe demand to change, on the stream
    /// `format` names or on any stream when it is `None`.
    ///
    /// Returns whether a change was signalled; call
    /// [`keyframe_required`](Self::keyframe_required) for the new state.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime, and
    /// [`Error::InvalidConfiguration`](crate::Error::InvalidConfiguration)
    /// when `timeout` exceeds [`crate::MAX_TIMEOUT`].
    pub fn wait_for_keyframe_change(
        &self,
        timeout: Duration,
        format: Option<VideoPacketFormat>,
    ) -> Result<bool> {
        let table = self.runtime().table::<KeyframeDemand>()?;
        let timeout = crate::to_ms_checked(timeout)?;
        let format = format.map(VideoPacketFormat::raw);
        // SAFETY: the sender instance is live for this borrow.
        Ok(native!(KeyframeDemand, table => unsafe {
            table.NDIlib_send_wait_for_keyframe_request(
                self.native_instance(),
                timeout,
                stream(format.as_ref()),
            )
        }))
    }
}

/// SpeedHQ encoder quality for one stream, from [`Sender::quality_plan`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QualityPlan {
    quality: u32,
    new_format: bool,
    short_dc: bool,
}

impl QualityPlan {
    /// The quality factor to encode with.
    pub fn quality(self) -> u32 {
        self.quality
    }

    /// Whether every connected receiver accepts the newer SpeedHQ format.
    pub fn permits_new_format(self) -> bool {
        self.new_format
    }

    /// Whether the encoder should use the shorter DC coefficient coding.
    pub fn enable_short_dc(self) -> bool {
        self.short_dc
    }
}

/// The stream a receiver takes its video from.
///
/// These are the only two choices a receiver can switch between while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoQuality {
    /// The full-quality program stream.
    Program,
    /// The low-bandwidth preview stream.
    Preview,
}

/// Which streams a receiver wants to be sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamSelection {
    video: bool,
    audio: bool,
    metadata: bool,
}

impl StreamSelection {
    /// Select the streams to receive.
    pub fn new(video: bool, audio: bool, metadata: bool) -> Self {
        Self {
            video,
            audio,
            metadata,
        }
    }

    /// Whether video is selected.
    pub fn video(self) -> bool {
        self.video
    }

    /// Whether audio is selected.
    pub fn audio(self) -> bool {
        self.audio
    }

    /// Whether metadata is selected.
    pub fn metadata(self) -> bool {
        self.metadata
    }
}

/// The result of [`Receiver::set_streams`].
///
/// The SDK reports only whether the enabled streams changed, so a request that
/// was already in effect and one the receiver's creation settings forbid look
/// the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamChange {
    /// The enabled streams changed.
    Applied,
    /// Nothing changed: the selection was already in effect, or the receiver
    /// was created without one of the requested streams.
    UnchangedOrRejected,
}

impl<M: ReceiverMode> Receiver<M> {
    /// Ask the source for a keyframe. The request is advisory: the source
    /// decides whether and when to send one.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime.
    pub fn request_keyframe(&self) -> Result<()> {
        let table = self.core._ndi.table::<KeyframeRequest>()?;
        // SAFETY: the receiver instance is live for this borrow.
        native!(KeyframeRequest, table => unsafe { table.NDIlib_recv_request_keyframe(self.core.instance) });
        Ok(())
    }

    /// Switch between the source's program and preview streams.
    ///
    /// Returns whether the SDK applied the change; a receiver created without
    /// video cannot change its video stream. The SDK applies the change only
    /// under a vendor ID that licenses this function, and reports `false`
    /// without one.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime.
    pub fn set_video_quality(&mut self, quality: VideoQuality) -> Result<bool> {
        let table = self.core._ndi.table::<ReceiverBandwidthControl>()?;
        let bandwidth = match quality {
            VideoQuality::Program => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
            VideoQuality::Preview => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
        };
        // SAFETY: the receiver instance is live for this borrow.
        Ok(native!(ReceiverBandwidthControl, table => unsafe {
            table.NDIlib_recv_set_bandwidth(self.core.instance, bandwidth)
        }))
    }

    /// Choose which streams the source sends to this receiver. A stream the
    /// receiver was created without cannot be enabled. The SDK applies the
    /// change only under a vendor ID that licenses this function, and reports
    /// [`StreamChange::UnchangedOrRejected`] without one.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime.
    pub fn set_streams(&mut self, streams: StreamSelection) -> Result<StreamChange> {
        let table = self.core._ndi.table::<ReceiverStreamControl>()?;
        // SAFETY: the receiver instance is live for this borrow.
        let changed = native!(ReceiverStreamControl, table => unsafe {
            table.NDIlib_recv_set_enabled_streams(
                self.core.instance,
                streams.video,
                streams.audio,
                streams.metadata,
            )
        });
        Ok(if changed {
            StreamChange::Applied
        } else {
            StreamChange::UnchangedOrRejected
        })
    }
}
