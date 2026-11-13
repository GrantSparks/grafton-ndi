use grafton_ndi::Receiver;
use std::time::Duration;

fn release(receiver: Receiver) {
    let item = receiver.capture_any_ref(Duration::ZERO).unwrap();
    drop(receiver);
    drop(item);
}

fn main() {}
