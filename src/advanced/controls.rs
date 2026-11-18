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
/// FrameSync and AVSync depend on one particular native receiver. Their
/// `controls()` methods lend this view instead of a mutable receiver, so it
/// cannot be replaced while those resources exist. Allocator owners lend this
/// view too; their registrations independently retain the original receiver core.
/// Ordinary shared controls remain available through the owner's
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
    #[doc(alias = "NDIlib_recv_set_bandwidth")]
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
    #[doc(alias = "NDIlib_recv_set_enabled_streams")]
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
fn guidance(called: crate::sys::table::Called<i32>) -> Result<u32> {
    let (value, operation) = called.into_parts();
    u32::try_from(value).map_err(|_| {
        crate::Error::UnexpectedSdkResult(crate::SdkResultDefect::InvalidGuidance {
            operation,
            value,
        })
    })
}

/// Refuse a format the quality factor does not apply to.
///
/// `Processing.NDI.Advanced.h`: "If the frame passed in is not SpeedHQ then
/// this will return a value of -1."
#[cfg(any(ndi_has_advanced_encoder_guidance, ndi_has_advanced_quality_planning))]
fn require_speedhq(format: VideoPacketFormat) -> Result<()> {
    match format.codec() {
        VideoCodec::SpeedHq420 | VideoCodec::SpeedHq422 | VideoCodec::SpeedHq422Alpha => Ok(()),
        codec => Err(invalid_input(crate::InputDefect::SpeedHqCodec { codec })),
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
    #[doc(alias = "NDIlib_send_set_tally")]
    pub fn set_external_tally(&self, tally: Tally) -> Result<()> {
        let table = self.runtime().table::<SenderTally>()?;
        native!(SenderTally, table, uses(tally) => {
            let tally = tally.to_sdk();
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
    #[doc(alias = "NDIlib_send_get_target_frame_size")]
    pub fn target_frame_bytes(&self, format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        native!(EncoderGuidance, table, uses(format) => {
            let format = format.raw();
            // SAFETY: the sender instance is live for this borrow.
            let value = unsafe {
                table.named().NDIlib_send_get_target_frame_size(self.native_instance(), &format)
            };
            guidance(value)
        })
    }

    /// The bit rate, in bits per second, an encoder should aim for when
    /// producing `format` for this sender's connections.
    ///
    /// # Errors
    ///
    /// As for [`target_frame_bytes`](Self::target_frame_bytes), with a
    /// negative bit rate.
    #[doc(alias = "NDIlib_send_get_target_bit_rate")]
    pub fn target_bit_rate(&self, format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        native!(EncoderGuidance, table, uses(format) => {
            let format = format.raw();
            // SAFETY: the sender instance is live for this borrow.
            let value = unsafe {
                table.named().NDIlib_send_get_target_bit_rate(self.native_instance(), &format)
            };
            guidance(value)
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
    #[doc(alias = "NDIlib_send_get_q_factor")]
    pub fn legacy_quality(&self, format: VideoPacketFormat) -> Result<u32> {
        let table = self.runtime().table::<EncoderGuidance>()?;
        native!(EncoderGuidance, table, uses(format) => {
            require_speedhq(format)?;
            let format = format.raw();
            // SAFETY: the sender instance is live for this borrow.
            let value = unsafe { table.named().NDIlib_send_get_q_factor(self.native_instance(), &format) };
            guidance(value)
        })
    }

    /// The SpeedHQ quality for `format` and whether the connected receivers
    /// accept the newer SpeedHQ format. Pass `supports_new_format` as `true`
    /// when the encoder can produce it.
    ///
    /// # Errors
    ///
    /// As for [`legacy_quality`](Self::legacy_quality).
    #[doc(alias = "NDIlib_send_get_q_factor_v2")]
    pub fn quality_plan(
        &self,
        format: VideoPacketFormat,
        supports_new_format: bool,
    ) -> Result<QualityPlan> {
        let table = self.runtime().table::<QualityPlanning>()?;
        native!(QualityPlanning, table, uses(format, supports_new_format) => {
            require_speedhq(format)?;
            let format = format.raw();
            let mut short_dc = false;
            // SAFETY: the sender instance is live for this borrow and `short_dc`
            // is writable storage for the flag.
            let value = unsafe {
                table.named().NDIlib_send_get_q_factor_v2(
                    self.native_instance(),
                    &format,
                    supports_new_format,
                    &mut short_dc,
                )
            };
            let value = guidance(value)?;
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
    #[doc(alias = "NDIlib_send_is_keyframe_required")]
    pub fn keyframe_required(&self, format: Option<VideoPacketFormat>) -> Result<bool> {
        let table = self.runtime().table::<KeyframeDemand>()?;
        native!(KeyframeDemand, table, uses(format) => {
            let format = format.map(VideoPacketFormat::raw);
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
    /// waiter reports the update when the shared sample observes demand for
    /// that stream; otherwise it keeps waiting within the same timeout. Demand is sampled
    /// once per active stream selector before any observer is woken, so a send
    /// made by the first observer cannot erase another observer's report.
    /// These are coalesced invalidations, not a per-stream event history:
    /// pending demand can be reported again, or a concurrent send can clear it
    /// before the shared sample. A zero timeout beside another native waiter
    /// only reads reports that waiter has already published.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`]
    /// without an Advanced runtime, and
    /// [`Error::InvalidInput`]
    /// when `timeout` exceeds [`crate::MAX_TIMEOUT`].
    #[doc(alias = "NDIlib_send_wait_for_keyframe_request")]
    pub fn wait_for_keyframe_change(
        &self,
        format: Option<VideoPacketFormat>,
        timeout: Duration,
    ) -> Result<bool> {
        let table = self.runtime().table::<KeyframeDemand>()?;
        native!(KeyframeDemand, table, uses(timeout, format) => {
            let deadline = crate::timeout::Deadline::after(timeout)?;
            Ok(self.keyframe_changes().wait(
                format,
                deadline,
                |timeout| {
                    // SAFETY: the sender lives for the wait. A null selector
                    // observes all updates without competing native selectors.
                    unsafe { table.NDIlib_send_wait_for_keyframe_request(
                        self.native_instance(), timeout, std::ptr::null(),
                    ) }
                },
                |format| {
                    let format = format.raw();
                    // SAFETY: this non-consuming query reads only the live
                    // sender and the descriptor, which lasts through the call.
                    unsafe { table.NDIlib_send_is_keyframe_required(
                        self.native_instance(), &format,
                    ) }
                },
            ))
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
    #[doc(alias = "NDIlib_recv_request_keyframe")]
    pub fn request_keyframe(&self) -> Result<()> {
        let table = self.core.ndi().table::<KeyframeRequest>()?;
        native!(KeyframeRequest, table => {
            self.core.lock_mutating_control()?.request_keyframe(table);
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
    #[doc(alias = "NDIlib_recv_set_bandwidth")]
    pub fn set_stream_tier(&mut self, tier: StreamTier) -> Result<SettingChange> {
        let exclusive = self.exclusive();
        let table = exclusive.ndi().table::<ReceiverBandwidthControl>()?;
        native!(ReceiverBandwidthControl, table, uses(tier) => {
            let bandwidth = match tier {
                StreamTier::Program => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
                StreamTier::Preview => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
            };
            // SAFETY: exclusive receiver authority excludes advertiser changes.
            let switched = unsafe { table.NDIlib_recv_set_bandwidth(exclusive.instance(), bandwidth) };
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
    #[doc(alias = "NDIlib_recv_set_enabled_streams")]
    pub fn set_streams(&mut self, streams: MediaTypes) -> Result<SettingChange> {
        let exclusive = self.exclusive();
        let table = exclusive.ndi().table::<ReceiverStreamControl>()?;
        native!(ReceiverStreamControl, table, uses(streams) => {
            // SAFETY: exclusive receiver authority excludes advertiser changes.
            let changed = unsafe {
                table.NDIlib_recv_set_enabled_streams(
                    exclusive.instance(),
                    streams.contains(MediaTypes::VIDEO),
                    streams.contains(MediaTypes::AUDIO),
                    streams.contains(MediaTypes::METADATA),
                )
            };
            Ok(SettingChange::from_sdk(changed))
        })
    }
}

impl<M: ReceiverMode> std::fmt::Debug for ReceiverControls<'_, M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiverControls")
            .field("receiver", self.receiver())
            .finish_non_exhaustive()
    }
}
