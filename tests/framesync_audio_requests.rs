//! SDK regression: an application follows the source's channels through
//! `FrameSync::audio_input_format`, and a capture request keeps its own
//! channel count when the source changes.
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

use grafton_ndi::{
    AudioFrame, FrameSync, FrameSyncAudioRequest, Receiver, ReceiverOptions, Sender, SenderOptions,
};
use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

fn wait_for_channels(fs: &FrameSync, channels: u32) -> grafton_ndi::AudioInputFormat {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(format) = fs.audio_input_format().unwrap() {
            if format.num_channels() == channels {
                assert_eq!(format.sample_rate(), 48_000);
                return format;
            }
        }
        assert!(
            Instant::now() < deadline,
            "input format did not reach {channels} channels"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn queried_channels_remain_fixed_when_source_format_changes() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    eprintln!(
        "SDK: {} on {}",
        ndi.info().raw_version(),
        std::env::consts::OS
    );
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("FrameSync audio request regression"))
            .clock_audio(true),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source)).unwrap();
    let fs = FrameSync::new(receiver).unwrap();
    let source_channels = Arc::new(AtomicU32::new(2));
    std::thread::scope(|scope| {
        let worker_channels = source_channels.clone();
        scope.spawn(move || {
            let stereo = AudioFrame::builder()
                .num_channels(2)
                .num_samples(480)
                .build()
                .unwrap();
            let quad = AudioFrame::builder()
                .num_channels(4)
                .num_samples(480)
                .build()
                .unwrap();
            loop {
                match worker_channels.load(Ordering::Relaxed) {
                    0 => break,
                    2 => sender.send_audio(&stereo),
                    4 => sender.send_audio(&quad),
                    _ => unreachable!(),
                }
            }
        });
        struct Stop(Arc<AtomicU32>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.store(0, Ordering::Relaxed);
            }
        }
        let _stop = Stop(source_channels.clone());

        let format = wait_for_channels(&fs, 2);
        let request = FrameSyncAudioRequest::for_input(format, 480).unwrap();
        {
            let frame = fs.capture_audio_ref(request).unwrap();
            assert_eq!((frame.num_channels(), frame.num_samples()), (2, 480));
            assert!(frame.channels().all(|channel| channel.len() == 480));
        }

        // Deliberately change the source between the format query, request
        // construction and capture.
        source_channels.store(4, Ordering::Relaxed);
        let new_format = wait_for_channels(&fs, 4);
        for _ in 0..3 {
            let frame = fs.capture_audio_ref(request).unwrap();
            assert_eq!(
                (
                    frame.sample_rate(),
                    frame.num_channels(),
                    frame.num_samples()
                ),
                (48_000, 2, 480)
            );
            assert_eq!(frame.channels().len(), 2);
            assert!(frame.channels().all(|channel| channel.len() == 480));
            let owned = fs.capture_audio(request).unwrap();
            assert_eq!((owned.num_channels(), owned.num_samples()), (2, 480));
            assert_eq!(owned.channel_data(1).unwrap().len(), 480);
        }
        // Applications can explicitly choose to follow the newly queried format.
        let updated = FrameSyncAudioRequest::for_input(new_format, 480).unwrap();
        let frame = fs.capture_audio_ref(updated).unwrap();
        assert_eq!((frame.num_channels(), frame.num_samples()), (4, 480));
        assert!(frame.channels().all(|channel| channel.len() == 480));
    });
}
