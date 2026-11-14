use grafton_ndi::FrameSync;
use std::time::Duration;

fn poll(sync: &FrameSync) {
    let _ = sync.receiver().poll_event(Duration::ZERO);
}

fn main() {}
