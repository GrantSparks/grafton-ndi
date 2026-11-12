//! Receiving compressed packets instead of decoded frames.

use std::{marker::PhantomData, rc::Rc, time::Duration};

use super::{
    packets, AdvancedConfig, AudioCodec, CompressedAudioFrame, CompressedVideoFrame, VideoCodec,
};
use crate::{
    capture::{AudioKind, CaptureKind, Guard, MetadataKind, VideoKind},
    sys::{
        self,
        advanced::{declared, native, ReceiverConfiguration},
        bindings::*,
    },
    timeout::Deadline,
    AudioFrame, Error, MetadataFrame, Receiver, ReceiverOptions, Result, VideoFrame, NDI,
};

/// Which compressed formats a [`PacketReceiver`] passes through.
///
/// A frame in a format the preset names is delivered as the source sent it;
/// any other frame is delivered decoded, so every capture can yield either
/// kind. Decoded video is 8-bit unless the preset is one of the "best" ones,
/// which keep the source's bit depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PassthroughPreset {
    /// SpeedHQ only.
    SpeedHqOnly,
    /// SpeedHQ; other sources are decoded.
    SpeedHq,
    /// As [`SpeedHq`](Self::SpeedHq), decoding to the source's bit depth.
    SpeedHqBest,
    /// SpeedHQ and H.264.
    H264,
    /// As [`H264`](Self::H264), decoding to the source's bit depth.
    H264Best,
    /// As [`H264`](Self::H264), with compressed audio.
    H264WithAudio,
    /// SpeedHQ, H.264 and HEVC.
    Hevc,
    /// As [`Hevc`](Self::Hevc), decoding to the source's bit depth.
    HevcBest,
    /// As [`Hevc`](Self::Hevc), with compressed audio.
    HevcWithAudio,
    /// SpeedHQ, H.264 and HEVC, including H.264 and HEVC with alpha.
    Alpha,
    /// As [`Alpha`](Self::Alpha), decoding to the source's bit depth.
    AlphaBest,
    /// As [`Alpha`](Self::Alpha), with compressed audio including Opus.
    AlphaWithAudio,
    /// As [`Alpha`](Self::Alpha), for protected streams as well.
    Protected,
    /// As [`Protected`](Self::Protected), decoding to the source's bit depth.
    ProtectedBest,
    /// As [`Protected`](Self::Protected), with compressed audio including Opus.
    ProtectedWithAudio,
}

impl PassthroughPreset {
    /// The receiver color format that selects this preset.
    fn color_format(
        self,
        table: &'static ReceiverConfiguration,
    ) -> Result<NDIlib_recv_color_format_e> {
        native!(ReceiverConfiguration, table => {
            use sys::advanced_bindings::*;
            // The value of a constant, or its name when the headers this
            // build was made with do not declare it.
            macro_rules! optional_format {
                ($constant:ident) => {
                    declared! { $constant {
                        Ok(u32::from_ne_bytes($constant.to_ne_bytes()))
                    } else { Err(stringify!($constant)) } }
                };
            }
            let format: u32 = match self {
                Self::SpeedHqOnly => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v1)
                }
                Self::SpeedHq => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v2)
                }
                Self::SpeedHqBest => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v2_best)
                }
                Self::H264 => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v3)
                }
                Self::H264Best => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v3_best)
                }
                Self::H264WithAudio => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v3_with_audio)
                }
                Self::Hevc => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v4)
                }
                Self::HevcBest => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v4_best)
                }
                Self::HevcWithAudio => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v4_with_audio)
                }
                Self::Alpha => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v5)
                }
                Self::AlphaBest => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v5_best)
                }
                Self::AlphaWithAudio => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v5_with_audio)
                }
                Self::Protected => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v6)
                }
                Self::ProtectedBest => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v6_best)
                }
                Self::ProtectedWithAudio => {
                    optional_format!(NDIlib_recv_color_format_ex_e_NDIlib_recv_color_format_ex_compressed_v6_with_audio)
                }
            }.map_err(|constant: &'static str| Error::UnsupportedCapability {
                capability: "compressed receive preset",
                reason: crate::capabilities::CapabilityUnavailable::BuildAbiUnavailable,
                // The headers declare the constant without the enumeration's
                // name, which the generated binding prefixes it with.
                detail: constant
                    .strip_prefix("NDIlib_recv_color_format_ex_e_")
                    .unwrap_or(constant)
                    .to_owned(),
            })?;
            // The extended color formats take the target's C enum type; the
            // creation settings carry the same bits.
            Ok(NDIlib_recv_color_format_e::from_ne_bytes(format.to_ne_bytes()))
        })
    }
}

