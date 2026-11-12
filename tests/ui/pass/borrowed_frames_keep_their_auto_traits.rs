use grafton_ndi::{
    AudioFrameRef, BorrowedVideoFrame, FrameSyncAudioRef, FrameSyncVideoRef, MetadataFrameRef,
    VideoFrameRef,
};
use std::panic::{RefUnwindSafe, UnwindSafe};

// Receiver frames and borrowed send frames move to scoped workers; every
// borrowed frame can cross an unwind boundary. FrameSync frames stay on their
// capture thread, which tests/ui/fail/framesync_refs_remain_thread_affine.rs
// checks.
fn thread_and_unwind_safe<T: Send + Sync + UnwindSafe + RefUnwindSafe>() {}
fn unwind_safe<T: UnwindSafe + RefUnwindSafe>() {}

fn main() {
    thread_and_unwind_safe::<VideoFrameRef<'static>>();
    thread_and_unwind_safe::<AudioFrameRef<'static>>();
    thread_and_unwind_safe::<MetadataFrameRef<'static>>();
    thread_and_unwind_safe::<BorrowedVideoFrame<'static>>();
    unwind_safe::<FrameSyncVideoRef<'static>>();
    unwind_safe::<FrameSyncAudioRef<'static>>();
}
