use grafton_ndi::{FrameSync, ScanType};
fn recover(fs: FrameSync) {
    let frame = fs.capture_video(ScanType::Progressive).unwrap();
    let _ = fs.into_receiver();
    drop(frame);
}
fn main() {}
