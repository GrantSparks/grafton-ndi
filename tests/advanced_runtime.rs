//! Advanced SDK operations against a real runtime, using only peers these
//! tests create.
//!
//! The tests that need an Advanced SDK runtime start with `advanced_ndi()`, so
//! they are skipped visibly elsewhere and fail where
//! `GRAFTON_NDI_REQUIRE_RUNTIME` promises `advanced`. They compile in every
//! build: the Advanced API is the same with and without the SDK's headers.
#![cfg(feature = "advanced")]

#[path = "support/hx_fixture.rs"]
mod hx_fixture;

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(
    runtime,
    prerequisites,
    name,
    advanced,
    hx,
    unsupported,
    feature,
    loopback,
    endpoint,
    socket,
    discovery_server
);

use common::Requirement::{H264Decoder, Hevc444Decoder};
use grafton_ndi::{
    advanced::*,
    capabilities::{CapabilityStatus, RuntimeFeature, SdkEdition},
    AudioFrame, Error, Finder, FinderOptions, FrameRate, MediaTypes, MetadataFrame, Ndi,
    PictureAspectRatio, Receiver, ReceiverBandwidth, ReceiverColorFormat, ReceiverMode,
    ReceiverOptions, ScanType, Sender, SenderMode, SenderOptions, SettingChange, Source,
    SourceCache, StreamTier, Tally, Timecode, VideoFrame, VideoFrameRef,
};
use std::{
    alloc::{alloc_zeroed, dealloc, Layout},
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

/// How long a test waits for something its own peers must deliver.
const PATIENCE: Duration = Duration::from_secs(15);

/// One capture while waiting; a wait is made of as many as it needs.
const SLICE: Duration = Duration::from_millis(100);

/// The tests that create peers take turns: each waits for what its own
/// senders deliver, and peers of several tests at once would compete for the
/// machine's sockets and time.
fn sockets() -> MutexGuard<'static, ()> {
    static SOCKETS: Mutex<()> = Mutex::new(());
    SOCKETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn options(suffix: &str) -> SenderOptions {
    SenderOptions::new(format!("grafton-local-{}-{suffix}", std::process::id()))
        .clock_video(false)
        .clock_audio(false)
}

fn video() -> VideoFrame {
    VideoFrame::builder().resolution(64, 32).build().unwrap()
}

fn unconnected_receiver(ndi: &Ndi) -> Receiver {
    Receiver::new(ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap()
}

/// What `receiver` says about its connection, for the message of a test that
/// waited for media in vain: the source it was given, and its connection
/// count and frame counters, which tell a receiver that never connected from
/// one that connected and was given nothing. On Windows the TCP table of the
/// sender's port follows (see `common::socket_table`).
fn link<M: ReceiverMode>(receiver: &Receiver<M>) -> String {
    format!(
        "the receiver of {:?} reports {:?}{}",
        receiver.connection_target(),
        receiver.connection_stats().unwrap(),
        tcp_table(&receiver.connection_target())
    )
}

/// The TCP table of the port `source` names, on a line of its own, or nothing
/// where [`common::socket_table`] collects none.
fn tcp_table(source: &Source) -> String {
    common::port_of(source)
        .and_then(common::socket_table)
        .map_or_else(String::new, |table| format!("\n{table}"))
}

/// How many receivers `sender` counts, for the same messages.
fn served<M: SenderMode>(sender: &Sender<M>) -> String {
    match sender.connection_count() {
        Ok(count) => format!("the sender counts {count} connections"),
        Err(error) => format!("the sender's connection count failed: {error}"),
    }
}

#[test]
fn advanced_native_constructors_controls_and_queue() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let config = AdvancedConfig::parse("{}").unwrap();
    let sender = Sender::new(&ndi, &options("controls").config(config.clone())).unwrap();
    let source = sender.source().unwrap();
    let mut receiver =
        Receiver::new(&ndi, &ReceiverOptions::new(source).config(config.clone())).unwrap();
    let finder = Finder::new(&ndi, &FinderOptions::new().config(config.clone())).unwrap();
    let _snapshot = finder.current_sources_with_metadata().unwrap();
    sender.set_external_tally(Tally::new(false, false)).unwrap();
    let _ = sender.keyframe_required(None).unwrap();
    let _ = sender
        .wait_for_keyframe_change(None, Duration::ZERO)
        .unwrap();
    receiver.request_keyframe().unwrap();
    // Without a vendor ID licensed for them the SDK applies neither change.
    for change in [
        receiver.set_stream_tier(StreamTier::Program).unwrap(),
        receiver.set_streams(MediaTypes::ALL).unwrap(),
    ] {
        assert!(matches!(
            change,
            SettingChange::Applied | SettingChange::Unchanged
        ));
    }
    {
        let session = receiver.kvm().unwrap();
        let _ = session.is_supported();
    }
    {
        // Nothing is connected, so the SDK sends no KVM message, and the
        // session records nothing as pressed.
        let mut unconnected = unconnected_receiver(&ndi);
        let mut session = unconnected.kvm().unwrap();
        assert!(!session.is_supported());
        assert!(matches!(
            session.mouse_down(MouseButton::Left),
            Err(Error::NativeOperationFailed {
                operation}) if operation.name() == c"NDIlib_recv_kvm_send_left_mouse_click"
        ));
        assert!(matches!(
            session.key_down(0xff0d),
            Err(Error::NativeOperationFailed {
                operation}) if operation.name() == c"NDIlib_recv_kvm_send_keyboard_press"
        ));
        assert!(matches!(
            session.move_pointer(0.5, 1.5),
            Err(Error::InvalidInput(_))
        ));
        session.release_all().unwrap();
    }
    // Quality guidance applies to SpeedHQ alone, and is refused for any other
    // codec before the SDK is asked.
    let speedhq = VideoPacketFormat::new(VideoCodec::SpeedHq422, 1920, 1080).unwrap();
    let h264 = VideoPacketFormat::new(VideoCodec::H264, 1920, 1080).unwrap();
    for format in [speedhq, h264] {
        sender.target_frame_bytes(format).unwrap();
        sender.target_bit_rate(format).unwrap();
    }
    assert!(sender.legacy_quality(speedhq).unwrap() < 100);
    sender.quality_plan(speedhq, true).unwrap();
    assert!(matches!(
        sender.legacy_quality(h264),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        sender.quality_plan(h264, false),
        Err(Error::InvalidInput(_))
    ));
    let mut router = grafton_ndi::routing::Router::new(
        &ndi,
        &grafton_ndi::routing::RouterOptions::new(format!(
            "grafton-local-{}-router",
            std::process::id()
        ))
        .config(config.clone()),
    )
    .unwrap();
    router
        .add_connection_metadata(
            &grafton_ndi::MetadataFrame::new("<local-test/>")
                .unwrap()
                .with_timecode(grafton_ndi::Timecode::from_ticks(0)),
        )
        .unwrap();
    router.clear_connection_metadata().unwrap();
    use grafton_ndi::discovery_server::{
        AdvertiserOptions, ListenerOptions, RecvAdvertiser, RecvListener, SendAdvertiser,
        SendListener,
    };
    // The Discovery Server objects need NDI 6.4.0. An Advanced runtime older
    // than that refuses to create them, with or without a configuration.
    fn created<T>(ndi: &Ndi, feature: RuntimeFeature, object: grafton_ndi::Result<T>) -> Option<T> {
        match (ndi.require_feature(feature), object) {
            (Ok(()), object) => Some(object.unwrap()),
            (Err(_), Err(Error::UnsupportedCapability { .. })) => None,
            (Err(gate), Err(other)) => panic!("{gate}, but creation failed with: {other}"),
            (Err(gate), Ok(_)) => panic!("{gate}, but the object was created"),
        }
    }
    let advertisers = AdvertiserOptions::default()
        .servers(["127.0.0.1"])
        .config(config.clone());
    let listeners = ListenerOptions::default()
        .server("127.0.0.1")
        .config(config.clone());
    let _send_advertiser = created(
        &ndi,
        RuntimeFeature::SenderAdvertising,
        SendAdvertiser::new(&ndi, &advertisers),
    );
    let _recv_advertiser = created(
        &ndi,
        RuntimeFeature::ReceiverAdvertising,
        RecvAdvertiser::new(&ndi, &advertisers),
    );
    let _send_listener = created(
        &ndi,
        RuntimeFeature::SenderDiscovery,
        SendListener::new(&ndi, &listeners),
    );
    let _recv_listener = created(
        &ndi,
        RuntimeFeature::ReceiverDiscovery,
        RecvListener::new(&ndi, &listeners),
    );
    // An attached receiver keeps every control and its video and metadata
    // capture, while the synchronizer consumes its audio and notifications.
    let mut av = AvSync::new(receiver).unwrap();
    {
        let receiver = av.receiver();
        receiver.set_tally(Tally::new(true, false));
        let _ = receiver.connection_stats().unwrap();
        let _ = receiver.ptz().is_supported();
        receiver.request_keyframe().unwrap();
        let _ = receiver.capture_metadata(Duration::ZERO).unwrap();
        let _ = receiver.capture_video_ref(Duration::ZERO).unwrap();
        receiver.reconnect();
    }
    for change in [
        av.controls().set_stream_tier(StreamTier::Program).unwrap(),
        av.controls().set_streams(MediaTypes::ALL).unwrap(),
    ] {
        assert!(matches!(
            change,
            SettingChange::Applied | SettingChange::Unchanged
        ));
    }
    let _ = av.controls().kvm().unwrap().is_supported();
    drop(av.into_receiver());
    let mut genlock = Genlock::new(&ndi, &GenlockOptions::new().config(config.clone())).unwrap();
    genlock.disconnect();
    let _ = genlock.is_active();
    drop(genlock);
    let packets = PacketReceiver::new(
        &ndi,
        &ReceiverOptions::new(Source::new("").unwrap()).config(config.clone()),
        PassthroughPreset::HevcWithAudio,
    )
    .unwrap();
    let _ = packets.capture_any(Duration::ZERO).unwrap();
    // The preset's color format replaces the options' own, so only the
    // preset's is required of the runtime: Best444, which needs NDI 6.4, is
    // never asked for.
    drop(
        PacketReceiver::new(
            &ndi,
            &ReceiverOptions::new(Source::new("").unwrap())
                .color_format(ReceiverColorFormat::Best444),
            PassthroughPreset::HevcWithAudio,
        )
        .unwrap(),
    );
    // Nothing is connected, so there is nowhere to send metadata.
    assert!(matches!(
        packets
            .receiver()
            .send_metadata(&grafton_ndi::MetadataFrame::new("<local-test/>").unwrap()),
        Err(Error::NotConnected)
    ));
    let compressed =
        CompressedSender::new(&ndi, &options("compressed").config(config.clone())).unwrap();
    let reclaimed = compressed.close();
    assert!(reclaimed.program.is_none() && reclaimed.preview.is_none());
    let mut queue = QueuedSender::new(
        &ndi,
        &options("queue").config(config),
        &QueueLimits::default().max_frames(2),
    )
    .unwrap();
    let id = queue.try_submit(video()).unwrap();
    let completed = queue
        .next_completed(Duration::from_secs(5))
        .unwrap()
        .expect("native completion without receivers");
    assert_eq!(id, completed.id());
}

/// An allocation made under one policy and freed after that policy was
/// replaced must still be released: when the receiver is gone, nothing may
/// remain charged to the process.
#[test]
fn advanced_native_allocator_replacement_and_capture() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let sender = Sender::new(&ndi, &options("allocation")).unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &sender).unwrap()),
    )
    .unwrap();
    let service =
        AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
    let mut receiver = service.attach(receiver).unwrap();
    let audio_count = Arc::new(AtomicUsize::new(0));
    let count = audio_count.clone();
    receiver
        .set_audio_allocator(move |request: AllocationRequest| {
            count.fetch_add(1, Ordering::Relaxed);
            AllocationBuffer::zeroed(request.with_stride_bytes(request.stride_bytes() + 64)?)
        })
        .unwrap();
    let audio = AudioFrame::builder()
        .sample_rate(48_000)
        .num_channels(2)
        .num_samples(1600)
        .build()
        .unwrap();
    let frame = video();
    let mut video_allocations = 0;
    for generation in 0..3 {
        let count = Arc::new(AtomicUsize::new(0));
        let allocation_count = count.clone();
        // Replacing the policy while the receiver is running leaves frames of
        // the earlier generation queued inside the SDK.
        receiver
            .set_video_allocator(move |request: AllocationRequest| {
                allocation_count.fetch_add(1, Ordering::Relaxed);
                AllocationBuffer::zeroed(request.with_stride_bytes(request.stride_bytes() + 64)?)
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut captured = false;
        let mut last_error = None;
        while Instant::now() < deadline {
            sender.send_video(&frame).unwrap();
            sender.send_audio(&audio);
            match receiver
                .receiver()
                .capture_video(Duration::from_millis(100))
            {
                Ok(Some(received)) => {
                    assert_eq!(received.width(), 64);
                    if count.load(Ordering::Relaxed) > 0 {
                        captured = true;
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => last_error = Some(error),
            }
        }
        assert!(
            captured,
            "the local sender delivered no frame from generation {generation}: allocator \
             calls={}, connected={}, last error={last_error:?}",
            count.load(Ordering::Relaxed),
            receiver.receiver().is_connected()
        );
        video_allocations += count.load(Ordering::Relaxed);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while audio_count.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        sender.send_audio(&audio);
        let _ = receiver
            .receiver()
            .capture_audio(Duration::from_millis(100));
    }
    assert!(
        audio_count.load(Ordering::Relaxed) > 0,
        "custom audio allocator did not allocate"
    );
    assert!(video_allocations >= 3);

    // Back to the SDK's own memory. Frames of the replaced policies may still
    // be queued; destroying the receiver makes the SDK free every one.
    let receiver = receiver.into_receiver();
    assert_eq!(service.usage().active_generations(), 0);
    drop(receiver);
    drop(sender);
    let usage = service.usage();
    assert_eq!(
        (
            usage.retained_allocations(),
            usage.retained_bytes(),
            usage.generations()
        ),
        (0, 0, 0),
        "an allocation outlived its receiver: {usage:?}"
    );
}

/// The receiver of an allocated packet receiver can be advertised, and the
/// registration keeps the native receiver alive after the allocated receiver
/// is gone. Dropping it returns the receiver to the SDK's own memory first;
/// the allocations the policy made are still freed, through the process-wide
/// registry, when the registration ends and the native receiver with it.
#[test]
fn an_advertised_packet_receiver_outlives_its_allocators() {
    use grafton_ndi::discovery_server::{AdvertiserOptions, RecvAdvertisement, RecvAdvertiser};
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    if !common::require_feature(ndi.require_feature(RuntimeFeature::ReceiverAdvertising)) {
        return;
    }
    let _turn = sockets();
    let sender = Sender::new(&ndi, &options("advertised-allocation")).unwrap();
    let packets = PacketReceiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &sender).unwrap()),
        PassthroughPreset::ProtectedWithAudio,
    )
    .unwrap();
    let service =
        AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
    let mut packets = service.attach_packets(packets).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let allocations = count.clone();
    packets
        .set_video_allocator(move |request: AllocationRequest| {
            allocations.fetch_add(1, Ordering::Relaxed);
            AllocationBuffer::zeroed(request)
        })
        .unwrap();
    let frame = video();
    let deadline = Instant::now() + PATIENCE;
    let mut events = 0;
    while count.load(Ordering::Relaxed) == 0 {
        assert!(
            Instant::now() < deadline,
            "the policy made no allocation, and {events} events were captured; {}; {}",
            link(packets.receiver()),
            served(&sender)
        );
        sender.send_video(&frame).unwrap();
        events += usize::from(packets.capture_any(SLICE).unwrap().is_some());
    }

    let mut advertiser =
        RecvAdvertiser::new(&ndi, &AdvertiserOptions::default().servers(["127.0.0.1"])).unwrap();
    let registration = advertiser
        .register(packets.receiver(), &RecvAdvertisement::new())
        .unwrap();
    // Frames the policy allocated stay queued in the SDK while the allocated
    // receiver goes; the registration keeps the native receiver.
    for _ in 0..3 {
        sender.send_video(&frame).unwrap();
    }
    drop(packets);
    assert_eq!(service.usage().active_generations(), 0);
    // The native receiver is still alive, and connected, in the SDK's memory.
    assert_eq!(sender.connection_count().unwrap(), 1);
    for _ in 0..3 {
        sender.send_video(&frame).unwrap();
    }

    advertiser.unregister(registration).unwrap();
    drop(advertiser);
    drop(sender);
    let usage = service.usage();
    assert_eq!(
        (
            usage.retained_allocations(),
            usage.retained_bytes(),
            usage.generations()
        ),
        (0, 0, 0),
        "an allocation outlived its advertised receiver: {usage:?}"
    );
}

/// A constructor that takes ownership returns what it was given when it
/// fails, still usable.
#[test]
fn failed_constructors_hand_back_what_they_were_given() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let _turn = sockets();

    // No queue accepts a limit of zero frames, on any runtime.
    let sender = Sender::new(&ndi, &options("handback")).unwrap();
    let source = sender.source().unwrap();
    let rejected = QueuedSender::from_sender(sender, &QueueLimits::default().max_frames(0))
        .expect_err("zero frames is not a valid queue limit");
    assert!(matches!(
        rejected.error(),
        Error::InvalidInput(_) | Error::UnsupportedCapability { .. }
    ));
    let sender = rejected.into_inner();
    sender.send_video(&video()).unwrap();
    assert_eq!(sender.source().unwrap(), source);

    // These succeed on an Advanced runtime and are refused elsewhere; either
    // way the receiver comes back.
    let receiver = match AvSync::new(unconnected_receiver(&ndi)) {
        Ok(sync) => sync.into_receiver(),
        Err(rejected) => {
            assert!(matches!(
                rejected.error(),
                Error::UnsupportedCapability { .. }
            ));
            rejected.into_inner()
        }
    };
    assert!(!receiver.is_connected());

    let service =
        AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
    let receiver = match service.attach(receiver) {
        Ok(attached) => attached.into_receiver(),
        Err(rejected) => {
            assert!(matches!(
                rejected.error(),
                Error::UnsupportedCapability { .. }
            ));
            rejected.into_inner()
        }
    };
    assert!(!receiver.is_connected());
}

