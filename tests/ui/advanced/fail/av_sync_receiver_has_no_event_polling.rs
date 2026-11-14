use grafton_ndi::advanced::AvSync;
use std::time::Duration;

fn poll(sync: &AvSync) {
    let _ = sync.receiver().poll_event(Duration::ZERO);
}

fn main() {}