/// Video from a [`PacketReceiver`]: a codec packet when the source's format is
/// one the preset passes through, a decoded frame otherwise.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReceivedVideo {
    /// A decoded frame.
    Decoded(VideoFrame),
    /// A codec packet, exactly as the source sent it.
    Compressed(CompressedVideoFrame),
}

/// Audio from a [`PacketReceiver`]: a codec packet when the source's format is
/// one the preset passes through, decoded samples otherwise.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReceivedAudio {
    /// Decoded planar floating-point audio.
    Decoded(AudioFrame),
    /// A codec packet, exactly as the source sent it.
    Compressed(CompressedAudioFrame),
}

/// What one [`PacketReceiver::capture`] call produced.
#[derive(Debug)]
#[non_exhaustive]
pub enum PacketEvent {
    /// A video frame.
    Video(ReceivedVideo),
    /// An audio frame.
    Audio(ReceivedAudio),
    /// A metadata frame.
    Metadata(MetadataFrame),
    /// Nothing arrived within the timeout.
    Timeout,
    /// The connection or the source's properties changed.
    StatusChanged,
    /// The receiver is now connected to a different source.
    SourceChanged,
    /// A frame type this crate does not know, with its native value.
    Unknown(u32),
}

/// A receiver that delivers compressed packets where it can.
///
/// One capture call returns whichever frame arrives first, as a
/// [`PacketEvent`]; every result owns its data. The receiver is used from the
/// thread that owns it, one call at a time. It cannot feed a
/// [`FrameSync`](crate::framesync::FrameSync), which needs decoded frames.
///
/// # Examples
///
/// ```no_run
/// use std::time::Duration;
/// use grafton_ndi::{
///     advanced::{AdvancedConfig, PacketEvent, PacketReceiver, PassthroughPreset, ReceivedVideo},
///     ReceiverOptions, Source, NDI,
/// };
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = NDI::new()?;
/// # let source = Source::default();
/// let mut receiver = PacketReceiver::new(
///     &ndi,
///     &ReceiverOptions::builder(source).build(),
///     PassthroughPreset::Hevc,
///     &AdvancedConfig::parse("{}")?,
/// )?;
/// if let PacketEvent::Video(ReceivedVideo::Compressed(packet)) =
///     receiver.capture(Duration::from_secs(1))?
/// {
///     println!("{:?}: {} bytes", packet.format().codec(), packet.data().len());
/// }
/// # Ok(())
/// # }
/// ```
pub struct PacketReceiver {
    pub(crate) receiver: Receiver,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl PacketReceiver {
    /// Create a receiver that passes through the formats `preset` names.
    ///
    /// The options' color format is replaced by the preset; fielded video is
    /// always allowed, because a compressed stream cannot be deinterlaced.
    ///
    /// # Errors
    ///
    /// As for [`Receiver::new_with_config`].
    pub fn new(
        ndi: &NDI,
        options: &ReceiverOptions,
        preset: PassthroughPreset,
        config: &AdvancedConfig,
    ) -> Result<Self> {
        let table = ndi.table::<ReceiverConfiguration>()?;
        let color_format = preset.color_format(table)?;
        // SAFETY: the closure returns what the SDK's configured receiver
        // creation returned for the settings it was handed with the preset's
        // color format and fielded video; the strings the copy points to
        // belong to the creation path, which outlives the call.
        let receiver = unsafe {
            <Receiver as super::Configurable>::configured(
                ndi,
                options,
                "NDIlib_recv_create_v4",
                config,
                |table: &'static ReceiverConfiguration, settings, text| {
                    let settings = NDIlib_recv_create_v3_t {
                        color_format,
                        allow_video_fields: true,
                        ..*settings
                    };
                    native!(ReceiverConfiguration, table => table.NDIlib_recv_create_v4(&settings, text))
                },
            )
        }?;
        Ok(Self::from_receiver(receiver))
    }