/// Without an Advanced runtime, or in a build without the Advanced SDK
/// headers, every native operation says so and core operations carry on.
#[test]
fn operations_report_why_the_advanced_sdk_is_unavailable() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let _turn = sockets();
    if ndi
        .require_feature(RuntimeFeature::AdvancedSenderConfiguration)
        .is_ok()
    {
        // An Advanced runtime in a build with the headers: nothing is unavailable.
        return;
    }
    let config = AdvancedConfig::parse("{}").unwrap();
    let edition = ndi.info().edition();
    // Each operation is refused for its own capability, with the reason its
    // status gives: the runtime for its edition, or the build for the
    // declarations it lacks, which it names. Nothing here is a version gate.
    let unsupported = |expected: RuntimeFeature, error: Error| {
        let Error::UnsupportedCapability {
            capability: grafton_ndi::Capability::Runtime(feature),
            refusal,
        } = error
        else {
            panic!("expected an unsupported capability, got {error:?}");
        };
        assert_eq!(feature, expected);
        assert_eq!(
            ndi.capabilities().status(feature),
            CapabilityStatus::Unavailable(refusal),
            "{feature:?}"
        );
        match refusal {
            grafton_ndi::CapabilityRefusal::WrongEdition(_) => {
                assert_eq!(edition, SdkEdition::Standard, "{feature:?}");
            }
            grafton_ndi::CapabilityRefusal::BuildAbiUnavailable(symbol) => {
                assert!(
                    symbol.to_bytes().starts_with(b"NDIlib_"),
                    "{feature:?}: {symbol:?}"
                );
            }
            other => panic!("{feature:?}: unexpected reason {other:?}"),
        }
    };

    unsupported(
        RuntimeFeature::AdvancedSenderConfiguration,
        Sender::new(&ndi, &options("unavailable").config(config.clone()))
            .err()
            .unwrap(),
    );
    unsupported(
        RuntimeFeature::AdvancedReceiverConfiguration,
        Receiver::new(
            &ndi,
            &ReceiverOptions::new(Source::new("").unwrap()).config(config.clone()),
        )
        .err()
        .unwrap(),
    );
    unsupported(
        RuntimeFeature::AdvancedFinderConfiguration,
        Finder::new(&ndi, &FinderOptions::new().config(config))
            .err()
            .unwrap(),
    );
    unsupported(
        RuntimeFeature::Genlock,
        Genlock::new(&ndi, &GenlockOptions::new()).err().unwrap(),
    );
    unsupported(
        RuntimeFeature::Passthrough(PassthroughPreset::Hevc),
        PacketReceiver::new(
            &ndi,
            &ReceiverOptions::new(Source::new("").unwrap()),
            PassthroughPreset::Hevc,
        )
        .err()
        .unwrap(),
    );
    unsupported(
        RuntimeFeature::AdvancedSenderConfiguration,
        CompressedSender::new(&ndi, &options("unavailable-compressed"))
            .err()
            .unwrap(),
    );
    unsupported(
        RuntimeFeature::SendCompletion,
        QueuedSender::new(&ndi, &options("unavailable-queue"), &QueueLimits::default())
            .err()
            .unwrap(),
    );

    let sender = Sender::new(&ndi, &options("unavailable-core")).unwrap();
    unsupported(
        RuntimeFeature::SenderTally,
        sender
            .set_external_tally(Tally::new(true, false))
            .unwrap_err(),
    );
    unsupported(
        RuntimeFeature::KeyframeDemand,
        sender.keyframe_required(None).unwrap_err(),
    );
    let mut receiver = unconnected_receiver(&ndi);
    unsupported(
        RuntimeFeature::KeyframeRequest,
        receiver.request_keyframe().unwrap_err(),
    );
    unsupported(
        RuntimeFeature::ReceiverStreamTier,
        receiver.set_stream_tier(StreamTier::Preview).unwrap_err(),
    );
    unsupported(
        RuntimeFeature::ReceiverStreamSelection,
        receiver.set_streams(MediaTypes::VIDEO).unwrap_err(),
    );
    unsupported(RuntimeFeature::Kvm, receiver.kvm().err().unwrap());
    let finder = Finder::new(&ndi, &FinderOptions::default()).unwrap();
    unsupported(
        RuntimeFeature::SourceMetadata,
        finder.current_sources_with_metadata().unwrap_err(),
    );

    // The core API is unaffected.
    sender.send_video(&video()).unwrap();
    assert!(finder.current_sources().is_ok());
}

/// The loopback endpoint of a source that a sender of this process advertises.
fn loopback(ndi: &Ndi, advertised: &Source) -> Source {
    common::loopback_endpoint(ndi, advertised.name()).unwrap()
}

/// A receiver of everything `advertised` sends, compressed, on the video
/// stream `bandwidth` selects.
fn packet_receiver(ndi: &Ndi, advertised: &Source, bandwidth: ReceiverBandwidth) -> PacketReceiver {
    PacketReceiver::new(
        ndi,
        &ReceiverOptions::new(loopback(ndi, advertised)).bandwidth(bandwidth),
        PassthroughPreset::ProtectedWithAudio,
    )
    .unwrap()
}

/// Capture until `wanted` picks something out of what arrives.
fn await_event<T>(
    receiver: &mut PacketReceiver,
    what: &str,
    mut wanted: impl FnMut(PacketItem) -> Option<T>,
) -> T {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(found) = receiver.capture_any(SLICE).unwrap().and_then(&mut wanted) {
            return found;
        }
        assert!(Instant::now() < deadline, "{what} did not arrive");
    }
}

/// Timecodes below this mark the frames sent only to find out when a
/// receiver has connected; a frame sent before that goes nowhere.
const FIRST_TIMECODE: i64 = 1_000_000;

/// The next compressed video frame, which must be the one sent with
/// `timecode`: nothing else may arrive before it, twice, or out of order.
fn await_video(receiver: &mut PacketReceiver, timecode: i64) -> ReceivedVideoPacket {
    await_event(
        receiver,
        &format!("video frame {timecode}"),
        |event| match event {
            PacketItem::Video(ReceivedVideo::Compressed(frame))
                if frame.timecode().ticks() >= FIRST_TIMECODE =>
            {
                assert_eq!(
                    frame.timecode().ticks(),
                    timecode,
                    "an unexpected video frame"
                );
                Some(frame)
            }
            PacketItem::Video(ReceivedVideo::Decoded(frame)) => {
                panic!("video frame {:?} arrived decoded", frame.timecode())
            }
            _ => None,
        },
    )
}

/// As [`await_video`], for audio.
fn await_audio(receiver: &mut PacketReceiver, timecode: i64) -> ReceivedAudioPacket {
    await_event(
        receiver,
        &format!("audio frame {timecode}"),
        |event| match event {
            PacketItem::Audio(ReceivedAudio::Compressed(frame))
                if frame.timecode().ticks() >= FIRST_TIMECODE =>
            {
                assert_eq!(
                    frame.timecode().ticks(),
                    timecode,
                    "an unexpected audio frame"
                );
                Some(frame)
            }
            PacketItem::Audio(ReceivedAudio::Decoded(frame)) => {
                panic!("audio frame {:?} arrived decoded", frame.timecode())
            }
            _ => None,
        },
    )
}

/// Send frames with `probe` until `receiver` is given one. A failure names
/// the receiver `what`.
fn connect(receiver: &mut PacketReceiver, what: &str, mut probe: impl FnMut(i64)) {
    let deadline = Instant::now() + PATIENCE;
    let mut events = 0;
    for timecode in 0.. {
        probe(timecode);
        match receiver.capture_any(SLICE).unwrap() {
            Some(PacketItem::Video(_) | PacketItem::Audio(_)) => return,
            Some(_) => events += 1,
            None => {}
        }
        assert!(
            Instant::now() < deadline,
            "{what} did not connect: {} probes sent, {events} other events captured; {}",
            timecode + 1,
            link(receiver.receiver())
        );
    }
}

/// Bytes that differ from one frame to the next.
fn payload(seed: i64, len: usize) -> Vec<u8> {
    (0..len as i64)
        .map(|index| (index * 31 + seed * 7 + 1) as u8)
        .collect()
}

/// The bytes of video frame `seed`: an envelope around a payload, with
/// parameter sets on a keyframe, for H.264 and HEVC, and a payload behind a
/// quality byte for SpeedHQ.
fn video_bytes(codec: VideoCodec, seed: i64, keyframe: bool) -> Vec<u8> {
    match codec.envelope_codec() {
        Some(envelope) => PacketEnvelope::encode(
            envelope,
            90_000 + seed,
            89_000 - seed,
            keyframe,
            &payload(seed, 4000),
            &payload(-seed, if keyframe { 24 } else { 0 }),
        )
        .unwrap(),
        None => {
            let mut bytes = payload(seed, 4096);
            bytes[0] %= 100;
            bytes
        }
    }
}

/// A format for `codec` on the stream `tier`: a full-size picture for the
/// program stream and one a preview receiver is given for the preview stream.
fn video_format(codec: VideoCodec, tier: StreamTier) -> VideoPacketFormat {
    let (width, height) = match tier {
        StreamTier::Program => (1280, 720),
        StreamTier::Preview => (640, 360),
    };
    VideoPacketFormat::new(codec, width, height)
        .unwrap()
        .with_frame_rate(FrameRate::new(60_000, 1001).unwrap())
        .with_picture_aspect_ratio(PictureAspectRatio::new(4.0 / 3.0).unwrap())
        .with_tier(tier)
}

/// A keyframe of `codec` on `tier` with timecode `timecode`.
fn keyframe(codec: VideoCodec, tier: StreamTier, timecode: i64) -> CompressedVideoFrame {
    let mut frame = CompressedVideoFrame::new(
        video_format(codec, tier),
        video_bytes(codec, timecode, true),
    )
    .unwrap();
    frame.set_timecode(Timecode::from_ticks(timecode));
    frame
}

/// The sample rates an AAC configuration can name, in the order of its
/// sampling-frequency index.
const AAC_SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// The channel counts an AAC configuration can name, in the order of its
/// channel configuration, which starts at one.
const AAC_CHANNELS: [u32; 7] = [1, 2, 3, 4, 5, 6, 8];

/// Every video codec.
const VIDEO_CODECS: [VideoCodec; 7] = [
    VideoCodec::H264,
    VideoCodec::Hevc,
    VideoCodec::H264Alpha,
    VideoCodec::HevcAlpha,
    VideoCodec::SpeedHq420,
    VideoCodec::SpeedHq422,
    VideoCodec::SpeedHq422Alpha,
];

/// A two-byte AAC-LC AudioSpecificConfig.
fn aac_config(sample_rate: u32, channels: u32) -> [u8; 2] {
    let position = |values: &[u32], value| {
        values
            .iter()
            .position(|candidate| *candidate == value)
            .unwrap() as u16
    };
    let frequency_index = position(&AAC_SAMPLE_RATES, sample_rate);
    let channel_configuration = position(&AAC_CHANNELS, channels) + 1;
    ((2 << 11) | (frequency_index << 7) | (channel_configuration << 3)).to_be_bytes()
}

/// The bytes of audio frame `seed`: an envelope with its configuration for
/// AAC, a bare packet for Opus.
fn audio_bytes(codec: AudioCodec, sample_rate: u32, channels: u32, seed: i64) -> Vec<u8> {
    match codec {
        AudioCodec::Aac => PacketEnvelope::encode(
            EnvelopeCodec::Aac,
            48_000 + seed,
            47_000 - seed,
            true,
            &payload(seed, 300),
            &aac_config(sample_rate, channels),
        )
        .unwrap(),
        _ => payload(seed, 120),
    }
}

/// `bytes` cut into blocks at each of `cuts`.
fn split(bytes: &[u8], cuts: &[usize]) -> Vec<Vec<u8>> {
    let mut blocks = Vec::new();
    let mut start = 0;
    for end in cuts.iter().copied().chain([bytes.len()]) {
        blocks.push(bytes[start..end].to_vec());
        start = end;
    }
    blocks
}

/// Check that `received` is the packet `sent`. The SDK owns the envelope's
/// reserved field, so an envelope is compared by everything else in it.
fn assert_same_packet(
    (bytes, received): (&[u8], Option<PacketEnvelope<'_>>),
    sent: &[u8],
    envelope: Option<EnvelopeCodec>,
) {
    let Some(codec) = envelope else {
        assert_eq!(bytes, sent);
        assert!(received.is_none());
        return;
    };
    let received = received.expect("the validated packet exposes its envelope");
    let sent = PacketEnvelope::parse(sent).unwrap();
    assert_eq!(received.codec(), Some(codec));
    assert_eq!(
        (
            received.presentation_time(),
            received.decode_time(),
            received.flags()
        ),
        (sent.presentation_time(), sent.decode_time(), sent.flags())
    );
    assert_eq!(received.data(), sent.data());
    assert_eq!(received.extra_data(), sent.extra_data());
    assert!(received.is_known_version() && received.trailing_bytes().is_empty());
}

/// How a compressed frame is handed to the sender.
#[derive(Clone, Copy, Debug)]
enum Submission {
    Contiguous,
    ContiguousAsync,
    Scatter,
    ScatterAsync,
}

/// A sender-wide drain releases contiguous and scatter loans on both tiers,
/// and the same source delivers the recovered storage when it is sent again.
#[test]
fn compressed_finish_keeps_the_source_and_returns_frames_that_can_be_sent_again() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut sender = CompressedSender::new(&ndi, &options("compressed-finish")).unwrap();
    let source = sender.sender().source().unwrap();
    let mut program = packet_receiver(&ndi, &source, ReceiverBandwidth::Highest);
    let mut preview = packet_receiver(&ndi, &source, ReceiverBandwidth::Lowest);
    for (receiver, tier) in [
        (&mut program, StreamTier::Program),
        (&mut preview, StreamTier::Preview),
    ] {
        connect(
            receiver,
            &format!("the {tier:?} drain receiver"),
            |timecode| {
                sender.send_video(&mut keyframe(VideoCodec::H264, tier, timecode));
            },
        );
    }
    let mut program_frame = keyframe(VideoCodec::H264, StreamTier::Program, FIRST_TIMECODE + 1);
    let mut preview_frame = ScatterVideoFrame::new(
        video_format(VideoCodec::H264, StreamTier::Preview),
        split(
            &video_bytes(VideoCodec::H264, FIRST_TIMECODE + 2, true),
            &[44],
        ),
    )
    .unwrap();
    preview_frame.set_timecode(Timecode::from_ticks(FIRST_TIMECODE + 2));
    let program_pointer = program_frame.data().as_ptr();
    let preview_pointer = preview_frame.blocks().next().unwrap().as_ptr();
    for pass in 0..2 {
        let program_timecode = FIRST_TIMECODE + 1 + pass * 10;
        let preview_timecode = FIRST_TIMECODE + 2 + pass * 10;
        program_frame.set_timecode(Timecode::from_ticks(program_timecode));
        preview_frame.set_timecode(Timecode::from_ticks(preview_timecode));
        assert!(sender.submit_video(program_frame).is_none());
        assert!(sender
            .submit_scatter_video(preview_frame)
            .unwrap()
            .is_none());
        let reclaimed = sender.reclaim_video();
        let Some(SubmittedVideo::Contiguous(returned_program)) = reclaimed.program else {
            panic!("the contiguous program frame is recovered");
        };
        let Some(SubmittedVideo::Scatter(returned_preview)) = reclaimed.preview else {
            panic!("the scatter preview frame is recovered");
        };
        assert_eq!(returned_program.data().as_ptr(), program_pointer);
        assert_eq!(
            returned_preview.blocks().next().unwrap().as_ptr(),
            preview_pointer
        );
        await_video(&mut program, program_timecode);
        await_video(&mut preview, preview_timecode);
        assert_eq!(sender.sender().source().unwrap().name(), source.name());
        program_frame = returned_program;
        preview_frame = returned_preview;
    }
    assert!(sender.close().program.is_none());
}

/// Every video codec, on both streams, protected and not, arrives exactly as
/// it was sent, however it was submitted.
#[test]
fn compressed_video_arrives_as_sent_by_every_kind_of_send() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut sender = CompressedSender::new(&ndi, &options("video-matrix")).unwrap();
    let source = sender.sender().source().unwrap();
    let mut program = packet_receiver(&ndi, &source, ReceiverBandwidth::Highest);
    let mut preview = packet_receiver(&ndi, &source, ReceiverBandwidth::Lowest);
    for (receiver, tier) in [
        (&mut program, StreamTier::Program),
        (&mut preview, StreamTier::Preview),
    ] {
        connect(receiver, &format!("the {tier:?} receiver"), |timecode| {
            sender.send_video(&mut keyframe(VideoCodec::H264, tier, timecode));
        });
    }

    let scans = [
        ScanType::Progressive,
        ScanType::Interlaced,
        ScanType::Field0,
        ScanType::Field1,
    ];
    let submissions = [
        Submission::Contiguous,
        Submission::ContiguousAsync,
        Submission::Scatter,
        Submission::ScatterAsync,
    ];
    let mut timecode = FIRST_TIMECODE;
    let mut returned = 0;
    for codec in VIDEO_CODECS {
        for (tier, protected) in [
            (StreamTier::Program, false),
            (StreamTier::Program, true),
            (StreamTier::Preview, false),
            (StreamTier::Preview, true),
        ] {
            let receiver = match tier {
                StreamTier::Program => &mut program,
                StreamTier::Preview => &mut preview,
            };
            // A keyframe starts each stream; the frames that depend on it
            // follow, one by each kind of send.
            let frames = submissions
                .into_iter()
                .flat_map(|submission| [(submission, true), (submission, false)])
                .filter(|(_, keyframe)| *keyframe || codec.envelope_codec().is_some());
            for (submission, keyframe) in frames {
                timecode += 1;
                let format = video_format(codec, tier)
                    .with_protected(protected)
                    .with_scan_type(scans[timecode as usize % scans.len()]);
                let bytes = video_bytes(codec, timecode, keyframe);
                let metadata = format!("<frame number=\"{timecode}\"/>");
                let contiguous = || {
                    let mut frame = CompressedVideoFrame::new(format, bytes.clone()).unwrap();
                    frame.set_timecode(Timecode::from_ticks(timecode));
                    frame.set_metadata(metadata.as_str()).unwrap();
                    frame
                };
                let scatter = || {
                    // The header alone, the payload in two parts, and the
                    // parameter sets split from it.
                    let cuts: &[usize] = match codec.envelope_codec() {
                        Some(_) => &[44, 2000, bytes.len() - 5],
                        None => &[4, 2000],
                    };
                    let mut frame = ScatterVideoFrame::new(format, split(&bytes, cuts)).unwrap();
                    frame.set_timecode(Timecode::from_ticks(timecode));
                    frame.set_metadata(metadata.as_str()).unwrap();
                    frame
                };
                match submission {
                    Submission::Contiguous => {
                        let mut frame = contiguous();
                        sender.send_video(&mut frame);
                        assert_same_packet(
                            (frame.data(), frame.envelope()),
                            &bytes,
                            codec.envelope_codec(),
                        );
                    }
                    Submission::ContiguousAsync => {
                        returned += usize::from(sender.submit_video(contiguous()).is_some());
                    }
                    Submission::Scatter => {
                        let mut frame = scatter();
                        sender.send_scatter_video(&mut frame).unwrap();
                        assert_eq!(frame.total_bytes(), bytes.len());
                    }
                    Submission::ScatterAsync => {
                        returned +=
                            usize::from(sender.submit_scatter_video(scatter()).unwrap().is_some());
                    }
                }
                let received = await_video(receiver, timecode);
                assert_eq!(
                    received.format(),
                    format,
                    "{codec:?} {submission:?} keyframe={keyframe}"
                );
                assert_eq!(received.metadata(), Some(metadata.as_str()));
                assert_same_packet(
                    (received.data(), received.envelope()),
                    &bytes,
                    codec.envelope_codec(),
                );
            }
        }
    }
    // Every asynchronous send but the first on each stream returned the frame
    // it replaced, and the last two are still held.
    let asynchronous = VIDEO_CODECS
        .iter()
        .map(|codec| {
            if codec.envelope_codec().is_some() {
                16
            } else {
                8
            }
        })
        .sum::<usize>();
    assert_eq!(returned, asynchronous - 2);
    let reclaimed = sender.close();
    assert!(reclaimed.program.is_some() && reclaimed.preview.is_some());
}

