use grafton_ndi::{
    AudioFrameRef, BorrowedVideoFrame, FrameSyncAudioRef, FrameSyncVideoRef, MetadataFrameRef,
    VideoFrameRef,
};

// A borrowed frame can be used where a shorter borrow of its owner is
// expected: every frame reference is covariant in its lifetime.
fn shorten<'long: 'short, 'short>(
    video: VideoFrameRef<'long>,
    audio: AudioFrameRef<'long>,
    metadata: MetadataFrameRef<'long>,
    framesync_video: FrameSyncVideoRef<'long>,
    framesync_audio: FrameSyncAudioRef<'long>,
    borrowed: BorrowedVideoFrame<'long>,
) -> (
    VideoFrameRef<'short>,
    AudioFrameRef<'short>,
    MetadataFrameRef<'short>,
    FrameSyncVideoRef<'short>,
    FrameSyncAudioRef<'short>,
    BorrowedVideoFrame<'short>,
) {
    (
        video,
        audio,
        metadata,
        framesync_video,
        framesync_audio,
        borrowed,
    )
}

fn main() {
    let _ = shorten;
}
