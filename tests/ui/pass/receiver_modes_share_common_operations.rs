use grafton_ndi::{FrameSync, Receiver, ReceiverMode, Tally};
use std::time::Duration;
fn common<M: ReceiverMode>(receiver: &Receiver<M>) {
    receiver.set_tally(Tally::new(false, true));
    let _ = receiver.ptz().is_supported();
    let _ = receiver.connection_stats();
    let _ = receiver.source();
    let _ = receiver.audio().capture_ref(Duration::ZERO);
    let _ = receiver.metadata().capture(Duration::ZERO);
    let _ = receiver.poll_event(Duration::ZERO);
    let _ = receiver.reconnect();
}
fn workflow(receiver: Receiver) {
    common(&receiver);
    let fs = FrameSync::new(receiver).unwrap();
    common(fs.receiver());
    let receiver = fs.into_receiver();
    let _ = receiver.video().capture(Duration::ZERO);
}
fn assert_send_sync<T: Send + Sync>() {}
fn main() {
    let _ = workflow;
    assert_send_sync::<Receiver>();
    assert_send_sync::<Receiver<grafton_ndi::FrameSyncAttached>>();
    assert_send_sync::<FrameSync>();
}
