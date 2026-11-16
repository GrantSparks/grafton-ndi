use grafton_ndi::{
    advanced::{AllocatedReceiver, PacketAttached},
    Receiver,
};
use std::time::Duration;

fn raw_has_no_packet_capture(receiver: &AllocatedReceiver) {
    let _ = receiver.capture_any(Duration::ZERO);
}

fn packets_cannot_recover_raw(receiver: AllocatedReceiver<PacketAttached>) -> Receiver {
    receiver.into_receiver()
}

fn main() {}
