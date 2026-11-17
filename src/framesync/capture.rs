//! The borrowed clocked capture view, publicly named only by `advanced`.

#[cfg(test)]
use super::audio_test_calls;
use super::{
    input_format, AudioCall, AudioInputFormat, FrameSyncAudioRef, FrameSyncAudioRequest,
    FrameSyncResource, FrameSyncVideoRef,
};
#[cfg(feature = "advanced")]
use crate::frames::{AudioFrame, VideoFrame};
use crate::{frames::ScanType, Result};
use std::fmt;

/// Clocked capture access lent by [`FrameSync::split`](super::FrameSync::split).
///
/// This view borrows only the synchronizer, leaving its receiver available
/// through a separate exclusive control view. A KVM session can therefore hold
/// a button or key while video and audio continue. Creating or copying the
/// view allocates nothing and makes no SDK call. Capture has the same costs,
/// timing, validation and stale-output semantics as [`FrameSync`](super::FrameSync).
///
/// The split view stays on its borrowing thread (`!Send` and `!Sync`) so
/// exclusive stream changes stay sequenced with split capture. Share the
/// complete FrameSync for concurrent capture instead. Borrowed output frames
/// also remain neither `Send` nor `Sync`, retaining the owner's borrow even
/// if the view itself is dropped.
#[derive(Clone, Copy)]
pub struct FrameSyncCapture<'a> {
    resource: &'a FrameSyncResource,
    _thread: std::marker::PhantomData<*mut ()>,
}

impl fmt::Debug for FrameSyncCapture<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSyncCapture")
            .field("mode", &"FrameSyncAttached")
            .finish_non_exhaustive()
    }
}

impl<'a> FrameSyncCapture<'a> {
    pub(super) fn new(resource: &'a FrameSyncResource) -> Self {
        Self {
            resource,
            _thread: std::marker::PhantomData,
        }
    }

    /// Capture an owned clocked video frame; see [`FrameSync::capture_video`](super::FrameSync::capture_video).
    ///
    /// # Errors
    ///
    /// As for [`FrameSync::capture_video_ref`](super::FrameSync::capture_video_ref).
    #[cfg(feature = "advanced")]
    #[doc(alias = "NDIlib_framesync_capture_video")]
    pub fn capture_video(&self, scan_type: ScanType) -> Result<Option<VideoFrame>> {
        Ok(self
            .capture_video_ref(scan_type)?
            .map(|frame| frame.to_owned()))
    }

    /// Borrow clocked video without a wrapper payload copy or allocation.
    /// The output can repeat previous input; see [`FrameSync::capture_video_ref`](super::FrameSync::capture_video_ref).
    ///
    /// # Errors
    ///
    /// As for [`FrameSync::capture_video_ref`](super::FrameSync::capture_video_ref).
    #[doc(alias = "NDIlib_framesync_capture_video")]
    pub fn capture_video_ref(&self, scan_type: ScanType) -> Result<Option<FrameSyncVideoRef<'a>>> {
        let Some(guard) = self.resource.capture_video(scan_type) else {
            return Ok(None);
        };
        Ok(Some(FrameSyncVideoRef::new(guard)?))
    }

    /// Capture owned resampled audio; see [`FrameSync::capture_audio`](super::FrameSync::capture_audio).
    ///
    /// # Errors
    ///
    /// As for [`FrameSync::capture_audio_ref`](super::FrameSync::capture_audio_ref).
    #[cfg(feature = "advanced")]
    #[doc(alias = "NDIlib_framesync_capture_audio_v2")]
    pub fn capture_audio(&self, request: FrameSyncAudioRequest) -> Result<AudioFrame> {
        Ok(self.capture_audio_ref(request)?.to_owned())
    }

    /// Borrow resampled audio without a wrapper payload copy or allocation.
    /// The SDK can return silence; see [`FrameSync::capture_audio_ref`](super::FrameSync::capture_audio_ref).
    ///
    /// # Errors
    ///
    /// As for [`FrameSync::capture_audio_ref`](super::FrameSync::capture_audio_ref).
    #[doc(alias = "NDIlib_framesync_capture_audio_v2")]
    pub fn capture_audio_ref(
        &self,
        request: FrameSyncAudioRequest,
    ) -> Result<FrameSyncAudioRef<'a>> {
        let guard = self.resource.capture_audio(AudioCall::Samples(request));
        FrameSyncAudioRef::new(guard)
    }

    /// Query the cached input audio format; see [`FrameSync::audio_input_format`](super::FrameSync::audio_input_format).
    /// This does not establish fresh reception and allocates nothing on success.
    ///
    /// # Errors
    ///
    /// As for [`FrameSync::audio_input_format`](super::FrameSync::audio_input_format).
    #[doc(alias = "NDIlib_framesync_capture_audio_v2")]
    pub fn audio_input_format(&self) -> Result<Option<AudioInputFormat>> {
        let guard = self.resource.capture_audio(AudioCall::InputFormat);
        input_format(guard.frame())
    }

    /// The approximate queued sample count; see [`FrameSync::audio_queue_depth`](super::FrameSync::audio_queue_depth).
    /// This makes one native query without allocating or waiting for input.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`](crate::Error::UnexpectedSdkResult) when the native queue depth is negative.
    #[doc(alias = "NDIlib_framesync_audio_queue_depth")]
    pub fn audio_queue_depth(&self) -> Result<u32> {
        #[cfg(test)]
        audio_test_calls::query();
        // SAFETY: the view borrows a live synchronizer for the call.
        let depth = unsafe {
            self.resource
                .receiver()
                .sdk()
                .named()
                .NDIlib_framesync_audio_queue_depth(self.resource.instance.0)
        };
        depth.count()
    }
}
