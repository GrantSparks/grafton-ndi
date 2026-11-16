use grafton_ndi::advanced::{CompressedSender, QueuedSender};

fn queued(owner: &mut QueuedSender) {
    let (mut video, _) = owner.split();
    let _ = owner.try_next_completed();
    let _ = video.try_next_completed();
}

fn compressed(owner: CompressedSender) {
    let mut owner = owner;
    let (video, _) = owner.split();
    let _ = owner.close();
    drop(video);
}

fn main() {}
