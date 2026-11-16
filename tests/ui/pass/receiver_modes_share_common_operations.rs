use grafton_ndi::{
    AudioMode, EventMode, FrameSync, MetadataMode, Receiver, ReceiverMode, Tally, VideoMode,
};
use std::time::Duration;
fn common<M: ReceiverMode>(receiver: &Receiver<M>) {
    receiver.set_tally(Tally::new(false, true));
    let _ = receiver.ptz().is_supported();
    let _ = receiver.connection_stats().unwrap();
    let _ = receiver.connection_target();
    receiver.reconnect();
}
fn metadata<M: MetadataMode>(receiver: &Receiver<M>) {
    common(receiver);
    let _ = receiver.capture_metadata(Duration::ZERO);
}
fn events<M: EventMode>(receiver: &Receiver<M>) {
    let _ = receiver.next_event(Duration::ZERO);
}
fn capturing<M: AudioMode>(receiver: &Receiver<M>) {
    metadata(receiver);
    let _ = receiver.capture_audio_ref(Duration::ZERO);
}
fn video<M: VideoMode>(receiver: &Receiver<M>) {
    common(receiver);
    let _ = receiver.capture_video_ref(Duration::ZERO);
}
fn workflow(receiver: Receiver) {
    capturing(&receiver);
    events(&receiver);
    video(&receiver);
    let fs = FrameSync::new(receiver).unwrap();
    metadata(fs.receiver());
    let receiver = fs.into_receiver();
    capturing(&receiver);
    events(&receiver);
    let _ = receiver.capture_video(Duration::ZERO);
}
fn assert_send_sync<T: Send + Sync>() {}
fn main() {
    let _ = workflow;
    assert_send_sync::<Receiver>();
    assert_send_sync::<Receiver<grafton_ndi::FrameSyncAttached>>();
    assert_send_sync::<FrameSync>();
}
