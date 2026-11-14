//! Audio matched to video timestamps, and clocks locked to a reference source.

#[cfg(any(test, ndi_has_advanced_av_sync, ndi_has_advanced_genlock))]
use std::ptr;
use std::{cell::Cell, marker::PhantomData};

use super::{invalid_input, sdk_quantity, AdvancedConfig, ReceiverControls};
use crate::{
    finder::RawSource,
    frames::MAX_AUDIO_BYTES,
    receiver::AvSyncAttached,
    sys::{
        advanced::{native, AvSync as AvSyncTable, Genlock as GenlockTable},
        bindings::*,
    },
    AudioFrame, FrameRate, Ndi, Receiver, Rejected, Result, ScanType, SdkTimestamp, Source,
    VideoFrame, VideoFrameRef,
};

use super::descriptors;
#[cfg(any(test, ndi_has_advanced_av_sync))]
use crate::frames::{AudioParts, ReturnedAudio, ValidatedAudio};
#[cfg(ndi_has_advanced_av_sync)]
use crate::sys::advanced::AvSyncHandle;
#[cfg(ndi_has_advanced_genlock)]
use crate::sys::advanced::GenlockHandle;
#[cfg(any(doc, test, ndi_has_advanced_av_sync, ndi_has_advanced_genlock))]
use crate::Error;

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
    #[cfg(any(test, ndi_has_advanced_av_sync))]
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
///
/// These are successful audio or expected no-audio states. Native failures
/// and unknown statuses are returned through [`crate::Error`].
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
}

/// How the SDK classified one synchronization.
#[cfg(any(test, ndi_has_advanced_av_sync))]
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
    #[cfg_attr(
        not(ndi_has_advanced_av_sync),
        expect(dead_code, reason = "runtime authorizes native AV sync descriptors")
    )]
    ndi: crate::Ndi,
    #[cfg(ndi_has_advanced_av_sync)]
    handle: AvSyncHandle,
    table: &'static AvSyncTable,
}

// SAFETY: a synchronizer belongs to no thread. Processing.NDI.AVSync.h and the
// Advanced SDK manual (25.6.2) have it created from a receiver, used for as
// long as it is needed and destroyed before that receiver, and state no
// thread for any of its functions. Moving the handle changes only which
// thread makes those calls: the receiver it reads is Send and moves with it,
// inside the same AvSync, which destroys the synchronizer first, and AvSync is
// not Sync, so its calls never overlap. `an_avsync_moves_between_threads` in
// tests/advanced_runtime.rs creates, synchronizes and destroys one on three
// threads against each runtime the tests use.
unsafe impl Send for AvResource {}

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
/// frame. It matches audio to the frame's timestamp, so the frame is given
/// as an [`AvSyncVideo`], which only a frame with a timestamp can be; the
/// frame is read as it was captured, without copying it.
/// [`into_receiver`](Self::into_receiver) returns the receiver.
///
/// The receiver is lent in the [`AvSyncAttached`] mode, with its video and
/// metadata capture, its events and every receiver control, but no audio
/// capture of its own.
/// [`AvSyncOutcome`] distinguishes no audio stream, no samples matching a
/// video's time, a changed format and successful output. These outcomes do
/// not replace monitoring progress of the media the application expects;
/// see the [reception monitoring guide](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/usage.md#monitor-reception-of-the-expected-media).
///
/// # Threading
///
/// An `AvSync` is [`Send`] but not [`Sync`]. It can be created on one thread
/// and synchronize and be dropped on another, but it synchronizes one call at
/// a time: the SDK does not document two synchronizations of one
/// synchronizer running at once. The receiver it lends is `Sync`, so scoped
/// threads can capture video or metadata, poll events or reconnect through it
/// while the thread that holds the `AvSync` synchronizes.
///
/// # Examples
///
/// ```no_run
/// use std::time::Duration;
/// use grafton_ndi::{
///     advanced::{AvSync, AvSyncOutcome, AvSyncRequest, AvSyncVideo},
///     Receiver, ReceiverOptions, Source, Ndi,
/// };
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = Ndi::new()?;
/// # let source = Source::new("");
/// let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
/// let sync = AvSync::new(receiver)?;
///
/// // The frame stays in the SDK's buffer: nothing is copied.
/// if let Some(captured) = sync.receiver().video().capture_ref(Duration::from_secs(1))? {
///     // A captured frame has its sender's timestamp when the SDK knows it.
///     if let Some(video) = AvSyncVideo::new(&captured) {
///         if let AvSyncOutcome::Audio { frame, .. } =
///             sync.synchronize(video, AvSyncRequest::automatic())?
///         {
///             println!("{} samples belong to this frame", frame.num_samples());
///         }
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct AvSync {
    // Declared first so that it is destroyed before the receiver it reads.
    resource: AvResource,
    receiver: Receiver<AvSyncAttached>,
    // `synchronize` takes `&self`, so withholding Sync keeps two
    // synchronizations of one synchronizer from running at once, which
    // neither Processing.NDI.AVSync.h nor the Advanced SDK manual (25.6)
    // documents as safe. The raw handle already withholds Sync; this states
    // it. Send comes from the fields.
    _not_sync: NotSync,
}

