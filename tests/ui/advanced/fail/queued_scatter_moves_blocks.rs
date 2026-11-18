use grafton_ndi::advanced::{QueuedSender, ScatterFrame, VideoPacketFormat};

fn submit(queue: &mut QueuedSender, frame: ScatterFrame<VideoPacketFormat>) {
    let _ = queue.try_submit(frame);
    let _ = frame.blocks();
}

fn main() {}
