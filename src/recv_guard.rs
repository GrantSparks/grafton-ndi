//! RAII guards for NDI receive frames.
//!
//! This module provides internal RAII guards that ensure captured NDI frames
//! are always freed exactly once via the appropriate `NDIlib_recv_free_*` calls.
//! These guards are private implementation details that prevent frame leaks
//! in the receive path.

use crate::ndi_lib::*;

/// RAII guard for a captured video frame.
///
/// Automatically calls `NDIlib_recv_free_video_v2` when dropped,
/// ensuring the NDI SDK can reclaim the buffer.
pub(crate) struct RecvVideoGuard {
    instance: NDIlib_recv_instance_t,
    frame: NDIlib_video_frame_v2_t,
}

impl RecvVideoGuard {
    /// Create a new video frame guard.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - `instance` is a valid NDI receiver instance
    /// - `frame` was populated by a successful call to `NDIlib_recv_capture_v3`
    ///   that returned `NDIlib_frame_type_video`
    pub(crate) unsafe fn new(
        instance: NDIlib_recv_instance_t,
        frame: NDIlib_video_frame_v2_t,
    ) -> Self {
        Self { instance, frame }
    }

    /// Get a reference to the underlying frame for conversion to owned data.
    pub(crate) fn frame(&self) -> &NDIlib_video_frame_v2_t {
        &self.frame
    }
}

impl Drop for RecvVideoGuard {
    fn drop(&mut self) {
        unsafe {
            NDIlib_recv_free_video_v2(self.instance, &self.frame);
        }
    }
}

/// RAII guard for a captured audio frame.
///
/// Automatically calls `NDIlib_recv_free_audio_v3` when dropped,
/// ensuring the NDI SDK can reclaim the buffer.
pub(crate) struct RecvAudioGuard {
    instance: NDIlib_recv_instance_t,
    frame: NDIlib_audio_frame_v3_t,
}

impl RecvAudioGuard {
    /// Create a new audio frame guard.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - `instance` is a valid NDI receiver instance
    /// - `frame` was populated by a successful call to `NDIlib_recv_capture_v3`
    ///   that returned `NDIlib_frame_type_audio`
    pub(crate) unsafe fn new(
        instance: NDIlib_recv_instance_t,
        frame: NDIlib_audio_frame_v3_t,
    ) -> Self {
        Self { instance, frame }
    }

    /// Get a reference to the underlying frame for conversion to owned data.
    pub(crate) fn frame(&self) -> &NDIlib_audio_frame_v3_t {
        &self.frame
    }
}

impl Drop for RecvAudioGuard {
    fn drop(&mut self) {
        unsafe {
            NDIlib_recv_free_audio_v3(self.instance, &self.frame);
        }
    }
}

/// RAII guard for a captured metadata frame.
///
/// Automatically calls `NDIlib_recv_free_metadata` when dropped,
/// ensuring the NDI SDK can reclaim the buffer.
pub(crate) struct RecvMetadataGuard {
    instance: NDIlib_recv_instance_t,
    frame: NDIlib_metadata_frame_t,
}

impl RecvMetadataGuard {
    /// Create a new metadata frame guard.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - `instance` is a valid NDI receiver instance
    /// - `frame` was populated by a successful call to `NDIlib_recv_capture_v3`
    ///   that returned `NDIlib_frame_type_metadata`
    pub(crate) unsafe fn new(
        instance: NDIlib_recv_instance_t,
        frame: NDIlib_metadata_frame_t,
    ) -> Self {
        Self { instance, frame }
    }

    /// Get a reference to the underlying frame for conversion to owned data.
    pub(crate) fn frame(&self) -> &NDIlib_metadata_frame_t {
        &self.frame
    }
}

impl Drop for RecvMetadataGuard {
    fn drop(&mut self) {
        unsafe {
            NDIlib_recv_free_metadata(self.instance, &self.frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests verify the guards compile and have the correct structure.
    // Runtime tests require an actual NDI receiver instance and are not included here.

    #[test]
    fn test_guard_sizes() {
        // Guards should be small - just the instance pointer and the frame struct
        use std::mem::size_of;

        // The guards should not add significant overhead
        assert!(size_of::<RecvVideoGuard>() > 0);
        assert!(size_of::<RecvAudioGuard>() > 0);
        assert!(size_of::<RecvMetadataGuard>() > 0);
    }
}
