//! Advanced SDK controls on the ordinary sender and receiver.

use std::time::Duration;

use super::{invalid_input, VideoCodec, VideoPacketFormat};
use crate::{
    sys::{
        advanced::{
            native, EncoderGuidance, KeyframeDemand, KeyframeRequest, QualityPlanning,
            ReceiverBandwidthControl, ReceiverStreamControl, SenderTally,
        },
        bindings::*,
    },
    Error, MediaTypes, Receiver, ReceiverMode, Result, Sender, SettingChange, StreamTier, Tally,
};

/// Encoder guidance that `operation` returned, which `Processing.NDI.Advanced.h`
/// describes as a size, a bit rate or a quality: a negative value breaks that
/// contract.
fn guidance(value: i32, operation: &str) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| Error::UnexpectedSdkResult(format!("{operation} returned {value}")))
}

/// Refuse a format the quality factor does not apply to.
///
/// `Processing.NDI.Advanced.h`: "If the frame passed in is not SpeedHQ then
/// this will return a value of -1."
fn require_speedhq(format: VideoPacketFormat) -> Result<()> {
    match format.codec() {
        VideoCodec::SpeedHq420 | VideoCodec::SpeedHq422 | VideoCodec::SpeedHq422Alpha => Ok(()),
        codec => Err(invalid_input(format!(
            "SpeedHQ quality guidance does not apply to {codec:?}"
        ))),
    }
}

/// A null descriptor asks about any stream; otherwise the one `format` names.
fn stream(format: Option<&NDIlib_video_frame_v2_t>) -> *const NDIlib_video_frame_v2_t {
    format.map_or(std::ptr::null(), |format| format)
}

impl Sender {
    /// Set this sender's own contribution to its tally state, which is
    /// otherwise the combination of what its receivers report: for instance,
    /// that it is known to be on air. The SDK reflects the state to the
    /// sender's receivers.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`]
    /// without an Advanced runtime.
    pub fn set_external_tally(&self, tally: Tally) -> Result<()> {
        let table = self.runtime().table::<SenderTally>()?;
        let tally = tally.to_sdk();
        // `Processing.NDI.Advanced.h` describes this as control of the tally
        // state, reflected downstream, and gives its flag no meaning, so it
        // reports no failure.
        // SAFETY: the sender instance is live for this borrow.
        native!(SenderTally, table => unsafe {
            table.NDIlib_send_set_tally(self.native_instance(), &tally)
        });
        Ok(())
    }

    /// The compressed frame size, in bytes, an encoder should aim for when
    /// producing `format` for this sender's connections.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] without an Advanced runtime, and
    /// [`Error::UnexpectedSdkResult`] when the SDK returns a negative size.
    pub fn target_frame_bytes(&self, format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        let format = format.raw();
        // SAFETY: the sender instance is live for this borrow.
        let value = native!(EncoderGuidance, table => unsafe {
            table.NDIlib_send_get_target_frame_size(self.native_instance(), &format)
        });
        guidance(value, "NDIlib_send_get_target_frame_size")
    }

    /// The bit rate, in bits per second, an encoder should aim for when
    /// producing `format` for this sender's connections.
    ///
    /// # Errors
    ///
    /// As for [`target_frame_bytes`](Self::target_frame_bytes), with a
    /// negative bit rate.
    pub fn target_bit_rate(&self, format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        let format = format.raw();
        // SAFETY: the sender instance is live for this borrow.
        let value = native!(EncoderGuidance, table => unsafe {
            table.NDIlib_send_get_target_bit_rate(self.native_instance(), &format)
        });
        guidance(value, "NDIlib_send_get_target_bit_rate")
    }

    /// The SpeedHQ quality factor for `format`, as the original quality query
    /// reports it. It says nothing about whether receivers accept the newer
    /// SpeedHQ format; [`quality_plan`](Self::quality_plan) does.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] without an Advanced runtime,
    /// [`Error::InvalidInput`] when `format` is not SpeedHQ, and
    /// [`Error::UnexpectedSdkResult`] when the SDK returns a negative quality
    /// for one that is.
    pub fn legacy_quality(&self, format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        require_speedhq(format)?;
        let format = format.raw();
        // SAFETY: the sender instance is live for this borrow.
        let value = native!(EncoderGuidance, table => unsafe {
            table.NDIlib_send_get_q_factor(self.native_instance(), &format)
        });
        guidance(value, "NDIlib_send_get_q_factor")
    }

    /// The SpeedHQ quality for `format` and whether the connected receivers
    /// accept the newer SpeedHQ format. Pass `supports_new_format` as `true`
    /// when the encoder can produce it.
    ///
    /// # Errors
    ///
    /// As for [`legacy_quality`](Self::legacy_quality).
    pub fn quality_plan(
        &self,
        format: VideoPacketFormat,
        supports_new_format: bool,
    ) -> Result<QualityPlan> {
        let table = self.runtime().table::<QualityPlanning>()?;
        require_speedhq(format)?;
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
        let value = guidance(value, "NDIlib_send_get_q_factor_v2")?;
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
    /// [`Error::UnsupportedCapability`]
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
    /// [`Error::UnsupportedCapability`]
    /// without an Advanced runtime, and
    /// [`Error::InvalidInput`]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

impl<M: ReceiverMode> Receiver<M> {
    /// Ask the source for a keyframe. The request is advisory: the source
    /// decides whether and when to send one.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`]
    /// without an Advanced runtime.
    pub fn request_keyframe(&self) -> Result<()> {
        let table = self.core._ndi.table::<KeyframeRequest>()?;
        // SAFETY: the receiver instance is live for this borrow.
        native!(KeyframeRequest, table => unsafe { table.NDIlib_recv_request_keyframe(self.core.instance) });
        Ok(())
    }

    /// Switch between the source's program and preview streams.
    ///
    /// [`SettingChange::Unchanged`] when the SDK did not switch: the tier was
    /// already in effect, the receiver was created without video, or the
    /// runtime is not licensed to switch. The SDK switches only under a vendor
    /// ID that licenses this function.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`]
    /// without an Advanced runtime.
    pub fn set_stream_tier(&mut self, tier: StreamTier) -> Result<SettingChange> {
        let table = self.core._ndi.table::<ReceiverBandwidthControl>()?;
        let bandwidth = match tier {
            StreamTier::Program => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
            StreamTier::Preview => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
        };
        // SAFETY: the receiver instance is live for this borrow.
        let switched = native!(ReceiverBandwidthControl, table => unsafe {
            table.NDIlib_recv_set_bandwidth(self.core.instance, bandwidth)
        });
        Ok(SettingChange::from_sdk(switched))
    }

    /// Choose which streams the source sends to this receiver.
    ///
    /// [`SettingChange::Unchanged`] when the enabled streams did not change:
    /// they were already the ones requested, the request asked for a stream
    /// the receiver was created without, or the runtime is not licensed to
    /// change them. The SDK applies the change only under a vendor ID that
    /// licenses this function.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`]
    /// without an Advanced runtime.
    pub fn set_streams(&mut self, streams: MediaTypes) -> Result<SettingChange> {
        let table = self.core._ndi.table::<ReceiverStreamControl>()?;
        // SAFETY: the receiver instance is live for this borrow.
        let changed = native!(ReceiverStreamControl, table => unsafe {
            table.NDIlib_recv_set_enabled_streams(
                self.core.instance,
                streams.contains(MediaTypes::VIDEO),
                streams.contains(MediaTypes::AUDIO),
                streams.contains(MediaTypes::METADATA),
            )
        });
        Ok(SettingChange::from_sdk(changed))
    }
}
