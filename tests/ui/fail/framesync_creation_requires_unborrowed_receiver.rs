use grafton_ndi::{FrameSync, Receiver};
use std::time::Duration;
fn attach(receiver: Receiver) {
    let frame = receiver.video().try_capture_ref(Duration::ZERO).unwrap();
    let _ = FrameSync::new(receiver);
    drop(frame);
}
fn main() {}
