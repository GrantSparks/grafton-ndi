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
//! use std::{num::NonZeroI32, time::Duration};
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
//!     let audio = framesync.capture_audio(FrameSyncAudioRequest::Capture {
//!         sample_rate: Some(NonZeroI32::new(48_000).unwrap()),
//!         channels: Some(NonZeroI32::new(2).unwrap()),
//!         samples: NonZeroI32::new(1_024).unwrap(),
//!     })?;
//!     println!("{} audio samples", audio.num_samples());
//!
//!     Ok(())
//! }
//! ```

use std::{fmt, mem::ManuallyDrop, num::NonZeroI32, ptr};

use crate::{
    capture::{FrameSyncAudioFree, FrameSyncVideoFree, Guard},
    frames::{AudioFrame, AudioRef, ScanType, VideoFrame, VideoRef},
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

/// Explicit audio operation for [`FrameSync::capture_audio`].
///
/// FrameSync audio has two distinct SDK modes:
/// - [`QueryInput`](Self::QueryInput) asks the SDK for the current input audio
///   format without requesting samples.
/// - [`Capture`](Self::Capture) asks the SDK for a positive number of samples,
///   optionally using the source sample rate and/or source channel count.
///
/// `None` for `sample_rate` or `channels` maps to the SDK's `0` value, meaning
/// "use the current source value". The `samples` field must be positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameSyncAudioRequest {
    /// Query the current incoming audio format without requesting sample data.
    QueryInput,
    /// Capture audio samples with optional source-derived output parameters.
    Capture {
        /// Requested output sample rate. `None` uses the current source rate.
        sample_rate: Option<NonZeroI32>,
        /// Requested output channel count. `None` uses the current source count.
        channels: Option<NonZeroI32>,
        /// Number of samples to capture per channel.
        samples: NonZeroI32,
    },
}

impl FrameSyncAudioRequest {
    /// Capture samples using the source sample rate and source channel count.
    pub fn capture(samples: NonZeroI32) -> Self {
        Self::Capture {
            sample_rate: None,
            channels: None,
            samples,
        }
    }

    /// Capture samples with explicit optional sample-rate and channel requests.
    pub fn capture_with(
        sample_rate: Option<NonZeroI32>,
        channels: Option<NonZeroI32>,
        samples: NonZeroI32,
    ) -> Self {
        Self::Capture {
            sample_rate,
            channels,
            samples,
        }
    }

