use grafton_ndi::advanced::{AllocatedPacketReceiver, PacketItemRef};
use std::time::Duration;
fn escape(receiver: AllocatedPacketReceiver) -> PacketItemRef<'static> {
    receiver.capture_ref(Duration::ZERO).unwrap().unwrap()
}
fn main() {}