/// One compressed audio frame a test offers to the wrapper.
#[derive(Debug)]
struct AudioCandidate {
    codec: AudioCodec,
    sample_rate: u32,
    channels: u32,
    samples: u32,
    /// For AAC, the sample rate and channel count its configuration names,
    /// or `None` for a configuration too short to name them.
    config: Option<(u32, u32)>,
    /// For AAC, whether the envelope keeps its keyframe flag.
    flagged: bool,
}

impl AudioCandidate {
    fn opus(sample_rate: u32, channels: u32, samples: u32) -> Self {
        Self {
            codec: AudioCodec::Opus,
            sample_rate,
            channels,
            samples,
            config: None,
            flagged: true,
        }
    }

    /// An AAC frame whose configuration names the frame's own parameters
    /// where it can, and 48 kHz stereo where it cannot.
    fn aac(sample_rate: u32, channels: u32, samples: u32) -> Self {
        let nameable = |values: &[u32], value, otherwise| {
            if values.contains(&value) {
                value
            } else {
                otherwise
            }
        };
        Self {
            codec: AudioCodec::Aac,
            sample_rate,
            channels,
            samples,
            config: Some((
                nameable(&AAC_SAMPLE_RATES, sample_rate, 48_000),
                nameable(&AAC_CHANNELS, channels, 2),
            )),
            flagged: true,
        }
    }

    fn bytes(&self, seed: i64) -> Vec<u8> {
        if self.codec != AudioCodec::Aac {
            return payload(seed, 120);
        }
        let config = match self.config {
            Some((sample_rate, channels)) => aac_config(sample_rate, channels).to_vec(),
            None => vec![0x11],
        };
        let mut bytes = PacketEnvelope::encode(
            EnvelopeCodec::Aac,
            48_000 + seed,
            47_000 - seed,
            true,
            &payload(seed, 300),
            &config,
        )
        .unwrap();
        if !self.flagged {
            // The flags follow the two timestamps and the reserved field.
            bytes[32..36].copy_from_slice(&0u32.to_ne_bytes());
        }
        bytes
    }
}

/// Queued H.264 sources retain AAC and Opus as compressed audio through the
/// shared sender, for both contiguous and scatter audio storage.
#[test]
fn queued_compressed_sources_deliver_aac_and_opus() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut queue = QueuedSender::new(
        &ndi,
        &options("queued-compressed-audio"),
        &QueueLimits::default(),
    )
    .unwrap();
    let mut receiver = packet_receiver(
        &ndi,
        &queue.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    connect(
        &mut receiver,
        "queued compressed audio receiver",
        |timecode| {
            queue
                .try_submit(keyframe(VideoCodec::H264, StreamTier::Program, timecode))
                .unwrap();
            queue.next_completed(PATIENCE).unwrap().unwrap();
        },
    );
    let mut timecode = FIRST_TIMECODE;
    for (codec, samples) in [(AudioCodec::Aac, 1024), (AudioCodec::Opus, 960)] {
        for scatter in [false, true] {
            timecode += 1;
            let bytes = audio_bytes(codec, 48_000, 2, timecode);
            queue
                .try_submit(keyframe(VideoCodec::H264, StreamTier::Program, timecode))
                .unwrap();
            let (_video, audio) = queue.split();
            if scatter {
                let cuts = if codec == AudioCodec::Aac {
                    vec![44]
                } else {
                    vec![1]
                };
                let mut frame = ScatterAudioFrame::new(
                    AudioPacketFormat::new(codec, 48_000, 2, samples).unwrap(),
                    split(&bytes, &cuts),
                )
                .unwrap();
                frame.set_timecode(Timecode::from_ticks(timecode));
                audio.send_scatter_audio(&mut frame).unwrap();
            } else {
                let mut frame = CompressedAudioFrame::new(
                    AudioPacketFormat::new(codec, 48_000, 2, samples).unwrap(),
                    bytes.clone(),
                )
                .unwrap();
                frame.set_timecode(Timecode::from_ticks(timecode));
                audio.send_compressed_audio(&mut frame);
            }
            let received = await_audio(&mut receiver, timecode);
            assert_eq!(
                (
                    received.format().codec(),
                    received.format().sample_rate(),
                    received.format().num_channels(),
                    received.format().num_samples()
                ),
                (codec, 48_000, 2, samples)
            );
            assert_same_packet(
                (received.data(), received.envelope()),
                &bytes,
                (codec == AudioCodec::Aac).then_some(EnvelopeCodec::Aac),
            );
            queue.next_completed(PATIENCE).unwrap().unwrap();
        }
    }
}

/// The SDK takes any compressed audio frame and silently discards the ones
/// it does not accept. Of a grid of frames, sensible and not, every one the
/// wrapper admits arrives exactly as it was sent, from one buffer and from
/// several; the rest it refuses to build.
#[test]
fn every_audio_frame_the_wrapper_admits_is_delivered_as_sent() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let sender = CompressedSender::new(&ndi, &options("audio-matrix")).unwrap();
    let mut receiver = packet_receiver(
        &ndi,
        &sender.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    connect(&mut receiver, "the receiver", |timecode| {
        let mut probe = CompressedAudioFrame::new(
            AudioPacketFormat::new(AudioCodec::Opus, 48_000, 2, 960).unwrap(),
            vec![1; 64],
        )
        .unwrap();
        probe.set_timecode(Timecode::from_ticks(timecode));
        sender.sender().send_compressed_audio(&mut probe);
    });

    let mut candidates = Vec::new();
    for sample_rate in [
        8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 96_000,
    ] {
        // From 2.5 ms to 120 ms, in units of 2.5 ms.
        for (index, units) in [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48]
            .into_iter()
            .enumerate()
        {
            let channels = [1, 2, 6][index % 3];
            candidates.push(AudioCandidate::opus(
                sample_rate,
                channels,
                sample_rate * units / 400,
            ));
        }
    }
    for sample_rate in AAC_SAMPLE_RATES.into_iter().chain([22_000, 50_000]) {
        candidates.push(AudioCandidate::aac(sample_rate, 2, 1024));
    }
    for channels in [1, 3, 4, 5, 6, 7, 8, 9] {
        candidates.push(AudioCandidate::aac(48_000, channels, 1024));
    }
    for samples in [512, 960, 2048] {
        candidates.push(AudioCandidate::aac(48_000, 2, samples));
    }
    // A configuration for another sample rate or layout than the frame's, one
    // too short to name either, and a packet without the keyframe flag.
    candidates.extend([
        AudioCandidate {
            config: Some((44_100, 2)),
            ..AudioCandidate::aac(48_000, 2, 1024)
        },
        AudioCandidate {
            config: Some((48_000, 1)),
            ..AudioCandidate::aac(48_000, 2, 1024)
        },
        AudioCandidate {
            config: None,
            ..AudioCandidate::aac(48_000, 2, 1024)
        },
        AudioCandidate {
            flagged: false,
            ..AudioCandidate::aac(48_000, 2, 1024)
        },
    ]);

    let mut timecode = FIRST_TIMECODE;
    let (mut opus, mut aac) = (0, 0);
    for candidate in candidates {
        let AudioCandidate {
            codec,
            sample_rate,
            channels,
            samples,
            ..
        } = candidate;
        // An AAC configuration is split across the last two blocks.
        let cuts = |bytes: &[u8]| match codec {
            AudioCodec::Aac => vec![44, 100, bytes.len() - 1],
            _ => vec![1, 60],
        };
        let probe = candidate.bytes(0);
        let admitted = CompressedAudioFrame::new(
            AudioPacketFormat::new(codec, sample_rate, channels, samples).unwrap(),
            probe.clone(),
        )
        .is_ok();
        let admitted_in_blocks = ScatterAudioFrame::new(
            AudioPacketFormat::new(codec, sample_rate, channels, samples).unwrap(),
            split(&probe, &cuts(&probe)),
        )
        .is_ok();
        assert_eq!(
            admitted, admitted_in_blocks,
            "one buffer and several are admitted alike: {candidate:?}"
        );
        if !admitted {
            continue;
        }
        match codec {
            AudioCodec::Aac => aac += 1,
            _ => opus += 1,
        }
        for (scatter, protected) in [(false, false), (true, false), (false, true), (true, true)] {
            timecode += 1;
            let bytes = candidate.bytes(timecode);
            let metadata = format!("<frame number=\"{timecode}\"/>");
            if scatter {
                let blocks = split(&bytes, &cuts(&bytes));
                let mut frame = ScatterAudioFrame::new(
                    AudioPacketFormat::new(codec, sample_rate, channels, samples)
                        .unwrap()
                        .with_protected(protected),
                    blocks,
                )
                .unwrap();
                frame.set_timecode(Timecode::from_ticks(timecode));
                frame.set_metadata(metadata.as_str()).unwrap();
                sender.sender().send_scatter_audio(&mut frame).unwrap();
            } else {
                let mut frame = CompressedAudioFrame::new(
                    AudioPacketFormat::new(codec, sample_rate, channels, samples)
                        .unwrap()
                        .with_protected(protected),
                    bytes.clone(),
                )
                .unwrap();
                frame.set_timecode(Timecode::from_ticks(timecode));
                frame.set_metadata(metadata.as_str()).unwrap();
                sender.sender().send_compressed_audio(&mut frame);
            }
            let received = await_audio(&mut receiver, timecode);
            assert_eq!(
                (
                    received.format().codec(),
                    received.format().is_protected(),
                    received.format().sample_rate(),
                    received.format().num_channels(),
                    received.format().num_samples()
                ),
                (codec, protected, sample_rate, channels, samples),
                "scatter={scatter}"
            );
            assert_eq!(received.metadata(), Some(metadata.as_str()));
            let envelope = (codec == AudioCodec::Aac).then_some(EnvelopeCodec::Aac);
            assert_same_packet((received.data(), received.envelope()), &bytes, envelope);
        }
    }
    // Opus at five sample rates and six durations; AAC at each sample rate
    // its configuration can name, and in stereo at 48 kHz with each of the
    // six other layouts.
    assert_eq!((opus, aac), (5 * 6, 13 + 6));
}

/// As for audio: of a grid of video frames on both streams, every one the
/// wrapper admits arrives, from one buffer and from several, and the rest
/// (previews of H.264 and HEVC that are too large, SpeedHQ frames that do not
/// begin with a quality) it refuses to build.
#[test]
fn every_video_frame_the_wrapper_admits_is_delivered() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut sender = CompressedSender::new(&ndi, &options("video-grid")).unwrap();
    let source = sender.sender().source().unwrap();
    let mut program = packet_receiver(&ndi, &source, ReceiverBandwidth::Highest);
    let mut preview = packet_receiver(&ndi, &source, ReceiverBandwidth::Lowest);
    for (receiver, tier) in [
        (&mut program, StreamTier::Program),
        (&mut preview, StreamTier::Preview),
    ] {
        connect(receiver, &format!("the {tier:?} receiver"), |timecode| {
            sender.send_video(&mut keyframe(VideoCodec::H264, tier, timecode));
        });
    }

    let sizes = [
        (640, 360),
        (360, 640),
        (640, 640),
        (641, 360),
        (640, 641),
        (1280, 720),
        (1920, 1080),
    ];
    let mut timecode = FIRST_TIMECODE;
    let (mut envelopes, mut speedhq) = (0, 0);
    for codec in VIDEO_CODECS {
        // The quality a SpeedHQ frame begins with. An envelope has none.
        let qualities: &[Option<u8>] = match codec.envelope_codec() {
            Some(_) => &[None],
            None => &[Some(0), Some(99), Some(100), Some(255)],
        };
        for tier in [StreamTier::Program, StreamTier::Preview] {
            let receiver = match tier {
                StreamTier::Program => &mut program,
                StreamTier::Preview => &mut preview,
            };
            for ((width, height), quality) in sizes
                .into_iter()
                .flat_map(|size| qualities.iter().map(move |quality| (size, *quality)))
            {
                let format = VideoPacketFormat::new(codec, width, height)
                    .unwrap()
                    .with_tier(tier);
                // A format that leaves the aspect ratio at zero has square
                // pixels, and arrives with the ratio that stands for.
                let delivered = format.with_picture_aspect_ratio(
                    PictureAspectRatio::new(width as f32 / height as f32).unwrap(),
                );
                let bytes = |timecode| {
                    let mut bytes = video_bytes(codec, timecode, true);
                    if let Some(quality) = quality {
                        bytes[0] = quality;
                    }
                    bytes
                };
                let cuts: &[usize] = match codec.envelope_codec() {
                    Some(_) => &[44, 2000],
                    None => &[4, 2000],
                };
                let probe = bytes(0);
                let admitted = CompressedVideoFrame::new(format, probe.clone()).is_ok();
                let admitted_in_blocks =
                    ScatterVideoFrame::new(format, split(&probe, cuts)).is_ok();
                assert_eq!(
                    admitted, admitted_in_blocks,
                    "one buffer and several are admitted alike: {format:?} quality {quality:?}"
                );
                if !admitted {
                    continue;
                }
                match codec.envelope_codec() {
                    Some(_) => envelopes += 1,
                    None => speedhq += 1,
                }

                timecode += 1;
                let sent = bytes(timecode);
                let mut frame = CompressedVideoFrame::new(format, sent.clone()).unwrap();
                frame.set_timecode(Timecode::from_ticks(timecode));
                sender.send_video(&mut frame);
                let received = await_video(receiver, timecode);
                assert_eq!(received.format(), delivered);
                assert_same_packet(
                    (received.data(), received.envelope()),
                    &sent,
                    codec.envelope_codec(),
                );

                timecode += 1;
                let sent = bytes(timecode);
                let mut frame = ScatterVideoFrame::new(format, split(&sent, cuts)).unwrap();
                frame.set_timecode(Timecode::from_ticks(timecode));
                sender.send_scatter_video(&mut frame).unwrap();
                let received = await_video(receiver, timecode);
                assert_eq!(received.format(), delivered);
                assert_same_packet(
                    (received.data(), received.envelope()),
                    &sent,
                    codec.envelope_codec(),
                );
            }
        }
    }
    // Four envelope codecs: every size on the program stream, the three that
    // fit in 640 by 640 on the preview stream. Three SpeedHQ codecs: every
    // size on both streams, with either quality.
    assert_eq!((envelopes, speedhq), (4 * (7 + 3), 3 * 2 * 7 * 2));
}

