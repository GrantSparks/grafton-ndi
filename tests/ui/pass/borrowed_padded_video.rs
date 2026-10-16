use grafton_ndi::{BorrowedVideoFrame, PixelFormat, ScanType};
fn main() {
    let pixels = [0; 48];
    let frame = BorrowedVideoFrame::builder(&pixels, 2, 4, PixelFormat::BGRA)
        .line_stride(12)
        .metadata(c"<video/>")
        .scan_type(ScanType::Interlaced)
        .frame_rate(30000, 1001)
        .timecode(0)
        .timestamp(0)
        .build()
        .unwrap();
    let _: PixelFormat = frame.pixel_format();
    let _: usize = frame.validated_data_len();
    let _ = frame.try_to_owned().unwrap();
}