/// A field that withholds [`Sync`] from the type that holds it, and nothing
/// else: that type keeps [`Send`] and both unwind traits.
struct NotSync(PhantomData<Cell<()>>);

// It holds nothing, so a panic cannot leave it half-changed.
impl std::panic::RefUnwindSafe for NotSync {}

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
        native!(AvSync, table => {
            // SAFETY: the receiver instance is live, and this value keeps it
            // alive for longer than the synchronizer.
            let handle = unsafe { table.NDIlib_avsync_create(receiver.core.instance) };
            if handle.is_null() {
                return Err(Rejected::new(
                    Error::NativeOperationFailed {
                        operation: "NDIlib_avsync_create",
                    },
                    receiver,
                ));
            }
            Ok(Self {
                resource: AvResource { handle, table, ndi: receiver.core._ndi.clone() },
                receiver: receiver.into_mode(),
                _not_sync: NotSync(PhantomData),
            })
        })
    }

    /// The receiver this holds, in the [`AvSyncAttached`] mode: its video and
    /// metadata capture, its events, and the controls every receiver has:
    /// tally, PTZ, statistics, upstream metadata, keyframe requests, delays,
    /// connecting, reconnecting and source-name changes.
    ///
    /// A video frame captured through it can be handed to
    /// [`synchronize`](Self::synchronize) as it is, through [`AvSyncVideo`].
    /// Its own audio capture does not exist in this mode, because the
    /// synchronizer takes the audio:
    ///
    /// ```compile_fail,E0277
    /// use grafton_ndi::advanced::AvSync;
    /// fn capture(sync: &AvSync) {
    ///     let _ = sync.receiver().audio();
    /// }
    /// ```
    ///
    /// The synchronizer reads this receiver's native instance, so the
    /// receiver is never lent mutably, where it could be swapped for another
    /// `AvSync`'s. [`controls`](Self::controls) reaches exclusive stream
    /// controls and KVM; [`split`](Self::split) keeps synchronization usable
    /// while a KVM session holds the receiver borrow.
    pub fn receiver(&self) -> &Receiver<AvSyncAttached> {
        &self.receiver
    }

    /// Borrow exclusive receiver controls without permitting receiver replacement.
    /// This allocates nothing and makes no SDK call. Use [`Self::split`] to
    /// synchronize while a KVM session retains the receiver borrow.
    pub fn controls(&mut self) -> ReceiverControls<'_, AvSyncAttached> {
        ReceiverControls::new(&mut self.receiver)
    }

    /// Lend synchronization and exclusive receiver controls independently.
    ///
    /// Both views borrow this owner without allocating or making SDK calls.
    /// A session from the controls lends the receiver for video/metadata
    /// capture while the synchronization view continues matching audio.
    /// Release held inputs before changing source, and drop both views before
    /// recovering the receiver.
    pub fn split(&mut self) -> (AvSyncCapture<'_>, ReceiverControls<'_, AvSyncAttached>) {
        (
            AvSyncCapture {
                resource: &self.resource,
                _thread: PhantomData,
            },
            ReceiverControls::new(&mut self.receiver),
        )
    }

    /// Stop synchronizing and return the receiver, with its own audio
    /// capture again.
    pub fn into_receiver(self) -> Receiver {
        let Self {
            resource, receiver, ..
        } = self;
        drop(resource);
        receiver.into_mode()
    }

    /// Fetch the audio that belongs to `video`.
    ///
    /// The audio is matched to the frame's timestamp, which [`AvSyncVideo`]
    /// guarantees the frame has; [`queued_audio`](Self::queued_audio) takes
    /// the queued audio without a frame. The frame is read in place: a
    /// [`VideoFrameRef`] captured through [`receiver`](Self::receiver) is
    /// handed to the SDK as the SDK delivered it, and an owned copy of one as
    /// it is, so no pixels are copied. One call makes one native request: a
    /// changed format or missing audio is reported, never retried or
    /// converted.
    ///
    /// One call synchronizes at a time, on the thread that holds this
    /// `AvSync`, which is not `Sync`. Other threads may meanwhile capture
    /// video or metadata, poll events or use the controls through the
    /// receiver it lends. This does not take the receiver's capture guard, as
    /// [`FrameSync`](crate::FrameSync) captures do not, so a
    /// [`reconnect`](Receiver::reconnect) neither waits for it nor holds it
    /// off.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] when an owned video uses planar 4:4:4
    /// on a runtime without [`crate::RuntimeFeature::Planar444Video`].
    ///
    /// [`Error::InvalidFrame`] when the audio the SDK returned fails
    /// validation, [`Error::NativeOperationFailed`] when the SDK reports an
    /// internal error, and [`Error::UnexpectedSdkResult`] for an unknown
    /// status or a format change to a sample rate or channel count that is
    /// not positive.
    pub fn synchronize(
        &self,
        video: AvSyncVideo<'_>,
        request: AvSyncRequest,
    ) -> Result<AvSyncOutcome> {
        self.capture().synchronize(video, request)
    }

    /// Fetch whatever audio is currently queued, matched to no video frame.
    ///
    /// With [`AvSyncRequest::automatic`] this takes all the queued audio;
    /// with a sample count, that many samples. Otherwise as for
    /// [`synchronize`](Self::synchronize).
    ///
    /// # Errors
    ///
    /// As for [`synchronize`](Self::synchronize).
    pub fn queued_audio(&self, request: AvSyncRequest) -> Result<AvSyncOutcome> {
        self.capture().queued_audio(request)
    }

    /// The synchronization view of this owner, which `&self` keeps on its
    /// thread for as long as it is borrowed.
    fn capture(&self) -> AvSyncCapture<'_> {
        AvSyncCapture {
            resource: &self.resource,
            _thread: PhantomData,
        }
    }
}

