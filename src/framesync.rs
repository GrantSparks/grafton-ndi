//! Frame synchronization for clock-corrected video/audio capture.
//!
//! The [`FrameSync`] type provides a "pull" interface for receiving NDI streams with
//! automatic time-base correction. This transforms the NDI SDK's push-based capture
//! model into a pull model suitable for:
//!
//! - Video playback synced to GPU v-sync
//! - Audio playback synced to sound card clock
//! - Multi-source mixing with a common output clock
//!
//! # When to Use FrameSync vs Raw Capture
//!
//! | Use Case | Raw Receiver | FrameSync |
//! |----------|-------------|-----------|
//! | Recording (preserving timing) | ✓ | |
//! | Playback to GPU | | ✓ |
//! | Playback to sound card | | ✓ |
//! | Multi-source mixing | | ✓ |
//! | Analysis/processing pipeline | ✓ | |
//! | Low-latency monitoring | | ✓ |
//!
//! # Why FrameSync Exists
//!
//! Computer clocks drift. The NDI SDK documentation explains:
//!
//! > Computer clocks rely on crystals which while all rated for the same frequency
//! > are still not exact. If your sending computer has an audio clock that it "thinks"
//! > is 48000Hz, to the receiver computer that has a different audio clock this might
//! > be 48001Hz or 47998Hz.
//!
//! Without time-base correction, this causes:
//! - **Video jitter**: When sender/receiver clocks are nearly aligned, naive frame
//!   buffering causes visible jitter as frames occasionally repeat or skip.
//! - **Audio drift**: Accumulated clock difference causes audio to fall out of sync
//!   or cause glitches as the receiver runs out of or accumulates too many samples.
//!
//! FrameSync solves these by:
//! - Using hysteresis-based video timing to determine optimal frame repeat/skip points
//! - Dynamically resampling audio with high-order filters to track clock differences
//!
//! # Example
//!
//! ```no_run
//! use grafton_ndi::{
//!     NDI, Finder, FinderOptions, ReceiverOptions, Receiver, FrameSync,
//!     FrameSyncAudioRequest, ScanType,
//! };
//! use std::time::Duration;
//!
//! fn main() -> Result<(), grafton_ndi::Error> {
//!     let ndi = NDI::new()?;
//!     let finder = Finder::new(&ndi, &FinderOptions::default())?;
//!     finder.wait_for_sources(Duration::from_secs(1))?;
//!     let sources = finder.current_sources()?;
//!
//!     let options = ReceiverOptions::builder(sources[0].clone()).build();
//!     let receiver = Receiver::new(&ndi, &options)?;
//!
//!     // Create frame-sync from receiver
//!     let framesync = FrameSync::new(receiver)?;
//!
//!     // Capture video synced to your output timing
//!     if let Some(frame) = framesync.capture_video(ScanType::Progressive)? {
//!         println!("{}x{} frame", frame.width(), frame.height());
//!     }
//!
//!     // Capture audio at your sound card's rate
//!     let request = FrameSyncAudioRequest::capture_with(Some(48_000), 2, 1_024)?;
//!     let audio = framesync.capture_audio(request)?;
//!     println!("{} audio samples", audio.num_samples());
//!
//!     Ok(())
//! }
//! ```

use std::{fmt, mem::ManuallyDrop, ptr};

use crate::{
    capture::{FrameSyncAudioFree, FrameSyncVideoFree, Guard},
    frames::{
        validate_packed_audio_dimensions, AudioFrame, AudioRef, ScanType, VideoFrame, VideoRef,
    },
    ndi_lib::*,
    receiver::Receiver,
    Error, Result,
};

/// A zero-copy borrowed video frame from a [`FrameSync`] capture.
///
/// This is the FrameSync spelling of the generic
/// [`VideoRef`]; the [`Receiver`]
/// spelling is [`VideoFrameRef`](crate::VideoFrameRef). Both share one accessor
/// and `Debug` implementation. The frame is automatically freed when dropped via
/// `NDIlib_framesync_free_video`.
pub type FrameSyncVideoRef<'fs> = VideoRef<'fs, FrameSyncVideoFree>;

