// Allocator registrations retain this receiver's native instance.
use grafton_ndi::{
    advanced::{AllocatedPacketReceiver, PacketAttached},
    Receiver,
};
fn replaceable(allocated: &mut AllocatedPacketReceiver) -> &mut Receiver<PacketAttached> {
    allocated.receiver_mut()
}
fn main() {}
