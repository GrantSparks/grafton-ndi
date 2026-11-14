use grafton_ndi::advanced::{CompressedAudio, CompressedSender, QueuedSender, QueuedVideo};

fn queued<'a>(mut owner: QueuedSender) -> QueuedVideo<'a> {
    owner.split().0
}

fn compressed<'a>(mut owner: CompressedSender) -> CompressedAudio<'a> {
    owner.split().1
}

fn main() {}
