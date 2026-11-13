use grafton_ndi::{FrameSync, ScanType};
fn recover(mut sync: FrameSync) {
    let (capture, _) = sync.split();
    let frame = capture.capture_video_ref(ScanType::Progressive).unwrap();
    let receiver = sync.into_receiver();
    drop((frame, receiver));
}
fn main() {}
