//! Audio matched to video timestamps, and clocks locked to a reference source.

use std::{cell::Cell, marker::PhantomData, ptr};

use super::{invalid_input, sdk_quantity, AdvancedConfig};
use crate::{
    finder::RawSource,
    frames::{validate_audio_layout_allow_empty, validate_frame_metadata, MAX_AUDIO_BYTES},
    sys::{
        advanced::{
            native, AvSync as AvSyncTable, AvSyncHandle, Genlock as GenlockTable, GenlockHandle,
        },
        bindings::*,
    },
    AudioFrame, Capture, ConnectionStats, Error, FrameRate, Ndi, Receiver, Rejected, Result,
    ScanType, Source, Tally, VideoFrame, VideoKind,
};

use super::descriptors;

/// What audio an [`AvSync::synchronize`] call asks for.
///
/// AVSync selects the samples that belong to a video frame; it does not
/// resample. A request that names a sample rate states the rate its sample
/// count was computed for, and a source at another rate is reported as
/// [`AvSyncOutcome::FormatChanged`] rather than given audio. The audio always
/// has the source's own channels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AvSyncRequest {
    // Each in 1..=i32::MAX when stated.
    sample_rate: Option<u32>,
    num_samples: Option<u32>,
}

impl AvSyncRequest {
    /// Take whatever audio belongs to the video frame, at the source's rate.
    pub fn automatic() -> Self {
        Self::default()
    }

    /// State the sample rate a request is computed for, and ask for an exact
    /// number of samples per channel. Each `None` leaves that choice to the
    /// SDK.
    ///
    /// With a video frame, the SDK returns the number asked for when it is
    /// close to the audio that belongs to the frame; otherwise it returns
    /// that audio and reports the count as adjusted.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when a value is zero, exceeds `i32::MAX`, or
    /// asks for more audio than one frame can hold.
    pub fn new(sample_rate: Option<u32>, num_samples: Option<u32>) -> Result<Self> {
        let request = Self {
            sample_rate: sample_rate
                .map(|rate| quantity("AVSync sample rate", rate))
                .transpose()?,
            num_samples: num_samples
                .map(|samples| quantity("AVSync sample count", samples))
                .transpose()?,
        };
        // A frame has at least one channel.
        if request.num_samples.is_some_and(|samples| {
            (samples as usize)
                .checked_mul(size_of::<f32>())
                .is_none_or(|bytes| bytes > MAX_AUDIO_BYTES)
        }) {
            return Err(invalid_input(
                "AVSync request exceeds the supported audio storage limit",
            ));
        }
        Ok(request)
    }

    /// The sample rate in hertz the request was computed for, if stated.
    pub fn sample_rate(self) -> Option<u32> {
        self.sample_rate
    }

    /// The requested samples per channel, if any.
    pub fn num_samples(self) -> Option<u32> {
        self.num_samples
    }

    /// The descriptor the SDK reads the request from and writes the audio to.
    ///
    /// The SDK reads zero as "not specified". It returns planar
    /// floating-point audio and leaves the format as it found it, so the
    /// descriptor must already name that format.
    fn raw(self) -> NDIlib_audio_frame_v3_t {
        // Lossless: construction keeps both at or below i32::MAX.
        let field = |value: Option<u32>| value.map_or(0, |value| value as i32);
        NDIlib_audio_frame_v3_t {
            sample_rate: field(self.sample_rate),
            no_samples: field(self.num_samples),
            ..descriptors::audio_frame()
        }
    }
}

/// The result of one [`AvSync::synchronize`] call.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AvSyncOutcome {
    /// The audio that belongs to the video frame, owned by the caller.
    Audio {
        /// The audio.
        frame: AudioFrame,
        /// The SDK returned a different number of samples than requested, to
        /// keep a source whose audio and video clocks drift in sync.
        samples_adjusted: bool,
    },
    /// The synchronization succeeded and there were no samples to return.
    Empty {
        /// As for [`Audio`](Self::Audio).
        samples_adjusted: bool,
    },
    /// The source's audio is not at the sample rate the request stated.
    /// Nothing was returned; ask again for the reported rate, or
    /// automatically.
    FormatChanged {
        /// The source's sample rate in hertz.
        sample_rate: u32,
        /// The source's channel count.
        num_channels: u32,
    },
    /// The source currently sends no audio.
    NoAudioStream,
    /// The source sends audio, but none of it matches the video frame's time.
    NoMatchingSamples,
    /// The SDK reported an internal error.
    InternalError,
    /// A result this crate does not know, with its native value, which can
    /// be negative as the SDK's failure codes are.
    Unknown(i32),
}