/// A zero-copy borrowed audio frame from a [`FrameSync`] capture.
///
/// This is the FrameSync spelling of the generic
/// [`AudioRef`]; the [`Receiver`]
/// spelling is [`AudioFrameRef`](crate::AudioFrameRef). The FrameSync path can
/// produce a validated empty query/no-source state, so
/// [`is_empty`](AudioRef::is_empty) and an `Option`-returning
/// [`format`](AudioRef::format)/[`to_owned`](AudioRef::to_owned) are available.
/// The frame is automatically freed when dropped via
/// `NDIlib_framesync_free_audio_v2`.
pub type FrameSyncAudioRef<'fs> = AudioRef<'fs, FrameSyncAudioFree>;

/// A validated, reusable audio operation for [`FrameSync::capture_audio`].
///
/// [`capture`](Self::capture) and [`capture_with`](Self::capture_with) require
/// explicit positive output channels and samples per channel. Construction checks
/// the SDK's signed byte stride and the 64 MiB packed sample payload limit before
/// any native work. This bounds requested samples, not SDK internal memory use.
/// Successful construction and request reuse allocate nothing in the wrapper;
/// submitting a request performs no dimension checks, locks, or extra SDK queries.
/// Returned descriptors and metadata are still validated after each capture.
///
/// [`query_input`](Self::query_input) queries the current input format without
/// requesting samples. To follow source channels, query first, wait for a positive
/// channel count, then build a capture request with that concrete count. Re-query
/// and rebuild when the application chooses to follow format changes. The SDK
/// converts to the explicit output count even if the source changes meanwhile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSyncAudioRequest {
    sample_rate: i32,
    channels: i32,
    samples: i32,
}

impl FrameSyncAudioRequest {
    /// Query the current incoming audio format without requesting sample data.
    /// The result has zero sample rate and channels if no input format is known.
    pub const fn query_input() -> Self {
        Self {
            sample_rate: 0,
            channels: 0,
            samples: 0,
        }
    }

    /// Capture samples using the source sample rate and explicit output channels.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] for nonpositive dimensions, an
    /// unrepresentable signed byte stride, arithmetic overflow, or a sample
    /// payload exceeding 64 MiB. Validation does not allocate sample storage.
    pub fn capture(channels: i32, samples: i32) -> Result<Self> {
        Self::capture_with(None, channels, samples)
    }

    /// Capture samples with an optional source-derived sample rate.
    ///
    /// `None` uses the current source rate. `Some(rate)` must be positive.
    /// Channels and samples per channel must always be explicitly positive.
    /// A request can be copied and reused across borrowed and owned captures.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] for a nonpositive explicit sample rate or
    /// invalid dimensions as described in [`capture`](Self::capture).
    pub fn capture_with(sample_rate: Option<i32>, channels: i32, samples: i32) -> Result<Self> {
        if let Some(rate) = sample_rate {
            if rate <= 0 {
                return Err(Error::InvalidFrame(format!("Invalid sample rate: {rate}")));
            }
        }
        validate_packed_audio_dimensions(channels, samples)?;
        Ok(Self {
            sample_rate: sample_rate.unwrap_or(0),
            channels,
            samples,
        })
    }
}

/// Frame synchronizer for clock-corrected capture.
///
/// Converts push-based NDI streams into pull-based capture with automatic
/// time-base correction and dynamic audio resampling.
///
/// # Ownership
///
/// `FrameSync` takes ownership of the [`Receiver`] (similar to how `BufWriter`
/// wraps a `Write`). Use [`receiver()`](Self::receiver) to access the underlying
/// receiver for tally, PTZ, or status operations. Use
/// [`into_receiver()`](Self::into_receiver) to recover the receiver when done.
///
/// # Thread Safety
///
/// `FrameSync` is `Send + Sync` like `Receiver`, as the underlying NDI SDK
/// frame-sync functions are thread-safe. Frames returned by capture methods
/// borrow from the FrameSync and are neither `Send` nor `Sync`. Convert them
/// to owned frames before transferring them to another thread.
///
/// Raw video capture through the underlying receiver is disabled while this
/// FrameSync exists. [`Self::into_receiver`] destroys the synchronizer and
/// restores raw video capture before returning the receiver.
///
/// # Example
///
/// ```no_run
/// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, FrameSync, FrameSyncAudioRequest, Source, SourceAddress, ScanType};
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = NDI::new()?;
/// # let source = Source { name: "Test".into(), address: SourceAddress::None };
/// # let options = ReceiverOptions::builder(source).build();
/// # let receiver = Receiver::new(&ndi, &options)?;
/// let framesync = FrameSync::new(receiver)?;
///
/// // Sample available data without waiting for a new network frame
/// if let Some(video) = framesync.capture_video(ScanType::Progressive)? {
///     println!("Video: {}x{}", video.width(), video.height());
/// }
///
/// // Audio capture fixes the output dimensions before entering the SDK.
/// let audio = framesync.capture_audio(FrameSyncAudioRequest::capture_with(
///     Some(48_000), 2, 1_024,
/// )?)?;
/// println!("Audio: {} samples", audio.num_samples());
/// # Ok(())
/// # }
/// ```
pub struct FrameSync {
    instance: NDIlib_framesync_instance_t,
    receiver: Receiver,
}

