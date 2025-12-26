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
//! use grafton_ndi::{NDI, Finder, FinderOptions, ReceiverOptions, Receiver, FrameSync, ScanType};
//! use std::time::Duration;
//!
//! fn main() -> Result<(), grafton_ndi::Error> {
//!     let ndi = NDI::new()?;
//!     let finder = Finder::new(&ndi, &FinderOptions::default())?;
//!     finder.wait_for_sources(Duration::from_secs(1))?;
//!     let sources = finder.sources(Duration::ZERO)?;
//!
//!     let options = ReceiverOptions::builder(sources[0].clone()).build();
//!     let receiver = Receiver::new(&ndi, &options)?;
//!
//!     // Create frame-sync from receiver
//!     let framesync = FrameSync::new(&receiver)?;
//!
//!     // Capture video synced to your output timing
//!     if let Some(frame) = framesync.capture_video(ScanType::Progressive) {
//!         println!("{}x{} frame", frame.width(), frame.height());
//!     }
//!
//!     // Capture audio at your sound card's rate
//!     let audio = framesync.capture_audio(48000, 2, 1024);
//!     println!("{} audio samples", audio.num_samples());
//!
//!     Ok(())
//! }
//! ```

use std::{ffi::CStr, fmt, marker::PhantomData, slice};

use crate::{
    frames::{AudioFormat, AudioFrame, LineStrideOrSize, PixelFormat, ScanType, VideoFrame},
    ndi_lib::*,
    receiver::Receiver,
    Error, Result,
};

/// Frame synchronizer for clock-corrected capture.
///
/// Converts push-based NDI streams into pull-based capture with automatic
/// time-base correction and dynamic audio resampling.
///
/// # Lifetime
///
/// The `'rx` lifetime ties this `FrameSync` to the [`Receiver`] that created it.
/// This ensures the receiver cannot be dropped while the FrameSync is alive,
/// preventing use-after-free.
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
/// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, FrameSync, Source, SourceAddress, ScanType};
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = NDI::new()?;
/// # let source = Source { name: "Test".into(), address: SourceAddress::None };
/// # let options = ReceiverOptions::builder(source).build();
/// # let receiver = Receiver::new(&ndi, &options)?;
/// let framesync = FrameSync::new(&receiver)?;
///
/// // FrameSync captures always return immediately
/// if let Some(video) = framesync.capture_video(ScanType::Progressive) {
///     println!("Video: {}x{}", video.width(), video.height());
/// }
///
/// // Audio capture always returns data (silence if none available)
/// let audio = framesync.capture_audio(48000, 2, 1024);
/// println!("Audio: {} samples", audio.num_samples());
/// # Ok(())
/// # }
/// ```
pub struct FrameSync<'rx> {
    instance: NDIlib_framesync_instance_t,
    _receiver: PhantomData<&'rx Receiver>,
}

impl<'rx> FrameSync<'rx> {
    /// Create a frame synchronizer from a receiver.
    ///
    /// Once created, use the `FrameSync` for video/audio capture instead of
    /// the receiver's capture methods. The receiver can still be used for
    /// other operations (tally, PTZ, status, etc.).
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
    /// let framesync = FrameSync::new(&receiver)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(receiver: &'rx Receiver) -> Result<Self> {
        let instance = unsafe { NDIlib_framesync_create(receiver.instance) };

        if instance.is_null() {
            return Err(Error::InitializationFailed(
                "Failed to create NDI framesync instance".into(),
            ));
        }

        Ok(Self {
            instance,
            _receiver: PhantomData,
        })
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
    /// * `Some(FrameSyncVideoRef)` - A zero-copy reference to the captured frame
    /// * `None` - No video has been received yet (before first frame arrives)
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
    /// let framesync = FrameSync::new(&receiver)?;
    ///
    /// // Capture loop - call at your output frame rate
    /// loop {
    ///     if let Some(frame) = framesync.capture_video(ScanType::Progressive) {
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
    pub fn capture_video(&self, field_type: ScanType) -> Option<FrameSyncVideoRef<'_>> {
        let mut frame = NDIlib_video_frame_v2_t::default();

        unsafe {
            NDIlib_framesync_capture_video(self.instance, &mut frame, field_type.into());
        }

        // Per SDK docs: Returns zeroed struct if no video received yet
        if frame.p_data.is_null() || frame.xres == 0 || frame.yres == 0 {
            return None;
        }

        // Try to parse the pixel format - return None if unknown
        #[allow(clippy::unnecessary_cast)]
        let pixel_format = match PixelFormat::try_from(frame.FourCC as u32) {
            Ok(fmt) => fmt,
            Err(_) => return None,
        };

