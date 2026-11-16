use grafton_ndi::FrameSync;
use std::time::Duration;

fn poll(sync: &FrameSync) {
    let _ = sync.receiver().next_event(Duration::ZERO);
}

fn main() {}