impl FrameSync {
    /// Create a frame synchronizer from a receiver.
    ///
    /// Takes ownership of the receiver. Use [`receiver()`](Self::receiver)
    /// to access the underlying receiver for tally, PTZ, or status operations.
    /// Use [`into_receiver()`](Self::into_receiver) to recover the receiver.
    ///
    /// # Errors
    ///
    /// Returns an error if the NDI SDK fails to create the frame-sync instance.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, FrameSync, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let options = ReceiverOptions::builder(source).build();
    /// let receiver = Receiver::new(&ndi, &options)?;
    /// let framesync = FrameSync::new(receiver)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(receiver: Receiver) -> Result<Self> {
        let instance = unsafe { NDIlib_framesync_create(receiver.instance) };

        if instance.is_null() {
            return Err(Error::InitializationFailed(
                "Failed to create NDI framesync instance".into(),
            ));
        }

        receiver.set_framesync_attached(true);
        Ok(Self { instance, receiver })
    }

    /// Access the underlying receiver.
    ///
    /// Use this to access receiver functionality like tally, PTZ, or status
    /// queries while the FrameSync is active. Tally control publishes state
    /// upstream; it does not query incoming tally. Raw video capture on this receiver
    /// returns [`Error::InvalidConfiguration`]; use FrameSync capture methods or
    /// recover the receiver with [`Self::into_receiver`] first.
    pub fn receiver(&self) -> &Receiver {
        &self.receiver
    }

    /// Consume the FrameSync and recover the underlying Receiver.
    ///
    /// This destroys the frame synchronizer and returns the receiver for
    /// continued use with raw capture or for creating a new FrameSync.
    pub fn into_receiver(self) -> Receiver {
        // Destroy the framesync instance first, then extract the receiver
        // without running Drop (which would double-destroy the framesync).
        let this = ManuallyDrop::new(self);
        unsafe {
            NDIlib_framesync_destroy(this.instance);
        }
        this.receiver.set_framesync_attached(false);
        // SAFETY: We will not use `this` again after reading the receiver field,
        // and ManuallyDrop prevents the Drop impl from running.
        unsafe { ptr::read(&this.receiver) }
    }

    /// Capture video with time-base correction.
    ///
    /// This does not wait for a new network frame. It returns the best frame for
    /// the current output timing, handling clock drift and jitter automatically.
    /// The same frame may be returned multiple times when capture rate exceeds
    /// source frame rate. Wrapper synchronization and SDK processing still take time.
    ///
    /// # Arguments
    ///
    /// * `field_type` - The desired field format. Use `ScanType::Progressive` for
    ///   most applications. For interlaced output, use the appropriate field type
    ///   to maintain correct field ordering.
    ///
    /// # Returns
    ///
    /// * `Ok(Some(FrameSyncVideoRef))` - A validated zero-copy reference to the captured frame
    /// * `Ok(None)` - The SDK returned the documented all-zero "no video yet" state
    /// * `Err(_)` - The SDK returned an unsupported format or malformed frame
    ///   descriptor, including invalid dimensions, stride, buffers, or metadata
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, FrameSync, Source, SourceAddress, ScanType};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let options = ReceiverOptions::builder(source).build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let framesync = FrameSync::new(receiver)?;
    ///
    /// // Capture loop - call at your output frame rate
    /// loop {
    ///     if let Some(frame) = framesync.capture_video(ScanType::Progressive)? {
    ///         // Process/display frame
    ///         println!("{}x{} @ {}/{} fps",
    ///             frame.width(), frame.height(),
    ///             frame.frame_rate_n(), frame.frame_rate_d());
    ///     } else {
    ///         // No video received yet - display placeholder
    ///         println!("Waiting for video...");
    ///     }
    ///     # break;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn capture_video(&self, field_type: ScanType) -> Result<Option<FrameSyncVideoRef<'_>>> {
        let mut frame = NDIlib_video_frame_v2_t::default();

        unsafe {
            NDIlib_framesync_capture_video(self.instance, &mut frame, field_type.into());
        }

        // Per SDK docs: Returns zeroed struct if no video received yet
        if is_framesync_video_empty(&frame) {
            return Ok(None);
        }

        let guard = unsafe { Guard::<FrameSyncVideoFree>::new(self.instance, frame) };
        let frame = unsafe { FrameSyncVideoRef::new(guard)? };

        Ok(Some(frame))
    }

    /// Capture video and convert to an owned frame.
    ///
    /// This is a convenience method that captures video and immediately converts
    /// it to an owned [`VideoFrame`] that can be sent across threads.
    ///
    /// # Arguments
    ///
    /// * `field_type` - The desired field format.
    ///
    /// # Returns
    ///
    /// * `Ok(Some(VideoFrame))` - An owned copy of the captured frame
    /// * `Ok(None)` - No video has been received yet
    ///
    /// # Errors
    ///
    /// Propagates format and frame-validation errors from [`Self::capture_video`]
    /// or conversion to an owned frame.
    pub fn capture_video_owned(&self, field_type: ScanType) -> Result<Option<VideoFrame>> {
        self.capture_video(field_type)?
            .map(|frame| frame.to_owned())
            .transpose()
    }

    /// Capture audio with dynamic resampling.
    ///
    /// This does not wait for new network audio. Capture requests ask the NDI
    /// SDK to resample incoming audio to match the requested sample rate,
    /// channel count, and sample count. Query requests return the current input
    /// format without requesting samples. Wrapper synchronization and resampling
    /// still take time.
    ///
    /// Call this at the rate you need audio - the SDK will adapt the incoming
    /// signal to match your output timing using dynamic audio sampling.
    ///
    /// # Arguments
    ///
    /// * `request` - Explicit capture or query operation. For capture requests,
    ///   `None` sample rate means "use the current source rate"; channels are explicit.
    ///
    /// # Querying Input Format
    ///
    /// Use [`FrameSyncAudioRequest::query_input`] to query the current input
    /// format without capturing samples. The returned frame contains the
    /// input's sample rate and channel count, or a validated empty no-source
    /// state when no audio format has been received yet.
    ///
    /// # Errors
    ///
    /// Returns frame-validation errors if the SDK returns malformed audio
    /// dimensions, sample layout, buffers, or metadata. Request dimensions were
    /// already validated at construction.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, FrameSync, FrameSyncAudioRequest, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let options = ReceiverOptions::builder(source).build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let framesync = FrameSync::new(receiver)?;
    ///
    /// // Audio callback - called by sound card at its rate
    /// let request = FrameSyncAudioRequest::capture_with(Some(48_000), 2, 1_024)?;
    ///
    /// loop {
    ///     // Request 1024 stereo samples at 48kHz
    ///     let audio = framesync.capture_audio(request)?;
    ///
    ///     // Process each channel without reading inter-channel padding.
    ///     for samples in audio.channels() {
    ///         println!("Got {} samples in a channel", samples.len());
    ///     }
    ///
    ///     // Check for a validated query/no-source empty state
    ///     if audio.is_empty() {
    ///         println!("No audio source yet");
    ///     }
    ///     # break;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn capture_audio(&self, request: FrameSyncAudioRequest) -> Result<FrameSyncAudioRef<'_>> {
        let mut frame = NDIlib_audio_frame_v3_t::default();

        #[cfg(test)]
        audio_test_calls::capture();
        // SAFETY: The live FrameSync owns its receiver/runtime. Request fields
        // are private: capture construction checked the packed FLTP payload and
        // signed byte stride; only query_input() can submit zero samples/channels.
        // Processing.NDI.FrameSync.h permits explicit output channels independent
        // of input and all-zero format queries. No source-channel sentinel can
        // race a format change here. The SDK owns the returned allocation until
        // Guard frees it through this same instance, including validation errors.
        unsafe {
            NDIlib_framesync_capture_audio_v2(
                self.instance,
                &mut frame,
                request.sample_rate,
                request.channels,
                request.samples,
            );
        }

        let guard = unsafe { Guard::<FrameSyncAudioFree>::new(self.instance, frame) };
        unsafe { FrameSyncAudioRef::new(guard, request.samples == 0) }
    }

    /// Capture audio and convert to an owned frame.
    ///
    /// This is a convenience method that captures audio and immediately converts
    /// it to an owned [`AudioFrame`] that can be sent across threads. Query or
    /// no-source states that contain no sample buffer return `Ok(None)`.
    ///
    /// # Arguments
    ///
    /// * `request` - Explicit capture or query operation.
    ///
    /// # Errors
    ///
    /// Propagates returned-frame validation and owned-copy errors from
    /// [`Self::capture_audio`] and [`AudioRef::to_owned`].
    pub fn capture_audio_owned(
        &self,
        request: FrameSyncAudioRequest,
    ) -> Result<Option<AudioFrame>> {
        self.capture_audio(request)?.to_owned()
    }

    /// Query the current audio queue depth.
    ///
    /// Returns the approximate number of audio samples currently buffered.
    /// This can be useful when using an inaccurate timer for audio playback.
    ///
    /// **Note:** This value may change immediately after being read as new
    /// samples are continuously received. For most applications, simply call
    /// `capture_audio` at your desired rate and let the SDK handle timing.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, FrameSync, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let options = ReceiverOptions::builder(source).build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let framesync = FrameSync::new(receiver)?;
    ///
    /// // Check available samples before capture
    /// let available = framesync.audio_queue_depth();
    /// println!("Audio samples available: {}", available);
    /// # Ok(())
    /// # }
    /// ```
    pub fn audio_queue_depth(&self) -> i32 {
        unsafe { NDIlib_framesync_audio_queue_depth(self.instance) }
    }
}

