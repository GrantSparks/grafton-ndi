use grafton_ndi::Receiver;

fn two_sessions(receiver: &mut Receiver) {
    let first = receiver.kvm().unwrap();
    let second = receiver.kvm().unwrap();
    let _ = (first.is_supported(), second.is_supported());
}
fn main() {}
