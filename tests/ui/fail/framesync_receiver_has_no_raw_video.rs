use grafton_ndi::FrameSync;
fn raw_video(fs: &FrameSync) {
    let _ = fs.receiver().capture_video(std::time::Duration::ZERO);
    let _ = fs.receiver().capture_video_ref(std::time::Duration::ZERO);
}
fn main() {}
