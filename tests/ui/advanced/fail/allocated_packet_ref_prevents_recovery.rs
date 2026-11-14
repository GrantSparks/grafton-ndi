use grafton_ndi::advanced::AllocatedPacketReceiver;
use std::time::Duration;
fn recover(receiver: AllocatedPacketReceiver) {
    let frame = receiver.capture_ref(Duration::ZERO).unwrap();
    let _owner = receiver.into_packet_receiver();
    drop(frame);
}
fn main() {}
