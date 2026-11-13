use grafton_ndi::{FrameSyncVideoRef, Sender, VideoFrameRef};

fn receiver(sender: &mut Sender, frame: VideoFrameRef<'_>) {
    let _ = sender.send_video_async(frame);
}

fn framesync(sender: &mut Sender, frame: FrameSyncVideoRef<'_>) {
    let _ = sender.send_video_async(frame);
}

fn main() {}
