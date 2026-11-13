use grafton_ndi::FrameSync;
use std::time::Duration;

fn mixed(framesync: &FrameSync) {
    let _ = framesync.receiver().capture_any_ref(Duration::ZERO);
    let _ = framesync.receiver().capture_any(Duration::ZERO);
}

fn main() {}
