//! Explicit copying convenience for pixels owned by another subsystem.
//!
//! `Sender::send_video(&frame)` sends the same borrowed frame synchronously
//! without copying it; copy only when the asynchronous pipeline is wanted.
//! `send_video_async_copy` retains two reusable frames. Once both have enough
//! pixel and metadata capacity, subsequent copies allocate no frame storage.
use grafton_ndi::{BorrowedVideoFrame, FrameRate, Ndi, PixelFormat, SenderOptions};

fn borrow(pixels: &[u8]) -> Result<BorrowedVideoFrame<'_>, grafton_ndi::Error> {
    BorrowedVideoFrame::builder(pixels, 1920, 1080, PixelFormat::Bgrx)
        .frame_rate(FrameRate::new(30, 1)?)
        .metadata(c"<example/>")
        .build()
}

fn main() -> Result<(), grafton_ndi::Error> {
    let ndi = Ndi::new()?;
    let options = SenderOptions::new("Copying Async Example");
    let mut sender = grafton_ndi::Sender::new(&ndi, &options)?;
    let mut pixels = vec![0u8; 1920 * 1080 * 4];
    // Each call owns its copy before returning, so the source can be reused
    // immediately. The sender recycles released copies between submissions.
    for shade in 0..60 {
        pixels.fill(shade);
        sender.send_video_async_copy(&borrow(&pixels)?)?;
    }
    let last = sender.finish_video_async().unwrap();
    assert_eq!(last.data().len(), pixels.len());
    Ok(())
}
