use grafton_ndi::{BorrowedVideoFrame, FrameRate, PixelFormat, ScanType, Timecode};
fn main() {
    let pixels = [0; 48];
    let frame = BorrowedVideoFrame::builder(&pixels, 2, 4, PixelFormat::Bgra)
        .line_stride_bytes(12)
        .metadata(c"<video/>")
        .scan_type(ScanType::Interlaced)
        .frame_rate(FrameRate::new(30000, 1001).unwrap())
        .timecode(Timecode::from_ticks(0))
        .build()
        .unwrap();
    let _: PixelFormat = frame.pixel_format();
    let _: &[u8] = frame.data();
    let _: grafton_ndi::VideoFrame = frame.to_owned();
}
