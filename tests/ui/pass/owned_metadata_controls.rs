use grafton_ndi::{AudioFrame, VideoFrame};
fn main() -> grafton_ndi::Result<()> {
    let mut video = VideoFrame::builder().build()?;
    let mut audio = AudioFrame::builder().build()?;
    video.set_metadata("<video/>")?;
    audio.set_metadata(String::from("<audio/>"))?;
    video.clear_metadata();
    audio.clear_metadata();
    assert!(video.metadata().is_none() && audio.metadata().is_none());
    Ok(())
}