        Some(FrameSyncVideoRef {
            framesync: self,
            frame,
            pixel_format,
        })
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
    /// * `Some(Ok(VideoFrame))` - An owned copy of the captured frame
    /// * `Some(Err(_))` - Frame was captured but conversion failed
    /// * `None` - No video has been received yet
    pub fn capture_video_owned(&self, field_type: ScanType) -> Option<Result<VideoFrame>> {
        self.capture_video(field_type).map(|frame| frame.to_owned())
    }

    /// Capture audio with dynamic resampling.
    ///
    /// This function always returns immediately, inserting silence if no audio
    /// is available. The NDI SDK automatically resamples the incoming audio to
    /// match the requested sample rate, channel count, and sample count.
    ///
    /// Call this at the rate you need audio - the SDK will adapt the incoming
    /// signal to match your output timing using dynamic audio sampling.
    ///
    /// # Arguments
    ///
    /// * `sample_rate` - Desired output sample rate (e.g., 48000)
    /// * `channels` - Desired number of output channels (e.g., 2 for stereo)
    /// * `samples` - Number of samples to capture per channel
    ///
    /// # Querying Input Format
    ///
    /// Pass 0 for `sample_rate` and `channels` to query the current input format
    /// without capturing samples. The returned frame will contain the input's
    /// sample rate and channel count (or zeros if no audio received yet).
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
    /// let framesync = FrameSync::new(&receiver)?;
    ///
    /// // Audio callback - called by sound card at its rate
    /// loop {
    ///     // Request 1024 stereo samples at 48kHz
    ///     let audio = framesync.capture_audio(48000, 2, 1024);
    ///
    ///     // Process audio samples
    ///     let samples = audio.data();
    ///     println!("Got {} samples", samples.len());
    ///
    ///     // Check if we're receiving audio or just silence
    ///     if audio.sample_rate() == 0 {
    ///         println!("No audio source yet");
    ///     }
    ///     # break;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn capture_audio(
        &self,
        sample_rate: i32,
        channels: i32,
        samples: i32,
    ) -> FrameSyncAudioRef<'_> {
        let mut frame = NDIlib_audio_frame_v3_t::default();

        unsafe {
            NDIlib_framesync_capture_audio_v2(
                self.instance,
                &mut frame,
                sample_rate,
                channels,
                samples,
            );
        }

        FrameSyncAudioRef {
            framesync: self,
            frame,
        }
    }

    /// Capture audio and convert to an owned frame.
    ///
    /// This is a convenience method that captures audio and immediately converts
    /// it to an owned [`AudioFrame`] that can be sent across threads.
    ///
    /// # Arguments
    ///
    /// * `sample_rate` - Desired output sample rate
    /// * `channels` - Desired number of output channels
    /// * `samples` - Number of samples to capture per channel
    ///
    /// # Errors
    ///
    /// Returns an error if the frame conversion fails (e.g., invalid format).
    pub fn capture_audio_owned(
        &self,
        sample_rate: i32,
        channels: i32,
        samples: i32,
    ) -> Result<AudioFrame> {
        self.capture_audio(sample_rate, channels, samples)
            .to_owned()
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
    /// let framesync = FrameSync::new(&receiver)?;
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

impl Drop for FrameSync<'_> {
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
unsafe impl Send for FrameSync<'_> {}

/// # Safety
///
/// The NDI SDK guarantees that framesync capture functions are internally
/// synchronized and can be called concurrently from multiple threads.
unsafe impl Sync for FrameSync<'_> {}

impl fmt::Debug for FrameSync<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSync")
            .field("instance", &self.instance)
            .field("audio_queue_depth", &self.audio_queue_depth())
            .finish()
    }
}

/// A zero-copy borrowed video frame from FrameSync capture.
///
/// This type wraps a frame captured via [`FrameSync::capture_video`], providing
/// zero-copy access to the video data. The frame is automatically freed when
/// dropped via `NDIlib_framesync_free_video`.
///
/// **Key characteristics:**
/// - Zero allocations: References NDI SDK buffers directly
/// - Zero copies: No memcpy of pixel data
/// - RAII lifetime: Exactly one free per frame, enforced at compile time
/// - Not `Send`: Prevents accidental cross-thread use of FFI buffers
///
/// # Converting to Owned
///
/// To store the frame or send it across threads, use [`to_owned()`](Self::to_owned):
///
/// ```no_run
/// # use grafton_ndi::{NDI, ReceiverOptions, Receiver, FrameSync, Source, SourceAddress, ScanType};
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = NDI::new()?;
/// # let source = Source { name: "Test".into(), address: SourceAddress::None };
/// # let options = ReceiverOptions::builder(source).build();
/// # let receiver = Receiver::new(&ndi, &options)?;
/// let framesync = FrameSync::new(&receiver)?;
///
/// if let Some(frame_ref) = framesync.capture_video(ScanType::Progressive) {
///     let owned = frame_ref.to_owned()?;
///     // owned can now be sent across threads
/// }
/// # Ok(())
/// # }
/// ```
pub struct FrameSyncVideoRef<'fs> {
    framesync: &'fs FrameSync<'fs>,
    frame: NDIlib_video_frame_v2_t,
    pixel_format: PixelFormat,
}

