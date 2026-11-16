//! Both packet receivers, with and without allocators, can move between and
//! be shared across threads, as the receiver they lend can, and stay unwind
//! safe, with or without the Advanced SDK headers in the build.
use grafton_ndi::{
    advanced::{AllocatedReceiver, PacketAttached, PacketReceiver},
    Receiver,
};
use std::panic::{RefUnwindSafe, UnwindSafe};

fn require<T: Send + Sync + UnwindSafe + RefUnwindSafe>() {}

fn main() {
    require::<PacketReceiver>();
    require::<AllocatedReceiver<PacketAttached>>();
    require::<Receiver<PacketAttached>>();
}