/// How the SDK classified one synchronization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Synchronized {
    Matched,
    Adjusted,
    NoAudioStream,
    NoMatchingSamples,
    FormatChanged,
    InternalError,
    Unknown(i32),
}

/// A native synchronizer, destroyed when dropped.
struct AvResource {
    handle: AvSyncHandle,
    table: &'static AvSyncTable,
}

impl Drop for AvResource {
    fn drop(&mut self) {
        // SAFETY: the handle was created by this table and is destroyed once,
        // while the receiver it reads from is still alive.
        native!(AvSync, self.table => unsafe { self.table.NDIlib_avsync_destroy(self.handle) });
    }
}

/// Audio matched to the timestamps of a receiver's video.
///
/// The synchronizer takes over the receiver's audio, so it owns the receiver:
/// capture video through [`receiver`](Self::receiver), then ask
/// [`synchronize`](Self::synchronize) for the audio that belongs to each
/// frame. [`into_receiver`](Self::into_receiver) returns the receiver.
///
/// # Examples
///
/// ```no_run
/// use std::time::Duration;
/// use grafton_ndi::{
///     advanced::{AvSync, AvSyncOutcome, AvSyncRequest},
///     Receiver, ReceiverOptions, Source, Ndi,
/// };
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = Ndi::new()?;
/// # let source = Source::new("");
/// let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
/// let mut sync = AvSync::new(receiver)?;
///
/// if let Some(video) = sync.receiver().video().capture(Duration::from_secs(1))? {
///     if let AvSyncOutcome::Audio { frame, .. } =
///         sync.synchronize(Some(&video), AvSyncRequest::automatic())?
///     {
///         println!("{} samples belong to this frame", frame.num_samples());
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct AvSync {
    // Declared first so that it is destroyed before the receiver it reads.
    resource: AvResource,
    receiver: Receiver,
}

impl AvSync {
    /// Start synchronizing the audio of `receiver`.
    ///
    /// # Errors
    ///
    /// Returns the receiver, untouched, with
    /// [`Error::UnsupportedCapability`] without an Advanced runtime (or in a
    /// build without the Advanced SDK), or [`Error::NativeOperationFailed`]
    /// when the SDK creates no synchronizer.
    pub fn new(receiver: Receiver) -> std::result::Result<Self, Rejected<Receiver>> {
        let table = match receiver.core._ndi.table::<AvSyncTable>() {
            Ok(table) => table,
            Err(error) => return Err(Rejected::new(error, receiver)),
        };
        // SAFETY: the receiver instance is live, and this value keeps it
        // alive for longer than the synchronizer.
        let handle: AvSyncHandle = native!(AvSync, table => unsafe {
            table.NDIlib_avsync_create(receiver.core.instance)
        });
        if handle.is_null() {
            return Err(Rejected::new(
                Error::NativeOperationFailed {
                    operation: "NDIlib_avsync_create",
                },
                receiver,
            ));
        }
        Ok(Self {
            resource: AvResource { handle, table },
            receiver,
        })
    }

