// A frame captured through the receiver an AvSync lends keeps the AvSync
// borrowed, so the receiver cannot be taken back while the frame lives.
use grafton_ndi::advanced::AvSync;
use std::time::Duration;
fn outlive(sync: AvSync) {
    let video = sync.receiver().video().capture_ref(Duration::ZERO).unwrap();
    let receiver = sync.into_receiver();
    drop(video);
    drop(receiver);
}
fn main() {}
