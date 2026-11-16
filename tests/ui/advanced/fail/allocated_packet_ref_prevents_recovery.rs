use grafton_ndi::advanced::{AllocatedReceiver, PacketAttached};
use std::time::Duration;
fn recover(receiver: AllocatedReceiver<PacketAttached>) {
    let frame = receiver.capture_any_ref(Duration::ZERO).unwrap();
    let _owner = receiver.into_packet_receiver();
    drop(frame);
}
fn main() {}
