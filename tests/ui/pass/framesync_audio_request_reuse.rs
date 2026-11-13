use grafton_ndi::{FrameSync, FrameSyncAudioRequest};

fn reuse(fs: &FrameSync) -> Result<(), grafton_ndi::Error> {
    let request = FrameSyncAudioRequest::capture_at(48_000, 2, 1024)?;
    let copy = request;
    for _ in 0..2 {
        let _borrowed = fs.capture_audio_ref(request)?;
        let _owned = fs.capture_audio(copy)?;
    }
    if let Some(format) = fs.audio_input_format()? {
        let _followed = FrameSyncAudioRequest::for_input(format, 1024)?;
    }
    Ok(())
}

fn main() {
    let _ = reuse;
}
