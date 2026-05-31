#![allow(unused)]

use std::time::Duration;

use grafton_ndi::{Receiver, ReceiverOptions, Source, SourceAddress, NDI};

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
    let receiver = receiver(&ndi)?;

    if let Some(video) = receiver.video().try_capture_ref(Duration::from_millis(1))? {
        let _ = (video.width(), video.height(), video.data().len());
        let _owned = video.to_owned()?;
    }

    if let Some(audio) = receiver.audio().try_capture_ref(Duration::from_millis(1))? {
        let _ = (
            audio.num_channels(),
            audio.num_samples(),
            audio.data().len(),
        );
        let _owned = audio.to_owned()?;
    }

    if let Some(metadata) = receiver
        .metadata()
        .try_capture_ref(Duration::from_millis(1))?
    {
        let _ = metadata.data();
        let _owned = metadata.to_owned();
    }

    Ok(())
}
