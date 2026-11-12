use grafton_ndi::{Capabilities, Ndi, Runtime, RuntimeInfo};
use std::panic::UnwindSafe;

fn handle_contract<T: Copy + Send + Sync + UnwindSafe + 'static>() {}

fn main() {
    handle_contract::<Runtime>();

    // What a handle describes stays valid after the handle is gone.
    let _: fn(&Runtime) -> &'static RuntimeInfo = Runtime::info;
    let _: fn(&Runtime) -> &'static Capabilities = Runtime::capabilities;
    let _: fn(&Ndi) -> &'static RuntimeInfo = Ndi::info;
    let _: fn(&Ndi) -> &'static Capabilities = Ndi::capabilities;
    let _: fn(&Ndi) -> Runtime = Ndi::runtime;
}