/// Synchronization access lent by [`AvSync::split`].
///
/// The view borrows only the synchronizer, so a KVM session can hold the
/// independently lent receiver while this continues matching audio to video.
/// Creating or copying it allocates nothing and makes no SDK call. Like a
/// shared `&AvSync`, it is neither `Send` nor `Sync`: overlapping native
/// synchronizations are not documented as safe. Results own their audio.
#[derive(Clone, Copy)]
pub struct AvSyncCapture<'a> {
    resource: &'a AvResource,
    _thread: PhantomData<*mut ()>,
}

impl AvSyncCapture<'_> {
    /// Match audio to `video`; see [`AvSync::synchronize`] for timing,
    /// allocation costs, format changes and no-audio results.
    ///
    /// # Errors
    ///
    /// As for [`AvSync::synchronize`].
    pub fn synchronize(
        &self,
        video: AvSyncVideo<'_>,
        request: AvSyncRequest,
    ) -> Result<AvSyncOutcome> {
        self.request(Some(video), request)
    }

    /// Fetch the queued audio; see [`AvSync::queued_audio`].
    ///
    /// # Errors
    ///
    /// As for [`AvSync::synchronize`].
    pub fn queued_audio(&self, request: AvSyncRequest) -> Result<AvSyncOutcome> {
        self.request(None, request)
    }

    /// One native synchronization: for `video`, or with `None` for the
    /// queued audio.
    fn request(
        &self,
        _video: Option<AvSyncVideo<'_>>,
        _request: AvSyncRequest,
    ) -> Result<AvSyncOutcome> {
        native!(AvSync, self.resource.table => {
            // A copy of the frame's descriptor, pointing into the frame's own
            // buffers, which the borrow keeps allocated for the call, and
            // carrying the timestamp `AvSyncVideo::new` checked.
            let video = _video.map(|video| video.descriptor(&self.resource.ndi)).transpose()?;
            let AvResource { handle, table, .. } = *self.resource;
            let mut returned = Returned {
                audio: _request.raw(),
                release: |audio: &mut NDIlib_audio_frame_v3_t| {
                    // SAFETY: the synchronizer that filled the descriptor is live,
                    // and the descriptor is returned to it exactly once.
                    native!(AvSync, table => unsafe { table.NDIlib_avsync_free_audio(handle, audio) })
                },
            };
            let status = {
                // SAFETY: the synchronizer is live, and so is the receiver it
                // reads, whose owner this view borrows. The video
                // descriptor (if any) describes a frame borrowed for the call: an
                // owned frame's buffers, or the SDK's own descriptor of a captured
                // frame, whose guard keeps its buffers allocated. The SDK takes
                // it as a const reference and only reads it
                // (Processing.NDI.AVSync.h). The audio descriptor is writable.
                //
                // No other synchronization of this synchronizer runs, which the
                // header does not document as safe: this takes `&self`, and an
                // AvSync is not Sync. Captures through the lent
                // receiver may run on other threads meanwhile. The synchronizer
                // takes the receiver's audio as an audio capture of it would
                // (Processing.NDI.AVSync.h), and the SDK lets video, audio and
                // metadata be captured from one receiver on separate threads at
                // once (Processing.NDI.Recv.h). The AvSyncAttached mode has no
                // audio capture, so the synchronizer is the only audio consumer.
                //
                // The receiver's capture guard is not taken, as FrameSync's
                // captures do not take it. The guard keeps the crate's own
                // NDIlib_recv_capture_v3 calls from overlapping
                // NDIlib_recv_connect. How a synchronizer reads its receiver,
                // during this call or on its own, is internal to the SDK and not
                // documented, so a lock around this call could not be shown to
                // cover it. Processing.NDI.FrameSync.h leaves a frame
                // synchronizer's receiver usable for its other operations, and a
                // FrameSync's receiver already reconnects while it captures; this
                // synchronizer is treated the same way.
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
            };
            // SAFETY: the SDK owns whatever it returned until `returned` is
            // dropped, which happens after the copy.
            unsafe { outcome(status, &returned.audio) }
        })
    }
}

