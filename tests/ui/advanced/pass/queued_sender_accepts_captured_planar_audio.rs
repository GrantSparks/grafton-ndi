use grafton_ndi::{advanced::QueuedSender, AudioFrameRef, FrameSyncAudioRef};

fn captured(sender: &QueuedSender, received: &AudioFrameRef<'_>, synced: &FrameSyncAudioRef<'_>) {
    sender.sender().send_audio(received);
    sender.sender().send_audio(synced);
}

fn main() {
    let _ = captured;
}
