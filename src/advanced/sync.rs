//! Exclusive AV synchronization and independent, non-cloneable pacing clocks.
use super::{ffi, AdvancedConfig};
use crate::{
    finder::RawSource,
    frames::{validate_audio_layout_allow_empty, validate_frame_metadata},
    ndi_lib::{
        NDIlib_audio_frame_v3_t, NDIlib_recv_instance_t, NDIlib_source_t, NDIlib_video_frame_v2_t,
    },
    AudioFrame, Capture, ConnectionStats, Error, Receiver, Result, ScanType, Source, Tally,
    VideoFrame, VideoKind, NDI,
};
use std::{cell::Cell, ffi::c_char, marker::PhantomData, ptr};

#[cfg(ndi_has_advanced_header)]
type AvHandle = crate::ndi_lib::raw::NDIlib_avsync_instance_t;
#[cfg(not(ndi_has_advanced_header))]
type AvHandle = ffi::Opaque;
#[cfg(ndi_has_advanced_header)]
type GenHandle = crate::ndi_lib::raw::NDIlib_genlock_instance_t;
#[cfg(not(ndi_has_advanced_header))]
type GenHandle = ffi::Opaque;
type AvCreate = unsafe extern "C" fn(NDIlib_recv_instance_t) -> AvHandle;
type AvDestroy = unsafe extern "C" fn(AvHandle);
type AvSynchronize = unsafe extern "C" fn(
    AvHandle,
    *const NDIlib_video_frame_v2_t,
    *mut NDIlib_audio_frame_v3_t,
) -> i32;
type AvFree = unsafe extern "C" fn(AvHandle, *mut NDIlib_audio_frame_v3_t);

/// Validated AVSync request. Zero sentinels are represented by `None`.
/// AVSync matches samples to timestamps; it does not resample the source audio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AvSyncRequest {
    samples: i32,
    sample_rate: i32,
    channels: i32,
}
impl AvSyncRequest {
    /// Let the SDK choose the audio format and sample count matching the video.
    pub fn automatic() -> Self {
        Self::default()
    }
    /// Request an optional sample count and expected format. A format change is
    /// reported separately; the request never silently resamples or retries.
    pub fn new(
        samples: Option<u32>,
        sample_rate: Option<u32>,
        channels: Option<u32>,
    ) -> Result<Self> {
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
                .and_then(|n| n.checked_mul(4));
            if bytes.is_none_or(|n| n > crate::frames::MAX_AUDIO_BYTES || n > isize::MAX as usize) {
                return Err(ffi::invalid(
                    "AVSync request exceeds the supported audio storage limit",
                ));
            }
        }
        Ok(request)
    }
    pub fn samples(self) -> Option<u32> {
        (self.samples != 0).then_some(self.samples as u32)
    }
    pub fn sample_rate(self) -> Option<u32> {
        (self.sample_rate != 0).then_some(self.sample_rate as u32)
    }
    pub fn channels(self) -> Option<u32> {
        (self.channels != 0).then_some(self.channels as u32)
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

/// Result of one native AVSync operation. Returned audio is owned and independent
/// of both the source receiver and synchronizer.
#[derive(Debug)]
#[non_exhaustive]
pub enum AvSyncOutcome {
    Audio {
        frame: AudioFrame,
        samples_adjusted: bool,
    },
    Empty {
        samples_adjusted: bool,
    },
    FormatChanged {
        sample_rate: i32,
        channels: i32,
    },
    NoAudioStream,
    NoMatchingSamples,
    InternalError,
    Unknown(i32),
}

struct AvResource {
    handle: AvHandle,
    destroy: AvDestroy,
    synchronize: AvSynchronize,
    free: AvFree,
    _ndi: NDI,
}
impl Drop for AvResource {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.handle) };
    }
}

