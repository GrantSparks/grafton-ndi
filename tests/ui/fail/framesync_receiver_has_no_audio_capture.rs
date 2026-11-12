// The synchronizer takes the receiver's audio, so the Receiver a FrameSync
// lends has no audio capture of its own.
use grafton_ndi::{FrameSync, FrameSyncAttached, Receiver};
use std::time::Duration;
fn own_audio(receiver: &Receiver<FrameSyncAttached>) {
    let _ = receiver.audio().capture(Duration::ZERO);
}
fn lent_audio(framesync: &FrameSync) {
    let _ = framesync.receiver().audio().capture_ref(Duration::ZERO);
}
fn main() {}
