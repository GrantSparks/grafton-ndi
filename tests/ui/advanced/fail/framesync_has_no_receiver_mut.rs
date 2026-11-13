use grafton_ndi::FrameSync;
fn swap(first: &mut FrameSync, second: &mut FrameSync) {
    std::mem::swap(first.receiver_mut(), second.receiver_mut());
}
fn main() {}
