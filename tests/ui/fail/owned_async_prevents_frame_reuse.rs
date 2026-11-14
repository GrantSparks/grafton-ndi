use grafton_ndi::{Sender, VideoFrame};
fn reuse(sender: &mut Sender, mut frame: VideoFrame) {
    let _ = sender.send_video_async(frame);
    frame.data_mut().fill(0);
}
fn main() {
    let _ = reuse;
}
