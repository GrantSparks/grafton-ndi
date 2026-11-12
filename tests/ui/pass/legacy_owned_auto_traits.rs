use grafton_ndi::receiver::FrameSyncAttached;
use grafton_ndi::{FrameSync, Ndi, Receiver, VideoFrame};
use std::panic::UnwindSafe;

fn owned_contract<T: Send + Sync + UnwindSafe>() {}

fn main() {
    owned_contract::<Ndi>();
    owned_contract::<Receiver>();
    owned_contract::<Receiver<FrameSyncAttached>>();
    owned_contract::<FrameSync>();
    owned_contract::<VideoFrame>();
}
