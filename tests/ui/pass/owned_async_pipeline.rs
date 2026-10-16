#![allow(unused)]
use grafton_ndi::{Sender, VideoFrame};

fn pipeline(sender: &mut Sender, first: VideoFrame, second: VideoFrame) {
    let first_ptr = first.data().as_ptr();
    let initial: Option<VideoFrame> = sender.send_video_async(first);
    let returned: Option<VideoFrame> = sender.send_video_async(second);
    if let Some(mut first) = returned {
        assert_eq!(first.data().as_ptr(), first_ptr);
        first.data_mut().fill(0);
        let _ = sender.send_video_async(first);
    }
    let last: Option<VideoFrame> = sender.finish_video_async();
}
// Forgetting the sender also retains its owned in-flight storage.
fn forgotten_sender(mut sender: Sender, frame: VideoFrame) {
    let _ = sender.send_video_async(frame);
    std::mem::forget(sender);
}
fn main() {}
