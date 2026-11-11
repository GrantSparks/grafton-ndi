use grafton_ndi::receiver::FrameSyncAttached;
use grafton_ndi::{FrameSync, Receiver, VideoFrame, NDI};
use std::panic::UnwindSafe;

fn owned_contract<T: Send + Sync + UnwindSafe>() {}

fn main() {
    owned_contract::<NDI>();
    owned_contract::<Receiver>();
    owned_contract::<Receiver<FrameSyncAttached>>();
    owned_contract::<FrameSync>();
    owned_contract::<VideoFrame>();
}
