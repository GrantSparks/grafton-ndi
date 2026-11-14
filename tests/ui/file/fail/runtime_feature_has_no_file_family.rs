use grafton_ndi::{file::FileFamily, RuntimeFeature};

fn main() {
    let _ = RuntimeFeature::File(FileFamily::Playback);
}
