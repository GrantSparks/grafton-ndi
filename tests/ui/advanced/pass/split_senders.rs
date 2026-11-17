use grafton_ndi::{
    advanced::{CompressedAttached, CompressedAudioFrame, CompressedSender,
               CompressedVideoFrame, QueuedSender},
    AudioFrame, Sender, VideoFrame,
};

fn queued(queue: &mut QueuedSender, video: VideoFrame, audio: &AudioFrame) {
    let (mut frames, sender) = queue.split();
    std::thread::scope(|scope| {
        scope.spawn(move || { frames.try_submit(video).unwrap(); });
        scope.spawn(move || sender.send_audio(audio));
    });
}

fn compressed(sender: &mut CompressedSender, mut video: CompressedVideoFrame, mut audio: CompressedAudioFrame) {
    let (mut videos, audios) = sender.split();
    std::thread::scope(|scope| {
        scope.spawn(move || videos.send_video(&mut video));
        scope.spawn(move || {
            audios.send_compressed_audio(&mut audio);
            let _ = audios.source();
        });
    });
}

fn main() {
    fn shared<T: Send + Sync + Clone>() {}
    shared::<&'static Sender<CompressedAttached>>();
}
