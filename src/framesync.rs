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
//!     Ndi, Finder, FinderOptions, ReceiverOptions, Receiver, FrameSync,
//!     FrameSyncAudioRequest, ScanType,
//! };
//! use std::time::Duration;
//!
//! fn main() -> Result<(), grafton_ndi::Error> {
//!     let ndi = Ndi::new()?;
//!     let finder = Finder::new(&ndi, &FinderOptions::default())?;
//!     finder.wait_for_sources(Duration::from_secs(1))?;
//!     let sources = finder.current_sources()?;
//!
//!     let options = ReceiverOptions::new(sources[0].clone());
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
//!     let request = FrameSyncAudioRequest::capture_at(48_000, 2, 1_024)?;
//!     let audio = framesync.capture_audio(request)?;
//!     println!("{} audio samples", audio.num_samples());
//!
//!     Ok(())
//! }
//! ```

use std::{fmt, num::NonZeroU32};

use crate::{
    capture::{FrameSyncAudioFree, FrameSyncVideoFree, Guard},
    frames::{
        validate_audio_sample_rate, validate_packed_audio_dimensions, validate_returned_audio,
        AudioFrame, AudioRef, ReturnedAudio, ScanType, VideoFrame, VideoRef,
    },
    receiver::{FrameSyncAttached, Receiver},
    sys::bindings::*,
    Error, Rejected, Result,
};

/// A zero-copy borrowed video frame from a [`FrameSync`] capture.
///
/// The [`Receiver`] equivalent is [`VideoFrameRef`](crate::VideoFrameRef). Both share one accessor
/// and `Debug` implementation. The frame is automatically freed when dropped via
/// `NDIlib_framesync_free_video`.
/// This owning reference is neither `Send` nor `Sync`; its immutable plane/row
/// views support scoped worker reads while it stays borrowed on this thread.
pub type FrameSyncVideoRef<'fs> = VideoRef<'fs, FrameSyncVideoFree>;

/// A zero-copy borrowed audio frame from a [`FrameSync`] capture.
///
/// The [`Receiver`] equivalent is [`AudioFrameRef`](crate::AudioFrameRef). Both share one accessor
/// and `Debug` implementation, and both always hold samples. The frame is
/// automatically freed when dropped via `NDIlib_framesync_free_audio_v2`.
/// This owning reference is neither `Send` nor `Sync`.
pub type FrameSyncAudioRef<'fs> = AudioRef<'fs, FrameSyncAudioFree>;

/// The format of the audio a [`FrameSync`] is receiving, as reported by
/// [`FrameSync::audio_input_format`].
///
/// Both values are positive. The sample rate is at most `i32::MAX`, so
/// [`FrameSyncAudioRequest::capture_at`] accepts it. This is a plain value: it
/// is `Copy`, `Send` and `Sync`, and holds no SDK resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioInputFormat {
    sample_rate: NonZeroU32,
    num_channels: NonZeroU32,
}

impl AudioInputFormat {
    /// A format the SDK reported, with values already checked positive.
    pub(crate) const fn new(sample_rate: NonZeroU32, num_channels: NonZeroU32) -> Self {
        Self {
            sample_rate,
            num_channels,
        }
    }

    /// The incoming audio's sample rate in Hz: positive.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.get()
    }

    /// The incoming audio's channel count: positive.
    pub fn num_channels(&self) -> u32 {
        self.num_channels.get()
    }
}

/// A validated, reusable audio capture for [`FrameSync::capture_audio`] and
/// [`FrameSync::capture_audio_ref`].
///
/// [`capture_at`](Self::capture_at) and [`for_input`](Self::for_input) require
/// an explicit positive sample rate, output channels and samples per channel.
/// Construction checks the SDK's signed byte stride and the 64 MiB packed
/// sample payload limit before any native work. This bounds requested samples, not SDK internal memory use.
/// Successful construction and request reuse allocate nothing in the wrapper;
/// submitting a request performs no dimension checks, locks, or extra SDK queries.
/// Returned descriptors and metadata are still validated after each capture.
///
/// Every request captures samples. To follow the source's format, call
/// [`FrameSync::audio_input_format`] until it returns `Some`, then build a
/// request with [`for_input`](Self::for_input). Query again and rebuild when
/// the application chooses to follow format changes. The SDK converts to the
/// request's rate and channel count even if the source changes meanwhile.
///
/// # Example
///
/// ```no_run
/// # use grafton_ndi::{Ndi, ReceiverOptions, Receiver, FrameSync, FrameSyncAudioRequest, Source};
/// # use std::time::Duration;
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = Ndi::new()?;
/// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("Test")))?;
/// let framesync = FrameSync::new(receiver)?;
///
/// // Wait until the source's audio format is known.
/// let format = loop {
///     if let Some(format) = framesync.audio_input_format()? {
///         break format;
///     }
///     std::thread::sleep(Duration::from_millis(10));
/// };
///
/// // 1024 samples per channel, in the source's channels, at 48 kHz.
/// let request = FrameSyncAudioRequest::capture_at(48_000, format.num_channels(), 1_024)?;
/// let audio = framesync.capture_audio(request)?;
/// assert_eq!(audio.num_channels(), format.num_channels());
///
/// // Or in the source's rate and channels.
/// let request = FrameSyncAudioRequest::for_input(format, 1_024)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameSyncAudioRequest {
    // The SDK's signed arguments, all checked positive.
    sample_rate: i32,
    num_channels: i32,
    num_samples: i32,
}

