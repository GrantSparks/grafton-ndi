use grafton_ndi::{advanced::CompressedSender, AudioFrame};
fn send(sender: &CompressedSender, frame: &AudioFrame) {
    sender.sender().send_audio(frame);
}
fn main() {}
