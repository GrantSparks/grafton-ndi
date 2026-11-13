use grafton_ndi::FrameSyncAudioRequest;

fn main() {
    let mut request = FrameSyncAudioRequest::capture_at(48_000, 2, 1024).unwrap();
    request.sample_rate = -1;
    request.num_channels = i32::MAX;
    request.num_samples = i32::MAX;
}
