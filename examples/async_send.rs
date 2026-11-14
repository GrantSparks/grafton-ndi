//! Async video with two reusable owned frames; no completion-channel wait is needed.
use grafton_ndi::{Ndi, SenderOptions, VideoFrame};

fn main() -> Result<(), grafton_ndi::Error> {
    let ndi = Ndi::new()?;
    let options = SenderOptions::new("AsyncExample").clock_video(true);
    let mut sender = grafton_ndi::Sender::new(&ndi, &options)?;
    let first = VideoFrame::builder().resolution(1920, 1080).build()?;
    let mut next = VideoFrame::builder().resolution(1920, 1080).build()?;
    assert!(sender.send_video_async(first)?.is_none());
    for index in 1..120 {
        for pixel in next.data_mut().as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[index as u8, 0, 128, 255]);
        }
        next = sender
            .send_video_async(next)?
            .expect("previous completed frame");
    }
    let last = sender.finish_video_async().expect("final completed frame");
    println!(
        "Retained two buffers: {} and {} bytes",
        next.data().len(),
        last.data().len()
    );
    Ok(())
}
