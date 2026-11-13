use grafton_ndi::{FrameSync, FrameSyncVideoRef, ScanType};
fn escape(mut sync: FrameSync) -> FrameSyncVideoRef<'static> {
    let (capture, _) = sync.split();
    capture
        .capture_video_ref(ScanType::Progressive)
        .unwrap()
        .unwrap()
}
fn main() {}
