// The allocators are installed on the allocated packet receiver's native
// instance, so its Receiver is never lent out mutably, where it could be
// swapped for another packet receiver's.
use grafton_ndi::advanced::{AllocatedPacketReceiver, PacketReceiver};
fn swap(allocated: &mut AllocatedPacketReceiver, plain: &mut PacketReceiver) {
    std::mem::swap(allocated.receiver_mut(), plain.receiver_mut());
}
fn main() {}
