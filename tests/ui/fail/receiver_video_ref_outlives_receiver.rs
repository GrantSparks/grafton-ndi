#![allow(unused)]

use std::time::Duration;

use grafton_ndi::{Receiver, ReceiverOptions, Source, SourceAddress, NDI};

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
        let receiver = receiver(&ndi);
        receiver
            .video()
            .try_capture_ref(Duration::from_millis(1))
            .unwrap()
            .unwrap()
    };

    let _ = frame_ref.width();
}
