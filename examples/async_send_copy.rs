//! Explicit copying convenience for pixels owned by another subsystem.
use grafton_ndi::{BorrowedVideoFrame, PixelFormat, SenderOptions, NDI};

fn main() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let options = SenderOptions::builder("Copying Async Example").build();
    let mut sender = grafton_ndi::Sender::new(&ndi, &options)?;
    let pixels = vec![0u8; 1920 * 1080 * 4];
    let frame =
        BorrowedVideoFrame::try_from_uncompressed(&pixels, 1920, 1080, PixelFormat::BGRA, 30, 1)?;
    // Copies pixels and metadata into sender-owned storage; the token is optional
    // for memory safety, but waiting observes completion and runs the user handler.
    let token = sender.send_video_async_copy(&frame);
    token.wait()?;
    Ok(())
}
