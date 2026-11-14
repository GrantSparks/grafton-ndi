use grafton_ndi::{Receiver, Source};

fn escaped_view(receiver: &mut Receiver, next: &Source) {
    let session = receiver.kvm().unwrap();
    let view = session.receiver();
    drop(session);
    receiver.connect_to(next).unwrap();
    let _ = view.is_connected();
}

fn escaped_capture(receiver: &mut Receiver) {
    let session = receiver.kvm().unwrap();
    let capture = session.receiver().video();
    drop(session);
    receiver.disconnect();
    let _ = capture.capture_ref(std::time::Duration::ZERO);
}

fn main() {}
