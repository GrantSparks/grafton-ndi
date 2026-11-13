use grafton_ndi::{FrameSync, Receiver};
use std::time::Duration;

fn attach(receiver: Receiver) {
    let item = receiver.capture_any_ref(Duration::ZERO).unwrap();
    let _framesync = FrameSync::new(receiver);
    drop(item);
}

fn main() {}