    fn to_raw(self) -> Result<FrameSyncAudioRawRequest> {
        match self {
            Self::QueryInput => Ok(FrameSyncAudioRawRequest {
                sample_rate: 0,
                channels: 0,
                samples: 0,
                query_input: true,
            }),
            Self::Capture {
                sample_rate,
                channels,
                samples,
            } => Ok(FrameSyncAudioRawRequest {
                sample_rate: positive_optional_i32("sample_rate", sample_rate)?,
                channels: positive_optional_i32("channels", channels)?,
                samples: positive_i32("samples", samples)?,
                query_input: false,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FrameSyncAudioRawRequest {
    sample_rate: i32,
    channels: i32,
    samples: i32,
    query_input: bool,
}

fn positive_optional_i32(field: &str, value: Option<NonZeroI32>) -> Result<i32> {
    value.map_or(Ok(0), |value| positive_i32(field, value))
}

fn positive_i32(field: &str, value: NonZeroI32) -> Result<i32> {
    let value = value.get();
    if value > 0 {
        Ok(value)
    } else {
        Err(Error::InvalidConfiguration(format!(
            "FrameSync audio {field} must be positive, got {value}"
        )))
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
/// frame-sync functions are thread-safe. However, frames returned by capture
/// methods borrow from the FrameSync and are not `Send`.
///
/// # Example
///
/// ```no_run
/// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, FrameSync, FrameSyncAudioRequest, Source, SourceAddress, ScanType};
/// # use std::num::NonZeroI32;
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = NDI::new()?;
/// # let source = Source { name: "Test".into(), address: SourceAddress::None };
/// # let options = ReceiverOptions::builder(source).build();
/// # let receiver = Receiver::new(&ndi, &options)?;
/// let framesync = FrameSync::new(receiver)?;
///
/// // FrameSync captures always return immediately
/// if let Some(video) = framesync.capture_video(ScanType::Progressive)? {
///     println!("Video: {}x{}", video.width(), video.height());
/// }
///
/// // Audio capture uses an explicit request; None means "use source value".
/// let audio = framesync.capture_audio(FrameSyncAudioRequest::Capture {
///     sample_rate: Some(NonZeroI32::new(48_000).unwrap()),
///     channels: Some(NonZeroI32::new(2).unwrap()),
///     samples: NonZeroI32::new(1_024).unwrap(),
/// })?;
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

        Ok(Self { instance, receiver })
    }

    /// Access the underlying receiver.
    ///
    /// Use this to access receiver functionality like tally, PTZ, or status
    /// queries while the FrameSync is active.
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
        // SAFETY: We will not use `this` again after reading the receiver field,
        // and ManuallyDrop prevents the Drop impl from running.
        unsafe { ptr::read(&this.receiver) }
    }

    /// Capture video with time-base correction.
    ///
    /// This function always returns immediately. It returns the best frame for
    /// the current output timing, handling clock drift and jitter automatically.
    /// The same frame may be returned multiple times when capture rate exceeds
    /// source frame rate.
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
    /// * `Err(Error::InvalidFrame(_))` - The SDK returned non-empty malformed metadata
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
    /// Returns an error if the SDK returns non-empty malformed frame metadata.
    pub fn capture_video_owned(&self, field_type: ScanType) -> Result<Option<VideoFrame>> {
        self.capture_video(field_type)?
            .map(|frame| frame.to_owned())
            .transpose()
    }

    /// Capture audio with dynamic resampling.
    ///
    /// This function always returns immediately. Capture requests ask the NDI
    /// SDK to resample incoming audio to match the requested sample rate,
    /// channel count, and sample count. Query requests return the current input
    /// format without requesting samples.
    ///
    /// Call this at the rate you need audio - the SDK will adapt the incoming
    /// signal to match your output timing using dynamic audio sampling.
    ///
    /// # Arguments
    ///
    /// * `request` - Explicit capture or query operation. For capture requests,
    ///   `None` sample rate or channels means "use the current source value".
    ///
    /// # Querying Input Format
    ///
    /// Use [`FrameSyncAudioRequest::QueryInput`] to query the current input
    /// format without capturing samples. The returned frame contains the
    /// input's sample rate and channel count, or a validated empty no-source
    /// state when no audio format has been received yet.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if the SDK returns malformed audio
    /// metadata, or [`Error::InvalidConfiguration`] if the request contains a
    /// negative `NonZeroI32` value.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, FrameSync, FrameSyncAudioRequest, Source, SourceAddress};
    /// # use std::num::NonZeroI32;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let source = Source { name: "Test".into(), address: SourceAddress::None };
    /// # let options = ReceiverOptions::builder(source).build();
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let framesync = FrameSync::new(receiver)?;
    ///
    /// // Audio callback - called by sound card at its rate
    /// let request = FrameSyncAudioRequest::Capture {
    ///     sample_rate: Some(NonZeroI32::new(48_000).unwrap()),
    ///     channels: Some(NonZeroI32::new(2).unwrap()),
    ///     samples: NonZeroI32::new(1_024).unwrap(),
    /// };
    ///
    /// loop {
    ///     // Request 1024 stereo samples at 48kHz
    ///     let audio = framesync.capture_audio(request)?;
    ///
    ///     // Process audio samples
    ///     let samples = audio.data();
    ///     println!("Got {} samples", samples.len());
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
        let request = request.to_raw()?;
        let mut frame = NDIlib_audio_frame_v3_t::default();

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
        unsafe { FrameSyncAudioRef::new(guard, request.query_input) }
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
    /// Returns an error if the request is invalid or the SDK returns malformed
    /// audio metadata.
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
    }
}

/// # Safety
///
/// The NDI SDK documentation states that framesync operations are thread-safe.
/// The FrameSync struct only holds an opaque pointer returned by the SDK.
unsafe impl Send for FrameSync {}

/// # Safety
///
/// The NDI SDK guarantees that framesync capture functions are internally
/// synchronized and can be called concurrently from multiple threads.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::{AudioFormat, LineStrideOrSize, PixelFormat};
    use std::ptr;

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
        assert_eq!(frame.data().len(), expected_len);
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
    fn test_framesync_audio_request_rejects_negative_values() {
        let request = FrameSyncAudioRequest::Capture {
            sample_rate: Some(NonZeroI32::new(-48_000).unwrap()),
            channels: Some(NonZeroI32::new(2).unwrap()),
            samples: NonZeroI32::new(1_024).unwrap(),
        };

        assert!(matches!(
            request.to_raw(),
            Err(Error::InvalidConfiguration(_))
        ));
    }

    #[test]
    fn test_framesync_audio_ref_query_no_source_empty() {
        let raw = NDIlib_audio_frame_v3_t::default();
        let guard = unsafe { Guard::<FrameSyncAudioFree>::new(ptr::null_mut(), raw) };
        let frame = unsafe { FrameSyncAudioRef::new(guard, true) }.expect("empty query frame");

        assert!(frame.is_empty());
        assert_eq!(frame.metadata(), None);
        assert!(frame.data().is_empty());
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
        assert_eq!(frame.data().len(), 10);
        assert_eq!(frame.channel_data(0), Some(&[1.0, 2.0, 3.0, 4.0][..]));
        assert_eq!(frame.channel_data(1), Some(&[10.0, 20.0, 30.0, 40.0][..]));
        assert_eq!(frame.channel_stride_in_bytes(), stride_samples * 4);

        let owned = frame
            .to_owned()
            .expect("owned conversion")
            .expect("samples");
        assert_eq!(owned.metadata(), Some("framesync audio"));
        assert_eq!(owned.data().len(), 10);
        assert_eq!(owned.channel_data(1), Some(vec![10.0, 20.0, 30.0, 40.0]));
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
