use grafton_ndi::{AudioFormat, AudioFrame, AudioInputFormat, FrameSync, FrameSyncAudioRequest};
use std::{fmt::Debug, hash::Hash, time::Duration};

fn plain_value<T: Copy + Send + Sync + Eq + Hash + Debug + 'static>() {}

// trybuild runs pass fixtures; type-check this contract without creating SDK objects.
fn follow_source_channels(fs: &FrameSync) -> Result<(), grafton_ndi::Error> {
    let format: AudioInputFormat = loop {
        if let Some(format) = fs.audio_input_format()? {
            break format;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let (sample_rate, channels): (u32, u32) = (format.sample_rate(), format.num_channels());
    let request = FrameSyncAudioRequest::capture_at(sample_rate, channels, 1024)?;

    // A capture is always a frame of samples, borrowed or owned.
    let borrowed = fs.capture_audio_ref(request)?;
    let _: AudioFormat = borrowed.format();
    let _: AudioFrame = borrowed.to_owned();
    let _: AudioFrame = fs.capture_audio(request)?;
    Ok(())
}

fn main() {
    plain_value::<AudioInputFormat>();
    let _ = follow_source_channels;
}
