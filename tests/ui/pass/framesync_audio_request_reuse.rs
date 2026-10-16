use grafton_ndi::{FrameSync, FrameSyncAudioRequest};

fn reuse(fs: &FrameSync) -> Result<(), grafton_ndi::Error> {
    let request = FrameSyncAudioRequest::capture_with(Some(48_000), 2, 1024)?;
    let copy = request;
    for _ in 0..2 {
        let _borrowed = fs.capture_audio(request)?;
        let _owned = fs.capture_audio_owned(copy)?;
    }
    let _source_rate = FrameSyncAudioRequest::capture(2, 1024)?;
    let _query = FrameSyncAudioRequest::query_input();
    Ok(())
}

fn main() {
    let _ = reuse;
}
