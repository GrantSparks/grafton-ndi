use grafton_ndi::{advanced::QueuedSender, VideoFrame};
fn bypass_the_queue(sender: &QueuedSender, frame: VideoFrame) {
    let _ = sender.sender().submit_video(frame);
}
fn main() {}
