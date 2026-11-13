#![allow(unused)]

use std::time::Duration;

use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};

fn receiver(ndi: &Ndi) -> Result<Receiver, grafton_ndi::Error> {
    let source = Source::new("compile-contract");
    let options = ReceiverOptions::new(source);
    Receiver::new(ndi, &options)
}

// trybuild runs pass fixtures; type-check this contract without creating SDK objects.
fn compile_contract() -> Result<(), grafton_ndi::Error> {
    let ndi = Ndi::new()?;
    let receiver = receiver(&ndi)?;

    if let Some(video) = receiver.video().capture_ref(Duration::from_millis(1))? {
        let _ = (
            video.width(),
            video.height(),
            video
                .planes()
                .flat_map(|plane| plane.rows())
                .map(|row| row.len())
                .sum::<usize>(),
        );
        let mut owned = video.to_owned();
        // A receiver frame can be copied into a reused frame on a scoped worker.
        std::thread::scope(|scope| scope.spawn(|| video.copy_into(&mut owned)).join())
            .expect("copy worker");
    }

    if let Some(audio) = receiver.audio().capture_ref(Duration::from_millis(1))? {
        let _ = (
            audio.num_channels(),
            audio.num_samples(),
            audio.channels().len(),
        );
        let mut owned = audio.to_owned();
        std::thread::scope(|scope| scope.spawn(|| audio.copy_into(&mut owned)).join())
            .expect("copy worker");
    }

    if let Some(metadata) = receiver.metadata().capture_ref(Duration::from_millis(1))? {
        let _ = metadata.data();
        let _owned = metadata.to_owned();
    }

    Ok(())
}

fn main() {}
