use grafton_ndi::FrameSyncAudioRequest;

fn main() {
    let _ = FrameSyncAudioRequest {
        sample_rate: 48_000,
        channels: 2,
        samples: 1024,
    };
}
