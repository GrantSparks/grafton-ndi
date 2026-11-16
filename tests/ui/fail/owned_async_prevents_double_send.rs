use grafton_ndi::{Sender, VideoFrame};
fn resend(sender: &mut Sender, frame: VideoFrame) {
    let _ = sender.submit_video(frame);
    let _ = sender.submit_video(frame);
}
fn main() {
    let _ = resend;
}
