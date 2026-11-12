// A KVM session lends its receiver for the session's whole lifetime, so a
// captured frame can be held while keys, buttons and touches stay pressed, and
// a packet session captures through itself.
use std::time::Duration;

use grafton_ndi::{
    advanced::{MouseButton, PacketAttached, PacketReceiver},
    Receiver,
};

fn drag_across_frames(receiver: &mut Receiver) {
    let mut session = receiver.kvm().unwrap();
    let lent = session.receiver();
    let frame = lent.video().capture_ref(Duration::ZERO).unwrap();
    session.key_down(0xffe1).unwrap();
    session.move_pointer(0.25, 0.25).unwrap();
    session.mouse_down(MouseButton::Left).unwrap();
    let next = session
        .receiver()
        .video()
        .capture_ref(Duration::ZERO)
        .unwrap();
    session.move_pointer(0.75, 0.75).unwrap();
    session.mouse_up(MouseButton::Left).unwrap();
    session.release_all().unwrap();
    let _ = (frame, next);
}

fn packet_session(packets: &mut PacketReceiver) {
    let mut session = packets.receiver_mut().kvm().unwrap();
    session.mouse_down(MouseButton::Left).unwrap();
    let _event = session.capture(Duration::ZERO).unwrap();
    session.mouse_up(MouseButton::Left).unwrap();
    let _: &Receiver<PacketAttached> = session.receiver();
}

fn main() {
    let _ = drag_across_frames;
    let _ = packet_session;
}
