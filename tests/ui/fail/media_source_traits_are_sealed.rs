use grafton_ndi::{AudioSource, VideoSource};

struct Unchecked;
impl VideoSource for Unchecked {}
impl AudioSource for Unchecked {}

fn main() {}
