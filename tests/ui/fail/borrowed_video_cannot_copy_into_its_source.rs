use grafton_ndi::{BorrowedVideoFrame, VideoFrame};
fn main() {
    let mut frame = VideoFrame::builder().resolution(2, 2).build().unwrap();
    // A copy reads its source while it rewrites its destination, so a view
    // of a frame cannot be copied into that frame.
    let view = BorrowedVideoFrame::builder(
        frame.data(),
        frame.width(),
        frame.height(),
        frame.pixel_format(),
    )
    .build()
    .unwrap();
    view.copy_into(&mut frame);
}
