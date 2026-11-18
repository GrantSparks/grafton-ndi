use grafton_ndi::advanced::{CompressedFrame, QueuedSender, ScatterFrame, AudioPacketFormat};
fn send(queue: &mut QueuedSender, audio: &mut CompressedFrame<AudioPacketFormat>, scatter: &mut ScatterFrame<AudioPacketFormat>) {
    let (_video, sender) = queue.split();
    sender.send_compressed_audio(audio);
    let _ = sender.send_scatter_audio(scatter);
}
fn main() {}