impl Drop for FrameSync {
    fn drop(&mut self) {
        unsafe {
            NDIlib_framesync_destroy(self.instance);
        }
        self.receiver.set_framesync_attached(false);
    }
}

/// # Safety
///
/// The NDI SDK documentation states that framesync operations are thread-safe.
/// The FrameSync owns its receiver, keeping the SDK runtime alive. Borrowed
/// frame guards cannot be moved between threads and keep this owner borrowed
/// until their buffers are freed; destruction requires exclusive ownership.
unsafe impl Send for FrameSync {}

/// # Safety
///
/// The NDI SDK guarantees that framesync capture functions are internally
/// synchronized and can be called concurrently from multiple threads. Receiver
/// attachment disables raw video capture until the synchronizer is destroyed,
/// including when the receiver is accessed through [`FrameSync::receiver`].
unsafe impl Sync for FrameSync {}

impl fmt::Debug for FrameSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSync")
            .field("instance", &self.instance)
            .field("audio_queue_depth", &self.audio_queue_depth())
            .finish()
    }
}

pub(crate) fn is_framesync_video_empty(frame: &NDIlib_video_frame_v2_t) -> bool {
    frame.xres == 0
        && frame.yres == 0
        && frame.FourCC == 0
        && frame.frame_rate_N == 0
        && frame.frame_rate_D == 0
        && frame.picture_aspect_ratio == 0.0
        && frame.frame_format_type == 0
        && frame.timecode == 0
        && frame.p_data.is_null()
        && unsafe { frame.__bindgen_anon_1.data_size_in_bytes } == 0
        && frame.p_metadata.is_null()
        && frame.timestamp == 0
}

