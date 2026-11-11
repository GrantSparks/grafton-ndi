//! Owned async send lifecycle stress. The default path has no SDK callback.
mod common;

use grafton_ndi::{Sender, SenderOptions, VideoFrame};

#[test]
#[ignore = "slow stress test; run explicitly"]
fn rapid_owned_sender_lifecycle() -> Result<(), grafton_ndi::Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
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
