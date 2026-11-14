//! Advanced SDK controls on senders and receivers of every mode.

use std::{marker::PhantomData, time::Duration};

#[cfg(any(ndi_has_advanced_encoder_guidance, ndi_has_advanced_quality_planning))]
use super::{invalid_input, VideoCodec};
use super::{KvmSession, VideoPacketFormat};
#[cfg(ndi_has_advanced_keyframe_demand)]
use crate::sys::bindings::NDIlib_video_frame_v2_t;
#[cfg(ndi_has_advanced_receiver_bandwidth_control)]
use crate::sys::bindings::{
    NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
    NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
};
#[cfg(doc)]
use crate::Error;
use crate::{
    sys::advanced::{
        native, EncoderGuidance, KeyframeDemand, KeyframeRequest, QualityPlanning,
        ReceiverBandwidthControl, ReceiverStreamControl, SenderTally,
    },
    MediaTypes, Receiver, ReceiverMode, Result, Sender, SenderMode, SettingChange, StreamTier,
    Tally,
};

/// Exclusive controls for a receiver held by another resource.
///
/// FrameSync, AVSync and allocator registrations depend on one particular
/// native receiver. Their `controls()` methods lend this view instead of a
/// mutable receiver, so the receiver cannot be replaced while those resources
/// exist. Ordinary shared controls remain available through the owner's
/// `receiver()` method, or [`Self::receiver`].
///
/// The view allocates nothing and stays on its borrowing thread (`!Send` and
/// `!Sync`). This keeps exclusive stream changes sequenced with split capture;
/// the SDK does not document their overlap across application threads. Its
/// exclusive borrow prevents changing streams or starting a second KVM session
/// while a session is live. Use the synchronizer's `split()` method to keep
/// capturing synchronized output during a KVM session.
pub struct ReceiverControls<'a, M: ReceiverMode> {
    receiver: &'a mut Receiver<M>,
    _thread: PhantomData<*mut ()>,
}

impl<'a, M: ReceiverMode> ReceiverControls<'a, M> {
    pub(crate) fn new(receiver: &'a mut Receiver<M>) -> Self {
        Self {
            receiver,
            _thread: PhantomData,
        }
    }

    /// The receiver for shared controls and the capture allowed by its mode.
    /// This borrow never permits replacement or recovery of the receiver.
    pub fn receiver(&self) -> &Receiver<M> {
        self.receiver
    }

    /// Switch between program and preview; see [`Receiver::set_stream_tier`].
    /// This makes the same synchronous native call without allocating.
    ///
    /// # Errors
    ///
    /// As for [`Receiver::set_stream_tier`]. A refused or redundant change
    /// remains [`SettingChange::Unchanged`].
    pub fn set_stream_tier(&mut self, tier: StreamTier) -> Result<SettingChange> {
        self.receiver.set_stream_tier(tier)
    }

    /// Choose the enabled media; see [`Receiver::set_streams`].
    /// This makes the same synchronous native call without allocating. It
    /// does not promise to flush a synchronizer's cached output or format.
    ///
    /// # Errors
    ///
    /// As for [`Receiver::set_streams`]. A refused or redundant change
    /// remains [`SettingChange::Unchanged`].
    pub fn set_streams(&mut self, streams: MediaTypes) -> Result<SettingChange> {
        self.receiver.set_streams(streams)
    }

    /// Start KVM, retaining this view's exclusive receiver borrow.
    ///
    /// This consumes the view, so a temporary `owner.controls()` can create
    /// a session lasting until it is dropped. Session creation allocates
    /// nothing; holding inputs can allocate for tracking. To change source,
    /// successfully release all inputs and end the session first. Its
    /// restricted receiver view cannot change the connection.
    ///
    /// # Errors
    ///
    /// As for [`Receiver::kvm`].
    pub fn kvm(self) -> Result<KvmSession<'a, M>> {
        self.receiver.kvm()
    }
}

/// Encoder guidance that `operation` returned, which `Processing.NDI.Advanced.h`
/// describes as a size, a bit rate or a quality: a negative value breaks that
/// contract.
#[cfg(any(ndi_has_advanced_encoder_guidance, ndi_has_advanced_quality_planning))]
fn guidance(value: i32, operation: &str) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| crate::Error::UnexpectedSdkResult(format!("{operation} returned {value}")))
}

