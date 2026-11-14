use grafton_ndi::{Sender, VideoFrame};
fn resend(sender: &mut Sender, frame: VideoFrame) {
    let _ = sender.send_video_async(frame);
    let _ = sender.send_video_async(frame);
}
fn main() {
    let _ = resend;
}
