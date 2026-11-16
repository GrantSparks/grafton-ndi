use grafton_ndi::{MetadataFrameRef, Sender};
use std::time::Duration;

fn escape<'a>(sender: Sender) -> MetadataFrameRef<'a> {
    sender.capture_metadata_ref(Duration::ZERO).unwrap().unwrap()
}

fn main() {}