// A thread-local seam at the actual SDK capture/free boundaries. This lets tests
// exercise both public entry points without replacing SDK-owned frame storage.
#[cfg(test)]
pub(crate) mod audio_test_calls {
    use std::cell::Cell;
    std::thread_local! {
        static CALLS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    }
    pub(super) fn capture() {
        CALLS.with(|calls| {
            let (captures, frees) = calls.get();
            calls.set((captures + 1, frees));
        });
    }
    pub(crate) fn free() {
        CALLS.with(|calls| {
            let (captures, frees) = calls.get();
            calls.set((captures, frees + 1));
        });
    }
    pub(super) fn take() -> (usize, usize) {
        CALLS.with(|calls| calls.replace((0, 0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::{AudioFormat, LineStrideOrSize, PixelFormat};
    use std::ptr;

    #[test]
    fn audio_requests_reject_invalid_rates_and_dimensions_at_construction() {
        for rate in [i32::MIN, -48_000, -1, 0] {
            assert!(FrameSyncAudioRequest::capture_with(Some(rate), 2, 1024).is_err());
        }
        for (channels, samples) in [
            (0, 1024),
            (-1, 1024),
            (2, 0),
            (2, -1),
            (i32::MIN, i32::MIN),
            (2, 8_388_609),
            (2, 536_870_912),
            (1, 1_073_741_825),
            (i32::MAX, 1),
            (i32::MAX, i32::MAX),
        ] {
            assert!(FrameSyncAudioRequest::capture(channels, samples).is_err());
            assert!(FrameSyncAudioRequest::capture_with(Some(48_000), channels, samples).is_err());
        }
        // Exact cap, with no sample allocation or native call.
        assert!(FrameSyncAudioRequest::capture(2, 8_388_608).is_ok());
        assert!(FrameSyncAudioRequest::capture(1, 16_777_216).is_ok());
        assert!(FrameSyncAudioRequest::capture(1, 16_777_217).is_err());
    }

    #[test]
    fn audio_request_reuse_and_rejection_at_both_sdk_entry_points() {
        let ndi = crate::NDI::new().unwrap();
        let receiver = Receiver::new(
            &ndi,
            &crate::ReceiverOptions::builder(crate::Source::default()).build(),
        )
        .unwrap();
        let fs = FrameSync::new(receiver).unwrap();
        audio_test_calls::take();
        for (channels, samples) in [
            (2, 8_388_609),
            (2, 536_870_912),
            (1, 1_073_741_825),
            (i32::MAX, 1),
        ] {
            assert!(
                FrameSyncAudioRequest::capture_with(Some(48_000), channels, samples)
                    .and_then(|request| fs.capture_audio(request))
                    .is_err()
            );
            assert!(
                FrameSyncAudioRequest::capture_with(Some(48_000), channels, samples)
                    .and_then(|request| fs.capture_audio_owned(request))
                    .is_err()
            );
            assert_eq!(audio_test_calls::take(), (0, 0));
        }

        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..1000 {
                std::hint::black_box(FrameSyncAudioRequest::capture(2, 8_388_608).unwrap());
                std::hint::black_box(FrameSyncAudioRequest::query_input());
            }
        });
        assert_eq!(allocations, 0);
        let (request, allocations) = crate::test_alloc::allocation_count(|| {
            FrameSyncAudioRequest::capture_with(Some(48_000), 2, 1024).unwrap()
        });
        assert_eq!(allocations, 0);
        for _ in 0..3 {
            let ((), allocations) = crate::test_alloc::allocation_count(|| {
                let frame = fs.capture_audio(std::hint::black_box(request)).unwrap();
                assert_eq!(
                    (
                        frame.sample_rate(),
                        frame.num_channels(),
                        frame.num_samples()
                    ),
                    (48_000, 2, 1024)
                );
                assert!(frame
                    .channels()
                    .all(|channel| channel.iter().all(|&s| s == 0.0)));
            });
            assert_eq!(allocations, 0);
            assert_eq!(audio_test_calls::take(), (1, 1));
            assert!(fs.capture_audio_owned(request).unwrap().is_some());
            assert_eq!(audio_test_calls::take(), (1, 1));
        }

        // No source rate is known: response validation must still run and free
        // the SDK descriptor on its error path, for both entry points.
        let unknown_rate = FrameSyncAudioRequest::capture(2, 1024).unwrap();
        assert!(fs.capture_audio(unknown_rate).is_err());
        assert_eq!(audio_test_calls::take(), (1, 1));
        assert!(fs.capture_audio_owned(unknown_rate).is_err());
        assert_eq!(audio_test_calls::take(), (1, 1));
        assert!(fs
            .capture_audio(FrameSyncAudioRequest::query_input())
            .unwrap()
            .is_empty());
        assert_eq!(audio_test_calls::take(), (1, 1));
        assert!(fs
            .capture_audio_owned(FrameSyncAudioRequest::query_input())
            .unwrap()
            .is_none());
        assert_eq!(audio_test_calls::take(), (1, 1));
    }

    #[test]
    fn test_framesync_size() {
        // FrameSync contains an opaque pointer + the owned Receiver
        assert_eq!(
            std::mem::size_of::<FrameSync>(),
            std::mem::size_of::<NDIlib_framesync_instance_t>() + std::mem::size_of::<Receiver>()
        );
    }

    #[test]
    fn test_video_ref_size() {
        // FrameSyncVideoRef includes a reference, frame struct, and pixel_format
        let size = std::mem::size_of::<FrameSyncVideoRef>();
        assert!(size > 0, "FrameSyncVideoRef should have non-zero size");
    }

    #[test]
    fn test_audio_ref_size() {
        // FrameSyncAudioRef includes a reference and frame struct
        let size = std::mem::size_of::<FrameSyncAudioRef>();
        assert!(size > 0, "FrameSyncAudioRef should have non-zero size");
    }

    #[test]
    fn test_video_empty_classifier_accepts_only_all_zero() {
        let empty = NDIlib_video_frame_v2_t::default();
        assert!(is_framesync_video_empty(&empty));

        let mut partial = NDIlib_video_frame_v2_t {
            xres: 1920,
            ..NDIlib_video_frame_v2_t::default()
        };
        assert!(!is_framesync_video_empty(&partial));

        let mut byte = 0u8;
        partial = NDIlib_video_frame_v2_t {
            p_data: &mut byte as *mut u8,
            ..NDIlib_video_frame_v2_t::default()
        };
        assert!(!is_framesync_video_empty(&partial));
    }

    #[test]
    fn test_framesync_video_ref_uses_validated_layout() {
        let width = 16;
        let height = 8;
        let stride = width * 4;
        let expected_len = (stride * height) as usize;
        let mut data = vec![0u8; expected_len];
        let mut metadata = b"framesync video\0".to_vec();

        let raw = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: metadata.as_mut_ptr().cast(),
            timestamp: 0,
        };

        let guard = unsafe { Guard::<FrameSyncVideoFree>::new(ptr::null_mut(), raw) };
        let frame = unsafe { FrameSyncVideoRef::new(guard) }.expect("valid video frame");

        assert_eq!(frame.metadata(), Some("framesync video"));
        assert_eq!(frame.data().unwrap().len(), expected_len);
        assert_eq!(
            frame.line_stride_or_size(),
            LineStrideOrSize::LineStrideBytes(stride)
        );
        let owned = frame.to_owned().expect("owned conversion");
        assert_eq!(owned.metadata(), Some("framesync video"));
        assert!(format!("{frame:?}").contains("FrameSyncVideoRef"));
    }

    #[test]
    fn test_framesync_video_ref_rejects_malformed_metadata() {
        let width = 16;
        let height = 8;
        let stride = width * 4;
        let expected_len = (stride * height) as usize;
        let mut data = vec![0u8; expected_len];
        let mut metadata = vec![b'x'; crate::frames::MAX_METADATA_BYTES];

        let raw = NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: metadata.as_mut_ptr().cast(),
            timestamp: 0,
        };

        let guard = unsafe { Guard::<FrameSyncVideoFree>::new(ptr::null_mut(), raw) };
        assert!(matches!(
            unsafe { FrameSyncVideoRef::new(guard) },
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_framesync_video_ref_rejects_partial_empty() {
        let raw = NDIlib_video_frame_v2_t {
            xres: 16,
            yres: 8,
            FourCC: PixelFormat::BGRA.into(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.into(),
            timecode: 0,
            p_data: ptr::null_mut(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 16 * 4,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let guard = unsafe { Guard::<FrameSyncVideoFree>::new(ptr::null_mut(), raw) };
        let result = unsafe { FrameSyncVideoRef::new(guard) };
        assert!(matches!(result, Err(Error::InvalidFrame(_))));
    }

    #[test]
    fn test_framesync_audio_ref_query_no_source_empty() {
        let raw = NDIlib_audio_frame_v3_t::default();
        let guard = unsafe { Guard::<FrameSyncAudioFree>::new(ptr::null_mut(), raw) };
        let frame = unsafe { FrameSyncAudioRef::new(guard, true) }.expect("empty query frame");

        assert!(frame.is_empty());
        assert_eq!(frame.metadata(), None);
        assert!(frame.data().unwrap().is_empty());
        assert_eq!(frame.format(), None);
        assert!(frame.to_owned().expect("owned conversion").is_none());
        assert!(format!("{frame:?}").contains("FrameSyncAudioRef"));
    }

    #[test]
    fn test_framesync_audio_ref_query_source_format_empty() {
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            no_samples: 0,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: ptr::null_mut(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 0,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let guard = unsafe { Guard::<FrameSyncAudioFree>::new(ptr::null_mut(), raw) };
        let frame = unsafe { FrameSyncAudioRef::new(guard, true) }.expect("query source frame");

        assert!(frame.is_empty());
        assert_eq!(frame.sample_rate(), 48000);
        assert_eq!(frame.num_channels(), 2);
        assert_eq!(frame.format(), Some(AudioFormat::FLTP));
    }

    #[test]
    fn test_framesync_audio_ref_capture_rejects_empty() {
        let raw = NDIlib_audio_frame_v3_t::default();
        let guard = unsafe { Guard::<FrameSyncAudioFree>::new(ptr::null_mut(), raw) };
        let result = unsafe { FrameSyncAudioRef::new(guard, false) };

        assert!(matches!(result, Err(Error::InvalidFrame(_))));
    }

    #[test]
    fn test_framesync_audio_ref_supports_strided_planar_data() {
        let no_samples = 4;
        let no_channels = 2;
        let stride_samples = 6;
        let mut data = vec![0.0f32; 10];
        let mut metadata = b"framesync audio\0".to_vec();
        data[0..4].copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        data[6..10].copy_from_slice(&[10.0, 20.0, 30.0, 40.0]);

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels,
            no_samples,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: stride_samples * 4,
            },
            p_metadata: metadata.as_mut_ptr().cast(),
            timestamp: 0,
        };

        let guard = unsafe { Guard::<FrameSyncAudioFree>::new(ptr::null_mut(), raw) };
        let frame = unsafe { FrameSyncAudioRef::new(guard, false) }.expect("strided audio frame");

        assert_eq!(frame.metadata(), Some("framesync audio"));
        assert!(frame.data().is_none());
        assert_eq!(frame.channel_data(0), Some(&[1.0, 2.0, 3.0, 4.0][..]));
        assert_eq!(frame.channel_data(1), Some(&[10.0, 20.0, 30.0, 40.0][..]));
        assert_eq!(frame.channel_stride_in_bytes(), stride_samples * 4);

        let owned = frame
            .to_owned()
            .expect("owned conversion")
            .expect("samples");
        assert_eq!(owned.metadata(), Some("framesync audio"));
        assert_eq!(owned.data().len(), 8);
        assert_eq!(owned.channel_stride_in_bytes(), 16);
        assert_eq!(owned.channel_data(1), Some(&[10.0, 20.0, 30.0, 40.0][..]));
    }

    #[test]
    fn test_framesync_audio_ref_rejects_invalid_utf8_metadata() {
        let no_samples = 4;
        let no_channels = 2;
        let mut data = vec![0.0f32; (no_samples * no_channels) as usize];
        let mut metadata = vec![0xFF, 0];

        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels,
            no_samples,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_mut_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: no_samples * 4,
            },
            p_metadata: metadata.as_mut_ptr().cast(),
            timestamp: 0,
        };

        let guard = unsafe { Guard::<FrameSyncAudioFree>::new(ptr::null_mut(), raw) };
        assert!(matches!(
            unsafe { FrameSyncAudioRef::new(guard, false) },
            Err(Error::InvalidUtf8(_))
        ));
    }
}