impl FrameSyncAudioRequest {
    /// Requested output sample rate in Hz.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate as u32
    }

    /// Requested number of output channels.
    #[must_use]
    pub fn num_channels(&self) -> u32 {
        self.num_channels as u32
    }

    /// Requested number of output samples per channel.
    #[must_use]
    pub fn num_samples(&self) -> u32 {
        self.num_samples as u32
    }

    /// Capture samples resampled to `sample_rate` Hz, converted to
    /// `num_channels` channels of `num_samples` samples each.
    ///
    /// To follow the source's format, use [`for_input`](Self::for_input).
    ///
    /// A request can be copied and reused across borrowed and owned captures.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] for a sample rate outside
    /// `1..=i32::MAX`, a zero dimension, an unrepresentable signed byte
    /// stride, arithmetic overflow, or a sample payload exceeding 64 MiB.
    /// Validation does not allocate sample storage.
    pub fn capture_at(sample_rate: u32, num_channels: u32, num_samples: u32) -> Result<Self> {
        let sample_rate = validate_audio_sample_rate(sample_rate.into())?;
        validate_packed_audio_dimensions(num_channels, num_samples)?;
        // Lossless: the rate and the packed dimensions were checked to fit
        // the SDK's signed arguments.
        Ok(Self {
            sample_rate: sample_rate as i32,
            num_channels: num_channels as i32,
            num_samples: num_samples as i32,
        })
    }

    /// Capture samples at the sample rate and channel count of `format`, with
    /// `num_samples` samples per channel.
    ///
    /// `format` comes from [`FrameSync::audio_input_format`], which returns
    /// `None` until the source's audio has arrived. The request fixes the
    /// format it was built from: if the source changes later the SDK converts
    /// to the request's rate and channels. To follow such changes, query the
    /// format again and build a new request.
    ///
    /// # Errors
    ///
    /// As for [`capture_at`](Self::capture_at): [`Error::InvalidFrame`] for a
    /// zero or oversized `num_samples`, or a payload exceeding 64 MiB.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, ReceiverOptions, Receiver, FrameSync, FrameSyncAudioRequest, Source};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("Test")))?;
    /// let framesync = FrameSync::new(receiver)?;
    ///
    /// // Wait until the source's audio format is known.
    /// let format = loop {
    ///     if let Some(format) = framesync.audio_input_format()? {
    ///         break format;
    ///     }
    ///     std::thread::sleep(Duration::from_millis(10));
    /// };
    ///
    /// let request = FrameSyncAudioRequest::for_input(format, 1_024)?;
    /// let audio = framesync.capture_audio(request)?;
    /// assert_eq!(audio.sample_rate(), format.sample_rate());
    /// // To follow a source change, call `audio_input_format` again and
    /// // build a new request.
    /// # Ok(())
    /// # }
    /// ```
    pub fn for_input(format: AudioInputFormat, num_samples: u32) -> Result<Self> {
        Self::capture_at(format.sample_rate(), format.num_channels(), num_samples)
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
/// `FrameSync` is `Send + Sync` like `Receiver`: it may move to another
/// thread, and one FrameSync may be shared, for example by a video render
/// thread and an audio callback, whose captures may overlap. The SDK does not
/// document this; it rests on how the supported runtimes behave, measured as
/// recorded beside the `Sync` implementation. [`Self::audio_queue_depth`] is
/// read without the SDK's lock and is approximate.
///
/// The `capture_*` methods return owned frames, which can move to another
/// thread. Frames returned by the `capture_*_ref` methods borrow from the
/// FrameSync and are neither `Send` nor `Sync`; their immutable video
/// plane/row views can instead be moved or shared with scoped workers while
/// the frame guard stays on its capture thread until those workers finish.
///
/// Raw video and audio capture through the underlying receiver are disabled
/// while this FrameSync exists, because the synchronizer takes the receiver's
/// video and audio; see [`FrameSyncAttached`]. [`Self::into_receiver`]
/// destroys the synchronizer and restores raw video and audio capture before
/// returning the receiver.
///
/// # Example
///
/// ```no_run
/// # use grafton_ndi::{Ndi, ReceiverOptions, Receiver, FrameSync, FrameSyncAudioRequest, Source, SourceAddress, ScanType};
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = Ndi::new()?;
/// # let source = Source::new("Test");
/// # let options = ReceiverOptions::new(source);
/// # let receiver = Receiver::new(&ndi, &options)?;
/// let framesync = FrameSync::new(receiver)?;
///
/// // Sample available data without waiting for a new network frame
/// if let Some(video) = framesync.capture_video_ref(ScanType::Progressive)? {
///     println!("Video: {}x{}", video.width(), video.height());
/// }
///
/// // Audio capture fixes the output dimensions before entering the SDK.
/// let audio = framesync.capture_audio_ref(FrameSyncAudioRequest::capture_at(48_000, 2, 1_024)?)?;
/// println!("Audio: {} samples", audio.num_samples());
/// # Ok(())
/// # }
/// ```
pub struct FrameSync {
    // Rust drops fields in declaration order. The SDK requires FrameSync to be
    // destroyed before its receiver (Processing.NDI.FrameSync.h, create contract).
    resource: FrameSyncResource,
    receiver: Receiver<FrameSyncAttached>,
}

struct FrameSyncResource {
    instance: NDIlib_framesync_instance_t,
    // Retain the runtime directly until after native destruction.
    _ndi: crate::Ndi,
}

/// What one `NDIlib_framesync_capture_audio_v2` call asks the SDK for.
#[derive(Clone, Copy)]
enum AudioCall {
    /// Samples in a validated request's rate, channels and sample count.
    Samples(FrameSyncAudioRequest),
    /// The input-format query: a zero sample rate, channel count and sample
    /// count.
    InputFormat,
}

impl FrameSyncResource {
    /// Capture video for `scan_type`: the guard of the frame the SDK
    /// returned, or `None` for the documented all-zero "no video yet" state,
    /// which holds nothing to free. The one place a FrameSync video guard is
    /// made.
    fn capture_video(&self, scan_type: ScanType) -> Option<Guard<'_, FrameSyncVideoFree>> {
        let mut frame = NDIlib_video_frame_v2_t::default();
        // SAFETY: the FrameSync instance is live for this borrow and `frame` is
        // writable storage the SDK fills or leaves zeroed.
        unsafe {
            self._ndi.api().core.NDIlib_framesync_capture_video(
                self.instance,
                &mut frame,
                scan_type.to_sdk(),
            );
        }
        if is_framesync_video_empty(&frame) {
            return None;
        }
        // SAFETY: the call above filled `frame` through this instance, which
        // was created through `_ndi`. The guard borrows this resource, which
        // destroys the instance only when dropped, so the instance outlives
        // the guard, which frees the frame exactly once through
        // NDIlib_framesync_free_video, including when validation fails.
        Some(unsafe { Guard::new(&self._ndi, self.instance, frame) })
    }

    /// Make `call`: the guard of what the SDK returned, which it returns
    /// whether or not audio has arrived. The one place a FrameSync audio
    /// guard is made.
    fn capture_audio(&self, call: AudioCall) -> Guard<'_, FrameSyncAudioFree> {
        let (sample_rate, num_channels, num_samples) = match call {
            AudioCall::Samples(request) => (
                request.sample_rate,
                request.num_channels,
                request.num_samples,
            ),
            AudioCall::InputFormat => (0, 0, 0),
        };
        let mut frame = NDIlib_audio_frame_v3_t::default();

        #[cfg(test)]
        audio_test_calls::capture();
        // SAFETY: the FrameSync instance is live for this borrow and `frame` is
        // writable storage. Processing.NDI.FrameSync.h defines both calls:
        // - A request's private fields were checked at construction: positive
        //   channels and samples, the packed FLTP payload and the signed byte
        //   stride, and a positive sample rate. The SDK permits an output rate
        //   and channel count independent of the input, and supplies silence
        //   before audio arrives, so no source-format sentinel can race a
        //   format change here.
        // - A zero sample rate, channel count and sample count is a query
        //   that returns the current input format, or a zero sample rate and
        //   channel count when there is none, without capturing samples.
        // Either way the SDK owns what it returned until it is freed through
        // this same instance.
        unsafe {
            self._ndi.api().core.NDIlib_framesync_capture_audio_v2(
                self.instance,
                &mut frame,
                sample_rate,
                num_channels,
                num_samples,
            );
        }

        // SAFETY: the call above filled `frame` through this instance, which
        // was created through `_ndi`. The guard borrows this resource, which
        // destroys the instance only when dropped, so the instance outlives
        // the guard, which frees the frame exactly once through
        // NDIlib_framesync_free_audio_v2, including when validation fails.
        unsafe { Guard::new(&self._ndi, self.instance, frame) }
    }
}

