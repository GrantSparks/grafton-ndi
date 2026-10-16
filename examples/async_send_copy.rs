//! Explicit copying convenience for pixels owned by another subsystem.
use grafton_ndi::{BorrowedVideoFrame, PixelFormat, SenderOptions, NDI};

fn main() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let options = SenderOptions::builder("Copying Async Example").build();
    let mut sender = grafton_ndi::Sender::new(&ndi, &options)?;
    let pixels = vec![0u8; 1920 * 1080 * 4];
    let frame = BorrowedVideoFrame::builder(&pixels, 1920, 1080, PixelFormat::BGRA)
        .frame_rate(30, 1)
        .metadata(c"<example/>")
        .build()?;
    // The source can be reused as soon as this returns; the copy stays owned by
    // the sender until the next submission or finish returns it.
    let _ = sender.send_video_async_copy(&frame)?;
    let copy = sender.finish_video_async().unwrap();
    assert_eq!(copy.data().len(), pixels.len());
    Ok(())
}
