//! Explicit copying convenience for pixels owned by another subsystem.
//!
//! `Sender::send_video(frame)` sends the same borrowed frame synchronously
//! without copying it; copy only when the asynchronous pipeline is wanted.
//! `send_video_async_copy` copies into a new frame on every call, while
//! `BorrowedVideoFrame::copy_into` copies into a frame a send has returned, so
//! the loop below allocates no frames once it has two.
use grafton_ndi::{BorrowedVideoFrame, FrameRate, Ndi, PixelFormat, SenderOptions, VideoFrame};

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
    // The source can be reused as soon as this returns; the copy stays owned by
    // the sender until the next submission or finish returns it.
    let _ = sender.send_video_async_copy(&borrow(&pixels)?)?;
    let copy = sender.finish_video_async().unwrap();
    assert_eq!(copy.data().len(), pixels.len());

    // Reuse the recovered copy, and then whichever frame each send returns.
    let mut spare: Option<VideoFrame> = Some(copy);
    for shade in 0..60 {
        pixels.fill(shade);
        let frame = borrow(&pixels)?;
        let owned = match spare.take() {
            Some(mut reused) => {
                frame.copy_into(&mut reused);
                reused
            }
            None => frame.to_owned(),
        };
        spare = sender.send_video_async(owned)?;
    }
    sender.finish_video_async();
    Ok(())
}
