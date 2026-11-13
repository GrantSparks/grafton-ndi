//! Real-runtime checks for FrameSync output and per-media reception.
mod common;
#[path = "../examples/common/reception_monitor.rs"]
mod reception_monitor;

use grafton_ndi::{
    AudioFrame, FrameRate, FrameSync, FrameSyncAudioRequest, Receiver, ReceiverBandwidth,
    ReceiverOptions, ScanType, Sender, SenderOptions, VideoFrame,
};
use reception_monitor::{Reception, ReceptionMonitor};
use std::{
    sync::atomic::{AtomicU8, Ordering},
    thread,
    time::{Duration, Instant},
};

fn wait_until(mut check: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !check() {
        assert!(Instant::now() < deadline, "{message}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn audio_reception_distinguishes_silent_input_from_starvation_and_recovers() {
    let Some(ndi) = common::ndi() else { return };
    eprintln!(
        "FrameSync lifecycle: SDK {} {:?} on {} {}",
        ndi.info().raw_version(),
        ndi.info().edition(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("FrameSync reception lifecycle"))
            .clock_audio(false),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let framesync = FrameSync::new(
        Receiver::new(
            &ndi,
            &ReceiverOptions::new(source).bandwidth(ReceiverBandwidth::AudioOnly),
        )
        .unwrap(),
    )
    .map_err(grafton_ndi::Error::from)
    .unwrap();
    let request = FrameSyncAudioRequest::capture_at(48_000, 1, 480).unwrap();
    let mut monitor = ReceptionMonitor::new(Duration::from_millis(300));
    let capture = || {
        let output = framesync.capture_audio_ref(request).unwrap();
        assert_eq!(
            (
                output.sample_rate(),
                output.num_channels(),
                output.num_samples()
            ),
            (48_000, 1, 480)
        );
    };
    wait_until(
        || framesync.receiver().connection_stats().is_connected(),
        "receiver did not connect before audio",
    );
    let initial = framesync.receiver().connection_stats();
    assert_eq!(initial.audio_frames_received(), 0);
    assert_eq!(
        monitor.observe(true, initial.audio_frames_received(), Instant::now()),
        Reception::AwaitingInput
    );
    capture();
    // Before the first source packet, the fixed request still yields silence.
    assert!(framesync
        .capture_audio_ref(request)
        .unwrap()
        .channels()
        .flatten()
        .all(|sample| *sample == 0.0));

    let phase = AtomicU8::new(0); // 0 paused, 1 sending zeros, 2 stop worker
    thread::scope(|scope| {
        scope.spawn(|| {
            let audio = AudioFrame::builder()
                .sample_rate(48_000)
                .num_channels(2)
                .num_samples(480)
                .build()
                .unwrap();
            while phase.load(Ordering::Relaxed) != 2 {
                if phase.load(Ordering::Relaxed) == 1 {
                    sender.send_audio(&audio);
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        struct Stop<'a>(&'a AtomicU8);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(2, Ordering::Relaxed);
            }
        }
        let _stop = Stop(&phase);
        phase.store(1, Ordering::Relaxed);
        wait_until(
            || {
                capture();
                let stats = framesync.receiver().connection_stats();
                monitor.observe(
                    stats.is_connected(),
                    stats.audio_frames_received(),
                    Instant::now(),
                ) == Reception::Recent
            },
            "silent source packets did not advance the audio counter",
        );
        assert!(framesync.audio_input_format().unwrap().is_some());
        assert_eq!(
            framesync
                .receiver()
                .connection_stats()
                .video_frames_received(),
            0
        );
        phase.store(0, Ordering::Relaxed);
        wait_until(
            || {
                capture();
                let stats = framesync.receiver().connection_stats();
                monitor.observe(
                    stats.is_connected(),
                    stats.audio_frames_received(),
                    Instant::now(),
                ) == Reception::Starved
            },
            "paused audio never became starved",
        );
        framesync.receiver().disconnect();
        monitor.reset();
        wait_until(
            || {
                let stats = framesync.receiver().connection_stats();
                monitor.observe(
                    stats.is_connected(),
                    stats.audio_frames_received(),
                    Instant::now(),
                ) == Reception::Disconnected
            },
            "explicit disconnect was not observed",
        );
        framesync.receiver().reconnect().unwrap();
        monitor.reset();
        wait_until(
            || framesync.receiver().connection_stats().is_connected(),
            "receiver did not reconnect",
        );
        let stats = framesync.receiver().connection_stats();
        assert_eq!(
            monitor.observe(
                stats.is_connected(),
                stats.audio_frames_received(),
                Instant::now()
            ),
            Reception::AwaitingInput
        );
        capture();
        phase.store(1, Ordering::Relaxed);
        wait_until(
            || {
                capture();
                let stats = framesync.receiver().connection_stats();
                monitor.observe(
                    stats.is_connected(),
                    stats.audio_frames_received(),
                    Instant::now(),
                ) == Reception::Recent
            },
            "resumed audio did not establish fresh progress",
        );
        phase.store(2, Ordering::Relaxed);
    });
    drop(sender);
    wait_until(
        || {
            let stats = framesync.receiver().connection_stats();
            monitor.observe(
                stats.is_connected(),
                stats.audio_frames_received(),
                Instant::now(),
            ) == Reception::Disconnected
        },
        "sender disappearance was not observed",
    );
    capture();
}

#[test]
fn retained_video_output_does_not_advance_received_counter() {
    let Some(ndi) = common::ndi() else { return };
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("FrameSync retained video")).clock_video(false),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let framesync = FrameSync::new(Receiver::new(&ndi, &ReceiverOptions::new(source)).unwrap())
        .map_err(grafton_ndi::Error::from)
        .unwrap();
    let video = VideoFrame::builder()
        .resolution(64, 32)
        .frame_rate(FrameRate::new(30, 1).unwrap())
        .build()
        .unwrap();
    wait_until(
        || {
            sender.send_video(&video).unwrap();
            framesync
                .receiver()
                .connection_stats()
                .video_frames_received()
                > 0
        },
        "video was not received",
    );
    // Let in-flight input drain. Output may be retained or absent depending
    // on the runtime; either way, capture availability is not input progress.
    let mut previous = framesync
        .receiver()
        .connection_stats()
        .video_frames_received();
    let mut stable_since = Instant::now();
    wait_until(
        || {
            let now = framesync
                .receiver()
                .connection_stats()
                .video_frames_received();
            if now != previous {
                previous = now;
                stable_since = Instant::now();
            }
            stable_since.elapsed() >= Duration::from_millis(500)
        },
        "video counter did not stabilize",
    );
    let before = framesync
        .receiver()
        .connection_stats()
        .video_frames_received();
    let first = framesync
        .capture_video_ref(ScanType::Progressive)
        .unwrap()
        .is_some();
    let second = framesync
        .capture_video_ref(ScanType::Progressive)
        .unwrap()
        .is_some();
    let after = framesync
        .receiver()
        .connection_stats()
        .video_frames_received();
    eprintln!("retained video output: first={first}, second={second}, received={before}");
    assert_eq!(before, after);
    if ndi
        .info()
        .version()
        .is_some_and(|version| version.to_string() == "6.4.0.2")
    {
        assert!(
            first && second,
            "6.4.0.2 retained output after input stopped"
        );
    }
}
