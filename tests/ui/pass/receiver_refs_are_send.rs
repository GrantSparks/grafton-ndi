#![allow(unused)]
use grafton_ndi::{AudioFrameRef, MetadataFrameRef, VideoFrameRef};

fn assert_send_sync<T: Send + Sync>() {}

// The receiver stays borrowed while a captured frame moves to a scoped worker.
fn handoff(video: VideoFrameRef<'_>, audio: AudioFrameRef<'_>, metadata: MetadataFrameRef<'_>) {
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ = video.data();
            let _ = audio.num_samples();
            let _ = metadata.data();
        });
    });
}

fn main() {
    assert_send_sync::<VideoFrameRef<'static>>();
    assert_send_sync::<AudioFrameRef<'static>>();
    assert_send_sync::<MetadataFrameRef<'static>>();
}
