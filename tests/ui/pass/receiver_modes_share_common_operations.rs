use grafton_ndi::{FrameSync, Receiver, ReceiverMode, Tally};
use std::time::Duration;
fn common<M: ReceiverMode>(receiver: &Receiver<M>) {
    let _ = receiver.set_tally(&Tally::new(false, true));
    let _ = receiver.ptz_is_supported();
    let _ = receiver.connection_stats();
    let _ = receiver.source();
    let _ = receiver.audio().try_capture_ref(Duration::ZERO);
    let _ = receiver.metadata().try_capture(Duration::ZERO);
    let _ = receiver.poll_status_change(Duration::ZERO);
    let _ = receiver.reconnect();
}
fn workflow(receiver: Receiver) {
    common(&receiver);
    let fs = FrameSync::new(receiver).unwrap();
    common(fs.receiver());
    let receiver = fs.into_receiver();
    let _ = receiver.video().try_capture(Duration::ZERO);
}
fn assert_send_sync<T: Send + Sync>() {}
fn main() {
    let _ = workflow;
    assert_send_sync::<Receiver>();
    assert_send_sync::<Receiver<grafton_ndi::FrameSyncAttached>>();
    assert_send_sync::<FrameSync>();
}
