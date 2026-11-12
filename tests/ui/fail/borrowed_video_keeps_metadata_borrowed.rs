use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
fn main() {
    let pixels = [0; 16];
    let metadata = std::ffi::CString::new("<video/>").unwrap();
    let frame = BorrowedVideoFrame::builder(&pixels, 2, 2, PixelFormat::Bgra)
        .metadata(&metadata)
        .build()
        .unwrap();
    drop(metadata);
    let _ = frame.to_owned();
}
