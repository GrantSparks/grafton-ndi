use grafton_ndi::{FrameSync, Receiver};
fn needs_raw(_: &Receiver) {}
fn attached(fs: &FrameSync) {
    needs_raw(fs.receiver());
}
fn main() {}