    pub(crate) fn from_receiver(receiver: Receiver) -> Self {
        Self {
            receiver,
            _thread_affinity: PhantomData,
        }
    }

    /// Wait up to `timeout` for the next frame or event.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfiguration`] when `timeout` exceeds
    /// [`crate::MAX_TIMEOUT`], [`Error::CaptureFailed`] when the SDK reports a
    /// lost connection, and [`Error::InvalidFrame`] when a delivered frame
    /// fails validation.
    pub fn capture(&mut self, timeout: Duration) -> Result<PacketEvent> {
        Self::capture_from(&self.receiver, timeout)
    }

    /// One capture on `receiver`, under the same capture lock and timeout
    /// budget as the receiver's own capture verbs.
    pub(crate) fn capture_from(receiver: &Receiver, timeout: Duration) -> Result<PacketEvent> {
        let deadline = Deadline::after(timeout)?;
        let core = &*receiver.core;
        let mut video = NDIlib_video_frame_v2_t::default();
        let mut audio = NDIlib_audio_frame_v3_t::default();
        let mut metadata = NDIlib_metadata_frame_t::default();
        let kind = {
            // A connect or reconnect holds this lock exclusively. Waiting for
            // it spends the caller's budget; a zero timeout does not wait, and
            // when the lock is not available in time no capture is attempted.
            let Some(_capture) = core.capture_lock_until(deadline) else {
                return Ok(PacketEvent::Timeout);
            };
            let timeout_ms = deadline.remaining_ms();
            #[cfg(test)]
            native_capture_calls::record();
            // SAFETY: the receiver instance is live for this borrow, and the
            // three descriptors are writable locals. The capture lock held
            // here keeps the call from overlapping NDIlib_recv_connect on the
            // same instance, which Processing.NDI.Recv.h does not make safe.
            unsafe {
                core.sdk().NDIlib_recv_capture_v3(
                    core.instance,
                    &mut video,
                    &mut audio,
                    &mut metadata,
                    timeout_ms,
                )
            }
        };
        // In each frame arm, the SDK filled only the descriptor of the kind it
        // returned, for this instance, which was created through `ndi`. The
        // guard borrows the receiver, so the instance outlives the frame, and
        // it frees the frame exactly once through the matching
        // NDIlib_recv_free_* function, whether or not the copy succeeds.
        let (ndi, instance) = (&core._ndi, core.instance);
        match kind {
            NDIlib_frame_type_e_NDIlib_frame_type_video => {
                // SAFETY: `video` holds the delivered frame; see above.
                video_event(unsafe { Guard::<VideoKind>::new(ndi, instance, video) })
            }
            NDIlib_frame_type_e_NDIlib_frame_type_audio => {
                // SAFETY: `audio` holds the delivered frame; see above.
                audio_event(unsafe { Guard::<AudioKind>::new(ndi, instance, audio) })
            }
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                // SAFETY: `metadata` holds the delivered frame; see above.
                metadata_event(unsafe { Guard::<MetadataKind>::new(ndi, instance, metadata) })
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(PacketEvent::Timeout),
            NDIlib_frame_type_e_NDIlib_frame_type_status_change => Ok(PacketEvent::StatusChanged),
            NDIlib_frame_type_e_NDIlib_frame_type_source_change => Ok(PacketEvent::SourceChanged),
            NDIlib_frame_type_e_NDIlib_frame_type_error => Err(Error::CaptureFailed(
                "Packet receiver reported an SDK error".into(),
            )),
            other => Ok(PacketEvent::Unknown(other)),
        }
    }

