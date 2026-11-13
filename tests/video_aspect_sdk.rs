//! Display geometry survives outbound construction, receive, and owned copies.
mod common;

use grafton_ndi::{
    BorrowedVideoFrame, PixelFormat, Receiver, ReceiverOptions, Sender, SenderOptions, Timecode,
    VideoFrame,
};
use std::time::{Duration, Instant};

#[test]
fn non_widescreen_video_arrives_with_its_requested_display_geometry() -> grafton_ndi::Result<()> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("aspect regression"))
            .clock_video(false)
            .clock_audio(false),
    )?;
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &sender)?),
    )?;
    for (case, (width, height)) in [(640, 480), (1920, 1080), (1080, 1920), (64, 64)]
        .into_iter()
        .enumerate()
    {
        for (mode, ratio) in [None, Some(1.25)].into_iter().enumerate() {
            let timecode = Timecode::from_ticks(1 + (case * 2 + mode) as i64);
            let mut builder = VideoFrame::builder()
                .resolution(width, height)
                .timecode(timecode);
            if let Some(ratio) = ratio {
                builder = builder.picture_aspect_ratio(ratio);
            }
            let video = builder.build()?;
            let mut borrowed =
                BorrowedVideoFrame::builder(video.data(), width, height, PixelFormat::Bgra)
                    .timecode(timecode);
            if let Some(ratio) = ratio {
                borrowed = borrowed.picture_aspect_ratio(ratio);
            }
            let borrowed = borrowed.build()?;
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                // Exercise both construction paths on the native boundary.
                sender.send_video(&video)?;
                sender.send_video(&borrowed)?;
                if let Some(received) = receiver.video().capture_ref(Duration::from_millis(100))? {
                    if received.timecode() == timecode {
                        assert_eq!((received.width(), received.height()), (width, height));
                        // A runtime may resolve the zero sentinel to the dimension ratio.
                        let effective = if received.picture_aspect_ratio() == 0.0 {
                            width as f32 / height as f32
                        } else {
                            received.picture_aspect_ratio()
                        };
                        let expected = ratio.unwrap_or(width as f32 / height as f32);
                        assert!(
                            (effective - expected).abs() < 0.0001,
                            "{width}x{height}: expected {expected}, received {effective}"
                        );
                        let mut copy = received.to_owned();
                        assert_eq!(copy.picture_aspect_ratio(), received.picture_aspect_ratio());
                        copy.set_picture_aspect_ratio(2.0)?;
                        received.copy_into(&mut copy);
                        assert_eq!(copy.picture_aspect_ratio(), received.picture_aspect_ratio());
                        break;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "aspect case {case}/{mode} was not received"
                );
            }
        }
    }
    Ok(())
}
