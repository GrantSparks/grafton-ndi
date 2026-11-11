//! Audio matched to video timestamps, and clocks locked to a reference source.

use std::{cell::Cell, marker::PhantomData, ptr};

use super::{invalid, AdvancedConfig, Rejected};
use crate::{
    finder::RawSource,
    frames::{validate_audio_layout_allow_empty, validate_frame_metadata},
    sys::{
        advanced::{
            native, AvSync as AvSyncTable, AvSyncHandle, Genlock as GenlockTable, GenlockHandle,
        },
        bindings::*,
    },
    AudioFrame, Capture, ConnectionStats, Error, Receiver, Result, ScanType, Source, Tally,
    VideoFrame, VideoKind, NDI,
};

/// What audio an [`AvSync::synchronize`] call asks for.
///
/// AVSync selects the samples that belong to a video frame; it does not
/// resample. Naming a sample rate or channel count states what the caller
/// expects, and a source with a different format is reported as
/// [`AvSyncOutcome::FormatChanged`] rather than converted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AvSyncRequest {
    samples: i32,
    sample_rate: i32,
    channels: i32,
}

impl AvSyncRequest {
    /// Take whatever audio belongs to the video frame, in the source's format.
    pub fn automatic() -> Self {
        Self::default()
    }

    /// Ask for an exact number of samples per channel, and state the expected
    /// sample rate and channel count. Each `None` leaves that choice to the SDK.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when a value is zero, exceeds the SDK's range,
    /// or asks for more audio than one frame can hold.
    pub fn new(
        samples: Option<u32>,
        sample_rate: Option<u32>,
        channels: Option<u32>,
    ) -> Result<Self> {
        // The SDK reads zero as "not specified".
        fn value(v: Option<u32>) -> Result<i32> {
            v.map(positive).transpose().map(|v| v.unwrap_or(0))
        }
        let request = Self {
            samples: value(samples)?,
            sample_rate: value(sample_rate)?,
            channels: value(channels)?,
        };
        if request.samples > 0 {
            let bytes = (request.samples as usize)
                .checked_mul(request.channels.max(1) as usize)
                .and_then(|n| n.checked_mul(size_of::<f32>()));
            if bytes.is_none_or(|n| n > crate::frames::MAX_AUDIO_BYTES) {
                return Err(invalid(
                    "AVSync request exceeds the supported audio storage limit",
                ));
            }
        }
        Ok(request)
    }

    /// The requested samples per channel, if any.
    pub fn samples(self) -> Option<u32> {
        u32::try_from(self.samples).ok().filter(|n| *n != 0)
    }

    /// The expected sample rate in hertz, if stated.
    pub fn sample_rate(self) -> Option<u32> {
        u32::try_from(self.sample_rate).ok().filter(|n| *n != 0)
    }

    /// The expected channel count, if stated.
    pub fn channels(self) -> Option<u32> {
        u32::try_from(self.channels).ok().filter(|n| *n != 0)
    }

    fn raw(self) -> NDIlib_audio_frame_v3_t {
        NDIlib_audio_frame_v3_t {
            no_samples: self.samples,
            sample_rate: self.sample_rate,
            no_channels: self.channels,
            ..Default::default()
        }
    }
}

