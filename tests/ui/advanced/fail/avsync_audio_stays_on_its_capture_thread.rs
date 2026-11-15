use grafton_ndi::advanced::AvSyncAudioRef;
fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
fn main() {
    assert_send::<AvSyncAudioRef<'static>>();
    assert_sync::<AvSyncAudioRef<'static>>();
}
