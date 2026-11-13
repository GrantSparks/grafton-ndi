use grafton_ndi::{
    audio_conversion::AudioConverter,
    video_conversion::{V210Layout, VideoConverter},
    AudioFrame, AudioSource, BorrowedAudioFrame, BorrowedVideoFrame, PixelFormat, Sender,
    VideoFrame, VideoSource,
};

fn forward(sender: &Sender, audio: &dyn AudioSource, video: &dyn VideoSource) {
    sender.send_audio(audio);
    sender.send_video(video).unwrap();
}
fn convert(
    audio: &AudioConverter,
    video: &VideoConverter,
    planar: &dyn AudioSource,
    p216: &dyn VideoSource,
) {
    let mut interleaved = vec![0.0; planar.num_channels() as usize * planar.num_samples() as usize];
    audio.to_interleaved_f32(planar, &mut interleaved).unwrap();
    let layout = V210Layout::new(p216.width(), p216.height()).unwrap();
    let mut words = vec![0; layout.required_words()];
    video.p216_to_v210(p216, layout, &mut words).unwrap();
}
fn main() {
    let samples = [0.0; 4];
    let audio = BorrowedAudioFrame::builder(&samples, 48_000, 2, 2)
        .build()
        .unwrap();
    let erased: &dyn AudioSource = &audio;
    let _: AudioFrame = erased.to_owned();
    let pixels = [0; 24];
    let video = BorrowedVideoFrame::builder(&pixels, 6, 1, PixelFormat::P216)
        .build()
        .unwrap();
    let erased: &dyn VideoSource = &video;
    let _: VideoFrame = erased.to_owned();
    let _ = forward;
    let _ = convert;
}