/// The result of one [`AvSync::synchronize`] call.
#[derive(Debug)]
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
    /// The source's audio is not in the format the request stated. Nothing was
    /// returned; ask again with the reported format, or automatically.
    FormatChanged {
        /// The source's sample rate in hertz.
        sample_rate: i32,
        /// The source's channel count.
        channels: i32,
    },
    /// The source currently sends no audio.
    NoAudioStream,
    /// The source sends audio, but none of it matches the video frame's time.
    NoMatchingSamples,
    /// The SDK reported an internal error.
    InternalError,
    /// A result this crate does not know, with its native value.
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
        native!(self.table => unsafe { self.table.NDIlib_avsync_destroy(self.handle) });
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
///     Receiver, ReceiverOptions, Source, NDI,
/// };
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = NDI::new()?;
/// # let source = Source::default();
/// let receiver = Receiver::new(&ndi, &ReceiverOptions::builder(source).build())?;
/// let mut sync = AvSync::new(receiver)?;
///
/// let video = sync.receiver().video().capture(Duration::from_secs(1))?;
/// if let AvSyncOutcome::Audio { frame, .. } =
///     sync.synchronize(Some(&video), AvSyncRequest::automatic())?
/// {
///     println!("{} samples belong to this frame", frame.num_samples());
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
    /// build without the Advanced SDK), or [`Error::InitializationFailed`]
    /// when the SDK creates no synchronizer.
    pub fn new(receiver: Receiver) -> std::result::Result<Self, Rejected<Receiver>> {
        let table = match receiver.core._ndi.table::<AvSyncTable>() {
            Ok(table) => table,
            Err(error) => return Err(Rejected::new(error, receiver)),
        };
        // SAFETY: the receiver instance is live, and this value keeps it
        // alive for longer than the synchronizer.
        let handle: AvSyncHandle = native!(table => unsafe {
            table.NDIlib_avsync_create(receiver.core.instance)
        });
        if handle.is_null() {
            return Err(Rejected::new(
                Error::InitializationFailed("AVSync creation failed".into()),
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
    /// validation.
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
                native!(table => unsafe { table.NDIlib_avsync_free_audio(handle, audio) })
            },
        };
        let status = native!(table => {
            use crate::sys::advanced_bindings::*;
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
                NDIlib_avsync_ret_e_NDIlib_avsync_ret_success => Synchronized::Matched,
                NDIlib_avsync_ret_e_NDIlib_avsync_ret_success_num_samples_not_matched => {
                    Synchronized::Adjusted
                }
                NDIlib_avsync_ret_e_NDIlib_avsync_ret_no_audio_stream_received => {
                    Synchronized::NoAudioStream
                }
                NDIlib_avsync_ret_e_NDIlib_avsync_ret_no_samples_found => {
                    Synchronized::NoMatchingSamples
                }
                NDIlib_avsync_ret_e_NDIlib_avsync_ret_format_changed => {
                    Synchronized::FormatChanged
                }
                NDIlib_avsync_ret_e_NDIlib_avsync_ret_internal_error => {
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
        Synchronized::FormatChanged => AvSyncOutcome::FormatChanged {
            sample_rate: audio.sample_rate,
            channels: audio.no_channels,
        },
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
    pub fn set_tally(&self, tally: &Tally) -> bool {
        self.receiver.set_tally(tally)
    }
}

/// What paced a completed [`VideoClock::wait`] or [`AudioClock::wait`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    _ndi: NDI,
}

// SAFETY: the SDK allows the video wait and the audio wait to run on separate
// threads at the same time. Those are the only operations reachable through a
// shared reference: one non-cloneable token exists for each, and connecting
// or destroying requires exclusive access to the owner.
unsafe impl Sync for GenResource {}

impl Drop for GenResource {
    fn drop(&mut self) {
        // SAFETY: the handle was created by this table and is destroyed once.
        native!(self.table => unsafe { self.table.NDIlib_genlock_destroy(self.handle) });
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
/// use grafton_ndi::{advanced::Genlock, ScanType, Source, NDI};
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = NDI::new()?;
/// # let reference = Source::default();
/// let mut genlock = Genlock::new(&ndi, Some(&reference), None)?;
/// let (mut video, _audio) = genlock.clocks();
/// let paced_by = video.wait(60_000, 1001, ScanType::Progressive)?;
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
    /// containing NUL, and [`Error::InitializationFailed`] when the SDK
    /// creates no genlock.
    pub fn new(
        ndi: &NDI,
        source: Option<&Source>,
        config: Option<&AdvancedConfig>,
    ) -> Result<Self> {
        let table = ndi.table::<GenlockTable>()?;
        let source = source.map(Source::to_raw).transpose()?.map(Box::new);
        // SAFETY: the source descriptor and the configuration point to
        // strings that this value keeps alive for as long as the genlock.
        let handle: GenlockHandle = native!(table => unsafe {
            table.NDIlib_genlock_create(
                source.as_ref().map_or(ptr::null(), |source| &source.raw),
                config.map_or(ptr::null(), AdvancedConfig::as_ptr),
            )
        });
        if handle.is_null() {
            return Err(Error::InitializationFailed(
                "Genlock creation failed".into(),
            ));
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
        native!(table => unsafe {
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
        native!(table => unsafe { table.NDIlib_genlock_is_active(handle) })
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
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when a frame-rate part is zero or exceeds the
    /// SDK's range.
    pub fn wait(
        &mut self,
        numerator: u32,
        denominator: u32,
        scan: ScanType,
    ) -> Result<ClockSource> {
        let timing = video_timing(numerator, denominator, scan)?;
        let GenResource { handle, table, .. } = *self.resource;
        // SAFETY: the genlock outlives this clock, and only this clock waits
        // for video.
        Ok(ClockSource::from_active(native!(table => unsafe {
            table.NDIlib_genlock_wait_video(handle, &timing)
        })))
    }
}

/// The audio clock of a [`Genlock`].
pub struct AudioClock<'a> {
    resource: &'a GenResource,
    // One thread at a time waits on a clock.
    _not_sync: PhantomData<Cell<()>>,
}

impl AudioClock<'_> {
    /// Block until `samples` samples per channel at `sample_rate` hertz are
    /// due, and report which clock timed them.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] when a value is zero or exceeds the SDK's range.
    pub fn wait(&mut self, sample_rate: u32, samples: u32) -> Result<ClockSource> {
        let timing = audio_timing(sample_rate, samples)?;
        let GenResource { handle, table, .. } = *self.resource;
        // SAFETY: the genlock outlives this clock, and only this clock waits
        // for audio.
        Ok(ClockSource::from_active(native!(table => unsafe {
            table.NDIlib_genlock_wait_audio(handle, &timing)
        })))
    }
}

fn positive(value: u32) -> Result<i32> {
    if value == 0 {
        return Err(invalid(
            "Timing and explicit AVSync quantities must be positive",
        ));
    }
    i32::try_from(value).map_err(|_| invalid("Timing or AVSync quantity exceeds SDK signed range"))
}

fn video_timing(
    numerator: u32,
    denominator: u32,
    scan: ScanType,
) -> Result<NDIlib_video_frame_v2_t> {
    Ok(NDIlib_video_frame_v2_t {
        frame_rate_N: positive(numerator)?,
        frame_rate_D: positive(denominator)?,
        frame_format_type: scan.into(),
        ..Default::default()
    })
}

fn audio_timing(sample_rate: u32, samples: u32) -> Result<NDIlib_audio_frame_v3_t> {
    Ok(NDIlib_audio_frame_v3_t {
        sample_rate: positive(sample_rate)?,
        no_samples: positive(samples)?,
        ..Default::default()
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
                channels: 2
            }
        ));
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
        assert!(AvSyncRequest::new(Some(0), None, None).is_err());
        assert!(AvSyncRequest::new(None, Some(u32::MAX), None).is_err());
        assert!(AvSyncRequest::new(
            Some((crate::frames::MAX_AUDIO_BYTES / 8 + 1) as u32),
            None,
            Some(2)
        )
        .is_err());
        assert!(AvSyncRequest::new(
            Some((crate::frames::MAX_AUDIO_BYTES / 4 + 1) as u32),
            None,
            None
        )
        .is_err());
        assert_eq!(AvSyncRequest::automatic().samples(), None);
        let request = AvSyncRequest::new(Some(1600), Some(48_000), Some(2)).unwrap();
        assert_eq!(
            (request.samples(), request.sample_rate(), request.channels()),
            (Some(1600), Some(48_000), Some(2))
        );
        assert!(audio_timing(48_000, 0).is_err());
        assert!(video_timing(60_000, 0, ScanType::Progressive).is_err());
        let raw = video_timing(60_000, 1001, ScanType::Progressive).unwrap();
        assert_eq!((raw.frame_rate_N, raw.frame_rate_D), (60_000, 1001));
        assert_eq!(ClockSource::from_active(false), ClockSource::SystemClock);
        fn send<T: Send>() {}
        send::<VideoClock<'_>>();
        send::<AudioClock<'_>>();
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
