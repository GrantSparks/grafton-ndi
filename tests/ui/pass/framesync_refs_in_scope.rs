#![allow(unused)]

use grafton_ndi::{
    FrameSync, FrameSyncAudioRequest, Receiver, ReceiverOptions, ScanType, Source, SourceAddress,
    NDI,
};

fn receiver(ndi: &NDI) -> Result<Receiver, grafton_ndi::Error> {
    let source = Source {
        name: "compile-contract".to_owned(),
        address: SourceAddress::None,
    };
    let options = ReceiverOptions::builder(source).build();
    Receiver::new(ndi, &options)
}

fn main() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let framesync = FrameSync::new(receiver(&ndi)?)?;

    if let Some(video) = framesync.capture_video(ScanType::Progressive)? {
        let _ = (
            video.width(),
            video.height(),
            video
                .planes()
                .flat_map(|plane| plane.rows())
                .map(|row| row.len())
                .sum::<usize>(),
        );
        let _owned = video.to_owned()?;
    }

    let audio = framesync.capture_audio(FrameSyncAudioRequest::query_input())?;
    let _ = (
        audio.sample_rate(),
        audio.num_channels(),
        audio.channels().len(),
    );
    let _owned = audio.to_owned()?;

    Ok(())
}
