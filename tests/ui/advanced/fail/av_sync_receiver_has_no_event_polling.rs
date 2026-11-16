use grafton_ndi::advanced::AvSync;
use std::time::Duration;

fn poll(sync: &AvSync) {
    let _ = sync.receiver().next_event(Duration::ZERO);
}

fn main() {}