/// Owns its receiver exclusively while AVSync consumes that receiver's audio.
/// The restricted receiver view exposes video and controls, never raw audio or
/// a route to FrameSync. Recover the receiver with `into_receiver` first.
pub struct AvSync {
    resource: AvResource,
    receiver: Receiver,
}
impl AvSync {
    pub fn new(receiver: Receiver) -> Result<Self> {
        let ndi = &receiver.core._ndi;
        // Resolve all creation/use/free/destroy operations before creating state.
        let create: AvCreate = unsafe { ffi::required(ndi, b"NDIlib_avsync_create\0")? };
        let destroy = unsafe { ffi::required(ndi, b"NDIlib_avsync_destroy\0")? };
        let synchronize = unsafe { ffi::required(ndi, b"NDIlib_avsync_synchronize\0")? };
        let free = unsafe { ffi::required(ndi, b"NDIlib_avsync_free_audio\0")? };
        let handle = unsafe { create(receiver.core.instance) };
        if handle.is_null() {
            return Err(Error::InitializationFailed("AVSync creation failed".into()));
        }
        let resource = AvResource {
            handle,
            destroy,
            synchronize,
            free,
            _ndi: ndi.clone(),
        };
        Ok(Self { resource, receiver })
    }
    pub fn receiver(&self) -> AvSyncReceiver<'_> {
        AvSyncReceiver {
            receiver: &self.receiver,
        }
    }
    pub fn into_receiver(self) -> Receiver {
        let Self { resource, receiver } = self;
        drop(resource);
        receiver
    }
    /// Match audio to an owned video frame, or drain current audio when video is
    /// None. Exactly one native call is made; format changes remain visible.
    pub fn synchronize(
        &mut self,
        video: Option<&VideoFrame>,
        request: AvSyncRequest,
    ) -> Result<AvSyncOutcome> {
        let video = video.map(VideoFrame::to_raw);
        let mut raw = request.raw();
        let result = unsafe {
            (self.resource.synchronize)(
                self.resource.handle,
                video.as_ref().map_or(ptr::null(), |v| v),
                &mut raw,
            )
        };
        let guard = AvAudioGuard {
            handle: self.resource.handle,
            raw,
            free: self.resource.free,
        };
        // SAFETY: the live SDK owns every returned buffer until the guard frees
        // it. Validation and owned copying occur while that allocation is live.
        unsafe { outcome(result, guard) }
    }
}

/// Borrowed controls/video view with no raw-audio capture capability.
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
    pub fn video(&self) -> Capture<'_, VideoKind> {
        self.receiver.video()
    }
    pub fn source(&self) -> &Source {
        self.receiver.source()
    }
    pub fn is_connected(&self) -> bool {
        self.receiver.is_connected()
    }
    pub fn connection_stats(&self) -> ConnectionStats {
        self.receiver.connection_stats()
    }
    pub fn set_tally(&self, tally: &Tally) -> bool {
        self.receiver.set_tally(tally)
    }
}

struct AvAudioGuard {
    handle: AvHandle,
    raw: NDIlib_audio_frame_v3_t,
    free: AvFree,
}
impl Drop for AvAudioGuard {
    fn drop(&mut self) {
        // Negative/format results can still carry SDK allocations. Free those
        // through the originating synchronizer, including validation failures.
        if !self.raw.p_data.is_null() || !self.raw.p_metadata.is_null() {
            unsafe { (self.free)(self.handle, &mut self.raw) };
        }
    }
}
unsafe fn outcome(result: i32, guard: AvAudioGuard) -> Result<AvSyncOutcome> {
    Ok(match result {
        1 | 2 => {
            let layout = validate_audio_layout_allow_empty(&guard.raw)?;
            if layout.is_empty() {
                AvSyncOutcome::Empty {
                    samples_adjusted: result == 2,
                }
            } else {
                let metadata = unsafe { validate_frame_metadata(guard.raw.p_metadata)? };
                let frame = unsafe { AudioFrame::from_raw_validated(guard.raw, layout, metadata)? };
                AvSyncOutcome::Audio {
                    frame,
                    samples_adjusted: result == 2,
                }
            }
        }
        -1 => AvSyncOutcome::NoAudioStream,
        -2 => AvSyncOutcome::NoMatchingSamples,
        -3 => AvSyncOutcome::FormatChanged {
            sample_rate: guard.raw.sample_rate,
            channels: guard.raw.no_channels,
        },
        -4 => AvSyncOutcome::InternalError,
        other => AvSyncOutcome::Unknown(other),
    })
}