/// The audio descriptor of one synchronization. Whatever the SDK allocated
/// for it is handed to `release` when the descriptor is dropped.
#[cfg(any(test, ndi_has_advanced_av_sync))]
struct Returned<F: FnMut(&mut NDIlib_audio_frame_v3_t)> {
    audio: NDIlib_audio_frame_v3_t,
    release: F,
}

#[cfg(any(test, ndi_has_advanced_av_sync))]
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
#[cfg(any(test, ndi_has_advanced_av_sync))]
unsafe fn outcome(status: Synchronized, audio: &NDIlib_audio_frame_v3_t) -> Result<AvSyncOutcome> {
    Ok(match status {
        Synchronized::Matched | Synchronized::Adjusted => {
            let samples_adjusted = status == Synchronized::Adjusted;
            // SAFETY: forwarded to the caller's obligation; the borrow holds
            // the descriptor through the copy.
            match unsafe { ValidatedAudio::returned_audio(audio)? } {
                // The format of an empty result is not reported.
                ReturnedAudio::Empty(_) => AvSyncOutcome::Empty { samples_adjusted },
                ReturnedAudio::Samples(validated) => AvSyncOutcome::Audio {
                    frame: validated.owned_copy(),
                    samples_adjusted,
                },
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
        Synchronized::InternalError => {
            return Err(Error::NativeOperationFailed {
                operation: "NDIlib_avsync_synchronize",
            });
        }
        Synchronized::Unknown(code) => {
            return Err(Error::UnexpectedSdkResult(format!(
                "NDIlib_avsync_synchronize returned unknown status {code}"
            )));
        }
    })
}

mod video_sealed {
    use crate::{sys::bindings::NDIlib_video_frame_v2_t, SdkTimestamp};

    /// What [`AvSyncVideo`](super::AvSyncVideo) reads from a frame. The
    /// supertraits let a borrowed frame cross threads as the frame itself can.
    pub trait Sealed: Sync + std::panic::RefUnwindSafe {
        /// A copy of the frame's SDK descriptor, which points into buffers
        /// that the borrow of `self` keeps allocated and unchanged.
        fn descriptor(&self, ndi: &crate::Ndi) -> crate::Result<NDIlib_video_frame_v2_t>;

        /// The time the frame's sender submitted it, when known.
        fn receive_timestamp(&self) -> Option<SdkTimestamp>;
    }
}

/// A video frame [`AvSyncVideo::new`] accepts: a [`VideoFrameRef`] captured
/// from a receiver, such as the one [`AvSync::receiver`] lends, or an owned
/// [`VideoFrame`].
///
/// The trait is sealed; those two are the only video frames it covers.
pub trait AvSyncFrame: video_sealed::Sealed {}

impl video_sealed::Sealed for VideoFrame {
    fn descriptor(&self, ndi: &crate::Ndi) -> crate::Result<NDIlib_video_frame_v2_t> {
        use crate::frames::VideoParts;
        let authorized = crate::frames::SendableVideoLayout::new(*self.layout(), ndi)?;
        Ok(self.to_raw(authorized))
    }

    fn receive_timestamp(&self) -> Option<SdkTimestamp> {
        self.timestamp()
    }
}

impl AvSyncFrame for VideoFrame {}

impl video_sealed::Sealed for VideoFrameRef<'_> {
    fn descriptor(&self, _: &crate::Ndi) -> crate::Result<NDIlib_video_frame_v2_t> {
        let mut raw = *self.raw();
        if self.metadata().is_none() {
            raw.p_metadata = std::ptr::null();
        }
        Ok(raw)
    }

    fn receive_timestamp(&self) -> Option<SdkTimestamp> {
        self.timestamp()
    }
}

impl AvSyncFrame for VideoFrameRef<'_> {}

/// A video frame that carries the time its sender submitted it: what
/// [`AvSync::synchronize`] matches audio to.
///
/// AV sync selects the audio whose timestamps belong to a video frame's
/// timestamp (Advanced SDK manual, 25.6.1), so a frame without one gives it
/// nothing to match. [`new`](Self::new) checks the timestamp once, and only
/// this type is accepted, so synchronization is never given a frame without
/// one. A frame captured from a receiver carries its sender's timestamp when
/// the SDK knows it, and an owned copy keeps it. A frame the application
/// builds has none.
///
/// The value borrows the frame, whose pixels the SDK reads in place: making
/// it copies nothing and makes no SDK call. Like the frames it borrows, it is
/// `Send` and `Sync`.
///
/// # Examples
///
/// ```
/// use grafton_ndi::{advanced::AvSyncVideo, VideoFrame};
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// // A frame the application builds was never sent, so it has no timestamp.
/// let built = VideoFrame::builder().resolution(64, 32).build()?;
/// assert!(AvSyncVideo::new(&built).is_none());
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy)]
pub struct AvSyncVideo<'a> {
    // The borrow is held in every build, so the type has the same lifetime
    // and auto traits in all of them; only the native synchronization reads
    // the frame, and a build without the AV sync declarations has none.
    #[cfg_attr(
        not(any(test, ndi_has_advanced_av_sync)),
        expect(dead_code, reason = "only the native synchronization reads the frame")
    )]
    frame: &'a dyn video_sealed::Sealed,
    timestamp: SdkTimestamp,
}

