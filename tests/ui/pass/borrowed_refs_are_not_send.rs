#![allow(unused)]

use grafton_ndi::{
    AudioFrameRef, FrameSyncAudioRef, FrameSyncVideoRef, MetadataFrameRef, VideoFrameRef,
};

trait AmbiguousIfSend<A> {
    fn assert_not_send() {}
}

impl<T: ?Sized> AmbiguousIfSend<()> for T {}
impl<T: Send + ?Sized> AmbiguousIfSend<u8> for T {}

fn assert_not_send<T: ?Sized>() {
    let _ = <T as AmbiguousIfSend<_>>::assert_not_send;
}

fn main() {
    assert_not_send::<VideoFrameRef<'static>>();
    assert_not_send::<AudioFrameRef<'static>>();
    assert_not_send::<MetadataFrameRef<'static>>();
    assert_not_send::<FrameSyncVideoRef<'static>>();
    assert_not_send::<FrameSyncAudioRef<'static>>();
}
