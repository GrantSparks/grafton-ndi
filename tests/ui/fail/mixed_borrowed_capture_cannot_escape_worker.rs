use grafton_ndi::Receiver;
use std::time::Duration;

fn worker(receiver: Receiver) {
    std::thread::spawn(move || receiver.capture_any_ref(Duration::ZERO));
}

fn main() {}
