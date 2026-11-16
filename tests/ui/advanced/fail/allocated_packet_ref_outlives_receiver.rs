use grafton_ndi::advanced::{AllocatedReceiver, PacketAttached, PacketItemRef};
use std::time::Duration;
fn escape(receiver: AllocatedReceiver<PacketAttached>) -> PacketItemRef<'static> {
    receiver.capture_any_ref(Duration::ZERO).unwrap().unwrap()
}
fn main() {}