impl<'a> AvSyncVideo<'a> {
    /// `frame`, when it carries the time its sender submitted it.
    ///
    /// Returns `None` for a frame without one: a frame the application
    /// built, or a captured frame whose sender's timestamp the SDK did not
    /// know.
    pub fn new<F: AvSyncFrame>(frame: &'a F) -> Option<Self> {
        Some(Self {
            timestamp: frame.receive_timestamp()?,
            frame,
        })
    }

    /// The time the frame's sender submitted it.
    pub fn timestamp(&self) -> SdkTimestamp {
        self.timestamp
    }

    /// The frame's descriptor, carrying the timestamp [`Self::new`] checked.
    #[cfg(any(test, ndi_has_advanced_av_sync))]
    fn descriptor(&self, ndi: &crate::Ndi) -> crate::Result<NDIlib_video_frame_v2_t> {
        Ok(NDIlib_video_frame_v2_t {
            timestamp: self.timestamp.ticks(),
            ..self.frame.descriptor(ndi)?
        })
    }
}

impl std::fmt::Debug for AvSyncVideo<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvSyncVideo")
            .field("timestamp", &self.timestamp)
            .finish_non_exhaustive()
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
    #[cfg(any(test, ndi_has_advanced_genlock))]
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
    #[cfg(ndi_has_advanced_genlock)]
    handle: GenlockHandle,
    table: &'static GenlockTable,
    _ndi: Ndi,
}

