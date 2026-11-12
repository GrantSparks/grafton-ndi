// The synchronizer takes the receiver's audio, so the Receiver an AvSync lends
// has no audio capture of its own.
use grafton_ndi::{
    advanced::{AvSync, AvSyncAttached},
    Receiver,
};
use std::time::Duration;
fn own_audio(receiver: &Receiver<AvSyncAttached>) {
    let _ = receiver.audio().capture(Duration::ZERO);
}
fn lent_audio(sync: &AvSync) {
    let _ = sync.receiver().audio().capture_ref(Duration::ZERO);
}
fn main() {}