    /// Switch between the source's program and preview streams; see
    /// [`Receiver::set_video_quality`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::set_video_quality`].
    pub fn set_video_quality(&mut self, quality: super::VideoQuality) -> Result<bool> {
        self.receiver.set_video_quality(quality)
    }

    /// Choose which streams the source sends; see [`Receiver::set_streams`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::set_streams`].
    pub fn set_streams(&mut self, streams: super::StreamSelection) -> Result<super::StreamChange> {
        self.receiver.set_streams(streams)
    }

    /// Ask the source for a keyframe, which a decoder needs before it can
    /// start on a compressed stream; see [`Receiver::request_keyframe`].
    ///
    /// # Errors
    ///
    /// As for [`Receiver::request_keyframe`].
    pub fn request_keyframe(&self) -> Result<()> {
        self.receiver.request_keyframe()
    }

    /// Send metadata upstream to the source. Returns whether the receiver is
    /// connected and the SDK accepted the frame.
    pub fn send_metadata(&self, metadata: &MetadataFrame) -> bool {
        self.receiver.send_metadata(metadata)
    }

    /// Whether the receiver is connected to its source.
    pub fn is_connected(&self) -> bool {
        self.receiver.is_connected()
    }
}

// Each delivered frame is held by the receiver's shared capture guard, which
// returns it to the SDK when dropped: after the copy, or when validation fails
// first.

/// Copy a delivered video frame: a codec packet as it is, any other frame as
/// the receiver's owned video capture copies it.
fn video_event(frame: Guard<'_, VideoKind>) -> Result<PacketEvent> {
    let video = if VideoCodec::from_outer(frame.frame().FourCC).is_some() {
        // SAFETY: the FourCC marks a compressed frame, whose descriptor holds
        // the size of its data. The guard keeps the frame's buffers allocated
        // until it is dropped, after the copy.
        ReceivedVideo::Compressed(unsafe { packets::copy_video_packet(frame.frame())? })
    } else {
        ReceivedVideo::Decoded(decoded(frame)?)
    };
    Ok(PacketEvent::Video(video))
}

/// Copy a delivered audio frame; see [`video_event`].
fn audio_event(frame: Guard<'_, AudioKind>) -> Result<PacketEvent> {
    let audio = if AudioCodec::from_outer(frame.frame().FourCC).is_some() {
        // SAFETY: as for video.
        ReceivedAudio::Compressed(unsafe { packets::copy_audio_packet(frame.frame())? })
    } else {
        ReceivedAudio::Decoded(decoded(frame)?)
    };
    Ok(PacketEvent::Audio(audio))
}

/// Copy a delivered metadata frame.
fn metadata_event(frame: Guard<'_, MetadataKind>) -> Result<PacketEvent> {
    decoded(frame).map(PacketEvent::Metadata)
}

/// Copy an uncompressed frame through the receiver's validated borrowed view
/// and its owned conversion, exactly as [`Receiver`] capture does.
fn decoded<K: CaptureKind>(frame: Guard<'_, K>) -> Result<K::Owned> {
    // SAFETY: a receiver guard of kind `K` holds a frame its capture delivered
    // as that kind; the view validates the rest before reading through it.
    let frame = unsafe { K::make_ref(frame)? };
    K::ref_to_owned(&frame)
}

