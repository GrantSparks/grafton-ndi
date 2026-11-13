use grafton_ndi::FrameSync;
fn two(sync: &mut FrameSync) {
    let first = sync.controls().kvm().unwrap();
    let second = sync.controls().kvm().unwrap();
    drop((first, second));
}
fn main() {}