    /// The receiver's video and controls. Its audio belongs to the
    /// synchronizer and is not reachable here.
    pub fn receiver(&self) -> AvSyncReceiver<'_> {
        AvSyncReceiver {
            receiver: &self.receiver,
        }
    }

    /// Stop synchronizing and return the receiver.
    pub fn into_receiver(self) -> Receiver {
        let Self { resource, receiver } = self;
        drop(resource);
        receiver
    }

    /// Fetch the audio that belongs to `video`, or with `None` whatever audio
    /// is currently queued.
    ///
    /// One call makes one native request: a changed format or missing audio
    /// is reported, never retried or converted.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when the audio the SDK returned fails
    /// validation, and [`Error::UnexpectedSdkResult`] when it reports a
    /// format change to a sample rate or channel count that is not positive.
    pub fn synchronize(
        &mut self,
        video: Option<&VideoFrame>,
        request: AvSyncRequest,
    ) -> Result<AvSyncOutcome> {
        let video = video.map(VideoFrame::to_raw);
        let AvResource { handle, table } = self.resource;
        let mut returned = Returned {
            audio: request.raw(),
            release: |audio: &mut NDIlib_audio_frame_v3_t| {
                // SAFETY: the synchronizer that filled the descriptor is live,
                // and the descriptor is returned to it exactly once.
                native!(AvSync, table => unsafe { table.NDIlib_avsync_free_audio(handle, audio) })
            },
        };
        let status = native!(AvSync, table => {
            // SAFETY: the synchronizer is live, the video descriptor (if any)
            // borrows a frame that outlives the call, and the audio
            // descriptor is writable.
            let code = unsafe {
                table.NDIlib_avsync_synchronize(
                    handle,
                    video.as_ref().map_or(ptr::null(), |video| video),
                    &mut returned.audio,
                )
            };
            match code {
                crate::sys::advanced_bindings::NDIlib_avsync_ret_e_NDIlib_avsync_ret_success => Synchronized::Matched,
                crate::sys::advanced_bindings::NDIlib_avsync_ret_e_NDIlib_avsync_ret_success_num_samples_not_matched => {
                    Synchronized::Adjusted
                }
                crate::sys::advanced_bindings::NDIlib_avsync_ret_e_NDIlib_avsync_ret_no_audio_stream_received => {
                    Synchronized::NoAudioStream
                }
                crate::sys::advanced_bindings::NDIlib_avsync_ret_e_NDIlib_avsync_ret_no_samples_found => {
                    Synchronized::NoMatchingSamples
                }
                crate::sys::advanced_bindings::NDIlib_avsync_ret_e_NDIlib_avsync_ret_format_changed => {
                    Synchronized::FormatChanged
                }
                crate::sys::advanced_bindings::NDIlib_avsync_ret_e_NDIlib_avsync_ret_internal_error => {
                    Synchronized::InternalError
                }
                other => Synchronized::Unknown(other),
            }
        });
        // SAFETY: the SDK owns whatever it returned until `returned` is
        // dropped, which happens after the copy.
        unsafe { outcome(status, &returned.audio) }
    }
}

/// The audio descriptor of one synchronization. Whatever the SDK allocated
/// for it is handed to `release` when the descriptor is dropped.
struct Returned<F: FnMut(&mut NDIlib_audio_frame_v3_t)> {
    audio: NDIlib_audio_frame_v3_t,
    release: F,
}

impl<F: FnMut(&mut NDIlib_audio_frame_v3_t)> Drop for Returned<F> {
    fn drop(&mut self) {
        // Every result, including the failures, can carry SDK allocations.
        if !self.audio.p_data.is_null() || !self.audio.p_metadata.is_null() {
            (self.release)(&mut self.audio);
        }
    }
}

/// Turn one synchronization into an owned result.
///
/// # Safety
///
/// For the two successful statuses, `audio` is a frame descriptor whose data
/// and metadata pointers are null or valid for the layout it describes.
unsafe fn outcome(status: Synchronized, audio: &NDIlib_audio_frame_v3_t) -> Result<AvSyncOutcome> {
    Ok(match status {
        Synchronized::Matched | Synchronized::Adjusted => {
            let samples_adjusted = status == Synchronized::Adjusted;
            let layout = validate_audio_layout_allow_empty(audio)?;
            if layout.is_empty() {
                AvSyncOutcome::Empty { samples_adjusted }
            } else {
                // SAFETY: forwarded to the caller's obligation.
                let frame = unsafe {
                    let metadata = validate_frame_metadata(audio.p_metadata)?;
                    AudioFrame::from_raw_validated(*audio, layout, metadata)?
                };
                AvSyncOutcome::Audio {
                    frame,
                    samples_adjusted,
                }
            }
        }
        Synchronized::NoAudioStream => AvSyncOutcome::NoAudioStream,
        Synchronized::NoMatchingSamples => AvSyncOutcome::NoMatchingSamples,
        Synchronized::FormatChanged => {
            match (
                sdk_quantity(audio.sample_rate),
                sdk_quantity(audio.no_channels),
            ) {
                (Some(sample_rate), Some(num_channels)) => AvSyncOutcome::FormatChanged {
                    sample_rate,
                    num_channels,
                },
                _ => {
                    return Err(Error::UnexpectedSdkResult(format!(
                    "NDIlib_avsync_synchronize reported a format change to {} Hz and {} channels",
                    audio.sample_rate, audio.no_channels
                )))
                }
            }
        }
        Synchronized::InternalError => AvSyncOutcome::InternalError,
        Synchronized::Unknown(code) => AvSyncOutcome::Unknown(code),
    })
}

