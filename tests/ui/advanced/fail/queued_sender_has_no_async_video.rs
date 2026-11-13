use grafton_ndi::{advanced::QueuedSender, VideoFrame};
fn bypass_the_queue(sender: &QueuedSender, frame: VideoFrame) {
    let _ = sender.sender().send_video_async(frame);
}
fn main() {}
