use grafton_ndi::advanced::{AvSync, AvSyncOutcomeRef, AvSyncRequest};
fn recover(mut sync: AvSync) {
    let frame = {
        let (capture, _) = sync.split();
        let AvSyncOutcomeRef::Audio { frame, .. } =
            capture.queued_audio_ref(AvSyncRequest::automatic()).unwrap()
        else { return; };
        frame
    };
    let receiver = sync.into_receiver();
    drop((frame, receiver));
}
fn main() {}
