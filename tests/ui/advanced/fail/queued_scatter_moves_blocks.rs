use grafton_ndi::advanced::{QueuedSender, ScatterVideoFrame};

fn submit(queue: &mut QueuedSender, frame: ScatterVideoFrame) {
    let _ = queue.try_submit(frame);
    let _ = frame.blocks();
}

fn main() {}