type GenCreate = unsafe extern "C" fn(*const NDIlib_source_t, *const c_char) -> GenHandle;
type GenDestroy = unsafe extern "C" fn(GenHandle);
type GenConnect = unsafe extern "C" fn(GenHandle, *const NDIlib_source_t);
type GenActive = unsafe extern "C" fn(GenHandle) -> bool;
type WaitVideo = unsafe extern "C" fn(GenHandle, *const NDIlib_video_frame_v2_t) -> bool;
type WaitAudio = unsafe extern "C" fn(GenHandle, *const NDIlib_audio_frame_v3_t) -> bool;

// Type-check exact native signatures without emitting mandatory shared-library
// references. These constants are never used as runtime dispatch addresses.
#[cfg(ndi_has_advanced_header)]
mod abi_signatures {
    use super::*;
    use crate::ndi_lib::raw;
    const _: AvCreate = raw::NDIlib_avsync_create;
    const _: AvDestroy = raw::NDIlib_avsync_destroy;
    const _: AvSynchronize = raw::NDIlib_avsync_synchronize;
    const _: AvFree = raw::NDIlib_avsync_free_audio;
    const _: GenCreate = raw::NDIlib_genlock_create;
    const _: GenDestroy = raw::NDIlib_genlock_destroy;
    const _: GenConnect = raw::NDIlib_genlock_connect;
    const _: GenActive = raw::NDIlib_genlock_is_active;
    const _: WaitVideo = raw::NDIlib_genlock_wait_video;
    const _: WaitAudio = raw::NDIlib_genlock_wait_audio;
}

/// Timing reference used by the completed wait, including native fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClockSource {
    ReferenceSignal,
    SystemClock,
}
impl From<bool> for ClockSource {
    fn from(active: bool) -> Self {
        if active {
            Self::ReferenceSignal
        } else {
            Self::SystemClock
        }
    }
}

struct GenResource {
    handle: GenHandle,
    destroy: GenDestroy,
    video: WaitVideo,
    audio: WaitAudio,
    _ndi: NDI,
}
// SAFETY: SDK documents independent video/audio waits as thread safe. Only
// non-cloneable clock tokens expose these operations, one token per clock;
// connect/destruction require exclusive access to their borrowed owner.
unsafe impl Sync for GenResource {}
impl Drop for GenResource {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.handle) };
    }
}

/// Owned genlock reference. Connection changes require exclusive access.
/// Native waits have no cancellation or deadline and may block their thread.
/// Disable a sender's built-in clock before pacing it with these clocks.
pub struct Genlock {
    resource: GenResource,
    connect: GenConnect,
    active: GenActive,
    _source: Option<Box<RawSource>>,
    _config: Option<AdvancedConfig>,
}
impl Genlock {
    pub fn new(
        ndi: &NDI,
        source: Option<&Source>,
        config: Option<&AdvancedConfig>,
    ) -> Result<Self> {
        let create: GenCreate = unsafe { ffi::required(ndi, b"NDIlib_genlock_create\0")? };
        let destroy = unsafe { ffi::required(ndi, b"NDIlib_genlock_destroy\0")? };
        let connect = unsafe { ffi::required(ndi, b"NDIlib_genlock_connect\0")? };
        let active = unsafe { ffi::required(ndi, b"NDIlib_genlock_is_active\0")? };
        let video = unsafe { ffi::required(ndi, b"NDIlib_genlock_wait_video\0")? };
        let audio = unsafe { ffi::required(ndi, b"NDIlib_genlock_wait_audio\0")? };
        let source = source.map(Source::to_raw).transpose()?.map(Box::new);
        let handle = unsafe {
            create(
                source.as_ref().map_or(ptr::null(), |s| &s.raw),
                config.map_or(ptr::null(), AdvancedConfig::as_ptr),
            )
        };
        if handle.is_null() {
            return Err(Error::InitializationFailed(
                "Genlock creation failed".into(),
            ));
        }
        Ok(Self {
            resource: GenResource {
                handle,
                destroy,
                video,
                audio,
                _ndi: ndi.clone(),
            },
            connect,
            active,
            _source: source,
            _config: config.cloned(),
        })
    }
    /// Reconnect, or disconnect into system-clock fallback. Existing clock
    /// tokens must be dropped before this method can be called.
    pub fn connect(&mut self, source: Option<&Source>) -> Result<()> {
        let source = source.map(Source::to_raw).transpose()?.map(Box::new);
        unsafe {
            (self.connect)(
                self.resource.handle,
                source.as_ref().map_or(ptr::null(), |s| &s.raw),
            )
        };
        self._source = source;
        Ok(())
    }
    pub fn is_active(&self) -> bool {
        unsafe { (self.active)(self.resource.handle) }
    }
    /// Borrow one exclusive token for each independently paced clock. Tokens
    /// can move to scoped workers; their lifetime keeps this genlock alive.
    pub fn clocks(&mut self) -> (VideoClock<'_>, AudioClock<'_>) {
        (
            VideoClock {
                resource: &self.resource,
                _exclusive: PhantomData,
            },
            AudioClock {
                resource: &self.resource,
                _exclusive: PhantomData,
            },
        )
    }
}