// SAFETY: a genlock belongs to no thread. Processing.NDI.Genlock.h and the
// Advanced SDK manual (25.5) state no thread for creating, connecting,
// querying or destroying one, and let its two waits run on separate threads,
// neither of which need be the one that created it. In the 6.3.2 and 6.4
// Linux x86_64 runtimes, `NDIlib_genlock_connect` hands the source to
// `NDIlib_recv_connect` on a receiver the genlock owns, a receiver operation
// that this crate already makes from any thread. Moving the handle changes
// only which thread makes the calls: the runtime handle it keeps is Send, a
// `Genlock` is not Sync, and connecting and destroying take it exclusively,
// so neither overlaps a wait or a query. `a_genlock_moves_between_threads` in
// tests/advanced_runtime.rs creates, connects, waits on and destroys one on
// five threads against each runtime the tests use.
unsafe impl Send for GenResource {}

// SAFETY: the SDK allows the video wait and the audio wait to run on separate
// threads at the same time. Those are the only operations that reach this
// from another thread: one non-cloneable clock exists for each, and while the
// clocks exist they borrow the genlock exclusively, so neither `is_active`,
// reached through a `Genlock`, which is not Sync, nor connecting or
// destroying overlaps them.
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
/// A completed paced wait does not prove live reference media; inspect the
/// returned [`ClockSource`] for reference versus system-clock fallback.
///
/// # Threading
///
/// A genlock is [`Send`] but not [`Sync`]: it can be created on one thread
/// and connected, waited on and dropped on others. Each of its two clocks
/// can be moved to a thread of its own while they borrow it, because the SDK
/// lets the video and the audio wait run on separate threads at once. The
/// SDK gives no such assurance for [`is_active`](Self::is_active), so a
/// shared reference to the genlock stays on one thread.
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
    // Through a shared reference only `is_active` is reached, and the SDK
    // documents only the two waits as safe on separate threads at once
    // (Processing.NDI.Genlock.h, Advanced SDK manual 25.5), not concurrent
    // queries, so Sync is withheld. The waits run through the clocks, which
    // borrow the genlock exclusively. Send comes from the fields.
    _not_sync: NotSync,
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
        _source: Option<&Source>,
        _config: Option<&AdvancedConfig>,
    ) -> Result<Self> {
        let table = ndi.table::<GenlockTable>()?;
        native!(Genlock, table => {
            let source = _source.map(Source::to_raw).transpose()?.map(Box::new);
            // SAFETY: the source descriptor and the configuration point to
            // strings that this value keeps alive for as long as the genlock.
            let handle = unsafe {
                table.NDIlib_genlock_create(
                    source.as_ref().map_or(ptr::null(), |source| &source.raw),
                    _config.map_or(ptr::null(), AdvancedConfig::as_ptr),
                )
            };
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
                _config: _config.cloned(),
                _not_sync: NotSync(PhantomData),
            })
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
        // Validate before reaching the native boundary.
        let _source = source.map(Source::to_raw).transpose()?.map(Box::new);
        native!(Genlock, self.resource.table => {
            let GenResource { handle, table, .. } = self.resource;
            // SAFETY: the genlock is live and no wait is in progress, and the new
            // descriptor is stored below before the earlier one is released.
            unsafe {
                table.NDIlib_genlock_connect(
                    handle,
                    _source.as_ref().map_or(ptr::null(), |source| &source.raw),
                )
            };
            self._source = _source;
            Ok(())
        })
    }

    /// Whether a reference signal is being received.
    pub fn is_active(&self) -> bool {
        // SAFETY: the genlock is live for this borrow.
        native!(Genlock, self.resource.table => unsafe {
            self.resource.table.NDIlib_genlock_is_active(self.resource.handle)
        })
    }

    /// The video clock and the audio clock.
    ///
    /// Each can be moved to its own thread; both borrow the genlock, so it can
    /// be neither reconnected nor dropped while they exist.
    pub fn clocks(&mut self) -> (VideoClock<'_>, AudioClock<'_>) {
        (
            VideoClock {
                resource: &self.resource,
                _not_sync: NotSync(PhantomData),
            },
            AudioClock {
                resource: &self.resource,
                _not_sync: NotSync(PhantomData),
            },
        )
    }
}

