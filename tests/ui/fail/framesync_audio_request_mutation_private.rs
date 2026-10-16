use grafton_ndi::FrameSyncAudioRequest;

fn main() {
    let mut request = FrameSyncAudioRequest::capture(2, 1024).unwrap();
    request.sample_rate = -1;
    request.channels = i32::MAX;
    request.samples = i32::MAX;
}
