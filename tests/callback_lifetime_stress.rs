//! Owned async send lifecycle stress. The default path has no SDK callback.
use grafton_ndi::{Sender, SenderOptions, VideoFrame, NDI};

#[test]
#[ignore = "Slow stress test requiring an installed NDI runtime"]
fn rapid_owned_sender_lifecycle() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    for iteration in 0..20 {
        let options = SenderOptions::builder(format!("owned lifecycle {iteration}"))
            .clock_video(false)
            .clock_audio(false)
            .build();
        let mut sender = Sender::new(&ndi, &options)?;
        let mut next = VideoFrame::builder().resolution(16, 16).build()?;
        for value in 0..50 {
            next.data_mut().fill(value);
            let returned = sender.send_video_async(next);
            next = match returned {
                Some(frame) => frame,
                None => VideoFrame::builder().resolution(16, 16).build()?,
            };
        }
        // Exercise sender-owned draining without an explicit finish call.
        drop(sender);
    }
    Ok(())
}
