use grafton_ndi::{
    advanced::{CompressedVideoFrame, QueuedSender},
    VideoFrame,
};

fn pixels(queue: &mut QueuedSender, frame: VideoFrame) {
    let _ = queue.try_submit(frame);
    let _ = frame.data();
}

fn packet(queue: &mut QueuedSender, frame: CompressedVideoFrame) {
    let _ = queue.try_submit(frame);
    let _ = frame.data();
}

fn main() {}
