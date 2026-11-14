use grafton_ndi::{
    advanced::{AvSync, PacketReceiver},
    FrameSync,
};
use std::time::Duration;

fn framesync(sync: &mut FrameSync) {
    let session = sync.controls().kvm().unwrap();
    let view = session.receiver();
    let _ = view.video();
    let _ = view.audio();
    let _ = view.capture_any_ref(Duration::ZERO);
    let _ = view.capture_any(Duration::ZERO);
}

fn avsync(sync: &mut AvSync) {
    let session = sync.controls().kvm().unwrap();
    let view = session.receiver();
    let _ = view.audio();
    let _ = view.capture_any_ref(Duration::ZERO);
    let _ = view.capture_any(Duration::ZERO);
}

fn packets(packets: &mut PacketReceiver) {
    let session = packets.controls().kvm().unwrap();
    let view = session.receiver();
    let _ = view.video();
    let _ = view.audio();
    let _ = view.metadata();
    let _ = view.poll_event(Duration::ZERO);
    let _ = view.capture_any_ref(Duration::ZERO);
    let _ = view.capture_any(Duration::ZERO);
}

fn main() {}
