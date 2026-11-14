use grafton_ndi::{Receiver, Source};

fn retarget(receiver: &mut Receiver, next: &Source) {
    let mut session = receiver.kvm().unwrap();
    session.key_down(0xffe1).unwrap();
    session.receiver().connect_to(next).unwrap();
    session.receiver().disconnect();
    session.receiver().reconnect().unwrap();
    session.release_all().unwrap();
}

fn main() {}
