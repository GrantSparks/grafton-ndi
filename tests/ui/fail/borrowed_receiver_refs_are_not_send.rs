#![allow(unused)]

use grafton_ndi::{AudioFrameRef, MetadataFrameRef, VideoFrameRef};

fn assert_send<T: Send>() {}

fn main() {
    assert_send::<VideoFrameRef<'static>>();
    assert_send::<AudioFrameRef<'static>>();
    assert_send::<MetadataFrameRef<'static>>();
}