/// The video and controls of a receiver that an [`AvSync`] owns.
///
/// There is deliberately no way to capture audio here: the synchronizer
/// consumes it.
///
/// ```compile_fail,E0599
/// use grafton_ndi::advanced::AvSync;
/// fn capture(sync: &AvSync) {
///     let _ = sync.receiver().audio();
/// }
/// ```
pub struct AvSyncReceiver<'a> {
    receiver: &'a Receiver,
}

impl AvSyncReceiver<'_> {
    /// Capture video; see [`Receiver::video`].
    pub fn video(&self) -> Capture<'_, VideoKind> {
        self.receiver.video()
    }

    /// The source the receiver was created for.
    pub fn source(&self) -> &Source {
        self.receiver.source()
    }

    /// Whether the receiver is connected to its source.
    pub fn is_connected(&self) -> bool {
        self.receiver.is_connected()
    }

    /// Connection and performance statistics; see
    /// [`Receiver::connection_stats`].
    pub fn connection_stats(&self) -> ConnectionStats {
        self.receiver.connection_stats()
    }

    /// Publish this receiver's tally state; see [`Receiver::set_tally`].
    pub fn set_tally(&self, tally: Tally) {
        self.receiver.set_tally(tally);
    }
}

/// What paced a completed [`VideoClock::wait`] or [`AudioClock::wait`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ClockSource {
    /// The reference source's clock.
    ReferenceSignal,
    /// The local system clock, because no reference signal is being received.
    SystemClock,
}

impl ClockSource {
    fn from_active(active: bool) -> Self {
        if active {
            Self::ReferenceSignal
        } else {
            Self::SystemClock
        }
    }
}

/// A native genlock, destroyed when dropped.
struct GenResource {
    handle: GenlockHandle,
    table: &'static GenlockTable,
    _ndi: Ndi,
}

// SAFETY: the SDK allows the video wait and the audio wait to run on separate
// threads at the same time. Those are the only operations reachable through a
// shared reference: one non-cloneable token exists for each, and connecting
// or destroying requires exclusive access to the owner.
unsafe impl Sync for GenResource {}

impl Drop for GenResource {
    fn drop(&mut self) {
        // SAFETY: the handle was created by this table and is destroyed once.
        native!(Genlock, self.table => unsafe { self.table.NDIlib_genlock_destroy(self.handle) });
    }
}

/// Clocks locked to the timing of a reference NDI source.
///
/// A genlock paces a sender so that its frames leave in step with another
/// source instead of the local clock: take the two clocks with
/// [`clocks`](Self::clocks), wait on the matching one before submitting each
/// frame, and create the sender with its own clocking disabled. While no
/// reference signal arrives, the waits fall back to the system clock and say
/// so.
///
/// The waits block their thread and cannot be cancelled or given a deadline.
///
/// # Examples
///
/// ```no_run
/// use grafton_ndi::{advanced::Genlock, FrameRate, Ndi, ScanType, Source};
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = Ndi::new()?;
/// # let reference = Source::new("");
/// let mut genlock = Genlock::new(&ndi, Some(&reference), None)?;
/// let (mut video, _audio) = genlock.clocks();
/// let paced_by = video.wait(FrameRate::new(60_000, 1001)?, ScanType::Progressive);
/// println!("frame paced by {paced_by:?}");
/// # Ok(())
/// # }
/// ```
pub struct Genlock {
    // Declared first so that it is destroyed before the strings it was given.
    resource: GenResource,
    _source: Option<Box<RawSource>>,
    _config: Option<AdvancedConfig>,
}

