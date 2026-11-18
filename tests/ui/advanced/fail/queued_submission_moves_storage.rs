use grafton_ndi::{
    advanced::{CompressedFrame, QueuedSender, VideoPacketFormat},
    VideoFrame,
};

fn pixels(queue: &mut QueuedSender, frame: VideoFrame) {
    let _ = queue.try_submit(frame);
    let _ = frame.data();
}

fn packet(queue: &mut QueuedSender, frame: CompressedFrame<VideoPacketFormat>) {
    let _ = queue.try_submit(frame);
    let _ = frame.data();
}

fn main() {}
