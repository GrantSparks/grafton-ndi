use grafton_ndi::FrameSyncAudioRequest;

fn main() {
    let _ = FrameSyncAudioRequest {
        sample_rate: 48_000,
        num_channels: 2,
        num_samples: 1024,
    };
}
