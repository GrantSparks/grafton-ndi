use grafton_ndi::{advanced::AvSync, FrameSync};
use std::time::Duration;

fn framesync(sync: &mut FrameSync) {
    let session = sync.controls().kvm().unwrap();
    let _ = session.receiver().poll_event(Duration::ZERO);
}

fn avsync(sync: &mut AvSync) {
    let session = sync.controls().kvm().unwrap();
    let _ = session.receiver().poll_event(Duration::ZERO);
}

fn main() {}
