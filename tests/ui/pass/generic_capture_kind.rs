#![allow(unused)]
use grafton_ndi::{Capture, CaptureKind};
use std::time::Duration;
fn capture<K: CaptureKind>(view: &Capture<'_, K>) -> Result<Option<K::Owned>, grafton_ndi::Error> {
    view.try_capture(Duration::ZERO)
}
fn copy<K: CaptureKind>(frame: &K::Ref<'_>) -> Result<K::Owned, grafton_ndi::Error> {
    K::ref_to_owned(frame)
}
fn main() {}
