use grafton_ndi::{RawCapture, Receiver};

// The deadline/stop capture entry point exists for internal adapters, but
// application code must go through the public capture methods.
fn raw_kernel() {
    let _ = Receiver::<RawCapture>::capture_any_ref_until;
}

fn main() {}
