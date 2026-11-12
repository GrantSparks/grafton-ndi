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
        let _owned = video.try_to_owned()?;
    }

    let audio = framesync.capture_audio_ref(FrameSyncAudioRequest::query_input())?;
    let _ = (
        audio.sample_rate(),
        audio.num_channels(),
        audio.channels().len(),
    );
    let _owned = audio.try_to_owned()?;

    Ok(())
}

fn main() {}
