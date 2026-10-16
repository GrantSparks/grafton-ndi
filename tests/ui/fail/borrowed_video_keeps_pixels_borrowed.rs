use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
fn main() {
    let mut pixels = [0; 16];
    let frame = BorrowedVideoFrame::builder(&pixels, 2, 2, PixelFormat::BGRA)
        .build()
        .unwrap();
    pixels.fill(1);
    let _ = frame.try_to_owned();
}
