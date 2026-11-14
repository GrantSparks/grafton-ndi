use grafton_ndi::{
    advanced::{AvSync, CompressedSender, Genlock, ScatterVideoFrame},
    FrameRate, Receiver, ScanType,
};

fn independent_scoped_clocks(genlock: &mut Genlock, frame_rate: FrameRate) {
    let (mut video, mut audio) = genlock.clocks();
    std::thread::scope(|scope| {
        scope.spawn(move || video.wait(frame_rate, ScanType::Progressive));
        scope.spawn(move || audio.wait(48_000, 1600));
    });
}
fn owned_receiver(receiver: Receiver) {
    let _ = AvSync::new(receiver);
}
fn owned_submission(sender: &mut CompressedSender, frame: ScatterVideoFrame) {
    let _ = sender.send_scatter_video_async(frame);
}
fn main() {
    let _ = (independent_scoped_clocks, owned_receiver, owned_submission);
}
