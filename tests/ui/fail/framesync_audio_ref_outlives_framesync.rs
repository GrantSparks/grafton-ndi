#![allow(unused)]

use grafton_ndi::{FrameSync, FrameSyncAudioRequest, Ndi, Receiver, ReceiverOptions, Source};

fn receiver(ndi: &Ndi) -> Receiver {
    let source = Source::new("compile-contract");
    let options = ReceiverOptions::new(source);
    Receiver::new(ndi, &options).unwrap()
}

fn main() {
    let ndi = Ndi::new().unwrap();

    let frame_ref = {
        let framesync = FrameSync::new(receiver(&ndi)).unwrap();
        framesync
            .capture_audio_ref(FrameSyncAudioRequest::capture(2, 1024).unwrap())
            .unwrap()
    };

    let _ = frame_ref.num_channels();
}
