//! One Standard capture wait receives whichever stream is actually present.
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

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

#[cfg(any(feature = "tokio", feature = "smol"))]
async fn scoped_loopback<R: grafton_ndi::asynchronous::AsyncRuntime>(
    ndi: &grafton_ndi::Ndi,
    sender: &Sender,
    source: grafton_ndi::Source,
) -> Result<(), Error> {
    use grafton_ndi::{asynchronous::AsyncReceiver, ReceiverItemRef};
    let receiver = AsyncReceiver::<R>::new(Receiver::new(ndi, &ReceiverOptions::new(source))?);
    let video = VideoFrame::builder()
        .resolution(16, 16)
        .timecode(Timecode::from_ticks(1234))
        .metadata("<scoped-video/>")
        .build()?;
    let mut audio = AudioFrame::builder()
        .sample_rate(48_000)
        .num_channels(2)
        .num_samples(480)
        .build()?;
    audio.data_mut().fill(0.25);
    let metadata = MetadataFrame::new("<scoped-metadata/>")?;
    for mixed in [false, true] {
        receiver.reconnect().await?;
        let mut observed = [false; 3];
        let deadline = Instant::now() + Duration::from_secs(15);
        while observed != [true; 3] {
            sender.send_video(&video)?;
            sender.send_audio(&audio);
            sender.send_metadata(&metadata);
            if mixed {
                let summary = receiver
                    .capture_any_with(Duration::from_millis(30), |item| match item {
                        ReceiverItemRef::Video(frame) => {
                            assert_eq!((frame.width(), frame.height()), (16, 16));
                            assert_eq!(frame.timecode(), Timecode::from_ticks(1234));
                            assert_eq!(frame.metadata(), Some("<scoped-video/>"));
                            Some(0)
                        }
                        ReceiverItemRef::Audio(frame) => {
                            assert_eq!((frame.sample_rate(), frame.num_channels()), (48_000, 2));
                            for ch in 0..frame.num_channels() {
                                assert!(frame
                                    .channel_data(ch as usize)
                                    .unwrap()
                                    .iter()
                                    .all(|&v| (v - 0.25).abs() < 0.001));
                            }
                            Some(1)
                        }
                        ReceiverItemRef::Metadata(frame)
                            if frame.data() == "<scoped-metadata/>" =>
                        {
                            Some(2)
                        }
                        _ => None,
                    })
                    .await?;
                if let Some(Some(kind)) = summary {
                    observed[kind] = true;
                }
            } else {
                if !observed[0] {
                    observed[0] = receiver
                        .video()
                        .capture_with(Duration::from_millis(30), |frame| {
                            assert_eq!((frame.width(), frame.height()), (16, 16));
                            assert_eq!(frame.timecode(), Timecode::from_ticks(1234));
                            assert_eq!(frame.metadata(), Some("<scoped-video/>"));
                        })
                        .await?
                        .is_some();
                }
                if !observed[1] {
                    observed[1] = receiver
                        .audio()
                        .capture_with(Duration::from_millis(30), |frame| {
                            assert_eq!(frame.num_channels(), 2);
                            assert!(frame
                                .channel_data(0)
                                .unwrap()
                                .iter()
                                .all(|&v| (v - 0.25).abs() < 0.001));
                        })
                        .await?
                        .is_some();
                }
                if !observed[2] {
                    observed[2] = receiver
                        .metadata()
                        .capture_with(Duration::from_millis(30), |frame| {
                            frame.data() == "<scoped-metadata/>"
                        })
                        .await?
                        == Some(true);
                }
            }
            assert!(
                Instant::now() < deadline,
                "scoped mixed={mixed}: saw {observed:?}"
            );
        }
    }
    Ok(())
}

#[cfg(any(feature = "tokio", feature = "smol"))]
#[test]
fn scoped_async_capture_receives_loopback_media() -> Result<(), Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    eprintln!(
        "scoped async SDK {} {:?} on {} {}",
        ndi.info().raw_version(),
        ndi.info().edition(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("scoped async"))
            .clock_video(false)
            .clock_audio(false),
    )?;
    let source = common::loopback_source(&ndi, &sender)?;
    #[cfg(feature = "tokio")]
    ::tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(scoped_loopback::<grafton_ndi::asynchronous::Tokio>(
            &ndi,
            &sender,
            source.clone(),
        ))?;
    #[cfg(feature = "smol")]
    ::smol::block_on(scoped_loopback::<grafton_ndi::asynchronous::Smol>(
        &ndi, &sender, source,
    ))?;
    Ok(())
}