impl Genlock {
    /// Create a genlock that follows `source`, or with `None` one that runs on
    /// the system clock until [`connect`](Self::connect) names a source.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] without an Advanced runtime (or in a
    /// build without the Advanced SDK), an invalid-string error for a source
    /// containing NUL, and [`Error::NativeOperationFailed`] when the SDK
    /// creates no genlock.
    pub fn new(
        ndi: &Ndi,
        source: Option<&Source>,
        config: Option<&AdvancedConfig>,
    ) -> Result<Self> {
        let table = ndi.table::<GenlockTable>()?;
        let source = source.map(Source::to_raw).transpose()?.map(Box::new);
        // SAFETY: the source descriptor and the configuration point to
        // strings that this value keeps alive for as long as the genlock.
        let handle: GenlockHandle = native!(Genlock, table => unsafe {
            table.NDIlib_genlock_create(
                source.as_ref().map_or(ptr::null(), |source| &source.raw),
                config.map_or(ptr::null(), AdvancedConfig::as_ptr),
            )
        });
        if handle.is_null() {
            return Err(Error::NativeOperationFailed {
                operation: "NDIlib_genlock_create",
            });
        }
        Ok(Self {
            resource: GenResource {
                handle,
                table,
                _ndi: ndi.clone(),
            },
            _source: source,
            _config: config.cloned(),
        })
    }

    /// Follow a different source, or with `None` run on the system clock.
    ///
    /// The exclusive borrow means no clock from [`clocks`](Self::clocks) is in
    /// use while the reference changes.
    ///
    /// # Errors
    ///
    /// An invalid-string error for a source containing NUL.
    pub fn connect(&mut self, source: Option<&Source>) -> Result<()> {
        let source = source.map(Source::to_raw).transpose()?.map(Box::new);
        let GenResource { handle, table, .. } = self.resource;
        // SAFETY: the genlock is live and no wait is in progress, and the new
        // descriptor is stored below before the earlier one is released.
        native!(Genlock, table => unsafe {
            table.NDIlib_genlock_connect(
                handle,
                source.as_ref().map_or(ptr::null(), |source| &source.raw),
            )
        });
        self._source = source;
        Ok(())
    }

    /// Whether a reference signal is being received.
    pub fn is_active(&self) -> bool {
        let GenResource { handle, table, .. } = self.resource;
        // SAFETY: the genlock is live for this borrow.
        native!(Genlock, table => unsafe { table.NDIlib_genlock_is_active(handle) })
    }

    /// The video clock and the audio clock.
    ///
    /// Each can be moved to its own thread; both borrow the genlock, so it can
    /// be neither reconnected nor dropped while they exist.
    pub fn clocks(&mut self) -> (VideoClock<'_>, AudioClock<'_>) {
        (
            VideoClock {
                resource: &self.resource,
                _not_sync: PhantomData,
            },
            AudioClock {
                resource: &self.resource,
                _not_sync: PhantomData,
            },
        )
    }
}

/// The video clock of a [`Genlock`].
pub struct VideoClock<'a> {
    resource: &'a GenResource,
    // One thread at a time waits on a clock.
    _not_sync: PhantomData<Cell<()>>,
}

impl VideoClock<'_> {
    /// Block until the next frame of a stream with this frame rate and scan
    /// type is due, and report which clock timed it.
    pub fn wait(&mut self, frame_rate: FrameRate, scan_type: ScanType) -> ClockSource {
        let timing = video_timing(frame_rate, scan_type);
        let GenResource { handle, table, .. } = *self.resource;
        // SAFETY: the genlock outlives this clock, and only this clock waits
        // for video.
        ClockSource::from_active(native!(Genlock, table => unsafe {
            table.NDIlib_genlock_wait_video(handle, &timing)
        }))
    }
}

/// The audio clock of a [`Genlock`].
pub struct AudioClock<'a> {
    resource: &'a GenResource,
    // One thread at a time waits on a clock.
    _not_sync: PhantomData<Cell<()>>,
}

impl AudioClock<'_> {
    /// Block until `num_samples` samples per channel at `sample_rate` hertz
    /// are due, and report which clock timed them.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when a value is zero or exceeds `i32::MAX`.
    pub fn wait(&mut self, sample_rate: u32, num_samples: u32) -> Result<ClockSource> {
        let timing = audio_timing(sample_rate, num_samples)?;
        let GenResource { handle, table, .. } = *self.resource;
        // SAFETY: the genlock outlives this clock, and only this clock waits
        // for audio.
        Ok(ClockSource::from_active(native!(Genlock, table => unsafe {
            table.NDIlib_genlock_wait_audio(handle, &timing)
        })))
    }
}

