//! Advanced SDK operations against a real runtime, using only peers these
//! tests create.
//!
//! The tests that need an Advanced SDK runtime start with `advanced_ndi()`, so
//! they are skipped visibly elsewhere and fail where
//! `GRAFTON_NDI_REQUIRE_RUNTIME` promises `advanced`. They compile in every
//! build: the Advanced API is the same with and without the SDK's headers.
#![cfg(feature = "advanced")]

mod common;

use grafton_ndi::{
    advanced::*, capabilities::RuntimeFeature, AudioFrame, Error, Finder, FinderOptions, Receiver,
    ReceiverOptions, Sender, SenderOptions, Source, Tally, VideoFrame, NDI,
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

fn options(suffix: &str) -> SenderOptions {
    SenderOptions::builder(format!("grafton-local-{}-{suffix}", std::process::id()))
        .clock_video(false)
        .clock_audio(false)
        .build()
}

fn video() -> VideoFrame {
    VideoFrame::builder().resolution(64, 32).build().unwrap()
}

fn unconnected_receiver(ndi: &NDI) -> Receiver {
    Receiver::new(ndi, &ReceiverOptions::builder(Source::default()).build()).unwrap()
}

#[test]
fn advanced_native_constructors_controls_and_queue() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let config = AdvancedConfig::parse("{}").unwrap();
    let sender = Sender::new_with_config(&ndi, &options("controls"), &config).unwrap();
    let source = sender.source().unwrap();
    let mut receiver =
        Receiver::new_with_config(&ndi, &ReceiverOptions::builder(source).build(), &config)
            .unwrap();
    let finder = Finder::new_with_config(&ndi, &FinderOptions::default(), &config).unwrap();
    let _snapshot = finder.sources_with_metadata().unwrap();
    let _ = sender.set_external_tally(Tally::new(false, false)).unwrap();
    let _ = sender.keyframe_required(None).unwrap();
    let _ = sender
        .wait_for_keyframe_change(Duration::ZERO, None)
        .unwrap();
    receiver.request_keyframe().unwrap();
    let _ = receiver.set_video_quality(VideoQuality::Program).unwrap();
    let _ = receiver
        .set_streams(StreamSelection::new(true, true, true))
        .unwrap();
    {
        let session = receiver.kvm().unwrap();
        let _ = session.is_supported();
    }
    let router = grafton_ndi::routing::Router::new_with_config(
        &ndi,
        &grafton_ndi::routing::RouterOptions::new(format!(
            "grafton-local-{}-router",
            std::process::id()
        )),
        &config,
    )
    .unwrap();
    router
        .add_connection_metadata(
            &grafton_ndi::MetadataFrame::with_data("<local-test/>", 0).unwrap(),
        )
        .unwrap();
    router.clear_connection_metadata().unwrap();
    use grafton_ndi::discovery::{
        AdvertiserOptions, ListenerOptions, RecvAdvertiser, RecvListener, SendAdvertiser,
        SendListener,
    };
    // The Discovery Server objects need NDI 6.4.0. An Advanced runtime older
    // than that refuses to create them, with or without a configuration.
    fn created<T>(ndi: &NDI, feature: RuntimeFeature, object: grafton_ndi::Result<T>) -> Option<T> {
        match (ndi.require_feature(feature), object) {
            (Ok(()), object) => Some(object.unwrap()),
            (Err(_), Err(Error::UnsupportedCapability { .. })) => None,
            (Err(gate), Err(other)) => panic!("{gate}, but creation failed with: {other}"),
            (Err(gate), Ok(_)) => panic!("{gate}, but the object was created"),
        }
    }
    let advertisers = AdvertiserOptions::default().servers("127.0.0.1");
    let listeners = ListenerOptions::default().server("127.0.0.1").unwrap();
    let _send_advertiser = created(
        &ndi,
        RuntimeFeature::SenderAdvertising,
        SendAdvertiser::new_with_config(&ndi, &advertisers, &config),
    );
    let _recv_advertiser = created(
        &ndi,
        RuntimeFeature::ReceiverAdvertising,
        RecvAdvertiser::new_with_config(&ndi, &advertisers, &config),
    );
    let _send_listener = created(
        &ndi,
        RuntimeFeature::SenderDiscovery,
        SendListener::new_with_config(&ndi, &listeners, &config),
    );
    let _recv_listener = created(
        &ndi,
        RuntimeFeature::ReceiverDiscovery,
        RecvListener::new_with_config(&ndi, &listeners, &config),
    );
    let av = AvSync::new(receiver).unwrap();
    drop(av.into_receiver());
    let mut genlock = Genlock::new(&ndi, None, Some(&config)).unwrap();
    genlock.connect(None).unwrap();
    let _ = genlock.is_active();
    drop(genlock);
    let mut packets = PacketReceiver::new(
        &ndi,
        &ReceiverOptions::builder(Source::default()).build(),
        PassthroughPreset::HevcWithAudio,
        &config,
    )
    .unwrap();
    let _ = packets.capture(Duration::ZERO).unwrap();
    let compressed =
        CompressedSender::new_with_config(&ndi, &options("compressed"), &config).unwrap();
    let unsent = compressed.close();
    assert!(unsent.program.is_none() && unsent.preview.is_none());
    let mut queue = QueuedSender::new_with_config(
        &ndi,
        &options("queue"),
        &config,
        QueueOptions::default().max_frames(2),
    )
    .unwrap();
    let id = queue.try_submit(video()).unwrap();
    let completed = queue
        .wait_completed(Duration::from_secs(5))
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
    let sender = Sender::new(&ndi, &options("allocation")).unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::builder(common::loopback_source(&ndi, &sender).unwrap()).build(),
    )
    .unwrap();
    let service =
        AllocatorService::new(ndi.process_lifetime(), AllocatorLimits::default()).unwrap();
    let mut receiver = service.attach(receiver).unwrap();
    let audio_count = Arc::new(AtomicUsize::new(0));
    let count = audio_count.clone();
    receiver
        .set_audio_allocator(move |request: AllocationRequest| {
            count.fetch_add(1, Ordering::Relaxed);
            AllocationBuffer::zeroed(request.with_stride(request.stride() + 64)?)
        })
        .unwrap();
    let audio = AudioFrame::builder()
        .sample_rate(48_000)
        .channels(2)
        .samples(1600)
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
                AllocationBuffer::zeroed(request.with_stride(request.stride() + 64)?)
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut captured = false;
        let mut last_error = None;
        while Instant::now() < deadline {
            sender.send_video(&frame);
            sender.send_audio(&audio);
            match receiver
                .receiver()
                .video()
                .capture(Duration::from_millis(100))
            {
                Ok(received) => {
                    assert_eq!(received.width(), 64);
                    if count.load(Ordering::Relaxed) > 0 {
                        captured = true;
                        break;
                    }
                }
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
            .audio()
            .capture(Duration::from_millis(100));
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

/// A constructor that takes ownership returns what it was given when it
/// fails, still usable.
#[test]
fn failed_constructors_hand_back_what_they_were_given() {
    let Some(ndi) = common::ndi() else {
        return;
    };

    // No queue accepts a limit of zero frames, on any runtime.
    let sender = Sender::new(&ndi, &options("handback")).unwrap();
    let name = sender.source().unwrap().name;
    let rejected = QueuedSender::from_sender(sender, QueueOptions::default().max_frames(0))
        .err()
        .expect("zero frames is not a valid queue limit");
    assert!(matches!(
        rejected.error(),
        Error::InvalidConfiguration(_) | Error::UnsupportedCapability { .. }
    ));
    let sender = rejected.into_inner();
    sender.send_video(&video());
    assert_eq!(sender.source().unwrap().name, name);

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
        AllocatorService::new(ndi.process_lifetime(), AllocatorLimits::default()).unwrap();
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
    if ndi
        .require_feature(RuntimeFeature::AdvancedSenderConfiguration)
        .is_ok()
    {
        // An Advanced runtime in a build with the headers: nothing is unavailable.
        return;
    }
    let config = AdvancedConfig::parse("{}").unwrap();
    let unsupported = |error: Error| {
        let Error::UnsupportedCapability { capability, reason } = error else {
            panic!("expected an unsupported capability, got {error:?}");
        };
        assert!(!capability.is_empty() && !reason.is_empty());
    };

    unsupported(
        Sender::new_with_config(&ndi, &options("unavailable"), &config)
            .err()
            .unwrap(),
    );
    unsupported(
        Receiver::new_with_config(
            &ndi,
            &ReceiverOptions::builder(Source::default()).build(),
            &config,
        )
        .err()
        .unwrap(),
    );
    unsupported(
        Finder::new_with_config(&ndi, &FinderOptions::default(), &config)
            .err()
            .unwrap(),
    );
    unsupported(Genlock::new(&ndi, None, None).err().unwrap());
    unsupported(
        PacketReceiver::new(
            &ndi,
            &ReceiverOptions::builder(Source::default()).build(),
            PassthroughPreset::Hevc,
            &config,
        )
        .err()
        .unwrap(),
    );
    unsupported(
        CompressedSender::new(&ndi, &options("unavailable-compressed"))
            .err()
            .unwrap(),
    );
    unsupported(
        QueuedSender::new(&ndi, &options("unavailable-queue"), QueueOptions::default())
            .err()
            .unwrap(),
    );

    let sender = Sender::new(&ndi, &options("unavailable-core")).unwrap();
    unsupported(
        sender
            .set_external_tally(Tally::new(true, false))
            .unwrap_err(),
    );
    unsupported(sender.keyframe_required(None).unwrap_err());
    let mut receiver = unconnected_receiver(&ndi);
    unsupported(receiver.request_keyframe().unwrap_err());
    unsupported(
        receiver
            .set_video_quality(VideoQuality::Preview)
            .unwrap_err(),
    );
    unsupported(receiver.kvm().err().unwrap());
    let finder = Finder::new(&ndi, &FinderOptions::default()).unwrap();
    unsupported(finder.sources_with_metadata().unwrap_err());

    // The core API is unaffected.
    sender.send_video(&video());
    assert!(finder.current_sources().is_ok());
}
