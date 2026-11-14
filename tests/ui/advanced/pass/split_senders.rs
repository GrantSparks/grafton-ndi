use grafton_ndi::{
    advanced::{CompressedAudio, CompressedAudioFrame, CompressedSender, CompressedVideo,
               CompressedVideoFrame, QueuedSender, QueuedVideo},
    AudioFrame, VideoFrame,
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
    let (mut videos, mut audios) = sender.split();
    std::thread::scope(|scope| {
        scope.spawn(move || videos.send_video(&mut video).unwrap());
        scope.spawn(move || {
            audios.send_audio(&mut audio).unwrap();
            let _ = audios.sender().source();
        });
    });
}

fn send<T: Send>() {}
fn main() {
    send::<QueuedVideo<'static>>();
    send::<CompressedVideo<'static>>();
    send::<CompressedAudio<'static>>();
    <QueuedVideo<'static> as AmbiguousIfSync<_>>::check();
    <CompressedVideo<'static> as AmbiguousIfSync<_>>::check();
    <CompressedAudio<'static> as AmbiguousIfSync<_>>::check();
    <QueuedVideo<'static> as AmbiguousIfClone<_>>::check();
    <CompressedVideo<'static> as AmbiguousIfClone<_>>::check();
    <CompressedAudio<'static> as AmbiguousIfClone<_>>::check();
}
