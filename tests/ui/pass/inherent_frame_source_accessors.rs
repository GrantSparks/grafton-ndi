// No source trait is imported: every concrete frame exposes its full API.
use grafton_ndi::{
    AudioFrame, AudioFrameRef, BorrowedAudioFrame, BorrowedVideoFrame,
    FrameSyncAudioRef, FrameSyncVideoRef, VideoFrame, VideoFrameRef,
};
macro_rules! video {
    ($name:ident, $type:ty, $data:ty) => {
        fn $name(frame: &$type, destination: &mut VideoFrame) {
            let _ = frame.width();
            let _ = frame.height();
            let _ = frame.pixel_format();
            let _ = frame.frame_rate();
            let _: grafton_ndi::PictureAspectRatio = frame.picture_aspect_ratio();
            let _ = frame.scan_type();
            let _ = frame.timecode();
            let _ = frame.timestamp();
            let _ = frame.line_stride_bytes();
            let _ = frame.metadata();
            let _ = frame.metadata_status();
            let _ = frame.color_info();
            let _ = frame.plane(0);
            let _ = frame.planes();
            let _: $data = frame.data();
            let _: VideoFrame = frame.to_owned();
            frame.copy_into(destination);
            let _ = frame.sends_in_place();
        }
    };
}
macro_rules! audio {
    ($name:ident, $type:ty, $data:ty) => {
        fn $name(frame: &$type, destination: &mut AudioFrame) {
            let _ = frame.sample_rate();
            let _ = frame.num_channels();
            let _ = frame.num_samples();
            let _ = frame.format();
            let _ = frame.timecode();
            let _ = frame.timestamp();
            let _ = frame.channel_stride_bytes();
            let _ = frame.metadata();
            let _ = frame.metadata_status();
            let _ = frame.channel_data(0);
            let _ = frame.channels();
            let _: $data = frame.data();
            let _: AudioFrame = frame.to_owned();
            frame.copy_into(destination);
        }
    };
}
video!(owned_video, VideoFrame, &[u8]);
video!(borrowed_video, BorrowedVideoFrame<'_>, Option<&[u8]>);
video!(received_video, VideoFrameRef<'_>, Option<&[u8]>);
video!(synchronized_video, FrameSyncVideoRef<'_>, Option<&[u8]>);
audio!(owned_audio, AudioFrame, &[f32]);
audio!(borrowed_audio, BorrowedAudioFrame<'_>, Option<&[f32]>);
audio!(received_audio, AudioFrameRef<'_>, Option<&[f32]>);
audio!(synchronized_audio, FrameSyncAudioRef<'_>, Option<&[f32]>);
#[cfg(feature = "advanced")]
audio!(avsync_audio, grafton_ndi::advanced::AvSyncAudioRef<'_>, Option<&[f32]>);
fn main() {}
