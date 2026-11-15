use grafton_ndi::advanced::{AvSync, AvSyncOutcomeRef, AvSyncRequest};
fn recover(sync: AvSync) {
    let AvSyncOutcomeRef::Audio { frame, .. } =
        sync.queued_audio_ref(AvSyncRequest::automatic()).unwrap()
    else { return; };
    let receiver = sync.into_receiver();
    drop((frame, receiver));
}
fn main() {}
