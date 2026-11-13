mod common;

use grafton_ndi::{
    AudioFrame, Error, FrameRate, FrameSync, FrameSyncAudioRequest, Receiver, ReceiverOptions,
    ScanType, Sender, SenderOptions, Source, Tally, VideoFrame,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

// Raw audio capture on the attached receiver does not compile: the
// synchronizer takes the receiver's audio
// (tests/ui/fail/framesync_receiver_has_no_audio_capture.rs).
#[test]
fn attached_receiver_permits_controls_metadata_and_events_and_recovery_restores_raw_capture(
) -> Result<(), Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let source = Source::new("FrameSync capture boundary");
    let mut receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
    for _ in 0..3 {
        let framesync = FrameSync::new(receiver)?;
        let _video = framesync.capture_video_ref(ScanType::Progressive)?;
        drop(_video);
        let _audio =
            framesync.capture_audio_ref(FrameSyncAudioRequest::capture_at(48_000, 2, 32)?)?;
        drop(_audio);
        framesync.receiver().set_tally(Tally::new(false, true));
        assert!(!framesync.receiver().ptz().is_supported());
        let _ = framesync.receiver().metadata().capture(Duration::ZERO)?;
        let _ = framesync.receiver().poll_event(Duration::ZERO)?;
        receiver = framesync.into_receiver();
        assert!(receiver.video().capture(Duration::ZERO)?.is_none());
        assert!(receiver.audio().capture_ref(Duration::ZERO)?.is_none());
    }
    Ok(())
}

/// Whether every pixel of a received BGRX frame has the gray level of its
/// first pixel, as every frame the sender below sends does.
fn uniform<'a>(rows: impl Iterator<Item = &'a [u8]>) -> bool {
    let mut level = None;
    rows.flat_map(|row| row.as_chunks::<4>().0).all(|pixel| {
        let first = *level.get_or_insert(pixel[0]);
        pixel[..3].iter().all(|&value| value.abs_diff(first) <= 6)
    })
}

/// `FrameSync` is `Sync`: one FrameSync serves video and audio captures, the
/// input-format query and the queue depth from several threads at once, with
/// a live source. Every call succeeds, every video frame is whole and every
/// audio sample is one the source could have sent.
#[test]
fn one_framesync_serves_captures_from_several_threads_at_once() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("FrameSync threads")).clock_video(true),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let framesync = FrameSync::new(Receiver::new(&ndi, &ReceiverOptions::new(source)).unwrap())
        .map_err(Error::from)
        .unwrap();
    let audio = AudioFrame::builder()
        .num_samples(1600)
        .data(
            (0..3200)
                .map(|i| (i % 1600) as f32 / 1600.0 - 0.5)
                .collect(),
        )
        .build()
        .unwrap();
    let stop = AtomicBool::new(false);
    struct Stop<'a>(&'a AtomicBool);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    std::thread::scope(|scope| {
        scope.spawn(|| {
            for frame in 0u32.. {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                // Each frame is one gray level, 16 steps from the last.
                let level = (frame % 16 * 16 + 8) as u8;
                let pixels = [level, level, level, 255].repeat(64 * 36);
                let video = VideoFrame::builder()
                    .resolution(64, 36)
                    .frame_rate(FrameRate::new(30, 1).unwrap())
                    .data(pixels)
                    .build()
                    .unwrap();
                sender.send_video(&video).unwrap();
                sender.send_audio(&audio);
            }
        });
        // Ends the sender however the captures below end.
        let _stop = Stop(&stop);

        let deadline = Instant::now() + Duration::from_secs(15);
        while framesync
            .capture_video_ref(ScanType::Progressive)
            .unwrap()
            .is_none()
        {
            assert!(Instant::now() < deadline, "no video arrived");
            std::thread::sleep(Duration::from_millis(10));
        }

        let framesync = &framesync;
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                scope.spawn(move || {
                    let request = FrameSyncAudioRequest::capture_at(48_000, 2, 480).unwrap();
                    let began = Instant::now();
                    let mut calls = 0u32;
                    while began.elapsed() < Duration::from_secs(1) {
                        match (worker + calls) % 4 {
                            0 => {
                                let video = framesync
                                    .capture_video_ref(ScanType::Progressive)
                                    .unwrap()
                                    .expect("video arrived before the workers started");
                                assert_eq!((video.width(), video.height()), (64, 36));
                                let plane = video.planes().next().unwrap();
                                assert!(uniform(plane.rows()), "a torn video frame");
                            }
                            1 => {
                                let audio = framesync.capture_audio_ref(request).unwrap();
                                assert!(audio
                                    .channels()
                                    .flatten()
                                    .all(|sample| sample.is_finite() && sample.abs() <= 2.0));
                            }
                            2 => {
                                let _ = framesync.audio_input_format().unwrap();
                            }
                            _ => {
                                let _ = framesync.audio_queue_depth();
                            }
                        }
                        calls += 1;
                    }
                    calls
                })
            })
            .collect();
        for worker in workers {
            assert!(worker.join().unwrap() > 0);
        }
    });
}
