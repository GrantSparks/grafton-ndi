#![allow(unused)]

use grafton_ndi::{
    FrameSync, FrameSyncAudioRequest, Ndi, Receiver, ReceiverOptions, ScanType, Source,
};

fn receiver(ndi: &Ndi) -> Result<Receiver, grafton_ndi::Error> {
    let source = Source::new("compile-contract");
    let options = ReceiverOptions::new(source);
    Receiver::new(ndi, &options)
}

// trybuild runs pass fixtures; type-check this contract without creating SDK objects.
fn compile_contract() -> Result<(), grafton_ndi::Error> {
    let ndi = Ndi::new()?;
    let framesync = FrameSync::new(receiver(&ndi)?)?;

    if let Some(video) = framesync.capture_video_ref(ScanType::Progressive)? {
        let _ = (
            video.width(),
            video.height(),
            video
                .planes()
                .flat_map(|plane| plane.rows())
                .map(|row| row.len())
                .sum::<usize>(),
        );
        let mut owned = video.try_to_owned()?;
        // A FrameSync frame is copied on its capture thread.
        video.copy_into(&mut owned)?;
    }

    let audio = framesync.capture_audio_ref(FrameSyncAudioRequest::capture_at(48_000, 2, 1024)?)?;
    let _ = (
        audio.sample_rate(),
        audio.num_channels(),
        audio.channels().len(),
    );
    let mut owned = audio.try_to_owned()?;
    audio.copy_into(&mut owned)?;

    Ok(())
}

fn main() {}