/// A relay sends on what it receives. The SDK delivers every envelope with
/// its reserved field set, so a relay works only if such packets can be sent.
#[test]
fn a_relay_delivers_every_packet_it_receives() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut origin = CompressedSender::new(&ndi, &options("relay-origin")).unwrap();
    let mut upstream = packet_receiver(
        &ndi,
        &origin.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    let mut relay = CompressedSender::new(&ndi, &options("relay")).unwrap();
    let mut downstream = packet_receiver(
        &ndi,
        &relay.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    for (what, sender, receiver) in [
        ("the upstream receiver", &mut origin, &mut upstream),
        ("the downstream receiver", &mut relay, &mut downstream),
    ] {
        connect(receiver, what, |timecode| {
            sender.send_video(&mut keyframe(
                VideoCodec::H264,
                StreamTier::Program,
                timecode,
            ));
        });
    }

    let mut timecode = FIRST_TIMECODE;
    let video = [
        (VideoCodec::H264, true),
        (VideoCodec::H264, false),
        (VideoCodec::H264, false),
        (VideoCodec::Hevc, true),
        (VideoCodec::Hevc, false),
        (VideoCodec::H264Alpha, true),
        (VideoCodec::HevcAlpha, true),
        (VideoCodec::SpeedHq422, true),
    ];
    for (index, (codec, is_keyframe)) in video.into_iter().enumerate() {
        timecode += 1;
        let format = video_format(codec, StreamTier::Program);
        let bytes = video_bytes(codec, timecode, is_keyframe);
        let mut frame = CompressedVideoFrame::new(format, bytes.clone()).unwrap();
        frame.set_timecode(Timecode::from_ticks(timecode));
        frame.set_metadata("<relayed/>").unwrap();
        origin.send_video(&mut frame);

        let mut received =
            CompressedVideoFrame::try_from(await_video(&mut upstream, timecode)).unwrap();
        if index % 2 == 0 {
            relay.send_video(&mut received);
        } else {
            relay.submit_video(received);
        }
        let relayed = await_video(&mut downstream, timecode);
        assert_eq!(relayed.format(), format);
        assert_eq!(relayed.metadata(), Some("<relayed/>"));
        assert_same_packet(
            (relayed.data(), relayed.envelope()),
            &bytes,
            codec.envelope_codec(),
        );
    }

    let audio = [
        (AudioCodec::Aac, 48_000, 2, 1024),
        (AudioCodec::Aac, 44_100, 6, 1024),
        (AudioCodec::Opus, 48_000, 2, 960),
        (AudioCodec::Opus, 16_000, 1, 320),
    ];
    for (codec, sample_rate, channels, samples) in audio {
        for _ in 0..2 {
            timecode += 1;
            let bytes = audio_bytes(codec, sample_rate, channels, timecode);
            let mut frame = CompressedAudioFrame::new(
                AudioPacketFormat::new(codec, sample_rate, channels, samples).unwrap(),
                bytes.clone(),
            )
            .unwrap();
            frame.set_timecode(Timecode::from_ticks(timecode));
            origin.sender().send_compressed_audio(&mut frame);

            let mut received =
                CompressedAudioFrame::try_from(await_audio(&mut upstream, timecode)).unwrap();
            relay.sender().send_compressed_audio(&mut received);
            let relayed = await_audio(&mut downstream, timecode);
            assert_eq!(
                (
                    relayed.format().codec(),
                    relayed.format().sample_rate(),
                    relayed.format().num_channels(),
                    relayed.format().num_samples()
                ),
                (codec, sample_rate, channels, samples)
            );
            let envelope = (codec == AudioCodec::Aac).then_some(EnvelopeCodec::Aac);
            assert_same_packet((relayed.data(), relayed.envelope()), &bytes, envelope);
        }
    }
}

/// The SDK writes to a frame while it sends it. The frame is still the frame
/// that was built, and can be sent again by every kind of send.
#[test]
fn a_frame_can_be_sent_again_after_the_sdk_has_written_to_it() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut sender = CompressedSender::new(&ndi, &options("resend")).unwrap();
    let mut receiver = packet_receiver(
        &ndi,
        &sender.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    connect(&mut receiver, "the receiver", |timecode| {
        sender.send_video(&mut keyframe(
            VideoCodec::H264,
            StreamTier::Program,
            timecode,
        ));
    });
    let format = video_format(VideoCodec::H264, StreamTier::Program);
    let h264 = Some(EnvelopeCodec::H264);
    let mut timecode = FIRST_TIMECODE;

    // One contiguous frame, synchronously.
    timecode += 1;
    let bytes = video_bytes(VideoCodec::H264, timecode, true);
    let mut frame = CompressedVideoFrame::new(format, bytes.clone()).unwrap();
    frame.set_timecode(Timecode::from_ticks(timecode));
    frame.set_metadata("<again/>").unwrap();
    for _ in 0..3 {
        sender.send_video(&mut frame);
        let received = await_video(&mut receiver, timecode);
        assert_same_packet((received.data(), received.envelope()), &bytes, h264);
        assert_eq!(received.metadata(), Some("<again/>"));
        assert_same_packet((frame.data(), frame.envelope()), &bytes, h264);
        assert_eq!(frame.metadata(), Some("<again/>"));
        ReceivedVideoPacket::from(frame.clone())
            .validate_for_send()
            .unwrap();
    }

    // One scatter frame, synchronously.
    timecode += 1;
    let bytes = video_bytes(VideoCodec::H264, timecode, true);
    let mut scatter = ScatterVideoFrame::new(format, split(&bytes, &[44, 2000])).unwrap();
    scatter.set_timecode(Timecode::from_ticks(timecode));
    for _ in 0..3 {
        sender.send_scatter_video(&mut scatter).unwrap();
        let received = await_video(&mut receiver, timecode);
        assert_same_packet((received.data(), received.envelope()), &bytes, h264);
    }
    let joined = scatter.into_contiguous_copy();
    assert_same_packet((joined.data(), joined.envelope()), &bytes, h264);

    // Two frames that take turns being held by an asynchronous send.
    let mut frames = Vec::new();
    for _ in 0..2 {
        timecode += 1;
        let bytes = video_bytes(VideoCodec::H264, timecode, true);
        let mut frame = CompressedVideoFrame::new(format, bytes.clone()).unwrap();
        frame.set_timecode(Timecode::from_ticks(timecode));
        frames.push((timecode, bytes, Some(frame)));
    }
    for round in 0..6 {
        let (timecode, bytes, frame) = &mut frames[round % 2];
        let frame = frame.take().expect("the frame came back from the sender");
        let replaced = sender.submit_video(frame);
        let received = await_video(&mut receiver, *timecode);
        assert_same_packet((received.data(), received.envelope()), bytes, h264);
        match replaced {
            Some(SubmittedVideo::Contiguous(earlier)) => {
                let (_, bytes, slot) = &mut frames[(round + 1) % 2];
                assert_same_packet((earlier.data(), earlier.envelope()), bytes, h264);
                *slot = Some(earlier);
            }
            None => assert_eq!(round, 0),
            Some(other) => panic!("a contiguous frame was submitted, not {other:?}"),
        }
    }

    // Audio in an envelope, and without one.
    for (codec, samples) in [(AudioCodec::Aac, 1024), (AudioCodec::Opus, 960)] {
        timecode += 1;
        let bytes = audio_bytes(codec, 48_000, 2, timecode);
        let envelope = (codec == AudioCodec::Aac).then_some(EnvelopeCodec::Aac);
        let mut frame = CompressedAudioFrame::new(
            AudioPacketFormat::new(codec, 48_000, 2, samples).unwrap(),
            bytes.clone(),
        )
        .unwrap();
        frame.set_timecode(Timecode::from_ticks(timecode));
        for _ in 0..3 {
            sender.sender().send_compressed_audio(&mut frame);
            let received = await_audio(&mut receiver, timecode);
            assert_same_packet((received.data(), received.envelope()), &bytes, envelope);
            assert_same_packet((frame.data(), frame.envelope()), &bytes, envelope);
        }
    }
}

/// Waiting cannot admit a frame that exceeds the entire queue byte budget.
#[test]
fn a_queued_frame_larger_than_the_byte_limit_is_not_retryable() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut queue = QueuedSender::new(
        &ndi,
        &options("queue-byte-limit"),
        &QueueLimits::default().max_bytes(1 << 20),
    )
    .unwrap();
    let frame = VideoFrame::builder()
        .resolution(1920, 1080)
        .build()
        .unwrap();
    let original = frame.data().as_ptr();
    let refused = queue.try_submit(frame).unwrap_err();
    assert!(matches!(refused.error(), Error::InvalidInput(_)));
    let QueuedVideoFrame::Pixels(frame) = refused.into_inner() else {
        panic!("the rejected frame changed type");
    };
    assert_eq!(frame.data().as_ptr(), original);
    assert_eq!(queue.retained_frames(), 0);
    assert!(queue.next_completed(Duration::ZERO).unwrap().is_none());
}

/// Scatter completions return each original block allocation for reuse.
#[test]
fn queued_scatter_frames_return_their_original_blocks() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut queue =
        QueuedSender::new(&ndi, &options("queued-scatter"), &QueueLimits::default()).unwrap();
    let mut receiver = packet_receiver(
        &ndi,
        &queue.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    connect(&mut receiver, "queued scatter receiver", |timecode| {
        queue
            .try_submit(keyframe(VideoCodec::H264, StreamTier::Program, timecode))
            .unwrap();
        queue.next_completed(PATIENCE).unwrap().unwrap();
    });
    for number in 1..=6 {
        let timecode = FIRST_TIMECODE + number;
        let bytes = video_bytes(VideoCodec::H264, timecode, true);
        let blocks = vec![
            bytes[..44].to_vec(),
            bytes[44..2000].to_vec(),
            bytes[2000..].to_vec(),
        ];
        let pointers = blocks
            .iter()
            .map(|block| (block.as_ptr() as usize, block.capacity()))
            .collect::<Vec<_>>();
        let mut frame =
            ScatterVideoFrame::new(video_format(VideoCodec::H264, StreamTier::Program), blocks)
                .unwrap();
        frame.set_timecode(Timecode::from_ticks(timecode));
        let id = queue.try_submit(frame).unwrap();
        let received = await_video(&mut receiver, timecode);
        assert_same_packet(
            (received.data(), received.envelope()),
            &bytes,
            Some(EnvelopeCodec::H264),
        );
        let done = queue.next_completed(PATIENCE).unwrap().unwrap();
        assert_eq!(done.id(), id);
        let QueuedVideoFrame::Scatter(frame) = done.into_frame() else {
            panic!("scatter frame changed type")
        };
        let blocks = frame.into_blocks();
        assert_eq!(
            blocks
                .iter()
                .map(|block| (block.as_ptr() as usize, block.capacity()))
                .collect::<Vec<_>>(),
            pointers
        );
    }
    assert_eq!(queue.retained_frames(), 0);
    assert!(!queue.completion_protocol_failed());
}

/// A frame a queue has sent comes back complete and can be queued again.
#[test]
fn a_frame_returned_by_a_queued_sender_can_be_resubmitted() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut queue = QueuedSender::new(&ndi, &options("requeue"), &QueueLimits::default()).unwrap();
    let mut receiver = packet_receiver(
        &ndi,
        &queue.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    connect(&mut receiver, "the receiver", |timecode| {
        queue
            .try_submit(keyframe(VideoCodec::H264, StreamTier::Program, timecode))
            .unwrap();
        queue
            .next_completed(PATIENCE)
            .unwrap()
            .expect("the SDK completes a frame it was given");
    });

    let timecode = FIRST_TIMECODE + 1;
    let bytes = video_bytes(VideoCodec::H264, timecode, true);
    let format = video_format(VideoCodec::H264, StreamTier::Program);
    let mut packet = CompressedVideoFrame::new(format, bytes.clone()).unwrap();
    packet.set_timecode(Timecode::from_ticks(timecode));
    let mut frame = QueuedVideoFrame::from(packet);
    for _ in 0..4 {
        let id = queue.try_submit(frame).unwrap();
        let received = await_video(&mut receiver, timecode);
        assert_same_packet(
            (received.data(), received.envelope()),
            &bytes,
            Some(EnvelopeCodec::H264),
        );
        let completed = queue
            .next_completed(PATIENCE)
            .unwrap()
            .expect("the SDK completes a frame it has sent");
        assert_eq!(completed.id(), id);
        frame = completed.into_frame();
        let QueuedVideoFrame::Compressed(sent) = &frame else {
            panic!("a compressed frame was queued");
        };
        assert_same_packet(
            (sent.data(), sent.envelope()),
            &bytes,
            Some(EnvelopeCodec::H264),
        );
    }
    assert_eq!(queue.retained_frames(), 0);
    assert!(!queue.completion_protocol_failed());
}

/// Split Advanced senders carry audio and video on independent native threads.
#[test]
fn advanced_senders_lend_audio_and_video_to_separate_threads() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    fn await_both(receiver: &mut PacketReceiver) {
        let deadline = Instant::now() + PATIENCE;
        let (mut video, mut audio) = (false, false);
        while !(video && audio) {
            match receiver.capture_any(SLICE).unwrap() {
                Some(PacketItem::Video(_)) => video = true,
                Some(PacketItem::Audio(_)) => audio = true,
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "split media missing: video={video}, audio={audio}"
            );
        }
    }
    let _turn = sockets();
    let mut queue = QueuedSender::new(
        &ndi,
        &options("split-queue").clock_video(true).clock_audio(true),
        &QueueLimits::default(),
    )
    .unwrap();
    let mut receiver = packet_receiver(
        &ndi,
        &queue.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    connect(&mut receiver, "split queue", |timecode| {
        queue
            .try_submit(keyframe(VideoCodec::H264, StreamTier::Program, timecode))
            .unwrap();
        queue.next_completed(PATIENCE).unwrap().unwrap();
    });
    let audio = AudioFrame::builder()
        .num_channels(2)
        .num_samples(480)
        .sample_rate(48_000)
        .build()
        .unwrap();
    {
        let (mut video, sender) = queue.split();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                for number in 0..6 {
                    video
                        .try_submit(keyframe(
                            VideoCodec::H264,
                            StreamTier::Program,
                            FIRST_TIMECODE + number,
                        ))
                        .unwrap();
                    video.next_completed(PATIENCE).unwrap().unwrap();
                }
            });
            scope.spawn(|| {
                for _ in 0..6 {
                    sender.send_audio(&audio);
                }
            });
        });
    }
    await_both(&mut receiver);
    assert_eq!(queue.retained_frames(), 0);

    let mut sender = CompressedSender::new(
        &ndi,
        &options("split-compressed")
            .clock_video(true)
            .clock_audio(true),
    )
    .unwrap();
    let mut receiver = packet_receiver(
        &ndi,
        &sender.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    connect(&mut receiver, "split compressed", |timecode| {
        let mut frame = keyframe(VideoCodec::H264, StreamTier::Program, timecode);
        sender.send_video(&mut frame);
    });
    {
        let (mut video, audio) = sender.split();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                for number in 0..6 {
                    let mut frame = keyframe(
                        VideoCodec::H264,
                        StreamTier::Program,
                        FIRST_TIMECODE + number,
                    );
                    video.send_video(&mut frame);
                }
            });
            scope.spawn(move || {
                for _ in 0..6 {
                    let mut frame = CompressedAudioFrame::new(
                        AudioPacketFormat::new(AudioCodec::Opus, 48_000, 2, 960).unwrap(),
                        vec![1; 64],
                    )
                    .unwrap();
                    audio.send_compressed_audio(&mut frame);
                }
            });
        });
    }
    await_both(&mut receiver);
}

/// A queue works on threads other than the one that created it: it submits
/// and collects completions after a move, is read through a shared reference
/// from a third thread, and is destroyed on a fourth, where destruction waits
/// for the SDK's last completions.
#[test]
fn a_queued_sender_moves_between_threads() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut queue =
        QueuedSender::new(&ndi, &options("moved-queue"), &QueueLimits::default()).unwrap();
    let mut receiver = packet_receiver(
        &ndi,
        &queue.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    connect(&mut receiver, "the receiver", |timecode| {
        queue
            .try_submit(keyframe(VideoCodec::H264, StreamTier::Program, timecode))
            .unwrap();
        queue
            .next_completed(PATIENCE)
            .unwrap()
            .expect("the SDK completes a frame it was given");
    });

    let (submitted, sent) = std::sync::mpsc::channel();
    let (received, delivered) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        for timecode in FIRST_TIMECODE + 1..FIRST_TIMECODE + 5 {
            let id = queue
                .try_submit(keyframe(VideoCodec::H264, StreamTier::Program, timecode))
                .unwrap();
            submitted.send(timecode).unwrap();
            delivered.recv().unwrap();
            let completed = queue
                .next_completed(PATIENCE)
                .unwrap()
                .expect("the SDK completes a frame it has sent");
            assert_eq!(completed.id(), id);
        }
        queue
    });
    for timecode in sent {
        let frame = await_video(&mut receiver, timecode);
        assert_same_packet(
            (frame.data(), frame.envelope()),
            &video_bytes(VideoCodec::H264, timecode, true),
            Some(EnvelopeCodec::H264),
        );
        received.send(()).unwrap();
    }
    let queue = worker.join().unwrap();

    std::thread::scope(|scope| {
        scope.spawn(|| {
            assert_eq!(queue.retained_frames(), 0);
            assert!(!queue.completion_protocol_failed());
            assert!(queue.sender().source().is_ok());
        });
    });
    std::thread::spawn(move || drop(queue)).join().unwrap();
}

/// Closing immediately after submission returns each retained id and its
/// original allocation once, whether completion came before or during close.
#[test]
fn closing_a_queued_sender_returns_every_submission_once() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut queue = QueuedSender::new(
        &ndi,
        &options("close-queue"),
        &QueueLimits::default().max_frames(8),
    )
    .unwrap();
    let original = std::array::from_fn::<_, 8, _>(|_| {
        let frame = video();
        let pointer = frame.data().as_ptr();
        (queue.try_submit(frame).unwrap(), pointer)
    });
    assert_eq!(queue.retained_frames(), original.len());
    let mut reclaimed = queue.close();
    assert_eq!(reclaimed.len(), original.len());
    reclaimed.sort_unstable_by_key(|done| done.id().get());
    for (completed, (id, pointer)) in reclaimed.into_iter().zip(original) {
        assert_eq!(completed.id(), id);
        let QueuedVideoFrame::Pixels(frame) = completed.into_frame() else {
            panic!("the frame changed type");
        };
        assert_eq!(frame.data().as_ptr(), pointer);
    }
}

/// Use `receiver` through shared references from four threads at once, each
/// making every call the receiver's controls offer that changes nothing at
/// the source; it is connected, so each sees the connection.
fn share_controls<M: grafton_ndi::ReceiverMode>(receiver: &Receiver<M>) {
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..50 {
                    assert!(receiver.is_connected());
                    assert!(receiver.connection_stats().unwrap().connections() >= 1);
                    receiver.set_tally(Tally::new(true, false));
                    let _ = receiver
                        .wait_for_source_name_change(Duration::ZERO)
                        .unwrap();
                }
            });
        }
    });
}

/// A packet receiver works on threads other than the one that created it: it
/// captures packets after a move, its receiver's controls are used through
/// shared references from several threads at once, and it is destroyed on a
/// third thread.
#[test]
fn a_packet_receiver_moves_between_threads() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut sender = CompressedSender::new(&ndi, &options("moved-packets")).unwrap();
    // Created on this thread, which never uses it.
    let receiver = packet_receiver(
        &ndi,
        &sender.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    let stop = AtomicBool::new(false);
    let receiver = std::thread::scope(|scope| {
        let capture = scope.spawn(|| {
            let _stop = StopOnDrop(&stop);
            let receiver = receiver;
            let deadline = Instant::now() + PATIENCE;
            let mut verified = 0;
            while verified < 4 {
                assert!(
                    Instant::now() < deadline,
                    "{verified} packets arrived; {}",
                    link(receiver.receiver())
                );
                if let Some(PacketItem::Video(ReceivedVideo::Compressed(frame))) =
                    receiver.capture_any(SLICE).unwrap()
                {
                    let timecode = frame.timecode().ticks();
                    assert!(timecode >= FIRST_TIMECODE);
                    assert_same_packet(
                        (frame.data(), frame.envelope()),
                        &video_bytes(VideoCodec::H264, timecode, true),
                        Some(EnvelopeCodec::H264),
                    );
                    verified += 1;
                }
            }
            receiver
        });
        let mut timecode = FIRST_TIMECODE;
        while !stop.load(Ordering::Acquire) {
            sender.send_video(&mut keyframe(
                VideoCodec::H264,
                StreamTier::Program,
                timecode,
            ));
            timecode += 1;
            std::thread::sleep(Duration::from_millis(5));
        }
        capture.join().unwrap()
    });
    share_controls(receiver.receiver());
    std::thread::spawn(move || drop(receiver)).join().unwrap();
    assert!(sender.close().program.is_none());
}

