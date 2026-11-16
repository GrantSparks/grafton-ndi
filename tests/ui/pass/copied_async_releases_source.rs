use grafton_ndi::{BorrowedVideoFrame, PixelFormat, Sender, VideoFrame};

fn submit(sender: &mut Sender, mut source: VideoFrame) {
    sender.submit_video_copy(&source).unwrap();
    source.data_mut().fill(7);
    drop(source);
    let mut bytes = vec![0; 16 * 16 * 4];
    let frame = BorrowedVideoFrame::builder(&bytes, 16, 16, PixelFormat::Bgra)
        .build()
        .unwrap();
    sender.submit_video_copy(&frame).unwrap();
    bytes.fill(9);
    drop(bytes);
    let _ = sender.reclaim_video();
}
fn main() {
    let _ = submit;
}
