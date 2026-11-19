use grafton_ndi::{Ndi, VideoSource};

fn compare(frame: &impl VideoSource, ndi: &Ndi) -> bool {
    frame.timestamp().unwrap() < ndi.timestamp().unwrap()
}

fn main() {}
