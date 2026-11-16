use grafton_ndi::{advanced::{CompressedAttached, CompressedSender, QueuedSender, QueuedVideo}, Sender};

fn queued<'a>(mut owner: QueuedSender) -> QueuedVideo<'a> {
    owner.split().0
}

fn compressed<'a>(mut owner: CompressedSender) -> &'a Sender<CompressedAttached> {
    owner.split().1
}

fn main() {}
