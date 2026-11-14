use grafton_ndi::{CaptureMode, FrameSync, MetadataMode, Receiver, ReceiverMode, Tally, VideoMode};
use std::time::Duration;
fn common<M: ReceiverMode>(receiver: &Receiver<M>) {
    receiver.set_tally(Tally::new(false, true));
    let _ = receiver.ptz().is_supported();
    let _ = receiver.connection_stats();
    let _ = receiver.connection_target();
    receiver.reconnect();
}
fn metadata_and_events<M: MetadataMode>(receiver: &Receiver<M>) {
    common(receiver);
    let _ = receiver.metadata().capture(Duration::ZERO);
    let _ = receiver.poll_event(Duration::ZERO);
}
fn capturing<M: CaptureMode>(receiver: &Receiver<M>) {
    metadata_and_events(receiver);
    let _ = receiver.audio().capture_ref(Duration::ZERO);
}
fn video<M: VideoMode>(receiver: &Receiver<M>) {
    common(receiver);
    let _ = receiver.video().capture_ref(Duration::ZERO);
}
fn workflow(receiver: Receiver) {
    capturing(&receiver);
    video(&receiver);
    let fs = FrameSync::new(receiver).unwrap();
    metadata_and_events(fs.receiver());
    let receiver = fs.into_receiver();
    capturing(&receiver);
    let _ = receiver.video().capture(Duration::ZERO);
}
fn assert_send_sync<T: Send + Sync>() {}
fn main() {
    let _ = workflow;
    assert_send_sync::<Receiver>();
    assert_send_sync::<Receiver<grafton_ndi::FrameSyncAttached>>();
    assert_send_sync::<FrameSync>();
}
