use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
fn main() {
    let mut pixels = [0; 24];
    let frame = BorrowedVideoFrame::builder(&pixels, 2, 2, PixelFormat::Bgra)
        .line_stride(12)
        .build()
        .unwrap();
    let plane = frame.plane(0).unwrap();
    pixels.fill(1);
    let _ = plane.row(0);
}
