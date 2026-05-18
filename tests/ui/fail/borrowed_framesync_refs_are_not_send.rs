#![allow(unused)]

use grafton_ndi::{FrameSyncAudioRef, FrameSyncVideoRef};

fn assert_send<T: Send>() {}

fn main() {
    assert_send::<FrameSyncVideoRef<'static>>();
    assert_send::<FrameSyncAudioRef<'static>>();
}