/// Counts native capture calls on the calling thread, so that tests can tell
/// a capture that was skipped from one that found nothing.
#[cfg(test)]
mod native_capture_calls {
    use std::cell::Cell;
    std::thread_local! {
        static CALLS: Cell<usize> = const { Cell::new(0) };
    }
    pub(super) fn record() {
        CALLS.with(|calls| calls.set(calls.get() + 1));
    }
    pub(super) fn take() -> usize {
        CALLS.with(|calls| calls.replace(0))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        os::raw::c_char,
        ptr,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::{
        advanced::{packet_envelope, EnvelopeCodec, VideoPacketFormat},
        frames::PixelFormat,
    };

    unsafe fn count_release<T>(instance: NDIlib_recv_instance_t, _frame: &mut T) {
        // SAFETY: `deliver` keeps the counter alive through every guard.
        unsafe { &*instance.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
    }

    /// Hand `raw` to `event` as a delivered frame of kind `K`, and count how
    /// often the frame is released.
    fn deliver<K: CaptureKind>(
        raw: K::RawFrame,
        event: fn(Guard<'_, K>) -> Result<PacketEvent>,
    ) -> (Result<PacketEvent>, usize) {
        let releases = AtomicUsize::new(0);
        let instance = (&releases as *const AtomicUsize).cast_mut().cast();
        // SAFETY: every caller's descriptor refers to buffers it keeps alive
        // past this call, and the release only counts.
        let frame = unsafe { Guard::<K>::with_test_free(instance, raw, count_release) };
        let result = event(frame);
        (result, releases.load(Ordering::SeqCst))
    }

    const NOT_UTF8: &[u8] = b"\xff\0";

    #[test]
    fn delivered_video_is_copied_and_freed_once_even_when_invalid() {
        let mut source = VideoFrame::builder()
            .resolution(4, 2)
            .pixel_format(PixelFormat::BGRA)
            .timecode(17)
            .metadata("<frame/>")
            .build()
            .unwrap();
        for (index, byte) in source.data_mut().iter_mut().enumerate() {
            *byte = index as u8;
        }
        let (event, releases) = deliver::<VideoKind>(source.to_raw(), video_event);
        assert_eq!(releases, 1);
        let PacketEvent::Video(ReceivedVideo::Decoded(frame)) = event.unwrap() else {
            panic!("an uncompressed frame is decoded");
        };
        assert_eq!(frame.data(), source.data());
        assert_eq!((frame.width(), frame.height()), (4, 2));
        assert_eq!(frame.pixel_format(), PixelFormat::BGRA);
        assert_eq!(frame.timecode(), 17);
        assert_eq!(frame.metadata(), Some("<frame/>"));

        let no_pixels = NDIlib_video_frame_v2_t {
            p_data: ptr::null_mut(),
            ..source.to_raw()
        };
        let bad_metadata = NDIlib_video_frame_v2_t {
            p_metadata: NOT_UTF8.as_ptr().cast(),
            ..source.to_raw()
        };
        for raw in [no_pixels, bad_metadata] {
            let (event, releases) = deliver::<VideoKind>(raw, video_event);
            assert!(event.is_err());
            assert_eq!(releases, 1);
        }

        let format = VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap();
        let packet =
            packet_envelope(EnvelopeCodec::H264, 10, 9, true, &[1, 2, 3], &[4, 5]).unwrap();
        let mut sent = CompressedVideoFrame::new(format, packet.clone()).unwrap();
        let (event, releases) = deliver::<VideoKind>(sent.raw(), video_event);
        assert_eq!(releases, 1);
        let PacketEvent::Video(ReceivedVideo::Compressed(copied)) = event.unwrap() else {
            panic!("a codec packet is passed through");
        };
        assert_eq!(copied.data(), packet);
        assert_eq!(copied.format().codec(), VideoCodec::H264);

        let mut truncated = sent.raw();
        truncated.__bindgen_anon_1.data_size_in_bytes = 0;
        let (event, releases) = deliver::<VideoKind>(truncated, video_event);
        assert!(event.is_err());
        assert_eq!(releases, 1);
    }

    #[test]
    fn delivered_audio_is_copied_and_freed_once_even_when_invalid() {
        let mut source = AudioFrame::builder()
            .sample_rate(48_000)
            .channels(2)
            .samples(4)
            .timecode(23)
            .build()
            .unwrap();
        for (index, sample) in source.data_mut().iter_mut().enumerate() {
            *sample = index as f32;
        }
        let (event, releases) = deliver::<AudioKind>(source.to_raw(), audio_event);
        assert_eq!(releases, 1);
        let PacketEvent::Audio(ReceivedAudio::Decoded(frame)) = event.unwrap() else {
            panic!("uncompressed audio is decoded");
        };
        assert_eq!(frame.data(), source.data());
        assert_eq!((frame.sample_rate(), frame.num_channels()), (48_000, 2));
        assert_eq!(frame.timecode(), 23);

        let no_samples = NDIlib_audio_frame_v3_t {
            p_data: ptr::null_mut(),
            ..source.to_raw()
        };
        let bad_metadata = NDIlib_audio_frame_v3_t {
            p_metadata: NOT_UTF8.as_ptr().cast(),
            ..source.to_raw()
        };
        for raw in [no_samples, bad_metadata] {
            let (event, releases) = deliver::<AudioKind>(raw, audio_event);
            assert!(event.is_err());
            assert_eq!(releases, 1);
        }

        let mut sent =
            CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![1; 8]).unwrap();
        let (event, releases) = deliver::<AudioKind>(sent.raw(), audio_event);
        assert_eq!(releases, 1);
        let PacketEvent::Audio(ReceivedAudio::Compressed(copied)) = event.unwrap() else {
            panic!("a codec packet is passed through");
        };
        assert_eq!(copied.data(), &[1; 8]);
        assert_eq!(copied.codec(), AudioCodec::Opus);

        let mut truncated = sent.raw();
        truncated.__bindgen_anon_1.data_size_in_bytes = 0;
        let (event, releases) = deliver::<AudioKind>(truncated, audio_event);
        assert!(event.is_err());
        assert_eq!(releases, 1);
    }

    #[test]
    fn delivered_metadata_is_copied_and_freed_once_even_when_invalid() {
        let source = MetadataFrame::with_data("<tally/>", 31).unwrap();
        let (event, releases) = deliver::<MetadataKind>(source.to_raw(), metadata_event);
        assert_eq!(releases, 1);
        let PacketEvent::Metadata(frame) = event.unwrap() else {
            panic!("metadata is copied");
        };
        assert_eq!((frame.data(), frame.timecode()), ("<tally/>", 31));

        let bad_text = NDIlib_metadata_frame_t {
            length: NOT_UTF8.len() as i32,
            timecode: 0,
            p_data: NOT_UTF8.as_ptr().cast::<c_char>().cast_mut(),
        };
        let (event, releases) = deliver::<MetadataKind>(bad_text, metadata_event);
        assert!(matches!(event, Err(Error::InvalidUtf8(_))));
        assert_eq!(releases, 1);
    }

    #[test]
    fn packet_capture_takes_the_capture_lock_within_its_budget() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &ReceiverOptions::default()).unwrap();
        native_capture_calls::take();
        assert!(matches!(
            PacketReceiver::capture_from(&receiver, Duration::MAX),
            Err(Error::InvalidConfiguration(_))
        ));
        let reconnect = receiver.core.reconnect_lock();
        std::thread::scope(|scope| {
            let (done, completed) = std::sync::mpsc::channel();
            let receiver = &receiver;
            let worker = scope.spawn(move || {
                for timeout in [Duration::ZERO, Duration::from_millis(5)] {
                    assert!(matches!(
                        PacketReceiver::capture_from(receiver, timeout).unwrap(),
                        PacketEvent::Timeout
                    ));
                }
                done.send(native_capture_calls::take()).unwrap();
            });
            // The reconnect lock stays held until the worker has returned: a
            // wait for it without a bound would not finish.
            let calls = completed.recv_timeout(Duration::from_secs(2));
            drop(reconnect);
            worker.join().unwrap();
            // Neither capture reached the SDK while a reconnect held the lock.
            assert_eq!(calls.unwrap(), 0);
        });
        PacketReceiver::capture_from(&receiver, Duration::ZERO).unwrap();
        assert_eq!(native_capture_calls::take(), 1);
    }

    /// Only the thread auto traits are withheld; tests/ui/advanced/fail checks
    /// those.
    #[test]
    fn packet_receivers_stay_unwind_safe() {
        fn unwind_safe<T: std::panic::UnwindSafe>() {}
        unwind_safe::<PacketReceiver>();
        unwind_safe::<crate::advanced::AllocatedPacketReceiver>();
    }
}
