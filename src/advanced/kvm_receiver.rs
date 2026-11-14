//! Shared receiver operations compatible with a live KVM session.

use std::time::Duration;

use crate::{
    AudioKind, Capture, CaptureMode, ConnectionStats, EventMode, MetadataKind, MetadataMode,
    MetadataSource, Ptz, RawCapture, Receiver, ReceiverEvent, ReceiverItem, ReceiverItemRef,
    ReceiverMode, Result, SettingChange, Source, SourceNameChange, Tally, VideoKind, VideoMode,
};

/// Restricted receiver access lent by [`super::KvmSession::receiver`].
///
/// Capture and event polling follow separate sealed mode bounds. FrameSync and
/// AV sync views permit metadata capture but no event polling. Shared controls delegate
/// to the same receiver, but this view cannot connect, disconnect, reconnect,
/// or recover unrestricted receiver access. To switch sources, successfully
/// release all input, end the session, change the owner's connection and start
/// a new session. See [`super::KvmSession`] for best-effort release limitations.
///
/// This reference-sized view allocates nothing and makes no SDK calls or lock
/// acquisitions when constructed. Capture handles, borrowed frames and PTZ
/// controls retain the original receiver borrow, so they can coexist with
/// mutable session input calls. Like the receiver, the view is `Send` and `Sync`.
pub struct KvmReceiver<'a, M: ReceiverMode = RawCapture> {
    receiver: &'a Receiver<M>,
}

impl<'a, M: ReceiverMode> KvmReceiver<'a, M> {
    pub(super) fn new(receiver: &'a Receiver<M>) -> Self {
        Self { receiver }
    }

    /// Set video buffering; see [`Receiver::set_video_delay`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::set_video_delay`].
    pub fn set_video_delay(&self, delay: Duration) -> Result<SettingChange> {
        self.receiver.set_video_delay(delay)
    }

    /// Query video buffering; see [`Receiver::video_delay`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::video_delay`].
    pub fn video_delay(&self) -> Result<Duration> {
        self.receiver.video_delay()
    }

    /// Set audio buffering; see [`Receiver::set_audio_delay`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::set_audio_delay`].
    pub fn set_audio_delay(&self, delay: Duration) -> Result<SettingChange> {
        self.receiver.set_audio_delay(delay)
    }

    /// Query audio buffering; see [`Receiver::audio_delay`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::audio_delay`].
    pub fn audio_delay(&self) -> Result<Duration> {
        self.receiver.audio_delay()
    }

    /// Register connection metadata; see [`Receiver::add_connection_metadata`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::add_connection_metadata`].
    pub fn add_connection_metadata<S: MetadataSource + ?Sized>(&self, metadata: &S) -> Result<()> {
        self.receiver.add_connection_metadata(metadata)
    }

    /// Clear registered metadata; see [`Receiver::clear_connection_metadata`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::clear_connection_metadata`].
    pub fn clear_connection_metadata(&self) -> Result<()> {
        self.receiver.clear_connection_metadata()
    }

    /// Copy the device web-control URL; see [`Receiver::web_control_url`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::web_control_url`].
    pub fn web_control_url(&self) -> Result<Option<String>> {
        self.receiver.web_control_url()
    }

    /// Query source-name changes; see [`Receiver::poll_source_name_change`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::poll_source_name_change`].
    pub fn poll_source_name_change(&self, timeout: Duration) -> Result<Option<SourceNameChange>> {
        self.receiver.poll_source_name_change(timeout)
    }

    /// Set upstream tally; see [`Receiver::set_tally`].
    pub fn set_tally(&self, tally: Tally) {
        self.receiver.set_tally(tally)
    }

    /// Send upstream metadata; see [`Receiver::send_metadata`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::send_metadata`].
    pub fn send_metadata<S: MetadataSource + ?Sized>(&self, metadata: &S) -> Result<()> {
        self.receiver.send_metadata(metadata)
    }

    /// Borrow PTZ controls; see [`Receiver::ptz`].
    pub fn ptz(&self) -> Ptz<'a> {
        self.receiver.ptz()
    }

    /// Query connection status; see [`Receiver::is_connected`].
    pub fn is_connected(&self) -> bool {
        self.receiver.is_connected()
    }

    /// Copy the last requested target; see [`Receiver::connection_target`].
    pub fn connection_target(&self) -> Source {
        self.receiver.connection_target()
    }

    /// Query reception statistics; see [`Receiver::connection_stats`].
    pub fn connection_stats(&self) -> ConnectionStats {
        self.receiver.connection_stats()
    }

    /// Request a keyframe; see [`Receiver::request_keyframe`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::request_keyframe`].
    pub fn request_keyframe(&self) -> Result<()> {
        self.receiver.request_keyframe()
    }
}

impl<'a, M: VideoMode> KvmReceiver<'a, M> {
    /// Borrow typed video capture; see [`Receiver::video`] and [`Capture`].
    #[must_use = "the Capture view does nothing until a capture verb is called"]
    pub fn video(&self) -> Capture<'a, VideoKind> {
        self.receiver.video()
    }
}

impl<'a, M: CaptureMode> KvmReceiver<'a, M> {
    /// Borrow typed audio capture; see [`Receiver::audio`] and [`Capture`].
    #[must_use = "the Capture view does nothing until a capture verb is called"]
    pub fn audio(&self) -> Capture<'a, AudioKind> {
        self.receiver.audio()
    }
}

impl<'a, M: MetadataMode> KvmReceiver<'a, M> {
    /// Borrow typed metadata capture; see [`Receiver::metadata`] and [`Capture`].
    #[must_use = "the Capture view does nothing until a capture verb is called"]
    pub fn metadata(&self) -> Capture<'a, MetadataKind> {
        self.receiver.metadata()
    }
}

impl<'a, M: EventMode> KvmReceiver<'a, M> {
    /// Poll receiver events; see [`Receiver::poll_event`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::poll_event`].
    pub fn poll_event(&self, timeout: Duration) -> Result<Option<ReceiverEvent>> {
        self.receiver.poll_event(timeout)
    }
}

impl<'a> KvmReceiver<'a> {
    /// Borrow mixed media or events; see [`Receiver::capture_any_ref`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::capture_any_ref`].
    pub fn capture_any_ref(&self, timeout: Duration) -> Result<Option<ReceiverItemRef<'a>>> {
        self.receiver.capture_any_ref(timeout)
    }

    /// Copy mixed media or events; see [`Receiver::capture_any`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::capture_any`].
    pub fn capture_any(&self, timeout: Duration) -> Result<Option<ReceiverItem>> {
        self.receiver.capture_any(timeout)
    }
}
