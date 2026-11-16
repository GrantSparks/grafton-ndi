use grafton_ndi::{Receiver, Source};

fn escaped_view(receiver: &mut Receiver, next: &Source) {
    let session = receiver.kvm().unwrap();
    let view = session.receiver();
    drop(session);
    receiver.connect_to(next);
    let _ = view.is_connected();
}

fn escaped_capture(receiver: &mut Receiver) {
    let session = receiver.kvm().unwrap();
    let frame = session.receiver().capture_video_ref(std::time::Duration::ZERO).unwrap();
    drop(session);
    receiver.disconnect();
    drop(frame);
}

fn main() {}
