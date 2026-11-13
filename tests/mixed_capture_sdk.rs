//! One Standard capture wait receives whichever stream is actually present.
mod common;

use grafton_ndi::{
    AudioFrame, Error, MetadataFrame, Receiver, ReceiverItem, ReceiverOptions, Sender,
    SenderOptions, Timecode, VideoFrame,
};
use std::time::{Duration, Instant};

#[test]
fn combined_capture_receives_single_kinds_and_interleaved_media_after_reconnect(
) -> Result<(), Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    eprintln!(
        "combined capture SDK {} {:?} on {} {}",
        ndi.info().raw_version(),
        ndi.info().edition(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    for expected in [
        [true, false, false],
        [false, true, false],
        [false, false, true],
        [true, true, true],
    ] {
        let sender = Sender::new(
            &ndi,
            &SenderOptions::new(common::unique_name(&format!("combined {expected:?}")))
                .clock_video(false)
                .clock_audio(false),
        )?;
        let source = common::loopback_source(&ndi, &sender)?;
        let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
        let mut video = VideoFrame::builder()
            .resolution(16, 16)
            .metadata("<mixed-video/>")
            .timecode(Timecode::from_ticks(1234))
            .build()?;
        for pixel in video.data_mut().as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[80, 80, 80, 255]);
        }
        let mut audio = AudioFrame::builder()
            .sample_rate(48_000)
            .num_channels(2)
            .num_samples(480)
            .metadata("<mixed-audio/>")
            .build()?;
        audio.data_mut().fill(0.25);
        for epoch in 0..=usize::from(expected == [true, true, true]) {
            if epoch != 0 {
                receiver.disconnect();
                receiver.reconnect()?;
            }
            let marker = format!("<mixed-metadata epoch=\"{epoch}\"/>");
            let metadata =
                MetadataFrame::new(marker.clone())?.with_timecode(Timecode::from_ticks(3456));
            let mut observed = [false; 3];
            let deadline = Instant::now() + Duration::from_secs(15);
            while observed != expected {
                if expected[0] {
                    sender.send_video(&video)?;
                }
                if expected[1] {
                    sender.send_audio(&audio);
                }
                if expected[2] {
                    sender.send_metadata(&metadata);
                }
                match receiver.capture_any(Duration::from_millis(30))? {
                    Some(ReceiverItem::Video(frame)) => {
                        assert!(expected[0]);
                        assert_eq!((frame.width(), frame.height()), (16, 16));
                        assert_eq!(frame.timecode(), Timecode::from_ticks(1234));
                        assert_eq!(frame.metadata(), Some("<mixed-video/>"));
                        observed[0] = true;
                    }
                    Some(ReceiverItem::Audio(frame)) => {
                        assert!(expected[1]);
                        assert_eq!(frame.sample_rate(), 48_000);
                        assert_eq!(frame.num_channels(), 2);
                        assert!(frame
                            .data()
                            .iter()
                            .all(|&sample| (sample - 0.25).abs() < 0.001));
                        observed[1] = true;
                    }
                    Some(ReceiverItem::Metadata(frame)) if frame.data() == marker => {
                        assert!(expected[2]);
                        assert_eq!(frame.timecode(), Timecode::from_ticks(3456));
                        observed[2] = true;
                    }
                    _ => {}
                }
                assert!(
                    Instant::now() < deadline,
                    "expected {expected:?}, saw {observed:?}, epoch {epoch}, stats {:?}",
                    receiver.connection_stats()
                );
            }
        }
    }
    Ok(())
}