pub struct VideoClock<'a> {
    resource: &'a GenResource,
    _exclusive: PhantomData<Cell<()>>,
}
impl VideoClock<'_> {
    /// Wait without a deadline or cancellation. The returned value states which
    /// timing reference actually paced this frame.
    pub fn wait(
        &mut self,
        numerator: u32,
        denominator: u32,
        scan: ScanType,
    ) -> Result<ClockSource> {
        let raw = video_timing(numerator, denominator, scan)?;
        Ok(unsafe { (self.resource.video)(self.resource.handle, &raw) }.into())
    }
}
pub struct AudioClock<'a> {
    resource: &'a GenResource,
    _exclusive: PhantomData<Cell<()>>,
}
impl AudioClock<'_> {
    /// Wait without a deadline or cancellation for a positive sample count.
    pub fn wait(&mut self, sample_rate: u32, samples: u32) -> Result<ClockSource> {
        let raw = audio_timing(sample_rate, samples)?;
        Ok(unsafe { (self.resource.audio)(self.resource.handle, &raw) }.into())
    }
}
fn positive(value: u32) -> Result<i32> {
    if value == 0 {
        return Err(ffi::invalid(
            "Timing and explicit AVSync quantities must be positive",
        ));
    }
    i32::try_from(value)
        .map_err(|_| ffi::invalid("Timing or AVSync quantity exceeds SDK signed range"))
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    static FREED: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn free(_: AvHandle, _: *mut NDIlib_audio_frame_v3_t) {
        FREED.fetch_add(1, Ordering::SeqCst);
    }
    #[test]
    fn distinguishes_native_outcomes_and_frees_even_invalid_or_changed_frames() {
        for code in [-3, -4, 99, 1] {
            let before = FREED.load(Ordering::SeqCst);
            let raw = NDIlib_audio_frame_v3_t {
                p_data: std::ptr::dangling_mut::<u8>(),
                sample_rate: 48_000,
                no_channels: 2,
                no_samples: 1,
                ..Default::default()
            };
            let result = unsafe {
                outcome(
                    code,
                    AvAudioGuard {
                        handle: ptr::null_mut(),
                        raw,
                        free,
                    },
                )
            };
            assert_eq!(FREED.load(Ordering::SeqCst), before + 1);
            match code {
                -3 => assert!(matches!(
                    result.unwrap(),
                    AvSyncOutcome::FormatChanged {
                        sample_rate: 48_000,
                        channels: 2
                    }
                )),
                -4 => assert!(matches!(result.unwrap(), AvSyncOutcome::InternalError)),
                99 => assert!(matches!(result.unwrap(), AvSyncOutcome::Unknown(99))),
                _ => assert!(result.is_err()),
            }
        }
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
        assert!(audio_timing(48_000, 0).is_err());
        assert!(video_timing(60_000, 0, ScanType::Progressive).is_err());
        let raw = video_timing(60_000, 1001, ScanType::Progressive).unwrap();
        assert_eq!((raw.frame_rate_N, raw.frame_rate_D), (60_000, 1001));
        assert_eq!(ClockSource::from(false), ClockSource::SystemClock);
        fn send<T: Send>() {}
        send::<VideoClock<'_>>();
        send::<AudioClock<'_>>();
    }
}
