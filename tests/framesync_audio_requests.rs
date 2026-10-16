//! SDK regression for query-to-request migration and changing source channels.
use grafton_ndi::{
    AudioFrame, FrameSync, FrameSyncAudioRequest, Receiver, ReceiverOptions, Sender, SenderOptions,
    SourceAddress, NDI,
};
use std::{
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

fn wait_for_channels(fs: &FrameSync, channels: i32) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let format = fs
            .capture_audio(FrameSyncAudioRequest::query_input())
            .unwrap();
        if format.num_channels() == channels {
            assert_eq!(format.sample_rate(), 48_000);
            assert!(format.is_empty());
            return format.num_channels();
        }
        assert!(
            Instant::now() < deadline,
            "input format did not reach {channels} channels"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "Requires NDI runtime and loopback networking; run alone"]
fn queried_channels_remain_fixed_when_source_format_changes() {
    let ndi = NDI::new().unwrap();
    eprintln!(
        "SDK: {} on {}",
        NDI::version().unwrap(),
        std::env::consts::OS
    );
    let sender = Sender::new(
        &ndi,
        &SenderOptions::builder("FrameSync audio request regression")
            .clock_audio(true)
            .build(),
    )
    .unwrap();
    let mut source = sender.source().unwrap();
    let port = match &source.address {
        SourceAddress::Ip(address) => address.rsplit(':').next().unwrap_or("5961"),
        _ => "5961",
    };
    source.address = SourceAddress::Ip(format!("127.0.0.1:{port}"));
    let receiver = Receiver::new(&ndi, &ReceiverOptions::builder(source).build()).unwrap();
    let fs = FrameSync::new(receiver).unwrap();
    let source_channels = Arc::new(AtomicI32::new(2));
    std::thread::scope(|scope| {
        let worker_channels = source_channels.clone();
        scope.spawn(move || {
            let stereo = AudioFrame::builder()
                .channels(2)
                .samples(480)
                .build()
                .unwrap();
            let quad = AudioFrame::builder()
                .channels(4)
                .samples(480)
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
        struct Stop(Arc<AtomicI32>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.store(0, Ordering::Relaxed);
            }
        }
        let _stop = Stop(source_channels.clone());

        let channels = wait_for_channels(&fs, 2);
        let request = FrameSyncAudioRequest::capture(channels, 480).unwrap();
        {
            let frame = fs.capture_audio(request).unwrap();
            assert_eq!((frame.num_channels(), frame.num_samples()), (2, 480));
            assert!(frame.channels().all(|channel| channel.len() == 480));
        }

        // Deliberately change the source between query/construction and capture.
        source_channels.store(4, Ordering::Relaxed);
        let new_channels = wait_for_channels(&fs, 4);
        for _ in 0..3 {
            let frame = fs.capture_audio(request).unwrap();
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
            let owned = fs.capture_audio_owned(request).unwrap().unwrap();
            assert_eq!((owned.num_channels(), owned.num_samples()), (2, 480));
            assert_eq!(owned.channel_data(1).unwrap().len(), 480);
        }
        // Applications can explicitly choose to follow the newly queried count.
        let updated = FrameSyncAudioRequest::capture(new_channels, 480).unwrap();
        let frame = fs.capture_audio(updated).unwrap();
        assert_eq!((frame.num_channels(), frame.num_samples()), (4, 480));
        assert!(frame.channels().all(|channel| channel.len() == 480));
    });
}
