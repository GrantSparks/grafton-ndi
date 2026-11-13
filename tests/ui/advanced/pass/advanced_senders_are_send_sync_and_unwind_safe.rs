//! Both Advanced senders, and the senders they lend, can move between and be
//! shared across threads, and stay unwind safe, with or without the Advanced
//! SDK headers in the build.
use grafton_ndi::{
    advanced::{CompressedSender, QueuedSender},
    CompressedAttached, QueueAttached, Sender,
};
use std::panic::UnwindSafe;

fn require<T: Send + Sync + UnwindSafe>() {}

fn main() {
    require::<CompressedSender>();
    require::<QueuedSender>();
    require::<Sender<CompressedAttached>>();
    require::<Sender<QueueAttached>>();
}
