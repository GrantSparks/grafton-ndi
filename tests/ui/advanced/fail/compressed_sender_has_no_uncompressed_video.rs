use grafton_ndi::{advanced::CompressedSender, VideoFrame};
fn send(sender: &CompressedSender, frame: &VideoFrame) {
    let _ = sender.sender().send_video(frame);
}
fn main() {}