/// A packet receiver with allocators works on threads other than the one that
/// attached them: it captures into the policy's memory after a move, replaces
/// the policy there, shares its receiver's controls across threads, and is
/// destroyed on another thread, after which the SDK has freed every
/// allocation either policy made.
#[test]
fn an_allocated_packet_receiver_moves_between_threads() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let sender = Sender::new(&ndi, &options("moved-allocation")).unwrap();
    let packets = PacketReceiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &sender).unwrap()),
        PassthroughPreset::ProtectedWithAudio,
    )
    .unwrap();
    let service =
        AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
    let policy = |count: &Arc<AtomicUsize>| {
        let count = count.clone();
        move |request: AllocationRequest| {
            count.fetch_add(1, Ordering::Relaxed);
            AllocationBuffer::zeroed(request)
        }
    };
    // Attached and given its first policy on this thread, which never uses
    // it again.
    let (first, second) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut packets = service.attach_packets(packets).unwrap();
    packets.set_video_allocator(policy(&first)).unwrap();

    let stop = AtomicBool::new(false);
    let packets = std::thread::scope(|scope| {
        let capture = scope.spawn(|| {
            let _stop = StopOnDrop(&stop);
            let mut packets = packets;
            // Capture until video arrives after `count`'s policy has run.
            let used = |packets: &mut AllocatedReceiver<PacketAttached>,
                        count: &AtomicUsize,
                        which: &str| {
                let deadline = Instant::now() + PATIENCE;
                let mut videos = 0;
                loop {
                    assert!(
                        Instant::now() < deadline,
                        "the {which} policy was not used: {videos} video frames arrived and it \
                         made {} allocations; {}",
                        count.load(Ordering::Relaxed),
                        link(packets.receiver())
                    );
                    let video = matches!(
                        packets.capture_any(SLICE).unwrap(),
                        Some(PacketItem::Video(_))
                    );
                    videos += usize::from(video);
                    if video && count.load(Ordering::Relaxed) > 0 {
                        return;
                    }
                }
            };
            used(&mut packets, &first, "first");
            packets.set_video_allocator(policy(&second)).unwrap();
            used(&mut packets, &second, "second");
            packets
        });
        let frame = video();
        while !stop.load(Ordering::Acquire) {
            sender.send_video(&frame).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        capture.join().unwrap()
    });
    share_controls(packets.receiver());
    std::thread::spawn(move || drop(packets)).join().unwrap();
    assert_eq!(service.usage().active_generations(), 0);

    drop(sender);
    let usage = service.usage();
    assert_eq!(
        (
            usage.retained_allocations(),
            usage.retained_bytes(),
            usage.generations()
        ),
        (0, 0, 0),
        "an allocation outlived its receiver: {usage:?}"
    );
}

/// Connect a receiver to the source `sender` advertises and exchange
/// metadata with it in both directions, using only the operations every
/// sender mode shares: connection metadata reaches the receiver when it
/// connects, sent metadata reaches it afterwards, and metadata the receiver
/// sends upstream is captured by the sender.
fn exchange_metadata<M: SenderMode>(ndi: &Ndi, sender: &Sender<M>, kind: &str) -> Receiver {
    let connection = MetadataFrame::new(format!("<connection kind=\"{kind}\"/>")).unwrap();
    sender.add_connection_metadata(&connection);
    let receiver = Receiver::new(
        ndi,
        &ReceiverOptions::new(loopback(ndi, &sender.source().unwrap())),
    )
    .unwrap();
    let downstream = MetadataFrame::new(format!("<to-receiver kind=\"{kind}\"/>")).unwrap();
    let upstream = MetadataFrame::new(format!("<to-sender kind=\"{kind}\"/>")).unwrap();
    let (mut connected, mut delivered, mut returned) = (false, false, false);
    let deadline = Instant::now() + PATIENCE;
    while !(connected && delivered && returned) {
        assert!(
            Instant::now() < deadline,
            "{kind}: connection metadata {connected}, sent {delivered}, returned {returned}; \
             {}; {}",
            link(&receiver),
            served(sender)
        );
        sender.send_metadata(&downstream);
        if let Some(frame) = receiver.capture_metadata(SLICE).unwrap() {
            connected |= frame.data() == connection.data();
            delivered |= frame.data() == downstream.data();
        }
        match receiver.send_metadata(&upstream) {
            Ok(()) | Err(Error::NotConnected) => {}
            Err(error) => panic!("{kind}: {error}"),
        }
        if let Some(frame) = sender.capture_metadata(SLICE).unwrap() {
            returned |= frame.data() == upstream.data();
        }
    }
    assert!(sender.connection_count().unwrap() >= 1);
    sender.clear_connection_metadata();
    receiver
}

#[test]
fn kvm_empty_touch_and_cleanup_send_the_explicit_finish_message() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let sender = Sender::new(&ndi, &options("kvm-touch-finish")).unwrap();
    let mut receiver = exchange_metadata(&ndi, &sender, "kvm-touch-finish");
    // Drain incidental connection messages before comparing messages from
    // the same connection. The native KVM encoding is deliberately opaque.
    while sender.capture_metadata(Duration::ZERO).unwrap().is_some() {}
    let next_kvm = || {
        let deadline = Instant::now() + PATIENCE;
        loop {
            assert!(Instant::now() < deadline, "KVM message was not delivered");
            if let Some(frame) = sender.capture_metadata(SLICE).unwrap() {
                if frame.data().starts_with("<ndi_kvm ") {
                    return frame.data().to_owned();
                }
            }
        }
    };
    let mut session = receiver.kvm().unwrap();
    session.finish_touch().unwrap();
    let finish = next_kvm();
    // The SDK owns the opaque wire encoding. Some released Linux libraries
    // leave bytes in the touch header uninitialized, even for consecutive
    // explicit finish calls. Check delivery and payload length here;
    // deterministic boundary tests assert the exact function called. Byte
    // equality cannot establish equivalence on those native libraries.
    session.finish_touch().unwrap();
    let other_finish = next_kvm();
    let assert_finish = |message: String| {
        assert_eq!(message.len(), finish.len());
        assert_eq!(other_finish.len(), finish.len());
    };
    session.touch(&[]).unwrap();
    assert_finish(next_kvm());
    session.touch(&[[0.25, 0.5]]).unwrap();
    assert_ne!(next_kvm(), finish);
    session.release_all().unwrap();
    assert_finish(next_kvm());
    session.touch(&[[0.25, 0.5]]).unwrap();
    assert_ne!(next_kvm(), finish);
    drop(session);
    assert_finish(next_kvm());
}

/// The sender a `CompressedSender` or a `QueuedSender` lends exchanges
/// metadata with its receivers as an ordinary one does, and a queued source
/// carries uncompressed audio beside the video it queues.
#[test]
fn wrapped_senders_exchange_metadata_and_audio_through_the_sender_they_lend() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let compressed = CompressedSender::new(&ndi, &options("lent-compressed")).unwrap();
    drop(exchange_metadata(&ndi, compressed.sender(), "compressed"));
    assert!(compressed.close().program.is_none());

    let queue = QueuedSender::new(&ndi, &options("lent-queue"), &QueueLimits::default()).unwrap();
    let receiver = exchange_metadata(&ndi, queue.sender(), "queued");
    let samples = 1_600;
    let sound = AudioFrame::builder()
        .sample_rate(48_000)
        .num_channels(2)
        .num_samples(samples)
        .data(vec![0.25; 2 * samples as usize])
        .build()
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    let received = loop {
        queue.sender().send_audio(&sound);
        if let Some(frame) = receiver.capture_audio(SLICE).unwrap() {
            break frame;
        }
        assert!(Instant::now() < deadline, "queued audio did not arrive");
    };
    assert_eq!(
        (
            received.sample_rate(),
            received.num_channels(),
            received.num_samples()
        ),
        (48_000, 2, samples)
    );
    assert!(received.data().iter().all(|&sample| sample == 0.25));
    assert_eq!(queue.retained_frames(), 0);
}

/// The common tally state belongs to every sender mode, including the
/// restricted sender views lent by both Advanced sending owners.
#[test]
fn wrapped_sender_tally_polls_preserve_waiters_changes() {
    fn check<M: SenderMode>(sender: &Sender<M>, receiver: &Receiver) {
        for next in [Tally::new(true, false), Tally::new(false, true)] {
            receiver.set_tally(next);
            let end = Instant::now() + PATIENCE;
            while sender.tally() != next {
                assert!(
                    Instant::now() < end,
                    "wrapped sender did not observe {next:?}"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(sender.wait_for_tally_change(SLICE).unwrap(), Some(next));
            assert_eq!(sender.wait_for_tally_change(Duration::ZERO).unwrap(), None);
        }
    }
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let compressed = CompressedSender::new(&ndi, &options("tally-compressed")).unwrap();
    let receiver = exchange_metadata(&ndi, compressed.sender(), "tally-compressed");
    check(compressed.sender(), &receiver);
    drop((receiver, compressed));
    let queue = QueuedSender::new(&ndi, &options("tally-queue"), &QueueLimits::default()).unwrap();
    let receiver = exchange_metadata(&ndi, queue.sender(), "tally-queue");
    check(queue.sender(), &receiver);
}

/// Sets a flag when it goes out of scope, so that a thread that runs until
/// the flag is set also stops when the test fails.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// What each [`synchronized_audio`] request passes along with the video frame
/// that paces it.
#[derive(Clone, Copy)]
enum Pass {
    /// The frame as captured, borrowed from the SDK's buffer.
    Captured,
    /// An owned copy of the frame.
    Owned,
    /// No frame: it only paces the requests.
    Nothing,
}

/// A frame the runtime delivered, which carries its sender's timestamp.
fn stamped<F: AvSyncFrame>(frame: &F) -> AvSyncVideo<'_> {
    AvSyncVideo::new(frame).expect("a received frame carries its sender's timestamp")
}

/// Ask `sync` for audio, once for each video frame that arrives, until it has
/// returned audio `count` times, passing along what `pass` says.
fn synchronized_audio(
    sync: &AvSync,
    request: AvSyncRequest,
    pass: Pass,
    count: usize,
) -> Vec<(AudioFrame, bool)> {
    let deadline = Instant::now() + PATIENCE;
    let mut audio = Vec::new();
    // Video frames captured, and the requests answered with no audio stream,
    // with no matching samples and with no samples.
    let (mut videos, mut no_stream, mut no_match, mut empty) = (0, 0, 0, 0);
    while audio.len() < count {
        assert!(
            Instant::now() < deadline,
            "AVSync returned audio {} times, not {count}, for {videos} video frames; it found \
             no audio stream {no_stream} times, no matching samples {no_match} times and no \
             samples {empty} times; {}",
            audio.len(),
            link(sync.receiver())
        );
        let Some(video) = sync.receiver().capture_video_ref(SLICE).unwrap() else {
            continue;
        };
        videos += 1;
        let outcome = match pass {
            Pass::Captured => sync.synchronize(stamped(&video), request),
            Pass::Owned => sync.synchronize(stamped(&video.to_owned()), request),
            Pass::Nothing => {
                if request.num_samples().is_some() {
                    // A captured video can arrive before its audio, and the
                    // preceding synchronization can leave only a short tail.
                    // Establish enough queued input before testing an exact
                    // queued read, rather than depending on packet arrival.
                    drop(sync.queued_audio(AvSyncRequest::automatic()).unwrap());
                    let received = sync
                        .receiver()
                        .connection_stats()
                        .unwrap()
                        .audio_frames_received();
                    while sync
                        .receiver()
                        .connection_stats()
                        .unwrap()
                        .audio_frames_received()
                        .saturating_sub(received)
                        < 2
                    {
                        assert!(Instant::now() < deadline, "audio did not reach the queue");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                sync.queued_audio(request)
            }
        };
        match outcome.unwrap() {
            AvSyncOutcome::Audio {
                frame,
                samples_adjusted,
            } => audio.push((frame, samples_adjusted)),
            // The streams are starting, or this frame's audio is still on
            // its way.
            AvSyncOutcome::NoAudioStream => no_stream += 1,
            AvSyncOutcome::NoMatchingSamples => no_match += 1,
            AvSyncOutcome::Empty { .. } => empty += 1,
            other => panic!("unexpected AVSync outcome {other:?}"),
        }
    }
    audio
}

/// AVSync returns the audio of a source as audio: the amount that belongs to
/// a video frame when asked automatically, and the amount asked for when a
/// request names one.
#[test]
fn avsync_returns_the_audio_that_belongs_to_a_video_frame() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    // Video paced at 30 frames per second, each frame followed by the 1600
    // samples of 48 kHz audio that last as long.
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(format!("grafton-local-{}-avsync", std::process::id()))
            .clock_video(true)
            .clock_audio(false),
    )
    .unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &sender).unwrap()),
    )
    .unwrap();
    let sync = AvSync::new(receiver).unwrap();
    let video = VideoFrame::builder()
        .resolution(64, 32)
        .frame_rate(FrameRate::new(30, 1).unwrap())
        .metadata("<frame/>")
        .build()
        .unwrap();
    let audio = AudioFrame::builder()
        .sample_rate(48_000)
        .num_channels(2)
        .num_samples(1600)
        .build()
        .unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stop = StopOnDrop(&stop);
        scope.spawn(|| {
            while !stop.load(Ordering::Acquire) {
                sender.send_video(&video).unwrap();
                sender.send_audio(&audio);
                sender.send_metadata(&MetadataFrame::new("<avsync/>").unwrap());
            }
        });
        // The lent receiver captures metadata on another thread while this
        // one synchronizes.
        let (receiver, stopped) = (sync.receiver(), &stop);
        let metadata = scope.spawn(move || {
            let mut captured = 0;
            while !stopped.load(Ordering::Acquire) {
                if receiver.capture_metadata_ref(SLICE).unwrap().is_some() {
                    captured += 1;
                }
            }
            captured
        });
        let is_source_format = |frame: &AudioFrame| {
            assert_eq!((frame.sample_rate(), frame.num_channels()), (48_000, 2));
        };

        // Automatic: the audio that spans each video frame. The span is
        // measured between the times the frames were sent, so single results
        // vary by a few samples around the 1600 a frame lasts.
        let automatic = synchronized_audio(&sync, AvSyncRequest::automatic(), Pass::Captured, 9);
        let mut counts = Vec::new();
        for (frame, samples_adjusted) in &automatic {
            is_source_format(frame);
            assert!(!samples_adjusted);
            counts.push(frame.num_samples());
        }
        counts.sort_unstable();
        assert!(
            (1520..=1680).contains(&counts[counts.len() / 2]),
            "automatic requests returned {counts:?} samples"
        );

        // A count close to what belongs to a video frame is returned exactly.
        let request = AvSyncRequest::new(Some(48_000), Some(1600)).unwrap();
        let exact = synchronized_audio(&sync, request, Pass::Owned, 5);
        for (frame, samples_adjusted) in &exact {
            is_source_format(frame);
            assert!(
                *samples_adjusted || frame.num_samples() == 1600,
                "an unadjusted result has {} samples",
                frame.num_samples()
            );
        }
        assert!(exact.iter().any(|(_, samples_adjusted)| !samples_adjusted));

        // A count far from it is not: the frame's own audio comes back, and
        // the result says that the count was not met.
        let request = AvSyncRequest::new(Some(48_000), Some(800)).unwrap();
        for (frame, samples_adjusted) in synchronized_audio(&sync, request, Pass::Captured, 3) {
            is_source_format(&frame);
            assert!(samples_adjusted && frame.num_samples() != 800);
        }

        // An exact count without a video frame is taken from the queued audio.
        let request = AvSyncRequest::new(Some(48_000), Some(480)).unwrap();
        for (frame, samples_adjusted) in synchronized_audio(&sync, request, Pass::Nothing, 3) {
            is_source_format(&frame);
            assert_eq!((frame.num_samples(), samples_adjusted), (480, false));
        }

        // Everything that is queued.
        let all = synchronized_audio(&sync, AvSyncRequest::automatic(), Pass::Nothing, 1);
        is_source_format(&all[0].0);
        assert!(all[0].0.num_samples() > 0);

        // A request computed for another sample rate than the source's is
        // told the source's format instead of being given audio, for an owned
        // frame as for a captured one.
        let request = AvSyncRequest::new(Some(44_100), Some(1470)).unwrap();
        let frame = sync
            .receiver()
            .capture_video(PATIENCE)
            .unwrap()
            .expect("a frame within the wait");
        assert!(matches!(
            sync.synchronize(stamped(&frame), request).unwrap(),
            AvSyncOutcome::FormatChanged {
                sample_rate: 48_000,
                num_channels: 2
            }
        ));
        stop.store(true, Ordering::Release);
        assert!(metadata.join().unwrap() > 0, "no metadata was captured");
    });
    drop(sync.into_receiver());
}

/// A stated rate must work for non-stereo sources as well as stereo. The SDK
/// headers mention channels in the format comparison, while the manual names
/// only the rate; pin both synchronization entry paths against real peers.
#[test]
fn avsync_states_rates_for_non_stereo_sources() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    for (rate, channels, samples) in [(44_100, 1, 1470), (48_000, 4, 1600)] {
        let sender = Sender::new(
            &ndi,
            &SenderOptions::new(common::unique_name("avsync-non-stereo"))
                .clock_video(true)
                .clock_audio(false),
        )
        .unwrap();
        let receiver = Receiver::new(
            &ndi,
            &ReceiverOptions::new(common::loopback_source(&ndi, &sender).unwrap()),
        )
        .unwrap();
        let sync = AvSync::new(receiver).unwrap();
        let video = VideoFrame::builder()
            .resolution(64, 32)
            .frame_rate(FrameRate::new(30, 1).unwrap())
            .build()
            .unwrap();
        let values: Vec<f32> = (0..channels)
            .flat_map(|channel| std::iter::repeat_n((channel + 1) as f32 / 8.0, samples as usize))
            .collect();
        let audio = AudioFrame::builder()
            .sample_rate(rate)
            .num_channels(channels)
            .num_samples(samples)
            .data(values)
            .build()
            .unwrap();
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let _stop = StopOnDrop(&stop);
            scope.spawn(|| {
                while !stop.load(Ordering::Acquire) {
                    sender.send_video(&video).unwrap();
                    sender.send_audio(&audio);
                }
            });
            let requests = [
                ("automatic", AvSyncRequest::automatic(), Pass::Captured),
                (
                    "stated rate and count",
                    AvSyncRequest::new(Some(rate), Some(samples)).unwrap(),
                    Pass::Captured,
                ),
                (
                    "count only",
                    AvSyncRequest::new(None, Some(samples)).unwrap(),
                    Pass::Captured,
                ),
                (
                    "queued stated rate",
                    AvSyncRequest::new(Some(rate), Some(480)).unwrap(),
                    Pass::Nothing,
                ),
            ];
            for (label, request, pass) in requests {
                let deadline = Instant::now() + PATIENCE;
                loop {
                    let returned = synchronized_audio(&sync, request, pass, 1);
                    let (frame, adjusted) = &returned[0];
                    assert_eq!(
                        (frame.sample_rate(), frame.num_channels()),
                        (rate, channels),
                        "{label}"
                    );
                    if matches!(pass, Pass::Nothing) {
                        assert_eq!((frame.num_samples(), *adjusted), (480, false));
                    }
                    // Initial audio may contain alignment silence. Wait for a
                    // full source frame, then require every channel's value.
                    if frame.channels().enumerate().all(|(channel, samples)| {
                        !samples.is_empty()
                            && samples
                                .iter()
                                .all(|sample| (*sample - (channel + 1) as f32 / 8.0).abs() < 0.001)
                    }) {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "{label} did not preserve {rate} Hz, {channels}-channel samples"
                    );
                }
            }
            let different_rate = if rate == 48_000 { 44_100 } else { 48_000 };
            let video = sync
                .receiver()
                .capture_video(PATIENCE)
                .unwrap()
                .expect("a frame within the wait");
            assert!(matches!(
                sync.synchronize(
                    stamped(&video),
                    AvSyncRequest::new(Some(different_rate), Some(samples)).unwrap()
                )
                .unwrap(),
                AvSyncOutcome::FormatChanged { sample_rate, num_channels }
                    if sample_rate == rate && num_channels == channels
            ));
        });
    }
}

