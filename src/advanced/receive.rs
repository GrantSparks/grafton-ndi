//! Receiving compressed packets instead of decoded frames.

#[cfg(test)]
use crate::capture::CaptureKind;
use std::time::Duration;

use super::{
    AdvancedConfig, AudioCodec, CompressedAudioFrame, CompressedAudioRef, CompressedVideoFrame,
    CompressedVideoRef, ReceiverControls, VideoCodec,
};
use crate::{
    capture::{AudioKind, CaptureStrategy, Delivered, Guard, MetadataKind, VideoKind},
    receiver::PacketAttached,
    sys::{
        advanced::{native, ReceiverConfiguration},
        bindings::*,
    },
    timeout::Deadline,
    AudioFrame, AudioFrameRef, MetadataFrame, MetadataFrameRef, Ndi, Receiver, ReceiverEvent,
    ReceiverOptions, Result, VideoFrame, VideoFrameRef,
};

/// Which compressed formats a [`PacketReceiver`] passes through.
///
/// A frame in a format the preset names is delivered as the source sent it;
/// any other frame is delivered decoded, so every capture can yield either
/// kind. Decoded video is 8-bit unless the preset is one of the "best" ones,
/// which keep the source's bit depth.
///
/// Whether a preset can be used is
/// [`RuntimeFeature::Passthrough`](crate::capabilities::RuntimeFeature::Passthrough):
/// the headers of an older SDK may not declare every preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

/// Video from a [`PacketReceiver`]: a codec packet when the source's format is
/// one the preset passes through, a decoded frame otherwise.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ReceivedVideo {
    /// A decoded frame.
    Decoded(VideoFrame),
    /// A codec packet, exactly as the source sent it.
    Compressed(CompressedVideoFrame),
}

/// Audio from a [`PacketReceiver`]: a codec packet when the source's format is
/// one the preset passes through, decoded samples otherwise.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ReceivedAudio {
    /// Decoded planar floating-point audio.
    Decoded(AudioFrame),
    /// A codec packet, exactly as the source sent it.
    Compressed(CompressedAudioFrame),
}

/// What one [`PacketReceiver::capture`] call produced.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum PacketItem {
    /// A video frame.
    Video(ReceivedVideo),
    /// An audio frame.
    Audio(ReceivedAudio),
    /// A metadata frame.
    Metadata(MetadataFrame),
    /// A receiver invalidation, with the same status snapshot and semantics
    /// as [`crate::ReceiverItem::Event`].
    Event(ReceiverEvent),
}

/// Borrowed video returned by a packet receiver, retaining its native buffer.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReceivedVideoRef<'rx> {
    /// Decoded pixels borrowed from the receiver.
    Decoded(VideoFrameRef<'rx>),
    /// Codec bytes borrowed in place.
    Compressed(CompressedVideoRef<'rx>),
}

impl ReceivedVideoRef<'_> {
    /// Copy into the corresponding owned packet or decoded frame.
    pub fn to_owned(&self) -> ReceivedVideo {
        match self {
            Self::Decoded(frame) => ReceivedVideo::Decoded(frame.to_owned()),
            Self::Compressed(frame) => ReceivedVideo::Compressed(frame.to_owned()),
        }
    }
}

/// Borrowed audio returned by a packet receiver.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReceivedAudioRef<'rx> {
    /// Decoded samples borrowed from the receiver.
    Decoded(AudioFrameRef<'rx>),
    /// Codec bytes borrowed in place.
    Compressed(CompressedAudioRef<'rx>),
}

impl ReceivedAudioRef<'_> {
    /// Copy into the corresponding owned packet or decoded samples.
    pub fn to_owned(&self) -> ReceivedAudio {
        match self {
            Self::Decoded(frame) => ReceivedAudio::Decoded(frame.to_owned()),
            Self::Compressed(frame) => ReceivedAudio::Compressed(frame.to_owned()),
        }
    }
}

