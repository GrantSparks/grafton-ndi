use grafton_ndi::FrameSyncAudioRequest;

fn main() {
    let request = FrameSyncAudioRequest::capture_at(48_000, 2, 1024).unwrap();
    let _ = FrameSyncAudioRequest { ..request };
}