/// A synchronizer works on threads other than the one that created it: it
/// returns a source's audio, sample for sample, on a thread it was moved to,
/// and is destroyed on a third while the source is still sending.
#[test]
fn an_avsync_moves_between_threads() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("moved-avsync"))
            .clock_video(true)
            .clock_audio(false),
    )
    .unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &sender).unwrap()),
    )
    .unwrap();
    // Created on this thread, which never uses it.
    let sync = AvSync::new(receiver).unwrap();
    let video = VideoFrame::builder()
        .resolution(64, 32)
        .frame_rate(FrameRate::new(30, 1).unwrap())
        .build()
        .unwrap();
    let audio = AudioFrame::builder()
        .sample_rate(48_000)
        .num_channels(2)
        .num_samples(1600)
        .data(vec![0.25; 3200])
        .build()
        .unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stop = StopOnDrop(&stop);
        scope.spawn(|| {
            while !stop.load(Ordering::Acquire) {
                sender.send_video(&video).unwrap();
                sender.send_audio(&audio);
            }
        });
        let sync = scope
            .spawn(move || {
                // The first audio after the streams start may hold alignment
                // silence; the source's own samples follow.
                let deadline = Instant::now() + PATIENCE;
                loop {
                    let returned =
                        synchronized_audio(&sync, AvSyncRequest::automatic(), Pass::Captured, 1);
                    let frame = &returned[0].0;
                    assert_eq!((frame.sample_rate(), frame.num_channels()), (48_000, 2));
                    if frame
                        .data()
                        .iter()
                        .all(|sample| (sample - 0.25).abs() < 0.001)
                    {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "AVSync returned none of the source's samples"
                    );
                }
                sync
            })
            .join()
            .unwrap();
        scope.spawn(move || drop(sync)).join().unwrap();
    });
}

/// The policy calls of a test, which wait for each other inside the policy.
#[derive(Default)]
struct PolicyCalls {
    /// Calls inside the policy now, the most there have been at once, the
    /// most the service has reported in progress, and whether to stop waiting.
    state: Mutex<(usize, usize, usize, bool)>,
    changed: Condvar,
}

impl PolicyCalls {
    fn lock(&self) -> MutexGuard<'_, (usize, usize, usize, bool)> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The default limits promise a number of concurrent policy calls. That many
/// receivers, each with a frame arriving, are all inside the policy at once.
#[test]
fn allocator_calls_run_concurrently_up_to_the_documented_limit() {
    /// What the documentation of `AllocatorLimits` gives as its default.
    const CONCURRENT_CALLS: usize = 16;
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let sender = Sender::new(&ndi, &options("allocator-calls")).unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let service =
        AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
    let calls = Arc::new(PolicyCalls::default());
    let receivers: Vec<_> = (0..CONCURRENT_CALLS)
        .map(|_| {
            let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source.clone())).unwrap();
            let mut receiver = service.attach(receiver).unwrap();
            let calls = calls.clone();
            receiver
                .set_video_allocator(move |request: AllocationRequest| {
                    let mut state = calls.lock();
                    state.0 += 1;
                    state.1 = state.1.max(state.0);
                    state.2 = state.2.max(service.usage().in_progress_allocations());
                    calls.changed.notify_all();
                    // Stay inside until every receiver's call is inside too.
                    let (mut state, _) = calls
                        .changed
                        .wait_timeout_while(state, PATIENCE, |state| {
                            state.1 < CONCURRENT_CALLS && !state.3
                        })
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.0 -= 1;
                    drop(state);
                    AllocationBuffer::zeroed(request)
                })
                .unwrap();
            receiver
        })
        .collect();

    let frame = video();
    let deadline = Instant::now() + PATIENCE;
    let (peak, in_progress) = loop {
        sender.send_video(&frame).unwrap();
        let (mut state, _) = calls
            .changed
            .wait_timeout_while(calls.lock(), SLICE, |state| state.1 < CONCURRENT_CALLS)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.1 >= CONCURRENT_CALLS || Instant::now() >= deadline {
            state.3 = true;
            calls.changed.notify_all();
            break (state.1, state.2);
        }
    };
    assert_eq!(
        (peak, in_progress),
        (CONCURRENT_CALLS, CONCURRENT_CALLS),
        "policy calls that ran at once, and calls the service counted in progress"
    );

    drop(receivers);
    drop(sender);
    let usage = service.usage();
    assert_eq!(
        (
            usage.retained_allocations(),
            usage.retained_bytes(),
            usage.generations(),
            usage.in_progress_allocations()
        ),
        (0, 0, 0, 0),
        "an allocation outlived its receiver: {usage:?}"
    );
}

/// A sender names its metadata in its configuration, and a finder that uses
/// the same Discovery Server is told it; a sender that names none has none.
#[cfg(all(target_os = "linux", not(ndi_link_static)))]
#[test]
fn a_finder_is_told_the_metadata_a_sender_advertises() {
    use common::discovery_server::{server_binary, Server};

    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let Some(binary) = server_binary(&ndi) else {
        common::skip(
            common::Requirement::DiscoveryServer,
            "the SDK's ndi-discovery-server is not beside the selected runtime",
        );
        return;
    };
    let server = Server::start(&binary);
    let config = |properties: &str| {
        AdvancedConfig::parse(format!(
            r#"{{"ndi":{{"networks":{{"discovery":"{}"}}{properties}}}}}"#,
            server.endpoint()
        ))
        .unwrap()
    };
    let described = Sender::new(
        &ndi,
        &options("described").config(config(
            r#","send":{"metadata":"<grafton-test kind=\"described\"/>"}"#,
        )),
    )
    .unwrap();
    let undescribed = Sender::new(&ndi, &options("undescribed").config(config(""))).unwrap();
    let finder = Finder::new(&ndi, &FinderOptions::new().config(config(""))).unwrap();

    let sources = [described.source().unwrap(), undescribed.source().unwrap()];
    let deadline = Instant::now() + PATIENCE;
    let mut watch = finder.watch();
    let metadata = loop {
        let found_sources = finder.current_sources_with_metadata().unwrap();
        let found: Vec<_> = sources
            .iter()
            .filter_map(|wanted| {
                found_sources
                    .iter()
                    .find(|source| source.source().name() == wanted.name())
            })
            .map(|source| source.metadata().map(str::to_owned))
            .collect();
        if found.len() == sources.len() {
            break found;
        }
        assert!(
            Instant::now() < deadline,
            "the finder was told of {} of the two senders",
            found.len()
        );
        watch.wait_for_change(SLICE).unwrap();
    };
    assert_eq!(
        metadata,
        [Some("<grafton-test kind=\"described\"/>".to_owned()), None]
    );
}

/// Per-sender machine names are SDK label text, including spaces and underscores,
/// and remain selectable through a cache by another ASCII letter case.
#[test]
fn configured_machine_names_are_discovered_and_cached_without_dns_restrictions() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let machine = format!("Grafton Studio_{}", std::process::id());
    let group = common::unique_name("machine-query");
    let config =
        AdvancedConfig::parse(format!(r#"{{"ndi":{{"machinename":"{machine}"}}}}"#)).unwrap();
    let sender = Sender::new(
        &ndi,
        &options("machine-query").groups([&group]).config(config),
    )
    .unwrap();
    let advertised = sender.source().unwrap();
    assert!(
        advertised
            .name()
            .split_once(" (")
            .is_some_and(|(label, _)| label.eq_ignore_ascii_case(&machine)),
        "the configured machine name was not advertised: {:?}",
        advertised.name()
    );
    let finder = Finder::new(
        &ndi,
        &FinderOptions::new()
            .groups([&group])
            .extra_ips(["127.0.0.1"]),
    )
    .unwrap();
    let discovered = finder
        .find_source_by_name(advertised.name(), PATIENCE)
        .unwrap()
        .expect("the finder did not discover its configured-machine-name sender");
    assert!(discovered.matches_host(&machine.to_ascii_uppercase()));
    let cache = SourceCache::new(finder);
    let matches = cache
        .find_sources_by_host(&machine.to_ascii_lowercase(), SLICE)
        .unwrap();
    assert!(
        matches
            .iter()
            .any(|source| source.name() == advertised.name()),
        "the cache could not select its sender by machine name: {matches:?}"
    );
    assert_eq!(cache.len(), 1);
    cache.invalidate(&format!("{}.", machine.to_ascii_uppercase()));
    assert!(cache.is_empty());
}

/// Send a DCI 8K frame in the widest uncompressed format over a native link.
#[test]
#[cfg_attr(miri, ignore = "requires the Advanced runtime and a full DCI 8K frame")]
fn dci_8k_nb4g_sends_and_decodes_at_full_resolution() {
    use grafton_ndi::{frames::MAX_VIDEO_BYTES, BorrowedVideoFrame, PixelFormat};

    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    if !ndi.capabilities().supports(RuntimeFeature::Planar444Video)
        || !ndi.capabilities().supports(RuntimeFeature::ReceiveBest444)
    {
        common::unsupported("DCI 8K NB4G exchange needs Planar444Video and ReceiveBest444");
        return;
    }

    const WIDTH: u32 = 8192;
    const HEIGHT: u32 = 4320;
    let _turn = sockets();
    let sender = Sender::new(&ndi, &options("dci-8k-nb4g")).unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(loopback(&ndi, &sender.source().unwrap()))
            .color_format(ReceiverColorFormat::Best444),
    )
    .unwrap();

    let mut frame = VideoFrame::builder()
        .resolution(WIDTH, HEIGHT)
        .pixel_format(PixelFormat::Nb4g)
        .build()
        .unwrap();
    assert_eq!(frame.data().len(), MAX_VIDEO_BYTES);
    let plane_bytes = WIDTH as usize * HEIGHT as usize * size_of::<u16>();
    for (index, plane) in frame.data_mut().chunks_exact_mut(plane_bytes).enumerate() {
        for row in plane.chunks_exact_mut(WIDTH as usize * size_of::<u16>()) {
            for (x, sample) in row.chunks_exact_mut(size_of::<u16>()).enumerate() {
                let value = match index {
                    0 => 16_384 + x as u16 * 4,
                    1 | 2 => 32_768,
                    _ => u16::MAX,
                };
                sample.copy_from_slice(&value.to_le_bytes());
            }
        }
    }
    let borrowed = BorrowedVideoFrame::builder(frame.data(), WIDTH, HEIGHT, PixelFormat::Nb4g)
        .build()
        .unwrap();
    assert_eq!(borrowed.data().unwrap().len(), MAX_VIDEO_BYTES);

    let deadline = Instant::now() + PATIENCE;
    loop {
        sender.send_video(&frame).unwrap();
        if let Some(received) = receiver.capture_video_ref(SLICE).unwrap() {
            assert_eq!((received.width(), received.height()), (WIDTH, HEIGHT));
            return;
        }
        assert!(
            Instant::now() < deadline,
            "DCI 8K frame did not arrive: {}",
            link(&receiver)
        );
    }
}

/// A real encoded picture exercises the decoder and planar pixel layout. The
/// adjacent-pixel chroma pattern detects accidental 4:2:2/4:2:0 conversion.
#[test]
fn hevc_444_decodes_to_the_original_full_resolution_chroma_planes() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    if ndi.require_feature(RuntimeFeature::ReceiveBest444).is_err() {
        common::skip(
            Hevc444Decoder,
            "HEVC 4:4:4 decoding needs the NDI 6.4 Best444 receiver",
        );
        return;
    }
    // A decoder known to be missing skips here, and one that is missing
    // although nothing showed it beforehand is told by its placeholders below.
    if !common::hx_decoder(&ndi, Hevc444Decoder) {
        return;
    }
    let _turn = sockets();
    let mut sender = CompressedSender::new(&ndi, &options("hevc-444")).unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(loopback(&ndi, &sender.sender().source().unwrap()))
            .color_format(grafton_ndi::ReceiverColorFormat::Best444),
    )
    .unwrap();
    let bitstream = hx_fixture::hevc();
    // This encoder emits VPS, SPS and PPS before the IDR. A NAL header's type
    // is its first byte's bits 1 to 6, and types below 32 are slices.
    let parameter_sets = parameter_sets(bitstream, |header| ((header >> 1) & 0x3f) < 32);
    let format = VideoPacketFormat::new(VideoCodec::Hevc, 64, 32)
        .unwrap()
        .with_frame_rate(FrameRate::new(30, 1).unwrap());
    let mut pts = 0;
    let Some(frame) = decoded_hx_frame(&receiver, &format, Hevc444Decoder, "HEVC 4:4:4", || {
        let bytes = PacketEnvelope::encode(
            EnvelopeCodec::Hevc,
            pts,
            pts,
            true,
            bitstream,
            parameter_sets,
        )
        .unwrap();
        let mut packet = CompressedVideoFrame::new(format, bytes).unwrap();
        sender.send_video(&mut packet);
        pts += 3000;
    }) else {
        return;
    };
    assert_eq!(frame.pixel_format(), grafton_ndi::PixelFormat::Na48);
    assert_eq!(frame.planes().len(), 3);
    for (plane_index, plane) in frame.planes().enumerate() {
        assert_eq!(plane.row_count(), 32);
        for (y, row) in plane.rows().enumerate() {
            assert_eq!(row.len(), 64);
            for (x, value) in row.iter().copied().enumerate() {
                let expected = match plane_index {
                    0 => 64 + (x + y) % 64,
                    1 => 64 + 64 * (x % 2),
                    2 => 80 + 64 * (y % 2),
                    _ => unreachable!(),
                } as u8;
                assert_eq!(value, expected, "plane {plane_index}, ({x}, {y})");
            }
        }
    }
}

/// The H.264 test picture's quadrants as (luma, Cb, Cr): top left, top right,
/// bottom left, bottom right. `tests/support/generate_hx.py` states the same.
const H264_QUADRANTS: [[u8; 3]; 4] = [[48, 96, 176], [96, 176, 96], [160, 64, 128], [208, 160, 64]];

/// H.264 is the NDI|HX codec the Advanced SDK manual documents as decoded
/// without plugins on Windows and macOS, and it needs no 6.4 receiver mode.
/// Flat quadrants, each with its own luma and chroma, come through 4:2:0 and
/// QP 1 coding unchanged; none of them is the placeholder's black, and a
/// swapped, shifted or desaturated picture fails.
#[test]
fn h264_420_decodes_to_the_original_quadrants() {
    /// How far a decoded sample may stray from the coded picture. Through the
    /// runtime and FFmpeg 7 on Linux every sample is exact; the margin is for
    /// decoders and conversions that round differently.
    const TOLERANCE: u8 = 4;

    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    if !common::hx_decoder(&ndi, H264Decoder) {
        return;
    }
    let _turn = sockets();
    let mut sender = CompressedSender::new(&ndi, &options("h264-420")).unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(loopback(&ndi, &sender.sender().source().unwrap()))
            .color_format(ReceiverColorFormat::UyvyBgra),
    )
    .unwrap();
    let bitstream = hx_fixture::h264();
    // x264 emits SPS and PPS before the IDR. A NAL header's type is its first
    // byte's low five bits, and types 1 to 5 are slices.
    let parameter_sets = parameter_sets(bitstream, |header| matches!(header & 0x1f, 1..=5));
    let format = VideoPacketFormat::new(VideoCodec::H264, 64, 64)
        .unwrap()
        .with_frame_rate(FrameRate::new(30, 1).unwrap());
    let mut pts = 0;
    let Some(frame) = decoded_hx_frame(&receiver, &format, H264Decoder, "H.264 4:2:0", || {
        let bytes = PacketEnvelope::encode(
            EnvelopeCodec::H264,
            pts,
            pts,
            true,
            bitstream,
            parameter_sets,
        )
        .unwrap();
        let mut packet = CompressedVideoFrame::new(format, bytes).unwrap();
        sender.send_video(&mut packet);
        pts += 3000;
    }) else {
        return;
    };
    assert_eq!(frame.pixel_format(), grafton_ndi::PixelFormat::Uyvy);
    assert_eq!(frame.planes().len(), 1);
    let plane = frame.plane(0).unwrap();
    assert_eq!(plane.row_count(), 64);
    for (y, row) in plane.rows().enumerate() {
        // Each pair of pixels is Cb, Y, Cr, Y.
        let (pairs, rest) = row.as_chunks::<4>();
        assert_eq!((pairs.len(), rest.len()), (32, 0));
        for (x, &[cb_value, left, cr_value, right]) in pairs.iter().enumerate() {
            let [luma, cb, cr] = H264_QUADRANTS[2 * usize::from(y >= 32) + usize::from(x >= 16)];
            for (sample, pixel, value, expected) in [
                ("Cb", 2 * x, cb_value, cb),
                ("Y", 2 * x, left, luma),
                ("Cr", 2 * x, cr_value, cr),
                ("Y", 2 * x + 1, right, luma),
            ] {
                assert!(
                    value.abs_diff(expected) <= TOLERANCE,
                    "{sample} at ({pixel}, {y}) is {value}, coded as {expected}"
                );
            }
        }
    }
}

