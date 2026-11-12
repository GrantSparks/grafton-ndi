use grafton_ndi::{
    advanced::{CompressedSender, QueuedSender, VideoPacketFormat},
    audio_conversion::InterleavedAudioF32,
    AudioFrame, CompressedAttached, MetadataFrame, QueueAttached, Sender, Source, Tally,
};
use std::time::Duration;

fn compressed(sender: &CompressedSender, metadata: &MetadataFrame, failover: &Source) {
    let sender = sender.sender();
    let _ = sender.source();
    sender.send_metadata(metadata);
    let _ = sender.capture_metadata(Duration::ZERO);
    let _ = sender.tally(Duration::ZERO);
    let _ = sender.connection_count(Duration::ZERO);
    sender.add_connection_metadata(metadata);
    sender.clear_connection_metadata();
    let _ = sender.set_failover(failover);
    sender.clear_failover();
    let _ = sender.set_external_tally(Tally::new(true, false));
    let _ = sender.keyframe_required(None);
    let _ = sender.wait_for_keyframe_change(Duration::ZERO, None);
}
fn queued(
    sender: &QueuedSender,
    metadata: &MetadataFrame,
    failover: &Source,
    format: VideoPacketFormat,
    sound: &AudioFrame,
    interleaved: &InterleavedAudioF32<'_>,
) {
    let sender = sender.sender();
    let _ = sender.source();
    sender.send_audio(sound);
    let _ = sender.send_audio_interleaved_f32(interleaved);
    sender.send_metadata(metadata);
    let _ = sender.capture_metadata(Duration::ZERO);
    let _ = sender.tally(Duration::ZERO);
    let _ = sender.connection_count(Duration::ZERO);
    sender.add_connection_metadata(metadata);
    sender.clear_connection_metadata();
    let _ = sender.set_failover(failover);
    sender.clear_failover();
    let _ = sender.set_external_tally(Tally::new(false, true));
    let _ = sender.target_bit_rate(format);
}
fn assert_send_sync_unpin<T: Send + Sync + Unpin>() {}
fn assert_send_unpin<T: Send + Unpin>() {}
fn assert_unpin<T: Unpin>() {}
fn main() {
    let _ = (compressed, queued);
    // Every mode keeps the ordinary sender's auto traits.
    assert_send_sync_unpin::<Sender<CompressedAttached>>();
    assert_send_sync_unpin::<Sender<QueueAttached>>();
    // `CompressedSender` is `Sync` only in a build without the Advanced SDK
    // headers, so that is not asserted here; `QueuedSender` is neither `Send`
    // nor `Sync` (see the failing contracts).
    assert_send_unpin::<CompressedSender>();
    assert_unpin::<QueuedSender>();
}
