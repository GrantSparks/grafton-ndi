use grafton_ndi::advanced::{CompressedAudioFrame, QueuedSender, ScatterAudioFrame};
fn send(queue: &mut QueuedSender, audio: &mut CompressedAudioFrame, scatter: &mut ScatterAudioFrame) {
    let (_video, sender) = queue.split();
    sender.send_compressed_audio(audio);
    let _ = sender.send_scatter_audio(scatter);
}
fn main() {}