/// `value`, named `name` in the error, when it is in `1..=i32::MAX`, the
/// range of the SDK's signed fields.
///
/// # Errors
///
/// [`Error::InvalidInput`] otherwise.
fn quantity(name: &str, value: u32) -> Result<u32> {
    sdk_quantity(value)
        .ok_or_else(|| invalid_input(format!("{name} {value} is not in 1..={}", i32::MAX)))
}

/// The descriptor a video clock reads its timing from.
fn video_timing(frame_rate: FrameRate, scan_type: ScanType) -> NDIlib_video_frame_v2_t {
    let (numerator, denominator) = frame_rate.to_sdk();
    NDIlib_video_frame_v2_t {
        frame_rate_N: numerator,
        frame_rate_D: denominator,
        frame_format_type: scan_type.to_sdk(),
        ..descriptors::video_frame()
    }
}

/// The descriptor an audio clock reads its timing from.
fn audio_timing(sample_rate: u32, num_samples: u32) -> Result<NDIlib_audio_frame_v3_t> {
    // Lossless: each is at most i32::MAX.
    Ok(NDIlib_audio_frame_v3_t {
        sample_rate: quantity("Clock sample rate", sample_rate)? as i32,
        no_samples: quantity("Clock sample count", num_samples)? as i32,
        ..descriptors::audio_frame()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> NDIlib_audio_frame_v3_t {
        NDIlib_audio_frame_v3_t {
            p_data: ptr::dangling_mut::<u8>(),
            sample_rate: 48_000,
            no_channels: 2,
            no_samples: 1,
            ..Default::default()
        }
    }

    #[test]
    fn distinguishes_native_outcomes() {
        let audio = descriptor();
        // SAFETY: none of these statuses reads the descriptor's buffers.
        let classify = |status| unsafe { outcome(status, &audio) }.unwrap();
        assert!(matches!(
            classify(Synchronized::FormatChanged),
            AvSyncOutcome::FormatChanged {
                sample_rate: 48_000,
                num_channels: 2
            }
        ));
        for (sample_rate, no_channels) in [(0, 2), (48_000, -1), (-48_000, 2)] {
            let changed = NDIlib_audio_frame_v3_t {
                sample_rate,
                no_channels,
                ..audio
            };
            // SAFETY: a format change reads no buffer.
            assert!(matches!(
                unsafe { outcome(Synchronized::FormatChanged, &changed) },
                Err(Error::UnexpectedSdkResult(_))
            ));
        }
        assert!(matches!(
            classify(Synchronized::NoAudioStream),
            AvSyncOutcome::NoAudioStream
        ));
        assert!(matches!(
            classify(Synchronized::NoMatchingSamples),
            AvSyncOutcome::NoMatchingSamples
        ));
        assert!(matches!(
            classify(Synchronized::InternalError),
            AvSyncOutcome::InternalError
        ));
        assert!(matches!(
            classify(Synchronized::Unknown(99)),
            AvSyncOutcome::Unknown(99)
        ));

        let empty = NDIlib_audio_frame_v3_t {
            p_data: ptr::null_mut(),
            no_samples: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            ..audio
        };
        // SAFETY: an empty frame has no buffers to read.
        assert!(matches!(
            unsafe { outcome(Synchronized::Adjusted, &empty) }.unwrap(),
            AvSyncOutcome::Empty {
                samples_adjusted: true
            }
        ));
    }

    #[test]
    fn rejects_invalid_clock_and_request_values_before_native_wait() {
        let refused = |result: Result<AvSyncRequest>| {
            assert!(matches!(result, Err(Error::InvalidInput(_))), "{result:?}");
        };
        refused(AvSyncRequest::new(Some(0), None));
        refused(AvSyncRequest::new(None, Some(0)));
        refused(AvSyncRequest::new(Some(u32::MAX), None));
        refused(AvSyncRequest::new(Some(i32::MAX.unsigned_abs() + 1), None));
        let most = (MAX_AUDIO_BYTES / 4) as u32;
        assert!(AvSyncRequest::new(None, Some(most)).is_ok());
        refused(AvSyncRequest::new(None, Some(most + 1)));
        assert_eq!(AvSyncRequest::automatic().num_samples(), None);
        assert_eq!(AvSyncRequest::automatic().sample_rate(), None);
        let request = AvSyncRequest::new(Some(48_000), Some(1600)).unwrap();
        assert_eq!(
            (request.sample_rate(), request.num_samples()),
            (Some(48_000), Some(1600))
        );
        for (sample_rate, num_samples) in [(48_000, 0), (0, 1600), (u32::MAX, 1600)] {
            assert!(matches!(
                audio_timing(sample_rate, num_samples),
                Err(Error::InvalidInput(_))
            ));
        }
        let raw = video_timing(FrameRate::new(60_000, 1001).unwrap(), ScanType::Progressive);
        assert_eq!((raw.frame_rate_N, raw.frame_rate_D), (60_000, 1001));
        assert_eq!(ClockSource::from_active(false), ClockSource::SystemClock);
        fn send<T: Send>() {}
        send::<VideoClock<'_>>();
        send::<AudioClock<'_>>();
    }

    /// The SDK fills in the audio it returns but not its format, and reads
    /// the request from the same descriptor: every request starts as the
    /// descriptor the SDK itself would construct, with what it asks for set.
    #[test]
    fn a_request_is_the_sdk_default_descriptor_with_what_it_asks_for() {
        let planar_float = NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP;
        let automatic = AvSyncRequest::automatic().raw();
        assert_eq!(automatic.FourCC, planar_float);
        assert_eq!((automatic.no_samples, automatic.sample_rate), (0, 0));
        assert_eq!(automatic.timecode, NDIlib_send_timecode_synthesize);
        assert!(automatic.p_data.is_null() && automatic.p_metadata.is_null());

        let explicit = AvSyncRequest::new(Some(44_100), Some(1601)).unwrap().raw();
        assert_eq!(explicit.FourCC, planar_float);
        assert_eq!((explicit.no_samples, explicit.sample_rate), (1601, 44_100));

        // What the SDK returns for a successful request validates as audio.
        let returned = NDIlib_audio_frame_v3_t {
            no_samples: 0,
            sample_rate: 48_000,
            no_channels: 2,
            ..automatic
        };
        // SAFETY: an empty frame has no buffers to read.
        assert!(matches!(
            unsafe { outcome(Synchronized::Matched, &returned) }.unwrap(),
            AvSyncOutcome::Empty {
                samples_adjusted: false
            }
        ));

        // The clocks read a rate from descriptors built the same way.
        let video = video_timing(FrameRate::new(60_000, 1001).unwrap(), ScanType::Interlaced);
        assert_eq!(
            video.FourCC,
            NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVY
        );
        assert_eq!(
            video.frame_format_type,
            NDIlib_frame_format_type_e_NDIlib_frame_format_type_interleaved
        );
        let audio = audio_timing(48_000, 1600).unwrap();
        assert_eq!(audio.FourCC, planar_float);
        assert_eq!((audio.sample_rate, audio.no_samples), (48_000, 1600));
    }

    /// The SDK's allocations are returned for every result, including the
    /// failures and frames that fail validation.
    #[test]
    fn returned_audio_is_released_whatever_the_result() {
        let released = Cell::new(0);
        let returned = |audio| Returned {
            audio,
            release: |_: &mut NDIlib_audio_frame_v3_t| released.set(released.get() + 1),
        };
        for status in [
            Synchronized::FormatChanged,
            Synchronized::InternalError,
            Synchronized::Unknown(99),
            // One sample at a misaligned address fails validation.
            Synchronized::Matched,
        ] {
            let before = released.get();
            let audio = returned(descriptor());
            // SAFETY: validation rejects the descriptor before any buffer is read.
            let result = unsafe { outcome(status, &audio.audio) };
            assert_eq!(result.is_err(), status == Synchronized::Matched);
            drop(audio);
            assert_eq!(released.get(), before + 1);
        }

        // Nothing was allocated, so nothing is released.
        drop(returned(NDIlib_audio_frame_v3_t::default()));
        assert_eq!(released.get(), 4);
    }
}