/// Refuse a format the quality factor does not apply to.
///
/// `Processing.NDI.Advanced.h`: "If the frame passed in is not SpeedHQ then
/// this will return a value of -1."
#[cfg(any(ndi_has_advanced_encoder_guidance, ndi_has_advanced_quality_planning))]
fn require_speedhq(format: VideoPacketFormat) -> Result<()> {
    match format.codec() {
        VideoCodec::SpeedHq420 | VideoCodec::SpeedHq422 | VideoCodec::SpeedHq422Alpha => Ok(()),
        codec => Err(invalid_input(format!(
            "SpeedHQ quality guidance does not apply to {codec:?}"
        ))),
    }
}

/// A null descriptor asks about any stream; otherwise the one `format` names.
#[cfg(ndi_has_advanced_keyframe_demand)]
fn stream(format: Option<&NDIlib_video_frame_v2_t>) -> *const NDIlib_video_frame_v2_t {
    format.map_or(std::ptr::null(), |format| format)
}

impl<M: SenderMode> Sender<M> {
    /// Set this sender's own contribution to its tally state, which is
    /// otherwise the combination of what its receivers report: for instance,
    /// that it is known to be on air. The SDK reflects the state to the
    /// sender's receivers.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`]
    /// without an Advanced runtime.
    pub fn set_external_tally(&self, _tally: Tally) -> Result<()> {
        let table = self.runtime().table::<SenderTally>()?;
        native!(SenderTally, table => {
            let tally = _tally.to_sdk();
            // The SDK gives this control flag no failure meaning.
            // SAFETY: the sender instance is live for this borrow.
            unsafe { table.NDIlib_send_set_tally(self.native_instance(), &tally) };
            Ok(())
        })
    }

    /// The compressed frame size, in bytes, an encoder should aim for when
    /// producing `format` for this sender's connections.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] without an Advanced runtime, and
    /// [`Error::UnexpectedSdkResult`] when the SDK returns a negative size.
    pub fn target_frame_bytes(&self, _format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        native!(EncoderGuidance, table => {
            let format = _format.raw();
            // SAFETY: the sender instance is live for this borrow.
            let value = unsafe {
                table.NDIlib_send_get_target_frame_size(self.native_instance(), &format)
            };
            guidance(value, "NDIlib_send_get_target_frame_size")
        })
    }

