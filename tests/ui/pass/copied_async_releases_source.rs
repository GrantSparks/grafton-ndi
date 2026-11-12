#![allow(dead_code)]
use grafton_ndi::{BorrowedVideoFrame, PixelFormat, Sender, VideoFrame};

fn submit(sender: &mut Sender, mut source: VideoFrame) {
    let _ = sender
        .send_video_async_copy(&BorrowedVideoFrame::from(&source))
        .unwrap();
    source.data_mut().fill(7);
    drop(source);
    let mut bytes = vec![0; 16 * 16 * 4];
    let frame = BorrowedVideoFrame::builder(&bytes, 16, 16, PixelFormat::Bgra)
        .build()
        .unwrap();
    let previous = sender.send_video_async_copy(&frame).unwrap();
    bytes.fill(9);
    drop(bytes);
    let _ = sender.finish_video_async();
    drop(previous);
}
fn main() {}
