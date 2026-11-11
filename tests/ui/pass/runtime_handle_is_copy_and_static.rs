use grafton_ndi::{Capabilities, Runtime, RuntimeInfo, NDI};
use std::panic::UnwindSafe;

fn handle_contract<T: Copy + Send + Sync + UnwindSafe + 'static>() {}

fn main() {
    handle_contract::<Runtime>();

    // What a handle describes stays valid after the handle is gone.
    let _: fn(&Runtime) -> &'static RuntimeInfo = Runtime::info;
    let _: fn(&Runtime) -> &'static Capabilities = Runtime::capabilities;
    let _: fn(&NDI) -> &'static RuntimeInfo = NDI::info;
    let _: fn(&NDI) -> &'static Capabilities = NDI::capabilities;
    let _: fn(&NDI) -> Runtime = NDI::runtime;
}
