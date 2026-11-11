//! Explicitly opted-in native Advanced SDK tests, using only test-owned peers.
#![cfg(all(feature = "advanced", ndi_has_advanced_header))]
mod common;
use grafton_ndi::{
    advanced::*, capabilities::SdkEdition, runtime::RuntimeOptions, AudioFrame, Finder,
    FinderOptions, Receiver, ReceiverOptions, Sender, SenderOptions, Source, Tally, VideoFrame,
    NDI,
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

fn runtime() -> NDI {
    let mut options = RuntimeOptions::default().edition(SdkEdition::Advanced);
    if let Some(path) = std::env::var_os("GRAFTON_NDI_RUNTIME") {
        options = options.library_path(std::path::PathBuf::from(path));
    }
    NDI::with_options(&options).expect("authorized Advanced runtime required")
}
fn options(suffix: &str) -> SenderOptions {
    SenderOptions::builder(format!("grafton-local-{}-{suffix}", std::process::id()))
        .clock_video(false)
        .clock_audio(false)
        .build()
}
fn video() -> VideoFrame {
    VideoFrame::builder().resolution(64, 32).build().unwrap()
}

#[test]
#[ignore = "requires an authorized local Advanced SDK runtime"]
fn advanced_native_constructors_controls_and_queue() {
    let ndi = runtime();
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
    let _send_advertiser = SendAdvertiser::new_with_config(
        &ndi,
        &AdvertiserOptions::default().servers("127.0.0.1"),
        &config,
    )
    .unwrap();
    let _recv_advertiser = RecvAdvertiser::new_with_config(
        &ndi,
        &AdvertiserOptions::default().servers("127.0.0.1"),
        &config,
    )
    .unwrap();
    let _send_listener = SendListener::new_with_config(
        &ndi,
        &ListenerOptions::default().server("127.0.0.1").unwrap(),
        &config,
    )
    .unwrap();
    let _recv_listener = RecvListener::new_with_config(
        &ndi,
        &ListenerOptions::default().server("127.0.0.1").unwrap(),
        &config,
    )
    .unwrap();
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
    drop(compressed.close());
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

#[test]
#[ignore = "requires an authorized local Advanced runtime and local transport"]
fn advanced_native_allocator_replacement_and_capture() {
    let ndi = runtime();
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
    for _generation in 0..2 {
        let count = Arc::new(AtomicUsize::new(0));
        let allocation_count = count.clone();
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
        assert!(captured, "test-owned local sender did not deliver a video frame; generation={_generation}, allocator calls={}, connected={}, last error={last_error:?}", count.load(Ordering::Relaxed), receiver.receiver().is_connected());
        assert!(
            count.load(Ordering::Relaxed) > 0,
            "custom generation did not allocate"
        );
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
    receiver.reset_video_allocator();
    receiver.reset_audio_allocator();
    drop(receiver.into_receiver());
}
