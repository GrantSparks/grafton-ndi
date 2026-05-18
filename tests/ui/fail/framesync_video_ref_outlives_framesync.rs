#![allow(unused)]

use grafton_ndi::{FrameSync, NDI, Receiver, ReceiverOptions, ScanType, Source, SourceAddress};

fn receiver(ndi: &NDI) -> Receiver {
    let source = Source {
        name: "compile-contract".to_owned(),
        address: SourceAddress::None,
    };
    let options = ReceiverOptions::builder(source).build();
    Receiver::new(ndi, &options).unwrap()
}

fn main() {
    let ndi = NDI::new().unwrap();

    let frame_ref = {
        let framesync = FrameSync::new(receiver(&ndi)).unwrap();
        framesync
            .capture_video(ScanType::Progressive)
            .unwrap()
            .unwrap()
    };

    let _ = frame_ref.width();
}
