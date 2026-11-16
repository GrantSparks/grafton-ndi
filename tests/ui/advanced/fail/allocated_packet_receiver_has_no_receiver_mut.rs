// Allocator registrations retain this receiver's native instance.
use grafton_ndi::{
    advanced::{AllocatedReceiver, PacketAttached},
    Receiver,
};
fn replaceable(allocated: &mut AllocatedReceiver<PacketAttached>) -> &mut Receiver<PacketAttached> {
    allocated.receiver_mut()
}
fn main() {}