impl<'fs> FrameSyncVideoRef<'fs> {
    /// Get the frame width in pixels.
    pub fn width(&self) -> i32 {
        self.frame.xres
    }

    /// Get the frame height in pixels.
    pub fn height(&self) -> i32 {
        self.frame.yres
    }

    /// Get the pixel format (FourCC code).
    pub fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    /// Get the frame rate numerator.
    pub fn frame_rate_n(&self) -> i32 {
        self.frame.frame_rate_N
    }

    /// Get the frame rate denominator.
    pub fn frame_rate_d(&self) -> i32 {
        self.frame.frame_rate_D
    }

    /// Get the picture aspect ratio.
    pub fn picture_aspect_ratio(&self) -> f32 {
        self.frame.picture_aspect_ratio
    }

    /// Get the scan type (progressive, interlaced, etc.).
    pub fn scan_type(&self) -> ScanType {
        #[allow(clippy::unnecessary_cast)]
        ScanType::try_from(self.frame.frame_format_type as u32).unwrap_or(ScanType::Progressive)
    }

    /// Get the timecode.
    pub fn timecode(&self) -> i64 {
        self.frame.timecode
    }

    /// Get the timestamp.
    pub fn timestamp(&self) -> i64 {
        self.frame.timestamp
    }

    /// Get the line stride or data size.
    pub fn line_stride_or_size(&self) -> LineStrideOrSize {
        if self.pixel_format.is_uncompressed() {
            let line_stride = unsafe { self.frame.__bindgen_anon_1.line_stride_in_bytes };
            LineStrideOrSize::LineStrideBytes(line_stride)
        } else {
            let data_size = unsafe { self.frame.__bindgen_anon_1.data_size_in_bytes };
            LineStrideOrSize::DataSizeBytes(data_size)
        }
    }

    /// Get the metadata as a `CStr`, if present.
    pub fn metadata(&self) -> Option<&CStr> {
        if self.frame.p_metadata.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(self.frame.p_metadata) })
        }
    }

    /// Get a zero-copy view of the frame data.
    ///
    /// This returns a slice directly into the NDI SDK's buffer.
    /// No allocation or memcpy is performed.
    pub fn data(&self) -> &[u8] {
        if self.frame.p_data.is_null() {
            return &[];
        }

        let data_size = if self.pixel_format.is_uncompressed() {
            let line_stride = unsafe { self.frame.__bindgen_anon_1.line_stride_in_bytes };
            if line_stride > 0 && self.frame.yres > 0 && self.frame.xres > 0 {
                self.pixel_format
                    .info()
                    .buffer_len(line_stride, self.frame.yres)
            } else {
                0
            }
        } else {
            let size = unsafe { self.frame.__bindgen_anon_1.data_size_in_bytes };
            if size > 0 {
                size as usize
            } else {
                0
            }
        };

        if data_size == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(self.frame.p_data, data_size) }
        }
    }

    /// Convert this borrowed frame to an owned `VideoFrame`.
    ///
    /// This performs a single memcpy of the frame data and metadata,
    /// allowing the frame to outlive the NDI buffer and be sent across threads.
    pub fn to_owned(&self) -> Result<VideoFrame> {
        unsafe { VideoFrame::from_raw(&self.frame) }
    }
}

impl Drop for FrameSyncVideoRef<'_> {
    fn drop(&mut self) {
        unsafe {
            NDIlib_framesync_free_video(self.framesync.instance, &mut self.frame);
        }
    }
}

impl fmt::Debug for FrameSyncVideoRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSyncVideoRef")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("pixel_format", &self.pixel_format())
            .field("frame_rate_n", &self.frame_rate_n())
            .field("frame_rate_d", &self.frame_rate_d())
            .field("picture_aspect_ratio", &self.picture_aspect_ratio())
            .field("scan_type", &self.scan_type())
            .field("timecode", &self.timecode())
            .field("data (bytes)", &self.data().len())
            .field("line_stride_or_size", &self.line_stride_or_size())
            .field("metadata", &self.metadata())
            .field("timestamp", &self.timestamp())
            .finish()
    }
}