impl Drop for FrameSyncResource {
    fn drop(&mut self) {
        // SAFETY: Creation checked for null; exclusive ownership and borrowed
        // guards prevent destruction while capture buffers remain live.
        unsafe { self._ndi.api().core.NDIlib_framesync_destroy(self.instance) };
        #[cfg(test)]
        crate::receiver::lifecycle_test_events::record("framesync");
    }
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
    /// Returns the receiver, unchanged, with
    /// [`Error::NativeOperationFailed`] if the NDI SDK does not create the
    /// frame-sync instance.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, ReceiverOptions, Receiver, FrameSync, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let source = Source::new("Test");
    /// # let options = ReceiverOptions::new(source);
    /// let receiver = Receiver::new(&ndi, &options)?;
    /// let framesync = FrameSync::new(receiver)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(receiver: Receiver) -> std::result::Result<Self, Rejected<Receiver>> {
        // SAFETY: The owned receiver retains a live SDK resource through creation;
        // successful creation transfers both resources into the ordered owners.
        let core = &receiver.core._ndi.api().core;
        Self::new_with(receiver, |instance| unsafe {
            core.NDIlib_framesync_create(instance)
        })
    }

    fn new_with(
        receiver: Receiver,
        create: impl FnOnce(NDIlib_recv_instance_t) -> NDIlib_framesync_instance_t,
    ) -> std::result::Result<Self, Rejected<Receiver>> {
        let instance = create(receiver.core.instance);
        if instance.is_null() {
            // The untouched raw receiver goes back to the caller.
            return Err(Rejected::new(
                Error::NativeOperationFailed {
                    operation: "NDIlib_framesync_create",
                },
                receiver,
            ));
        }
        let resource = FrameSyncResource {
            instance,
            _ndi: receiver.core._ndi.clone(),
        };
        Ok(Self {
            resource,
            receiver: receiver.into_mode(),
        })
    }

    /// Access the underlying receiver.
    ///
    /// Use this to access receiver functionality like tally, PTZ, status
    /// queries, metadata capture or event polling while the FrameSync is
    /// active. Tally control publishes state upstream; it does not query
    /// incoming tally.
    ///
    /// This receiver mode has no raw video or audio capture, because the
    /// synchronizer takes the receiver's video and audio: raw audio capture
    /// would take frames the FrameSync then replaces with silence (see
    /// [`FrameSyncAttached`]). Use the FrameSync capture methods, or recover
    /// the receiver with [`Self::into_receiver`] first.
    ///
    /// ```compile_fail,E0277
    /// use grafton_ndi::FrameSync;
    /// fn capture(framesync: &FrameSync) {
    ///     let _ = framesync.receiver().audio();
    /// }
    /// ```
    pub fn receiver(&self) -> &Receiver<FrameSyncAttached> {
        &self.receiver
    }

    /// Consume the FrameSync and recover the underlying Receiver.
    ///
    /// This destroys the frame synchronizer and returns the receiver for
    /// continued use with raw capture or for creating a new FrameSync.
    pub fn into_receiver(self) -> Receiver {
        let Self { resource, receiver } = self;
        drop(resource);
        receiver.into_mode()
    }

    /// Capture video with time-base correction, as an owned frame that can
    /// be sent across threads.
    ///
    /// This does not wait for a new network frame. It returns the best frame for
    /// the current output timing, handling clock drift and jitter automatically.
    /// The same frame may be returned multiple times when capture rate exceeds
    /// source frame rate. Wrapper synchronization and SDK processing still take time.
    /// [`Self::capture_video_ref`] borrows the frame instead of copying it.
    ///
    /// # Arguments
    ///
    /// * `scan_type` - The desired field format. Use `ScanType::Progressive` for
    ///   most applications. For interlaced output, use the appropriate field type
    ///   to maintain correct field ordering.
    ///
    /// # Returns
    ///
    /// * `Ok(Some(VideoFrame))` - An owned copy of the captured frame
    /// * `Ok(None)` - The SDK returned the documented all-zero "no video yet" state
    ///
    /// # Errors
    ///
    /// As for [`Self::capture_video_ref`]. The owned copy itself cannot
    /// fail.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, ReceiverOptions, Receiver, FrameSync, Source, SourceAddress, ScanType};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let source = Source::new("Test");
    /// # let options = ReceiverOptions::new(source);
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let framesync = FrameSync::new(receiver)?;
    ///
    /// // Capture loop - call at your output frame rate
    /// loop {
    ///     if let Some(frame) = framesync.capture_video(ScanType::Progressive)? {
    ///         // Process/display frame
    ///         let rate = frame.frame_rate();
    ///         println!("{}x{} @ {}/{} fps",
    ///             frame.width(), frame.height(),
    ///             rate.numerator(), rate.denominator());
    ///     } else {
    ///         // No video received yet - display placeholder
    ///         println!("Waiting for video...");
    ///     }
    ///     # break;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn capture_video(&self, scan_type: ScanType) -> Result<Option<VideoFrame>> {
        Ok(self
            .capture_video_ref(scan_type)?
            .map(|frame| frame.to_owned()))
    }

    /// Capture video with time-base correction, borrowing the SDK's frame
    /// without copying it.
    ///
    /// Timing is as for [`Self::capture_video`].
    ///
    /// # Returns
    ///
    /// * `Ok(Some(FrameSyncVideoRef))` - A validated zero-copy output frame; it
    ///   may repeat or retain earlier input and does not prove new reception
    /// * `Ok(None)` - The SDK returned the documented all-zero "no video yet" state
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] when the SDK returned an unsupported
    /// format or malformed frame descriptor, including invalid dimensions,
    /// stride, buffers, or metadata.
    pub fn capture_video_ref(&self, scan_type: ScanType) -> Result<Option<FrameSyncVideoRef<'_>>> {
        // Per SDK docs: a zeroed struct if no video has been received yet.
        let Some(guard) = self.resource.capture_video(scan_type) else {
            return Ok(None);
        };
        // SAFETY: the guard owns a frame this capture just returned.
        let frame = unsafe { FrameSyncVideoRef::new(guard)? };

        Ok(Some(frame))
    }

    /// Capture audio with dynamic resampling, as an owned frame that can be
    /// sent across threads.
    ///
    /// This does not wait for new network audio. Capture requests ask the NDI
    /// SDK to resample incoming audio to match the requested sample rate,
    /// channel count, and sample count. Wrapper synchronization and resampling
    /// still take time. [`Self::capture_audio_ref`] borrows the samples
    /// instead of copying them.
    ///
    /// Call this at the rate you need audio - the SDK will adapt the incoming
    /// signal to match your output timing using dynamic audio sampling.
    ///
    /// # Arguments
    ///
    /// * `request` - The output sample rate, channels and samples per
    ///   channel; see [`FrameSyncAudioRequest`].
    ///
    /// # Returns
    ///
    /// An owned copy of the captured samples, in the request's channels and
    /// samples per channel. The SDK can supply silence whenever current
    /// input is unavailable. To read the SDK's known source format, use
    /// [`Self::audio_input_format`].
    ///
    /// # Errors
    ///
    /// As for [`Self::capture_audio_ref`].
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, ReceiverOptions, Receiver, FrameSync, FrameSyncAudioRequest, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let source = Source::new("Test");
    /// # let options = ReceiverOptions::new(source);
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let framesync = FrameSync::new(receiver)?;
    ///
    /// // Audio callback - called by sound card at its rate
    /// let request = FrameSyncAudioRequest::capture_at(48_000, 2, 1_024)?;
    ///
    /// loop {
    ///     // Request 1024 stereo samples at 48kHz
    ///     let audio = framesync.capture_audio(request)?;
    ///     // Process each channel.
    ///     for samples in audio.channels() {
    ///         println!("Got {} samples in a channel", samples.len());
    ///     }
    ///     # break;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn capture_audio(&self, request: FrameSyncAudioRequest) -> Result<AudioFrame> {
        Ok(self.capture_audio_ref(request)?.to_owned())
    }

    /// Capture audio with dynamic resampling, borrowing the SDK's samples
    /// without copying them.
    ///
    /// Resampling is as for [`Self::capture_audio`]. The frame always holds
    /// the request's channels and samples per channel; the SDK can supply
    /// silence whenever current input is unavailable, including after input
    /// previously arrived. Successful output does not prove fresh reception.
    /// See the [reception monitoring guide](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/usage.md#monitor-reception-of-the-expected-media).
    ///
    /// # Following the source's format
    ///
    /// A request names its sample rate and channel count explicitly. To
    /// capture in the source's format, wait for [`Self::audio_input_format`]
    /// to return `Some`, then build a request with
    /// [`FrameSyncAudioRequest::for_input`].
    ///
    /// # Errors
    ///
    /// Returns frame-validation errors only if the SDK returns malformed audio
    /// dimensions, sample layout, buffers, or metadata. A capture before any
    /// audio has arrived is not an error. Request dimensions were already
    /// validated at construction.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, ReceiverOptions, Receiver, FrameSync, FrameSyncAudioRequest, Source, SourceAddress};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let source = Source::new("Test");
    /// # let options = ReceiverOptions::new(source);
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let framesync = FrameSync::new(receiver)?;
    ///
    /// let format = loop {
    ///     match framesync.audio_input_format()? {
    ///         Some(format) => break format,
    ///         None => std::thread::sleep(Duration::from_millis(10)),
    ///     }
    /// };
    /// let request = FrameSyncAudioRequest::for_input(format, 1_024)?;
    ///
    /// let audio = framesync.capture_audio_ref(request)?;
    /// println!("{} channels at {} Hz", audio.num_channels(), audio.sample_rate());
    /// # Ok(())
    /// # }
    /// ```
    pub fn capture_audio_ref(
        &self,
        request: FrameSyncAudioRequest,
    ) -> Result<FrameSyncAudioRef<'_>> {
        let guard = self.resource.capture_audio(AudioCall::Samples(request));
        // SAFETY: the guard owns the samples this capture request just
        // returned.
        unsafe { FrameSyncAudioRef::new(guard) }
    }

    /// Query the format of the audio this FrameSync is receiving, without
    /// capturing samples.
    ///
    /// This makes the SDK's input-format query: one native call that does not
    /// wait for network audio and frees what the SDK returned before
    /// returning. It allocates nothing unless it fails. Like the other
    /// FrameSync methods it may be called from any thread.
    ///
    /// # Returns
    ///
    /// * `Ok(Some(format))` - The SDK's known source sample rate and channel
    ///   count, both positive. This knowledge may persist across starvation,
    ///   disconnect, reconnect or source changes; it is not a freshness signal.
    /// * `Ok(None)` - The SDK reports no known audio format
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] when the SDK's answer is neither of
    /// those: samples, a data pointer or channel stride, a negative value, a
    /// zero sample rate with a positive channel count or the reverse, or an
    /// unsupported FourCC.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, ReceiverOptions, Receiver, FrameSync, Source};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("Test")))?;
    /// let framesync = FrameSync::new(receiver)?;
    ///
    /// match framesync.audio_input_format()? {
    ///     Some(format) => println!(
    ///         "{} channels at {} Hz",
    ///         format.num_channels(),
    ///         format.sample_rate()
    ///     ),
    ///     None => println!("No audio yet"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn audio_input_format(&self) -> Result<Option<AudioInputFormat>> {
        // The guard frees what the query returned when it is dropped here.
        let guard = self.resource.capture_audio(AudioCall::InputFormat);
        input_format(guard.frame())
    }

    /// Query the current audio queue depth.
    ///
    /// Returns the approximate number of audio samples currently buffered.
    /// This can be useful when using an inaccurate timer for audio playback.
    ///
    /// **Note:** This value may change immediately after being read as new
    /// samples are continuously received, and the SDK reads it without the
    /// lock its captures take, so a capture on another thread may be under
    /// way. For most applications, simply call `capture_audio` at your desired
    /// rate and let the SDK handle timing.
    /// A positive depth does not establish fresh input; see the
    /// [reception monitoring guide](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/usage.md#monitor-reception-of-the-expected-media).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, ReceiverOptions, Receiver, FrameSync, Source, SourceAddress};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let source = Source::new("Test");
    /// # let options = ReceiverOptions::new(source);
    /// # let receiver = Receiver::new(&ndi, &options)?;
    /// let framesync = FrameSync::new(receiver)?;
    ///
    /// // Check available samples before capture
    /// let available = framesync.audio_queue_depth();
    /// println!("Audio samples available: {}", available);
    /// # Ok(())
    /// # }
    /// ```
    pub fn audio_queue_depth(&self) -> u32 {
        // SAFETY: the FrameSync instance is live for this borrow.
        let depth = unsafe {
            self.core()
                .NDIlib_framesync_audio_queue_depth(self.resource.instance)
        };
        // A sample count is never negative; one that broke that contract
        // would mean no samples to request.
        u32::try_from(depth).unwrap_or(0)
    }

    /// The entry points of the runtime this FrameSync was created through.
    fn core(&self) -> &'static crate::sys::Core {
        &self.resource._ndi.api().core
    }
}

