use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
fn main() {
    let mut pixels = [0; 16];
    let frame = BorrowedVideoFrame::builder(&pixels, 2, 2, PixelFormat::Bgra)
        .build()
        .unwrap();
    pixels.fill(1);
    let _ = frame.to_owned();
}
