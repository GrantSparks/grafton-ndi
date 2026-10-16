use grafton_ndi::{FrameSyncAudioRef, FrameSyncVideoRef};
fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
fn main() {
    assert_send::<FrameSyncVideoRef<'static>>();
    assert_send::<FrameSyncAudioRef<'static>>();
    assert_sync::<FrameSyncVideoRef<'static>>();
    assert_sync::<FrameSyncAudioRef<'static>>();
}