/// # Safety
///
/// The FrameSync instance has no affinity to the thread that created it: the
/// SDK's own FrameSync audio examples create it on one thread and capture
/// from an audio device's callback thread. The FrameSync owns its receiver,
/// keeping the SDK runtime alive. Borrowed frame guards cannot be moved
/// between threads and keep this owner borrowed until their buffers are
/// freed; destruction requires exclusive ownership.
unsafe impl Send for FrameSync {}

/// # Safety
///
/// Every `&self` method may run on several threads at once: video and audio
/// capture, the input-format query, the queue depth, and the frees of
/// borrowed frames. Neither `Processing.NDI.FrameSync.h` nor the SDK
/// documentation of NDI 6.1.1, 6.3.2 or 6.4 says whether that is safe (the
/// one threading statement, in `Processing.NDI.Recv.h`, covers receiver
/// capture), so this rests on measurement, recorded in docs/sdk-validation.md:
///
/// - In the Linux x86_64 runtimes of all three versions, video capture, audio
///   capture (and with it the input-format query) and the video free take the
///   instance's exclusive lock, and the audio free returns its buffer through
///   atomic operations. The queue depth is read without the lock, so it is
///   approximate, which [`FrameSync::audio_queue_depth`] documents.
/// - Under helgrind, four threads making all of these calls on one FrameSync
///   of a live source show no conflict between two of them other than those
///   two lock-free paths, on 6.3.2 and 6.4.
/// - Sixteen threads making them for 120 s on each of 6.1.1, 6.3.2 and 6.4
///   receive no torn video frame, no invalid audio and no error.
///
/// `one_framesync_serves_captures_from_several_threads_at_once` in
/// tests/framesync_capture_boundary.rs repeats the last check briefly with
/// every runtime the tests run against. Receiver attachment disables raw
/// video and audio capture until the synchronizer is destroyed, including
/// when the receiver is accessed through [`FrameSync::receiver`].
unsafe impl Sync for FrameSync {}

