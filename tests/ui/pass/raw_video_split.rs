use grafton_ndi::{AudioFrame, RawVideo, Sender, VideoFrame};
fn send(sender: &mut Sender, frame: VideoFrame, audio: &AudioFrame) {
    let (mut video, shared) = sender.split();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ = video.submit_video(frame);
            let _ = video.reclaim_video();
        });
        scope.spawn(move || shared.send_audio(audio));
    });
}
fn movable<T: Send>() {}
fn main() { movable::<RawVideo<'static>>(); }
