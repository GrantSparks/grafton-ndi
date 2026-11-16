use grafton_ndi::{
    advanced::{AvSync, PacketReceiver},
    FrameSync, ReceiverItem, ReceiverItemRef,
};
use std::time::Duration;

fn framesync(sync: &mut FrameSync) {
    let session = sync.controls().kvm().unwrap();
    let view = session.receiver();
    let _ = view.capture_video(Duration::ZERO);
    let _ = view.capture_audio(Duration::ZERO);
    let _ = view.capture_any_ref(Duration::ZERO);
    let _ = view.capture_any(Duration::ZERO);
}

fn avsync(sync: &mut AvSync) {
    let session = sync.controls().kvm().unwrap();
    let view = session.receiver();
    let _ = view.capture_audio(Duration::ZERO);
    let _ = view.capture_any_ref(Duration::ZERO);
    let _ = view.capture_any(Duration::ZERO);
}

fn packets(packets: &mut PacketReceiver) {
    let session = packets.controls().kvm().unwrap();
    let view = session.receiver();
    let _ = view.capture_video(Duration::ZERO);
    let _ = view.capture_audio(Duration::ZERO);
    let _ = view.capture_metadata(Duration::ZERO);
    let _ = view.next_event(Duration::ZERO);
    let _: Option<ReceiverItemRef<'_>> = view.capture_any_ref(Duration::ZERO).unwrap();
    let _: Option<ReceiverItem> = view.capture_any(Duration::ZERO).unwrap();
}

fn main() {}
