use grafton_ndi::{FrameSyncVideoRef, Sender, VideoFrameRef};

fn receiver(sender: &mut Sender, frame: VideoFrameRef<'_>) {
    let _ = sender.submit_video(frame);
}

fn framesync(sender: &mut Sender, frame: FrameSyncVideoRef<'_>) {
    let _ = sender.submit_video(frame);
}

fn main() {}
