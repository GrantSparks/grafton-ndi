use grafton_ndi::FrameSync;
fn recover(mut sync: FrameSync) {
    let session = sync.controls().kvm().unwrap();
    let receiver = sync.into_receiver();
    drop((session, receiver));
}
fn main() {}
