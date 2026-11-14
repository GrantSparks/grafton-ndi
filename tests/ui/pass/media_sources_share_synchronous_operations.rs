use grafton_ndi::{
    audio_conversion::{AudioConverter, AudioReferenceLevel},
    AudioFrame, AudioFrameRef, AudioSource, BorrowedVideoFrame, FrameSyncAudioRef,
    FrameSyncVideoRef, Sender, VideoFrame, VideoFrameRef, VideoSource,
};

fn video<S: VideoSource + ?Sized>(sender: &mut Sender, frame: &S) {
    sender.send_video(frame).unwrap();
    sender.send_video_async_copy(frame).unwrap();
    let _ = frame.sends_in_place();
}

fn audio<S: AudioSource + ?Sized>(sender: &Sender, converter: &AudioConverter, frame: &S) {
    sender.send_audio(frame);
    let _ = converter.to_interleaved_i16(frame, &mut [0; 4096], AudioReferenceLevel::SEND);
    let _ =
        converter.to_interleaved_i32(frame, &mut [0; 4096], AudioReferenceLevel::RECEIVE_HEADROOM);
    let _ = converter.to_interleaved_f32(frame, &mut [0.0; 4096]);
}

fn sources(
    sender: &mut Sender,
    converter: &AudioConverter,
    owned_video: &VideoFrame,
    external_video: &BorrowedVideoFrame<'_>,
    received_video: &VideoFrameRef<'_>,
    synced_video: &FrameSyncVideoRef<'_>,
    owned_audio: &AudioFrame,
    received_audio: &AudioFrameRef<'_>,
    synced_audio: &FrameSyncAudioRef<'_>,
) {
    video(sender, owned_video);
    video(sender, external_video);
    video(sender, received_video);
    video(sender, synced_video);
    audio(sender, converter, owned_audio);
    audio(sender, converter, received_audio);
    audio(sender, converter, synced_audio);
}

fn main() {
    let _ = sources;
}