/// The video clock of a [`Genlock`].
pub struct VideoClock<'a> {
    resource: &'a GenResource,
    // One thread at a time waits on a clock.
    _not_sync: NotSync,
}

impl VideoClock<'_> {
    /// Block until the next frame of a stream with this frame rate and scan
    /// type is due, and report which clock timed it.
    pub fn wait(&mut self, _frame_rate: FrameRate, _scan_type: ScanType) -> ClockSource {
        native!(Genlock, self.resource.table => {
            let timing = video_timing(_frame_rate, _scan_type);
            let GenResource { handle, table, .. } = *self.resource;
            // SAFETY: the genlock outlives this clock, and only this clock waits
            // for video.
            ClockSource::from_active(unsafe {
                table.NDIlib_genlock_wait_video(handle, &timing)
            })
        })
    }
}

/// The audio clock of a [`Genlock`].
pub struct AudioClock<'a> {
    resource: &'a GenResource,
    // One thread at a time waits on a clock.
    _not_sync: NotSync,
}

impl AudioClock<'_> {
    /// Block until `num_samples` samples per channel at `sample_rate` hertz
    /// are due, and report which clock timed them.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when a value is zero or exceeds `i32::MAX`.
    pub fn wait(&mut self, sample_rate: u32, num_samples: u32) -> Result<ClockSource> {
        // Validate the request even in builds without the native family.
        let _timing = audio_timing(sample_rate, num_samples)?;
        native!(Genlock, self.resource.table => {
            let GenResource { handle, table, .. } = *self.resource;
            // SAFETY: the genlock outlives this clock, and only this clock waits
            // for audio.
            Ok(ClockSource::from_active(unsafe {
                table.NDIlib_genlock_wait_audio(handle, &_timing)
            }))
        })
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
#[cfg(any(test, ndi_has_advanced_genlock))]
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
        // SAFETY: failure statuses never read the descriptor's buffers.
        assert!(matches!(
            unsafe { outcome(Synchronized::InternalError, &audio) },
            Err(Error::NativeOperationFailed {
                operation: "NDIlib_avsync_synchronize"
            })
        ));
        for code in [-99, 99] {
            // SAFETY: an unknown status never reads a buffer.
            assert!(matches!(
                unsafe { outcome(Synchronized::Unknown(code), &audio) },
                Err(Error::UnexpectedSdkResult(message)) if message.contains(&code.to_string())
            ));
        }

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

    /// A captured 64x32 UYVY frame with `timestamp`, over `pixels`.
    fn captured(pixels: &mut [u8], timestamp: i64) -> VideoFrameRef<'_> {
        use crate::capture::{CaptureStrategy, Guard, VideoKind};
        assert_eq!(pixels.len(), 128 * 32);
        let raw = NDIlib_video_frame_v2_t {
            xres: 64,
            yres: 32,
            FourCC: NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVY,
            frame_rate_N: 30_000,
            frame_rate_D: 1001,
            frame_format_type: NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive,
            p_data: pixels.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 128,
            },
            timestamp,
            ..Default::default()
        };
        // SAFETY: `pixels` holds every row the descriptor describes and stays
        // borrowed for as long as the frame; the synthetic free owns nothing.
        unsafe { VideoKind::make_ref(Guard::with_test_free(ptr::null_mut(), raw, |_, _| {})) }
            .unwrap()
    }

    /// Only a frame that carries its sender's timestamp can be synchronized,
    /// and the SDK is handed that frame, read in place, with exactly the
    /// timestamp that was checked.
    #[test]
    fn only_a_frame_with_a_timestamp_can_be_synchronized() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let built = VideoFrame::builder().resolution(64, 32).build().unwrap();
        assert!(AvSyncVideo::new(&built).is_none());

        let mut unstamped = vec![16; 128 * 32];
        let unstamped = captured(&mut unstamped, NDIlib_recv_timestamp_undefined);
        assert!(AvSyncVideo::new(&unstamped).is_none());

        let sent = 17_946_474_843_047_964;
        let mut pixels = vec![16; 128 * 32];
        let start = pixels.as_ptr();
        let frame = captured(&mut pixels, sent);
        let video = AvSyncVideo::new(&frame).unwrap();
        assert_eq!(video.timestamp(), SdkTimestamp::from_ticks(sent));
        let descriptor = video.descriptor(&ndi).unwrap();
        assert_eq!(descriptor.timestamp, sent);
        assert_eq!(descriptor.p_data.cast_const(), start);
        assert!(format!("{video:?}").contains("timestamp"));

        // An owned copy keeps the timestamp and is read in its own buffer.
        let owned = frame.to_owned();
        let video = AvSyncVideo::new(&owned).unwrap();
        assert_eq!(video.timestamp(), SdkTimestamp::from_ticks(sent));
        assert_eq!(video.descriptor(&ndi).unwrap().timestamp, sent);
        assert_eq!(
            video.descriptor(&ndi).unwrap().p_data.cast_const(),
            owned.data().as_ptr()
        );
    }

    #[test]
    fn owned_planar_444_avsync_checks_the_receiver_runtime() {
        use crate::{
            capture::{CaptureStrategy, Guard, VideoKind},
            PixelFormat, ReceiverOptions, Source,
        };
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        if !ndi.capabilities().supports(crate::RuntimeFeature::AvSync) {
            return;
        }
        let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new(""))).unwrap();
        let mut sync = AvSync::new(receiver).unwrap();
        let supported = ndi
            .capabilities()
            .supports(crate::RuntimeFeature::Planar444Video);
        for format in [
            PixelFormat::Na48,
            PixelFormat::Nb48,
            PixelFormat::Na4g,
            PixelFormat::Nb4g,
        ] {
            let backing = VideoFrame::builder()
                .resolution(64, 32)
                .pixel_format(format)
                .build()
                .unwrap();
            let mut raw = backing.as_test_raw();
            raw.timestamp = 1234;
            // SAFETY: backing owns the descriptor's complete validated storage
            // throughout this synthetic capture and copy; no-op free owns nothing.
            let captured = unsafe {
                VideoKind::make_ref(Guard::with_test_free(ptr::null_mut(), raw, |_, _| {}))
            }
            .unwrap();
            let owned = captured.to_owned();
            let video = AvSyncVideo::new(&owned).unwrap();
            for result in [
                sync.synchronize(video, AvSyncRequest::automatic()),
                sync.split()
                    .0
                    .synchronize(video, AvSyncRequest::automatic()),
            ] {
                if supported {
                    assert!(result.is_ok(), "{format:?}: {result:?}");
                } else {
                    assert!(
                        matches!(
                            result,
                            Err(Error::UnsupportedCapability {
                                feature: crate::RuntimeFeature::Planar444Video,
                                reason: crate::CapabilityUnavailable::RuntimeTooOld,
                                ..
                            })
                        ),
                        "{format:?}: {result:?}"
                    );
                }
            }
        }
    }

    /// Only `Sync` is withheld, which
    /// tests/ui/advanced/pass/av_sync_is_send_but_not_sync.rs checks.
    #[test]
    fn av_sync_is_send_and_unwind_safe() {
        fn traits<T: Send + std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
        traits::<AvSync>();
    }

    /// As for AvSync; tests/ui/advanced/pass/genlock_is_send_but_not_sync.rs
    /// checks that `Sync` is withheld.
    #[test]
    fn genlock_is_send_and_unwind_safe() {
        fn traits<T: Send + std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
        traits::<Genlock>();
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
            assert_eq!(result.is_ok(), status == Synchronized::FormatChanged);
            drop(audio);
            assert_eq!(released.get(), before + 1);
        }

        // Nothing was allocated, so nothing is released.
        drop(returned(NDIlib_audio_frame_v3_t::default()));
        assert_eq!(released.get(), 4);
    }
}
