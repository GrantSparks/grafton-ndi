#![allow(unused)]
use grafton_ndi::{Capture, CaptureKind};
use std::time::Duration;
fn capture<K: CaptureKind>(view: &Capture<'_, K>) -> Result<Option<K::Owned>, grafton_ndi::Error> {
    view.capture(Duration::ZERO)
}
fn capture_ref<'rx, K: CaptureKind>(
    view: &Capture<'rx, K>,
) -> Result<Option<K::Ref<'rx>>, grafton_ndi::Error> {
    view.capture_ref(Duration::ZERO)
}
fn main() {}