/// One request must reach every observer, including observers using a null
/// stream selector beside observers naming the H.264 stream explicitly.
#[test]
fn concurrent_keyframe_waiters_observe_the_same_request() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut sender = CompressedSender::new(&ndi, &options("broadcast-keyframe")).unwrap();
    let mut receiver = packet_receiver(
        &ndi,
        &sender.sender().source().unwrap(),
        ReceiverBandwidth::Highest,
    );
    let format = VideoPacketFormat::new(VideoCodec::H264, 64, 64)
        .unwrap()
        .with_frame_rate(FrameRate::new(30, 1).unwrap());
    let bitstream = hx_fixture::h264();
    let parameter_sets = parameter_sets(bitstream, |header| matches!(header & 0x1f, 1..=5));
    let bytes =
        PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, true, bitstream, parameter_sets).unwrap();
    let mut frame = CompressedVideoFrame::new(format, bytes).unwrap();
    connect(&mut receiver, "keyframe observer", |_| {
        sender.send_video(&mut frame);
    });

    for selectors in [
        [None, None],
        [Some(format), Some(format)],
        [None, Some(format)],
    ] {
        for _ in 0..3 {
            // The SDK can clear or reassert demand asynchronously while the
            // receiver processes earlier frames. Establish actual readiness
            // before starting the waiters instead of assuming send cleared it.
            let ready_by = Instant::now() + PATIENCE;
            loop {
                sender.send_video(&mut frame);
                let received = matches!(
                    receiver.capture_any(SLICE).unwrap(),
                    Some(PacketItem::Video(_))
                );
                let required = sender.sender().keyframe_required(None).unwrap();
                if received && !required {
                    break;
                }
                assert!(
                    Instant::now() < ready_by,
                    "keyframe preparation did not deliver video and clear demand for \
                     {selectors:?} (demand={required}); {}; {}",
                    link(receiver.receiver()),
                    served(sender.sender()),
                );
            }
            // Consume earlier update reports so only the explicit request
            // should wake the concurrent waiters.
            for selector in [None, Some(format)] {
                while sender
                    .sender()
                    .wait_for_keyframe_change(selector, Duration::ZERO)
                    .unwrap()
                {}
            }
            let ready = std::sync::Barrier::new(3);
            let (mut video, audio) = sender.split();
            std::thread::scope(|scope| {
                let sender = audio;
                let idle_stream = scope.spawn(|| {
                    sender
                        .wait_for_keyframe_change(
                            Some(format.with_tier(StreamTier::Preview)),
                            Duration::from_millis(400),
                        )
                        .unwrap()
                });
                let ready = &ready;
                let frame = &mut frame;
                let consumer = scope.spawn(move || {
                    ready.wait();
                    let changed = sender
                        .wait_for_keyframe_change(selectors[0], Duration::from_secs(2))
                        .unwrap();
                    if changed {
                        // Sending the requested keyframe may clear native demand
                        // before another observer resumes. Its report stays latched.
                        video.send_video(frame);
                    }
                    changed
                });
                let follower = scope.spawn(move || {
                    ready.wait();
                    sender
                        .wait_for_keyframe_change(selectors[1], Duration::from_secs(2))
                        .unwrap()
                });
                ready.wait();
                std::thread::sleep(Duration::from_millis(100));
                let requested = Instant::now();
                receiver.receiver().request_keyframe().unwrap();
                for wait in [consumer, follower] {
                    assert!(
                        wait.join().unwrap(),
                        "a keyframe waiter missed {selectors:?}"
                    );
                }
                assert!(
                    !idle_stream.join().unwrap(),
                    "an idle preview stream observed program demand"
                );
                assert!(
                    requested.elapsed() < Duration::from_secs(1),
                    "a keyframe waiter returned late for {selectors:?}"
                );
            });
        }
    }
}

/// The Annex B units before the first slice of `bitstream`, start codes
/// included: its parameter sets, which the SDK asks for both inline and in
/// the envelope's extra data. `is_slice` tells a slice by its NAL header's
/// first byte.
fn parameter_sets(bitstream: &[u8], is_slice: impl Fn(u8) -> bool) -> &[u8] {
    let end = bitstream
        .windows(4)
        .enumerate()
        .find_map(|(i, bytes)| {
            // A start code is three or four bytes; the NAL header follows it.
            let header = if bytes == [0, 0, 0, 1] {
                *bitstream.get(i + 4)?
            } else if bytes[..3] == [0, 0, 1] {
                bytes[3]
            } else {
                return None;
            };
            is_slice(header).then_some(i)
        })
        .expect("the fixture has a slice after its parameter sets");
    &bitstream[..end]
}

/// The first decoded frame `receiver` delivers while `send` sends NDI|HX
/// video of `format`, once per capture slice.
///
/// The runtime reports no error for video it cannot decode: it delivers its
/// black 1920x1080 placeholder instead (see [`common::is_hx_placeholder`]).
/// Any other frame is the decoded picture, is asserted to have the size that
/// was sent, and is returned. Returns `None`, with the calling test reported
/// as skipped for want of `decoder`, when every frame delivered within
/// [`PATIENCE`] was the placeholder. Fails when no frame arrived at all.
fn decoded_hx_frame<'rx>(
    receiver: &'rx Receiver,
    format: &VideoPacketFormat,
    decoder: common::Requirement,
    video: &str,
    mut send: impl FnMut(),
) -> Option<VideoFrameRef<'rx>> {
    let sent = (format.width(), format.height());
    let deadline = Instant::now() + PATIENCE;
    let mut placeholders = 0;
    loop {
        send();
        match receiver.capture_video_ref(SLICE).unwrap() {
            Some(frame) if common::is_hx_placeholder(sent, (frame.width(), frame.height())) => {
                placeholders += 1;
            }
            Some(frame) => {
                assert_eq!((frame.width(), frame.height()), sent, "{video}");
                return Some(frame);
            }
            None => {}
        }
        if Instant::now() >= deadline {
            assert!(
                placeholders > 0,
                "no {video} video arrived within {PATIENCE:?}"
            );
            common::skip(
                decoder,
                &format!(
                    "the NDI runtime has no NDI|HX decoder for {video}: all {placeholders} frames \
                     it delivered in place of the {}x{} picture within {PATIENCE:?} were its \
                     1920x1080 placeholder",
                    sent.0, sent.1
                ),
            );
            return None;
        }
    }
}

#[test]
fn genlock_clocks_wait_on_the_system_clock_without_a_reference() {
    let audio_request = AudioClockRequest::new(48_000, 800).unwrap();
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut genlock = Genlock::new(&ndi, &GenlockOptions::new()).unwrap();
    for _ in 0..2 {
        let (mut video, mut audio) = genlock.clocks();
        std::thread::scope(|scope| {
            let video = scope.spawn(move || {
                let began = Instant::now();
                for _ in 0..4 {
                    assert_eq!(
                        video.wait(FrameRate::new(60, 1).unwrap(), ScanType::Progressive),
                        ClockSource::SystemClock
                    );
                }
                began.elapsed()
            });
            let audio = scope.spawn(move || {
                let began = Instant::now();
                for _ in 0..4 {
                    assert_eq!(audio.wait(audio_request), ClockSource::SystemClock);
                }
                began.elapsed()
            });
            for elapsed in [video.join().unwrap(), audio.join().unwrap()] {
                assert!(
                    elapsed >= Duration::from_millis(20),
                    "the clock did not pace: {elapsed:?}"
                );
                assert!(elapsed < PATIENCE, "the clock took {elapsed:?}");
            }
        });
        // Reconnecting is allowed only once both unique clock borrows ended.
        genlock.disconnect();
    }
}

#[test]
fn avsync_follows_source_format_changes_and_preserves_audio_samples() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("avsync-format"))
            .clock_video(true)
            .clock_audio(false),
    )
    .unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &sender).unwrap()),
    )
    .unwrap();
    let sync = AvSync::new(receiver).unwrap();
    let phase = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stop = StopOnDrop(&stop);
        scope.spawn(|| {
            let video = [
                VideoFrame::builder()
                    .resolution(64, 32)
                    .frame_rate(FrameRate::new(30, 1).unwrap())
                    .build()
                    .unwrap(),
                VideoFrame::builder()
                    .resolution(128, 64)
                    .frame_rate(FrameRate::new(25, 1).unwrap())
                    .build()
                    .unwrap(),
            ];
            let audio = [
                AudioFrame::builder()
                    .sample_rate(48_000)
                    .num_channels(2)
                    .num_samples(1600)
                    .data(vec![0.25; 3200])
                    .build()
                    .unwrap(),
                AudioFrame::builder()
                    .sample_rate(44_100)
                    .num_channels(1)
                    .num_samples(1764)
                    .data(vec![0.5; 1764])
                    .build()
                    .unwrap(),
            ];
            while !stop.load(Ordering::Acquire) {
                let index = phase.load(Ordering::Acquire);
                sender.send_video(&video[index]).unwrap();
                sender.send_audio(&audio[index]);
            }
        });
        for (index, rate, channels, width, sample) in
            [(0, 48_000, 2, 64, 0.25f32), (1, 44_100, 1, 128, 0.5)]
        {
            phase.store(index, Ordering::Release);
            let deadline = Instant::now() + PATIENCE;
            let mut matches = 0;
            // Video frames captured, those of this format, and the audio
            // returned for them.
            let (mut videos, mut of_format, mut audio) = (0, 0, 0);
            while matches < 4 {
                assert!(
                    Instant::now() < deadline,
                    "AVSync did not follow format {index}: {videos} video frames arrived, \
                     {of_format} of them {width} wide, audio was returned {audio} times and \
                     matched {matches} times; {}",
                    link(sync.receiver())
                );
                let Some(video) = sync.receiver().capture_video_ref(SLICE).unwrap() else {
                    continue;
                };
                videos += 1;
                of_format += usize::from(video.width() == width);
                if let AvSyncOutcome::Audio { frame, .. } = sync
                    .synchronize(stamped(&video), AvSyncRequest::automatic())
                    .unwrap()
                {
                    audio += 1;
                    if video.width() == width
                        && frame.sample_rate() == rate
                        && frame.num_channels() == channels
                    {
                        assert!(!frame.data().is_empty());
                        // The first packet after a format change may contain
                        // alignment silence. A stable, matching frame must
                        // eventually preserve the source's samples.
                        if frame
                            .data()
                            .iter()
                            .all(|value| (*value - sample).abs() < 0.001)
                        {
                            matches += 1;
                        }
                    }
                }
            }
        }
    });
}

#[test]
fn genlock_follows_a_local_reference_and_returns_to_the_system_clock() {
    let audio_request = AudioClockRequest::new(48_000, 1600).unwrap();
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    if genlock_acquires_nothing(&ndi) {
        return;
    }
    let _turn = sockets();
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("genlock-reference"))
            .clock_video(true)
            .clock_audio(false),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let mut genlock = Genlock::new(&ndi, &GenlockOptions::new().source(source.clone())).unwrap();
    let thirty = FrameRate::new(30, 1).unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stop = StopOnDrop(&stop);
        scope.spawn(|| {
            let video = video();
            let audio = AudioFrame::builder()
                .sample_rate(48_000)
                .num_channels(2)
                .num_samples(1600)
                .build()
                .unwrap();
            while !stop.load(Ordering::Acquire) {
                sender.send_video(&video).unwrap();
                sender.send_audio(&audio);
            }
        });
        await_reference(&mut genlock, &sender, &source);
        let (mut video, mut audio) = genlock.clocks();
        let deadline = Instant::now() + PATIENCE;
        loop {
            let video = video.wait(thirty, ScanType::Progressive);
            let audio = audio.wait(audio_request);
            if video == ClockSource::ReferenceSignal && audio == ClockSource::ReferenceSignal {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the clocks did not use the reference"
            );
        }
    });
    genlock.disconnect();
    let deadline = Instant::now() + PATIENCE;
    let (mut video, mut audio) = genlock.clocks();
    loop {
        let video = video.wait(thirty, ScanType::Progressive);
        let audio = audio.wait(audio_request);
        if video == ClockSource::SystemClock && audio == ClockSource::SystemClock {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the clocks did not fall back after disconnect"
        );
    }
}

/// Whether this runtime's genlock acquires no reference: the Windows 6.4
/// runtimes, x64 and x86, whose genlock connects to its reference and stays
/// inactive. The measured limitation is recorded at:
/// https://github.com/GrantSparks/grafton-ndi/blob/226fa3ef36196bd20c3f0060c3fa0e233b700ace/README.md#receiving-and-sending
fn genlock_acquires_nothing(ndi: &Ndi) -> bool {
    let unsupported = cfg!(windows)
        && ndi
            .info()
            .version()
            .is_some_and(|version| (version.major(), version.minor()) >= (6, 4));
    if unsupported {
        common::unsupported("the Windows 6.4 runtime's genlock acquires no reference");
    }
    unsupported
}

/// Wait on `genlock`'s video clock until the genlock receives the reference
/// that `sender` sends, which it was connected to at `source`.
fn await_reference(genlock: &mut Genlock, sender: &Sender, source: &Source) {
    let thirty = FrameRate::new(30, 1).unwrap();
    let deadline = Instant::now() + PATIENCE;
    let mut waits = 0;
    while !genlock.is_active() {
        assert!(
            Instant::now() < deadline,
            "genlock did not acquire the reference of {source:?} in {waits} waits on its video \
             clock; {}{}",
            served(sender),
            tcp_table(source)
        );
        let (mut video, _) = genlock.clocks();
        video.wait(thirty, ScanType::Progressive);
        waits += 1;
    }
}

/// Wait with `wait` until what it reports is `wanted`, on whatever thread
/// calls this.
fn until_paced_by(wanted: ClockSource, mut wait: impl FnMut() -> ClockSource, what: &str) {
    let deadline = Instant::now() + PATIENCE;
    while wait() != wanted {
        assert!(
            Instant::now() < deadline,
            "the {what} clock was not paced by {wanted:?}"
        );
    }
}

/// A genlock works on threads other than the one that created it: it is
/// connected to a reference and acquires it on a second thread, its clocks
/// are paced by the reference on two more, and it is disconnected and
/// destroyed on a fifth.
#[test]
fn a_genlock_moves_between_threads() {
    let audio_request = AudioClockRequest::new(48_000, 1600).unwrap();
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    if genlock_acquires_nothing(&ndi) {
        return;
    }
    let _turn = sockets();
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("moved-genlock"))
            .clock_video(true)
            .clock_audio(false),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    // Created on this thread, without a reference, and never used here again.
    let genlock = Genlock::new(&ndi, &GenlockOptions::new()).unwrap();
    let thirty = FrameRate::new(30, 1).unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stop = StopOnDrop(&stop);
        scope.spawn(|| {
            let video = video();
            let audio = AudioFrame::builder()
                .sample_rate(48_000)
                .num_channels(2)
                .num_samples(1600)
                .build()
                .unwrap();
            while !stop.load(Ordering::Acquire) {
                sender.send_video(&video).unwrap();
                sender.send_audio(&audio);
            }
        });
        let (source, sender) = (&source, &sender);
        let genlock = scope
            .spawn(move || {
                let mut genlock = genlock;
                genlock.connect_to(source);
                await_reference(&mut genlock, sender, source);
                let (mut video, mut audio) = genlock.clocks();
                std::thread::scope(|clocks| {
                    clocks.spawn(move || {
                        let wait = || video.wait(thirty, ScanType::Progressive);
                        until_paced_by(ClockSource::ReferenceSignal, wait, "video");
                    });
                    clocks.spawn(move || {
                        let wait = || audio.wait(audio_request);
                        until_paced_by(ClockSource::ReferenceSignal, wait, "audio");
                    });
                });
                genlock
            })
            .join()
            .unwrap();
        scope
            .spawn(move || {
                let mut genlock = genlock;
                genlock.disconnect();
                let (mut video, _) = genlock.clocks();
                let wait = || video.wait(thirty, ScanType::Progressive);
                until_paced_by(ClockSource::SystemClock, wait, "video");
                drop(genlock);
            })
            .join()
            .unwrap();
    });
}

/// Aligned external storage shared by the native packet allocator tests.
struct PacketStorage {
    pointer: NonNull<u8>,
    layout: Layout,
    freed: Arc<AtomicBool>,
}
// SAFETY: PacketStorage exclusively owns its aligned initialized allocation;
// transfer between SDK threads changes no pointer or storage extent.
unsafe impl Send for PacketStorage {}
// SAFETY: alloc_zeroed provides 64-byte aligned initialized memory, the
// pointer/extent never change, and only the SDK accesses it before Drop.
unsafe impl ExternalAllocation for PacketStorage {
    fn pointer(&self) -> NonNull<u8> {
        self.pointer
    }
    fn capacity(&self) -> usize {
        self.layout.size()
    }
}
impl Drop for PacketStorage {
    fn drop(&mut self) {
        // SAFETY: this value owns the allocation made with this Layout,
        // and the SDK has released its last lease before dropping it.
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
        self.freed.store(true, Ordering::Release);
    }
}

/// A borrowed compressed packet stays in the exact application allocation
/// granted to the native receiver, until its receive guard is released.
#[test]
fn borrowed_packet_payload_stays_in_external_allocator_memory() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let buffers = Arc::new(Mutex::new(Vec::new()));
    let mut sender = CompressedSender::new(&ndi, &options("borrowed-external-packet")).unwrap();
    let receiver = PacketReceiver::new(
        &ndi,
        &ReceiverOptions::new(loopback(&ndi, &sender.sender().source().unwrap())),
        PassthroughPreset::ProtectedWithAudio,
    )
    .unwrap();
    let service =
        AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
    let mut receiver = service.attach_packets(receiver).unwrap();
    let grants = buffers.clone();
    receiver
        .set_video_allocator(move |request: AllocationRequest| {
            let layout = Layout::from_size_align(request.minimum_bytes(), 64).unwrap();
            // SAFETY: requests are positive bounded extents, with checked alignment.
            let pointer = NonNull::new(unsafe { alloc_zeroed(layout) }).expect("test allocation");
            let freed = Arc::new(AtomicBool::new(false));
            grants
                .lock()
                .unwrap()
                .push((pointer.as_ptr() as usize, layout.size(), freed.clone()));
            AllocationBuffer::from_external(
                request,
                PacketStorage {
                    pointer,
                    layout,
                    freed,
                },
            )
            .map_err(|error| error.into_error())
        })
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    let held = loop {
        assert!(
            Instant::now() < deadline,
            "no allocator-backed compressed packet: {}",
            link(receiver.receiver())
        );
        let mut frame = keyframe(VideoCodec::H264, StreamTier::Program, FIRST_TIMECODE);
        sender.send_video(&mut frame);
        if let Some(PacketItemRef::Video(ReceivedVideoRef::Compressed(frame))) =
            receiver.capture_any_ref(SLICE).unwrap()
        {
            break frame;
        }
    };
    let pointer = held.data().as_ptr() as usize;
    let length = held.data().len();
    let freed = buffers
        .lock()
        .unwrap()
        .iter()
        .find_map(|(start, capacity, freed)| {
            (*start <= pointer && pointer + length <= start + capacity).then(|| freed.clone())
        })
        .expect("payload must be inside the application's granted allocation");
    assert!(!freed.load(Ordering::Acquire));
    assert!(service.usage().retained_allocations() > 0);
    std::thread::sleep(Duration::from_millis(20));
    assert!(
        !freed.load(Ordering::Acquire),
        "held reference keeps its allocation alive"
    );
    receiver.receiver().connection_stats().unwrap();
    drop(held);
    let deadline = Instant::now() + PATIENCE;
    while !freed.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "native packet lease outlived its dropped guard"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // The exact buffer is now released, but PacketStorage::drop sets its flag
    // before the registry finishes bookkeeping, and the SDK may still own
    // other queued buffers. Destroying the receiver drains those callbacks
    // and prevents new grants before inspecting the process-wide counters.
    drop(receiver);
    drop(sender);
    let usage = service.usage();
    assert_eq!(
        (
            usage.retained_allocations(),
            usage.retained_bytes(),
            usage.generations(),
            usage.active_generations(),
        ),
        (0, 0, 0, 0),
        "an allocation or generation outlived its receiver: {usage:?}"
    );
}

