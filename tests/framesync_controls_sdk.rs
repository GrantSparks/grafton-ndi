//! Stream changes preserve a live FrameSync; KVM support is queried without
//! injecting real keyboard, mouse, touch or clipboard input.
#![cfg(feature = "advanced")]
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, prerequisites, name, advanced, loopback);

use grafton_ndi::{
    AudioFrame, FrameSync, FrameSyncAudioRequest, MediaTypes, Receiver, ReceiverOptions,
    RuntimeFeature, ScanType, Sender, SenderOptions, SettingChange, StreamTier, VideoFrame,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[test]
fn stream_changes_leave_clocked_capture_and_recovery_usable() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    for feature in [
        RuntimeFeature::ReceiverStreamTier,
        RuntimeFeature::ReceiverStreamSelection,
        RuntimeFeature::Kvm,
    ] {
        if !ndi.capabilities().supports(feature) {
            common::skip(
                common::Requirement::Advanced,
                &format!("{feature:?} unavailable"),
            );
            return;
        }
    }
    eprintln!(
        "FrameSync controls SDK: {} {:?} {}",
        ndi.info().raw_version(),
        ndi.info().edition(),
        std::env::consts::OS
    );
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("FrameSync control lifecycle")).clock_video(true),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source.clone())).unwrap();
    let mut sync = FrameSync::new(receiver).unwrap();
    let running = AtomicBool::new(true);
    struct Stop<'a>(&'a AtomicBool);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Relaxed);
        }
    }
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let video = VideoFrame::builder()
                .resolution(64, 32)
                .data(vec![128; 64 * 32 * 4])
                .build()
                .unwrap();
            let audio = AudioFrame::builder().num_samples(1600).build().unwrap();
            while running.load(Ordering::Relaxed) {
                sender.send_audio(&audio);
                sender.send_video(&video).unwrap();
            }
        });
        let _stop = Stop(&running);
        let deadline = Instant::now() + Duration::from_secs(15);
        while sync
            .capture_video_ref(ScanType::Progressive)
            .unwrap()
            .is_none()
        {
            assert!(
                Instant::now() < deadline,
                "source produced no clocked video"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        for change in [
            sync.controls()
                .set_stream_tier(StreamTier::Preview)
                .unwrap(),
            sync.controls().set_streams(MediaTypes::AUDIO).unwrap(),
            sync.controls().set_streams(MediaTypes::ALL).unwrap(),
            sync.controls()
                .set_stream_tier(StreamTier::Program)
                .unwrap(),
        ] {
            assert!(matches!(
                change,
                SettingChange::Applied | SettingChange::Unchanged
            ));
        }
        {
            let (capture, controls) = sync.split();
            let mut session = controls.kvm().unwrap();
            let _ = session.is_supported();
            let video = capture
                .capture_video_ref(ScanType::Progressive)
                .unwrap()
                .unwrap();
            let audio = capture
                .capture_audio_ref(FrameSyncAudioRequest::capture_at(48_000, 2, 480).unwrap())
                .unwrap();
            assert!(video.width() > 0 && video.height() > 0);
            assert_eq!((audio.num_channels(), audio.num_samples()), (2, 480));
            session.release_all().unwrap();
        }
        assert_eq!(sync.receiver().connection_target(), source);
        assert!(sync.capture_video(ScanType::Progressive).unwrap().is_some());
    });
    let receiver = sync.into_receiver();
    assert_eq!(receiver.connection_target(), source);
    drop(receiver);
}
