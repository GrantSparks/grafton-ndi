//! The 6.4 bandwidth values must suppress audio in a real connection.
mod common;

use grafton_ndi::{
    capabilities::RuntimeFeature, AudioFrame, FrameRate, Receiver, ReceiverBandwidth,
    ReceiverOptions, Sender, SenderOptions, VideoFrame,
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
    // Send in real time: clocked 30 fps video, each frame paired with 1600
    // samples (1/30 s) of 48 kHz audio. An unclocked loop sends hundreds of
    // times faster, and on a busy machine the runtime then drops the ordinary
    // receiver's connection and does not restore it within the deadline.
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("video-only"))
            .clock_video(true)
            .clock_audio(false),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let ordinary = Receiver::new(&ndi, &ReceiverOptions::new(source.clone())).unwrap();
    let video = VideoFrame::builder()
        .resolution(64, 32)
        .frame_rate(FrameRate::new(30, 1).unwrap())
        .build()
        .unwrap();
    let audio = AudioFrame::builder()
        .sample_rate(48_000)
        .num_channels(2)
        .num_samples(1600)
        .build()
        .unwrap();
    for bandwidth in [
        ReceiverBandwidth::VideoOnlyHighest,
        ReceiverBandwidth::VideoOnlyLowest,
    ] {
        let receiver = Receiver::new(
            &ndi,
            &ReceiverOptions::new(source.clone()).bandwidth(bandwidth),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut videos = 0;
        let mut ordinary_audio = 0;
        let mut connected_since = None;
        loop {
            sender.send_video(&video).unwrap();
            sender.send_audio(&audio);
            ordinary_audio +=
                usize::from(ordinary.audio().capture(Duration::ZERO).unwrap().is_some());
            if receiver
                .video()
                .capture(Duration::from_millis(20))
                .unwrap()
                .is_some()
            {
                videos += 1;
                connected_since.get_or_insert_with(Instant::now);
            }
            assert!(
                receiver.audio().capture(Duration::ZERO).unwrap().is_none(),
                "{bandwidth:?} delivered audio"
            );
            if videos >= 10
                && ordinary_audio > 0
                && connected_since.is_some_and(|since| since.elapsed() >= Duration::from_secs(1))
            {
                break;
            }
            // What the video-only receiver, the ordinary one and the sender
            // report tells a receiver that never connected from one that was
            // given no stream at this bandwidth.
            assert!(
                Instant::now() < deadline,
                "{bandwidth:?}: received {videos} video frames and {ordinary_audio} ordinary \
                 audio frames; the {bandwidth:?} receiver of {:?} reports {:?}, the ordinary \
                 one {:?}, and the sender counts {:?} connections",
                receiver.source(),
                receiver.connection_stats(),
                ordinary.connection_stats(),
                sender.connection_count(Duration::ZERO)
            );
        }
    }
}
