// The Receiver an AvSync lends, in the AvSyncAttached mode, keeps its video
// and metadata capture and every common control, and a frame
// captured through it is synchronized in place, without a copy, once it is
// known to carry its sender's timestamp.
use grafton_ndi::{
    advanced::{AvSync, AvSyncAttached, AvSyncOutcome, AvSyncRequest, AvSyncVideo},
    MediaTypes, MetadataFrame, MetadataMode, Receiver, ReceiverMode, Source, StreamTier, Tally,
    VideoMode,
};
use std::time::Duration;

fn common<M: ReceiverMode>(_: &Receiver<M>) {}
fn metadata<M: MetadataMode>(receiver: &Receiver<M>) {
    let _ = receiver.metadata().capture_ref(Duration::ZERO);
}
fn video<M: VideoMode>(receiver: &Receiver<M>) {
    let _ = receiver.video().capture(Duration::ZERO);
}
fn controls(receiver: &Receiver<AvSyncAttached>) {
    common(receiver);
    metadata(receiver);
    video(receiver);
    receiver.set_tally(Tally::new(true, false));
    let _ = receiver.connection_stats();
    let _ = receiver.ptz().is_supported();
    let _ = receiver.request_keyframe();
    let _ = receiver.send_metadata(&MetadataFrame::new("<sync/>").unwrap());
    let _ = receiver.set_video_delay(Duration::ZERO);
    let _ = receiver.connect_to(&Source::new("OTHER (Program)"));
    receiver.reconnect();
}
fn zero_copy(sync: &AvSync) -> grafton_ndi::Result<()> {
    controls(sync.receiver());
    // Capture in place, then synchronize that same frame.
    while let Some(video) = sync
        .receiver()
        .video()
        .capture_ref(Duration::from_millis(100))?
    {
        let Some(stamped) = AvSyncVideo::new(&video) else {
            continue;
        };
        if let AvSyncOutcome::Audio { frame, .. } =
            sync.synchronize(stamped, AvSyncRequest::automatic())?
        {
            let _ = (video.width(), frame.num_samples(), stamped.timestamp());
        }
    }
    // An owned copy of a captured frame keeps its timestamp, and the queued
    // audio is taken without a frame.
    if let Some(owned) = sync.receiver().video().capture(Duration::ZERO)? {
        if let Some(stamped) = AvSyncVideo::new(&owned) {
            let _ = sync.synchronize(stamped, AvSyncRequest::automatic())?;
        }
    }
    let _ = sync.queued_audio(AvSyncRequest::automatic())?;
    Ok(())
}
fn exclusive(sync: &mut AvSync) {
    let _ = sync.controls().set_stream_tier(StreamTier::Preview);
    let _ = sync.controls().set_streams(MediaTypes::ALL);
    let _ = sync.controls().kvm().map(|session| session.is_supported());
}
fn scoped(sync: &AvSync) {
    // Another thread uses the lent receiver while this one synchronizes.
    std::thread::scope(|scope| {
        let receiver = sync.receiver();
        scope.spawn(move || {
            let _ = receiver.metadata().capture(Duration::ZERO);
            receiver.reconnect();
        });
        let _ = sync.queued_audio(AvSyncRequest::automatic());
    });
}
fn main() {
    let _ = zero_copy;
    let _ = exclusive;
    let _ = scoped;
}
