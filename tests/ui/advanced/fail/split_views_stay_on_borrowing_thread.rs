use grafton_ndi::{advanced::ReceiverControls, FrameSyncAttached, FrameSyncCapture};
fn send<T: Send>() {}
fn sync<T: Sync>() {}
fn main() {
    send::<FrameSyncCapture<'static>>();
    sync::<FrameSyncCapture<'static>>();
    send::<ReceiverControls<'static, FrameSyncAttached>>();
    sync::<ReceiverControls<'static, FrameSyncAttached>>();
}