impl fmt::Debug for FrameSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSync")
            .field("instance", &self.resource.instance)
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
        // SAFETY: both members of the descriptor's union are `int`s at the
        // same offset, so reading either one reads the four bytes the SDK
        // wrote as a valid `i32`. The SDK's zeroed "no video" state has zero
        // there whichever member `Processing.NDI.structs.h` makes it.
        && unsafe { frame.__bindgen_anon_1.data_size_in_bytes } == 0
        && frame.p_metadata.is_null()
        && frame.timestamp == 0
}

/// The answer to an input-format query: the format, or `None` when no source
/// format is known. Samples in the answer, or any other shape
/// [`validate_returned_audio`] rejects, are an error. The metadata pointer is
/// not read: the answer exposes no metadata.
fn input_format(returned: &NDIlib_audio_frame_v3_t) -> Result<Option<AudioInputFormat>> {
    match validate_returned_audio(returned)? {
        ReturnedAudio::Empty(format) => Ok(format),
        ReturnedAudio::Samples(layout) => Err(Error::InvalidFrame(format!(
            "FrameSync audio input-format query returned {} channels of {} samples",
            layout.no_channels, layout.no_samples
        ))),
    }
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
    use crate::{
        frames::{AudioFormat, PixelFormat},
        FourCc,
    };
    use std::ptr;

    #[test]
    fn receiver_modes_preserve_resource_order_and_exactly_once_destruction() {
        use crate::receiver::lifecycle_test_events::track;
        let Some(runtime) = crate::test_support::ndi() else {
            return;
        };
        for recover in [false, true] {
            let events = track(|| {
                let ndi = runtime.clone();
                let mut receiver =
                    Receiver::new(&ndi, &crate::ReceiverOptions::new(crate::Source::new("")))
                        .unwrap();
                let original = receiver.core.instance;
                drop(ndi);
                for _ in 0..3 {
                    let fs = FrameSync::new(receiver).unwrap();
                    assert_eq!(fs.receiver().core.instance, original);
                    receiver = fs.into_receiver();
                    assert_eq!(receiver.core.instance, original);
                    assert!(receiver.video().capture(std::time::Duration::ZERO).is_ok());
                }
                let fs = FrameSync::new(receiver).unwrap();
                if recover {
                    drop(fs.into_receiver());
                } else {
                    drop(fs);
                }
            });
            assert_eq!(
                events,
                [
                    "framesync",
                    "framesync",
                    "framesync",
                    "framesync",
                    "receiver"
                ]
            );
        }
        let events = track(|| {
            let ndi = runtime.clone();
            let receiver =
                Receiver::new(&ndi, &crate::ReceiverOptions::new(crate::Source::new(""))).unwrap();
            drop(ndi);
            let rejected = match FrameSync::new_with(receiver, |_| ptr::null_mut()) {
                Err(rejected) => rejected,
                Ok(_) => panic!("a null instance must be rejected"),
            };
            assert!(matches!(
                rejected.error(),
                Error::NativeOperationFailed {
                    operation: "NDIlib_framesync_create"
                }
            ));
            // The receiver comes back usable, and is dropped exactly once.
            let receiver = rejected.into_inner();
            assert!(receiver.video().capture(std::time::Duration::ZERO).is_ok());
        });
        assert_eq!(events, ["receiver"]);
    }

    #[test]
    fn audio_requests_reject_invalid_rates_and_dimensions_at_construction() {
        for rate in [0, 1 << 31, u32::MAX] {
            assert!(FrameSyncAudioRequest::capture_at(rate, 2, 1024).is_err());
        }
        assert!(FrameSyncAudioRequest::capture_at(i32::MAX.unsigned_abs(), 2, 1024).is_ok());
        for (channels, samples) in [
            (0, 1024),
            (u32::MAX, 1024),
            (2, 0),
            (2, u32::MAX),
            (0, 0),
            (2, 8_388_609),
            (2, 536_870_912),
            (1, 1_073_741_825),
            (i32::MAX.unsigned_abs(), 1),
            (u32::MAX, u32::MAX),
        ] {
            assert!(FrameSyncAudioRequest::capture_at(48_000, channels, samples).is_err());
        }
        // Exact cap, with no sample allocation or native call.
        assert!(FrameSyncAudioRequest::capture_at(48_000, 2, 8_388_608).is_ok());
        assert!(FrameSyncAudioRequest::capture_at(48_000, 1, 16_777_216).is_ok());
        assert!(FrameSyncAudioRequest::capture_at(48_000, 1, 16_777_217).is_err());
        let format = AudioInputFormat::new(
            NonZeroU32::new(44_100).unwrap(),
            NonZeroU32::new(6).unwrap(),
        );
        assert_eq!(
            FrameSyncAudioRequest::for_input(format, 480).unwrap(),
            FrameSyncAudioRequest::capture_at(44_100, 6, 480).unwrap()
        );
        assert!(FrameSyncAudioRequest::for_input(format, 0).is_err());
        assert!(FrameSyncAudioRequest::for_input(format, 536_870_912).is_err());
    }

    #[test]
    fn audio_request_reuse_and_rejection_at_both_sdk_entry_points() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver =
            Receiver::new(&ndi, &crate::ReceiverOptions::new(crate::Source::new(""))).unwrap();
        let fs = FrameSync::new(receiver).unwrap();
        audio_test_calls::take();
        for (channels, samples) in [
            (2, 8_388_609),
            (2, 536_870_912),
            (1, 1_073_741_825),
            (i32::MAX.unsigned_abs(), 1),
        ] {
            assert!(FrameSyncAudioRequest::capture_at(48_000, channels, samples)
                .and_then(|request| fs.capture_audio_ref(request).map(drop))
                .is_err());
            assert!(FrameSyncAudioRequest::capture_at(48_000, channels, samples)
                .and_then(|request| fs.capture_audio(request))
                .is_err());
            assert_eq!(audio_test_calls::take(), (0, 0));
        }

        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..1000 {
                std::hint::black_box(
                    FrameSyncAudioRequest::capture_at(48_000, 2, 8_388_608).unwrap(),
                );
            }
        });
        assert_eq!(allocations, 0);
        let (request, allocations) = crate::test_alloc::allocation_count(|| {
            FrameSyncAudioRequest::capture_at(48_000, 2, 1024).unwrap()
        });
        assert_eq!(allocations, 0);
        for _ in 0..3 {
            let ((), allocations) = crate::test_alloc::allocation_count(|| {
                let frame = fs.capture_audio_ref(std::hint::black_box(request)).unwrap();
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
            let owned = fs.capture_audio(request).unwrap();
            assert_eq!((owned.num_channels(), owned.num_samples()), (2, 1024));
            assert_eq!(audio_test_calls::take(), (1, 1));
        }

        // No audio has arrived: a request at any explicit shape still gets
        // silence in exactly that shape from both entry points, and each
        // capture frees the SDK descriptor exactly once.
        let shaped = FrameSyncAudioRequest::capture_at(44_100, 6, 480).unwrap();
        let frame = fs.capture_audio_ref(shaped).unwrap();
        assert_eq!(
            (
                frame.sample_rate(),
                frame.num_channels(),
                frame.num_samples()
            ),
            (44_100, 6, 480)
        );
        assert_eq!(frame.channels().count(), 6);
        assert!(frame
            .channels()
            .all(|channel| channel.len() == 480 && channel.iter().all(|&s| s == 0.0)));
        drop(frame);
        assert_eq!(audio_test_calls::take(), (1, 1));
        let owned = fs.capture_audio(shaped).unwrap();
        assert_eq!(
            (
                owned.sample_rate(),
                owned.num_channels(),
                owned.num_samples()
            ),
            (44_100, 6, 480)
        );
        assert_eq!(audio_test_calls::take(), (1, 1));

        // The input-format query reports that no source format is known, frees
        // what the SDK returned exactly once, and allocates nothing.
        for _ in 0..3 {
            let (format, allocations) =
                crate::test_alloc::allocation_count(|| fs.audio_input_format());
            assert_eq!(format.unwrap(), None);
            assert_eq!(allocations, 0);
            assert_eq!(audio_test_calls::take(), (1, 1));
        }
    }

    #[test]
    fn test_framesync_size() {
        // FrameSync contains an opaque pointer, its runtime handle and the owned Receiver
        assert_eq!(
            std::mem::size_of::<FrameSync>(),
            std::mem::size_of::<NDIlib_framesync_instance_t>()
                + std::mem::size_of::<crate::Ndi>()
                + std::mem::size_of::<Receiver>()
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
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: metadata.as_mut_ptr().cast(),
            timestamp: 0,
        };

        let guard =
            unsafe { Guard::<FrameSyncVideoFree>::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let frame = unsafe { FrameSyncVideoRef::new(guard) }.expect("valid video frame");

        assert_eq!(frame.metadata(), Some("framesync video"));
        assert_eq!(frame.data().unwrap().len(), expected_len);
        assert_eq!(frame.line_stride_bytes(), stride as usize);
        let owned = frame.to_owned();
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
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 0,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride,
            },
            p_metadata: metadata.as_mut_ptr().cast(),
            timestamp: 0,
        };

        let guard =
            unsafe { Guard::<FrameSyncVideoFree>::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
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
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            timecode: 0,
            p_data: ptr::null_mut(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 16 * 4,
            },
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        let guard =
            unsafe { Guard::<FrameSyncVideoFree>::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let result = unsafe { FrameSyncVideoRef::new(guard) };
        assert!(matches!(result, Err(Error::InvalidFrame(_))));
    }

    /// The answer to an input-format query maps to `None` for the no-source
    /// state and to the format otherwise; samples in the answer are refused.
    #[test]
    fn input_format_maps_no_source_and_format_states() {
        let fltp = NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP;
        // No source yet, with the FourCC unset or set.
        let no_source = NDIlib_audio_frame_v3_t::default();
        assert_eq!(input_format(&no_source).unwrap(), None);
        let no_source_fltp = NDIlib_audio_frame_v3_t {
            FourCC: fltp,
            ..no_source
        };
        assert_eq!(input_format(&no_source_fltp).unwrap(), None);

        let format = NDIlib_audio_frame_v3_t {
            sample_rate: 48_000,
            no_channels: 2,
            FourCC: fltp,
            ..no_source
        };
        let format = input_format(&format).unwrap().expect("a reported format");
        assert_eq!((format.sample_rate(), format.num_channels()), (48_000, 2));
        let widest = NDIlib_audio_frame_v3_t {
            sample_rate: i32::MAX,
            no_channels: i32::MAX,
            FourCC: fltp,
            ..no_source
        };
        let widest = input_format(&widest).unwrap().expect("a reported format");
        assert_eq!(widest.sample_rate(), i32::MAX.unsigned_abs());
        assert_eq!(widest.num_channels(), i32::MAX.unsigned_abs());
        assert!(FrameSyncAudioRequest::capture_at(widest.sample_rate(), 2, 1).is_ok());

        // Well-formed samples are still not an answer to a query.
        let mut samples = [0.0f32; 8];
        let with_samples = NDIlib_audio_frame_v3_t {
            sample_rate: 48_000,
            no_channels: 2,
            no_samples: 4,
            FourCC: fltp,
            p_data: samples.as_mut_ptr().cast(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 16,
            },
            ..no_source
        };
        match input_format(&with_samples) {
            Err(Error::InvalidFrame(message)) => {
                assert!(
                    message.contains("query returned 2 channels of 4 samples"),
                    "{message}"
                )
            }
            other => panic!("expected InvalidFrame, got {other:?}"),
        }
        // A partial state is neither answer.
        for (sample_rate, no_channels) in [(48_000, 0), (0, 2), (-1, 2), (48_000, -1)] {
            let partial = NDIlib_audio_frame_v3_t {
                sample_rate,
                no_channels,
                FourCC: fltp,
                ..no_source
            };
            assert!(matches!(
                input_format(&partial),
                Err(Error::InvalidFrame(_))
            ));
        }
    }

    /// A capture always holds samples: neither SDK state without samples is a
    /// FrameSync audio frame.
    #[test]
    fn test_framesync_audio_ref_capture_rejects_empty() {
        let no_source = NDIlib_audio_frame_v3_t::default();
        let format_only = NDIlib_audio_frame_v3_t {
            sample_rate: 48_000,
            no_channels: 2,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            ..no_source
        };
        for raw in [no_source, format_only] {
            let guard = unsafe {
                Guard::<FrameSyncAudioFree>::with_test_free(ptr::null_mut(), raw, |_, _| {})
            };
            let result = unsafe { FrameSyncAudioRef::new(guard) };

            assert!(matches!(result, Err(Error::InvalidFrame(_))));
        }
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

        let guard =
            unsafe { Guard::<FrameSyncAudioFree>::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let frame = unsafe { FrameSyncAudioRef::new(guard) }.expect("strided audio frame");

        assert_eq!(frame.metadata(), Some("framesync audio"));
        assert!(frame.data().is_none());
        assert_eq!(frame.format(), AudioFormat::Fltp);
        assert_eq!(frame.channel_data(0), Some(&[1.0, 2.0, 3.0, 4.0][..]));
        assert_eq!(frame.channel_data(1), Some(&[10.0, 20.0, 30.0, 40.0][..]));
        assert_eq!(frame.channel_stride_bytes(), stride_samples as usize * 4);
        assert!(format!("{frame:?}").contains("FrameSyncAudioRef"));

        let owned = frame.to_owned();
        assert_eq!(owned.metadata(), Some("framesync audio"));
        assert_eq!(owned.data().len(), 8);
        assert_eq!(owned.channel_stride_bytes(), 16);
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

        let guard =
            unsafe { Guard::<FrameSyncAudioFree>::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        assert!(matches!(
            unsafe { FrameSyncAudioRef::new(guard) },
            Err(Error::InvalidUtf8(_))
        ));
    }
}
