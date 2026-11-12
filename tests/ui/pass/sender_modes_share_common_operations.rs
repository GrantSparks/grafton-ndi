use grafton_ndi::{
    AudioFrame, AudioSendMode, MetadataFrame, RawSend, Sender, SenderMode, Source, VideoFrame,
};
use std::time::Duration;

// The operations every sender mode shares.
fn common<M: SenderMode>(sender: &Sender<M>, metadata: &MetadataFrame, failover: &Source) {
    let _ = sender.source();
    sender.send_metadata(metadata);
    let _ = sender.capture_metadata(Duration::ZERO);
    let _ = sender.tally();
    let _ = sender.wait_for_tally_change(Duration::ZERO);
    let _ = sender.connection_count(Duration::ZERO);
    sender.add_connection_metadata(metadata);
    sender.clear_connection_metadata();
    let _ = sender.set_failover(failover);
    sender.clear_failover();
}
fn audio<M: AudioSendMode>(sender: &Sender<M>, frame: &AudioFrame) {
    sender.send_audio(frame);
}
// The default mode is the ordinary sender, with every operation.
fn ordinary(mut sender: Sender<RawSend>, frame: VideoFrame, sound: &AudioFrame) {
    let source = sender.source().unwrap();
    let metadata = MetadataFrame::new("<x/>").unwrap();
    common(&sender, &metadata, &source);
    audio(&sender, sound);
    let _ = sender.send_video(&frame);
    let _ = sender.send_video_async(frame);
    sender.flush_async_blocking();
    let _ = sender.finish_video_async();
}
fn assert_owned<T: Send + Sync + Unpin>() {}
fn main() {
    let _ = ordinary;
    assert_owned::<Sender>();
    assert_owned::<Sender<RawSend>>();
}