/// One borrowed packet, decoded frame, metadata frame, or receiver event.
///
/// Media retains its SDK or custom-allocator buffer until dropped. Each
/// reference borrows the receiver, and may be moved or shared across threads.
#[derive(Debug)]
#[non_exhaustive]
pub enum PacketItemRef<'rx> {
    /// Video, decoded or compressed according to the preset.
    Video(ReceivedVideoRef<'rx>),
    /// Audio, decoded or compressed according to the preset.
    Audio(ReceivedAudioRef<'rx>),
    /// A standalone metadata frame.
    Metadata(MetadataFrameRef<'rx>),
    /// A coalesced receiver invalidation, as in owned capture.
    Event(ReceiverEvent),
}

impl PacketItemRef<'_> {
    /// Copy the item into independent owned storage. Events are copied directly.
    pub fn to_owned(&self) -> PacketItem {
        match self {
            Self::Video(frame) => PacketItem::Video(frame.to_owned()),
            Self::Audio(frame) => PacketItem::Audio(frame.to_owned()),
            Self::Metadata(frame) => PacketItem::Metadata(frame.to_owned()),
            Self::Event(event) => PacketItem::Event(*event),
        }
    }
}

/// A receiver that delivers compressed packets where it can.
///
/// One capture call returns whichever frame or event arrives first, as a
/// [`PacketItem`] from [`capture`](Self::capture), or a borrowed
/// [`PacketItemRef`] from [`capture_ref`](Self::capture_ref). It cannot feed a
/// [`FrameSync`](crate::framesync::FrameSync), which needs decoded frames.
///
/// [`receiver`](Self::receiver) and [`controls`](Self::controls) reach
/// the [`Receiver`] it holds, in the [`PacketAttached`] mode, for the common
/// controls: tally, PTZ, statistics, upstream metadata, keyframe requests,
/// stream selection, reconnecting and KVM. Its own frame and event capture is
/// unavailable, because packet capture takes them.
///
/// # Threading
///
/// A packet receiver is [`Send`] and [`Sync`], as the [`Receiver`] it holds
/// is: it can be created on one thread, capture on another and be dropped on a
/// third. Both capture methods take a shared reference, so packet references
/// coexist with shared receiver controls. Native captures run under the
/// receiver's shared capture guard; reconnecting takes the exclusive guard.
///
/// # Examples
///
/// ```no_run
/// use std::time::Duration;
/// use grafton_ndi::{
///     advanced::{PacketItem, PacketReceiver, PassthroughPreset, ReceivedVideo},
///     ReceiverOptions, Source, Ndi, Tally,
/// };
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = Ndi::new()?;
/// # let source = Source::new("ENCODER (HEVC)");
/// let mut receiver =
///     PacketReceiver::new(&ndi, &ReceiverOptions::new(source), PassthroughPreset::Hevc)?;
/// receiver.receiver().set_tally(Tally::new(true, false));
/// if let Some(PacketItem::Video(ReceivedVideo::Compressed(packet))) =
///     receiver.capture(Duration::from_secs(1))?
/// {
///     println!("{:?}: {} bytes", packet.format().codec(), packet.data().len());
/// }
/// # Ok(())
/// # }
/// ```
pub struct PacketReceiver {
    // Its Send and Sync are the receiver's. Nothing here ties a packet
    // receiver to a thread: it is created as an ordinary receiver is, with a
    // passthrough color format, and its capture is the receiver's
    // NDIlib_recv_capture_v3, which the SDK lets several threads call at
    // once (Processing.NDI.Recv.h), under the receiver's capture guard.
    // Borrowed results retain their Guard until dropped; its lifetime keeps
    // this owner alive. Owned capture copies through that same borrowed path.
    pub(crate) receiver: Receiver<PacketAttached>,
}

impl PacketReceiver {
    /// Create a receiver that passes through the formats `preset` names,
    /// with the options' Advanced SDK
    /// [configuration](ReceiverOptions::config), or the SDK's default one when
    /// they carry none.
    ///
    /// The options' color format is replaced by the preset, and is neither
    /// used nor checked against the runtime; fielded video is always allowed,
    /// because a compressed stream cannot be deinterlaced.
    ///
    /// # Errors
    ///
    /// As for [`Receiver::new`], with [`Error::UnsupportedCapability`]
    /// without an Advanced runtime (or in a build without the Advanced SDK),
    /// and reported for [`RuntimeFeature::Passthrough`] with this preset.
    ///
    /// [`Error::UnsupportedCapability`]: crate::Error::UnsupportedCapability
    /// [`RuntimeFeature::Passthrough`]: crate::capabilities::RuntimeFeature::Passthrough
    pub fn new(ndi: &Ndi, options: &ReceiverOptions, preset: PassthroughPreset) -> Result<Self> {
        let options = creation_options(options, ndi.passthrough_format(preset)?);
        let config = AdvancedConfig::or_empty(options.config.as_ref());
        // SAFETY: the closure returns what the SDK's configured receiver
        // creation returned for the settings it was handed, which carry the
        // preset's color format and fielded video; the strings they point to
        // belong to the creation path, which outlives the call.
        let receiver = unsafe {
            <Receiver as super::Configurable>::configured(
                ndi,
                &options,
                "NDIlib_recv_create_v4",
                &config,
                |table: &'static ReceiverConfiguration, _settings, _text| native!(ReceiverConfiguration, table => table.NDIlib_recv_create_v4(_settings, _text)),
            )
        }?;
        Ok(Self::from_receiver(receiver.into_mode()))
    }

    pub(crate) fn from_receiver(receiver: Receiver<PacketAttached>) -> Self {
        Self { receiver }
    }

    /// The receiver this holds, for the common controls: tally, PTZ,
    /// statistics, upstream metadata, keyframe requests, delays, reconnecting
    /// and source-name changes.
    ///
    /// Its own frame and event capture is unavailable in the
    /// [`PacketAttached`] mode; [`capture`](Self::capture) delivers both.
    pub fn receiver(&self) -> &Receiver<PacketAttached> {
        &self.receiver
    }

    /// Borrow exclusive stream controls or start KVM, without permitting
    /// receiver replacement. This allocates nothing and makes no SDK call.
    /// A KVM session captures packets through its own `capture` method.
    pub fn controls(&mut self) -> ReceiverControls<'_, PacketAttached> {
        ReceiverControls::new(&mut self.receiver)
    }

    /// Wait up to `timeout` for the next frame or event, or return `None`
    /// when none arrived in time.
    ///
    /// A delivered packet may have been queued; a timeout alone does not
    /// establish disconnection. Use the attached receiver's per-media
    /// counters as described in the
    /// [reception monitoring guide](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/usage.md#monitor-reception-of-the-expected-media).
    /// Notifications are coalesced [`ReceiverEvent`] invalidations, consumed
    /// once. Status changes include the current connection count, as in raw
    /// mixed capture; pending notifications are delivered before waiting.
    ///
    /// The timeout is spent as [`Capture::capture`](crate::Capture::capture)
    /// spends it. The SDK returns early, with neither a frame nor an event,
    /// while a new connection synchronizes; this captures again until
    /// something arrives, and returns `None` only once the whole timeout is
    /// spent. [`Duration::ZERO`] makes one attempt without waiting. Every SDK
    /// call holds the receiver's shared capture guard for one attempt of at
    /// most 100 ms, so none overlaps a reconnect and a reconnect waits for one
    /// attempt at most; waiting for that guard spends the same timeout.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds
    /// [`crate::MAX_TIMEOUT`], [`Error::CaptureFailed`] when the SDK reports a
    /// lost connection, and [`Error::InvalidFrame`] when a delivered frame
    /// fails validation.
    ///
    /// [`Error::InvalidInput`]: crate::Error::InvalidInput
    /// [`Error::CaptureFailed`]: crate::Error::CaptureFailed
    /// [`Error::InvalidFrame`]: crate::Error::InvalidFrame
    pub fn capture(&self, timeout: Duration) -> Result<Option<PacketItem>> {
        Self::capture_from(&self.receiver, timeout)
    }

    /// Borrow the next packet or decoded frame without copying its payload or
    /// metadata. The item keeps its native buffer until dropped, including
    /// application memory charged to a custom allocator's limits.
    ///
    /// Timeout, event and reconnect semantics are those of [`Self::capture`].
    /// Shared receiver controls remain available while an item is borrowed.
    ///
    /// # Errors
    ///
    /// As for [`Self::capture`].
    pub fn capture_ref(&self, timeout: Duration) -> Result<Option<PacketItemRef<'_>>> {
        Self::capture_ref_from(&self.receiver, timeout)
    }

    pub(crate) fn capture_from(
        receiver: &Receiver<PacketAttached>,
        timeout: Duration,
    ) -> Result<Option<PacketItem>> {
        Ok(Self::capture_ref_from(receiver, timeout)?.map(|item| item.to_owned()))
    }

    pub(crate) fn capture_ref_from(
        receiver: &Receiver<PacketAttached>,
        timeout: Duration,
    ) -> Result<Option<PacketItemRef<'_>>> {
        receiver.core.capture_any_until_with(
            Deadline::after(timeout)?,
            crate::receiver::NeverStop,
            PacketItemRef::Event,
            |delivered| Self::capture_result_ref(receiver, delivered),
        )
    }

    fn capture_result_ref<'rx>(
        receiver: &'rx Receiver<PacketAttached>,
        delivered: Delivered<'rx>,
    ) -> Result<Option<PacketItemRef<'rx>>> {
        let item = match delivered {
            Delivered::Video(frame) => video_ref_event(frame)?,
            Delivered::Audio(frame) => audio_ref_event(frame)?,
            Delivered::Metadata(frame) => metadata_ref_event(frame)?,
            Delivered::Notification(notification) => {
                return Ok(receiver.core.event(notification).map(PacketItemRef::Event));
            }
        };
        Ok(Some(item))
    }

    #[cfg(test)]
    fn capture_result(
        receiver: &Receiver<PacketAttached>,
        delivered: Delivered<'_>,
    ) -> Result<Option<PacketItem>> {
        Ok(Self::capture_result_ref(receiver, delivered)?.map(|item| item.to_owned()))
    }
}

