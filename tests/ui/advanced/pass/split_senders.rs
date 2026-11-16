use grafton_ndi::{
    advanced::{CompressedAttached, CompressedAudioFrame, CompressedSender, CompressedVideo,
               CompressedVideoFrame, QueuedSender, QueuedVideo},
    AudioFrame, Sender, VideoFrame,
};

trait AmbiguousIfSync<A> { fn check() {} }
impl<T: ?Sized> AmbiguousIfSync<()> for T {}
impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
trait AmbiguousIfClone<A> { fn check() {} }
impl<T: ?Sized> AmbiguousIfClone<()> for T {}
impl<T: ?Sized + Clone> AmbiguousIfClone<u8> for T {}

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

fn send<T: Send>() {}
fn main() {
    send::<QueuedVideo<'static>>();
    send::<CompressedVideo<'static>>();
    send::<&'static Sender<CompressedAttached>>();
    fn shared<T: Send + Sync + Clone>() {}
    shared::<&'static Sender<CompressedAttached>>();
    <QueuedVideo<'static> as AmbiguousIfSync<_>>::check();
    <CompressedVideo<'static> as AmbiguousIfSync<_>>::check();
    <QueuedVideo<'static> as AmbiguousIfClone<_>>::check();
    <CompressedVideo<'static> as AmbiguousIfClone<_>>::check();
}