    /// The bit rate, in bits per second, an encoder should aim for when
    /// producing `format` for this sender's connections.
    ///
    /// # Errors
    ///
    /// As for [`target_frame_bytes`](Self::target_frame_bytes), with a
    /// negative bit rate.
    pub fn target_bit_rate(&self, _format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        native!(EncoderGuidance, table => {
            let format = _format.raw();
            // SAFETY: the sender instance is live for this borrow.
            let value = unsafe {
                table.NDIlib_send_get_target_bit_rate(self.native_instance(), &format)
            };
            guidance(value, "NDIlib_send_get_target_bit_rate")
        })
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
    pub fn legacy_quality(&self, _format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        native!(EncoderGuidance, table => {
            require_speedhq(_format)?;
            let format = _format.raw();
            // SAFETY: the sender instance is live for this borrow.
            let value = unsafe { table.NDIlib_send_get_q_factor(self.native_instance(), &format) };
            guidance(value, "NDIlib_send_get_q_factor")
        })
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
        _format: VideoPacketFormat,
        _supports_new_format: bool,
    ) -> Result<QualityPlan> {
        let table = self.runtime().table::<QualityPlanning>()?;
        native!(QualityPlanning, table => {
            require_speedhq(_format)?;
            let format = _format.raw();
            let mut short_dc = false;
            // SAFETY: the sender instance is live for this borrow and `short_dc`
            // is writable storage for the flag.
            let value = unsafe {
                table.NDIlib_send_get_q_factor_v2(
                    self.native_instance(),
                    &format,
                    _supports_new_format,
                    &mut short_dc,
                )
            };
            let value = guidance(value, "NDIlib_send_get_q_factor_v2")?;
            // A value of 100 or more permits the newer format; the quality is
            // then the value less 100.
            let (quality, new_format) = match value.checked_sub(100) {
                Some(quality) => (quality, true),
                None => (value, false),
            };
            Ok(QualityPlan { quality, new_format, short_dc })
        })
    }

    /// Whether a connected receiver needs a keyframe: on the stream `format`
    /// names, or on any stream when `format` is `None`.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`]
    /// without an Advanced runtime.
    pub fn keyframe_required(&self, _format: Option<VideoPacketFormat>) -> Result<bool> {
        let table = self.runtime().table::<KeyframeDemand>()?;
        native!(KeyframeDemand, table => {
            let format = _format.map(VideoPacketFormat::raw);
            // SAFETY: the sender instance is live for this borrow.
            Ok(unsafe {
                table.NDIlib_send_is_keyframe_required(self.native_instance(), stream(format.as_ref()))
            })
        })
    }

    /// Wait up to `timeout` for keyframe demand to change, on the stream
    /// `format` names or on any stream when it is `None`.
    ///
    /// Returns whether a change was signalled; call
    /// [`keyframe_required`](Self::keyframe_required) for the new state.
    /// Concurrent waiters share one native update report. A format-specific
    /// waiter reports the update when that stream currently needs a keyframe;
    /// otherwise it keeps waiting within the same timeout. These reports are
    /// coalesced invalidations, not a per-stream event history: pending demand
    /// can be reported again, or a concurrent send can clear it before the
    /// query. A zero timeout beside another native waiter only reads reports
    /// that waiter has already published.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`]
    /// without an Advanced runtime, and
    /// [`Error::InvalidInput`]
    /// when `timeout` exceeds [`crate::MAX_TIMEOUT`].
    pub fn wait_for_keyframe_change(
        &self,
        _timeout: Duration,
        _format: Option<VideoPacketFormat>,
    ) -> Result<bool> {
        let table = self.runtime().table::<KeyframeDemand>()?;
        native!(KeyframeDemand, table => {
            let deadline = crate::timeout::Deadline::after(_timeout)?;
            let format = _format.map(VideoPacketFormat::raw);
            let changes = self.keyframe_changes();
            let mut generation = changes.generation();
            loop {
                // SAFETY: the sender lives for this borrow. A null selector
                // observes all updates; separate native waits for overlapping
                // selectors can consume one another's report.
                let changed = changes.wait_since(generation, deadline, |timeout| unsafe {
                    table.NDIlib_send_wait_for_keyframe_request(
                        self.native_instance(), timeout, std::ptr::null(),
                    )
                });
                if !changed { return Ok(false); }
                // Observe before filtering so a later report cannot fall into
                // a gap between the demand query and the next wait.
                generation = changes.generation();
                // SAFETY: this query does not consume an update. The optional
                // descriptor lives through the synchronous call.
                if format.is_none() || unsafe {
                    table.NDIlib_send_is_keyframe_required(self.native_instance(), stream(format.as_ref()))
                } {
                    return Ok(true);
                }
                if deadline.is_expired() { return Ok(false); }
            }
        })
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
        native!(KeyframeRequest, table => {
            // SAFETY: the receiver instance is live for this borrow.
            unsafe { table.NDIlib_recv_request_keyframe(self.core.instance) };
            Ok(())
        })
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
    pub fn set_stream_tier(&mut self, _tier: StreamTier) -> Result<SettingChange> {
        let table = self.core._ndi.table::<ReceiverBandwidthControl>()?;
        native!(ReceiverBandwidthControl, table => {
            let bandwidth = match _tier {
                StreamTier::Program => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
                StreamTier::Preview => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
            };
            // SAFETY: the receiver instance is live for this borrow.
            let switched = unsafe { table.NDIlib_recv_set_bandwidth(self.core.instance, bandwidth) };
            Ok(SettingChange::from_sdk(switched))
        })
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
    pub fn set_streams(&mut self, _streams: MediaTypes) -> Result<SettingChange> {
        let table = self.core._ndi.table::<ReceiverStreamControl>()?;
        native!(ReceiverStreamControl, table => {
            // SAFETY: the receiver instance is live for this borrow.
            let changed = unsafe {
                table.NDIlib_recv_set_enabled_streams(
                    self.core.instance,
                    _streams.contains(MediaTypes::VIDEO),
                    _streams.contains(MediaTypes::AUDIO),
                    _streams.contains(MediaTypes::METADATA),
                )
            };
            Ok(SettingChange::from_sdk(changed))
        })
    }
}
