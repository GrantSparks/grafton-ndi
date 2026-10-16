use grafton_ndi::FrameSync;
fn raw_video(fs: &FrameSync) {
    let _ = fs.receiver().video();
}
fn main() {}
