use grafton_ndi::AudioInputFormat;
use std::num::NonZeroU32;

fn main() {
    let _ = AudioInputFormat {
        sample_rate: NonZeroU32::MIN,
        num_channels: NonZeroU32::MIN,
    };
}
