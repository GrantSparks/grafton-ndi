use grafton_ndi::{
    AudioFrame, AudioFrameRef, AudioMode, MetadataFrame, MetadataFrameRef, MetadataMode, Receiver,
    Result, VideoFrame, VideoFrameRef, VideoMode,
};
use std::time::Duration;

// Public capability bounds support generic application code without naming
// private capture kinds, strategies, handles, or permission traits.
fn video<M: VideoMode>(
    receiver: &Receiver<M>,
) -> Result<(Option<VideoFrame>, Option<VideoFrameRef<'_>>)> {
    Ok((
        receiver.capture_video(Duration::ZERO)?,
        receiver.capture_video_ref(Duration::ZERO)?,
    ))
}

fn audio<M: AudioMode>(
    receiver: &Receiver<M>,
) -> Result<(Option<AudioFrame>, Option<AudioFrameRef<'_>>)> {
    Ok((
        receiver.capture_audio(Duration::ZERO)?,
        receiver.capture_audio_ref(Duration::ZERO)?,
    ))
}

fn metadata<M: MetadataMode>(
    receiver: &Receiver<M>,
) -> Result<(Option<MetadataFrame>, Option<MetadataFrameRef<'_>>)> {
    Ok((
        receiver.capture_metadata(Duration::ZERO)?,
        receiver.capture_metadata_ref(Duration::ZERO)?,
    ))
}

fn main() {
    let _ = video::<grafton_ndi::RawCapture>;
    let _ = audio::<grafton_ndi::RawCapture>;
    let _ = metadata::<grafton_ndi::RawCapture>;
    let _ = metadata::<grafton_ndi::FrameSyncAttached>;
    #[cfg(feature = "advanced")]
    {
        let _ = video::<grafton_ndi::advanced::AvSyncAttached>;
        let _ = metadata::<grafton_ndi::advanced::AvSyncAttached>;
    }
}