/// The options a packet receiver is created from: `options` with the preset's
/// native `color_format` in place of their own, and with video fields
/// allowed, because a compressed stream cannot be deinterlaced.
///
/// The creation path checks these against the runtime, so it checks what the
/// SDK is given: a color format the preset replaces is never required of the
/// runtime.
fn creation_options(
    options: &ReceiverOptions,
    color_format: NDIlib_recv_color_format_e,
) -> ReceiverOptions {
    options
        .clone()
        .with_passthrough(color_format)
        .allow_video_fields(true)
}

fn video_ref_event(frame: Guard<'_, VideoKind>) -> Result<PacketItemRef<'_>> {
    let video = if VideoCodec::from_outer(frame.frame().FourCC).is_some() {
        ReceivedVideoRef::Compressed(CompressedVideoRef::new(frame)?)
    } else {
        // SAFETY: this delivered guard owns the SDK's uncompressed frame;
        // construction validates its descriptor while retaining its buffers.
        ReceivedVideoRef::Decoded(unsafe { VideoKind::make_ref(frame)? })
    };
    Ok(PacketItemRef::Video(video))
}

fn audio_ref_event(frame: Guard<'_, AudioKind>) -> Result<PacketItemRef<'_>> {
    let audio = if AudioCodec::from_outer(frame.frame().FourCC).is_some() {
        ReceivedAudioRef::Compressed(CompressedAudioRef::new(frame)?)
    } else {
        // SAFETY: as for decoded video, with this audio guard's sample storage.
        ReceivedAudioRef::Decoded(unsafe { AudioKind::make_ref(frame)? })
    };
    Ok(PacketItemRef::Audio(audio))
}

fn metadata_ref_event(frame: Guard<'_, MetadataKind>) -> Result<PacketItemRef<'_>> {
    // SAFETY: the delivered guard retains this standalone metadata allocation
    // while the validated reference borrows it.
    Ok(PacketItemRef::Metadata(unsafe {
        MetadataKind::make_ref(frame)?
    }))
}

#[cfg(test)]
fn video_event(frame: Guard<'_, VideoKind>) -> Result<PacketItem> {
    video_ref_event(frame).map(|frame| frame.to_owned())
}
#[cfg(test)]
fn audio_event(frame: Guard<'_, AudioKind>) -> Result<PacketItem> {
    audio_ref_event(frame).map(|frame| frame.to_owned())
}
#[cfg(test)]
fn metadata_event(frame: Guard<'_, MetadataKind>) -> Result<PacketItem> {
    metadata_ref_event(frame).map(|frame| frame.to_owned())
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
        advanced::{EnvelopeCodec, PacketEnvelope, VideoPacketFormat},
        capture::Notification,
        frames::PixelFormat,
        receiver::native_capture_calls,
        Error, Timecode,
    };

    #[test]
    fn packet_notifications_use_the_shared_receiver_event_payloads() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let receiver = Receiver::new(&ndi, &ReceiverOptions::new(crate::Source::new("")))
            .unwrap()
            .into_mode();
        let Some(PacketItem::Event(ReceiverEvent::StatusChanged(status))) =
            PacketReceiver::capture_result(
                &receiver,
                Delivered::Notification(Notification::StatusChanged),
            )
            .unwrap()
        else {
            panic!("status changes include a receiver status snapshot");
        };
        assert_eq!(
            status.connections(),
            receiver.connection_stats().connections()
        );
        for (notification, expected) in [
            (Notification::SourceChanged, ReceiverEvent::SourceChanged),
            (Notification::Unknown(987), ReceiverEvent::Unknown(987)),
        ] {
            let Some(PacketItem::Event(event)) =
                PacketReceiver::capture_result(&receiver, Delivered::Notification(notification))
                    .unwrap()
            else {
                panic!("expected receiver event {expected:?}");
            };
            assert_eq!(event, expected);
        }
        assert!(PacketReceiver::capture_result(
            &receiver,
            Delivered::Notification(Notification::None),
        )
        .unwrap()
        .is_none());
    }

    unsafe fn count_release<T>(instance: NDIlib_recv_instance_t, _frame: &mut T) {
        // SAFETY: `deliver` keeps the counter alive through every guard.
        unsafe { &*instance.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
    }

    /// Hand `raw` to `event` as a delivered frame of kind `K`, and count how
    /// often the frame is released.
    fn deliver<K: CaptureKind>(
        raw: K::RawFrame,
        event: fn(Guard<'_, K>) -> Result<PacketItem>,
    ) -> (Result<PacketItem>, usize) {
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
            .pixel_format(PixelFormat::Bgra)
            .timecode(Timecode::from_ticks(17))
            .metadata("<frame/>")
            .build()
            .unwrap();
        for (index, byte) in source.data_mut().iter_mut().enumerate() {
            *byte = index as u8;
        }
        let (event, releases) = deliver::<VideoKind>(source.as_test_raw(), video_event);
        assert_eq!(releases, 1);
        let PacketItem::Video(ReceivedVideo::Decoded(frame)) = event.unwrap() else {
            panic!("an uncompressed frame is decoded");
        };
        assert_eq!(frame.data(), source.data());
        assert_eq!((frame.width(), frame.height()), (4, 2));
        assert_eq!(frame.pixel_format(), PixelFormat::Bgra);
        assert_eq!(frame.timecode(), Timecode::from_ticks(17));
        assert_eq!(frame.metadata(), Some("<frame/>"));

        let no_pixels = NDIlib_video_frame_v2_t {
            p_data: ptr::null_mut(),
            ..source.as_test_raw()
        };
        let bad_metadata = NDIlib_video_frame_v2_t {
            p_metadata: NOT_UTF8.as_ptr().cast(),
            ..source.as_test_raw()
        };
        let (event, releases) = deliver::<VideoKind>(bad_metadata, video_event);
        let PacketItem::Video(ReceivedVideo::Decoded(frame)) = event.unwrap() else {
            panic!("valid media survives unreadable optional metadata");
        };
        assert_eq!(frame.data(), source.data());
        assert_eq!(frame.metadata(), None);
        assert_eq!(releases, 1);
        let (event, releases) = deliver::<VideoKind>(no_pixels, video_event);
        assert!(event.is_err());
        assert_eq!(releases, 1);

        let format = VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap();
        let packet =
            PacketEnvelope::encode(EnvelopeCodec::H264, 10, 9, true, &[1, 2, 3], &[4, 5]).unwrap();
        let mut sent = CompressedVideoFrame::new(format, packet.clone()).unwrap();
        let (event, releases) = deliver::<VideoKind>(sent.raw(), video_event);
        assert_eq!(releases, 1);
        let PacketItem::Video(ReceivedVideo::Compressed(copied)) = event.unwrap() else {
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
            .num_channels(2)
            .num_samples(4)
            .timecode(Timecode::from_ticks(23))
            .build()
            .unwrap();
        for (index, sample) in source.data_mut().iter_mut().enumerate() {
            *sample = index as f32;
        }
        let (event, releases) = deliver::<AudioKind>(source.to_raw(), audio_event);
        assert_eq!(releases, 1);
        let PacketItem::Audio(ReceivedAudio::Decoded(frame)) = event.unwrap() else {
            panic!("uncompressed audio is decoded");
        };
        assert_eq!(frame.data(), source.data());
        assert_eq!((frame.sample_rate(), frame.num_channels()), (48_000, 2));
        assert_eq!(frame.timecode(), Timecode::from_ticks(23));

        let no_samples = NDIlib_audio_frame_v3_t {
            p_data: ptr::null_mut(),
            ..source.to_raw()
        };
        let bad_metadata = NDIlib_audio_frame_v3_t {
            p_metadata: NOT_UTF8.as_ptr().cast(),
            ..source.to_raw()
        };
        let (event, releases) = deliver::<AudioKind>(bad_metadata, audio_event);
        let PacketItem::Audio(ReceivedAudio::Decoded(frame)) = event.unwrap() else {
            panic!("valid media survives unreadable optional metadata");
        };
        assert_eq!(frame.data(), source.data());
        assert_eq!(frame.metadata(), None);
        assert_eq!(releases, 1);
        let (event, releases) = deliver::<AudioKind>(no_samples, audio_event);
        assert!(event.is_err());
        assert_eq!(releases, 1);

        let mut sent =
            CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![1; 8]).unwrap();
        let (event, releases) = deliver::<AudioKind>(sent.raw(), audio_event);
        assert_eq!(releases, 1);
        let PacketItem::Audio(ReceivedAudio::Compressed(copied)) = event.unwrap() else {
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
        let source = MetadataFrame::new("<tally/>")
            .unwrap()
            .with_timecode(Timecode::from_ticks(31));
        let (event, releases) = deliver::<MetadataKind>(source.to_raw(), metadata_event);
        assert_eq!(releases, 1);
        let PacketItem::Metadata(frame) = event.unwrap() else {
            panic!("metadata is copied");
        };
        assert_eq!(
            (frame.data(), frame.timecode()),
            ("<tally/>", Timecode::from_ticks(31))
        );

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
        let receiver: Receiver<PacketAttached> =
            Receiver::new(&ndi, &ReceiverOptions::new(crate::Source::new("")))
                .unwrap()
                .into_mode();
        native_capture_calls::take();
        assert!(matches!(
            PacketReceiver::capture_from(&receiver, Duration::MAX),
            Err(Error::InvalidInput(_))
        ));
        let reconnect = receiver.core.reconnect_lock();
        std::thread::scope(|scope| {
            let (done, completed) = std::sync::mpsc::channel();
            let receiver = &receiver;
            let worker = scope.spawn(move || {
                for timeout in [Duration::ZERO, Duration::from_millis(5)] {
                    assert!(PacketReceiver::capture_from(receiver, timeout)
                        .unwrap()
                        .is_none());
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

        // Nothing is connected, so nothing arrives: the capture waits out its
        // whole budget, capturing again as often as the SDK returns early.
        let budget = Duration::from_millis(250);
        let began = std::time::Instant::now();
        assert!(PacketReceiver::capture_from(&receiver, budget)
            .unwrap()
            .is_none());
        assert!(began.elapsed() >= budget);
        assert!(native_capture_calls::take() >= 1);
    }

    /// A packet receiver is created from the preset's color format, so that is
    /// the one checked against the runtime: options asking for Best444, which
    /// a 6.3.2 runtime is too old for, still make a packet receiver there.
    #[test]
    fn packet_receivers_validate_the_color_format_the_sdk_is_given() {
        use crate::{
            capabilities::{
                Capabilities, CapabilityUnavailable, RuntimeFeature, RuntimeInfo, SdkEdition,
                SdkLinkage, SdkVersion,
            },
            sys::{advanced::AdvancedApi, table::ExportsWhere, Api, Evidence},
            ReceiverColorFormat, Source,
        };

        // An Advanced 6.3.2 runtime that exports every function the crate
        // knows, gated as the loader gates the runtime it selects.
        let exports = ExportsWhere(|_: &str| true);
        let (api, advanced) = (
            Api::load(&exports).unwrap(),
            AdvancedApi::load(&exports).unwrap(),
        );
        let version = SdkVersion::parse("NDI SDK TEST 6.3.2.0");
        let capabilities = Capabilities::detect(&api, &advanced, version, SdkEdition::Advanced);
        let info = RuntimeInfo {
            edition: SdkEdition::Advanced,
            linkage: SdkLinkage::Shared,
            library_path: None,
            raw_version: "NDI SDK TEST 6.3.2.0".into(),
            version,
        };
        let runtime = Evidence {
            api: &api,
            advanced: &advanced,
            info: &info,
            capabilities: &capabilities,
        };
        let require = |feature| runtime.require(feature);
        let too_old = |result: Result<()>, wanted: RuntimeFeature| {
            assert!(
                matches!(
                    result,
                    Err(Error::UnsupportedCapability {
                        feature,
                        reason: CapabilityUnavailable::RuntimeTooOld,
                        ..
                    }) if feature == wanted
                ),
                "{result:?}"
            );
        };

        let best = ReceiverOptions::new(Source::new(""))
            .color_format(ReceiverColorFormat::Best444)
            .allow_video_fields(false);
        // An ordinary receiver would be given Best444.
        too_old(
            best.validate_runtime(require),
            RuntimeFeature::ReceiveBest444,
        );

        let mut checked = 0;
        for preset in PassthroughPreset::ALL {
            let Ok(color_format) = runtime.passthrough_format(preset) else {
                // Only a preset this build's headers do not declare.
                assert!(!AdvancedApi::declares(RuntimeFeature::Passthrough(preset)));
                continue;
            };
            let packets = creation_options(&best, color_format);
            packets.validate_runtime(require).unwrap();
            let settings = packets.as_test_raw().unwrap().raw;
            assert_eq!(settings.color_format, color_format, "{preset:?}");
            assert!(settings.allow_video_fields);

            // What the SDK is given is still checked: video-only bandwidth
            // needs NDI 6.4 too.
            let video_only = best
                .clone()
                .bandwidth(crate::ReceiverBandwidth::VideoOnlyHighest);
            too_old(
                creation_options(&video_only, color_format).validate_runtime(require),
                RuntimeFeature::ReceiveVideoOnly,
            );
            checked += 1;
        }
        let declared = PassthroughPreset::ALL
            .into_iter()
            .filter(|&preset| AdvancedApi::declares(RuntimeFeature::Passthrough(preset)))
            .count();
        assert_eq!(checked, declared);
    }

    /// Both packet receivers move between and are shared across threads, and
    /// stay unwind safe.
    #[test]
    fn packet_receivers_are_send_sync_and_unwind_safe() {
        fn traits<T: Send + Sync + std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
        traits::<PacketReceiver>();
        traits::<crate::advanced::AllocatedPacketReceiver>();
    }
}

#[cfg(test)]
#[path = "receive/borrowed_tests.rs"]
mod borrowed_tests;
