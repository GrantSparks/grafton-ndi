//! The 6.4 bandwidth values must suppress audio in a real connection.
mod common;

use grafton_ndi::{
    capabilities::RuntimeFeature, AudioFrame, Receiver, ReceiverBandwidth, ReceiverOptions, Sender,
    SenderOptions, VideoFrame,
};
use std::time::{Duration, Instant};

#[test]
fn video_only_modes_deliver_video_while_an_ordinary_receiver_also_gets_audio() {
    let Some(ndi) = common::ndi() else { return };
    if ndi
        .require_feature(RuntimeFeature::ReceiveVideoOnly)
        .is_err()
    {
        // The constructors' rejection on older runtimes is tested separately.
        return;
    }
    let sender = Sender::new(
        &ndi,
        &SenderOptions::builder(common::unique_name("video-only"))
            .clock_video(false)
            .clock_audio(false)
            .build(),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let ordinary = Receiver::new(&ndi, &ReceiverOptions::builder(source.clone()).build()).unwrap();
    let video = VideoFrame::builder().resolution(64, 32).build().unwrap();
    let audio = AudioFrame::builder()
        .sample_rate(48_000)
        .channels(2)
        .samples(1600)
        .build()
        .unwrap();
    for bandwidth in [
        ReceiverBandwidth::VideoOnlyHighest,
        ReceiverBandwidth::VideoOnlyLowest,
    ] {
        let receiver = Receiver::new(
            &ndi,
            &ReceiverOptions::builder(source.clone())
                .bandwidth(bandwidth)
                .build(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut videos = 0;
        let mut ordinary_audio = 0;
        let mut connected_since = None;
        loop {
            sender.send_video(&video);
            sender.send_audio(&audio);
            ordinary_audio += usize::from(
                ordinary
                    .audio()
                    .try_capture(Duration::ZERO)
                    .unwrap()
                    .is_some(),
            );
            if receiver
                .video()
                .try_capture(Duration::from_millis(20))
                .unwrap()
                .is_some()
            {
                videos += 1;
                connected_since.get_or_insert_with(Instant::now);
            }
            assert!(
                receiver
                    .audio()
                    .try_capture(Duration::ZERO)
                    .unwrap()
                    .is_none(),
                "{bandwidth:?} delivered audio"
            );
            if videos >= 10
                && ordinary_audio > 0
                && connected_since.is_some_and(|since| since.elapsed() >= Duration::from_secs(1))
            {
                break;
            }
            assert!(Instant::now() < deadline,
                "{bandwidth:?}: received {videos} video frames and {ordinary_audio} ordinary audio frames");
        }
    }
}