/// A zero-copy borrowed audio frame from FrameSync capture.
///
/// This type wraps a frame captured via [`FrameSync::capture_audio`], providing
/// zero-copy access to the audio samples. The frame is automatically freed when
/// dropped via `NDIlib_framesync_free_audio_v2`.
///
/// **Key characteristics:**
/// - Zero allocations: References NDI SDK buffers directly
/// - Zero copies: No memcpy of audio samples
/// - RAII lifetime: Exactly one free per frame, enforced at compile time
/// - Not `Send`: Prevents accidental cross-thread use of FFI buffers
///
/// # Always Returns Data
///
/// Unlike raw receiver capture, FrameSync audio capture *always* returns data.
/// If no audio is available, the SDK inserts silence. Check `sample_rate() == 0`
/// to detect when no source audio has been received yet.
pub struct FrameSyncAudioRef<'fs> {
    framesync: &'fs FrameSync<'fs>,
    frame: NDIlib_audio_frame_v3_t,
}

impl<'fs> FrameSyncAudioRef<'fs> {
    /// Get the sample rate in Hz.
    ///
    /// Returns 0 if no audio source has been received yet.
    pub fn sample_rate(&self) -> i32 {
        self.frame.sample_rate
    }

    /// Get the number of audio channels.
    ///
    /// Returns 0 if no audio source has been received yet.
    pub fn num_channels(&self) -> i32 {
        self.frame.no_channels
    }

    /// Get the number of samples per channel.
    pub fn num_samples(&self) -> i32 {
        self.frame.no_samples
    }

    /// Get the timecode.
    pub fn timecode(&self) -> i64 {
        self.frame.timecode
    }

    /// Get the timestamp.
    pub fn timestamp(&self) -> i64 {
        self.frame.timestamp
    }

    /// Get the audio format (FourCC code).
    pub fn format(&self) -> Option<AudioFormat> {
        match self.frame.FourCC {
            NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP => Some(AudioFormat::FLTP),
            _ => None,
        }
    }

    /// Get the channel stride in bytes.
    pub fn channel_stride_in_bytes(&self) -> i32 {
        unsafe { self.frame.__bindgen_anon_1.channel_stride_in_bytes }
    }

    /// Get the metadata as a `CStr`, if present.
    pub fn metadata(&self) -> Option<&CStr> {
        if self.frame.p_metadata.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(self.frame.p_metadata) })
        }
    }

    /// Get a zero-copy view of the audio data as 32-bit floats.
    ///
    /// This returns a slice directly into the NDI SDK's buffer.
    /// No allocation or memcpy is performed.
    ///
    /// The data is in planar format: all samples for channel 0, then all for
    /// channel 1, etc.
    pub fn data(&self) -> &[f32] {
        if self.frame.p_data.is_null() {
            return &[];
        }

        let sample_count = (self.frame.no_samples * self.frame.no_channels) as usize;
        if sample_count == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(self.frame.p_data as *const f32, sample_count) }
        }
    }

    /// Convert this borrowed frame to an owned `AudioFrame`.
    ///
    /// This performs a single memcpy of the audio data and metadata,
    /// allowing the frame to outlive the NDI buffer and be sent across threads.
    pub fn to_owned(&self) -> Result<AudioFrame> {
        AudioFrame::from_raw(self.frame)
    }
}

impl Drop for FrameSyncAudioRef<'_> {
    fn drop(&mut self) {
        unsafe {
            NDIlib_framesync_free_audio_v2(self.framesync.instance, &mut self.frame);
        }
    }
}

impl fmt::Debug for FrameSyncAudioRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSyncAudioRef")
            .field("sample_rate", &self.sample_rate())
            .field("num_channels", &self.num_channels())
            .field("num_samples", &self.num_samples())
            .field("timecode", &self.timecode())
            .field("format", &self.format())
            .field("data (samples)", &self.data().len())
            .field("channel_stride_in_bytes", &self.channel_stride_in_bytes())
            .field("metadata", &self.metadata())
            .field("timestamp", &self.timestamp())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_framesync_size() {
        // FrameSync should be a small struct - just a pointer + PhantomData
        assert_eq!(
            std::mem::size_of::<FrameSync>(),
            std::mem::size_of::<*mut ()>()
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
}
