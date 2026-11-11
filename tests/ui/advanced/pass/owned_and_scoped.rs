use grafton_ndi::{advanced::{AvSync, CompressedSender, Genlock, ScatterVideoFrame}, Receiver, ScanType};

fn independent_scoped_clocks(genlock: &mut Genlock) {
    let (mut video, mut audio) = genlock.clocks();
    std::thread::scope(|scope| {
        scope.spawn(move || video.wait(30, 1, ScanType::Progressive));
        scope.spawn(move || audio.wait(48_000, 1600));
    });
}
fn owned_receiver(receiver: Receiver) {
    let _ = AvSync::new(receiver);
}
fn owned_submission(sender: &mut CompressedSender, frame: ScatterVideoFrame) {
    let _ = sender.send_scatter_video_async(frame);
}
fn main() {}
