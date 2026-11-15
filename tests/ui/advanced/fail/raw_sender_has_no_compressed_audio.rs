use grafton_ndi::{advanced::CompressedAudioFrame, Sender};
fn send(sender: &Sender, audio: &mut CompressedAudioFrame) {
    sender.send_compressed_audio(audio);
}
fn main() {}