#[test]
fn compressed_audio_packets_use_exact_custom_allocator_grants() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let mut sender = CompressedSender::new(&ndi, &options("external-compressed-audio")).unwrap();
    let receiver = PacketReceiver::new(
        &ndi,
        &ReceiverOptions::new(loopback(&ndi, &sender.sender().source().unwrap())),
        PassthroughPreset::ProtectedWithAudio,
    )
    .unwrap();
    let service =
        AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
    let mut receiver = service.attach_packets(receiver).unwrap();
    receiver
        .set_video_allocator(AllocationBuffer::zeroed)
        .unwrap();
    let grants = Arc::new(Mutex::new(Vec::new()));
    let audio_grants = grants.clone();
    receiver
        .set_audio_allocator(move |request: AllocationRequest| {
            let layout = Layout::from_size_align(request.minimum_bytes(), 64).unwrap();
            // SAFETY: the callback's request bounds this positive initialized
            // allocation, and its native lease owns the resulting buffer.
            let pointer = NonNull::new(unsafe { alloc_zeroed(layout) }).expect("test allocation");
            let freed = Arc::new(AtomicBool::new(false));
            audio_grants
                .lock()
                .unwrap()
                .push((pointer.as_ptr() as usize, request, freed.clone()));
            AllocationBuffer::from_external(
                request,
                PacketStorage {
                    pointer,
                    layout,
                    freed,
                },
            )
            .map_err(|error| error.into_error())
        })
        .unwrap();
    let declines = service.usage().declined_allocations();
    for (index, (codec, samples)) in [(AudioCodec::Aac, 1024), (AudioCodec::Opus, 960)]
        .into_iter()
        .enumerate()
    {
        let timecode = FIRST_TIMECODE + index as i64;
        let bytes = audio_bytes(codec, 48_000, 2, timecode);
        let mut audio = CompressedAudioFrame::new(
            AudioPacketFormat::new(codec, 48_000, 2, samples).unwrap(),
            bytes,
        )
        .unwrap();
        audio.set_timecode(Timecode::from_ticks(timecode));
        let mut video = keyframe(VideoCodec::H264, StreamTier::Program, timecode);
        let deadline = Instant::now() + PATIENCE;
        loop {
            assert!(
                Instant::now() < deadline,
                "no custom-allocated {codec:?} audio arrived: {}",
                link(receiver.receiver()),
            );
            sender.send_video(&mut video);
            sender.sender().send_compressed_audio(&mut audio);
            if let Some(PacketItemRef::Audio(ReceivedAudioRef::Compressed(packet))) =
                receiver.capture_any_ref(SLICE).unwrap()
            {
                if packet.timecode().ticks() != timecode {
                    continue;
                }
                assert_eq!(packet.format().codec(), codec);
                assert_eq!(
                    (
                        packet.format().sample_rate(),
                        packet.format().num_channels(),
                        packet.format().num_samples()
                    ),
                    (48_000, 2, samples),
                );
                let pointer = packet.data().as_ptr() as usize;
                let length = packet.data().len();
                let grants = grants.lock().unwrap();
                let (_, request, _) = grants
                    .iter()
                    .find(|(start, request, freed)| {
                        !freed.load(Ordering::Acquire)
                            && *start <= pointer
                            && pointer + length <= start + request.minimum_bytes()
                    })
                    .expect("compressed audio must remain inside a live application grant");
                let fourcc = match codec {
                    AudioCodec::Aac => 0xff,
                    AudioCodec::Opus => u32::from_le_bytes(*b"Opus"),
                    _ => unreachable!("the test sends AAC and Opus only"),
                };
                assert_eq!(
                    request.format(),
                    AllocationFormat::CompressedAudio(grafton_ndi::FourCc::from_raw(fourcc)),
                );
                assert_eq!(request.minimum_bytes(), length, "{codec:?} request size");
                assert_eq!(request.stride_bytes(), length, "{codec:?} size union");
                assert_eq!((request.width(), request.height()), (samples, 2));
                assert_eq!(service.usage().declined_allocations(), declines);
                break;
            }
        }
    }
    drop(receiver);
    drop(sender);
    assert!(grants
        .lock()
        .unwrap()
        .iter()
        .all(|(_, _, freed)| freed.load(Ordering::Acquire)));
    let usage = service.usage();
    assert_eq!(usage.declined_allocations(), declines, "{usage:?}");
    assert_eq!(
        (
            usage.retained_allocations(),
            usage.retained_bytes(),
            usage.generations(),
            usage.active_generations(),
            usage.in_progress_allocations(),
            usage.in_progress_reclamations(),
        ),
        (0, 0, 0, 0, 0, 0),
        "compressed audio storage outlived its receiver: {usage:?}",
    );
}

#[test]
fn borrowed_packet_capture_decodes_formats_outside_its_preset() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    if !common::hx_decoder(&ndi, H264Decoder) {
        return;
    }
    let _turn = sockets();
    let sender = CompressedSender::new(
        &ndi,
        &options("borrowed-decoded-fallback").clock_video(true),
    )
    .unwrap();
    let receiver = PacketReceiver::new(
        &ndi,
        &ReceiverOptions::new(loopback(&ndi, &sender.sender().source().unwrap())),
        PassthroughPreset::SpeedHq,
    )
    .unwrap();
    let bitstream = hx_fixture::h264();
    let config = parameter_sets(bitstream, |header| matches!(header & 0x1f, 1..=5));
    let format = VideoPacketFormat::new(VideoCodec::H264, 64, 64)
        .unwrap()
        .with_frame_rate(FrameRate::new(30, 1).unwrap());
    // Decoding runs asynchronously. Keep a correctly paced source feeding it
    // while capture waits, rather than making a declared 30fps source depend
    // on 100ms receive timeouts or bursts of status/metadata notifications.
    let stop = AtomicBool::new(false);
    let submitted = AtomicUsize::new(0);
    struct StopOnDrop<'a>(&'a AtomicBool);
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    std::thread::scope(|scope| {
        // This guard signals even when capture or an assertion unwinds, so
        // the scope never waits for a producer whose stop flag was forgotten.
        let _stop_on_exit = StopOnDrop(&stop);
        scope.spawn(|| {
            let mut sender = sender;
            let mut pts = 0;
            while !stop.load(Ordering::Acquire) {
                let bytes =
                    PacketEnvelope::encode(EnvelopeCodec::H264, pts, pts, true, bitstream, config)
                        .unwrap();
                let mut packet = CompressedVideoFrame::new(format, bytes).unwrap();
                sender.send_video(&mut packet);
                submitted.fetch_add(1, Ordering::Release);
                pts += 3000;
            }
        });
        let deadline = Instant::now() + PATIENCE;
        let mut placeholders = 0;
        let mut other_items = 0;
        loop {
            match receiver.capture_any_ref(SLICE).unwrap() {
                Some(PacketItemRef::Video(ReceivedVideoRef::Decoded(frame))) => {
                    if common::is_hx_placeholder((64, 64), (frame.width(), frame.height())) {
                        placeholders += 1;
                    } else {
                        assert_eq!((frame.width(), frame.height()), (64, 64));
                        assert!(frame
                            .planes()
                            .flat_map(|plane| plane.rows())
                            .flatten()
                            .any(|&byte| byte != 0));
                        assert_eq!(frame.to_owned().width(), 64);
                        return;
                    }
                }
                Some(PacketItemRef::Video(ReceivedVideoRef::Compressed(_))) => {
                    panic!("preset must decode H.264")
                }
                Some(_) => other_items += 1,
                None => {}
            }
            if Instant::now() >= deadline {
                assert!(
                    placeholders > 0,
                    "no fallback video arrived after {} submitted frames and {other_items} other items: {}",
                    submitted.load(Ordering::Acquire),
                    link(receiver.receiver())
                );
                common::skip(
                    H264Decoder,
                    "packet receiver delivered only the SDK decoder placeholder",
                );
                return;
            }
        }
    });
}

/// AV-sync loans keep separate native buffers across consecutive requests.
/// Borrowing, owned conversion, forwarding and reverse-order release all use
/// the same matched samples and known sender timestamp.
#[test]
fn avsync_borrows_two_results_and_releases_them_out_of_order() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("avsync-loans"))
            .clock_video(true)
            .clock_audio(false),
    )
    .unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &sender).unwrap()),
    )
    .unwrap();
    let sync = AvSync::new(receiver).unwrap();
    let forward = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("avsync-forward")).clock_audio(false),
    )
    .unwrap();
    let listener = Receiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &forward).unwrap()),
    )
    .unwrap();
    let video = VideoFrame::builder()
        .resolution(64, 32)
        .frame_rate(FrameRate::new(30, 1).unwrap())
        .build()
        .unwrap();
    let audio = AudioFrame::builder()
        .sample_rate(48_000)
        .num_channels(2)
        .num_samples(1600)
        .data(vec![0.25; 3200])
        .build()
        .unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stop = StopOnDrop(&stop);
        scope.spawn(|| {
            while !stop.load(Ordering::Acquire) {
                sender.send_video(&video).unwrap();
                sender.send_audio(&audio);
            }
        });
        let next = || {
            let deadline = Instant::now() + PATIENCE;
            loop {
                assert!(Instant::now() < deadline, "no borrowed AV-sync audio");
                let Some(video) = sync.receiver().capture_video_ref(SLICE).unwrap() else {
                    continue;
                };
                if let AvSyncOutcomeRef::Audio { frame, .. } = sync
                    .synchronize_ref(stamped(&video), AvSyncRequest::automatic())
                    .unwrap()
                {
                    break frame;
                }
            }
        };
        let first = next();
        let first_samples = first.to_owned();
        let second = next();
        assert!(first.timestamp().is_some() && second.timestamp().is_some());
        assert_ne!(
            first.channel_data(0).unwrap().as_ptr(),
            second.channel_data(0).unwrap().as_ptr()
        );
        assert_eq!(first.channel_data(0), first_samples.channel_data(0));
        assert!(first
            .channels()
            .chain(second.channels())
            .flatten()
            .all(|&sample| sample == 0.25));
        let copied = AvSyncOutcomeRef::Audio {
            frame: second,
            samples_adjusted: false,
        };
        let AvSyncOutcome::Audio { frame: owned, .. } = copied.to_owned() else {
            panic!("owned audio")
        };
        if let AvSyncOutcomeRef::Audio { frame, .. } = &copied {
            assert_eq!(frame.channel_data(0), owned.channel_data(0));
            let deadline = Instant::now() + PATIENCE;
            loop {
                forward.send_audio(frame);
                if let Some(received) = listener.capture_audio(SLICE).unwrap() {
                    assert!(received.data().iter().all(|&sample| sample == 0.25));
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "borrowed forwarding produced no audio"
                );
            }
        }
        drop(copied);
        assert_eq!(first.channel_data(0), first_samples.channel_data(0));
        drop(first);
    });
}

#[test]
fn router_connection_metadata_survives_the_registered_input() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    let sender = Sender::new(&ndi, &options("router-metadata-source")).unwrap();
    let mut router = grafton_ndi::routing::Router::new(
        &ndi,
        &grafton_ndi::routing::RouterOptions::new(common::unique_name("router metadata")),
    )
    .unwrap();
    router
        .set_route(&loopback(&ndi, &sender.source().unwrap()))
        .unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(loopback(&ndi, &router.source().unwrap())),
    )
    .unwrap();
    let frame = video();
    // Routed media bypasses the router. Require the router itself to report
    // a connected receiver as well as observing video: its metadata operation
    // promises immediate delivery to receivers already connected to the router.
    // Drain every slot throughout startup and delivery; retained-pointer
    // fixtures separately cover reads on later connections, clear and destroy.
    let connected_deadline = Instant::now() + PATIENCE;
    let mut received_video = false;
    loop {
        sender.send_video(&frame).unwrap();
        received_video |= matches!(
            receiver.capture_any_ref(SLICE).unwrap(),
            Some(grafton_ndi::ReceiverItemRef::Video(_))
        );
        let router_connections = router.connection_count().unwrap();
        if received_video && router_connections > 0 {
            break;
        }
        assert!(
            Instant::now() < connected_deadline,
            "router not ready before registration: {router_connections} router connections, \
             received video {received_video}; {}",
            link(&receiver)
        );
    }
    let text = "<router-registration/>";
    router
        .add_connection_metadata(
            &MetadataFrame::new(text)
                .unwrap()
                .with_timecode(Timecode::from_ticks(22)),
        )
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    let mut observed_metadata = Vec::new();
    loop {
        sender.send_video(&frame).unwrap();
        if let Some(grafton_ndi::ReceiverItemRef::Metadata(metadata)) =
            receiver.capture_any_ref(SLICE).unwrap()
        {
            if metadata.data() == text {
                break;
            }
            if observed_metadata.len() < 4 {
                observed_metadata.push((
                    metadata.timecode(),
                    metadata.data().len(),
                    metadata.data().chars().take(256).collect::<String>(),
                ));
            }
        }
        assert!(
            Instant::now() < deadline,
            "router connection metadata was not delivered; router connections {:?}; \
             observed metadata (timecode, full byte length, first 256 chars) {observed_metadata:?}; {}",
            router.connection_count(),
            link(&receiver)
        );
    }
    router.clear_connection_metadata().unwrap();
}

/// Preallocated 1080p blocks are released exactly once after native capture
/// and receiver teardown, even when callbacks run on other SDK threads.
#[test]
fn pool_backed_receive_releases_every_1080p_block() {
    use std::{
        alloc::{alloc_zeroed, dealloc, Layout},
        ptr::NonNull,
    };
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let _turn = sockets();
    struct Pool {
        blocks: Vec<PoolBlock>,
        free: Mutex<Vec<usize>>,
        layout: Layout,
        loans: AtomicUsize,
        releases: AtomicUsize,
    }
    // SAFETY: each aligned zeroed allocation stays owned by the pool Arc.
    // Removing a slot grants exclusive SDK access until release returns it.
    unsafe impl ExternalPool for Pool {
        unsafe fn release(&self, block: PoolBlock) {
            assert_eq!(self.blocks[block.tag()], block);
            self.releases.fetch_add(1, Ordering::AcqRel);
            self.free.lock().unwrap().push(block.tag());
        }
    }
    impl Drop for Pool {
        fn drop(&mut self) {
            assert_eq!(self.free.get_mut().unwrap().len(), self.blocks.len());
            for block in &self.blocks {
                // SAFETY: the last pool Arc is gone only after every SDK loan
                // has released; each allocation is reclaimed once with its Layout.
                unsafe { dealloc(block.pointer().as_ptr(), self.layout) };
            }
        }
    }
    let layout = Layout::from_size_align(1920 * 1080 * 2, 64).unwrap();
    let blocks = (0..16)
        .map(|tag| {
            // SAFETY: the positive frame extent and 64-byte alignment are checked.
            let pointer = NonNull::new(unsafe { alloc_zeroed(layout) }).expect("pool allocation");
            PoolBlock::new(pointer, layout.size(), tag)
        })
        .collect();
    let pool = Arc::new(Pool {
        blocks,
        free: Mutex::new((0..16).collect()),
        layout,
        loans: AtomicUsize::new(0),
        releases: AtomicUsize::new(0),
    });
    let sender = Sender::new(&ndi, &options("pool-allocation")).unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &sender).unwrap())
            .color_format(ReceiverColorFormat::UyvyBgra),
    )
    .unwrap();
    let service =
        AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
    let mut receiver = service.attach(receiver).unwrap();
    let policy_pool = pool.clone();
    receiver
        .set_video_allocator(move |request: AllocationRequest| {
            let tag = policy_pool
                .free
                .lock()
                .unwrap()
                .pop()
                .ok_or(Error::AllocationFailed(grafton_ndi::AllocationFailure {
                    kind: grafton_ndi::AllocationKind::AllocatorLeases,
                    bytes: None,
                }))?;
            let block = policy_pool.blocks[tag];
            // SAFETY: this slot was removed exclusively from the free list. The
            // pool owns its initialized aligned memory until this loan is released.
            match unsafe { AllocationBuffer::from_pool(request, &policy_pool, block) } {
                Ok(buffer) => {
                    policy_pool.loans.fetch_add(1, Ordering::AcqRel);
                    Ok(buffer)
                }
                Err(rejected) => {
                    policy_pool.free.lock().unwrap().push(tag);
                    Err(rejected.into_error())
                }
            }
        })
        .unwrap();
    let frame = VideoFrame::builder()
        .resolution(1920, 1080)
        .pixel_format(grafton_ndi::PixelFormat::Uyvy)
        .build()
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(
            Instant::now() < deadline,
            "no pool-backed native 1080p frame"
        );
        sender.send_video(&frame).unwrap();
        if let Some(received) = receiver.receiver().capture_video(SLICE).unwrap() {
            assert_eq!((received.width(), received.height()), (1920, 1080));
            if pool.loans.load(Ordering::Acquire) > 0 {
                break;
            }
        }
    }
    drop(receiver);
    drop(sender);
    assert!(pool.loans.load(Ordering::Acquire) > 0);
    assert_eq!(
        pool.loans.load(Ordering::Acquire),
        pool.releases.load(Ordering::Acquire)
    );
    assert_eq!(pool.free.lock().unwrap().len(), pool.blocks.len());
}
