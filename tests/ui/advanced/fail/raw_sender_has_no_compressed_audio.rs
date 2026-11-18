use grafton_ndi::{
    advanced::{AudioPacketFormat, CompressedFrame},
    Sender,
};
fn send(sender: &Sender, audio: &mut CompressedFrame<AudioPacketFormat>) {
    sender.send_compressed_audio(audio);
}
fn main() {}
